use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::process::Stdio;

use anyhow::{Result, anyhow};
use async_trait::async_trait;
use pair_protocol::{Action, AgentOp, BackendInfo, Card, ErrorCard, SummaryCard, TokenUsage};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::Mutex;

use crate::{
    BackendAction, BackendAdapter, BackendMetadata, BackendProgress, BackendRequest,
    BackendResponse, ProgressReporter, enforce_card_contract, estimate_tokens,
};

pub struct CodexAppBackend {
    command: String,
    args: Vec<String>,
    model: Option<String>,
    effort: Option<String>,
    state: Mutex<CodexAppState>,
}

#[derive(Default)]
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
    patches: Vec<StructuredFilePatch>,
}

#[derive(Deserialize)]
struct StructuredGoalStep {
    op: String,
    status: GoalStepStatus,
    title: String,
    explanation: String,
    patches: Vec<StructuredFilePatch>,
    summary: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum GoalStepStatus {
    Continue,
    Complete,
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
            state: Mutex::new(CodexAppState {
                process: None,
                next_id: 1,
                threads: HashMap::new(),
                context_fingerprints: HashMap::new(),
            }),
        }
    }

    async fn ensure(state: &mut CodexAppState, command: &str, args: &[String]) -> Result<()> {
        if state.process.is_some() {
            return Ok(());
        }

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

        let _ = state
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
            .await?;
        debug("codex app-server initialized");

        Ok(())
    }

    async fn thread_id(
        state: &mut CodexAppState,
        req: &BackendRequest,
        model: &Option<String>,
    ) -> Result<String> {
        let patch_turn = req.card_contract.expected_kind == Some(pair_protocol::CardKind::Patch);
        let thread_key = thread_key(req);

        if let Some(thread_id) = state.threads.get(&thread_key) {
            return Ok(thread_id.clone());
        }

        let base_instructions = if patch_turn {
            "You are a local Pairagen pair-programming partner. Do not use tools, commands, file reads, or repo inspection. Never edit files. Return exactly one final JSON object matching the supplied output schema and no prose."
        } else {
            "You are a local Pairagen pair-programming partner. You may use at most two targeted read-only project tool calls to find the next relevant code block. Stop searching once the supplied context supports an exact location. Never edit files. Return exactly one final JSON object matching the supplied output schema and no prose."
        };
        let developer_instructions = if patch_turn {
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
        let mut state = self.state.lock().await;

        Self::ensure(&mut state, &self.command, &self.args).await?;

        let thread_id = Self::thread_id(&mut state, req, &self.model).await?;
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

    fn error_card(message: impl Into<String>) -> Card {
        Card::Error(ErrorCard {
            id: "c_codex_app_error".into(),
            title: "Codex app-server error".into(),
            message: message.into(),
            actions: vec![Action::Retry, Action::EditPrompt, Action::Stop],
        })
    }
}

fn thread_key(req: &BackendRequest) -> String {
    let patch_turn = req.card_contract.expected_kind == Some(pair_protocol::CardKind::Patch);
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

impl CodexAppState {
    fn next_request_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    async fn request(&mut self, mut request: Value) -> Result<Value> {
        let id = self.next_request_id();
        request["id"] = json!(id);

        let process = self
            .process
            .as_mut()
            .ok_or_else(|| anyhow!("codex app-server process unavailable"))?;
        let line = serde_json::to_string(&request)?;

        process.stdin.write_all(line.as_bytes()).await?;
        process.stdin.write_all(b"\n").await?;
        process.stdin.flush().await?;

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
        let process = self
            .process
            .as_mut()
            .ok_or_else(|| anyhow!("codex app-server process unavailable"))?;

        loop {
            let Some(line) = process.stdout.next_line().await? else {
                self.process = None;

                return Err(anyhow!("codex app-server closed stdout"));
            };

            if line.trim().is_empty() {
                continue;
            }

            return Ok(serde_json::from_str(&line)?);
        }
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

        let process = self
            .process
            .as_mut()
            .ok_or_else(|| anyhow!("codex app-server process unavailable"))?;
        let line = serde_json::to_string(&response)?;

        process.stdin.write_all(line.as_bytes()).await?;
        process.stdin.write_all(b"\n").await?;
        process.stdin.flush().await?;

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
    let patch_turn = req.card_contract.expected_kind == Some(pair_protocol::CardKind::Patch);
    let continuation = req.card_contract.allow_goal_completion;
    let turn_rules = if continuation {
        format!(
            "- Continue the original session goal; do not rediscover it or repeat any completed local step.\n\
             - If the goal is unresolved, return status continue with exactly one file and one hunk changing at most {} added/removed lines.\n\
             - If the goal is fully resolved, return status complete with no patches and a concise summary.\n\
             - A continue step must be internally coherent and must not introduce undefined symbols or leave stale references in the supplied block.\n\
             - Use only the supplied buffer excerpt. Do not inspect the project or use tools.",
            req.card_contract.max_changed_lines
        )
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

    let output_contract = if continuation {
        "- goal_step: {\"op\":\"goal_step\",\"status\":\"continue\"|\"complete\",\"title\":string,\"explanation\":string,\"patches\":array,\"summary\":string}"
    } else {
        "- hypothesis: {\"op\":\"hypothesis\",\"title\":string,\"claim\":string,\"evidence\":object|null,\"next\":object|null}\n\
- finding: {\"op\":\"finding\",\"title\":string,\"finding\":string,\"location\":object|null,\"annotation\":string|null}\n\
- patch: use the exact structured patch schema supplied by the API. Each hunk has old_start, new_start, and lines with kind context/remove/add plus line text without a diff prefix.\n\
- error: {\"op\":\"error\",\"title\":string,\"message\":string}"
    };

    let ranked_context = if patch_turn || continuation || req.context.artifacts.is_empty() {
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
    }
}

fn parse_card(output: &str, contract: &crate::CardContract) -> Result<Card> {
    if contract.allow_goal_completion {
        return parse_goal_step(output);
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
        patches,
        actions: vec![
            Action::Apply,
            Action::Retry,
            Action::EditPrompt,
            Action::Stop,
        ],
    }))
}

fn parse_goal_step(output: &str) -> Result<Card> {
    let step = serde_json::from_str::<StructuredGoalStep>(output.trim())?;
    if step.op != "goal_step" {
        return Err(anyhow!(
            "codex returned op {:?}, expected goal_step",
            step.op
        ));
    }

    match step.status {
        GoalStepStatus::Continue => {
            if step.patches.len() != 1 {
                return Err(anyhow!(
                    "continuing goal step must contain exactly one patch"
                ));
            }
            let patches = structured_file_patches(step.patches)?;

            Ok(Card::Patch(pair_protocol::PatchCard {
                id: "c_agent".into(),
                title: step.title,
                explanation: step.explanation,
                warnings: vec![],
                patches,
                actions: vec![
                    Action::Apply,
                    Action::Retry,
                    Action::EditPrompt,
                    Action::Stop,
                ],
            }))
        }
        GoalStepStatus::Complete => {
            if !step.patches.is_empty() {
                return Err(anyhow!("completed goal step cannot contain patches"));
            }
            if step.summary.trim().is_empty() {
                return Err(anyhow!("completed goal step has an empty summary"));
            }

            Ok(Card::Summary(SummaryCard {
                id: "c_agent".into(),
                title: step.title,
                summary: step.summary,
                changed_files: vec![],
                next_actions: vec![Action::RunCheck, Action::Stop],
            }))
        }
    }
}

fn structured_file_patches(
    patches: Vec<StructuredFilePatch>,
) -> Result<Vec<pair_protocol::FilePatch>> {
    patches
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
        .collect()
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
    if req.card_contract.allow_goal_completion {
        return goal_step_schema(&req.card_contract);
    }

    match req.card_contract.expected_kind {
        Some(pair_protocol::CardKind::Patch) => patch_schema(&req.card_contract),
        Some(pair_protocol::CardKind::Hypothesis) => hypothesis_schema(),
        Some(pair_protocol::CardKind::Finding) => finding_schema(),
        Some(pair_protocol::CardKind::Choice) => choice_schema(),
        Some(pair_protocol::CardKind::Deny) => deny_schema(),
        Some(pair_protocol::CardKind::Summary) => summary_schema(),
        Some(pair_protocol::CardKind::Error) => error_schema(),
        None => error_schema(),
    }
}

fn goal_step_schema(contract: &crate::CardContract) -> Value {
    let mut patches = patch_schema(contract)["properties"]["patches"].clone();
    patches["minItems"] = json!(0);

    object_schema(
        &["op", "status", "title", "explanation", "patches", "summary"],
        json!({
            "op": {"type": "string", "enum": ["goal_step"]},
            "status": {"type": "string", "enum": ["continue", "complete"]},
            "title": {"type": "string"},
            "explanation": {"type": "string"},
            "patches": patches,
            "summary": {"type": "string"}
        }),
    )
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
        &["op", "title", "explanation", "patches"],
        json!({
            "op": {"type": "string", "enum": ["patch"]},
            "title": {"type": "string"},
            "explanation": {"type": "string"},
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
        &["op", "title", "reason"],
        json!({
            "op": {"type": "string", "enum": ["deny"]},
            "title": {"type": "string"},
            "reason": {"type": "string"}
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
    fn parses_continuing_goal_step_as_patch() {
        let output = json!({
            "op": "goal_step",
            "status": "continue",
            "title": "Update the consumer",
            "explanation": "The producer is ready; update one consumer now.",
            "patches": [{
                "id": null,
                "file": "templates/layout.html",
                "explanation": "Render the concrete preview.",
                "hunks": [{
                    "old_start": 10,
                    "new_start": 10,
                    "lines": [
                        {"kind": "remove", "text": "{{ block.preview_html|safe }}"},
                        {"kind": "add", "text": "{{ block.preview }}"}
                    ]
                }]
            }],
            "summary": ""
        });

        assert!(matches!(
            parse_goal_step(&output.to_string()).unwrap(),
            Card::Patch(_)
        ));
    }

    #[test]
    fn parses_completed_goal_step_as_summary() {
        let output = json!({
            "op": "goal_step",
            "status": "complete",
            "title": "Goal complete",
            "explanation": "No further local changes are needed.",
            "patches": [],
            "summary": "The concrete preview now flows into the template."
        });

        let Card::Summary(card) = parse_goal_step(&output.to_string()).unwrap() else {
            panic!("expected summary card");
        };
        assert!(!card.next_actions.contains(&Action::Next));
    }

    #[test]
    fn goal_step_schema_has_one_concrete_root_shape() {
        let schema = goal_step_schema(&crate::CardContract::default());

        assert_eq!(schema["type"], "object");
        assert_eq!(schema["properties"]["op"]["enum"][0], "goal_step");
        assert_eq!(schema["properties"]["patches"]["minItems"], 0);
        assert_eq!(schema["properties"]["patches"]["maxItems"], 1);
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
