//! End-to-end tests that spawn the loopbioticd binary, select the mock
//! backend, and speak newline-delimited JSON-RPC over its stdio.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::time::{Duration, Instant};

use loopbiotic_patch::{PatchApply, UnifiedDiff};
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
///   real_codex_auto_proposal_is_fast_and_non_mutating -- --ignored --nocapture
#[test]
#[ignore = "requires an authenticated real Codex CLI"]
fn real_codex_auto_proposal_is_fast_and_non_mutating() {
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
            "auto",
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

/// Full real-agent product gate: a question and reply stay conversational,
/// Fix returns one small draft, and accepting it automatically yields a
/// conversational next card without an intermediate summary.
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
            "auto",
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
    assert_conversational(&after_accept, "post-accept continuation");
    assert_ne!(
        after_accept["card"]["kind"],
        json!("summary"),
        "accept must not insert an intermediate summary"
    );
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
            "auto",
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
