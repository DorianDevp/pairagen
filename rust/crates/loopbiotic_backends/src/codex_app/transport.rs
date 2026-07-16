//! JSON-RPC transport to a single `codex app-server` process: request/response
//! correlation, turn streaming, and the server-initiated requests Loopbiotic
//! declines on the model's behalf.

use std::collections::HashMap;

use anyhow::{Result, anyhow};
use loopbiotic_protocol::TokenUsage;
use serde_json::{Value, json};
use tokio::io::{AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, ChildStdin, ChildStdout};

use crate::ProgressReporter;
use crate::support::report_progress;

use super::debug;

pub(super) struct CodexAppState {
    pub(super) process: Option<CodexAppProcess>,
    next_id: u64,
    pub(super) threads: HashMap<String, String>,
    pub(super) context_fingerprints: HashMap<String, u64>,
}

pub(super) struct CodexAppProcess {
    pub(super) child: Child,
    pub(super) stdin: ChildStdin,
    pub(super) stdout: Lines<BufReader<ChildStdout>>,
}

#[derive(Debug)]
pub(super) struct TurnOutput {
    pub(super) text: String,
    pub(super) token_usage: Option<TokenUsage>,
    pub(super) activities: Vec<String>,
}

impl Drop for CodexAppProcess {
    fn drop(&mut self) {
        let _ = self.child.start_kill();
    }
}

impl Default for CodexAppState {
    fn default() -> Self {
        Self {
            process: None,
            next_id: 1,
            threads: HashMap::new(),
            context_fingerprints: HashMap::new(),
        }
    }
}

impl CodexAppState {
    pub(super) fn clear_conversation(&mut self) {
        self.threads.clear();
        self.context_fingerprints.clear();
    }

    pub(super) fn invalidate_process(&mut self) {
        self.process = None;
        self.clear_conversation();
    }

    /// Kills a wedged app-server and forgets everything about it so the next
    /// turn spawns a fresh process with full context.
    pub(super) fn kill_process(&mut self) {
        if let Some(process) = self.process.as_mut() {
            let _ = process.child.start_kill();
        }
        self.invalidate_process();
    }

    fn next_request_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    pub(super) async fn request(&mut self, mut request: Value) -> Result<Value> {
        let id = self.next_request_id();
        request["id"] = json!(id);

        let line = serde_json::to_string(&request)?;
        self.send_line(&line).await?;

        loop {
            let message = self.next_message().await?;

            if self.handle_server_request(&message).await? {
                continue;
            }

            if message.get("id").and_then(Value::as_u64) != Some(id) {
                continue;
            }

            if let Some(error) = message.get("error") {
                let message = error
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("codex app-server request failed");

                return Err(anyhow!(message.to_string()));
            }

            return Ok(message.get("result").cloned().unwrap_or(Value::Null));
        }
    }

    pub(super) async fn read_turn(
        &mut self,
        turn_id: &str,
        session_id: &str,
        progress: Option<&ProgressReporter>,
    ) -> Result<TurnOutput> {
        let mut text = String::new();
        let mut token_usage = None;
        let mut activities = Vec::new();

        loop {
            let message = self.next_message().await?;

            if self.handle_server_request(&message).await? {
                continue;
            }

            let method = message.get("method").and_then(Value::as_str);
            let params = message.get("params").unwrap_or(&Value::Null);
            let message_turn_id = message_turn_id(params);

            if let Some((phase, label)) = progress_for_message(&message, turn_id) {
                report_progress(progress, session_id, phase, label);
            }

            if method == Some("item/completed")
                && message_turn_id == Some(turn_id)
                && let Some(item) = params.get("item")
                && item.get("type").and_then(Value::as_str) == Some("agentMessage")
                && item.get("phase").and_then(Value::as_str) == Some("final_answer")
                && let Some(value) = item.get("text").and_then(Value::as_str)
            {
                text = value.to_string();
            }

            if method == Some("item/completed")
                && message_turn_id == Some(turn_id)
                && let Some(item) = params.get("item")
                && let Some(activity) = activity_summary(item)
                && !activities.contains(&activity)
            {
                activities.push(activity);
            }

            if method == Some("thread/tokenUsage/updated")
                && message_turn_id == Some(turn_id)
                && let Some(usage) = parse_usage(params.get("tokenUsage"))
            {
                token_usage = Some(usage);
            }

            if method == Some("turn/completed") && message_turn_id == Some(turn_id) {
                debug("codex turn completed");
                if let Some(error) = params
                    .get("turn")
                    .and_then(|turn| turn.get("error"))
                    .filter(|error| !error.is_null())
                {
                    return Err(anyhow!("codex turn failed: {error}"));
                }

                if text.trim().is_empty() {
                    return Err(anyhow!("codex turn completed without final answer"));
                }

                return Ok(TurnOutput {
                    text,
                    token_usage,
                    activities,
                });
            }
        }
    }

