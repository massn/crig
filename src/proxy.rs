//! Rule-based routing proxy for `LLMConfig::Router`.
//!
//! Speaks the Anthropic Messages API. The spawned agent points its
//! `ANTHROPIC_BASE_URL` at this proxy; for every `/v1/messages` request we
//! inspect the conversation and forward it to either the `weak` or `strong`
//! backend, rewriting the `model` field and auth headers for the chosen
//! target. Responses (including SSE streams) are passed straight back.

use anyhow::{Context, Result};
use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::Method;
use axum::response::Response;
use axum::routing::any;
use axum::Router as AxumRouter;
use serde_json::Value;
use std::net::SocketAddr;
use std::sync::mpsc;
use std::sync::Arc;
use std::thread::JoinHandle;
use tokio::sync::oneshot;

use crate::config::RouterThresholds;

/// How to authenticate against a backend.
#[derive(Clone)]
pub enum Auth {
    /// Anthropic-style `x-api-key` header (the real Claude API).
    ApiKey(String),
    /// `Authorization: Bearer <token>` (Ollama / Anthropic-compatible shims).
    Bearer(String),
}

/// A concrete LLM endpoint the proxy can forward to.
#[derive(Clone)]
pub struct Backend {
    /// Base URL without a trailing slash, e.g. `https://api.anthropic.com`.
    pub base_url: String,
    pub auth: Auth,
    /// Model name to inject into the outgoing request body.
    pub model: String,
}

/// Fully resolved routing configuration handed to the proxy.
pub struct ResolvedRouter {
    pub weak: Backend,
    pub strong: Backend,
    pub thresholds: RouterThresholds,
}

struct AppState {
    router: ResolvedRouter,
    client: reqwest::Client,
}

/// Handle to a running proxy. Drop via [`ProxyHandle::stop`] to shut it down.
pub struct ProxyHandle {
    pub addr: SocketAddr,
    shutdown: Option<oneshot::Sender<()>>,
    thread: Option<JoinHandle<()>>,
}

