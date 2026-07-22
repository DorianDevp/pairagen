use std::collections::{HashMap, VecDeque};
use std::io::{self, BufRead, Write};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Result;
use loopbiotic_backends::{
    BackendAdapter, ClaudeAppBackend, CodexAppBackend, GenericCliBackend, MockBackend,
    OllamaBackend, OpenAiCompatibleBackend, ProgressReporter, StdioAgentBackend,
};
use loopbiotic_harness::{Engine, LocationGranter, PrefetchMode, SourceContextProvider};
use loopbiotic_protocol::{
    ActionParams, BackendInfo, ContextBundle, JsonRpcNotification, JsonRpcRequest, JsonRpcResponse,
    PatchApplyResult, ReplyParams, StartSessionParams,
};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};

mod ab_report;
mod token_report;

const OPEN_LOCATION_TIMEOUT: Duration = Duration::from_secs(120);
const READ_FILE_TIMEOUT: Duration = Duration::from_secs(10);
/// JSON-RPC error code returned by `initialize` when the client announces a
/// protocol version that differs from [`loopbiotic_protocol::PROTOCOL_VERSION`].
const PROTOCOL_MISMATCH_CODE: i64 = -32001;
static NEXT_EDITOR_REQUEST_ID: AtomicU64 = AtomicU64::new(1);
static NEXT_TURN_ID: AtomicU64 = AtomicU64::new(1);
const DEFAULT_CONVERSATION_DEADLINE: Duration = Duration::from_secs(10);
const DEFAULT_WORK_DEADLINE: Duration = Duration::from_secs(20);

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
        [cmd, sub] if cmd == "dev" && sub == "project-profile" => {
            print_project_profile(std::path::Path::new("."))
        }
        [cmd, sub, root] if cmd == "dev" && sub == "project-profile" => {
            print_project_profile(std::path::Path::new(root))
        }
        [cmd, sub] if cmd == "dev" && sub == "stdio-agent" => run_stdio_agent(),
        [cmd, sub, rest @ ..] if cmd == "dev" && sub == "token-report" => {
            token_report::run(rest).await
        }
        [cmd, sub, rest @ ..] if cmd == "dev" && sub == "ab-report" => ab_report::run(rest).await,
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

    let mut server = Server::new(backend, progress_reporter(stdout.clone()), stdout.clone());
    {
        let mut engine = server.engine.lock().await;
        engine.set_location_granter(location_granter(
            stdout.clone(),
            lines.clone(),
            deferred.clone(),
        ));
        engine.set_source_context_provider(source_context_provider(
            stdout.clone(),
            lines.clone(),
            deferred.clone(),
        ));
    }

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
        Ok("openai") | Ok("openai_compatible") | Ok("lm_studio") => {
            Ok(Arc::new(OpenAiCompatibleBackend::from_env()?))
        }
        Ok("agent") | Ok("agent_stdio") => Ok(Arc::new(StdioAgentBackend::from_env()?)),
        Ok("generic") | Ok("generic_cli") => Ok(Arc::new(GenericCliBackend::from_env()?)),
        _ => Ok(Arc::new(MockBackend)),
    }
}

struct Server {
    backend: Arc<dyn BackendAdapter>,
    engine: Arc<tokio::sync::Mutex<Engine>>,
    progress: ProgressReporter,
    stdout: Arc<Mutex<io::Stdout>>,
    pending: HashMap<String, PendingTurn>,
    interaction_feedback: HashMap<String, Vec<String>>,
}

struct PendingTurn {
    turn_id: String,
    generation: u64,
    abort: tokio::task::AbortHandle,
}

enum TurnCommand {
    Start {
        session_id: String,
        generation: u64,
    },
    Action {
        session_id: String,
        generation: u64,
        action: loopbiotic_protocol::Action,
    },
    Reply {
        session_id: String,
        generation: u64,
        text: String,
        mode: loopbiotic_protocol::Mode,
    },
    Apply {
        result: Box<PatchApplyResult>,
        generation: u64,
    },
}

impl TurnCommand {
    fn kind(&self) -> &'static str {
        match self {
            Self::Reply {
                mode: loopbiotic_protocol::Mode::Fix | loopbiotic_protocol::Mode::Propose,
                ..
            } => "work",
            Self::Start { .. } | Self::Reply { .. } => "conversation",
            Self::Action {
                action:
                    loopbiotic_protocol::Action::Fix
                    | loopbiotic_protocol::Action::Goal
                    | loopbiotic_protocol::Action::Retry
                    | loopbiotic_protocol::Action::EditPrompt,
                ..
            } => "work",
            Self::Action { .. } => "conversation",
            Self::Apply { .. } => "continuation",
        }
    }
}

