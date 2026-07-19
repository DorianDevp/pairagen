//! End-to-end tests that spawn the loopbioticd binary, select the mock
//! backend, and speak newline-delimited JSON-RPC over its stdio.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::time::{Duration, Instant};

use loopbiotic_patch::{DiffLine, PatchApply, UnifiedDiff};
use loopbiotic_protocol::PROTOCOL_VERSION;
use serde_json::{Value, json};

/// Generous per-message deadline so slow CI cannot flake, while a hung daemon
/// still fails the test instead of blocking forever.
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(60);
const REAL_CODEX_CONVERSATION_BUDGET: Duration = Duration::from_secs(11);
const REAL_CODEX_WORK_BUDGET: Duration = Duration::from_secs(21);
const LOCAL_INTERACTION_BUDGET: Duration = Duration::from_secs(2);

struct Daemon {
    child: Child,
    stdin: ChildStdin,
    lines: Receiver<String>,
}

impl Daemon {
    fn spawn() -> Self {
        let mut command = Command::new(env!("CARGO_BIN_EXE_loopbioticd"));
        command
            .arg("--stdio")
            .env("LOOPBIOTIC_BACKEND", "mock")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        Self::spawn_command(command)
    }

    fn spawn_codex() -> Self {
        let model =
            std::env::var("LOOPBIOTIC_REAL_CODEX_MODEL").unwrap_or_else(|_| "gpt-5.4-mini".into());
        Self::spawn_codex_model(model)
    }

    fn spawn_codex_model(model: impl AsRef<str>) -> Self {
        let mut command = Command::new(env!("CARGO_BIN_EXE_loopbioticd"));
        command
            .arg("--stdio")
            .env("LOOPBIOTIC_BACKEND", "codex_app")
            .env("LOOPBIOTIC_CODEX_COMMAND", "codex")
            .env("LOOPBIOTIC_CODEX_ARGS_JSON", r#"["app-server","--stdio"]"#)
            .env("LOOPBIOTIC_CODEX_MODEL", model.as_ref())
            .env("LOOPBIOTIC_CODEX_EFFORT", "low")
            .env("LOOPBIOTIC_CODEX_DISCOVERY_MODEL", "gpt-5.4-mini")
            .env("LOOPBIOTIC_CODEX_DISCOVERY_EFFORT", "low")
            .env("LOOPBIOTIC_TURN_TIMEOUT_SECS", "120")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        Self::spawn_command(command)
    }

    fn spawn_codex_with_deadlines(conversation_ms: u64, work_ms: u64) -> Self {
        let model =
            std::env::var("LOOPBIOTIC_REAL_CODEX_MODEL").unwrap_or_else(|_| "gpt-5.4-mini".into());
        let mut command = Command::new(env!("CARGO_BIN_EXE_loopbioticd"));
        command
            .arg("--stdio")
            .env("LOOPBIOTIC_BACKEND", "codex_app")
            .env("LOOPBIOTIC_CODEX_COMMAND", "codex")
            .env("LOOPBIOTIC_CODEX_ARGS_JSON", r#"["app-server","--stdio"]"#)
            .env("LOOPBIOTIC_CODEX_MODEL", model)
            .env("LOOPBIOTIC_CODEX_EFFORT", "low")
            .env("LOOPBIOTIC_CODEX_DISCOVERY_MODEL", "gpt-5.4-mini")
            .env("LOOPBIOTIC_CODEX_DISCOVERY_EFFORT", "low")
            .env("LOOPBIOTIC_TURN_TIMEOUT_SECS", "120")
            .env(
                "LOOPBIOTIC_CONVERSATION_DEADLINE_MS",
                conversation_ms.to_string(),
            )
            .env("LOOPBIOTIC_WORK_DEADLINE_MS", work_ms.to_string())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        Self::spawn_command(command)
    }

    fn spawn_command(mut command: Command) -> Self {
        let mut child = command.spawn().expect("spawn loopbioticd");
        let stdin = child.stdin.take().expect("child stdin");
        let stdout = child.stdout.take().expect("child stdout");

        // A reader thread feeds a channel so tests can apply deadlines to
        // every read instead of blocking on the pipe.
        let (line_tx, lines) = channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else {
                    break;
                };
                if line_tx.send(line).is_err() {
                    break;
                }
            }
        });

