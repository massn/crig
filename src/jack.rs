use anyhow::{Context, Result};
use serde_json::json;
use std::fs;
use std::path::PathBuf;
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

/// Return value bundles the temporary settings path to clean up *and* any
/// ollama process we started so the caller can tear both down together.
struct ConfigureOutcome {
    settings_path: Option<PathBuf>,
    ollama: OllamaHandle,
}

trait LLMBackend {
    fn configure(&self, cmd: &mut Command, construct_name: &str) -> Result<ConfigureOutcome>;
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
}

impl LLMBackend for ClaudeApiBackend {
    fn configure(&self, cmd: &mut Command, _construct_name: &str) -> Result<ConfigureOutcome> {
        clear_bedrock_env(cmd);
        set_no_bedrock(cmd);
        if let Some(key) = &self.api_key {
            cmd.env("ANTHROPIC_API_KEY", key);
        }
        cmd.arg("--model").arg(&self.model);
        println!(">> LLM: {} (Cloud)", self.model);
        Ok(ConfigureOutcome { settings_path: None, ollama: OllamaHandle::None })
    }
}

impl LLMBackend for LocalLLMBackend {
    fn configure(&self, cmd: &mut Command, construct_name: &str) -> Result<ConfigureOutcome> {
        println!(">> LLM: {} @ {}", self.model, self.endpoint);
        println!(">> Generating temporary settings.json for local LLM...");

        let settings_path = generate_ollama_settings(construct_name, &self.endpoint, &self.model)?;
        println!(">> Settings file: {}", settings_path.display());
        println!(">> Model: {}", self.model);

        cmd.arg("--settings").arg(&settings_path);

        Ok(ConfigureOutcome {
            settings_path: Some(settings_path),
            ollama: OllamaHandle::None,
        })
    }
}

impl LLMBackend for OllamaBackend {
    fn configure(&self, cmd: &mut Command, construct_name: &str) -> Result<ConfigureOutcome> {
        println!(">> LLM: {} @ {} (Ollama)", self.model, self.endpoint);

        // Ensure ollama is reachable, starting it if necessary.
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

        // Ensure the requested model is present locally; pull on demand.
        ollama::ensure_model(&self.endpoint, &self.model)?;

        let settings_path = generate_ollama_settings(construct_name, &self.endpoint, &self.model)?;
        println!(">> Settings file: {}", settings_path.display());
        println!(">> Model: {}", self.model);

        cmd.arg("--settings").arg(&settings_path);

        Ok(ConfigureOutcome {
            settings_path: Some(settings_path),
            ollama: handle,
        })
    }
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
        LLMConfig::Ollama { endpoint, model } => Box::new(OllamaBackend {
            endpoint: endpoint.clone(),
            model: model.clone(),
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

    let outcome = backend.configure(&mut cmd, &construct.name)?;

    cmd.args(args);

    if !args.is_empty() {
        println!(">> Payload: {}", args.join(" "));
    }
    println!("\n>> Jacking in...\n");

    let status = cmd
        .status()
        .context("Connection failed - interface unreachable")?;

    // Tear down any resources we created for this session, in reverse order.
    if let Some(path) = outcome.settings_path {
        let _ = fs::remove_file(path);
    }
    if let OllamaHandle::OwnedChild(child) = outcome.ollama {
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

fn generate_ollama_settings(profile_name: &str, endpoint: &str, model: &str) -> Result<PathBuf> {
    let config_dir = crate::config::get_config_dir()?;
    let settings_path = config_dir.join(format!("settings_{}.json", profile_name));

    let full_model = if model.contains(':') {
        model.to_string()
    } else {
        format!("{}:latest", model)
    };

    let settings = json!({
        "model": &full_model,
        "env": {
            "ANTHROPIC_AUTH_TOKEN": "ollama",
            "ANTHROPIC_API_KEY": "",
            "ANTHROPIC_BASE_URL": endpoint,
            "CLAUDE_CODE_USE_BEDROCK": "0"
        }
    });

    let settings_str = serde_json::to_string_pretty(&settings)?;
    fs::write(&settings_path, settings_str)
        .context("Failed to write settings.json")?;

    Ok(settings_path)
}
