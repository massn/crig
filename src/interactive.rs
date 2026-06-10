use anyhow::Result;
use inquire::autocompletion::{Autocomplete, Replacement};
use inquire::{Confirm, CustomType, CustomUserError, Select, Text};

use crate::config::{
    add_or_update_construct, load_config, remove_construct, AgentType, Construct, LLMConfig,
    RouterThresholds, DEFAULT_OLLAMA_ENDPOINT,
};

/// Autocompleter that suggests construct names from the current config.
/// - Typing filters suggestions by prefix (case-insensitive).
/// - Tab with no highlighted suggestion replaces input with the longest common
///   prefix among matches (mirrors shell behaviour).
/// - Tab with a highlighted suggestion replaces input with that suggestion.
#[derive(Clone)]
struct ConstructCompleter {
    names: Vec<String>,
}

impl ConstructCompleter {
    fn new(names: Vec<String>) -> Self {
        Self { names }
    }

    fn matches(&self, input: &str) -> Vec<String> {
        let needle = input.to_lowercase();
        self.names
            .iter()
            .filter(|n| n.to_lowercase().starts_with(&needle))
            .cloned()
            .collect()
    }
}

impl Autocomplete for ConstructCompleter {
    fn get_suggestions(&mut self, input: &str) -> Result<Vec<String>, CustomUserError> {
        Ok(self.matches(input))
    }

    fn get_completion(
        &mut self,
        input: &str,
        highlighted_suggestion: Option<String>,
    ) -> Result<Replacement, CustomUserError> {
        // If a suggestion is highlighted, use it verbatim.
        if let Some(s) = highlighted_suggestion {
            return Ok(Some(s));
        }
        // Otherwise, compute the longest common prefix of current matches.
        let matches = self.matches(input);
        Ok(longest_common_prefix(&matches).filter(|p| p != input))
    }
}

fn longest_common_prefix(strings: &[String]) -> Option<String> {
    let mut iter = strings.iter();
    let first = iter.next()?.clone();
    let prefix = iter.fold(first, |acc, s| {
        let n = acc
            .chars()
            .zip(s.chars())
            .take_while(|(a, b)| a == b)
            .count();
        acc.chars().take(n).collect()
    });
    if prefix.is_empty() {
        None
    } else {
        Some(prefix)
    }
}

/// Prompt for a construct name with tab-completion. Returns the chosen name.
/// Caller is responsible for validating that the name resolves to a construct.
pub fn prompt_construct_name(prompt: &str) -> Result<String> {
    let config = load_config()?;
    if config.constructs.is_empty() {
        anyhow::bail!("No constructs configured. Run 'crig config' to create one.");
    }
    let names: Vec<String> = config.constructs.iter().map(|c| c.name.clone()).collect();
    let default_name = config.active_construct.clone();

    let mut text = Text::new(prompt)
        .with_autocomplete(ConstructCompleter::new(names))
        .with_help_message("Type a name, or press Tab to complete / list candidates");
    if !default_name.is_empty() {
        text = text.with_default(&default_name);
    }
    Ok(text.prompt()?)
}

pub fn remove(name: Option<String>) -> Result<()> {
    let name = match name {
        Some(n) => n,
        None => prompt_construct_name("Construct name to remove:")?,
    };

    let confirmed = Confirm::new(&format!("Remove construct '{}'?", name))
        .with_default(false)
        .prompt()?;

    if !confirmed {
        println!("Cancelled.");
        return Ok(());
    }

    remove_construct(&name)?;
    println!("Construct '{}' removed.", name);
    Ok(())
}