impl Server {
    fn new(
        backend: Arc<dyn BackendAdapter>,
        progress: ProgressReporter,
        stdout: Arc<Mutex<io::Stdout>>,
    ) -> Self {
        let mut engine = Engine::new(backend.clone());
        engine.set_prefetch_mode(prefetch_mode_from_env());

        Self {
            engine: Arc::new(tokio::sync::Mutex::new(engine)),
            backend,
            progress,
            stdout,
            pending: HashMap::new(),
            interaction_feedback: HashMap::new(),
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
                loopbiotic_protocol::validate_project_metadata(None, &params.skills)
                    .map_err(server_error(&id))?;
                loopbiotic_protocol::validate_project_signals(&params.project_signals)
                    .map_err(server_error(&id))?;
                let deadline = start_deadline(&params);
                let (session_id, generation, working) = {
                    let mut engine = self.engine.lock().await;
                    let (session_id, generation) = engine.reserve_start(params);
                    let turn_id = next_turn_id();
                    let working = engine
                        .working_result(&session_id, &turn_id, deadline.as_millis() as u64)
                        .map_err(server_error(&id))?;
                    (session_id, generation, (turn_id, working))
                };
                self.run_turn(
                    TurnCommand::Start {
                        session_id: session_id.clone(),
                        generation,
                    },
                    session_id,
                    generation,
                    working,
                    deadline,
                )
                .await
                .map_err(server_error(&id))?
            }
            "session/action" => {
                let params = parse::<ActionParams>(&id, request.params)?;
                self.abort_pending(&params.session_id).await;
                self.apply_interaction_feedback(&params.session_id)
                    .await
                    .map_err(server_error(&id))?;
                if params.action == loopbiotic_protocol::Action::CancelTurn {
                    let result = self
                        .engine
                        .lock()
                        .await
                        .cancel_turn(&params.session_id)
                        .await
                        .map_err(server_error(&id))?;
                    return Ok(JsonRpcResponse::ok(id, json!(result)));
                }
                if let Some(context) = params.context {
                    self.engine
                        .lock()
                        .await
                        .update_context(&params.session_id, context)
                        .map_err(server_error(&id))?;
                }
                let deadline = action_deadline(&params.action);
                let (generation, working) = {
                    let mut engine = self.engine.lock().await;
                    let generation = engine
                        .begin_turn(&params.session_id)
                        .map_err(server_error(&id))?;
                    let turn_id = next_turn_id();
                    let working = engine
                        .working_result(&params.session_id, &turn_id, deadline.as_millis() as u64)
                        .map_err(server_error(&id))?;
                    (generation, (turn_id, working))
                };
                self.run_turn(
                    TurnCommand::Action {
                        session_id: params.session_id.clone(),
                        generation,
                        action: params.action,
                    },
                    params.session_id,
                    generation,
                    working,
                    deadline,
                )
                .await
                .map_err(server_error(&id))?
            }
            "session/reply" => {
                let params = parse::<ReplyParams>(&id, request.params)?;
                loopbiotic_protocol::validate_project_metadata(None, &params.skills)
                    .map_err(server_error(&id))?;
                let deadline = reply_deadline(&params.mode);
                self.abort_pending(&params.session_id).await;
                self.apply_interaction_feedback(&params.session_id)
                    .await
                    .map_err(server_error(&id))?;
                if let Some(context) = params.context {
                    self.engine
                        .lock()
                        .await
                        .update_context_for_prompt(&params.session_id, context, &params.text)
                        .map_err(server_error(&id))?;
                }
                self.engine
                    .lock()
                    .await
                    .update_skills(&params.session_id, params.skills)
                    .map_err(server_error(&id))?;
                let (generation, working) = {
                    let mut engine = self.engine.lock().await;
                    let generation = engine
                        .begin_turn(&params.session_id)
                        .map_err(server_error(&id))?;
                    let turn_id = next_turn_id();
                    let working = engine
                        .working_result(&params.session_id, &turn_id, deadline.as_millis() as u64)
                        .map_err(server_error(&id))?;
                    (generation, (turn_id, working))
                };
                self.run_turn(
                    TurnCommand::Reply {
                        session_id: params.session_id.clone(),
                        generation,
                        text: params.text,
                        mode: params.mode,
                    },
                    params.session_id,
                    generation,
                    working,
                    deadline,
                )
                .await
                .map_err(server_error(&id))?
            }
            "patch/apply_result" => {
                let params = parse::<PatchApplyResult>(&id, request.params)?;
                self.abort_pending(&params.session_id).await;
                self.apply_interaction_feedback(&params.session_id)
                    .await
                    .map_err(server_error(&id))?;
                let session_id = params.session_id.clone();
                let deadline = if params.accepted {
                    Duration::from_millis(1)
                } else {
                    Duration::from_secs(2)
                };
                let (generation, working) = {
                    let mut engine = self.engine.lock().await;
                    let generation = engine.begin_turn(&session_id).map_err(server_error(&id))?;
                    let turn_id = next_turn_id();
                    let working = engine
                        .working_result(&session_id, &turn_id, deadline.as_millis() as u64)
                        .map_err(server_error(&id))?;
                    (generation, (turn_id, working))
                };
                self.run_turn(
                    TurnCommand::Apply {
                        result: Box::new(params),
                        generation,
                    },
                    session_id,
                    generation,
                    working,
                    deadline,
                )
                .await
                .map_err(server_error(&id))?
            }
            "session/stop" => {
                let params = parse::<ActionParams>(&id, request.params)?;
                self.abort_pending(&params.session_id).await;
                let result = self
                    .engine
                    .lock()
                    .await
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

                json!({"ok": true, "identity": self.backend.identity().await})
            }
            "shutdown" => json!({"ok": true}),
            method => return Err((id, format!("unknown method {method}"))),
        };

