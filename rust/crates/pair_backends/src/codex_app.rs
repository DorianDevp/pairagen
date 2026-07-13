use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::process::Stdio;
use std::sync::Arc;

use anyhow::{Result, anyhow};
use async_trait::async_trait;
use pair_protocol::{Action, AgentOp, BackendInfo, Card, ErrorCard, TokenUsage};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::Mutex;

use crate::{
    BackendAction, BackendAdapter, BackendMetadata, BackendProgress, BackendRequest,
    BackendResponse, ProgressReporter, enforce_card_contract, estimate_tokens,
};

/// Keeps discovery and patch work on independent app-server processes. The
/// split lets a speculative patch run while the user continues discovery,
/// matching the phase-isolated process model used by the Claude adapter.
pub struct CodexAppBackend {
    command: String,
    args: Vec<String>,
    model: Option<String>,
    effort: Option<String>,
    discovery: Arc<Mutex<CodexAppState>>,
    patch: Arc<Mutex<CodexAppState>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Phase {
    Discovery,
    Patch,
}

struct CodexAppState {
    process: Option<CodexAppProcess>,
    next_id: u64,
    threads: HashMap<String, String>,
    context_fingerprints: HashMap<String, u64>,
}

struct CodexAppProcess {
    child: Child,
    stdin: ChildStdin,
    stdout: Lines<BufReader<ChildStdout>>,
}

struct TurnOutput {
    text: String,
    token_usage: Option<TokenUsage>,
    activities: Vec<String>,
}

#[derive(Deserialize)]
struct StructuredPatchOp {
    op: String,
    title: String,
    explanation: String,
    #[serde(default)]
    goal_complete: bool,
    patches: Vec<StructuredFilePatch>,
}

#[derive(Deserialize)]
struct StructuredFilePatch {
    id: Option<String>,
    file: std::path::PathBuf,
    explanation: String,
    hunks: Vec<StructuredHunk>,
}

#[derive(Deserialize)]
struct StructuredHunk {
    old_start: usize,
    new_start: usize,
    lines: Vec<StructuredLine>,
}

#[derive(Deserialize)]
struct StructuredLine {
    kind: StructuredLineKind,
    text: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum StructuredLineKind {
    Context,
    Remove,
    Add,
}

impl CodexAppBackend {
    pub fn from_env() -> Result<Self> {
        let command = std::env::var("PAIR_CODEX_COMMAND").unwrap_or_else(|_| "codex".into());
        let args = args_from_env("PAIR_CODEX_ARGS_JSON", "PAIR_CODEX_ARGS")?;
        let model = std::env::var("PAIR_CODEX_MODEL")
            .ok()
            .filter(|value| !value.trim().is_empty());
        let effort = std::env::var("PAIR_CODEX_EFFORT")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .or_else(|| Some("low".into()));

        Ok(Self::new(command, args, model, effort))
    }

    pub fn new(
        command: impl Into<String>,
        args: Vec<String>,
        model: Option<String>,
        effort: Option<String>,
    ) -> Self {
        Self {
            command: command.into(),
            args,
            model,
            effort,
            discovery: Arc::new(Mutex::new(CodexAppState::default())),
            patch: Arc::new(Mutex::new(CodexAppState::default())),
        }
    }

    fn lane(&self, phase: Phase) -> Arc<Mutex<CodexAppState>> {
        match phase {
            Phase::Discovery => self.discovery.clone(),
            Phase::Patch => self.patch.clone(),
        }
    }

    async fn ensure(state: &mut CodexAppState, command: &str, args: &[String]) -> Result<()> {
        if state.process.is_some() {
            return Ok(());
        }

        // Threads are ephemeral to one app-server process. Never carry their
        // IDs or context cache into a replacement process after a crash.
        state.clear_conversation();

        debug("starting codex app-server");
        let mut child = Command::new(command)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow!("codex app-server stdin unavailable"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("codex app-server stdout unavailable"))?;

        state.process = Some(CodexAppProcess {
            child,
            stdin,
            stdout: BufReader::new(stdout).lines(),
        });

        if let Err(error) = state
            .request(json!({
                "method": "initialize",
                "params": {
                    "clientInfo": {
                        "name": "pairagen",
                        "version": env!("CARGO_PKG_VERSION")
                    },
                    "capabilities": {
                        "experimentalApi": true
                    }
                }
            }))
            .await
        {
            state.invalidate_process();
            return Err(error);
        }
        debug("codex app-server initialized");

        Ok(())
    }

