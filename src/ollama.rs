use anyhow::{Context, Result};
use std::fs;
use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use crate::config::{get_config_dir, DEFAULT_OLLAMA_ENDPOINT};

const HEALTH_CHECK_TIMEOUT: Duration = Duration::from_millis(500);
const START_WAIT_TIMEOUT: Duration = Duration::from_secs(15);
const START_POLL_INTERVAL: Duration = Duration::from_millis(250);

/// Outcome of attempting to start Ollama.
pub enum StartOutcome {
    /// Ollama was already reachable.
    AlreadyRunning,
    /// Ollama was started by this call. The `Child` stays alive as long as
    /// this value is held; dropping it without `.wait()` leaves the process
    /// running, so callers should either keep it or call `stop_child`.
    Started(Child),
}

/// Parse `http(s)://host:port` into `host:port` suitable for `OLLAMA_HOST`
/// and for TCP probing. Returns `(host, port)`.
///
/// Default port depends on scheme: 443 for `https://`, 11434 otherwise.
fn parse_endpoint(endpoint: &str) -> Result<(String, u16)> {
    let (is_https, without_scheme) = if let Some(rest) = endpoint.strip_prefix("https://") {
        (true, rest)
    } else if let Some(rest) = endpoint.strip_prefix("http://") {
        (false, rest)
    } else {
        (false, endpoint)
    };
    // Drop any trailing path.
    let authority = without_scheme.split('/').next().unwrap_or(without_scheme);
    let default_port = if is_https { 443 } else { 11434 };
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) => (
            h.to_string(),
            p.parse::<u16>()
                .with_context(|| format!("Invalid port in endpoint: {}", endpoint))?,
        ),
        None => (authority.to_string(), default_port),
    };
    Ok((host, port))
}

/// Returns true if `host` refers to the local machine (loopback or unspecified
/// address). For remote hosts crig must not try to spawn `ollama serve` or
/// invoke `ollama show`/`pull`, since those are local operations.
pub fn is_local_host(host: &str) -> bool {
    matches!(
        host,
        "localhost" | "127.0.0.1" | "::1" | "0.0.0.0" | "[::1]" | "[::]"
    )
}

/// Convenience: parse the endpoint and answer the locality question.
pub fn endpoint_is_local(endpoint: &str) -> bool {
    match parse_endpoint(endpoint) {
        Ok((host, _)) => is_local_host(&host),
        Err(_) => false,
    }
}

/// Returns true if a TCP connection to the endpoint's host:port succeeds.
pub fn is_running(endpoint: &str) -> bool {
    let Ok((host, port)) = parse_endpoint(endpoint) else {
        return false;
    };
    let Ok(mut addrs) = (host.as_str(), port).to_socket_addrs() else {
        return false;
    };
    addrs.any(|addr| TcpStream::connect_timeout(&addr, HEALTH_CHECK_TIMEOUT).is_ok())
}

fn pid_file_path() -> Result<PathBuf> {
    Ok(get_config_dir()?.join("ollama.pid"))
}

fn write_pid_file(pid: u32) -> Result<()> {
    let path = pid_file_path()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).ok();
    }
    let mut f = fs::File::create(&path)
        .with_context(|| format!("Failed to write PID file at {}", path.display()))?;
    writeln!(f, "{}", pid)?;
    Ok(())
}

fn read_pid_file() -> Result<Option<u32>> {
    let path = pid_file_path()?;
    if !path.exists() {
        return Ok(None);
    }
    let mut s = String::new();
    fs::File::open(&path)?.read_to_string(&mut s)?;
    let pid = s.trim().parse::<u32>().ok();
    Ok(pid)
}

fn remove_pid_file() {
    if let Ok(path) = pid_file_path() {
        let _ = fs::remove_file(path);
    }
}

