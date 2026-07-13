use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::process::Stdio;
use std::sync::Arc;

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
- {"op":"deny","title":string,"reason":string,"location":LOC|null}
- {"op":"open_location","reason":string,"location":LOC}
- {"op":"summary","title":string,"summary":string,"changed_files":[string]}
- {"op":"error","title":string,"message":string}
LOC is an object {"file":string,"line":int,"column":int,"annotation":string|null} with 1-based line and column; never a plain string.
choice option action is one of follow|why|fix|other_lead|retry|edit_prompt|open|run_check|next|stop.
Use deny when you cannot or should not proceed (ambiguous prompt, missing information, out-of-scope request); reason is shown to the user. error is only for technical failures.
If you can only proceed from a different file or location — for example the change belongs in another file than the supplied buffer — return open_location IMMEDIATELY with that exact place instead of attempting a patch. The editor asks the user for permission, opens the file, and the next message continues this same turn with a.kind "location_granted" and fresh ctx for that buffer; then produce the real op. Never draft a patch against a file that is not the supplied buffer. Use deny only for refusals that navigation cannot solve.
limits.expected, when set, names the op you must return (deny is always allowed instead; a clarifying choice is also accepted for hypothesis and finding). When limits.expected is null, choose whichever op fits best and ask via choice when the request is ambiguous.
Patch only for fix actions. patch.diff must be unified diff hunks starting with @@ against the supplied buffer.
A patch is one small local pair-programming step: one file, one hunk, no more changed lines than the supplied limit. Never plan or complete a whole refactor in one response.
Prefer the supplied context; you may use at most two targeted read-only searches when it is insufficient. Never edit files or run commands."#;

/// Keeps `claude` CLI processes alive across turns using its stream-json
/// stdin/stdout mode. Each Pair session gets up to two processes: a discovery
/// process (hypothesis/finding/choice turns, optionally on a faster model
/// with a capped thinking budget) and a patch process (full model). Separate
/// processes let a speculative patch prefetch run while the user keeps
/// navigating discovery cards, and are required anyway because the CLI cannot
/// switch models within one process.
pub struct ClaudeAppBackend {
    command: String,
    args: Vec<String>,
    model: Option<String>,
    discovery_model: Option<String>,
    discovery_thinking: Option<String>,
    state: Mutex<ClaudeAppState>,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum Phase {
    Discovery,
    Patch,
}

#[derive(Default)]
struct ClaudeAppState {
    session_key: Option<String>,
    slots: HashMap<Phase, Arc<Mutex<ClaudeSlot>>>,
    // Pre-spawned discovery process created by warmup() before a session
    // exists, adopted by the next session's first discovery turn.
    warm: Option<Arc<Mutex<ClaudeSlot>>>,
}

#[derive(Default)]
struct ClaudeSlot {
    process: Option<ClaudeAppProcess>,
    context_fingerprint: Option<u64>,
    model: Option<String>,
    reported_model: Option<String>,
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
    Delta(String),
    Result {
        text: String,
        token_usage: Option<TokenUsage>,
    },
    Failed(String),
    Other,
}

/// Extracts card fields from the partially streamed op JSON so the editor can
/// show what is being drafted before the turn completes.
#[derive(Default)]
struct StreamPreview {
    buffer: String,
    title_reported: bool,
    body_reported: bool,
}

const PREVIEW_BODY_FIELDS: &[&str] = &[
    "claim",
    "finding",
    "question",
    "explanation",
    "reason",
    "summary",
    "message",
];
const PREVIEW_BUFFER_LIMIT: usize = 4096;
const PREVIEW_BODY_MIN_CHARS: usize = 40;
const PREVIEW_BODY_MAX_CHARS: usize = 72;

impl StreamPreview {
    fn push(&mut self, delta: &str) -> Option<String> {
        if self.body_reported || self.buffer.len() > PREVIEW_BUFFER_LIMIT {
            return None;
        }

        self.buffer.push_str(delta);

        if !self.title_reported {
            let (title, complete) = extract_string_field(&self.buffer, "title")?;
            if !complete || title.trim().is_empty() {
                return None;
            }
            self.title_reported = true;
            return Some(format!("Drafting: {title}"));
        }

        let (title, _) = extract_string_field(&self.buffer, "title")?;
        let (body, complete) = PREVIEW_BODY_FIELDS
            .iter()
            .find_map(|field| extract_string_field(&self.buffer, field))?;
        if !complete && body.chars().count() < PREVIEW_BODY_MIN_CHARS {
            return None;
        }
        self.body_reported = true;
        let snippet = body
            .chars()
            .take(PREVIEW_BODY_MAX_CHARS)
            .collect::<String>();
        let ellipsis = if body.chars().count() > PREVIEW_BODY_MAX_CHARS || !complete {
            "…"
        } else {
            ""
        };

        Some(format!("{title}: {snippet}{ellipsis}"))
    }
}

/// Returns the (possibly still streaming) value of `"field":"..."` in `json`,
/// plus whether its closing quote has arrived.
fn extract_string_field(json: &str, field: &str) -> Option<(String, bool)> {
    let needle = format!("\"{field}\"");
    let start = json.find(&needle)? + needle.len();
    let rest = json[start..].trim_start();
    let rest = rest.strip_prefix(':')?.trim_start();
    let rest = rest.strip_prefix('"')?;

    let mut value = String::new();
    let mut chars = rest.chars();
    while let Some(next) = chars.next() {
        match next {
            '"' => return Some((value, true)),
            '\\' => match chars.next() {
                Some('n') => value.push('\n'),
                Some('t') => value.push('\t'),
                Some('u') => {
                    // Good enough for a preview: skip the escape digits.
                    for _ in 0..4 {
                        chars.next();
                    }
                    value.push('?');
                }
                Some(escaped) => value.push(escaped),
                None => return Some((value, false)),
            },
            _ => value.push(next),
        }
    }

    Some((value, false))
}

impl ClaudeAppBackend {
    pub fn from_env() -> Result<Self> {
        let command = std::env::var("PAIR_CLAUDE_COMMAND").unwrap_or_else(|_| "claude".into());
        let args = args_from_env("PAIR_CLAUDE_ARGS_JSON", "PAIR_CLAUDE_ARGS")?;
        let model = optional_env("PAIR_CLAUDE_MODEL");
        let discovery_model = optional_env("PAIR_CLAUDE_DISCOVERY_MODEL");
        let discovery_thinking = optional_env("PAIR_CLAUDE_DISCOVERY_THINKING");

        Ok(Self::new(
            command,
            args,
            model,
            discovery_model,
            discovery_thinking,
        ))
    }

