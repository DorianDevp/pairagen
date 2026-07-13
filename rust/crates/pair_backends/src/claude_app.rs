use std::hash::{DefaultHasher, Hash, Hasher};
use std::process::Stdio;

use anyhow::{Result, anyhow};
use async_trait::async_trait;
use pair_protocol::{Action, BackendInfo, Card, ErrorCard, TokenUsage};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::Mutex;

use crate::{
    BackendAction, BackendAdapter, BackendMetadata, BackendProgress, BackendRequest,
    BackendResponse, ProgressReporter, enforce_card_contract, estimate_tokens,
};

const SYSTEM_PROMPT: &str = r#"You are a local Pairagen pair-programming partner inside the user's editor.
Every user message is a JSON Pair request. Reply with exactly one JSON Pair op and nothing else: no prose, no markdown fences.
The discriminator field is named "op". Allowed ops, with exact shapes:
- {"op":"hypothesis","title":string,"claim":string,"evidence":LOC|null,"next":LOC|null}
- {"op":"finding","title":string,"finding":string,"location":LOC|null,"annotation":string|null}
- {"op":"patch","title":string,"explanation":string,"patches":[{"id":string|null,"file":string,"diff":string,"explanation":string}]}
- {"op":"choice","title":string,"question":string,"options":[{"id":string,"label":string,"action":string}]}
- {"op":"deny","title":string,"reason":string}
- {"op":"summary","title":string,"summary":string,"changed_files":[string]}
- {"op":"error","title":string,"message":string}
LOC is an object {"file":string,"line":int,"column":int,"annotation":string|null} with 1-based line and column; never a plain string.
choice option action is one of follow|why|fix|other_lead|retry|edit_prompt|open|run_check|next|stop.
Use deny when you cannot or should not proceed (ambiguous prompt, missing information, out-of-scope request); reason is shown to the user. error is only for technical failures.
Patch only for fix actions. patch.diff must be unified diff hunks starting with @@ against the supplied buffer.
A patch is one small local pair-programming step: one file, one hunk, no more changed lines than the supplied limit. Never plan or complete a whole refactor in one response.
Prefer the supplied context; you may use at most two targeted read-only searches when it is insufficient. Never edit files or run commands."#;

/// Keeps one `claude` CLI process alive per Pair session using its
/// stream-json stdin/stdout mode, so follow-up cards skip the CLI cold start
/// and reuse the conversation instead of resending the whole session.
pub struct ClaudeAppBackend {
    command: String,
    args: Vec<String>,
    model: Option<String>,
    state: Mutex<ClaudeAppState>,
}

#[derive(Default)]
struct ClaudeAppState {
    process: Option<ClaudeAppProcess>,
    session_key: Option<String>,
    context_fingerprint: Option<u64>,
    model: Option<String>,
}

struct ClaudeAppProcess {
    child: Child,
    stdin: ChildStdin,
    stdout: Lines<BufReader<ChildStdout>>,
}

impl Drop for ClaudeAppProcess {
    fn drop(&mut self) {
        let _ = self.child.start_kill();
    }
}

struct TurnOutput {
    text: String,
    token_usage: Option<TokenUsage>,
    model: Option<String>,
}

enum StreamEvent {
    Init(Option<String>),
    Working(String),
    Result {
        text: String,
        token_usage: Option<TokenUsage>,
    },
    Failed(String),
    Other,
}

impl ClaudeAppBackend {
    pub fn from_env() -> Result<Self> {
        let command = std::env::var("PAIR_CLAUDE_COMMAND").unwrap_or_else(|_| "claude".into());
        let args = args_from_env("PAIR_CLAUDE_ARGS_JSON", "PAIR_CLAUDE_ARGS")?;
        let model = std::env::var("PAIR_CLAUDE_MODEL")
            .ok()
            .filter(|value| !value.trim().is_empty());

        Ok(Self::new(command, args, model))
    }

    pub fn new(command: impl Into<String>, args: Vec<String>, model: Option<String>) -> Self {
        Self {
            command: command.into(),
            args,
            model,
            state: Mutex::new(ClaudeAppState::default()),
        }
    }