    async fn next_message(&mut self) -> Result<Value> {
        loop {
            let result = {
                let process = self
                    .process
                    .as_mut()
                    .ok_or_else(|| anyhow!("codex app-server process unavailable"))?;
                process.stdout.next_line().await
            };
            let line = match result {
                Ok(Some(line)) => line,
                Ok(None) => {
                    self.invalidate_process();
                    return Err(anyhow!("codex app-server closed stdout"));
                }
                Err(error) => {
                    self.invalidate_process();
                    return Err(error.into());
                }
            };

            if line.trim().is_empty() {
                continue;
            }

            return Ok(serde_json::from_str(&line)?);
        }
    }

    async fn send_line(&mut self, line: &str) -> Result<()> {
        let result = async {
            let process = self
                .process
                .as_mut()
                .ok_or_else(|| anyhow!("codex app-server process unavailable"))?;
            process.stdin.write_all(line.as_bytes()).await?;
            process.stdin.write_all(b"\n").await?;
            process.stdin.flush().await?;

            Ok(())
        }
        .await;

        if result.is_err() {
            self.invalidate_process();
        }

        result
    }

    async fn handle_server_request(&mut self, message: &Value) -> Result<bool> {
        let Some(id) = message.get("id").cloned() else {
            return Ok(false);
        };
        let Some(method) = message.get("method").and_then(Value::as_str) else {
            return Ok(false);
        };

        let response = match method {
            "item/commandExecution/requestApproval" | "execCommandApproval" => {
                json!({"id": id, "result": {"decision": "decline"}})
            }
            "item/fileChange/requestApproval" | "applyPatchApproval" => {
                json!({"id": id, "result": {"decision": "decline"}})
            }
            "item/permissions/requestApproval" => {
                json!({"id": id, "result": {"permissions": {}, "scope": "turn", "strictAutoReview": true}})
            }
            "item/tool/call" => {
                json!({"id": id, "result": {"contentItems": [], "success": false}})
            }
            "item/tool/requestUserInput" => json!({"id": id, "result": {"answers": {}}}),
            "mcpServer/elicitation/request" => {
                json!({"id": id, "result": {"action": "decline", "content": null, "_meta": null}})
            }
            "account/chatgptAuthTokens/refresh" | "attestation/generate" => {
                json!({"id": id, "error": {"code": -32603, "message": "Loopbiotic does not handle this app-server request"}})
            }
            _ => return Ok(false),
        };

        debug(&format!("handled codex server request {method}"));

        let line = serde_json::to_string(&response)?;
        self.send_line(&line).await?;

        Ok(true)
    }
}

fn message_turn_id(params: &Value) -> Option<&str> {
    params.get("turnId").and_then(Value::as_str).or_else(|| {
        params
            .get("turn")
            .and_then(|turn| turn.get("id"))
            .and_then(Value::as_str)
    })
}

/// Extracts token usage from the app-server's `thread/tokenUsage/updated`
/// notification. This wire format is specific to the Codex app-server; the
/// Claude adapter parses the Anthropic API's usage counters instead.
fn parse_usage(value: Option<&Value>) -> Option<TokenUsage> {
    let last = value?.get("last")?;
    let input = last.get("inputTokens")?.as_u64()? as usize;
    let cached_input = last
        .get("cachedInputTokens")
        .and_then(Value::as_u64)
        .unwrap_or_default() as usize;
    let output = last.get("outputTokens")?.as_u64()? as usize;
    let total = last.get("totalTokens")?.as_u64()? as usize;

    Some(TokenUsage {
        input_tokens: input,
        cached_input_tokens: cached_input,
        output_tokens: output,
        total_tokens: total,
        estimated: false,
    })
}

