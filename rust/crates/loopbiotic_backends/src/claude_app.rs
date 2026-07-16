use std::collections::HashMap;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Result, anyhow};
use async_trait::async_trait;
use loopbiotic_protocol::{BackendInfo, Card, TokenUsage};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::Mutex;

use crate::support::{
    Phase, TurnTimedOut, action_value, args_from_env, await_turn, context_fingerprint, error_card,
    optional_env, report_progress, turn_phase, turn_timeout_from_env,
};
use crate::{
    BackendAdapter, BackendIdentity, BackendMetadata, BackendPhaseModels, BackendRequest,
    BackendResponse, ProgressReporter, enforce_card_contract, estimate_tokens,
};

const SYSTEM_PROMPT: &str = r#"You are a local Loopbiotic pair-programming partner inside the user's editor.
Every user message is a JSON Loopbiotic request. Reply with exactly one JSON Loopbiotic op and nothing else: no prose, no markdown fences.
The discriminator field is named "op". Allowed ops, with exact shapes:
- {"op":"hypothesis","title":string,"claim":string,"evidence":LOC|null,"next":LOC|null}
- {"op":"finding","title":string,"finding":string,"location":LOC|null,"annotation":string|null}
- {"op":"patch","title":string,"explanation":string,"goal_complete":bool,"patches":[{"id":string|null,"file":string,"diff":string,"explanation":string}]}
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
Patch only for fix actions or when limits.goal_completion is true. patch.diff must be unified diff hunks starting with @@ against the corresponding project source.
When limits.goal_completion is true, drive the original goal from start to finish in one work turn. Inspect every required file with read-only tools and return the complete multi-file patch batch within limits.patch_files, limits.hunks_per_patch, and limits.changed_lines. Tool reads are valid patch source because Loopbiotic verifies every hunk against the live editor buffer before review. Loopbiotic reviews the batch locally without another model turn. Create missing files directly in the same batch. Set goal_complete=true when accepting the complete batch finishes the original goal. Use open_location only when a required source cannot be inspected. Return summary only when every stated requirement was already satisfied. Continue automatically from completed_steps and never repeat accepted work.
When limits.goal_completion is true and limits.expected is finding because the user asked why, explain the currently pending hunk without replacing it or advancing the goal. The same draft remains pending after the answer.
A non-goal patch is one small local pair-programming step: one file, one hunk, no more changed lines than the supplied limit. A goal patch contains the complete change for the current buffer and may contain multiple hunks within the supplied limits.
Prefer the supplied context; you may use at most two targeted read-only searches when it is insufficient. Never edit files or run commands."#;

/// Keeps `claude` CLI processes alive across turns using its stream-json
/// stdin/stdout mode. Each Loopbiotic session gets up to two processes: a discovery
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
    turn_timeout: Option<Duration>,
    state: Mutex<ClaudeAppState>,
}

#[derive(Default)]
struct ClaudeAppState {
    session_key: Option<String>,
    slots: HashMap<Phase, Arc<Mutex<ClaudeSlot>>>,
    // Pre-spawned discovery process created by warmup() before a session
    // exists, adopted by the next session's first discovery turn.
    warm: Option<Arc<Mutex<ClaudeSlot>>>,
    // Model a flagless CLI process reported — its true default. Only ever
    // written from processes that ran without --model, so a pinned
    // discovery model can never masquerade as the CLI default.
    cli_default_model: Option<String>,
}

#[derive(Default)]
struct ClaudeSlot {
    process: Option<ClaudeAppProcess>,
    context_fingerprint: Option<u64>,
    model: Option<String>,
    reported_model: Option<String>,
}