/// Start `ollama serve` if it is not already reachable at `endpoint`.
/// Returns `AlreadyRunning` when a health check succeeds without spawning.
pub fn start(endpoint: &str) -> Result<StartOutcome> {
    if is_running(endpoint) {
        return Ok(StartOutcome::AlreadyRunning);
    }

    let (host, port) = parse_endpoint(endpoint)?;
    let ollama_host = format!("{}:{}", host, port);

    let child = Command::new("ollama")
        .arg("serve")
        .env("OLLAMA_HOST", &ollama_host)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context(
            "Failed to spawn `ollama serve`. Is ollama installed and on your PATH?",
        )?;

    // Wait until the health check passes (or we time out).
    let start = Instant::now();
    while start.elapsed() < START_WAIT_TIMEOUT {
        if is_running(endpoint) {
            return Ok(StartOutcome::Started(child));
        }
        std::thread::sleep(START_POLL_INTERVAL);
    }

    // Did not become healthy in time — kill the child so we do not orphan it.
    let mut child = child;
    let _ = child.kill();
    let _ = child.wait();
    anyhow::bail!(
        "Ollama did not become reachable at {} within {}s",
        endpoint,
        START_WAIT_TIMEOUT.as_secs()
    );
}

/// Outcome of the detached start path.
pub enum DetachedStartOutcome {
    AlreadyRunning,
    Started { pid: u32 },
}

/// Start `ollama serve` in detached mode (for `crig ollama start`).
/// Writes a PID file so `crig ollama stop` can find and terminate it later.
pub fn start_detached(endpoint: &str) -> Result<DetachedStartOutcome> {
    if is_running(endpoint) {
        return Ok(DetachedStartOutcome::AlreadyRunning);
    }

    let (host, port) = parse_endpoint(endpoint)?;
    let ollama_host = format!("{}:{}", host, port);

    let child = Command::new("ollama")
        .arg("serve")
        .env("OLLAMA_HOST", &ollama_host)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context(
            "Failed to spawn `ollama serve`. Is ollama installed and on your PATH?",
        )?;

    let pid = child.id();
    write_pid_file(pid)?;

    // Release the Child handle so the OS process outlives crig. The PID file
    // is how `crig ollama stop` will later terminate it.
    drop(child);

    let start = Instant::now();
    while start.elapsed() < START_WAIT_TIMEOUT {
        if is_running(endpoint) {
            return Ok(DetachedStartOutcome::Started { pid });
        }
        std::thread::sleep(START_POLL_INTERVAL);
    }

    anyhow::bail!(
        "Ollama did not become reachable at {} within {}s",
        endpoint,
        START_WAIT_TIMEOUT.as_secs()
    );
}

/// Terminate a child process started via `start` (foreground mode).
pub fn stop_child(mut child: Child) -> Result<()> {
    let _ = child.kill();
    let _ = child.wait();
    Ok(())
}

/// Stop the detached ollama process recorded in the PID file.
/// Returns true if a process was signalled.
pub fn stop_detached() -> Result<bool> {
    let Some(pid) = read_pid_file()? else {
        return Ok(false);
    };
    // Use `kill` command for portability (Unix). Windows is not supported here.
    let status = Command::new("kill")
        .arg(pid.to_string())
        .status()
        .context("Failed to invoke `kill`")?;
    remove_pid_file();
    Ok(status.success())
}

/// CLI handler: `crig ollama start`.
pub fn cmd_start(endpoint: Option<String>) -> Result<()> {
    let endpoint = endpoint.unwrap_or_else(|| DEFAULT_OLLAMA_ENDPOINT.to_string());
    match start_detached(&endpoint)? {
        DetachedStartOutcome::AlreadyRunning => {
            println!(">> Ollama is already running at {}", endpoint);
        }
        DetachedStartOutcome::Started { pid } => {
            println!(
                ">> Started `ollama serve` (detached, pid={}) at {}",
                pid, endpoint
            );
            if let Ok(path) = pid_file_path() {
                println!(">> PID file: {}", path.display());
            }
        }
    }
    Ok(())
}