        Self {
            child,
            stdin,
            lines,
        }
    }

    fn send(&mut self, value: &Value) {
        let mut line = value.to_string();
        line.push('\n');
        self.stdin
            .write_all(line.as_bytes())
            .expect("write to daemon stdin");
        self.stdin.flush().expect("flush daemon stdin");
    }

    fn request(&mut self, id: &str, method: &str, params: Value) -> Value {
        self.send(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        }));
        self.response_for(id)
    }

    fn timed_request(&mut self, id: &str, method: &str, params: Value) -> (Value, Duration) {
        let started = Instant::now();
        self.send(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        }));
        let response = self.response_for(id);

        (response, started.elapsed())
    }

    fn timed_request_with_progress(
        &mut self,
        id: &str,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> (Value, Duration, Vec<(Duration, Value)>) {
        let started = Instant::now();
        let mut progress = Vec::new();
        self.send(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        }));

        loop {
            let remaining = timeout.saturating_sub(started.elapsed());
            assert!(
                !remaining.is_zero(),
                "timed out after {timeout:?} waiting for response {id}"
            );
            let message = self.next_message_with_timeout(remaining);
            if message.get("method").and_then(Value::as_str) == Some("agent/progress") {
                progress.push((started.elapsed(), message["params"].clone()));
                continue;
            }
            if message.get("method").is_some() {
                continue;
            }
            assert_eq!(
                message.get("id").and_then(Value::as_str),
                Some(id),
                "expected a response to {id}, got: {message}"
            );
            return (message, started.elapsed(), progress);
        }
    }

    /// Reads messages until the response with the given id arrives, skipping
    /// notifications (e.g. agent/progress) and daemon-initiated requests.
    fn response_for(&mut self, id: &str) -> Value {
        self.response_for_with_timeout(id, RESPONSE_TIMEOUT)
    }

    fn response_for_with_timeout(&mut self, id: &str, timeout: Duration) -> Value {
        let started = Instant::now();
        loop {
            let remaining = timeout.saturating_sub(started.elapsed());
            assert!(
                !remaining.is_zero(),
                "timed out after {timeout:?} waiting for response {id}"
            );
            let message = self.next_message_with_timeout(remaining);
            if message.get("method").is_some() {
                continue;
            }
            assert_eq!(
                message.get("id").and_then(Value::as_str),
                Some(id),
                "expected a response to {id}, got: {message}"
            );
            return message;
        }
    }

    fn response_for_with_editor_context(
        &mut self,
        id: &str,
        timeout: Duration,
        context: &Value,
    ) -> Value {
        let started = Instant::now();
        loop {
            let remaining = timeout.saturating_sub(started.elapsed());
            assert!(
                !remaining.is_zero(),
                "timed out after {timeout:?} waiting for response {id}"
            );
            let message = self.next_message_with_timeout(remaining);
            if let Some(method) = message.get("method").and_then(Value::as_str) {
                if let Some(request_id) = message.get("id").and_then(Value::as_str)
                    && matches!(method, "editor/read_file" | "editor/open_location")
                {
                    self.send(&json!({
                        "jsonrpc": "2.0",
                        "id": request_id,
                        "result": {
                            "granted": true,
                            "context": context,
                        },
                    }));
                }
                continue;
            }
            assert_eq!(
                message.get("id").and_then(Value::as_str),
                Some(id),
                "expected a response to {id}, got: {message}"
            );
            return message;
        }
    }

    fn finish_turn(
        &mut self,
        response: Value,
        timeout: Duration,
        context: Option<&Value>,
    ) -> Value {
        assert!(
            response.get("error").is_none(),
            "unexpected turn error: {response}"
        );
        let result = response["result"].clone();
        if result["card"]["kind"] != json!("working") {
            return result;
        }

        let turn_id = result["card"]["turn_id"]
            .as_str()
            .expect("working turn id")
            .to_owned();
        let started = Instant::now();
        loop {
            let remaining = timeout.saturating_sub(started.elapsed());
            assert!(
                !remaining.is_zero(),
                "timed out after {timeout:?} waiting for turn {turn_id}"
            );
            let message = self.next_message_with_timeout(remaining);
            if let Some(method) = message.get("method").and_then(Value::as_str) {
                if let Some(request_id) = message.get("id").and_then(Value::as_str)
                    && matches!(method, "editor/read_file" | "editor/open_location")
                {
                    self.send(&json!({
                        "jsonrpc": "2.0",
                        "id": request_id,
                        "result": match context {
                            Some(context) => json!({"granted": true, "context": context}),
                            None => json!({"granted": false}),
                        },
                    }));
                    continue;
                }
                if method == "agent/turn_ready" && message["params"]["turn_id"] == json!(turn_id) {
                    if let Some(error) = message["params"]["error"].as_str() {
                        panic!("background turn failed: {error}");
                    }
                    return message["params"]["result"].clone();
                }
            }
        }
    }

    fn next_message(&mut self) -> Value {
        self.next_message_with_timeout(RESPONSE_TIMEOUT)
    }

    fn next_message_with_timeout(&mut self, timeout: Duration) -> Value {
        match self.lines.recv_timeout(timeout) {
            Ok(line) => serde_json::from_str(&line).expect("daemon wrote invalid JSON"),
            Err(RecvTimeoutError::Timeout) => {
                panic!("timed out after {timeout:?} waiting for daemon output")
            }
            Err(RecvTimeoutError::Disconnected) => {
                panic!("daemon exited before writing a response")
            }
        }
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn test_cwd() -> std::path::PathBuf {
    // An empty, dedicated cwd keeps the context indexer away from real
    // machine state; "investigate" mode asks the backend for a lead card
    // instead of entering the goal loop (whose patches need a live editor
    // to answer editor/read_file validation requests).
    let cwd = std::env::temp_dir().join(format!("loopbioticd-rpc-test-{}", std::process::id()));
    std::fs::create_dir_all(&cwd).expect("create test cwd");
    cwd
}

fn start_session_params() -> Value {
    start_session_params_with(test_cwd(), "payload is empty", "investigate", "placeholder")
}

fn start_session_params_with(
    cwd: std::path::PathBuf,
    prompt: &str,
    mode: &str,
    buffer_text: &str,
) -> Value {
    json!({
        "cwd": cwd,
        "file": "src/work.ts",
        "cursor": {"line": 1, "column": 1},
        "selection": null,
        "prompt": prompt,
        "mode": mode,
        "buffer_text": buffer_text,
        "buffer_start_line": 1,
        "diagnostics": [],
    })
}

#[test]
fn initialize_reports_protocol_version_without_client_params() {
    let mut daemon = Daemon::spawn();

    // Old clients send no client block at all; the handshake must still work.
    let response = daemon.request("1", "initialize", json!({}));

    assert!(
        response.get("error").is_none(),
        "unexpected error: {response}"
    );
    let result = &response["result"];
    assert_eq!(result["server"], json!("loopbioticd"));
    assert_eq!(result["protocol_version"], json!(PROTOCOL_VERSION));
    assert_eq!(result["backend"]["name"], json!("mock"));
}

#[test]
fn initialize_accepts_matching_client_protocol_version() {
    let mut daemon = Daemon::spawn();

    let response = daemon.request(
        "1",
        "initialize",
        json!({
            "client": {
                "name": "loopbiotic.nvim",
                "protocol_version": PROTOCOL_VERSION,
            },
        }),
    );

    assert!(
        response.get("error").is_none(),
        "unexpected error: {response}"
    );
    assert_eq!(
        response["result"]["protocol_version"],
        json!(PROTOCOL_VERSION)
    );
}

#[test]
fn initialize_rejects_mismatched_client_protocol_version() {
    let mut daemon = Daemon::spawn();
    let client_version = PROTOCOL_VERSION + 1;

    let response = daemon.request(
        "1",
        "initialize",
        json!({
            "client": {
                "name": "loopbiotic.nvim",
                "protocol_version": client_version,
            },
        }),
    );

    assert!(
        response.get("result").is_none(),
        "expected an error: {response}"
    );
    let error = &response["error"];
    assert_eq!(error["code"], json!(-32001));
    let message = error["message"].as_str().expect("error message");
    assert!(
        message.contains("protocol version mismatch"),
        "message should name the failure: {message}"
    );
    assert!(
        message.contains(&client_version.to_string())
            && message.contains(&PROTOCOL_VERSION.to_string()),
        "message should include both versions: {message}"
    );
    assert!(
        message.to_lowercase().contains("update"),
        "message should tell the user to update: {message}"
    );
}

#[test]
fn session_start_returns_first_mock_card() {
    let mut daemon = Daemon::spawn();

    let init = daemon.request("1", "initialize", json!({}));
    assert!(init.get("error").is_none(), "unexpected error: {init}");

    let response = daemon.request("2", "session/start", start_session_params());

    assert!(
        response.get("error").is_none(),
        "unexpected error: {response}"
    );
    let result = &response["result"];
    assert!(
        result["session_id"]
            .as_str()
            .is_some_and(|id| !id.is_empty()),
        "missing session_id: {result}"
    );
    // The mock backend opens investigations with its hypothesis card.
    assert_eq!(result["card"]["kind"], json!("hypothesis"));
    assert_eq!(result["card"]["title"], json!("Payload may be skipped"));
    assert_eq!(result["goal"]["status"], json!("idle"));

    // Following the lead keeps the same session alive and yields the next card.
    let session_id = result["session_id"]
        .as_str()
        .expect("session id")
        .to_owned();
    let follow = daemon.request(
        "3",
        "session/action",
        json!({"session_id": session_id, "action": "follow"}),
    );
    assert!(follow.get("error").is_none(), "unexpected error: {follow}");
    assert_eq!(follow["result"]["session_id"], json!(session_id));
    assert_eq!(follow["result"]["card"]["kind"], json!("finding"));
}

#[test]
fn reply_prompt_submits_its_selected_mode() {
    let mut daemon = Daemon::spawn();
    let init = daemon.request("1", "initialize", json!({}));
    assert!(init.get("error").is_none(), "unexpected error: {init}");
    let start = daemon.request("2", "session/start", start_session_params());
    let session_id = start["result"]["session_id"]
        .as_str()
        .expect("session id")
        .to_owned();

    let reply = daemon.request(
        "3",
        "session/reply",
        json!({
            "session_id": session_id,
            "text": "Napraw to",
            "mode": "fix"
        }),
    );

    assert!(reply.get("error").is_none(), "unexpected error: {reply}");
    assert_eq!(reply["result"]["card"]["kind"], json!("patch"));
}

#[test]
fn reply_without_a_mode_is_rejected_at_the_rpc_boundary() {
    let mut daemon = Daemon::spawn();
    let init = daemon.request("1", "initialize", json!({}));
    assert!(init.get("error").is_none(), "unexpected error: {init}");
    let start = daemon.request("2", "session/start", start_session_params());
    let session_id = start["result"]["session_id"].as_str().expect("session id");

    let reply = daemon.request(
        "3",
        "session/reply",
        json!({"session_id": session_id, "text": "Napraw to"}),
    );

    assert!(
        reply.get("result").is_none(),
        "mode-less reply ran: {reply}"
    );
    assert!(
        reply["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("mode"))
    );
}

#[test]
fn backend_warmup_reports_the_backend_identity() {
    let mut daemon = Daemon::spawn();

    let init = daemon.request("1", "initialize", json!({}));
    assert!(init.get("error").is_none(), "unexpected error: {init}");

    let response = daemon.request("2", "backend/warmup", json!({}));

    assert!(
        response.get("error").is_none(),
        "unexpected error: {response}"
    );
    let result = &response["result"];
    assert_eq!(result["ok"], json!(true));
    assert_eq!(result["identity"]["backend"], json!("mock"));
    assert_eq!(result["identity"]["model"], json!("mock-model"));
    assert_eq!(
        result["identity"]["models"],
        json!(["mock-model", "mock-mini"])
    );
}

/// Manual product-behavior gate. It is ignored in CI because it requires an
/// authenticated Codex CLI and spends real tokens:
///
/// cargo test -p loopbioticd --test rpc \
///   real_codex_explanation_is_fast_and_non_mutating -- --ignored --nocapture
#[test]
#[ignore = "requires an authenticated real Codex CLI"]
fn real_codex_explanation_is_fast_and_non_mutating() {
    let cwd = std::env::temp_dir().join(format!("loopbiotic-real-codex-{}", std::process::id()));
    let source = cwd.join("src/work.ts");
    std::fs::create_dir_all(source.parent().expect("source parent")).expect("create fixture");
    std::fs::write(
        &source,
        "export function displayName(first, last) {\n  return `${first} ${last}`;\n}\n",
    )
    .expect("write fixture");

    let mut daemon = Daemon::spawn_codex();
    let init = daemon.request("1", "initialize", json!({}));
    assert!(init.get("error").is_none(), "unexpected error: {init}");

    let (response, elapsed) = daemon.timed_request(
        "2",
        "session/start",
        start_session_params_with(
            cwd,
            "How would you propose making displayName handle an empty last name?",
            "explain",
            "export function displayName(first, last) {\n  return `${first} ${last}`;\n}\n",
        ),
    );
    assert!(
        response.get("error").is_none(),
        "unexpected error: {response}"
    );
    let result = daemon.finish_turn(response, Duration::from_secs(120), None);
    let kind = result["card"]["kind"].as_str().unwrap_or("<missing>");
    let title = result["card"]["title"].as_str().unwrap_or("<missing>");
    let message = result["card"]["message"].as_str().unwrap_or("");
    eprintln!(
        "real Codex: elapsed={elapsed:?} kind={kind} title={title:?} model={} input={} output={} total={} activities={} message={message:?}",
        result["model"].as_str().unwrap_or("<unknown>"),
        result["turn_token_usage"]["input_tokens"],
        result["turn_token_usage"]["output_tokens"],
        result["turn_token_usage"]["total_tokens"],
        result["attempts"][0]["activities"]
            .as_array()
            .map(Vec::len)
            .unwrap_or(0),
    );

    assert!(
        elapsed <= REAL_CODEX_CONVERSATION_BUDGET,
        "proposal took {elapsed:?}; Neovim users must regain control within {REAL_CODEX_CONVERSATION_BUDGET:?}"
    );
    assert!(
        matches!(kind, "hypothesis" | "finding" | "choice"),
        "a proposal question must return a useful answer/plan, not {kind}: {title}: {message}"
    );
}

/// Manual regression for the production failure captured in the session log:
/// an imperative prompt in user-selected fix mode must draft the requested
/// change instead of returning a Finding that only identifies the edit location.
///
/// cargo test -p loopbioticd --test rpc \
///   real_codex_fix_mode_implementation_request_returns_patch -- --ignored --nocapture
#[test]
#[ignore = "requires an authenticated real Codex CLI"]
fn real_codex_fix_mode_implementation_request_returns_patch() {
    let cwd = std::env::temp_dir().join(format!(
        "loopbiotic-real-codex-fix-mode-{}",
        std::process::id()
    ));
    let source = cwd.join("src/work.ts");
    let buffer = "import { Component } from '@angular/core';\n\n@Component({\n  selector: 'app-customer-shell',\n  standalone: true,\n  template: '',\n})\nexport class CustomerShellComponent {}\n";
    std::fs::create_dir_all(source.parent().expect("source parent")).expect("create fixture");
    std::fs::write(&source, buffer).expect("write fixture");

    let mut daemon = Daemon::spawn_codex();
    let init = daemon.request("1", "initialize", json!({}));
    assert!(init.get("error").is_none(), "unexpected error: {init}");

    let response = daemon.request(
        "2",
        "session/start",
        start_session_params_with(
            cwd,
            "Potrzebuję dobrze przygotowanego shella: dodaj ładny wrapper zgodny ze stylem innych shelli i router-outlet.",
            "fix",
            buffer,
        ),
    );
    let result = daemon.finish_turn(response, Duration::from_secs(120), None);

    assert_eq!(
        result["card"]["kind"],
        json!("patch"),
        "user-selected fix mode was reduced to a non-actionable finding: {result}"
    );
    let diff = result["card"]["patches"][0]["diff"]
        .as_str()
        .expect("real Codex patch diff");
    UnifiedDiff::parse(diff).expect("real Codex returned parseable diff");
}

/// Real transport gate for the programmer-flow fast path. The long visible
/// deadline keeps the request in the foreground so this test can observe the
/// complete progress stream before the final response.
#[test]
#[ignore = "requires an authenticated real Codex CLI"]
fn real_codex_streams_a_preview_before_the_validated_card() {
    let cwd = std::env::temp_dir().join(format!(
        "loopbiotic-real-codex-stream-{}",
        std::process::id()
    ));
    let source = cwd.join("src/work.ts");
    std::fs::create_dir_all(source.parent().expect("source parent")).expect("create fixture");
    let buffer = "export function displayName(first, last) {\n  return `${first} ${last}`;\n}\n";
    std::fs::write(&source, buffer).expect("write fixture");

    let mut daemon = Daemon::spawn_codex_with_deadlines(120_000, 120_000);
    let init = daemon.request("1", "initialize", json!({}));
    assert!(init.get("error").is_none(), "unexpected error: {init}");

    let (response, complete, progress) = daemon.timed_request_with_progress(
        "2",
        "session/start",
        start_session_params_with(
            cwd,
            "Explain the smallest safe way to handle an empty last name.",
            "explain",
            buffer,
        ),
        Duration::from_secs(120),
    );
    assert!(
        response.get("error").is_none(),
        "unexpected error: {response}"
    );
    assert_ne!(
        response["result"]["card"]["kind"],
        json!("working"),
        "long foreground deadline should return the validated card"
    );

    let first_delta = progress
        .iter()
        .find(|(_, params)| params["phase"] == json!("streaming"))
        .map(|(elapsed, _)| *elapsed)
        .expect("Codex should expose its first agent-message delta");
    let (first_preview, preview) = progress
        .iter()
        .find(|(_, params)| params["preview"]["title"].as_str().is_some())
        .expect("Codex should expose a non-actionable structured preview");
    assert!(
        first_delta <= *first_preview,
        "preview cannot precede the first delta"
    );
    assert!(
        *first_preview < complete,
        "preview must arrive before the final card"
    );
    assert!(
        preview["preview"].get("actions").is_none(),
        "streaming preview must not expose final-card actions"
    );
    eprintln!(
        "real Codex stream: first_delta={first_delta:?} first_preview={first_preview:?} complete={complete:?}"
    );
}

/// Real-agent regression for cursor intent: an error at the cursor must beat a
/// distant deprecation in the same file, even though the error's source line is
/// already present in the primary editor excerpt.
///
/// cargo test -p loopbioticd --test rpc \
///   real_codex_prioritizes_cursor_local_error_over_distant_deprecation -- --ignored --nocapture
#[test]
#[ignore = "requires an authenticated real Codex CLI"]
fn real_codex_prioritizes_cursor_local_error_over_distant_deprecation() {
    let cwd = std::env::temp_dir().join(format!(
        "loopbiotic-real-codex-local-diagnostic-{}",
        std::process::id()
    ));
    let source = cwd.join("static/admin.js");
    std::fs::create_dir_all(source.parent().expect("source parent")).expect("create fixture");
    let mut lines = (1..=300)
        .map(|line| format!("// filler {line}"))
        .collect::<Vec<_>>();
    lines[164] = "document.write(html);".into();
    lines[252] = "const formData = new FormData(form);".into();
    lines[253] = "let body = formData;".into();
    lines[258] = "body = new URLSearchParams(formData);".into();
    lines[262] = "return navigate(form.action, { method: form.method, body });".into();
    std::fs::write(&source, lines.join("\n")).expect("write fixture");
    let excerpt = lines[234..270].join("\n");

    let context = json!({
        "cwd": cwd,
        "file": "static/admin.js",
        "cursor": {"line": 259, "column": 16},
        "selection": null,
        "buffer_text": excerpt,
        "buffer_start_line": 235,
        "diagnostics": [
            {
                "file": "static/admin.js",
                "line": 259,
                "column": 5,
                "severity": "1",
                "message": "Type 'URLSearchParams' is not assignable to type 'FormData'. Types of property 'append' are incompatible."
            },
            {
                "file": "static/admin.js",
                "line": 165,
                "column": 16,
                "severity": "4",
                "message": "The signature of 'document.write' is deprecated."
            }
        ],
        "hints": []
    });
    let params = json!({
        "cwd": cwd,
        "file": "static/admin.js",
        "cursor": {"line": 259, "column": 16},
        "selection": null,
        "prompt": "What's wrong with it?",
        "mode": "investigate",
        "buffer_text": excerpt,
        "buffer_start_line": 235,
        "diagnostics": context["diagnostics"],
        "hints": []
    });

    let mut daemon = Daemon::spawn_codex();
    let init = daemon.request("1", "initialize", json!({}));
    assert!(init.get("error").is_none(), "unexpected error: {init}");

    daemon.send(&json!({
        "jsonrpc": "2.0",
        "id": "2",
        "method": "session/start",
        "params": params,
    }));
    let response = daemon.response_for_with_editor_context("2", Duration::from_secs(120), &context);
    let result = daemon.finish_turn(response, Duration::from_secs(120), Some(&context));
    assert_conversational(&result, "cursor-local diagnostic");

    let card_text = result["card"].to_string().to_lowercase();
    let location_line = result["card"]["location"]["line"]
        .as_u64()
        .or_else(|| result["card"]["next"]["line"].as_u64())
        .unwrap_or_default();
    let input = result["turn_token_usage"]["input_tokens"]
        .as_u64()
        .unwrap_or_default();
    let cached = result["turn_token_usage"]["cached_input_tokens"]
        .as_u64()
        .unwrap_or_default();
    eprintln!(
        "real Codex local diagnostic: location={location_line} input={input} cached={cached} fresh={}",
        input.saturating_sub(cached)
    );

    assert!(
        card_text.contains("urlsearchparams")
            || card_text.contains("formdata")
            || card_text.contains("request body"),
        "agent missed the cursor-local type error: {result}"
    );
    assert!(
        (250..=265).contains(&location_line),
        "agent navigated away from the cursor-local error: {result}"
    );
    assert!(
        result["context_report"]["candidates"]
            .as_array()
            .is_some_and(|candidates| candidates.iter().any(|candidate| {
                candidate["selected"] == json!(true)
                    && candidate["reason"]
                        .as_str()
                        .is_some_and(|reason| reason.contains("URLSearchParams"))
            })),
        "optimizer did not deliver the cursor-local diagnostic: {result}"
    );

    let session_id = result["session_id"].as_str().expect("session id");
    daemon.send(&json!({
        "jsonrpc": "2.0",
        "id": "3",
        "method": "session/action",
        "params": {
            "session_id": session_id,
            "action": "fix",
            "context": context,
        },
    }));
    let fix_response =
        daemon.response_for_with_editor_context("3", Duration::from_secs(120), &context);
    let fix = daemon.finish_turn(fix_response, Duration::from_secs(120), Some(&context));
    assert_eq!(
        fix["card"]["kind"],
        json!("patch"),
        "Fix did not draft: {fix}"
    );
    let diff = fix["card"]["patches"][0]["diff"]
        .as_str()
        .expect("real Codex patch diff");
    let parsed = UnifiedDiff::parse(diff).expect("real Codex returned parseable diff");
    assert_eq!(parsed.hunks.len(), 1, "Fix must return one hunk: {diff}");
    let patch_text = diff.to_lowercase();
    let fix_input = fix["turn_token_usage"]["input_tokens"]
        .as_u64()
        .unwrap_or_default();
    let fix_cached = fix["turn_token_usage"]["cached_input_tokens"]
        .as_u64()
        .unwrap_or_default();
    eprintln!(
        "real Codex local fix: old_start={} input={fix_input} cached={fix_cached} fresh={}",
        parsed.hunks[0].old_start,
        fix_input.saturating_sub(fix_cached)
    );

    assert!(
        (250..=265).contains(&parsed.hunks[0].old_start),
        "Fix patched away from the cursor-local error: {diff}"
    );
    assert!(
        patch_text.contains("formdata")
            || patch_text.contains("urlsearchparams")
            || patch_text.contains("bodyinit"),
        "Fix ignored the type-error block: {diff}"
    );
    assert!(
        !patch_text.contains("document.write"),
        "Fix returned to the distant deprecation: {diff}"
    );
}

/// Real-agent regression for dependency ordering and review granularity.
/// Extracting a named interface needs two accepted compiler-safe patches: the
/// declaration first, then the later use. The first card must never hide both
/// edits inside one broad `@@` hunk.
///
/// cargo test -p loopbioticd --test rpc \
///   real_codex_interface_extraction_declares_before_use -- --ignored --nocapture
#[test]
#[ignore = "requires an authenticated real Codex CLI"]
fn real_codex_interface_extraction_declares_before_use() {
    let cwd = std::env::temp_dir().join(format!(
        "loopbiotic-real-codex-interface-order-{}",
        std::process::id()
    ));
    let source = cwd.join("src/work.ts");
    let original = "type Tone = \"info\" | \"warning\";\n\nexport class HomePageComponent {\n  readonly cards: {\n    header: string;\n    tone: Tone;\n  }[] = [];\n}\n";
    std::fs::create_dir_all(source.parent().expect("source parent")).expect("create fixture");
    std::fs::write(&source, original).expect("write fixture");

    let context = json!({
        "cwd": cwd,
        "file": "src/work.ts",
        "cursor": {"line": 4, "column": 12},
        "selection": null,
        "buffer_text": original,
        "buffer_start_line": 1,
        "diagnostics": [],
        "hints": [],
        "artifacts": []
    });

    let mut daemon = Daemon::spawn_codex();
    let init = daemon.request("1", "initialize", json!({}));
    assert!(init.get("error").is_none(), "unexpected error: {init}");

    daemon.send(&json!({
        "jsonrpc": "2.0",
        "id": "2",
        "method": "session/start",
        "params": start_session_params_with(
            cwd,
            "Extract the inline cards item type into a named interface in this file.",
            "investigate",
            original,
        ),
    }));
    let discovery_response =
        daemon.response_for_with_editor_context("2", Duration::from_secs(120), &context);
    let discovery =
        daemon.finish_turn(discovery_response, Duration::from_secs(120), Some(&context));
    let session_id = discovery["session_id"]
        .as_str()
        .expect("session id")
        .to_owned();

    daemon.send(&json!({
        "jsonrpc": "2.0",
        "id": "3",
        "method": "session/action",
        "params": {
            "session_id": session_id,
            "action": "goal",
            "context": context,
        },
    }));
    let patch_response =
        daemon.response_for_with_editor_context("3", Duration::from_secs(120), &context);
    let result = daemon.finish_turn(patch_response, Duration::from_secs(120), Some(&context));
    assert_eq!(
        result["card"]["kind"],
        json!("patch"),
        "interface extraction did not produce its first patch: {result}"
    );
    assert_eq!(
        result["card"]["patches"].as_array().map(Vec::len),
        Some(1),
        "first interface step must touch one file: {result}"
    );
    assert_eq!(
        result["attempts"].as_array().map(Vec::len),
        Some(1),
        "dependency-first prompt must not burn a hidden repair turn: {result}"
    );

    let diff = result["card"]["patches"][0]["diff"]
        .as_str()
        .expect("interface patch diff");
    let parsed = UnifiedDiff::parse(diff).expect("real Codex returned parseable diff");
    assert_eq!(
        parsed.hunks.len(),
        1,
        "first step must have one hunk: {diff}"
    );
    let change_runs = parsed.hunks[0]
        .lines
        .iter()
        .fold((0, false), |(runs, changing), line| {
            let changed = matches!(line, DiffLine::Remove(_) | DiffLine::Add(_));
            (runs + usize::from(changed && !changing), changed)
        })
        .0;
    assert_eq!(
        change_runs, 1,
        "one @@ header concealed multiple review steps: {diff}"
    );

    let updated = PatchApply::apply_to_text(original, &parsed).expect("apply interface patch");
    let interface_offset = updated.find("interface ").unwrap_or_else(|| {
        panic!("first patch did not introduce the interface declaration: {diff}")
    });
    let component_offset = updated
        .find("export class HomePageComponent")
        .expect("component remains present");
    assert!(
        interface_offset < component_offset,
        "interface must be declared before the component that will use it: {diff}"
    );
    assert!(
        updated.contains("readonly cards: {\n    header: string;\n    tone: Tone;\n  }[]"),
        "first patch used the interface before its declaration-only step was accepted: {diff}"
    );
    assert!(
        !result["card"]["goal_complete"].as_bool().unwrap_or(false),
        "declaration-only first step cannot claim the extraction is complete: {result}"
    );
}

/// Full real-agent product gate: a question and reply stay conversational,
/// Fix returns one small draft, and accepting it automatically advances to
/// the next patch or resolves the goal without an acceptance receipt.
///
/// cargo test -p loopbioticd --test rpc \
///   real_codex_interactive_question_fix_accept_workflow -- --ignored --nocapture
#[test]
#[ignore = "requires an authenticated real Codex CLI"]
fn real_codex_interactive_question_fix_accept_workflow() {
    let cwd = std::env::temp_dir().join(format!(
        "loopbiotic-real-codex-interactive-{}",
        std::process::id()
    ));
    let source = cwd.join("src/work.ts");
    let original = "export function displayName(first, last) {\n  return `${first} ${last}`;\n}\n";
    std::fs::create_dir_all(source.parent().expect("source parent")).expect("create fixture");
    std::fs::write(&source, original).expect("write fixture");

    let mut daemon = Daemon::spawn_codex();
    let init = daemon.request("1", "initialize", json!({}));
    assert!(init.get("error").is_none(), "unexpected error: {init}");

    let context = json!({
        "cwd": cwd,
        "file": "src/work.ts",
        "cursor": {"line": 2, "column": 3},
        "selection": null,
        "buffer_text": original,
        "buffer_start_line": 1,
        "diagnostics": [],
        "hints": [],
        "artifacts": []
    });

    let started = Instant::now();
    daemon.send(&json!({
        "jsonrpc": "2.0",
        "id": "2",
        "method": "session/start",
        "params": start_session_params_with(
            cwd.clone(),
            "What does displayName do when last is empty, and what is the smallest sensible behavior change?",
            "investigate",
            original,
        ),
    }));
    let first_response =
        daemon.response_for_with_editor_context("2", Duration::from_secs(120), &context);
    let first_visible = started.elapsed();
    assert!(
        first_visible <= REAL_CODEX_CONVERSATION_BUDGET,
        "question held the editor for {first_visible:?}"
    );
    let first = daemon.finish_turn(first_response, Duration::from_secs(120), Some(&context));
    eprintln!(
        "real Codex question: first_visible={first_visible:?} final_kind={}",
        first["card"]["kind"].as_str().unwrap_or("<missing>")
    );
    assert_conversational(&first, "initial question");
    let session_id = first["session_id"].as_str().expect("session id").to_owned();

    let reply_started = Instant::now();
    daemon.send(&json!({
        "jsonrpc": "2.0",
        "id": "3",
        "method": "session/reply",
        "params": {
            "session_id": session_id,
            "text": "Keep it minimal: should it return only first, or preserve a trailing space?",
            "mode": "explain",
            "context": context,
        },
    }));
    let reply_response =
        daemon.response_for_with_editor_context("3", Duration::from_secs(120), &context);
    let reply_visible = reply_started.elapsed();
    assert!(
        reply_visible <= REAL_CODEX_CONVERSATION_BUDGET,
        "reply held the editor for {reply_visible:?}"
    );
    let reply = daemon.finish_turn(reply_response, Duration::from_secs(120), Some(&context));
    eprintln!(
        "real Codex reply: first_visible={reply_visible:?} final_kind={}",
        reply["card"]["kind"].as_str().unwrap_or("<missing>")
    );
    assert_conversational(&reply, "follow-up question");

    let fix_started = Instant::now();
    daemon.send(&json!({
        "jsonrpc": "2.0",
        "id": "4",
        "method": "session/action",
        "params": {
            "session_id": session_id,
            "action": "fix",
            "context": context,
        },
    }));
    let fix_response =
        daemon.response_for_with_editor_context("4", Duration::from_secs(120), &context);
    let fix_visible = fix_started.elapsed();
    assert!(
        fix_visible <= REAL_CODEX_WORK_BUDGET,
        "Fix held the editor for {fix_visible:?}"
    );
    let fix = daemon.finish_turn(fix_response, Duration::from_secs(120), Some(&context));
    eprintln!(
        "real Codex fix: first_visible={fix_visible:?} final_kind={}",
        fix["card"]["kind"].as_str().unwrap_or("<missing>")
    );
    assert_eq!(
        fix["card"]["kind"],
        json!("patch"),
        "Fix did not draft: {fix}"
    );
    assert_eq!(
        fix["card"]["patches"].as_array().map(Vec::len),
        Some(1),
        "Fix must return one file: {fix}"
    );
    let patch = &fix["card"]["patches"][0];
    let diff = patch["diff"].as_str().expect("patch diff");
    let parsed = UnifiedDiff::parse(diff).expect("real Codex returned parseable diff");
    assert_eq!(parsed.hunks.len(), 1, "Fix must return one hunk: {diff}");
    let updated = PatchApply::apply_to_text(original, &parsed).expect("apply real Codex patch");
    std::fs::write(&source, &updated).expect("write accepted fixture");
    let updated_context = json!({
        "cwd": cwd,
        "file": "src/work.ts",
        "cursor": {"line": 2, "column": 3},
        "selection": null,
        "buffer_text": updated,
        "buffer_start_line": 1,
        "diagnostics": [],
        "hints": [],
        "artifacts": []
    });

    let accept_started = Instant::now();
    let accept_response = daemon.request(
        "5",
        "patch/apply_result",
        json!({
            "session_id": session_id,
            "card_id": fix["card"]["id"],
            "accepted": true,
            "patch_ids": [patch["id"]],
            "changed_files": ["src/work.ts"],
            "error": null,
            "context": updated_context,
        }),
    );
    let accept_visible = accept_started.elapsed();
    assert!(
        accept_visible <= LOCAL_INTERACTION_BUDGET,
        "accept held the editor for {accept_visible:?}"
    );
    let after_accept = daemon.finish_turn(
        accept_response,
        Duration::from_secs(120),
        Some(&updated_context),
    );
    eprintln!(
        "real Codex accept: first_visible={accept_visible:?} final_kind={}",
        after_accept["card"]["kind"].as_str().unwrap_or("<missing>")
    );
    let continuation_kind = after_accept["card"]["kind"].as_str().unwrap_or("<missing>");
    assert!(
        matches!(
            continuation_kind,
            "patch" | "summary" | "choice" | "deny" | "error"
        ),
        "accept did not continue or resolve the goal: {after_accept}"
    );
    assert_ne!(
        after_accept["card"]["title"],
        json!("Local step accepted"),
        "accept inserted a redundant receipt"
    );
}

/// Real-agent regression for the client path that previously crashed
/// Neovim: ask for a draft, then send a message before accepting or rejecting
/// it. The pending patch must be replaced by conversation, never redrafted
/// implicitly.
///
/// cargo test -p loopbioticd --test rpc \
///   real_codex_reply_replaces_pending_draft_conversationally -- --ignored --nocapture
#[test]
#[ignore = "requires an authenticated real Codex CLI"]
fn real_codex_reply_replaces_pending_draft_conversationally() {
    let cwd = std::env::temp_dir().join(format!(
        "loopbiotic-real-codex-draft-reply-{}",
        std::process::id()
    ));
    let source = cwd.join("src/work.ts");
    let original = "export function displayName(first, last) {\n  return `${first} ${last}`;\n}\n";
    std::fs::create_dir_all(source.parent().expect("source parent")).expect("create fixture");
    std::fs::write(&source, original).expect("write fixture");

    let mut daemon = Daemon::spawn_codex();
    let init = daemon.request("1", "initialize", json!({}));
    assert!(init.get("error").is_none(), "unexpected error: {init}");

    let context = json!({
        "cwd": cwd,
        "file": "src/work.ts",
        "cursor": {"line": 2, "column": 3},
        "selection": null,
        "buffer_text": original,
        "buffer_start_line": 1,
        "diagnostics": [],
        "hints": [],
        "artifacts": []
    });

    let first_response = daemon.request(
        "2",
        "session/start",
        start_session_params_with(
            cwd,
            "What is the smallest safe behavior when last is empty?",
            "investigate",
            original,
        ),
    );
    let first = daemon.finish_turn(first_response, Duration::from_secs(120), Some(&context));
    assert_conversational(&first, "initial question");
    let session_id = first["session_id"].as_str().expect("session id").to_owned();

    let fix_response = daemon.request(
        "3",
        "session/action",
        json!({
            "session_id": session_id,
            "action": "fix",
            "context": context,
        }),
    );
    let fix = daemon.finish_turn(fix_response, Duration::from_secs(120), Some(&context));
    assert_eq!(
        fix["card"]["kind"],
        json!("patch"),
        "Fix did not draft: {fix}"
    );

    let reply_started = Instant::now();
    daemon.send(&json!({
        "jsonrpc": "2.0",
        "id": "4",
        "method": "session/reply",
        "params": {
            "session_id": session_id,
            "text": "Before I apply that draft, explain the tradeoff in one concise response.",
            "mode": "explain",
            "context": context,
        },
    }));
    let reply_response =
        daemon.response_for_with_editor_context("4", Duration::from_secs(120), &context);
    let first_visible = reply_started.elapsed();
    assert!(
        first_visible <= REAL_CODEX_CONVERSATION_BUDGET,
        "reply over a pending draft held the editor for {first_visible:?}"
    );
    let reply = daemon.finish_turn(reply_response, Duration::from_secs(120), Some(&context));
    eprintln!(
        "real Codex draft reply: first_visible={first_visible:?} final_kind={}",
        reply["card"]["kind"].as_str().unwrap_or("<missing>")
    );
    assert_conversational(&reply, "reply over pending draft");
}

/// Real cancellation gate with deliberately tiny interaction budgets so the
/// real Codex turn yields a Working card deterministically.
///
/// cargo test -p loopbioticd --test rpc \
///   real_codex_working_card_can_interrupt_thinking -- --ignored --nocapture
#[test]
#[ignore = "requires an authenticated real Codex CLI"]
fn real_codex_working_card_can_interrupt_thinking() {
    let cwd = std::env::temp_dir().join(format!(
        "loopbiotic-real-codex-cancel-{}",
        std::process::id()
    ));
    let source = cwd.join("src/work.ts");
    let buffer = "export const answer = 42;\n";
    std::fs::create_dir_all(source.parent().expect("source parent")).expect("create fixture");
    std::fs::write(&source, buffer).expect("write fixture");

    let mut daemon = Daemon::spawn_codex_with_deadlines(25, 25);
    let init = daemon.request("1", "initialize", json!({}));
    assert!(init.get("error").is_none(), "unexpected error: {init}");

    let (working, elapsed) = daemon.timed_request(
        "2",
        "session/start",
        start_session_params_with(
            cwd,
            "Inspect this function and explain one possible edge case.",
            "investigate",
            buffer,
        ),
    );
    assert!(
        elapsed < LOCAL_INTERACTION_BUDGET,
        "Working card took {elapsed:?}"
    );
    assert_eq!(working["result"]["card"]["kind"], json!("working"));
    let session_id = working["result"]["session_id"]
        .as_str()
        .expect("session id")
        .to_owned();

    let cancel_started = Instant::now();
    let cancelled = daemon.request(
        "3",
        "session/action",
        json!({"session_id": session_id, "action": "cancel_turn"}),
    );
    let cancel_elapsed = cancel_started.elapsed();
    eprintln!("real Codex cancel: working_visible={elapsed:?} cancel={cancel_elapsed:?}");

    assert!(
        cancel_elapsed < LOCAL_INTERACTION_BUDGET,
        "cancellation took {cancel_elapsed:?}"
    );
    assert!(
        cancelled.get("error").is_none(),
        "cancel failed: {cancelled}"
    );
    assert_eq!(
        cancelled["result"]["card"]["title"],
        json!("Turn cancelled")
    );
}

fn assert_conversational(result: &Value, label: &str) {
    let kind = result["card"]["kind"].as_str().unwrap_or("<missing>");
    assert!(
        matches!(kind, "hypothesis" | "finding" | "choice" | "deny" | "error"),
        "{label} returned non-conversational {kind}: {result}"
    );
}

/// Manual real-backend gate for the review contract. Codex generates the
/// pending patch, but rejecting it must be a local daemon transition:
/// no replacement turn, no turn tokens, and no user-visible wait.
///
/// cargo test -p loopbioticd --test rpc \
///   real_codex_patch_reject_is_local -- --ignored --nocapture
#[test]
#[ignore = "requires an authenticated real Codex CLI"]
fn real_codex_patch_reject_is_local() {
    let cwd = std::env::temp_dir().join(format!(
        "loopbiotic-real-codex-reject-{}",
        std::process::id()
    ));
    let source = cwd.join("src/work.ts");
    let buffer_text =
        "export function displayName(first, last) {\n  return `${first} ${last}`;\n}\n";
    std::fs::create_dir_all(source.parent().expect("source parent")).expect("create fixture");
    std::fs::write(&source, buffer_text).expect("write fixture");

    let model =
        std::env::var("LOOPBIOTIC_REAL_CODEX_MODEL").unwrap_or_else(|_| "gpt-5.4-mini".into());
    let mut daemon = Daemon::spawn_codex_model(model);
    let init = daemon.request("1", "initialize", json!({}));
    assert!(init.get("error").is_none(), "unexpected error: {init}");

    let editor_context = json!({
        "cwd": cwd,
        "file": "src/work.ts",
        "cursor": {"line": 1, "column": 1},
        "selection": null,
        "buffer_text": buffer_text,
        "buffer_start_line": 1,
        "diagnostics": [],
        "hints": [],
        "artifacts": []
    });
    daemon.send(&json!({
        "jsonrpc": "2.0",
        "id": "2",
        "method": "session/start",
        "params": start_session_params_with(
            cwd.clone(),
            "Change displayName to return first when last is empty.",
            "fix",
            buffer_text,
        ),
    }));
    let patch_response =
        daemon.response_for_with_editor_context("2", Duration::from_secs(120), &editor_context);
    assert!(
        patch_response.get("error").is_none(),
        "unexpected patch error: {patch_response}"
    );
    let patch_result = daemon.finish_turn(
        patch_response,
        Duration::from_secs(120),
        Some(&editor_context),
    );
    assert_eq!(
        patch_result["card"]["kind"],
        json!("patch"),
        "real Codex did not return a patch: {patch_result}"
    );
    let session_id = patch_result["session_id"]
        .as_str()
        .expect("session id")
        .to_owned();
    let card_id = patch_result["card"]["id"]
        .as_str()
        .expect("card id")
        .to_owned();
    let patch_id = patch_result["card"]["patches"][0]["id"]
        .as_str()
        .expect("patch id")
        .to_owned();

    let (rejected, elapsed) = daemon.timed_request(
        "3",
        "patch/apply_result",
        json!({
            "session_id": session_id,
            "card_id": card_id,
            "accepted": false,
            "patch_ids": [patch_id],
            "changed_files": [],
            "error": null,
            "context": editor_context
        }),
    );
    assert!(
        rejected.get("error").is_none(),
        "unexpected rejection error: {rejected}"
    );
    let result = &rejected["result"];
    eprintln!(
        "real Codex reject: elapsed={elapsed:?} kind={} title={:?} turn_tokens={} attempts={}",
        result["card"]["kind"].as_str().unwrap_or("<missing>"),
        result["card"]["title"].as_str().unwrap_or("<missing>"),
        result["turn_token_usage"]["total_tokens"],
        result["attempts"].as_array().map(Vec::len).unwrap_or(0),
    );

    assert!(
        elapsed < LOCAL_INTERACTION_BUDGET,
        "reject took {elapsed:?}; it must not wait for Codex"
    );
    assert_eq!(result["card"]["kind"], json!("error"));
    assert_eq!(result["card"]["title"], json!("Draft rejected"));
    assert_eq!(result["turn_token_usage"]["total_tokens"], json!(0));
    assert_eq!(result["attempts"], json!([]));
}

#[test]
fn unknown_method_returns_error_response() {
    let mut daemon = Daemon::spawn();

    let response = daemon.request("1", "no/such_method", json!({}));

    assert!(
        response.get("result").is_none(),
        "expected an error: {response}"
    );
    let error = &response["error"];
    assert_eq!(error["code"], json!(-32603));
    assert!(
        error["message"]
            .as_str()
            .is_some_and(|message| message.contains("unknown method no/such_method")),
        "unexpected message: {error}"
    );
}

#[test]
fn stale_server_response_is_ignored_not_answered() {
    let mut daemon = Daemon::spawn();

    // A late reply to a daemon-initiated editor request (its granter already
    // timed out) must be dropped, not answered with a parse/method error.
    daemon.send(&json!({
        "jsonrpc": "2.0",
        "id": "loopbioticd_999",
        "result": {"granted": false},
    }));
    daemon.send(&json!({
        "jsonrpc": "2.0",
        "id": "after_stale",
        "method": "initialize",
        "params": {},
    }));

    // The very next daemon message must answer the initialize request; any
    // reply to the stale line would show up first and fail this assertion.
    let message = daemon.next_message();
    assert!(
        message.get("method").is_none(),
        "unexpected request: {message}"
    );
    assert_eq!(message.get("id"), Some(&json!("after_stale")));
    assert!(
        message.get("error").is_none(),
        "unexpected error: {message}"
    );
    assert_eq!(
        message["result"]["protocol_version"],
        json!(PROTOCOL_VERSION)
    );
}