    pub fn new(
        command: impl Into<String>,
        args: Vec<String>,
        model: Option<String>,
        discovery_model: Option<String>,
        discovery_thinking: Option<String>,
    ) -> Self {
        Self {
            command: command.into(),
            args,
            model,
            discovery_model,
            discovery_thinking,
            state: Mutex::new(ClaudeAppState::default()),
        }
    }

    fn phase_model(&self, phase: Phase) -> Option<String> {
        match phase {
            Phase::Patch => self.model.clone(),
            Phase::Discovery => self.discovery_model.clone().or_else(|| self.model.clone()),
        }
    }

    fn phase_thinking(&self, phase: Phase) -> Option<String> {
        match phase {
            // Patch turns keep the CLI's adaptive thinking: diff correctness
            // is where reasoning pays for itself.
            Phase::Patch => None,
            Phase::Discovery => self.discovery_thinking.clone(),
        }
    }

    fn spawn_args(&self, model: &Option<String>) -> Vec<String> {
        let mut args = vec![
            "-p".into(),
            "--input-format".into(),
            "stream-json".into(),
            "--output-format".into(),
            "stream-json".into(),
            "--verbose".into(),
            "--include-partial-messages".into(),
            "--disallowedTools".into(),
            "Edit,Write,NotebookEdit,Bash".into(),
            "--append-system-prompt".into(),
            SYSTEM_PROMPT.into(),
        ];

        if let Some(model) = model {
            args.push("--model".into());
            args.push(model.clone());
        }

        args.extend(self.args.iter().cloned());

        args
    }

    fn spawn_process(
        &self,
        model: &Option<String>,
        thinking: &Option<String>,
    ) -> Result<ClaudeAppProcess> {
        let mut command = Command::new(&self.command);
        command
            .args(self.spawn_args(model))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true);

        if let Some(thinking) = thinking {
            command.env("MAX_THINKING_TOKENS", thinking);
        }

        let mut child = command.spawn()?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow!("claude stdin unavailable"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("claude stdout unavailable"))?;