/// Ask the user for an LLM configuration. The list of choices depends on the
/// agent: Hermes cannot have its endpoint overridden by crig, so we hide the
/// "Anthropic-compatible" and "Router" options for Hermes constructs (the
/// router proxy relies on overriding the agent's endpoint, which Hermes does
/// not support).
fn prompt_llm_config(agent_type: AgentType) -> Result<LLMConfig> {
    let llm_options: Vec<&str> = match agent_type {
        AgentType::ClaudeCode => vec!["Claude API", "Anthropic-compatible", "Ollama", "Router"],
        AgentType::Hermes => vec!["Claude API", "Ollama"],
    };
    let help = match agent_type {
        AgentType::ClaudeCode => None,
        AgentType::Hermes => Some(
            "Hermes reads its endpoint from ~/.hermes/config.yaml, so crig cannot override it. \
             Use `hermes model` to point Hermes at a custom endpoint.",
        ),
    };
    let mut select = Select::new("Select LLM type:", llm_options);
    if let Some(h) = help {
        select = select.with_help_message(h);
    }
    let llm_choice = select.prompt()?;

    if llm_choice == "Router" {
        return prompt_router_config();
    }

    build_base_llm(llm_choice)
}

/// Build a single, non-router LLM backend from a chosen type label. Used both
/// for top-level constructs and for the weak/strong backends of a Router.
fn build_base_llm(llm_choice: &str) -> Result<LLMConfig> {
    let llm_config = match llm_choice {
        "Claude API" => {
            let api_key = Text::new("Claude API key (leave empty to use environment variable):")
                .with_help_message("Will use ANTHROPIC_API_KEY if not provided")
                .prompt_skippable()?;

            let model = Text::new("Model name:")
                .with_default("claude-sonnet-4-6")
                .prompt()?;

            LLMConfig::ClaudeApi { api_key, model }
        }
        "Ollama" => {
            let endpoint = Text::new("Ollama endpoint:")
                .with_default(DEFAULT_OLLAMA_ENDPOINT)
                .with_help_message("crig will start `ollama serve` here if not already running")
                .prompt()?;

            let model = Text::new("Model name:").with_default("llama3").prompt()?;

            let api_key = Text::new("API token (leave empty if not required):")
                .with_help_message(
                    "Used as ANTHROPIC_AUTH_TOKEN. Defaults to \"ollama\" when empty.",
                )
                .prompt_skippable()?
                .and_then(|s| if s.is_empty() { None } else { Some(s) });

            LLMConfig::Ollama {
                endpoint,
                model,
                api_key,
            }
        }
        _ => {
            let endpoint = Text::new("Endpoint URL:")
                .with_default("http://localhost:8080")
                .with_help_message("Any Anthropic-compatible endpoint (local or remote)")
                .prompt()?;

            let model = Text::new("Model name:").with_default("llama3").prompt()?;

            LLMConfig::AnthropicCompatible { endpoint, model }
        }
    };

    Ok(llm_config)
}

/// One-line description of an LLM backend, used to label existing constructs in
/// the router backend picker.
fn llm_summary(cfg: &LLMConfig) -> String {
    match cfg {
        LLMConfig::ClaudeApi { model, .. } => format!("Claude API, {}", model),
        LLMConfig::AnthropicCompatible { endpoint, model } => {
            format!("Anthropic-compatible, {} @ {}", model, endpoint)
        }
        LLMConfig::Ollama {
            endpoint, model, ..
        } => format!("Ollama, {} @ {}", model, endpoint),
        LLMConfig::Router { .. } => "Router".to_string(),
    }
}

/// Pick a single router backend (weak or strong) either by reusing the LLM
/// config of an existing construct, or by defining a new one inline. Existing
/// routers are not offered as candidates since routers cannot be nested.
fn prompt_router_backend(role: &str, candidates: &[Construct]) -> Result<LLMConfig> {
    const INLINE: &str = "<Define a new backend inline>";

    let mut options: Vec<String> = candidates
        .iter()
        .map(|c| format!("{} ({})", c.name, llm_summary(&c.llm_config)))
        .collect();
    options.push(INLINE.to_string());

    let choice = Select::new(&format!("Select {} backend:", role), options.clone()).prompt()?;

    if choice == INLINE {
        let backend_options = vec!["Claude API", "Anthropic-compatible", "Ollama"];
        let llm_choice = Select::new("Select LLM type:", backend_options).prompt()?;
        return build_base_llm(llm_choice);
    }

    let idx = options
        .iter()
        .position(|o| o == &choice)
        .expect("selection must come from the offered options");
    Ok(candidates[idx].llm_config.clone())
}