        Ok(JsonRpcResponse::ok(id, result))
    }

    async fn abort_pending(&mut self, session_id: &str) {
        if let Some(pending) = self.pending.remove(session_id) {
            let finished = pending.abort.is_finished();
            if !finished {
                pending.abort.abort();
                if let Err(error) = self.backend.cancel_turn(session_id).await {
                    eprintln!(
                        "loopbioticd: failed to cancel backend turn {}: {error:#}",
                        pending.turn_id
                    );
                }
            }
            eprintln!(
                "loopbioticd: {} pending turn {} generation {}",
                if finished { "reaped" } else { "cancelled" },
                pending.turn_id,
                pending.generation
            );
        }
    }

    async fn apply_interaction_feedback(&mut self, session_id: &str) -> Result<()> {
        let feedback = self
            .interaction_feedback
            .remove(session_id)
            .unwrap_or_default();
        if feedback.is_empty() {
            return Ok(());
        }

        let mut engine = self.engine.lock().await;
        for item in feedback {
            engine.record_interaction_feedback(session_id, item)?;
        }

        Ok(())
    }

    async fn run_turn(
        &mut self,
        command: TurnCommand,
        session_id: String,
        generation: u64,
        working: (String, loopbiotic_protocol::ActionResult),
        deadline: Duration,
    ) -> Result<Value> {
        let (turn_id, working_result) = working;
        let turn_kind = command.kind();
        let engine = self.engine.clone();
        let progress = self.progress.clone();
        let (result_tx, mut result_rx) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            let result = execute_turn(engine, command, progress)
                .await
                .map_err(|error| error.to_string());
            let _ = result_tx.send(result);
        });
        let abort = task.abort_handle();

        tokio::select! {
            result = &mut result_rx => {
                match result {
                    Ok(Ok(value)) => Ok(value),
                    Ok(Err(error)) => Err(anyhow::anyhow!(error)),
                    Err(_) => Err(anyhow::anyhow!("turn task stopped before returning a result")),
                }
            }
            _ = tokio::time::sleep(deadline) => {
                let deadline_ms = deadline.as_millis() as u64;
                self.interaction_feedback
                    .entry(session_id.clone())
                    .or_default()
                    .push(format!(
                        "The previous {turn_kind} turn exceeded Loopbiotic's {deadline_ms} ms interaction deadline and yielded control. Keep this response compact and interactive: return one useful answer or one small authorized step before optional investigation."
                    ));
                self.pending.insert(session_id.clone(), PendingTurn {
                    turn_id: turn_id.clone(),
                    generation,
                    abort,
                });
                let yielded = JsonRpcNotification {
                    jsonrpc: "2.0".into(),
                    method: "agent/turn_yielded".into(),
                    params: json!({
                        "session_id": session_id,
                        "turn_id": turn_id,
                        "generation": generation,
                        "turn_kind": turn_kind,
                        "deadline_ms": deadline_ms,
                    }),
                };
                if let Err(error) = write_json(&self.stdout, &yielded) {
                    eprintln!("loopbioticd: failed to write turn-yielded notification: {error}");
                }
                let stdout = self.stdout.clone();
                tokio::spawn(async move {
                    let params = match result_rx.await {
                        Ok(Ok(result)) => json!({
                            "session_id": session_id,
                            "turn_id": turn_id,
                            "generation": generation,
                            "result": result,
                        }),
                        Ok(Err(error)) => json!({
                            "session_id": session_id,
                            "turn_id": turn_id,
                            "generation": generation,
                            "error": error,
                        }),
                        Err(_) => return,
                    };
                    let notification = JsonRpcNotification {
                        jsonrpc: "2.0".into(),
                        method: "agent/turn_ready".into(),
                        params,
                    };
                    if let Err(error) = write_json(&stdout, &notification) {
                        eprintln!("loopbioticd: failed to write turn-ready notification: {error}");
                    }
                });

                Ok(json!(working_result))
            }
        }
    }
}