    async fn thread_id(
        state: &mut CodexAppState,
        req: &BackendRequest,
        model: &Option<String>,
    ) -> Result<String> {
        let patch_turn = turn_phase(req) == Phase::Patch;
        let goal_loop = req.card_contract.allow_goal_completion;
        let thread_key = thread_key(req);

        if let Some(thread_id) = state.threads.get(&thread_key) {
            return Ok(thread_id.clone());
        }

        let base_instructions = if goal_loop {
            "You are a local Pairagen coding agent executing one persistent goal. You may use targeted read-only project tools to inspect the repository and choose the next edit. Never edit files yourself. Return exactly one final JSON object matching the supplied output schema and no prose."
        } else if patch_turn {
            "You are a local Pairagen pair-programming partner. Do not use tools, commands, file reads, or repo inspection. Never edit files. Return exactly one final JSON object matching the supplied output schema and no prose."
        } else {
            "You are a local Pairagen pair-programming partner. You may use at most two targeted read-only project tool calls to find the next relevant code block. Stop searching once the supplied context supports an exact location. Never edit files. Return exactly one final JSON object matching the supplied output schema and no prose."
        };
        let developer_instructions = if goal_loop {
            "Drive the original goal from start to finish. In one work turn inspect every required file and return the complete multi-file patch batch; Pairagen reviews its hunks locally. When the user asks why, explain the pending hunk without advancing or replacing it. Preserve progress across turns and do not repeat accepted work."
        } else if patch_turn {
            "Work as an equal pair-programming partner. Propose one coherent local block at the supplied location and explain why this is the useful next move. Do not take over the whole task. Return one structured patch hunk as an editable draft, not a finished agenda."
        } else {
            "Work as an equal pair-programming partner. Inspect only enough code to identify one coherent next move. Explain what you noticed, why it matters, and how the code reveals it. Do not dictate line-by-line work or plan the whole task. Return one exact location so the keyboard can pass back to the user."
        };

        debug("starting codex thread");
        let response = state
            .request(json!({
                "method": "thread/start",
                "params": {
                    "cwd": req.context.cwd,
                    "sandbox": "read-only",
                    "approvalPolicy": "never",
                    "ephemeral": true,
                    "model": model,
                    "baseInstructions": base_instructions,
                    "developerInstructions": developer_instructions
                }
            }))
            .await?;

        let thread_id = response
            .get("thread")
            .and_then(|thread| thread.get("id"))
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("codex app-server thread/start returned no thread id"))?
            .to_string();

        state.threads.insert(thread_key, thread_id.clone());
        debug("codex thread started");

        Ok(thread_id)
    }

    async fn ask(
        &self,
        req: &BackendRequest,
        progress: Option<&ProgressReporter>,
    ) -> Result<TurnOutput> {
        report_progress(progress, &req.session.id, "starting", "Starting Codex");
        let lane = self.lane(turn_phase(req));
        let mut state = lane.lock().await;

        let first = self.ask_once(&mut state, req, progress).await;
        let Err(first_error) = first else {
            return first;
        };
        if state.process.is_some() {
            return Err(first_error);
        }

        // Transport failure invalidates the whole lane. Retry once on a fresh
        // app-server; invalidation cleared the old thread IDs and fingerprints,
        // so this attempt necessarily sends the complete source context.
        report_progress(
            progress,
            &req.session.id,
            "restarting",
            "Restarting the Codex session",
        );
        self.ask_once(&mut state, req, progress)
            .await
            .map_err(|retry| anyhow!("codex connection failed: {first_error}; retry: {retry}"))
    }

    async fn ask_once(
        &self,
        state: &mut CodexAppState,
        req: &BackendRequest,
        progress: Option<&ProgressReporter>,
    ) -> Result<TurnOutput> {
        Self::ensure(state, &self.command, &self.args).await?;

        let thread_id = Self::thread_id(state, req, &self.model).await?;
        let fingerprint = context_fingerprint(req);
        let include_context = state.context_fingerprints.get(&thread_id) != Some(&fingerprint);
        state
            .context_fingerprints
            .insert(thread_id.clone(), fingerprint);
        let input = prompt(req, include_context);
        report_progress(
            progress,
            &req.session.id,
            "requesting",
            "Sending the request to Codex",
        );
        debug("starting codex turn");
        let response = state
            .request(json!({
                "method": "turn/start",
                "params": {
                    "threadId": thread_id,
                    "cwd": req.context.cwd,
                    "approvalPolicy": "never",
                    "sandboxPolicy": {
                        "type": "readOnly",
                        "networkAccess": false
                    },
                    "input": [{
                        "type": "text",
                        "text": input,
                        "text_elements": []
                    }],
                    "model": self.model,
                    "effort": self.effort,
                    "outputSchema": output_schema(req)
                }
            }))
            .await?;

        let turn_id = response
            .get("turn")
            .and_then(|turn| turn.get("id"))
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("codex app-server turn/start returned no turn id"))?;
        let turn_id = turn_id.to_string();
        debug("codex turn started");

        report_progress(
            progress,
            &req.session.id,
            "working",
            "Codex is processing the request",
        );
        state.read_turn(&turn_id, &req.session.id, progress).await
    }

    async fn warm_up(&self) -> Result<()> {
        let lane = self.lane(Phase::Discovery);
        let mut state = lane.lock().await;

        Self::ensure(&mut state, &self.command, &self.args).await
    }

