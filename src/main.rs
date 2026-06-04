mod cli;
mod config;
mod interactive;
mod jack;
mod ollama;
mod proxy;
mod tui;

use anyhow::Result;
use clap::Parser;
use cli::{Cli, Commands, ConfigAction, OllamaAction};

fn main() -> Result<()> {
    let cli = Cli::parse();

    if let Some(config_path) = cli.config_path {
        config::set_config_path(config_path);
    }

    match cli.command {
        Some(Commands::Config { action }) => match action {
            None => interactive::configure()?,
            Some(ConfigAction::Remove { name }) => interactive::remove(name)?,
        },
        Some(Commands::List) => {
            tui::show_config()?;
        }
        Some(Commands::Jack { construct, args }) => {
            jack::jack_in(construct.as_deref(), &args)?;
        }
        Some(Commands::Ollama { action }) => match action {
            OllamaAction::Start { endpoint } => ollama::cmd_start(endpoint)?,
            OllamaAction::Stop { endpoint } => ollama::cmd_stop(endpoint)?,
            OllamaAction::Status { endpoint } => ollama::cmd_status(endpoint)?,
            OllamaAction::Pull { model, endpoint } => ollama::cmd_pull(endpoint, model)?,
        },
        None => {
            println!("Use 'crig --help' to see available commands");
        }
    }

    Ok(())
}
