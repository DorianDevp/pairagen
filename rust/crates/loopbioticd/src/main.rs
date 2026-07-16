use std::collections::VecDeque;
use std::io::{self, BufRead, Write};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Result;
use loopbiotic_backends::{
    BackendAdapter, ClaudeAppBackend, CodexAppBackend, GenericCliBackend, MockBackend,
    OllamaBackend, ProgressReporter, StdioAgentBackend,
};
use loopbiotic_harness::{Engine, LocationGranter, PrefetchMode, SourceContextProvider};
use loopbiotic_protocol::{
    ActionParams, BackendInfo, ContextBundle, JsonRpcNotification, JsonRpcRequest, JsonRpcResponse,
    PatchApplyResult, ReplyParams, StartSessionParams,
};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};

mod token_report;

const OPEN_LOCATION_TIMEOUT: Duration = Duration::from_secs(120);
const READ_FILE_TIMEOUT: Duration = Duration::from_secs(10);
/// JSON-RPC error code returned by `initialize` when the client announces a
/// protocol version that differs from [`loopbiotic_protocol::PROTOCOL_VERSION`].
const PROTOCOL_MISMATCH_CODE: i64 = -32001;
static NEXT_EDITOR_REQUEST_ID: AtomicU64 = AtomicU64::new(1);

#[tokio::main]
async fn main() -> Result<()> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();

    match args.as_slice() {
        [] => serve_stdio().await,
        [flag] if flag == "--stdio" => serve_stdio().await,
        [cmd, sub] if cmd == "backend" && sub == "list" => print_backends(),
        [cmd, sub] if cmd == "backend" && sub == "check" => check_backend(),
        [cmd, sub] if cmd == "schema" && sub == "card" => print_card_schema(),
        [cmd, sub] if cmd == "dev" && sub == "mock-session" => print_mock_session().await,
        [cmd, sub] if cmd == "dev" && sub == "stdio-agent" => run_stdio_agent(),
        [cmd, sub, rest @ ..] if cmd == "dev" && sub == "token-report" => {
            token_report::run(rest).await
        }
        _ => print_help(),
    }
}