impl ProxyHandle {
    pub fn stop(mut self) {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Start the proxy on an ephemeral localhost port, returning once it is bound.
pub fn start(router: ResolvedRouter) -> Result<ProxyHandle> {
    let (addr_tx, addr_rx) = mpsc::channel::<SocketAddr>();
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();

    let thread = std::thread::spawn(move || {
        let rt = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(rt) => rt,
            Err(e) => {
                eprintln!(">> [crig proxy] failed to start tokio runtime: {e}");
                return;
            }
        };

        rt.block_on(async move {
            let state = Arc::new(AppState {
                router,
                client: reqwest::Client::new(),
            });
            let app = AxumRouter::new().fallback(any(handle)).with_state(state);

            let listener = match tokio::net::TcpListener::bind(("127.0.0.1", 0)).await {
                Ok(l) => l,
                Err(e) => {
                    eprintln!(">> [crig proxy] failed to bind: {e}");
                    return;
                }
            };
            if let Ok(addr) = listener.local_addr() {
                let _ = addr_tx.send(addr);
            }

            let _ = axum::serve(listener, app)
                .with_graceful_shutdown(async move {
                    let _ = shutdown_rx.await;
                })
                .await;
        });
    });

    let addr = addr_rx
        .recv()
        .context("router proxy failed to bind a port")?;

    Ok(ProxyHandle {
        addr,
        shutdown: Some(shutdown_tx),
        thread: Some(thread),
    })
}

async fn handle(State(state): State<Arc<AppState>>, req: Request) -> Response {
    match forward(state, req).await {
        Ok(resp) => resp,
        Err(e) => Response::builder()
            .status(502)
            .body(Body::from(format!("crig proxy error: {e}")))
            .expect("static error response"),
    }
}

async fn forward(state: Arc<AppState>, req: Request) -> Result<Response> {
    let (parts, body) = req.into_parts();
    let path = parts.uri.path().to_string();
    let query = parts
        .uri
        .query()
        .map(|q| format!("?{q}"))
        .unwrap_or_default();
    let method = parts.method.clone();

    let bytes = axum::body::to_bytes(body, usize::MAX)
        .await
        .context("failed to read request body")?;
    let json: Option<Value> = serde_json::from_slice(&bytes).ok();

    // Route only real message requests; everything else (token counts, model
    // listings) goes to the weak backend untouched.
    let is_messages = method == Method::POST && path.ends_with("/v1/messages");
    let backend = match (is_messages, &json) {
        (true, Some(j)) => route(j, &state.router),
        _ => &state.router.weak,
    };

    // Rewrite the model for the chosen backend.
    let out_body: Vec<u8> = match &json {
        Some(j) if j.get("model").is_some() => {
            let mut j = j.clone();
            j["model"] = Value::String(backend.model.clone());
            serde_json::to_vec(&j).context("failed to re-encode request body")?
        }
        _ => bytes.to_vec(),
    };

    let url = format!("{}{}{}", backend.base_url, path, query);
    let mut rb = state.client.request(method, &url).body(out_body);

    // Pass through the headers the upstream actually needs; drop the client's
    // auth headers since we set our own per backend.
    for (name, value) in parts.headers.iter() {
        let n = name.as_str().to_ascii_lowercase();
        if matches!(
            n.as_str(),
            "anthropic-version" | "anthropic-beta" | "content-type" | "accept"
        ) {
            rb = rb.header(name, value);
        }
    }
    rb = match &backend.auth {
        Auth::ApiKey(k) => rb.header("x-api-key", k),
        Auth::Bearer(t) => rb.header("authorization", format!("Bearer {t}")),
    };

    let upstream = rb.send().await.context("upstream request failed")?;

    let mut builder = Response::builder().status(upstream.status());
    for (name, value) in upstream.headers().iter() {
        let n = name.as_str().to_ascii_lowercase();
        if matches!(n.as_str(), "content-length" | "transfer-encoding" | "connection") {
            continue;
        }
        builder = builder.header(name, value);
    }

    let body = Body::from_stream(upstream.bytes_stream());
    builder.body(body).context("failed to build proxy response")
}

/// Pick a backend for a `/v1/messages` request body.
fn route<'a>(body: &Value, r: &'a ResolvedRouter) -> &'a Backend {
    let t = &r.thresholds;
    let msgs = body.get("messages").and_then(|m| m.as_array());

    let mut escalate = false;
    let mut nmsgs = 0;
    let mut approx_tokens = 0;

    if let Some(arr) = msgs {
        nmsgs = arr.len();
        approx_tokens = estimate_chars(arr) / 4;

        if t.max_messages > 0 && nmsgs > t.max_messages {
            escalate = true;
        }
        if t.max_input_tokens > 0 && approx_tokens > t.max_input_tokens {
            escalate = true;
        }
        if t.escalate_on_tool_error && has_tool_error(arr) {
            escalate = true;
        }
    }

    let pick = if escalate { "strong" } else { "weak" };
    eprintln!(">> [crig proxy] route={pick} msgs={nmsgs} ~tokens={approx_tokens}");

    if escalate {
        &r.strong
    } else {
        &r.weak
    }
}

/// Rough character count of all message text content (string or block array).
fn estimate_chars(arr: &[Value]) -> usize {
    let mut n = 0;
    for m in arr {
        match &m["content"] {
            Value::String(s) => n += s.len(),
            Value::Array(blocks) => {
                for b in blocks {
                    if let Some(s) = b.get("text").and_then(|t| t.as_str()) {
                        n += s.len();
                    } else if let Some(s) = b.get("content").and_then(|c| c.as_str()) {
                        n += s.len();
                    }
                }
            }
            _ => {}
        }
    }
    n
}

/// True if any prior tool_result in the history is flagged as an error.
fn has_tool_error(arr: &[Value]) -> bool {
    for m in arr {
        if let Value::Array(blocks) = &m["content"] {
            for b in blocks {
                let is_tool_result =
                    b.get("type").and_then(|t| t.as_str()) == Some("tool_result");
                let is_error = b.get("is_error").and_then(|e| e.as_bool()) == Some(true);
                if is_tool_result && is_error {
                    return true;
                }
            }
        }
    }
    false
}