/// CLI handler: `crig ollama stop`.
pub fn cmd_stop(endpoint: Option<String>) -> Result<()> {
    let endpoint = endpoint.unwrap_or_else(|| DEFAULT_OLLAMA_ENDPOINT.to_string());
    let stopped = stop_detached()?;
    if stopped {
        println!(">> Stopped ollama (from PID file)");
    } else if is_running(&endpoint) {
        println!(
            ">> No PID file found, but ollama is still reachable at {}.",
            endpoint
        );
        println!(">> It was not started by `crig ollama start`; not stopping.");
    } else {
        println!(">> Ollama is not running.");
    }
    Ok(())
}

/// Normalize a model name by appending `:latest` if no tag is specified.
/// Matches the convention used when generating settings for Claude Code.
pub fn normalize_model(model: &str) -> String {
    if model.contains(':') {
        model.to_string()
    } else {
        format!("{}:latest", model)
    }
}

/// Returns true if `ollama show <model>` succeeds against the given endpoint.
/// Treats any failure (missing model, ollama unreachable, etc.) as "not present".
pub fn is_model_present(endpoint: &str, model: &str) -> Result<bool> {
    let (host, port) = parse_endpoint(endpoint)?;
    let ollama_host = format!("{}:{}", host, port);
    let status = Command::new("ollama")
        .arg("show")
        .arg(model)
        .env("OLLAMA_HOST", &ollama_host)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .context("Failed to invoke `ollama show`. Is ollama installed and on your PATH?")?;
    Ok(status.success())
}

/// Run `ollama pull <model>` against the given endpoint. Inherits stdout/stderr
/// so the user sees live progress output.
pub fn pull(endpoint: &str, model: &str) -> Result<()> {
    let (host, port) = parse_endpoint(endpoint)?;
    let ollama_host = format!("{}:{}", host, port);
    let status = Command::new("ollama")
        .arg("pull")
        .arg(model)
        .env("OLLAMA_HOST", &ollama_host)
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .context("Failed to invoke `ollama pull`. Is ollama installed and on your PATH?")?;
    if !status.success() {
        anyhow::bail!("`ollama pull {}` failed with status: {}", model, status);
    }
    Ok(())
}

/// Ensure the given model is available locally, pulling it if not.
/// Requires that ollama is already reachable at `endpoint`.
pub fn ensure_model(endpoint: &str, model: &str) -> Result<()> {
    let full_model = normalize_model(model);
    if is_model_present(endpoint, &full_model)? {
        println!(">> Model '{}' is already available.", full_model);
        return Ok(());
    }
    println!(">> Model '{}' not found locally — pulling...", full_model);
    pull(endpoint, &full_model)?;
    println!(">> Pull complete.");
    Ok(())
}

/// CLI handler: `crig ollama pull`.
pub fn cmd_pull(endpoint: Option<String>, model: String) -> Result<()> {
    let endpoint = endpoint.unwrap_or_else(|| DEFAULT_OLLAMA_ENDPOINT.to_string());
    if !is_running(&endpoint) {
        anyhow::bail!(
            "Ollama is not reachable at {}. Run `crig ollama start` first.",
            endpoint
        );
    }
    let full_model = normalize_model(&model);
    println!(">> Pulling '{}' from {}...", full_model, endpoint);
    pull(&endpoint, &full_model)?;
    println!(">> Pull complete.");
    Ok(())
}

/// CLI handler: `crig ollama status`.
pub fn cmd_status(endpoint: Option<String>) -> Result<()> {
    let endpoint = endpoint.unwrap_or_else(|| DEFAULT_OLLAMA_ENDPOINT.to_string());
    let running = is_running(&endpoint);
    let pid = read_pid_file().ok().flatten();

    if running {
        println!(">> Ollama: running ({})", endpoint);
    } else {
        println!(">> Ollama: not running ({})", endpoint);
    }
    if let Some(pid) = pid {
        println!(">> PID file: pid={}", pid);
    }
    Ok(())
}