    fn spawn_args(&self) -> Vec<String> {
        let mut args = vec![
            "-p".into(),
            "--input-format".into(),
            "stream-json".into(),
            "--output-format".into(),
            "stream-json".into(),
            "--verbose".into(),
            "--disallowedTools".into(),
            "Edit,Write,NotebookEdit,Bash".into(),
            "--append-system-prompt".into(),
            SYSTEM_PROMPT.into(),
        ];

        if let Some(model) = &self.model {
            args.push("--model".into());
            args.push(model.clone());
        }

        args.extend(self.args.iter().cloned());

        args
    }

    async fn ensure(&self, state: &mut ClaudeAppState, session_key: &str) -> Result<()> {
        // One Claude process holds one conversation; a new Pair session must
        // not inherit the previous session's context.
        if state.session_key.as_deref() != Some(session_key) {
            state.process = None;
            state.context_fingerprint = None;
            state.session_key = Some(session_key.to_string());
        }

        if state.process.is_some() {
            return Ok(());
        }

        let mut child = Command::new(&self.command)
            .args(self.spawn_args())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow!("claude stdin unavailable"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("claude stdout unavailable"))?;

        state.process = Some(ClaudeAppProcess {
            child,
            stdin,
            stdout: BufReader::new(stdout).lines(),
        });

        Ok(())
    }

    async fn ask(
        &self,
        req: &BackendRequest,
        progress: Option<&ProgressReporter>,
    ) -> Result<TurnOutput> {
        let mut state = self.state.lock().await;
        let fresh = state.session_key.as_deref() != Some(req.session.id.as_str())
            || state.process.is_none();

        report_progress(
            progress,
            &req.session.id,
            "starting",
            if fresh {
                "Starting Claude"
            } else {
                "Reusing the Claude session"
            },
        );
        self.ensure(&mut state, &req.session.id).await?;

        let fingerprint = context_fingerprint(req);
        let include_context = state.context_fingerprint != Some(fingerprint);
        state.context_fingerprint = Some(fingerprint);

        let prompt = turn_prompt(req, include_context);
        let message = json!({
            "type": "user",
            "message": {
                "role": "user",
                "content": [{"type": "text", "text": prompt}]
            }
        });
        let line = serde_json::to_string(&message)?;

        if let Err(error) = Self::send(&mut state, &line).await {
            // The previous process may have died between turns; retry once on
            // a fresh process before giving up.
            state.process = None;
            state.context_fingerprint = None;
            self.ensure(&mut state, &req.session.id).await?;
            let prompt = turn_prompt(req, true);
            let message = json!({
                "type": "user",
                "message": {
                    "role": "user",
                    "content": [{"type": "text", "text": prompt}]
                }
            });
            Self::send(&mut state, &serde_json::to_string(&message)?)
                .await
                .map_err(|retry_error| {
                    anyhow!("could not reach claude: {error}; retry failed: {retry_error}")
                })?;
        }

        report_progress(
            progress,
            &req.session.id,
            "working",
            "Claude is processing the request",
        );

        loop {
            let line = {
                let process = state
                    .process
                    .as_mut()
                    .ok_or_else(|| anyhow!("claude process unavailable"))?;
                match process.stdout.next_line().await? {
                    Some(line) => line,
                    None => {
                        state.process = None;
                        return Err(anyhow!(
                            "claude exited before finishing the turn; check that the claude CLI is logged in"
                        ));
                    }
                }
            };

            if line.trim().is_empty() {
                continue;
            }

            let Ok(value) = serde_json::from_str::<Value>(&line) else {
                continue;
            };

            match parse_stream_event(&value) {
                StreamEvent::Init(model) => {
                    state.model = self.model.clone().or(model);
                }
                StreamEvent::Working(activity) => {
                    report_progress(progress, &req.session.id, "working", &activity);
                }
                StreamEvent::Result { text, token_usage } => {
                    return Ok(TurnOutput {
                        text,
                        token_usage,
                        model: state.model.clone().or_else(|| self.model.clone()),
                    });
                }
                StreamEvent::Failed(message) => {
                    return Err(anyhow!("claude turn failed: {message}"));
                }
                StreamEvent::Other => {}
            }
        }
    }

    async fn send(state: &mut ClaudeAppState, line: &str) -> Result<()> {
        let process = state
            .process
            .as_mut()
            .ok_or_else(|| anyhow!("claude process unavailable"))?;

        process.stdin.write_all(line.as_bytes()).await?;
        process.stdin.write_all(b"\n").await?;
        process.stdin.flush().await?;

        Ok(())
    }