    fn error_card(message: impl Into<String>) -> Card {
        Card::Error(ErrorCard {
            id: "c_codex_app_error".into(),
            title: "Codex app-server error".into(),
            message: message.into(),
            actions: vec![Action::Retry, Action::EditPrompt, Action::Stop],
        })
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

fn thread_key(req: &BackendRequest) -> String {
    if req.card_contract.allow_goal_completion {
        return format!("{}:goal", req.session.id);
    }
    let patch_turn = turn_phase(req) == Phase::Patch;
    format!(
        "{}:{}:{}",
        req.session.id,
        if patch_turn { "patch" } else { "discover" },
        if patch_turn {
            req.session.completed_steps.len()
        } else {
            0
        }
    )
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
    fn clear_conversation(&mut self) {
        self.threads.clear();
        self.context_fingerprints.clear();
    }

    fn invalidate_process(&mut self) {
        self.process = None;
        self.clear_conversation();
    }

    fn next_request_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    async fn request(&mut self, mut request: Value) -> Result<Value> {
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

    async fn read_turn(
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
                json!({"id": id, "error": {"code": -32603, "message": "Pairagen does not handle this app-server request"}})
            }
            _ => return Ok(false),
        };

        debug(&format!("handled codex server request {method}"));

        let line = serde_json::to_string(&response)?;
        self.send_line(&line).await?;

        Ok(true)
    }
}

#[async_trait]
impl BackendAdapter for CodexAppBackend {
    async fn next_card(&self, req: BackendRequest) -> Result<BackendResponse> {
        self.next_card_with_progress(req, None).await
    }

    async fn next_card_with_progress(
        &self,
        req: BackendRequest,
        progress: Option<ProgressReporter>,
    ) -> Result<BackendResponse> {
        let output = self.ask(&req, progress.as_ref()).await?;
        let card = parse_card(&output.text, &req.card_contract)
            .unwrap_or_else(|error| Self::error_card(format!("{}\n\n{}", error, output.text)));
        let card = enforce_card_contract(card, &req.card_contract, "Codex", &output.text);

        Ok(BackendResponse {
            card,
            raw_output: Some(output.text.clone()),
            metadata: BackendMetadata {
                backend: "codex_app".into(),
                model: self.model.clone(),
                token_usage: output.token_usage.or_else(|| {
                    Some(TokenUsage::estimated(
                        estimate_tokens(&prompt(&req, true)),
                        estimate_tokens(&output.text),
                    ))
                }),
                activities: output.activities,
                attempts: vec![],
            },
        })
    }

    async fn warmup(&self) -> Result<()> {
        self.warm_up().await
    }