fn activity_summary(item: &Value) -> Option<String> {
    let kind = item.get("type").and_then(Value::as_str)?;
    if matches!(kind, "reasoning" | "agentMessage" | "plan") {
        return None;
    }

    let detail = match kind {
        "commandExecution" => item.get("command").map(compact_value),
        "fileChange" => item
            .get("path")
            .or_else(|| item.get("changes"))
            .map(compact_value),
        "mcpToolCall" => {
            let server = item.get("server").and_then(Value::as_str).unwrap_or("mcp");
            let tool = item
                .get("tool")
                .or_else(|| item.get("name"))
                .and_then(Value::as_str)
                .unwrap_or("tool");
            Some(format!("{server}/{tool}"))
        }
        "webSearch" => item.get("query").map(compact_value),
        "dynamicToolCall" | "toolCall" => item
            .get("tool")
            .or_else(|| item.get("name"))
            .map(compact_value),
        _ if kind.to_lowercase().contains("tool") || kind.to_lowercase().contains("command") => {
            item.get("name").map(compact_value)
        }
        _ => return None,
    };
    let detail = detail.filter(|value| !value.is_empty());
    Some(match detail {
        Some(detail) => format!("{kind}: {detail}"),
        None => kind.to_string(),
    })
}

fn compact_value(value: &Value) -> String {
    let value = value
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| value.to_string());
    let mut compact = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if compact.chars().count() > 240 {
        compact = compact.chars().take(240).collect::<String>();
        compact.push_str("...");
    }
    compact
}

fn progress_for_message(message: &Value, turn_id: &str) -> Option<(&'static str, &'static str)> {
    let params = message.get("params")?;

    if message_turn_id(params) != Some(turn_id) {
        return None;
    }

    match message.get("method").and_then(Value::as_str) {
        Some("turn/started") => Some(("working", "Codex is processing the request")),
        Some("item/started") => match params
            .get("item")
            .and_then(|item| item.get("type"))
            .and_then(Value::as_str)
        {
            Some("reasoning") => Some(("reviewing", "Codex is reviewing the provided context")),
            Some("agentMessage") => Some(("responding", "Codex is preparing a response")),
            _ => Some(("working", "Codex is processing the request")),
        },
        Some("item/completed")
            if params
                .get("item")
                .and_then(|item| item.get("type"))
                .and_then(Value::as_str)
                == Some("agentMessage") =>
        {
            Some(("validating", "Codex is validating the response"))
        }
        Some("turn/completed") => Some(("finishing", "Codex completed the response")),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalidating_a_process_discards_ephemeral_thread_state() {
        let mut state = CodexAppState::default();
        state.threads.insert("key".into(), "thread".into());
        state.context_fingerprints.insert("thread".into(), 42);

        state.invalidate_process();

        assert!(state.process.is_none());
        assert!(state.threads.is_empty());
        assert!(state.context_fingerprints.is_empty());
    }

    #[test]
    fn summarizes_tool_activity_without_reasoning_text() {
        let command = json!({
            "type": "commandExecution",
            "command": "rg layout_editor.html templates"
        });
        let reasoning = json!({"type": "reasoning", "text": "private"});

        assert!(
            activity_summary(&command)
                .unwrap()
                .contains("layout_editor.html")
        );
        assert_eq!(activity_summary(&reasoning), None);
    }

    #[test]
    fn parses_usage_from_app_server_notification() {
        let value = json!({
            "last": {
                "inputTokens": 10,
                "cachedInputTokens": 8,
                "outputTokens": 5,
                "totalTokens": 15
            }
        });
        let usage = parse_usage(Some(&value)).unwrap();

        assert_eq!(usage.input_tokens, 10);
        assert_eq!(usage.cached_input_tokens, 8);
        assert_eq!(usage.output_tokens, 5);
        assert!(!usage.estimated);
    }

    #[test]
    fn normalizes_progress_without_exposing_agent_text() {
        let event = json!({
            "method": "item/started",
            "params": {
                "turnId": "turn_1",
                "item": {
                    "type": "reasoning",
                    "text": "private model reasoning"
                }
            }
        });

        assert_eq!(
            progress_for_message(&event, "turn_1"),
            Some(("reviewing", "Codex is reviewing the provided context"))
        );
    }

    #[test]
    fn ignores_progress_for_another_turn() {
        let event = json!({
            "method": "turn/completed",
            "params": {"turnId": "turn_1"}
        });

        assert_eq!(progress_for_message(&event, "turn_2"), None);
    }
}
