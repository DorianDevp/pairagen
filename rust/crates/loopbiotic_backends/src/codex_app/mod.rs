mod parse;
mod schema;
mod transport;

use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Result, anyhow};
use async_trait::async_trait;
use loopbiotic_protocol::{BackendInfo, Card, TokenUsage};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::Mutex;

use crate::support::{
    Phase, TurnTimedOut, action_value, args_from_env, await_turn, context_fingerprint, error_card,
    optional_env, report_progress, turn_phase, turn_timeout_from_env,
};
use crate::{
    BackendAdapter, BackendIdentity, BackendMetadata, BackendRequest, BackendResponse,
    ProgressReporter, enforce_card_contract, estimate_tokens,
};

use transport::{CodexAppProcess, CodexAppState, TurnOutput};

/// Keeps discovery and patch work on independent app-server processes. The
/// split lets a speculative patch run while the user continues discovery,
/// matching the phase-isolated process model used by the Claude adapter.
pub struct CodexAppBackend {
    command: String,
    args: Vec<String>,
    model: Option<String>,
    effort: Option<String>,
    turn_timeout: Option<Duration>,
    discovery: Arc<Mutex<CodexAppState>>,
    patch: Arc<Mutex<CodexAppState>>,
}

impl CodexAppBackend {
    pub fn from_env() -> Result<Self> {
        let command = std::env::var("LOOPBIOTIC_CODEX_COMMAND").unwrap_or_else(|_| "codex".into());
        let args = args_from_env(
            "LOOPBIOTIC_CODEX_ARGS_JSON",
            "LOOPBIOTIC_CODEX_ARGS",
            "app-server --stdio",
        )?;
        let model = optional_env("LOOPBIOTIC_CODEX_MODEL");
        let effort = optional_env("LOOPBIOTIC_CODEX_EFFORT").or_else(|| Some("low".into()));

        Ok(Self::new(command, args, model, effort))
    }

    pub fn new(
        command: impl Into<String>,
        args: Vec<String>,
        model: Option<String>,
        effort: Option<String>,
    ) -> Self {
        Self::with_turn_timeout(command, args, model, effort, turn_timeout_from_env())
    }

