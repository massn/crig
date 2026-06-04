use anyhow::{Context, Result};
use std::process::{Child, Command};

use crate::config::{get_active_construct, load_config, AgentType, Construct, LLMConfig};
use crate::interactive;
use crate::ollama;
use crate::proxy;

/// Ownership of an auto-started ollama process. Held for the lifetime of
/// a single `jack` invocation so we can shut it down when the agent exits.
enum OllamaHandle {
    /// We did not start ollama (either not needed or it was already running).
    None,
    /// We spawned `ollama serve`; kill it when `jack` ends.
    OwnedChild(Child),
}

/// Resources started for one `jack` invocation that must be torn down when the
/// agent exits: any owned `ollama serve` processes plus an optional router proxy.
struct Session {
    ollama: Vec<Child>,
    proxy: Option<proxy::ProxyHandle>,
}

impl Session {
    fn empty() -> Self {
        Session {
            ollama: Vec::new(),
            proxy: None,
        }
    }

    fn from_handle(handle: OllamaHandle) -> Self {
        let mut s = Session::empty();
        if let OllamaHandle::OwnedChild(child) = handle {
            s.ollama.push(child);
        }
        s
    }

    fn shutdown(self) {
        if let Some(proxy) = self.proxy {
            println!(">> Stopping router proxy...");
            proxy.stop();
        }
        for child in self.ollama {
            println!(">> Stopping ollama (started by this session)...");
            let _ = ollama::stop_child(child);
        }
    }
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

    let session = configure_agent(&mut cmd, construct.agent_type, &construct.llm_config)?;

    cmd.args(args);

    if !args.is_empty() {
        println!(">> Payload: {}", args.join(" "));
    }
    println!("\n>> Jacking in...\n");

    let status = cmd
        .status()
        .context("Connection failed - interface unreachable")?;

    session.shutdown();

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
) -> Result<Session> {
    match agent_type {
        AgentType::ClaudeCode => configure_claude_code(cmd, llm),
        AgentType::Hermes => configure_hermes(cmd, llm),
    }
}

// ---------- Claude Code adapter ----------

fn configure_claude_code(cmd: &mut Command, llm: &LLMConfig) -> Result<Session> {
    match llm {
        LLMConfig::ClaudeApi { api_key, model } => {
            clear_bedrock_env(cmd);
            set_no_bedrock(cmd);
            if let Some(key) = api_key {
                cmd.env("ANTHROPIC_API_KEY", key);
            }
            cmd.arg("--model").arg(model);
            println!(">> LLM: {} (Cloud)", model);
            Ok(Session::empty())
        }
        LLMConfig::AnthropicCompatible { endpoint, model } => {
            println!(">> LLM: {} @ {}", model, endpoint);
            apply_anthropic_env(cmd, endpoint, None);
            cmd.arg("--model").arg(ollama::normalize_model(model));
            Ok(Session::empty())
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
            Ok(Session::from_handle(prep.handle))
        }
        LLMConfig::Router {
            weak,
            strong,
            thresholds,
        } => {
            let mut session = Session::empty();
            let weak_backend = resolve_backend(weak, &mut session)?;
            let strong_backend = resolve_backend(strong, &mut session)?;
            let placeholder_model = weak_backend.model.clone();

            println!(
                ">> Router: weak={} → strong={} (escalate at msgs>{}, ~tokens>{}, tool_error={})",
                weak_backend.model,
                strong_backend.model,
                thresholds.max_messages,
                thresholds.max_input_tokens,
                thresholds.escalate_on_tool_error,
            );

            let handle = proxy::start(proxy::ResolvedRouter {
                weak: weak_backend,
                strong: strong_backend,
                thresholds: thresholds.clone(),
            })?;
            let url = format!("http://{}", handle.addr);
            println!(">> Router proxy listening on {}", url);

            apply_anthropic_env(cmd, &url, Some("crig-proxy"));
            cmd.arg("--model").arg(placeholder_model);

            session.proxy = Some(handle);
            Ok(session)
        }
    }
}

/// Resolve a single LLM backend into a proxy `Backend`, bringing up any local
/// Ollama server it needs (tracked in `session` for teardown).
fn resolve_backend(llm: &LLMConfig, session: &mut Session) -> Result<proxy::Backend> {
    match llm {
        LLMConfig::ClaudeApi { api_key, model } => {
            let key = api_key
                .clone()
                .or_else(|| std::env::var("ANTHROPIC_API_KEY").ok())
                .filter(|s| !s.is_empty())
                .context(
                    "Router claude_api backend needs an api_key (in config or ANTHROPIC_API_KEY)",
                )?;
            Ok(proxy::Backend {
                base_url: "https://api.anthropic.com".to_string(),
                auth: proxy::Auth::ApiKey(key),
                model: model.clone(),
            })
        }
        LLMConfig::AnthropicCompatible { endpoint, model } => Ok(proxy::Backend {
            base_url: trim_url(endpoint),
            auth: proxy::Auth::Bearer("ollama".to_string()),
            model: ollama::normalize_model(model),
        }),
        LLMConfig::Ollama {
            endpoint,
            model,
            api_key,
        } => {
            let prep = prepare_ollama(endpoint, model)?;
            if let OllamaHandle::OwnedChild(child) = prep.handle {
                session.ollama.push(child);
            }
            Ok(proxy::Backend {
                base_url: trim_url(&prep.endpoint),
                auth: proxy::Auth::Bearer(api_key.clone().unwrap_or_else(|| "ollama".to_string())),
                model: ollama::normalize_model(model),
            })
        }
        LLMConfig::Router { .. } => {
            anyhow::bail!("A Router LLM config cannot be nested inside another Router")
        }
    }
}

fn trim_url(url: &str) -> String {
    url.trim_end_matches('/').to_string()
}

// ---------- Hermes adapter ----------
//
// Hermes is invoked as `hermes chat --provider <p> --model <m>`. It does NOT
// read `ANTHROPIC_BASE_URL` for endpoint overrides — those live in
// `~/.hermes/config.yaml`. So we only pass auth env vars and the
// `--provider`/`--model` flags; endpoint configuration is left to Hermes.

fn configure_hermes(cmd: &mut Command, llm: &LLMConfig) -> Result<Session> {
    cmd.arg("chat");
    match llm {
        LLMConfig::ClaudeApi { api_key, model } => {
            if let Some(key) = api_key {
                cmd.env("ANTHROPIC_API_KEY", key);
            }
            cmd.arg("--provider").arg("anthropic");
            cmd.arg("--model").arg(model);
            println!(">> LLM: {} via Hermes (provider=anthropic)", model);
            Ok(Session::empty())
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
            Ok(Session::from_handle(prep.handle))
        }
        LLMConfig::AnthropicCompatible { .. } => {
            anyhow::bail!(
                "Hermes does not support endpoint override via environment variables.\n\
                 Configure a custom endpoint with `hermes model` (writes ~/.hermes/config.yaml),\n\
                 then use a different LLM type in crig (e.g. claude_api) or invoke hermes directly."
            )
        }
        LLMConfig::Router { .. } => {
            anyhow::bail!(
                "Hermes does not support the router LLM config. Use a claude_code agent for routing."
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