/// Prompt for a `Router` config: a weak (default) backend, a strong
/// (escalation) backend, and the thresholds that trigger escalation. Each
/// backend can reuse an existing construct's LLM config or be defined inline.
/// Routers cannot be nested, so existing routers are excluded as candidates.
fn prompt_router_config() -> Result<LLMConfig> {
    println!(
        "\nA Router sends each request to a weak backend, escalating to a strong one\n\
         once a request crosses a difficulty threshold.\n"
    );

    let candidates: Vec<Construct> = load_config()
        .map(|c| {
            c.constructs
                .into_iter()
                .filter(|c| !matches!(c.llm_config, LLMConfig::Router { .. }))
                .collect()
        })
        .unwrap_or_default();

    let weak = prompt_router_backend("WEAK (default)", &candidates)?;
    let strong = prompt_router_backend("STRONG (escalation)", &candidates)?;

    let defaults = RouterThresholds::default();

    let max_messages = CustomType::<usize>::new("Escalate when message count exceeds (0 to disable):")
        .with_default(defaults.max_messages)
        .prompt()?;

    let max_input_tokens =
        CustomType::<usize>::new("Escalate when estimated input tokens exceed (0 to disable):")
            .with_default(defaults.max_input_tokens)
            .with_help_message("Estimated from message text as chars / 4")
            .prompt()?;

    let escalate_on_tool_error = Confirm::new("Escalate when a prior tool_result is an error?")
        .with_default(defaults.escalate_on_tool_error)
        .prompt()?;

    Ok(LLMConfig::Router {
        weak: Box::new(weak),
        strong: Box::new(strong),
        thresholds: RouterThresholds {
            max_messages,
            max_input_tokens,
            escalate_on_tool_error,
        },
    })
}

pub fn configure() -> Result<()> {
    println!("=== crig Configuration ===\n");

    let existing_names: Vec<String> = load_config()
        .map(|c| c.constructs.into_iter().map(|c| c.name).collect())
        .unwrap_or_default();

    let profile_name = loop {
        let name = Text::new("Construct name:")
            .with_default("remote")
            .prompt()?;
        if name == "default" {
            println!("\"default\" is reserved. Please use a different name.");
        } else if existing_names.contains(&name) {
            let overwrite = Confirm::new(&format!(
                "Construct '{}' already exists. Overwrite?",
                name
            ))
            .with_default(false)
            .prompt()?;
            if overwrite {
                break name;
            }
        } else {
            break name;
        }
    };

    let agent_options = vec!["Claude Code", "Hermes"];
    let agent_choice = Select::new("Select agent type:", agent_options).prompt()?;
    let agent_type = match agent_choice {
        "Hermes" => AgentType::Hermes,
        _ => AgentType::ClaudeCode,
    };

    let agent_path = Text::new("Agent path:")
        .with_default(agent_type.default_path())
        .with_help_message("Path to the agent CLI executable")
        .prompt()?;

    let llm_config = prompt_llm_config(agent_type)?;

    let construct = Construct {
        name: profile_name.clone(),
        agent_type,
        llm_config,
        agent_path,
    };

    add_or_update_construct(construct)?;

    println!("\n✓ Construct '{}' saved successfully!", profile_name);
    println!("\nNext steps:");
    println!("  - Use 'crig list' to view your constructs in TUI");
    println!("  - Use 'crig list' to list all constructs");
    println!(
        "  - Use 'crig jack {}' to jack into this construct",
        profile_name
    );

    Ok(())
}