    /// Internal constructor that fixes the per-turn deadline instead of
    /// reading it from the environment; tests use it to avoid env races.
    pub(crate) fn with_turn_timeout(
        command: impl Into<String>,
        args: Vec<String>,
        model: Option<String>,
        effort: Option<String>,
        turn_timeout: Option<Duration>,
    ) -> Self {
        Self {
            command: command.into(),
            args,
            model,
            effort,
            turn_timeout,
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
                        "name": "loopbiotic",
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
            "You are a local Loopbiotic coding agent executing one persistent goal. You may use targeted read-only project tools to inspect the repository and choose the next edit. Never edit files yourself. Return exactly one final JSON object matching the supplied output schema and no prose."
        } else if patch_turn {
            "You are a local Loopbiotic pair-programming partner. Do not use tools, commands, file reads, or repo inspection. Never edit files. Return exactly one final JSON object matching the supplied output schema and no prose."
        } else {
            "You are a local Loopbiotic pair-programming partner. You may use at most two targeted read-only project tool calls to find the next relevant code block. Stop searching once the supplied context supports an exact location. Never edit files. Return exactly one final JSON object matching the supplied output schema and no prose."
        };
        let developer_instructions = if goal_loop {
            "Drive the original goal from start to finish. In one work turn inspect every required file and return the complete multi-file patch batch; Loopbiotic reviews its hunks locally. When the user asks why, explain the pending hunk without advancing or replacing it. Preserve progress across turns and do not repeat accepted work."
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

        let first = await_turn(
            "Codex",
            self.turn_timeout,
            self.ask_once(&mut state, req, progress),
        )
        .await;
        let Err(first_error) = first else {
            return first;
        };
        if first_error.is::<TurnTimedOut>() {
            // A wedged app-server would only wedge again; kill it, drop the
            // lane's threads, and let the next turn spawn fresh.
            state.kill_process();
            return Err(first_error);
        }
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
        let retry = await_turn(
            "Codex",
            self.turn_timeout,
            self.ask_once(&mut state, req, progress),
        )
        .await;
        retry.map_err(|retry_error| {
            if retry_error.is::<TurnTimedOut>() {
                state.kill_process();
            }
            anyhow!("codex connection failed: {first_error}; retry: {retry_error}")
        })
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
                    "outputSchema": schema::output_schema(req)
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
        error_card("c_codex_app_error", "Codex app-server error", message)
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
        let card = parse::parse_card(&output.text, &req.card_contract)
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

    async fn identity(&self) -> BackendIdentity {
        BackendIdentity {
            backend: "codex_app".into(),
            // The app-server initialize handshake reports no default model or
            // model list, so only the configured model can be named; turns
            // with model: null use the server's own default.
            model: self.model.clone(),
            models: vec![],
        }
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
    let goal_question = goal_loop
        && req.card_contract.expected_kind == Some(loopbiotic_protocol::CardKind::Finding);
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
             - Tool reads are valid patch source. Loopbiotic verifies every returned hunk against the corresponding live editor buffer before review.\n\
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
    } else if patch_turn {
        format!(
            "- Return exactly one file and exactly one hunk changing at most {} added/removed lines.\n\
             - Change one coherent local block in the supplied excerpt. Leave later blocks for later Loopbiotic cards.\n\
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
        "Source context is unchanged from the preceding turn in this Loopbiotic thread. Reuse that exact buffer and ranked project context.".into()
    };

    format!(
        r#"Return exactly one JSON Loopbiotic op. No markdown. No prose.

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

fn debug(message: &str) {
    if std::env::var("LOOPBIOTIC_DEBUG").is_ok() {
        eprintln!("loopbiotic codex_app: {message}");
    }
}

#[cfg(test)]
mod tests {
    use loopbiotic_protocol::Action;

    use crate::BackendAction;

    use super::*;

    fn request() -> BackendRequest {
        BackendRequest {
            session: crate::SessionSnapshot {
                id: "s_1".into(),
                prompt: "inspect target".into(),
                completed_steps: vec![],
                known_observations: vec![],
                mode: loopbiotic_protocol::Mode::Auto,
                card_count: 0,
                last_card: None,
                last_summary: None,
            },
            action: BackendAction::Start,
            context: loopbiotic_protocol::ContextBundle {
                cwd: "/tmp/project".into(),
                file: "src/main.rs".into(),
                cursor: loopbiotic_protocol::Cursor { line: 1, column: 1 },
                selection: None,
                buffer_text: "unique source payload".into(),
                buffer_start_line: 1,
                diagnostics: vec![],
                hints: vec![],
                artifacts: vec![],
                report: None,
            },
            card_contract: crate::CardContract {
                expected_kind: Some(loopbiotic_protocol::CardKind::Hypothesis),
                ..Default::default()
            },
        }
    }

    #[test]
    fn routes_discovery_and_patch_turns_to_separate_phases() {
        let mut request = request();
        assert_eq!(turn_phase(&request), Phase::Discovery);

        request.card_contract.expected_kind = Some(loopbiotic_protocol::CardKind::Patch);
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

    #[tokio::test]
    async fn wedged_app_server_times_out_and_invalidates_the_lane() {
        // `sleep` accepts a spawn, swallows the initialize request, and never
        // writes to stdout: exactly a wedged CLI (auth prompt, deadlock).
        let backend = CodexAppBackend::with_turn_timeout(
            "sleep",
            vec!["30".into()],
            None,
            None,
            Some(Duration::from_millis(100)),
        );

        let error = backend.ask(&request(), None).await.unwrap_err();

        assert!(error.is::<TurnTimedOut>(), "unexpected error: {error}");
        let lane = backend.lane(Phase::Discovery);
        assert!(
            lane.lock().await.process.is_none(),
            "timed-out process must be invalidated so the next turn spawns fresh"
        );
    }

    #[tokio::test]
    async fn identity_reports_the_configured_model_without_spawning() {
        let backend =
            CodexAppBackend::new("codex-unused", vec![], Some("gpt-5.3-codex".into()), None);

        let identity = backend.identity().await;

        assert_eq!(identity.backend, "codex_app");
        assert_eq!(identity.model.as_deref(), Some("gpt-5.3-codex"));
        assert!(identity.models.is_empty());
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
        request.card_contract.expected_kind = Some(loopbiotic_protocol::CardKind::Patch);
        let first = thread_key(&request);
        request.session.completed_steps.push("first patch".into());

        assert_ne!(first, thread_key(&request));
        assert_eq!(thread_key(&request), "s_1:patch:1");
    }

    #[test]
    fn retry_within_the_same_step_reuses_patch_thread() {
        let mut request = request();
        request.card_contract.expected_kind = Some(loopbiotic_protocol::CardKind::Patch);
        let first = thread_key(&request);
        request.action = BackendAction::ContractRetry("repair it".into());

        assert_eq!(first, thread_key(&request));
    }

    #[test]
    fn why_uses_finding_schema_inside_the_goal_thread() {
        let mut request = request();
        request.card_contract.expected_kind = Some(loopbiotic_protocol::CardKind::Finding);
        request.card_contract.allow_goal_completion = true;
        request.action = BackendAction::User(Action::Why);
        let schema = schema::output_schema(&request);

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
}