    fn error_card(message: impl Into<String>) -> Card {
        Card::Error(ErrorCard {
            id: "c_claude_error".into(),
            title: "Claude error".into(),
            message: message.into(),
            actions: vec![Action::Retry, Action::EditPrompt, Action::Stop],
        })
    }
}

#[async_trait]
impl BackendAdapter for ClaudeAppBackend {
    async fn next_card(&self, req: BackendRequest) -> Result<BackendResponse> {
        self.next_card_with_progress(req, None).await
    }

    async fn next_card_with_progress(
        &self,
        req: BackendRequest,
        progress: Option<ProgressReporter>,
    ) -> Result<BackendResponse> {
        let output = self.ask(&req, progress.as_ref()).await?;
        let card = crate::parse_card(&output.text).unwrap_or_else(|error| {
            Self::error_card(format!("{error}\n\nRaw output:\n{}", output.text))
        });
        let card = enforce_card_contract(card, &req.card_contract, "Claude", &output.text);
        let token_usage = output.token_usage.unwrap_or_else(|| {
            TokenUsage::estimated(
                estimate_tokens(&turn_prompt(&req, true)),
                estimate_tokens(&output.text),
            )
        });

        Ok(BackendResponse {
            card,
            raw_output: Some(output.text),
            metadata: BackendMetadata {
                backend: "claude_app".into(),
                model: output.model,
                token_usage: Some(token_usage),
                activities: vec![],
                attempts: vec![],
            },
        })
    }

    fn capabilities(&self) -> BackendInfo {
        BackendInfo {
            name: "claude_app".into(),
            streaming: true,
            patches: true,
            reasoning: true,
            can_read_project: true,
            can_use_tools: true,
        }
    }
}

fn turn_prompt(req: &BackendRequest, include_context: bool) -> String {
    let mut value = json!({
        "s": {
            "id": req.session.id,
            "p": req.session.prompt,
            "completed_steps": req.session.completed_steps,
            "known_observations": req.session.known_observations,
            "mode": req.session.mode,
            "n": req.session.card_count,
            "last": req.session.last_summary
        },
        "a": action_value(&req.action),
        "limits": {
            "one": req.card_contract.one_card_only,
            "max": req.card_contract.max_body_chars,
            "patch_files": req.card_contract.max_patch_files,
            "hunks_per_patch": req.card_contract.max_hunks_per_patch,
            "changed_lines": req.card_contract.max_changed_lines,
            "goal_completion": req.card_contract.allow_goal_completion
        }
    });

    if include_context {
        value["ctx"] = crate::backend_context(&req.context);
    } else {
        value["ctx"] = json!("unchanged; reuse the context from the previous message");
    }

    serde_json::to_string(&value).unwrap_or_default()
}

fn context_fingerprint(req: &BackendRequest) -> u64 {
    let mut hasher = DefaultHasher::new();
    req.context.file.hash(&mut hasher);
    req.context.cursor.line.hash(&mut hasher);
    req.context.cursor.column.hash(&mut hasher);
    req.context.buffer_start_line.hash(&mut hasher);
    req.context.buffer_text.hash(&mut hasher);
    for diagnostic in &req.context.diagnostics {
        diagnostic.file.hash(&mut hasher);
        diagnostic.line.hash(&mut hasher);
        diagnostic.message.hash(&mut hasher);
    }
    for artifact in &req.context.artifacts {
        artifact.file.hash(&mut hasher);
        artifact.start_line.hash(&mut hasher);
        artifact.text.hash(&mut hasher);
    }
    hasher.finish()
}

fn action_value(action: &BackendAction) -> Value {
    match action {
        BackendAction::Start => json!({"kind": "start"}),
        BackendAction::User(action) => {
            json!({"kind": "user", "action": serde_json::to_value(action).unwrap_or_default()})
        }
        BackendAction::Reply(text) => json!({"kind": "reply", "text": text}),
        BackendAction::ContractRetry(reason) => {
            json!({"kind": "contract_retry", "reason": reason})
        }
    }
}

