use anyhow::{Context, Result};
use std::process::{Child, Command};

use crate::config::{load_config, get_active_construct, LLMConfig, Construct};
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

trait LLMBackend {
    /// Configure the spawned agent process for this backend. Returns any
    /// ollama process we own so the caller can tear it down at exit.
    fn configure(&self, cmd: &mut Command, construct_name: &str) -> Result<OllamaHandle>;
}

struct ClaudeApiBackend {
    api_key: Option<String>,
    model: String,
}

struct LocalLLMBackend {
    endpoint: String,
    model: String,
}

struct OllamaBackend {
    endpoint: String,
    model: String,
    api_key: Option<String>,
}

impl LLMBackend for ClaudeApiBackend {
    fn configure(&self, cmd: &mut Command, _construct_name: &str) -> Result<OllamaHandle> {
        clear_bedrock_env(cmd);
        set_no_bedrock(cmd);
        if let Some(key) = &self.api_key {
            cmd.env("ANTHROPIC_API_KEY", key);
        }
        cmd.arg("--model").arg(&self.model);
        println!(">> LLM: {} (Cloud)", self.model);
        Ok(OllamaHandle::None)
    }
}

impl LLMBackend for LocalLLMBackend {
    fn configure(&self, cmd: &mut Command, _construct_name: &str) -> Result<OllamaHandle> {
        println!(">> LLM: {} @ {}", self.model, self.endpoint);
        apply_anthropic_env(cmd, &self.endpoint, None);
        cmd.arg("--model").arg(ollama::normalize_model(&self.model));
        Ok(OllamaHandle::None)
    }
}

impl LLMBackend for OllamaBackend {
    fn configure(&self, cmd: &mut Command, _construct_name: &str) -> Result<OllamaHandle> {
        println!(">> LLM: {} @ {} (Ollama)", self.model, self.endpoint);

        let handle = if ollama::endpoint_is_local(&self.endpoint) {
            // Local Ollama: start `ollama serve` if needed, and ensure the model.
            let handle = match ollama::start(&self.endpoint)? {
                ollama::StartOutcome::AlreadyRunning => {
                    println!(">> Ollama is already running — leaving it alone.");
                    OllamaHandle::None
                }
                ollama::StartOutcome::Started(child) => {
                    println!(">> Started `ollama serve` for this session.");
                    OllamaHandle::OwnedChild(child)
                }
            };
            ollama::ensure_model(&self.endpoint, &self.model)?;
            handle
        } else {
            // Remote endpoint: do not spawn ollama or pull models locally.
            // Just verify the server is reachable.
            if !ollama::is_running(&self.endpoint) {
                anyhow::bail!(
                    "Remote Ollama endpoint {} is not reachable. Check the URL, your network, and that the server is up.",
                    self.endpoint
                );
            }
            println!(">> Remote endpoint reachable — skipping local serve / model pull.");
            OllamaHandle::None
        };

        apply_anthropic_env(cmd, &self.endpoint, self.api_key.as_deref());
        cmd.arg("--model").arg(ollama::normalize_model(&self.model));

        Ok(handle)
    }
}

/// Point Claude Code at a non-Anthropic OpenAI-compatible endpoint by setting
/// the env vars it reads on startup. Using direct env vars (instead of writing
/// a `--settings` JSON file) avoids leaving secrets on disk and avoids
/// overriding the user's persistent `~/.claude/settings.json`.
fn apply_anthropic_env(cmd: &mut Command, endpoint: &str, api_key: Option<&str>) {
    cmd.env("ANTHROPIC_BASE_URL", endpoint);
    cmd.env("ANTHROPIC_AUTH_TOKEN", api_key.unwrap_or("ollama"));
    cmd.env("ANTHROPIC_API_KEY", "");
    cmd.env("CLAUDE_CODE_USE_BEDROCK", "0");
}

fn backend_from_config(llm_config: &LLMConfig) -> Box<dyn LLMBackend> {
    match llm_config {
        LLMConfig::ClaudeApi { api_key, model } => Box::new(ClaudeApiBackend {
            api_key: api_key.clone(),
            model: model.clone(),
        }),
        LLMConfig::LocalLLM { endpoint, model } => Box::new(LocalLLMBackend {
            endpoint: endpoint.clone(),
            model: model.clone(),
        }),
        LLMConfig::Ollama { endpoint, model, api_key } => Box::new(OllamaBackend {
            endpoint: endpoint.clone(),
            model: model.clone(),
            api_key: api_key.clone(),
        }),
    }
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
    println!(">> Interface path: {}", construct.agent_path);

    establish_connection(&construct, args)?;

    Ok(())
}

fn establish_connection(construct: &Construct, args: &[String]) -> Result<()> {
    let mut cmd = Command::new(&construct.agent_path);
    let backend = backend_from_config(&construct.llm_config);

    let ollama_handle = backend.configure(&mut cmd, &construct.name)?;

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

fn set_no_bedrock(cmd: &mut Command) {
    cmd.env("CLAUDE_CODE_USE_BEDROCK", "0");
}

fn clear_bedrock_env(cmd: &mut Command) {
    cmd.env_remove("CLAUDE_CODE_USE_BEDROCK");
}