impl ClaudeSlot {
    /// Kills a wedged CLI and forgets its conversation so the next turn
    /// spawns a fresh process with full context.
    fn kill_process(&mut self) {
        if let Some(process) = self.process.as_mut() {
            let _ = process.child.start_kill();
        }
        self.process = None;
        self.context_fingerprint = None;
        self.reported_model = None;
    }
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

#[derive(Debug)]
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
        let command =
            std::env::var("LOOPBIOTIC_CLAUDE_COMMAND").unwrap_or_else(|_| "claude".into());
        let args = args_from_env("LOOPBIOTIC_CLAUDE_ARGS_JSON", "LOOPBIOTIC_CLAUDE_ARGS", "")?;
        let model = optional_env("LOOPBIOTIC_CLAUDE_MODEL");
        let discovery_model = optional_env("LOOPBIOTIC_CLAUDE_DISCOVERY_MODEL");
        let discovery_thinking = optional_env("LOOPBIOTIC_CLAUDE_DISCOVERY_THINKING");

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
        Self::with_turn_timeout(
            command,
            args,
            model,
            discovery_model,
            discovery_thinking,
            turn_timeout_from_env(),
        )
    }

    /// Internal constructor that fixes the per-turn deadline instead of
    /// reading it from the environment; tests use it to avoid env races.
    pub(crate) fn with_turn_timeout(
        command: impl Into<String>,
        args: Vec<String>,
        model: Option<String>,
        discovery_model: Option<String>,
        discovery_thinking: Option<String>,
        turn_timeout: Option<Duration>,
    ) -> Self {
        Self {
            command: command.into(),
            args,
            model,
            discovery_model,
            discovery_thinking,
            turn_timeout,
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

    /// Resolves the CLI's own default model (what a process spawned without
    /// --model runs): cached, else read from the warm discovery process when
    /// discovery itself is flagless, else from a short-lived flagless probe.
    async fn cli_default_model(&self) -> Option<String> {
        if let Some(model) = self.state.lock().await.cli_default_model.clone() {
            return Some(model);
        }

        let model = if self.phase_model(Phase::Discovery).is_none() {
            self.warm_init_model().await
        } else {
            self.probe_default_model().await
        };

        if let Some(model) = model.as_ref() {
            self.state.lock().await.cli_default_model = Some(model.clone());
        }

        model
    }

    async fn warm_init_model(&self) -> Option<String> {
        self.warm_up().await.ok()?;
        let warm = self.state.lock().await.warm.clone()?;
        let mut slot = warm.lock().await;
        if slot.reported_model.is_none() {
            // The warm process has not run a turn yet, so its init event is
            // still unread on stdout.
            slot.reported_model = read_init_model(&mut slot).await;
        }

        slot.reported_model.clone()
    }

    /// The warm discovery process runs a pinned model, so it cannot reveal
    /// the CLI default; spawn a flagless process just long enough to read
    /// its init event.
    async fn probe_default_model(&self) -> Option<String> {
        let mut slot = ClaudeSlot {
            process: self.spawn_process(&None, &None).ok(),
            ..ClaudeSlot::default()
        };
        let model = read_init_model(&mut slot).await;
        slot.kill_process();

        model
    }

    /// Returns the slot for this turn's phase, creating it (or adopting the
    /// warm process) as needed. The outer state lock is held only for the
    /// bookkeeping; the returned slot's own lock serializes the actual turn,
    /// so discovery and patch turns can run concurrently.
    async fn slot(&self, session_key: &str, phase: Phase) -> Arc<Mutex<ClaudeSlot>> {
        let mut state = self.state.lock().await;

        if state.session_key.as_deref() != Some(session_key) {
            // One process holds one conversation; a new Loopbiotic session must not
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

        self.guarded_turn(&mut slot, req, phase, progress).await
    }

    /// Runs one turn under the per-turn deadline. On expiry the wedged CLI is
    /// killed and its slot cleared so the next turn spawns a fresh process.
    async fn guarded_turn(
        &self,
        slot: &mut ClaudeSlot,
        req: &BackendRequest,
        phase: Phase,
        progress: Option<&ProgressReporter>,
    ) -> Result<TurnOutput> {
        let result = await_turn(
            "Claude",
            self.turn_timeout,
            self.run_turn(slot, req, phase, progress),
        )
        .await;

        if result
            .as_ref()
            .is_err_and(|error| error.is::<TurnTimedOut>())
        {
            slot.kill_process();
        }

        result
    }

    async fn run_turn(
        &self,
        slot: &mut ClaudeSlot,
        req: &BackendRequest,
        phase: Phase,
        progress: Option<&ProgressReporter>,
    ) -> Result<TurnOutput> {
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

        if let Err(error) = send_turn(slot, &turn_prompt(req, include_context)).await {
            // The process may have died between turns; retry once on a fresh
            // process with full context before giving up.
            slot.process = Some(self.spawn_process(&slot.model, &self.phase_thinking(phase))?);
            slot.context_fingerprint = Some(fingerprint);
            send_turn(slot, &turn_prompt(req, true))
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
        error_card("c_claude_error", "Claude error", message)
    }
}

/// Reads the freshly spawned CLI's init/system event, which names the model
/// the process will use. Bounded by a short deadline so identity() can never
/// hang on a wedged CLI; only called before the process's first turn.
async fn read_init_model(slot: &mut ClaudeSlot) -> Option<String> {
    const INIT_TIMEOUT: Duration = Duration::from_secs(5);

    let process = slot.process.as_mut()?;
    let init = async {
        while let Ok(Some(line)) = process.stdout.next_line().await {
            let Ok(value) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            if let StreamEvent::Init(model) = parse_stream_event(&value) {
                return model;
            }
        }
        None
    };

    tokio::time::timeout(INIT_TIMEOUT, init)
        .await
        .ok()
        .flatten()
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
        if let Some(model) = output.model.as_ref() {
            if self.phase_model(turn_phase(&req)).is_none() {
                self.state.lock().await.cli_default_model = Some(model.clone());
            }
        }
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

    async fn identity(&self) -> BackendIdentity {
        // `model` names the patch-phase model — the one that writes code.
        // A pinned discovery model is reported separately via `phases` so a
        // cheap discovery default is never presented as "the" model.
        let patch = match self.model.clone() {
            Some(model) => Some(model),
            None => self.cli_default_model().await,
        };
        let discovery = self.discovery_model.clone().or_else(|| patch.clone());
        let phases = (discovery != patch).then(|| BackendPhaseModels {
            discovery: discovery.clone(),
            patch: patch.clone(),
        });

        // The CLI has no model-listing API; offer the concrete models we
        // know about plus the stable aliases the CLI resolves server-side.
        let mut models: Vec<String> = vec![];
        for candidate in [&patch, &discovery].into_iter().flatten() {
            if !models.contains(candidate) {
                models.push(candidate.clone());
            }
        }
        for alias in ["sonnet", "opus", "haiku"] {
            if !models.iter().any(|known| known == alias) {
                models.push(alias.into());
            }
        }

        BackendIdentity {
            backend: "claude_app".into(),
            model: patch,
            models,
            phases,
        }
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
    let field = |name: &str| usage.get(name).and_then(Value::as_u64).unwrap_or(0) as usize;

    // Claude Code splits input across three counters: `input_tokens` is only the
    // fresh, uncached slice of the final request, while the (usually dominant)
    // rest of the context is billed through `cache_creation_input_tokens` and
    // `cache_read_input_tokens`. Reading `input_tokens` alone under-reported a
    // tool-heavy turn by an order of magnitude (e.g. 3k of a real 41k input).
    // The `result` event's usage is cumulative across the whole tool loop, so
    // summing the three input counters yields the real billed input; the cached
    // subset is the cache-read portion.
    let fresh_input = field("input_tokens");
    let cache_creation = field("cache_creation_input_tokens");
    let cache_read = field("cache_read_input_tokens");
    let output = field("output_tokens");

    if fresh_input == 0 && cache_creation == 0 && cache_read == 0 && output == 0 {
        return None;
    }

    let input = fresh_input + cache_creation + cache_read;
    Some(TokenUsage {
        input_tokens: input,
        cached_input_tokens: cache_read,
        output_tokens: output,
        total_tokens: input + output,
        estimated: false,
    })
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
    fn usage_counts_cached_and_cache_creation_input() {
        // A tool-heavy turn: the fresh `input_tokens` is a small slice of the
        // real billed input, which lives in the two cache counters.
        let value = json!({
            "type": "result",
            "result": "{\"op\":\"finding\",\"title\":\"T\",\"finding\":\"F\"}",
            "usage": {
                "input_tokens": 3024,
                "cache_creation_input_tokens": 20577,
                "cache_read_input_tokens": 17371,
                "output_tokens": 152
            }
        });

        let StreamEvent::Result { token_usage, .. } = parse_stream_event(&value) else {
            panic!("expected result event");
        };
        let usage = token_usage.unwrap();
        assert_eq!(usage.input_tokens, 3024 + 20577 + 17371);
        assert_eq!(usage.cached_input_tokens, 17371);
        assert_eq!(usage.output_tokens, 152);
        assert_eq!(usage.total_tokens, 3024 + 20577 + 17371 + 152);
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

    #[tokio::test]
    async fn wedged_claude_process_times_out_and_clears_the_slot() {
        // A `sleep` child stands in for a wedged CLI: it accepts the turn on
        // stdin but never writes a stream event to stdout.
        let mut child = Command::new("sleep")
            .arg("30")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let mut slot = ClaudeSlot {
            process: Some(ClaudeAppProcess {
                child,
                stdin,
                stdout: BufReader::new(stdout).lines(),
            }),
            ..ClaudeSlot::default()
        };
        let backend = ClaudeAppBackend::with_turn_timeout(
            "claude-unused",
            vec![],
            None,
            None,
            None,
            Some(Duration::from_millis(100)),
        );

        let error = backend
            .guarded_turn(&mut slot, &crate::test_request(), Phase::Discovery, None)
            .await
            .unwrap_err();

        assert!(error.is::<TurnTimedOut>(), "unexpected error: {error}");
        assert!(
            slot.process.is_none(),
            "timed-out process must be cleared so the next turn spawns fresh"
        );
    }

    #[tokio::test]
    async fn identity_reports_the_configured_model_without_spawning() {
        // "claude-unused" does not exist; the configured path must answer
        // without ever spawning a process.
        let backend = ClaudeAppBackend::with_turn_timeout(
            "claude-unused",
            vec![],
            Some("claude-fable-5".into()),
            None,
            None,
            None,
        );

        let identity = backend.identity().await;

        assert_eq!(identity.backend, "claude_app");
        assert_eq!(identity.model.as_deref(), Some("claude-fable-5"));
        assert!(identity.phases.is_none());
        assert_eq!(
            identity.models,
            ["claude-fable-5", "sonnet", "opus", "haiku"]
        );
    }

    #[tokio::test]
    async fn identity_reports_phase_models_when_discovery_differs() {
        let backend = ClaudeAppBackend::with_turn_timeout(
            "claude-unused",
            vec![],
            Some("claude-opus-4-8".into()),
            Some("claude-haiku-4-5".into()),
            None,
            None,
        );

        let identity = backend.identity().await;

        // The patch model is "the" model; the pinned discovery model rides
        // along in phases instead of hijacking the headline.
        assert_eq!(identity.model.as_deref(), Some("claude-opus-4-8"));
        let phases = identity.phases.expect("differing phases must be reported");
        assert_eq!(phases.discovery.as_deref(), Some("claude-haiku-4-5"));
        assert_eq!(phases.patch.as_deref(), Some("claude-opus-4-8"));
    }

    #[tokio::test]
    async fn identity_never_reports_a_pinned_discovery_model_as_the_model() {
        // The shipped default config: discovery pinned to a cheap model, no
        // patch model configured. The probe command does not exist, so the
        // CLI default stays unknown — identity must say so rather than
        // claim the discovery model is what patch turns will run.
        let backend = ClaudeAppBackend::with_turn_timeout(
            "claude-unused",
            vec![],
            None,
            Some("haiku".into()),
            None,
            None,
        );

        let identity = backend.identity().await;

        assert_eq!(identity.model, None);
        let phases = identity.phases.expect("differing phases must be reported");
        assert_eq!(phases.discovery.as_deref(), Some("haiku"));
        assert_eq!(phases.patch, None);
        assert_eq!(identity.models, ["haiku", "sonnet", "opus"]);
    }

    #[tokio::test]
    async fn identity_falls_back_to_the_cached_cli_default() {
        let backend =
            ClaudeAppBackend::with_turn_timeout("claude-unused", vec![], None, None, None, None);
        backend.state.lock().await.cli_default_model = Some("claude-fable-5".into());

        let identity = backend.identity().await;

        assert_eq!(identity.model.as_deref(), Some("claude-fable-5"));
        assert!(identity.phases.is_none());
    }

    #[tokio::test]
    async fn read_init_model_parses_the_init_event_from_the_process_stream() {
        // `echo` stands in for the CLI: it prints the init event and exits.
        let init = json!({
            "type": "system",
            "subtype": "init",
            "session_id": "abc",
            "model": "claude-fable-5"
        });
        let mut child = Command::new("echo")
            .arg(init.to_string())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let mut slot = ClaudeSlot {
            process: Some(ClaudeAppProcess {
                child,
                stdin,
                stdout: BufReader::new(stdout).lines(),
            }),
            ..ClaudeSlot::default()
        };

        assert_eq!(
            read_init_model(&mut slot).await.as_deref(),
            Some("claude-fable-5")
        );
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
