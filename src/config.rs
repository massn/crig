use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::env;
use std::fs;
use std::path::PathBuf;
use std::sync::OnceLock;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub active_construct: String,
    pub constructs: Vec<Construct>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Construct {
    pub name: String,
    pub agent_type: AgentType,
    pub llm_config: LLMConfig,
    #[serde(default = "default_agent_path", alias = "claude_code_path")]
    pub agent_path: String,
}

fn default_agent_path() -> String {
    "claude".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum AgentType {
    ClaudeCode,
    Custom,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum LLMConfig {
    ClaudeApi {
        api_key: Option<String>,
        model: String,
    },
    LocalLLM {
        endpoint: String,
        model: String,
    },
    Ollama {
        endpoint: String,
        model: String,
    },
}

pub const DEFAULT_OLLAMA_ENDPOINT: &str = "http://localhost:11434";

impl Default for Config {
    fn default() -> Self {
        Config {
            active_construct: "remote".to_string(),
            constructs: vec![Construct {
                name: "remote".to_string(),
                agent_type: AgentType::ClaudeCode,
                llm_config: LLMConfig::ClaudeApi {
                    api_key: None,
                    model: "claude-sonnet-4-6".to_string(),
                },
                agent_path: "claude".to_string(),
            }],
        }
    }
}

static CUSTOM_CONFIG_PATH: OnceLock<PathBuf> = OnceLock::new();

pub fn set_config_path(path: PathBuf) {
    CUSTOM_CONFIG_PATH.get_or_init(|| path);
}

pub fn get_config_dir() -> Result<PathBuf> {
    if let Some(custom_path) = CUSTOM_CONFIG_PATH.get() {
        return Ok(custom_path
            .parent()
            .context("Invalid config path")?
            .to_path_buf());
    }

    let home = env::var("HOME").context("HOME environment variable not set")?;
    Ok(PathBuf::from(home).join(".config").join("crig"))
}

pub fn get_config_path() -> Result<PathBuf> {
    if let Some(custom_path) = CUSTOM_CONFIG_PATH.get() {
        return Ok(custom_path.clone());
    }

    Ok(get_config_dir()?.join("config.toml"))
}

fn ensure_config_dir_exists() -> Result<()> {
    let config_dir = get_config_dir()?;
    if !config_dir.exists() {
        fs::create_dir_all(&config_dir).context("Failed to create config directory")?;
    }
    Ok(())
}

pub fn load_config() -> Result<Config> {
    let config_path = get_config_path()?;

    if !config_path.exists() {
        anyhow::bail!(
            "No config found. Run 'crig config' to create one."
        );
    }

    let content = fs::read_to_string(&config_path).context("Failed to read config file")?;
    toml::from_str(&content).context("Failed to parse config file")
}

pub fn save_config(config: &Config) -> Result<()> {
    ensure_config_dir_exists()?;

    let config_path = get_config_path()?;
    let content = toml::to_string_pretty(config)?;

    fs::write(&config_path, content).context("Failed to write config file")
}

pub fn get_active_construct() -> Result<Construct> {
    let config = load_config()?;
    config
        .constructs
        .iter()
        .find(|c| c.name == config.active_construct)
        .cloned()
        .context("Active construct not found")
}

pub fn add_or_update_construct(construct: Construct) -> Result<()> {
    let mut config = load_config().unwrap_or_default();

    if let Some(existing) = config.constructs.iter_mut().find(|c| c.name == construct.name) {
        *existing = construct;
    } else {
        config.constructs.push(construct);
    }

    save_config(&config)
}

pub fn remove_construct(name: &str) -> Result<()> {
    let mut config = load_config()?;

    let original_len = config.constructs.len();
    config.constructs.retain(|c| c.name != name);

    if config.constructs.len() == original_len {
        anyhow::bail!("Construct '{}' not found", name);
    }

    if config.active_construct == name {
        if let Some(first) = config.constructs.first() {
            config.active_construct = first.name.clone();
        } else {
            config.active_construct = String::new();
        }
    }

    save_config(&config)
}
