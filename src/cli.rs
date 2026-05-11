use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "crig")]
#[command(about = "construct-rig: Manage LLM and Agent configurations", long_about = None)]
pub struct Cli {
    #[arg(
        long,
        global = true,
        help = "Path to config file (default: ~/.config/crig/config.toml)"
    )]
    pub config_path: Option<PathBuf>,

    #[command(subcommand)]
    pub command: Option<Commands>,
}

#[derive(Subcommand)]
pub enum Commands {
    #[command(about = "Configure construct settings interactively (or manage existing ones)")]
    Config {
        #[command(subcommand)]
        action: Option<ConfigAction>,
    },

    #[command(about = "List all available constructs in TUI")]
    List,

    #[command(about = "Jack into a construct (omit name for tab-completion prompt, use 'default' for active construct)")]
    Jack {
        #[arg(help = "Construct name to jack into (omit for tab-completion prompt, 'default' for active construct)")]
        construct: Option<String>,

        #[arg(
            trailing_var_arg = true,
            allow_hyphen_values = true,
            help = "Neural payload to transmit"
        )]
        args: Vec<String>,
    },

    #[command(about = "Control the local ollama server")]
    Ollama {
        #[command(subcommand)]
        action: OllamaAction,
    },
}

#[derive(Subcommand)]
pub enum ConfigAction {
    #[command(about = "Remove a construct from the config")]
    Remove {
        #[arg(help = "Construct name to remove (omit to prompt with tab-completion)")]
        name: Option<String>,
    },
}

#[derive(Subcommand)]
pub enum OllamaAction {
    #[command(about = "Start ollama serve in detached mode")]
    Start {
        #[arg(long, help = "Endpoint URL (default: http://localhost:11434)")]
        endpoint: Option<String>,
    },
    #[command(about = "Stop the ollama server started by `crig ollama start`")]
    Stop {
        #[arg(long, help = "Endpoint URL (default: http://localhost:11434)")]
        endpoint: Option<String>,
    },
    #[command(about = "Show whether ollama is running")]
    Status {
        #[arg(long, help = "Endpoint URL (default: http://localhost:11434)")]
        endpoint: Option<String>,
    },
    #[command(about = "Pull a model into the ollama server")]
    Pull {
        #[arg(help = "Model name (e.g. llama3, llama3:8b)")]
        model: String,

        #[arg(long, help = "Endpoint URL (default: http://localhost:11434)")]
        endpoint: Option<String>,
    },
}