async fn serve_stdio() -> Result<()> {
    let backend = backend_from_env()?;
    let stdout = Arc::new(Mutex::new(io::stdout()));

    // Editor lines flow through a channel so that a mid-turn server request
    // (editor/open_location) can await its response while the main loop is
    // blocked inside the engine call that produced it.
    let (line_tx, line_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    std::thread::spawn(move || {
        let stdin = io::stdin();
        for line in stdin.lock().lines() {
            let Ok(line) = line else {
                break;
            };
            if line_tx.send(line).is_err() {
                break;
            }
        }
    });
    let lines = Arc::new(tokio::sync::Mutex::new(line_rx));
    // Client requests that arrived while a granter was waiting for its
    // response; the main loop drains them before reading new lines.
    let deferred = Arc::new(Mutex::new(VecDeque::<String>::new()));

    let mut server = Server::new(backend, progress_reporter(stdout.clone()));
    server.engine.set_location_granter(location_granter(
        stdout.clone(),
        lines.clone(),
        deferred.clone(),
    ));
    server
        .engine
        .set_source_context_provider(source_context_provider(
            stdout.clone(),
            lines.clone(),
            deferred.clone(),
        ));

    loop {
        let line = {
            // A poisoned lock only means another thread panicked while holding
            // it; the queue holds whole lines, so the inner data is still valid.
            let queued = deferred
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .pop_front();
            match queued {
                Some(line) => line,
                None => match lines.lock().await.recv().await {
                    Some(line) => line,
                    None => break,
                },
            }
        };

        if line.trim().is_empty() {
            continue;
        }

        // A response to a loopbioticd-initiated request that arrived after its
        // granter timed out; there is nothing left to deliver it to.
        if is_stale_server_response(&line) {
            continue;
        }

        let response = server.handle_line(&line).await;
        write_json(&stdout, &response)?;
    }

    Ok(())
}

fn is_stale_server_response(line: &str) -> bool {
    serde_json::from_str::<Value>(line)
        .ok()
        .and_then(|value| {
            let id = value.get("id")?.as_str()?.to_string();
            let is_response = value.get("method").is_none();
            Some(id.starts_with("loopbioticd_") && is_response)
        })
        .unwrap_or(false)
}

/// Asks the editor to open a location mid-turn: sends an editor/open_location
/// request and pumps incoming lines until its response arrives (deferring
/// unrelated client requests for the main loop) or the timeout expires.
fn location_granter(
    stdout: Arc<Mutex<io::Stdout>>,
    lines: Arc<tokio::sync::Mutex<tokio::sync::mpsc::UnboundedReceiver<String>>>,
    deferred: Arc<Mutex<VecDeque<String>>>,
) -> LocationGranter {
    Arc::new(move |request, session_id| {
        let stdout = stdout.clone();
        let lines = lines.clone();
        let deferred = deferred.clone();

        Box::pin(async move {
            request_editor_context(
                &stdout,
                &lines,
                &deferred,
                "editor/open_location",
                json!({
                    "session_id": session_id,
                    "reason": request.reason,
                    "location": request.location,
                }),
                OPEN_LOCATION_TIMEOUT,
            )
            .await
        })
    })
}

fn source_context_provider(
    stdout: Arc<Mutex<io::Stdout>>,
    lines: Arc<tokio::sync::Mutex<tokio::sync::mpsc::UnboundedReceiver<String>>>,
    deferred: Arc<Mutex<VecDeque<String>>>,
) -> SourceContextProvider {
    Arc::new(move |file, session_id| {
        let stdout = stdout.clone();
        let lines = lines.clone();
        let deferred = deferred.clone();

        Box::pin(async move {
            request_editor_context(
                &stdout,
                &lines,
                &deferred,
                "editor/read_file",
                json!({
                    "session_id": session_id,
                    "file": file,
                }),
                READ_FILE_TIMEOUT,
            )
            .await
        })
    })
}

async fn request_editor_context(
    stdout: &Arc<Mutex<io::Stdout>>,
    lines: &Arc<tokio::sync::Mutex<tokio::sync::mpsc::UnboundedReceiver<String>>>,
    deferred: &Arc<Mutex<VecDeque<String>>>,
    method: &str,
    params: Value,
    timeout: Duration,
) -> Option<ContextBundle> {
    let id = format!(
        "loopbioticd_{}",
        NEXT_EDITOR_REQUEST_ID.fetch_add(1, Ordering::Relaxed)
    );
    let message = json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": method,
        "params": params,
    });
    if write_json(stdout, &message).is_err() {
        return None;
    }

    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return None;
        }
        let line = {
            let mut lines = lines.lock().await;
            match tokio::time::timeout(remaining, lines.recv()).await {
                Err(_) => return None,
                Ok(None) => return None,
                Ok(Some(line)) => line,
            }
        };
        let Ok(value) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if value.get("id").and_then(Value::as_str) == Some(id.as_str())
            && value.get("method").is_none()
        {
            let result = value.get("result")?;
            if result.get("granted").and_then(Value::as_bool) != Some(true) {
                return None;
            }
            return result
                .get("context")
                .cloned()
                .and_then(|context| serde_json::from_value::<ContextBundle>(context).ok());
        }
        // A poisoned lock only means another thread panicked while holding
        // it; the queue holds whole lines, so the inner data is still valid.
        deferred
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .push_back(line);
    }
}

pub(crate) fn backend_from_env() -> Result<Arc<dyn BackendAdapter>> {
    match std::env::var("LOOPBIOTIC_BACKEND").as_deref() {
        Ok("codex_app") | Ok("codex") => Ok(Arc::new(CodexAppBackend::from_env()?)),
        Ok("claude_app") | Ok("claude") => Ok(Arc::new(ClaudeAppBackend::from_env()?)),
        Ok("ollama") => Ok(Arc::new(OllamaBackend::from_env()?)),
        Ok("agent") | Ok("agent_stdio") => Ok(Arc::new(StdioAgentBackend::from_env()?)),
        Ok("generic") | Ok("generic_cli") => Ok(Arc::new(GenericCliBackend::from_env()?)),
        _ => Ok(Arc::new(MockBackend)),
    }
}

struct Server {
    backend: Arc<dyn BackendAdapter>,
    engine: Engine,
    progress: ProgressReporter,
}

impl Server {
    fn new(backend: Arc<dyn BackendAdapter>, progress: ProgressReporter) -> Self {
        let mut engine = Engine::new(backend.clone());
        engine.set_prefetch_mode(prefetch_mode_from_env());

        Self {
            engine,
            backend,
            progress,
        }
    }

    async fn handle_line(&mut self, line: &str) -> JsonRpcResponse {
        let request = match serde_json::from_str::<JsonRpcRequest>(line) {
            Ok(request) => request,
            Err(error) => return JsonRpcResponse::err(Value::Null, -32700, error.to_string()),
        };

        match self.handle(request).await {
            Ok(response) => response,
            Err((id, message)) => JsonRpcResponse::err(id, -32603, message),
        }
    }

