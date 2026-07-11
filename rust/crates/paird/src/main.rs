use std::io::{self, BufRead, Write};
use std::sync::{Arc, Mutex};

use anyhow::Result;
use pair_backends::{
    BackendAdapter, CodexAppBackend, GenericCliBackend, MockBackend, ProgressReporter,
    StdioAgentBackend,
};
use pair_harness::Engine;
use pair_protocol::{
    ActionParams, BackendInfo, JsonRpcNotification, JsonRpcRequest, JsonRpcResponse,
    PatchApplyResult, ReplyParams, StartSessionParams,
};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};

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
        _ => print_help(),
    }
}

async fn serve_stdio() -> Result<()> {
    let backend = backend_from_env()?;
    let stdout = Arc::new(Mutex::new(io::stdout()));
    let mut server = Server::new(backend, progress_reporter(stdout.clone()));
    let stdin = io::stdin();

    for line in stdin.lock().lines() {
        let line = line?;

        if line.trim().is_empty() {
            continue;
        }

        let response = server.handle_line(&line).await;
        write_json(&stdout, &response)?;
    }

    Ok(())
}

fn backend_from_env() -> Result<Arc<dyn BackendAdapter>> {
    match std::env::var("PAIR_BACKEND").as_deref() {
        Ok("codex_app") | Ok("codex") => Ok(Arc::new(CodexAppBackend::from_env()?)),
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
        Self {
            engine: Engine::new(backend.clone()),
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
            "initialize" => json!({
                "server": "paird",
                "version": env!("CARGO_PKG_VERSION"),
                "protocol_version": pair_protocol::PROTOCOL_VERSION,
                "backend": self.backend.capabilities(),
            }),
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
                        pair_protocol::Action::Stop,
                        Some(self.progress.clone()),
                    )
                    .await
                    .map_err(server_error(&id))?;

                json!(result)
            }
            "shutdown" => json!({"ok": true}),
            method => return Err((id, format!("unknown method {method}"))),
        };

        Ok(JsonRpcResponse::ok(id, result))
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
            eprintln!("paird: failed to write progress notification: {error}");
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
        cursor: pair_protocol::Cursor { line: 1, column: 1 },
        selection: None,
        prompt: "payload is empty".into(),
        mode: pair_protocol::Mode::Auto,
        buffer_text: String::new(),
        buffer_start_line: 1,
        diagnostics: vec![],
    };
    let start = engine.start(params).await?;
    let patch = engine
        .action(&start.session_id, pair_protocol::Action::Fix)
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
                "t": "pair_progress",
                "phase": "reviewing",
                "message": "Reviewing the supplied context"
            }))?
        );
        println!(
            "{}",
            serde_json::to_string(&json!({
                "t": "pair_progress",
                "phase": "drafting",
                "message": "Drafting the next Pair card"
            }))?
        );
        println!(
            "{}",
            serde_json::to_string(&json!({
                "t": "pair_result",
                "result": op
            }))?
        );
        io::stdout().flush()?;
    }

    Ok(())
}

fn print_help() -> Result<()> {
    eprintln!("paird --stdio");
    eprintln!("paird backend list");
    eprintln!("paird backend check");
    eprintln!("paird schema card");
    eprintln!("paird dev mock-session");
    eprintln!("paird dev stdio-agent");

    Ok(())
}
