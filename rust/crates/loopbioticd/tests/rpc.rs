//! End-to-end tests that spawn the loopbioticd binary, select the mock
//! backend, and speak newline-delimited JSON-RPC over its stdio.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::time::{Duration, Instant};

use loopbiotic_protocol::PROTOCOL_VERSION;
use serde_json::{Value, json};

/// Generous per-message deadline so slow CI cannot flake, while a hung daemon
/// still fails the test instead of blocking forever.
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(60);
const REAL_CODEX_RESPONSE_BUDGET: Duration = Duration::from_secs(20);

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
            std::env::var("LOOPBIOTIC_REAL_CODEX_MODEL").unwrap_or_else(|_| "gpt-5.6-sol".into());
        let mut command = Command::new(env!("CARGO_BIN_EXE_loopbioticd"));
        command
            .arg("--stdio")
            .env("LOOPBIOTIC_BACKEND", "codex_app")
            .env("LOOPBIOTIC_CODEX_COMMAND", "codex")
            .env("LOOPBIOTIC_CODEX_ARGS_JSON", r#"["app-server","--stdio"]"#)
            .env("LOOPBIOTIC_CODEX_MODEL", model)
            .env("LOOPBIOTIC_CODEX_EFFORT", "low")
            .env("LOOPBIOTIC_TURN_TIMEOUT_SECS", "120")
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
    assert_eq!(result["goal"]["status"], json!("active"));

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
    let result = &response["result"];
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
        elapsed <= REAL_CODEX_RESPONSE_BUDGET,
        "proposal took {elapsed:?}; Neovim users must not wait over {REAL_CODEX_RESPONSE_BUDGET:?}"
    );
    assert!(
        matches!(kind, "hypothesis" | "finding" | "choice"),
        "a proposal question must return a useful answer/plan, not {kind}: {title}: {message}"
    );
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