        Ok(ClaudeAppProcess {
            child,
            stdin,
            stdout: BufReader::new(stdout).lines(),
        })
    }

    /// Pre-spawns a discovery process before any session exists so the first
    /// card skips the CLI cold start. Called while the user is still typing
    /// the prompt; idempotent and cheap when something is already running.
    async fn warm_up(&self) -> Result<()> {
        let slot = {
            let mut state = self.state.lock().await;
            if !state.slots.is_empty() || state.warm.is_some() {
                return Ok(());
            }
            let slot = Arc::new(Mutex::new(ClaudeSlot {
                model: self.phase_model(Phase::Discovery),
                ..ClaudeSlot::default()
            }));
            state.warm = Some(slot.clone());
            slot
        };

        let mut slot = slot.lock().await;
        if slot.process.is_none() {
            slot.process = Some(self.spawn_process(
                &self.phase_model(Phase::Discovery),
                &self.phase_thinking(Phase::Discovery),
            )?);
        }

        Ok(())
    }

    /// Returns the slot for this turn's phase, creating it (or adopting the
    /// warm process) as needed. The outer state lock is held only for the
    /// bookkeeping; the returned slot's own lock serializes the actual turn,
    /// so discovery and patch turns can run concurrently.
    async fn slot(&self, session_key: &str, phase: Phase) -> Arc<Mutex<ClaudeSlot>> {
        let mut state = self.state.lock().await;

        if state.session_key.as_deref() != Some(session_key) {
            // One process holds one conversation; a new Pair session must not
            // inherit the previous session's context.
            state.slots.clear();
            state.session_key = Some(session_key.to_string());
        }

        if let Some(slot) = state.slots.get(&phase) {
            return slot.clone();
        }

        let wanted_model = self.phase_model(phase);
        let slot = match (phase, state.warm.take()) {
            (Phase::Discovery, Some(warm)) => {
                let adoptable = warm
                    .try_lock()
                    .map(|slot| slot.model == wanted_model)
                    .unwrap_or(false);
                if adoptable {
                    warm
                } else {
                    Arc::new(Mutex::new(ClaudeSlot {
                        model: wanted_model,
                        ..ClaudeSlot::default()
                    }))
                }
            }
            (_, warm) => {
                state.warm = warm;
                Arc::new(Mutex::new(ClaudeSlot {
                    model: wanted_model,
                    ..ClaudeSlot::default()
                }))
            }
        };

        state.slots.insert(phase, slot.clone());

        slot
    }