    async fn handle(
        &mut self,
        request: JsonRpcRequest,
    ) -> Result<JsonRpcResponse, (Value, String)> {
        let id = request.id.clone();
        let result = match request.method.as_str() {
            "initialize" => {
                // Old clients omit params.client.protocol_version entirely;
                // the check only runs when the client announces a version.
                let client_version = request
                    .params
                    .get("client")
                    .and_then(|client| client.get("protocol_version"))
                    .and_then(Value::as_u64);
                if let Some(client_version) = client_version
                    && client_version != u64::from(loopbiotic_protocol::PROTOCOL_VERSION)
                {
                    return Ok(JsonRpcResponse::err(
                        id,
                        PROTOCOL_MISMATCH_CODE,
                        format!(
                            "protocol version mismatch: client speaks protocol version {client_version}, loopbioticd speaks {}; update the Loopbiotic plugin and loopbioticd so both are on the same version",
                            loopbiotic_protocol::PROTOCOL_VERSION
                        ),
                    ));
                }

                json!({
                    "server": "loopbioticd",
                    "version": env!("CARGO_PKG_VERSION"),
                    "protocol_version": loopbiotic_protocol::PROTOCOL_VERSION,
                    "backend": self.backend.capabilities(),
                })
            }
            "backend/list" => json!([self.backend.capabilities()]),
            "session/start" => {
                let params = parse::<StartSessionParams>(&id, request.params)?;
                let result = self
                    .engine
                    .start_with_progress(params, Some(self.progress.clone()))
                    .await
                    .map_err(server_error(&id))?;

                json!(result)
            }
            "session/action" => {
                let params = parse::<ActionParams>(&id, request.params)?;
                if let Some(context) = params.context {
                    self.engine
                        .update_context(&params.session_id, context)
                        .map_err(server_error(&id))?;
                }
                let result = self
                    .engine
                    .action_with_progress(
                        &params.session_id,
                        params.action,
                        Some(self.progress.clone()),
                    )
                    .await
                    .map_err(server_error(&id))?;

                json!(result)
            }
            "session/reply" => {
                let params = parse::<ReplyParams>(&id, request.params)?;
                if let Some(context) = params.context {
                    self.engine
                        .update_context(&params.session_id, context)
                        .map_err(server_error(&id))?;
                }
                let result = self
                    .engine
                    .reply_with_progress(
                        &params.session_id,
                        params.text,
                        Some(self.progress.clone()),
                    )
                    .await
                    .map_err(server_error(&id))?;

                json!(result)
            }
            "patch/apply_result" => {
                let params = parse::<PatchApplyResult>(&id, request.params)?;
                let result = self
                    .engine
                    .apply_result_with_progress(params, Some(self.progress.clone()))
                    .await
                    .map_err(server_error(&id))?;

                json!(result)
            }
            "session/stop" => {
                let params = parse::<ActionParams>(&id, request.params)?;
                let result = self
                    .engine
                    .action_with_progress(
                        &params.session_id,
                        loopbiotic_protocol::Action::Stop,
                        Some(self.progress.clone()),
                    )
                    .await
                    .map_err(server_error(&id))?;

                json!(result)
            }
            "backend/warmup" => {
                self.backend
                    .warmup()
                    .await
                    .map_err(|error| (id.clone(), error.to_string()))?;

                json!({"ok": true})
            }
            "shutdown" => json!({"ok": true}),
            method => return Err((id, format!("unknown method {method}"))),
        };

        Ok(JsonRpcResponse::ok(id, result))
    }
}

fn prefetch_mode_from_env() -> PrefetchMode {
    match std::env::var("LOOPBIOTIC_PREFETCH").as_deref() {
        Ok("fix") => PrefetchMode::Fix,
        _ => PrefetchMode::Off,
    }
}

fn parse<T>(id: &Value, value: Value) -> Result<T, (Value, String)>
where
    T: DeserializeOwned,
{
    serde_json::from_value(value).map_err(|error| (id.clone(), error.to_string()))
}

fn progress_reporter(stdout: Arc<Mutex<io::Stdout>>) -> ProgressReporter {
    Arc::new(move |progress| {
        let notification = JsonRpcNotification {
            jsonrpc: "2.0".into(),
            method: "agent/progress".into(),
            params: serde_json::to_value(progress).unwrap_or(Value::Null),
        };

        if let Err(error) = write_json(&stdout, &notification) {
            eprintln!("loopbioticd: failed to write progress notification: {error}");
        }
    })
}

fn write_json<T>(stdout: &Arc<Mutex<io::Stdout>>, value: &T) -> Result<()>
where
    T: Serialize,
{
    let json = serde_json::to_string(value)?;
    let mut stdout = stdout
        .lock()
        .map_err(|_| anyhow::anyhow!("stdout lock poisoned"))?;

    writeln!(stdout, "{json}")?;
    stdout.flush()?;

    Ok(())
}

fn server_error(id: &Value) -> impl FnOnce(anyhow::Error) -> (Value, String) + '_ {
    |error| (id.clone(), error.to_string())
}

fn print_backends() -> Result<()> {
    println!(
        "{}",
        serde_json::to_string_pretty(&vec![MockBackend::info()])?
    );

    Ok(())
}