fn parse_stream_event(value: &Value) -> StreamEvent {
    match value.get("type").and_then(Value::as_str) {
        Some("system") if value.get("subtype").and_then(Value::as_str) == Some("init") => {
            StreamEvent::Init(
                value
                    .get("model")
                    .and_then(Value::as_str)
                    .map(str::to_string),
            )
        }
        Some("result") => {
            let text = value
                .get("result")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let failed = value.get("is_error").and_then(Value::as_bool) == Some(true)
                || value.get("error").is_some_and(|error| !error.is_null());

            if failed {
                let message = value
                    .get("error")
                    .filter(|error| !error.is_null())
                    .map(Value::to_string)
                    .unwrap_or_else(|| text.clone());
                return StreamEvent::Failed(message);
            }

            StreamEvent::Result {
                text,
                token_usage: parse_usage(value.get("usage")),
            }
        }
        Some("assistant") => {
            let tool = value
                .get("message")
                .and_then(|message| message.get("content"))
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .find_map(|block| {
                    (block.get("type").and_then(Value::as_str) == Some("tool_use"))
                        .then(|| block.get("name").and_then(Value::as_str))
                        .flatten()
                });

            match tool {
                Some(name) => StreamEvent::Working(format!("Claude is using {name}")),
                None => StreamEvent::Working("Claude is drafting the next Pair card".into()),
            }
        }
        _ => StreamEvent::Other,
    }
}

fn parse_usage(value: Option<&Value>) -> Option<TokenUsage> {
    let usage = value?;
    let input = usage.get("input_tokens")?.as_u64()? as usize;
    let output = usage.get("output_tokens")?.as_u64()? as usize;

    Some(TokenUsage::reported(input, output))
}

fn args_from_env(json_name: &str, plain_name: &str) -> Result<Vec<String>> {
    if let Ok(value) = std::env::var(json_name)
        && !value.trim().is_empty()
    {
        return Ok(serde_json::from_str(&value)?);
    }

    Ok(std::env::var(plain_name)
        .unwrap_or_default()
        .split_whitespace()
        .map(str::to_string)
        .collect())
}

fn report_progress(
    progress: Option<&ProgressReporter>,
    session_id: &str,
    phase: &str,
    message: &str,
) {
    if let Some(progress) = progress {
        progress(BackendProgress {
            session_id: session_id.into(),
            phase: phase.into(),
            message: message.into(),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_result_text_and_usage() {
        let value = json!({
            "type": "result",
            "result": "{\"op\":\"finding\",\"title\":\"T\",\"finding\":\"F\"}",
            "usage": {"input_tokens": 120, "output_tokens": 30},
            "error": null
        });

        let StreamEvent::Result { text, token_usage } = parse_stream_event(&value) else {
            panic!("expected result event");
        };
        assert!(text.contains("\"op\":\"finding\""));
        let usage = token_usage.unwrap();
        assert_eq!(usage.input_tokens, 120);
        assert_eq!(usage.output_tokens, 30);
        assert!(!usage.estimated);
    }

    #[test]
    fn extracts_model_from_init_event() {
        let value = json!({
            "type": "system",
            "subtype": "init",
            "session_id": "abc",
            "model": "claude-opus-4-8"
        });

        let StreamEvent::Init(model) = parse_stream_event(&value) else {
            panic!("expected init event");
        };
        assert_eq!(model.as_deref(), Some("claude-opus-4-8"));
    }

    #[test]
    fn detects_failed_turns() {
        let value = json!({
            "type": "result",
            "result": "credit balance too low",
            "is_error": true
        });

        assert!(matches!(parse_stream_event(&value), StreamEvent::Failed(_)));
    }

    #[test]
    fn reports_tool_use_as_activity() {
        let value = json!({
            "type": "assistant",
            "message": {"content": [{"type": "tool_use", "name": "Grep", "input": {}}]}
        });

        let StreamEvent::Working(activity) = parse_stream_event(&value) else {
            panic!("expected working event");
        };
        assert!(activity.contains("Grep"));
    }

    #[test]
    fn turn_prompt_omits_unchanged_context() {
        let req = crate::test_request();
        let with_context = turn_prompt(&req, true);
        let without_context = turn_prompt(&req, false);

        assert!(with_context.contains("buffer_text"));
        assert!(!without_context.contains("buffer_text"));
        assert!(without_context.contains("unchanged"));
    }
}