    fn capabilities(&self) -> BackendInfo {
        BackendInfo {
            name: "codex_app".into(),
            streaming: true,
            patches: true,
            reasoning: true,
            can_read_project: true,
            can_use_tools: true,
        }
    }
}

fn prompt(req: &BackendRequest, include_context: bool) -> String {
    let patch_turn = turn_phase(req) == Phase::Patch;
    let goal_loop = req.card_contract.allow_goal_completion;
    let goal_question =
        goal_loop && req.card_contract.expected_kind == Some(pair_protocol::CardKind::Finding);
    let turn_rules = if goal_question {
        "- Explain why the currently pending patch is the right next step for the original goal.\n\
         - Address its behavior, tradeoffs, and relevant evidence from the code.\n\
         - Return one concise finding. Do not draft, replace, advance, or complete the goal.\n\
         - The exact pending patch remains awaiting user acceptance after this answer."
            .into()
    } else if goal_loop {
        format!(
            "- Continue executing the original session goal from the accepted progress; never restart or repeat a completed step.\n\
             - Inspect every required project file with targeted read-only tools and prepare the complete change in this turn.\n\
             - Tool reads are valid patch source. Pairagen verifies every returned hunk against the corresponding live editor buffer before review.\n\
             - Return one structured patch batch with up to {} files, up to {} hunks per file, and at most {} added/removed lines per hunk. Include every required edit; review granularity is handled locally.\n\
             - Create missing files directly in the same batch before patches that reference them.\n\
             - Use open_location only when a required source cannot be inspected with read-only project tools.\n\
             - Set goal_complete=true when accepting the complete returned patch finishes the original goal. Set it false only when another file or independently inspected stage remains.\n\
             - Return summary only when every requirement in the original goal is satisfied; cite the completed result.\n\
             - Return choice only when a genuine user decision blocks all safe progress.\n\
             - Do not return a finding, an assessment, a plan, or instructions for the user to request another draft.",
            req.card_contract.max_patch_files,
            req.card_contract.max_hunks_per_patch,
            req.card_contract.max_changed_lines,
        )
            .into()
    } else if patch_turn {
        format!(
            "- Return exactly one file and exactly one hunk changing at most {} added/removed lines.\n\
             - Change one coherent local block in the supplied excerpt. Leave later blocks for later Pair cards.\n\
             - Explain why this draft is the useful next move, not merely what lines it changes.\n\
             - The step must be internally coherent: do not introduce undefined symbols or dangling references.\n\
             - The code must remain type-correct after this hunk. Never change a field type while deferring its producer/initializer to a later card.\n\
             - If a safe step needs unseen references or more changed lines, limit this hunk to self-contained preparation such as adding only the new struct definition.\n\
             - Context and remove lines must be exact, contiguous source lines from the supplied buffer; never omit source lines between two context lines.\n\
             - Use only the supplied buffer excerpt. Do not inspect the project or use tools.",
            req.card_contract.max_changed_lines
        )
    } else {
        "- Find only one useful next move, not a plan for the whole solution.\n\
         - Inspect the supplied ranked project context first. Use targeted project search only when those fragments are insufficient.\n\
         - Do not stop just because the initial excerpt is indirect or missing.\n\
         - When the user names a destination or consumer such as a template, API, caller, or renderer, prefer that consumer block as the next location before changing its producer.\n\
         - Explain what you noticed, why it matters now, and how the code led you there. Do not dictate keystrokes or a line-by-line walkthrough.\n\
         - Return a concrete evidence/next/location pointing to that block so the editor can move there before Fix.\n\
         - Do not propose code changes yet; hand the keyboard back after identifying the next move."
            .into()
    };

    let output_contract = if goal_question {
        "- finding: concise explanation of the pending hunk"
    } else if goal_loop {
        "- patch: one complete structured patch for local hunk-by-hunk review; include goal_complete\n\
- open_location: when the next hunk belongs in another buffer; put the target in location (not next) and the explanation in reason (not message)\n\
- choice: only for a blocking user decision\n\
- summary: only when the complete original goal is satisfied"
    } else {
        "- hypothesis: {\"op\":\"hypothesis\",\"title\":string,\"claim\":string,\"evidence\":object|null,\"next\":object|null}\n\
- finding: {\"op\":\"finding\",\"title\":string,\"finding\":string,\"location\":object|null,\"annotation\":string|null}\n\
- patch: use the exact structured patch schema supplied by the API. Each hunk has old_start, new_start, and lines with kind context/remove/add plus line text without a diff prefix.\n\
- error: {\"op\":\"error\",\"title\":string,\"message\":string}"
    };

    let ranked_context = if (patch_turn && !goal_loop) || req.context.artifacts.is_empty() {
        "none".into()
    } else {
        req.context
            .artifacts
            .iter()
            .map(|artifact| {
                format!(
                    "--- {}:{}-{} ({:?}; {}) ---\n{}",
                    artifact.file.display(),
                    artifact.start_line,
                    artifact.end_line,
                    artifact.kind,
                    artifact.reason,
                    artifact.text
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    };

    let source_context = if include_context {
        format!(
            "File: {}\nCursor: {}:{}\nBuffer starts at file line: {}\nBuffer excerpt:\n```text\n{}\n```\nRanked project context (read before using tools):\n```text\n{}\n```",
            req.context.file.display(),
            req.context.cursor.line,
            req.context.cursor.column,
            req.context.buffer_start_line,
            req.context.buffer_text,
            ranked_context,
        )
    } else {
        "Source context is unchanged from the preceding turn in this Pair thread. Reuse that exact buffer and ranked project context.".into()
    };

    format!(
        r#"Return exactly one JSON Pair op. No markdown. No prose.

Allowed ops:
{output_contract}

Rules:
- Required card kind: {expected_kind}. Return that exact kind.
- Patch file paths must be relative.
{turn_rules}

Session prompt: {prompt}
Completed local steps: {completed_steps}
Known findings and signals (do not repeat): {known_observations}
Mode: {mode}
Action: {action}
Last card: {last}
{source_context}"#,
        prompt = req.session.prompt,
        completed_steps =
            serde_json::to_string(&req.session.completed_steps).unwrap_or_else(|_| "[]".into()),
        known_observations =
            serde_json::to_string(&req.session.known_observations).unwrap_or_else(|_| "[]".into()),
        mode = serde_json::to_string(&req.session.mode).unwrap_or_else(|_| "\"auto\"".into()),
        action = action_value(&req.action),
        expected_kind = req
            .card_contract
            .expected_kind
            .map(|kind| format!("{kind:?}").to_lowercase())
            .unwrap_or_else(|| "any allowed kind".into()),
        turn_rules = turn_rules,
        output_contract = output_contract,
        last = req.session.last_summary.as_deref().unwrap_or("none"),
        source_context = source_context,
    )
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

fn parse_card(output: &str, contract: &crate::CardContract) -> Result<Card> {
    if contract.allow_goal_completion {
        let value = serde_json::from_str::<Value>(output.trim())?;
        if value.get("op").and_then(Value::as_str) == Some("patch") {
            return parse_structured_patch(output);
        }
    }
    if contract.expected_kind == Some(pair_protocol::CardKind::Patch) {
        return parse_structured_patch(output);
    }

    let op = serde_json::from_str::<AgentOp>(output.trim())?;

    Ok(op.into_card("c_agent"))
}

fn parse_structured_patch(output: &str) -> Result<Card> {
    let op = serde_json::from_str::<StructuredPatchOp>(output.trim())?;
    if op.op != "patch" {
        return Err(anyhow!("codex returned op {:?}, expected patch", op.op));
    }

    let patches = op
        .patches
        .into_iter()
        .enumerate()
        .map(|(index, patch)| {
            Ok(pair_protocol::FilePatch {
                id: patch.id.unwrap_or_else(|| format!("p_{}", index + 1)),
                file: patch.file,
                diff: render_structured_diff(&patch.hunks)?,
                explanation: patch.explanation,
            })
        })
        .collect::<Result<Vec<_>>>()?;

    Ok(Card::Patch(pair_protocol::PatchCard {
        id: "c_agent".into(),
        title: op.title,
        explanation: op.explanation,
        warnings: vec![],
        goal_complete: op.goal_complete,
        patches,
        actions: vec![
            Action::Apply,
            Action::Why,
            Action::Retry,
            Action::EditPrompt,
            Action::Stop,
        ],
    }))
}

fn render_structured_diff(hunks: &[StructuredHunk]) -> Result<String> {
    let mut diff = String::new();

    for hunk in hunks {
        let old_len = hunk
            .lines
            .iter()
            .filter(|line| !matches!(line.kind, StructuredLineKind::Add))
            .count();
        let new_len = hunk
            .lines
            .iter()
            .filter(|line| !matches!(line.kind, StructuredLineKind::Remove))
            .count();

        if hunk.old_start == 0 || hunk.new_start == 0 {
            return Err(anyhow!("structured patch line numbers must start at 1"));
        }
        if hunk.lines.is_empty() {
            return Err(anyhow!("structured patch hunk has no lines"));
        }

        diff.push_str(&format!(
            "@@ -{},{} +{},{} @@\n",
            hunk.old_start, old_len, hunk.new_start, new_len
        ));

        for line in &hunk.lines {
            if line.text.contains(['\n', '\r']) {
                return Err(anyhow!("structured patch line contains a newline"));
            }

            let prefix = match line.kind {
                StructuredLineKind::Context => ' ',
                StructuredLineKind::Remove => '-',
                StructuredLineKind::Add => '+',
            };
            diff.push(prefix);
            diff.push_str(&line.text);
            diff.push('\n');
        }
    }

    Ok(diff)
}

fn output_schema(req: &BackendRequest) -> Value {
    if req.card_contract.allow_goal_completion
        && req.card_contract.expected_kind == Some(pair_protocol::CardKind::Finding)
    {
        return finding_schema();
    }
    if req.card_contract.allow_goal_completion {
        return goal_loop_schema(&req.card_contract);
    }

    match req.card_contract.expected_kind {
        Some(pair_protocol::CardKind::Patch) => patch_schema(&req.card_contract),
        Some(pair_protocol::CardKind::Hypothesis) => hypothesis_schema(),
        Some(pair_protocol::CardKind::Finding) => finding_schema(),
        Some(pair_protocol::CardKind::Choice) => choice_schema(),
        Some(pair_protocol::CardKind::Deny) => deny_schema(),
        Some(pair_protocol::CardKind::Summary) => summary_schema(),
        Some(pair_protocol::CardKind::Error) => error_schema(),
        Some(pair_protocol::CardKind::OpenLocation) | None => any_op_schema(),
    }
}

/// Schema for turns without a demanded kind: the agent picks whichever op
/// fits, including a clarifying choice or a deny. Mirrors
/// schemas/pair-agent-op.schema.json (every field present, unused ones null).
fn any_op_schema() -> Value {
    object_schema(
        &[
            "op",
            "title",
            "claim",
            "evidence",
            "next",
            "finding",
            "location",
            "annotation",
            "explanation",
            "goal_complete",
            "patches",
            "question",
            "options",
            "reason",
            "summary",
            "changed_files",
            "message",
        ],
        json!({
            "op": {"type": "string", "enum": ["hypothesis", "finding", "patch", "choice", "deny", "open_location", "summary", "error"]},
            "title": {"type": "string"},
            "claim": {"type": ["string", "null"]},
            "evidence": nullable_location_schema(),
            "next": nullable_location_schema(),
            "finding": {"type": ["string", "null"]},
            "location": nullable_location_schema(),
            "annotation": {"type": ["string", "null"]},
            "explanation": {"type": ["string", "null"]},
            "goal_complete": {"type": ["boolean", "null"]},
            "patches": {
                "type": ["array", "null"],
                "items": object_schema(
                    &["id", "file", "diff", "explanation"],
                    json!({
                        "id": {"type": ["string", "null"]},
                        "file": {"type": "string"},
                        "diff": {"type": "string"},
                        "explanation": {"type": "string"}
                    })
                )
            },
            "question": {"type": ["string", "null"]},
            "options": {
                "type": ["array", "null"],
                "items": object_schema(
                    &["id", "label", "action"],
                    json!({
                        "id": {"type": "string"},
                        "label": {"type": "string"},
                        "action": {
                            "type": "string",
                            "enum": ["follow", "why", "fix", "other_lead", "retry", "edit_prompt", "open", "run_check", "next", "stop"]
                        }
                    })
                )
            },
            "reason": {"type": ["string", "null"]},
            "summary": {"type": ["string", "null"]},
            "changed_files": {"type": ["array", "null"], "items": {"type": "string"}},
            "message": {"type": ["string", "null"]}
        }),
    )
}

fn goal_loop_schema(contract: &crate::CardContract) -> Value {
    let mut schema = any_op_schema();
    schema["properties"]["op"]["enum"] = json!([
        "patch",
        "choice",
        "deny",
        "open_location",
        "summary",
        "error"
    ]);
    let mut patches = patch_schema(contract)["properties"]["patches"].clone();
    patches["type"] = json!(["array", "null"]);
    schema["properties"]["patches"] = patches;
    schema
}

fn object_schema(required: &[&str], properties: Value) -> Value {
    json!({
        "type": "object",
        "required": required,
        "properties": properties,
        "additionalProperties": false
    })
}

fn nullable_location_schema() -> Value {
    json!({
        "anyOf": [
            object_schema(
                &["file", "line", "column", "annotation"],
                json!({
                    "file": {"type": "string"},
                    "line": {"type": "integer"},
                    "column": {"type": "integer"},
                    "annotation": {"type": ["string", "null"]}
                })
            ),
            {"type": "null"}
        ]
    })
}

fn location_schema() -> Value {
    object_schema(
        &["file", "line", "column", "annotation"],
        json!({
            "file": {"type": "string"},
            "line": {"type": "integer", "minimum": 1},
            "column": {"type": "integer", "minimum": 1},
            "annotation": {"type": ["string", "null"]}
        }),
    )
}

fn hypothesis_schema() -> Value {
    object_schema(
        &["op", "title", "claim", "evidence", "next"],
        json!({
            "op": {"type": "string", "enum": ["hypothesis"]},
            "title": {"type": "string"},
            "claim": {"type": "string"},
            "evidence": nullable_location_schema(),
            "next": location_schema()
        }),
    )
}

fn finding_schema() -> Value {
    object_schema(
        &["op", "title", "finding", "location", "annotation"],
        json!({
            "op": {"type": "string", "enum": ["finding"]},
            "title": {"type": "string"},
            "finding": {"type": "string"},
            "location": location_schema(),
            "annotation": {"type": ["string", "null"]}
        }),
    )
}

fn patch_schema(contract: &crate::CardContract) -> Value {
    object_schema(
        &["op", "title", "explanation", "goal_complete", "patches"],
        json!({
            "op": {"type": "string", "enum": ["patch"]},
            "title": {"type": "string"},
            "explanation": {"type": "string"},
            "goal_complete": {"type": "boolean"},
            "patches": {
                "type": "array",
                "minItems": 1,
                "maxItems": contract.max_patch_files,
                "items": object_schema(
                    &["id", "file", "explanation", "hunks"],
                    json!({
                        "id": {"type": ["string", "null"]},
                        "file": {"type": "string"},
                        "explanation": {"type": "string"},
                        "hunks": {
                            "type": "array",
                            "minItems": 1,
                            "maxItems": contract.max_hunks_per_patch,
                            "items": object_schema(
                                &["old_start", "new_start", "lines"],
                                json!({
                                    "old_start": {"type": "integer", "minimum": 1},
                                    "new_start": {"type": "integer", "minimum": 1},
                                    "lines": {
                                        "type": "array",
                                        "minItems": 1,
                                        "maxItems": contract.max_changed_lines + 8,
                                        "items": object_schema(
                                            &["kind", "text"],
                                            json!({
                                                "kind": {"type": "string", "enum": ["context", "remove", "add"]},
                                                "text": {"type": "string"}
                                            })
                                        )
                                    }
                                })
                            )
                        }
                    })
                )
            }
        }),
    )
}

fn choice_schema() -> Value {
    object_schema(
        &["op", "title", "question", "options"],
        json!({
            "op": {"type": "string", "enum": ["choice"]},
            "title": {"type": "string"},
            "question": {"type": "string"},
            "options": {
                "type": "array",
                "items": object_schema(
                    &["id", "label", "action"],
                    json!({
                        "id": {"type": "string"},
                        "label": {"type": "string"},
                        "action": {
                            "type": "string",
                            "enum": ["follow", "why", "fix", "other_lead", "retry", "edit_prompt", "open", "run_check", "next", "stop"]
                        }
                    })
                )
            }
        }),
    )
}

fn summary_schema() -> Value {
    object_schema(
        &["op", "title", "summary", "changed_files"],
        json!({
            "op": {"type": "string", "enum": ["summary"]},
            "title": {"type": "string"},
            "summary": {"type": "string"},
            "changed_files": {"type": "array", "items": {"type": "string"}}
        }),
    )
}

fn deny_schema() -> Value {
    object_schema(
        &["op", "title", "reason", "location"],
        json!({
            "op": {"type": "string", "enum": ["deny"]},
            "title": {"type": "string"},
            "reason": {"type": "string"},
            "location": nullable_location_schema()
        }),
    )
}

fn error_schema() -> Value {
    object_schema(
        &["op", "title", "message"],
        json!({
            "op": {"type": "string", "enum": ["error"]},
            "title": {"type": "string"},
            "message": {"type": "string"}
        }),
    )
}

fn parse_usage(value: Option<&Value>) -> Option<TokenUsage> {
    let last = value?.get("last")?;
    let input = last.get("inputTokens")?.as_u64()? as usize;
    let output = last.get("outputTokens")?.as_u64()? as usize;
    let total = last.get("totalTokens")?.as_u64()? as usize;

    Some(TokenUsage {
        input_tokens: input,
        output_tokens: output,
        total_tokens: total,
        estimated: false,
    })
}

fn args_from_env(json_name: &str, plain_name: &str) -> Result<Vec<String>> {
    if let Ok(value) = std::env::var(json_name)
        && !value.trim().is_empty()
    {
        return Ok(serde_json::from_str(&value)?);
    }

    Ok(std::env::var(plain_name)
        .unwrap_or_else(|_| "app-server --stdio".into())
        .split_whitespace()
        .map(str::to_string)
        .collect())
}

fn debug(message: &str) {
    if std::env::var("PAIR_DEBUG").is_ok() {
        eprintln!("pair codex_app: {message}");
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

    fn request() -> BackendRequest {
        BackendRequest {
            session: crate::SessionSnapshot {
                id: "s_1".into(),
                prompt: "inspect target".into(),
                completed_steps: vec![],
                known_observations: vec![],
                mode: pair_protocol::Mode::Auto,
                card_count: 0,
                last_card: None,
                last_summary: None,
            },
            action: BackendAction::Start,
            context: pair_protocol::ContextBundle {
                cwd: "/tmp/project".into(),
                file: "src/main.rs".into(),
                cursor: pair_protocol::Cursor { line: 1, column: 1 },
                selection: None,
                buffer_text: "unique source payload".into(),
                buffer_start_line: 1,
                diagnostics: vec![],
                hints: vec![],
                artifacts: vec![],
                report: None,
            },
            card_contract: crate::CardContract {
                expected_kind: Some(pair_protocol::CardKind::Hypothesis),
                ..Default::default()
            },
        }
    }

    #[test]
    fn routes_discovery_and_patch_turns_to_separate_phases() {
        let mut request = request();
        assert_eq!(turn_phase(&request), Phase::Discovery);

        request.card_contract.expected_kind = Some(pair_protocol::CardKind::Patch);
        assert_eq!(turn_phase(&request), Phase::Patch);

        request.card_contract.expected_kind = None;
        request.card_contract.allow_goal_completion = true;
        assert_eq!(turn_phase(&request), Phase::Patch);
        assert_eq!(thread_key(&request), "s_1:goal");
    }

    #[tokio::test]
    async fn discovery_and_patch_lanes_do_not_share_a_turn_lock() {
        let backend = CodexAppBackend::new("unused", vec![], None, None);
        let discovery = backend.lane(Phase::Discovery);
        let patch = backend.lane(Phase::Patch);

        assert!(!Arc::ptr_eq(&discovery, &patch));
        let _discovery_guard = discovery.lock().await;
        let patch_guard =
            tokio::time::timeout(std::time::Duration::from_millis(50), patch.lock()).await;

        assert!(patch_guard.is_ok(), "patch lane waited on discovery lane");
    }

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
    fn serializes_user_action_as_protocol_value() {
        let value = action_value(&BackendAction::User(Action::Fix));

        assert_eq!(value["action"], "fix");
    }

    #[test]
    fn unchanged_context_is_not_repeated_in_thread_prompt() {
        let request = request();

        assert!(prompt(&request, true).contains("unique source payload"));
        let repeated = prompt(&request, false);
        assert!(!repeated.contains("unique source payload"));
        assert!(repeated.contains("Source context is unchanged"));
    }

    #[test]
    fn accepted_patch_step_rotates_patch_thread() {
        let mut request = request();
        request.card_contract.expected_kind = Some(pair_protocol::CardKind::Patch);
        let first = thread_key(&request);
        request.session.completed_steps.push("first patch".into());

        assert_ne!(first, thread_key(&request));
        assert_eq!(thread_key(&request), "s_1:patch:1");
    }

    #[test]
    fn retry_within_the_same_step_reuses_patch_thread() {
        let mut request = request();
        request.card_contract.expected_kind = Some(pair_protocol::CardKind::Patch);
        let first = thread_key(&request);
        request.action = BackendAction::ContractRetry("repair it".into());

        assert_eq!(first, thread_key(&request));
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
    fn renders_typed_patch_hunks_as_unified_diff() {
        let output = json!({
            "op": "patch",
            "title": "Rename value",
            "explanation": "Use the new name.",
            "patches": [{
                "id": null,
                "file": "src/main.rs",
                "explanation": "Rename one line.",
                "hunks": [{
                    "old_start": 4,
                    "new_start": 4,
                    "lines": [
                        {"kind": "context", "text": "fn main() {"},
                        {"kind": "remove", "text": "    let old = 1;"},
                        {"kind": "add", "text": "    let new = 1;"},
                        {"kind": "context", "text": "}"}
                    ]
                }]
            }]
        });

        let Card::Patch(card) = parse_structured_patch(&output.to_string()).unwrap() else {
            panic!("expected patch card");
        };

        assert_eq!(
            card.patches[0].diff,
            "@@ -4,3 +4,3 @@\n fn main() {\n-    let old = 1;\n+    let new = 1;\n }\n"
        );
    }

    #[test]
    fn goal_loop_dispatches_structured_patch_output() {
        let output = json!({
            "op": "patch",
            "title": "Continue the goal",
            "explanation": "Apply the next accepted requirement.",
            "goal_complete": true,
            "patches": [{
                "id": null,
                "file": "src/main.rs",
                "explanation": "Update the next local block.",
                "hunks": [{
                    "old_start": 1,
                    "new_start": 1,
                    "lines": [
                        {"kind": "remove", "text": "old"},
                        {"kind": "add", "text": "new"}
                    ]
                }]
            }],
            "claim": null,
            "evidence": null,
            "next": null,
            "finding": null,
            "location": null,
            "annotation": null,
            "question": null,
            "options": null,
            "reason": null,
            "summary": null,
            "changed_files": null,
            "message": null
        });
        let contract = crate::CardContract {
            allow_goal_completion: true,
            ..Default::default()
        };

        let Card::Patch(card) = parse_card(&output.to_string(), &contract).unwrap() else {
            panic!("expected patch card");
        };
        assert!(card.goal_complete);
    }

    #[test]
    fn goal_loop_accepts_open_location_target_in_next() {
        let output = json!({
            "op": "open_location",
            "title": "Open inactive-account exception",
            "reason": "Create the exception before referencing it.",
            "location": null,
            "next": {
                "file": "src/Exception/OAuth/OAuthAccountNotActiveException.php",
                "line": 1,
                "column": 1,
                "annotation": "New exception type."
            }
        });
        let contract = crate::CardContract {
            allow_goal_completion: true,
            ..Default::default()
        };

        let Card::OpenLocation(card) = parse_card(&output.to_string(), &contract).unwrap() else {
            panic!("expected open_location card");
        };
        assert!(
            card.location
                .file
                .ends_with("OAuthAccountNotActiveException.php")
        );
    }

    #[test]
    fn strict_parser_rejects_prose_around_json() {
        let output = r#"Here is the result: {"op":"finding","title":"T","finding":"F","location":null,"annotation":null}"#;
        let contract = crate::CardContract {
            expected_kind: Some(pair_protocol::CardKind::Finding),
            ..crate::CardContract::default()
        };

        assert!(parse_card(output, &contract).is_err());
    }

    #[test]
    fn patch_schema_exposes_hunks_instead_of_raw_diff() {
        let schema = patch_schema(&crate::CardContract::default());
        let patch = &schema["properties"]["patches"]["items"];

        assert!(patch["properties"].get("diff").is_none());
        assert_eq!(patch["properties"]["hunks"]["type"], "array");
        assert_eq!(schema["properties"]["patches"]["maxItems"], 1);
        assert_eq!(patch["properties"]["hunks"]["maxItems"], 1);
    }

    #[test]
    fn goal_loop_schema_allows_structured_patch_or_summary() {
        let contract = crate::CardContract {
            max_patch_files: pair_protocol::MAX_GOAL_PATCH_FILES,
            max_hunks_per_patch: pair_protocol::MAX_GOAL_HUNKS_PER_PATCH,
            max_changed_lines: pair_protocol::MAX_GOAL_CHANGED_LINES,
            ..Default::default()
        };
        let schema = goal_loop_schema(&contract);
        let ops = schema["properties"]["op"]["enum"].as_array().unwrap();

        assert!(ops.contains(&json!("patch")));
        assert!(ops.contains(&json!("summary")));
        assert!(!ops.contains(&json!("finding")));
        assert_eq!(
            schema["properties"]["patches"]["maxItems"],
            pair_protocol::MAX_GOAL_PATCH_FILES
        );
        assert!(schema["properties"]["patches"]["items"]["properties"]["hunks"].is_object());
        assert_eq!(
            schema["properties"]["patches"]["items"]["properties"]["hunks"]["maxItems"],
            pair_protocol::MAX_GOAL_HUNKS_PER_PATCH
        );
        assert!(schema["properties"]["goal_complete"].is_object());
    }

    #[test]
    fn why_uses_finding_schema_inside_the_goal_thread() {
        let mut request = request();
        request.card_contract.expected_kind = Some(pair_protocol::CardKind::Finding);
        request.card_contract.allow_goal_completion = true;
        request.action = BackendAction::User(Action::Why);
        let schema = output_schema(&request);

        assert_eq!(thread_key(&request), "s_1:goal");
        assert_eq!(schema["properties"]["op"]["enum"][0], "finding");
        assert!(prompt(&request, true).contains("pending patch remains"));
    }

    #[test]
    fn goal_prompt_requests_one_verified_multi_file_batch() {
        let mut request = request();
        request.card_contract.allow_goal_completion = true;
        request.card_contract.expected_kind = None;
        let prompt = prompt(&request, true);

        assert!(prompt.contains("Tool reads are valid patch source"));
        assert!(prompt.contains("complete change in this turn"));
        assert!(prompt.contains("Create missing files directly in the same batch"));
    }

    #[test]
    fn discovery_schema_requires_a_concrete_next_location() {
        let schema = hypothesis_schema();

        assert_eq!(schema["properties"]["next"]["type"], "object");
        assert_eq!(
            schema["properties"]["next"]["properties"]["line"]["minimum"],
            1
        );
    }

    #[test]
    fn parses_usage_from_app_server_notification() {
        let value = json!({
            "last": {
                "inputTokens": 10,
                "outputTokens": 5,
                "totalTokens": 15
            }
        });
        let usage = parse_usage(Some(&value)).unwrap();

        assert_eq!(usage.input_tokens, 10);
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