fn check_backend() -> Result<()> {
    let info: BackendInfo = MockBackend::info();
    println!("{} ok", info.name);

    Ok(())
}

fn print_card_schema() -> Result<()> {
    let card = MockBackend::first_card()?;
    println!("{}", serde_json::to_string_pretty(&card)?);

    Ok(())
}

async fn print_mock_session() -> Result<()> {
    let backend = Arc::new(MockBackend);
    let mut engine = Engine::new(backend);
    let params = StartSessionParams {
        cwd: std::env::current_dir()?,
        file: "src/main.rs".into(),
        cursor: loopbiotic_protocol::Cursor { line: 1, column: 1 },
        selection: None,
        prompt: "payload is empty".into(),
        mode: loopbiotic_protocol::Mode::Auto,
        buffer_text: String::new(),
        buffer_start_line: 1,
        diagnostics: vec![],
        hints: vec![],
        context_policy: Default::default(),
    };
    let start = engine.start(params).await?;
    let patch = engine
        .action(&start.session_id, loopbiotic_protocol::Action::Fix)
        .await?;

    println!("{}", serde_json::to_string_pretty(&start)?);
    println!("{}", serde_json::to_string_pretty(&patch)?);

    Ok(())
}

fn run_stdio_agent() -> Result<()> {
    let stdin = io::stdin();

    for line in stdin.lock().lines() {
        let line = line?;
        let value = serde_json::from_str::<serde_json::Value>(&line)?;
        let action = value
            .get("a")
            .and_then(|value| value.get("action"))
            .and_then(|value| value.as_str())
            .unwrap_or("");
        let reply = value
            .get("a")
            .and_then(|value| value.get("text"))
            .and_then(|value| value.as_str());
        let op = if let Some(reply) = reply {
            json!({
                "op": "finding",
                "title": "Reply received",
                "finding": format!("You said: {reply}")
            })
        } else if action.contains("Fix") {
            json!({
                "op": "patch",
                "title": "Guard payload shape",
                "explanation": "Keep body present for callers.",
                "patches": [
                    {
                        "file": "src/work.ts",
                        "diff": "@@ -1,1 +1,1 @@\n-placeholder\n+payload = payload or {}\n",
                        "explanation": "Creates a payload fallback."
                    }
                ]
            })
        } else {
            json!({
                "op": "hypothesis",
                "title": "Payload may be skipped",
                "claim": "This path can return before the payload is built."
            })
        };

        println!(
            "{}",
            serde_json::to_string(&json!({
                "t": "loopbiotic_progress",
                "phase": "reviewing",
                "message": "Reviewing the supplied context"
            }))?
        );
        println!(
            "{}",
            serde_json::to_string(&json!({
                "t": "loopbiotic_progress",
                "phase": "drafting",
                "message": "Drafting the next Loopbiotic card"
            }))?
        );
        println!(
            "{}",
            serde_json::to_string(&json!({
                "t": "loopbiotic_result",
                "result": op
            }))?
        );
        io::stdout().flush()?;
    }

    Ok(())
}

fn print_help() -> Result<()> {
    eprintln!("loopbioticd --stdio");
    eprintln!("loopbioticd backend list");
    eprintln!("loopbioticd backend check");
    eprintln!("loopbioticd schema card");
    eprintln!("loopbioticd dev mock-session");
    eprintln!("loopbioticd dev stdio-agent");
    eprintln!("loopbioticd dev token-report [--fixtures DIR] [--json FILE] [--max-turns N]");
    eprintln!("loopbioticd dev token-report --render FILE");
    eprintln!("loopbioticd dev token-report --check BASELINE CURRENT");

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::is_stale_server_response;

    #[test]
    fn stale_detects_response_to_daemon_initiated_request() {
        assert!(is_stale_server_response(
            r#"{"jsonrpc":"2.0","id":"loopbioticd_7","result":{"granted":false}}"#
        ));
    }

    #[test]
    fn stale_ignores_requests_even_with_daemon_style_id() {
        assert!(!is_stale_server_response(
            r#"{"jsonrpc":"2.0","id":"loopbioticd_7","method":"editor/open_location","params":{}}"#
        ));
    }

    #[test]
    fn stale_ignores_client_responses_and_requests() {
        assert!(!is_stale_server_response(
            r#"{"jsonrpc":"2.0","id":"client_1","result":{}}"#
        ));
        assert!(!is_stale_server_response(
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#
        ));
    }

    #[test]
    fn stale_ignores_non_json_and_numeric_ids() {
        assert!(!is_stale_server_response("not json"));
        assert!(!is_stale_server_response(
            r#"{"jsonrpc":"2.0","id":42,"result":{}}"#
        ));
    }
}