    async fn ask(
        &self,
        req: &BackendRequest,
        progress: Option<&ProgressReporter>,
    ) -> Result<TurnOutput> {
        let phase = turn_phase(req);
        let slot = self.slot(&req.session.id, phase).await;
        let mut slot = slot.lock().await;

        report_progress(
            progress,
            &req.session.id,
            "starting",
            if slot.process.is_some() {
                "Reusing the Claude session"
            } else {
                "Starting Claude"
            },
        );

        if slot.process.is_none() {
            slot.process = Some(self.spawn_process(&slot.model, &self.phase_thinking(phase))?);
            slot.context_fingerprint = None;
        }

        let fingerprint = context_fingerprint(req);
        let include_context = slot.context_fingerprint != Some(fingerprint);
        slot.context_fingerprint = Some(fingerprint);

        if let Err(error) = send_turn(&mut slot, &turn_prompt(req, include_context)).await {
            // The process may have died between turns; retry once on a fresh
            // process with full context before giving up.
            slot.process = Some(self.spawn_process(&slot.model, &self.phase_thinking(phase))?);
            slot.context_fingerprint = Some(fingerprint);
            send_turn(&mut slot, &turn_prompt(req, true))
                .await
                .map_err(|retry| anyhow!("could not reach claude: {error}; retry: {retry}"))?;
        }

        report_progress(
            progress,
            &req.session.id,
            "working",
            "Claude is processing the request",
        );

        let mut preview = StreamPreview::default();

        loop {
            let line = {
                let process = slot
                    .process
                    .as_mut()
                    .ok_or_else(|| anyhow!("claude process unavailable"))?;
                match process.stdout.next_line().await? {
                    Some(line) => line,
                    None => {
                        slot.process = None;
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
                    slot.reported_model = model;
                }
                StreamEvent::Working(activity) => {
                    report_progress(progress, &req.session.id, "working", &activity);
                }
                StreamEvent::Delta(text) => {
                    if let Some(message) = preview.push(&text) {
                        report_progress(progress, &req.session.id, "drafting", &message);
                    }
                }
                StreamEvent::Result { text, token_usage } => {
                    return Ok(TurnOutput {
                        text,
                        token_usage,
                        model: slot.reported_model.clone().or_else(|| slot.model.clone()),
                    });
                }
                StreamEvent::Failed(message) => {
                    return Err(anyhow!("claude turn failed: {message}"));
                }
                StreamEvent::Other => {}
            }
        }
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

async fn send_turn(slot: &mut ClaudeSlot, prompt: &str) -> Result<()> {
    let message = json!({
        "type": "user",
        "message": {
            "role": "user",
            "content": [{"type": "text", "text": prompt}]
        }
    });
    let mut line = serde_json::to_vec(&message)?;
    line.push(b'\n');
    let process = slot
        .process
        .as_mut()
        .ok_or_else(|| anyhow!("claude process unavailable"))?;

    process.stdin.write_all(&line).await?;
    process.stdin.flush().await?;

    Ok(())
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

    async fn warmup(&self) -> Result<()> {
        self.warm_up().await
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

fn turn_phase(req: &BackendRequest) -> Phase {
    if req.card_contract.expected_kind == Some(pair_protocol::CardKind::Patch)
        || req.card_contract.allow_goal_completion
    {
        Phase::Patch
    } else {
        Phase::Discovery
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
            "goal_completion": req.card_contract.allow_goal_completion,
            "expected": req.card_contract.expected_kind
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
        BackendAction::LocationGranted => json!({"kind": "location_granted"}),
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
        Some("stream_event") => {
            let delta = value.get("event").and_then(|event| event.get("delta"));
            let text = delta
                .filter(|delta| delta.get("type").and_then(Value::as_str) == Some("text_delta"))
                .and_then(|delta| delta.get("text"))
                .and_then(Value::as_str);

            match text {
                Some(text) => StreamEvent::Delta(text.to_string()),
                None => StreamEvent::Other,
            }
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
                None => StreamEvent::Other,
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

fn optional_env(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
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
    fn extracts_text_deltas_and_skips_thinking() {
        let text = json!({
            "type": "stream_event",
            "event": {"type": "content_block_delta", "delta": {"type": "text_delta", "text": "abc"}}
        });
        let thinking = json!({
            "type": "stream_event",
            "event": {"type": "content_block_delta", "delta": {"type": "thinking_delta", "thinking": "hmm"}}
        });

        assert!(matches!(
            parse_stream_event(&text),
            StreamEvent::Delta(delta) if delta == "abc"
        ));
        assert!(matches!(parse_stream_event(&thinking), StreamEvent::Other));
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
    fn preview_reports_title_then_body_once() {
        let mut preview = StreamPreview::default();

        assert_eq!(preview.push("{\"op\":\"hypothesis\",\"ti"), None);
        assert_eq!(
            preview.push("tle\":\"Falsy guard\","),
            Some("Drafting: Falsy guard".into())
        );
        assert_eq!(preview.push("\"claim\":\"The guard rejects"), None);
        let body = preview
            .push(" 0, empty strings and false, so callers lose data\"")
            .expect("body preview");
        assert!(body.starts_with("Falsy guard: The guard rejects 0"));
        assert_eq!(preview.push("\"more\":\"noise\""), None);
    }

    #[test]
    fn extract_string_field_handles_escapes_and_partials() {
        assert_eq!(
            extract_string_field(r#"{"title":"a \"quoted\" step""#, "title"),
            Some(("a \"quoted\" step".into(), true))
        );
        assert_eq!(
            extract_string_field(r#"{"title":"still stream"#, "title"),
            Some(("still stream".into(), false))
        );
        assert_eq!(extract_string_field(r#"{"titl"#, "title"), None);
    }

    #[test]
    fn routes_patch_turns_to_the_patch_phase() {
        let mut req = crate::test_request();
        assert!(matches!(turn_phase(&req), Phase::Discovery));

        req.card_contract.expected_kind = Some(pair_protocol::CardKind::Patch);
        assert!(matches!(turn_phase(&req), Phase::Patch));

        req.card_contract.expected_kind = None;
        req.card_contract.allow_goal_completion = true;
        assert!(matches!(turn_phase(&req), Phase::Patch));
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