async fn execute_turn(
    engine: Arc<tokio::sync::Mutex<Engine>>,
    command: TurnCommand,
    progress: ProgressReporter,
) -> Result<Value> {
    let mut engine = engine.lock().await;
    match command {
        TurnCommand::Start {
            session_id,
            generation,
        } => engine
            .complete_start_with_progress(&session_id, generation, Some(progress))
            .await
            .map(|result| json!(result)),
        TurnCommand::Action {
            session_id,
            generation,
            action,
        } => engine
            .action_with_progress_generation(&session_id, generation, action, Some(progress))
            .await
            .map(|result| json!(result)),
        TurnCommand::Reply {
            session_id,
            generation,
            text,
            mode,
        } => engine
            .reply_with_progress_generation(&session_id, generation, text, mode, Some(progress))
            .await
            .map(|result| json!(result)),
        TurnCommand::Apply { result, generation } => engine
            .apply_result_with_progress_generation(*result, generation, Some(progress))
            .await
            .map(|result| json!(result)),
    }
}

fn next_turn_id() -> String {
    format!("t_{}", NEXT_TURN_ID.fetch_add(1, Ordering::Relaxed))
}

fn start_deadline(params: &StartSessionParams) -> Duration {
    if matches!(
        params.mode,
        loopbiotic_protocol::Mode::Fix | loopbiotic_protocol::Mode::Propose
    ) {
        work_deadline()
    } else {
        conversation_deadline()
    }
}

fn action_deadline(action: &loopbiotic_protocol::Action) -> Duration {
    if matches!(
        action,
        loopbiotic_protocol::Action::Fix
            | loopbiotic_protocol::Action::Goal
            | loopbiotic_protocol::Action::Retry
            | loopbiotic_protocol::Action::EditPrompt
    ) {
        work_deadline()
    } else {
        conversation_deadline()
    }
}

fn reply_deadline(mode: &loopbiotic_protocol::Mode) -> Duration {
    if matches!(
        mode,
        loopbiotic_protocol::Mode::Fix | loopbiotic_protocol::Mode::Propose
    ) {
        work_deadline()
    } else {
        conversation_deadline()
    }
}

fn conversation_deadline() -> Duration {
    deadline_from_env(
        "LOOPBIOTIC_CONVERSATION_DEADLINE_MS",
        DEFAULT_CONVERSATION_DEADLINE,
    )
}

fn work_deadline() -> Duration {
    deadline_from_env("LOOPBIOTIC_WORK_DEADLINE_MS", DEFAULT_WORK_DEADLINE)
}

fn deadline_from_env(name: &str, default: Duration) -> Duration {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|millis| *millis > 0)
        .map(Duration::from_millis)
        .unwrap_or(default)
}

fn prefetch_mode_from_env() -> PrefetchMode {
    match std::env::var("LOOPBIOTIC_PREFETCH").as_deref() {
        Ok("off") => PrefetchMode::Off,
        _ => PrefetchMode::ReadOnly,
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
        mode: loopbiotic_protocol::Mode::Investigate,
        buffer_text: String::new(),
        buffer_start_line: 1,
        diagnostics: vec![],
        hints: vec![],
        call_hierarchy: None,
        context_policy: Default::default(),
        project_signals: Default::default(),
        skills: vec![],
    };
    let start = engine.start(params).await?;
    let patch = engine
        .action(&start.session_id, loopbiotic_protocol::Action::Fix)
        .await?;

    println!("{}", serde_json::to_string_pretty(&start)?);
    println!("{}", serde_json::to_string_pretty(&patch)?);

    Ok(())
}

fn print_project_profile(root: &std::path::Path) -> Result<()> {
    let profile = loopbiotic_context::project::ProjectProfiler
        .inspect(root, &loopbiotic_protocol::ProjectSignals::default());
    println!("{}", serde_json::to_string_pretty(&profile)?);
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
    eprintln!("loopbioticd dev project-profile [ROOT]");
    eprintln!("loopbioticd dev token-report [--fixtures DIR] [--json FILE] [--max-turns N]");
    eprintln!("loopbioticd dev token-report --render FILE");
    eprintln!("loopbioticd dev token-report --check BASELINE CURRENT");
    eprintln!(
        "loopbioticd dev ab-report [--fixtures DIR] [--cases NAME,...] [--variants before,profile,after] [--repeat N] [--json FILE]"
    );

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
