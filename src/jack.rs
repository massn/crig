use anyhow::{Context, Result};
use std::process::{Child, Command};

use crate::config::{get_active_construct, load_config, AgentType, Construct, LLMConfig};
use crate::interactive;
use crate::ollama;

/// Ownership of an auto-started ollama process. Held for the lifetime of
/// a single `jack` invocation so we can shut it down when the agent exits.
enum OllamaHandle {
    /// We did not start ollama (either not needed or it was already running).
    None,
    /// We spawned `ollama serve`; kill it when `jack` ends.
    OwnedChild(Child),
}

/// Result of preparing an Ollama backend: the live endpoint plus any owned
/// `ollama serve` process to tear down on exit.
struct OllamaPrep {
    endpoint: String,
    handle: OllamaHandle,
}

pub fn jack_in(profile_name: Option<&str>, args: &[String]) -> Result<()> {
    // Resolve the construct name: explicit arg > interactive tab-completion prompt.
    let resolved_name = match profile_name {
        Some("default") => None, // use active construct
        Some(name) => Some(name.to_string()),
        None => Some(interactive::prompt_construct_name("Construct to jack into:")?),
    };

    let construct = match resolved_name {
        None => get_active_construct()?,
        Some(name) => {
            let config = load_config()?;
            config
                .constructs
                .iter()
                .find(|p| p.name == name)
                .context(format!("Construct '{}' not found", name))?
                .clone()
        }
    };

    println!(">> Initializing neural connection...");
    println!(">> Construct: {}", construct.name);
    println!(">> Agent: {} ({})", construct.agent_type.display_name(), construct.agent_path);

    establish_connection(&construct, args)?;

    Ok(())
}

fn establish_connection(construct: &Construct, args: &[String]) -> Result<()> {
    let mut cmd = Command::new(&construct.agent_path);

    let ollama_handle = configure_agent(&mut cmd, construct.agent_type, &construct.llm_config)?;

    cmd.args(args);

    if !args.is_empty() {
        println!(">> Payload: {}", args.join(" "));
    }
    println!("\n>> Jacking in...\n");

    let status = cmd
        .status()
        .context("Connection failed - interface unreachable")?;

    if let OllamaHandle::OwnedChild(child) = ollama_handle {
        println!(">> Stopping ollama (started by this session)...");
        let _ = ollama::stop_child(child);
    }

    if !status.success() {
        anyhow::bail!("Connection terminated with status: {}", status);
    }

    Ok(())
}

/// Configure the spawned agent process based on the (agent_type, llm) combination.
/// Returns any owned ollama process so the caller can tear it down at exit.
fn configure_agent(
    cmd: &mut Command,
    agent_type: AgentType,
    llm: &LLMConfig,
) -> Result<OllamaHandle> {
    match agent_type {
        AgentType::ClaudeCode => configure_claude_code(cmd, llm),
        AgentType::Hermes => configure_hermes(cmd, llm),
    }
}

// ---------- Claude Code adapter ----------

fn configure_claude_code(cmd: &mut Command, llm: &LLMConfig) -> Result<OllamaHandle> {
    match llm {
        LLMConfig::ClaudeApi { api_key, model } => {
            clear_bedrock_env(cmd);
            set_no_bedrock(cmd);
            if let Some(key) = api_key {
                cmd.env("ANTHROPIC_API_KEY", key);
            }
            cmd.arg("--model").arg(model);
            println!(">> LLM: {} (Cloud)", model);
            Ok(OllamaHandle::None)
        }
        LLMConfig::AnthropicCompatible { endpoint, model } => {
            println!(">> LLM: {} @ {}", model, endpoint);
            apply_anthropic_env(cmd, endpoint, None);
            cmd.arg("--model").arg(ollama::normalize_model(model));
            Ok(OllamaHandle::None)
        }
        LLMConfig::Ollama {
            endpoint,
            model,
            api_key,
        } => {
            println!(">> LLM: {} @ {} (Ollama)", model, endpoint);
            let prep = prepare_ollama(endpoint, model)?;
            apply_anthropic_env(cmd, &prep.endpoint, api_key.as_deref());
            cmd.arg("--model").arg(ollama::normalize_model(model));
            Ok(prep.handle)
        }
    }
}

