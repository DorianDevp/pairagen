//! End-to-end tests that spawn the loopbioticd binary, select the mock
//! backend, and speak newline-delimited JSON-RPC over its stdio.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::time::Duration;

use loopbiotic_protocol::PROTOCOL_VERSION;
use serde_json::{Value, json};

/// Generous per-message deadline so slow CI cannot flake, while a hung daemon
/// still fails the test instead of blocking forever.
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(60);

struct Daemon {
    child: Child,
    stdin: ChildStdin,
    lines: Receiver<String>,
}

impl Daemon {
    fn spawn() -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_loopbioticd"))
            .arg("--stdio")
            .env("LOOPBIOTIC_BACKEND", "mock")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn loopbioticd");
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

    /// Reads messages until the response with the given id arrives, skipping
    /// notifications (e.g. agent/progress) and daemon-initiated requests.
    fn response_for(&mut self, id: &str) -> Value {
        loop {
            let message = self.next_message();
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
        match self.lines.recv_timeout(RESPONSE_TIMEOUT) {
            Ok(line) => serde_json::from_str(&line).expect("daemon wrote invalid JSON"),
            Err(RecvTimeoutError::Timeout) => panic!("timed out waiting for daemon output"),
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

fn start_session_params() -> Value {
    // An empty, dedicated cwd keeps the context indexer away from real
    // machine state; "investigate" mode asks the backend for a lead card
    // instead of entering the goal loop (whose patches need a live editor
    // to answer editor/read_file validation requests).
    let cwd = std::env::temp_dir().join(format!("loopbioticd-rpc-test-{}", std::process::id()));
    std::fs::create_dir_all(&cwd).expect("create test cwd");
    json!({
        "cwd": cwd,
        "file": "src/work.ts",
        "cursor": {"line": 1, "column": 1},
        "selection": null,
        "prompt": "payload is empty",
        "mode": "investigate",
        "buffer_text": "placeholder",
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