// ---------- Hermes adapter ----------
//
// Hermes is invoked as `hermes chat --provider <p> --model <m>`. It does NOT
// read `ANTHROPIC_BASE_URL` for endpoint overrides — those live in
// `~/.hermes/config.yaml`. So we only pass auth env vars and the
// `--provider`/`--model` flags; endpoint configuration is left to Hermes.

fn configure_hermes(cmd: &mut Command, llm: &LLMConfig) -> Result<OllamaHandle> {
    cmd.arg("chat");
    match llm {
        LLMConfig::ClaudeApi { api_key, model } => {
            if let Some(key) = api_key {
                cmd.env("ANTHROPIC_API_KEY", key);
            }
            cmd.arg("--provider").arg("anthropic");
            cmd.arg("--model").arg(model);
            println!(">> LLM: {} via Hermes (provider=anthropic)", model);
            Ok(OllamaHandle::None)
        }
        LLMConfig::Ollama {
            endpoint, model, ..
        } => {
            // We manage the local Ollama server only — Hermes itself is
            // responsible for knowing how to reach it (configured via
            // `hermes model` / ~/.hermes/config.yaml).
            println!(">> LLM: {} via Hermes (provider=ollama)", model);
            let prep = prepare_ollama(endpoint, model)?;
            cmd.arg("--provider").arg("ollama");
            cmd.arg("--model").arg(ollama::normalize_model(model));
            println!(
                ">> Note: Hermes reads its endpoint from ~/.hermes/config.yaml, not from crig.\n\
                 >>       If it cannot reach {}, run `hermes model` to point it there.",
                prep.endpoint
            );
            Ok(prep.handle)
        }
        LLMConfig::AnthropicCompatible { .. } => {
            anyhow::bail!(
                "Hermes does not support endpoint override via environment variables.\n\
                 Configure a custom endpoint with `hermes model` (writes ~/.hermes/config.yaml),\n\
                 then use a different LLM type in crig (e.g. claude_api) or invoke hermes directly."
            )
        }
    }
}

// ---------- shared helpers ----------

/// Bring up (or attach to) an Ollama server reachable at `endpoint`, ensuring
/// `model` is available. For remote endpoints, only reachability is checked.
fn prepare_ollama(endpoint: &str, model: &str) -> Result<OllamaPrep> {
    let handle = if ollama::endpoint_is_local(endpoint) {
        let handle = match ollama::start(endpoint)? {
            ollama::StartOutcome::AlreadyRunning => {
                println!(">> Ollama is already running — leaving it alone.");
                OllamaHandle::None
            }
            ollama::StartOutcome::Started(child) => {
                println!(">> Started `ollama serve` for this session.");
                OllamaHandle::OwnedChild(child)
            }
        };
        ollama::ensure_model(endpoint, model)?;
        handle
    } else {
        if !ollama::is_running(endpoint) {
            anyhow::bail!(
                "Remote Ollama endpoint {} is not reachable. Check the URL, your network, and that the server is up.",
                endpoint
            );
        }
        println!(">> Remote endpoint reachable — skipping local serve / model pull.");
        OllamaHandle::None
    };
    Ok(OllamaPrep {
        endpoint: endpoint.to_string(),
        handle,
    })
}

/// Point a Claude-Code-compatible agent at a non-Anthropic OpenAI/Anthropic
/// compatible endpoint by setting the env vars it reads on startup. Using
/// direct env vars (instead of writing a `--settings` JSON file) avoids
/// leaving secrets on disk and avoids overriding the user's persistent
/// `~/.claude/settings.json`.
fn apply_anthropic_env(cmd: &mut Command, endpoint: &str, api_key: Option<&str>) {
    cmd.env("ANTHROPIC_BASE_URL", endpoint);
    cmd.env("ANTHROPIC_AUTH_TOKEN", api_key.unwrap_or("ollama"));
    cmd.env("ANTHROPIC_API_KEY", "");
    cmd.env("CLAUDE_CODE_USE_BEDROCK", "0");
}

fn set_no_bedrock(cmd: &mut Command) {
    cmd.env("CLAUDE_CODE_USE_BEDROCK", "0");
}

fn clear_bedrock_env(cmd: &mut Command) {
    cmd.env_remove("CLAUDE_CODE_USE_BEDROCK");
}
