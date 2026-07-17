use std::time::Duration;

use anyhow::{Result, anyhow};
use async_trait::async_trait;
use loopbiotic_protocol::{AgentOp, BackendInfo, Card, TokenUsage};
use serde_json::json;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;

use crate::support::{
    TurnTimedOut, action_value, args_from_env, await_turn, error_card, report_progress,
    turn_timeout_from_env,
};
use crate::{
    BackendAdapter, BackendMetadata, BackendRequest, BackendResponse, LoopbioticStreamEvent,
    ProgressReporter, enforce_card_contract, estimate_tokens, parse_loopbiotic_stream_event,
    result_text,
};

pub struct GenericCliBackend {
    command: String,
    args: Vec<String>,
    turn_timeout: Option<Duration>,
}

impl GenericCliBackend {
    pub fn new(command: impl Into<String>, args: Vec<String>) -> Self {
        Self::with_turn_timeout(command, args, turn_timeout_from_env())
    }

    /// Internal constructor that fixes the per-turn deadline instead of
    /// reading it from the environment; tests use it to avoid env races.
    pub(crate) fn with_turn_timeout(
        command: impl Into<String>,
        args: Vec<String>,
        turn_timeout: Option<Duration>,
    ) -> Self {
        Self {
            command: command.into(),
            args,
            turn_timeout,
        }
    }

    pub fn from_env() -> Result<Self> {
        let command = std::env::var("LOOPBIOTIC_GENERIC_COMMAND")
            .map_err(|_| anyhow!("LOOPBIOTIC_GENERIC_COMMAND is required"))?;
        let args = args_from_env(
            "LOOPBIOTIC_GENERIC_ARGS_JSON",
            "LOOPBIOTIC_GENERIC_ARGS",
            "",
        )?;

        Ok(Self::new(command, args))
    }

    fn prompt(&self, req: &BackendRequest) -> String {
        generic_prompt(req)
    }

    fn error_card(message: impl Into<String>) -> Card {
        error_card("c_backend_error", "Backend error", message)
    }
}

/// The op contract sent on every turn. A `const` so it can never interpolate
/// volatile data: it opens the prompt and anchors the provider prompt cache.
const GENERIC_API_CONTRACT: &str = "Return one JSON Loopbiotic op only. No prose. Ops: hypothesis(title,claim,evidence,next), finding(title,finding,location,annotation), patch(title,explanation,goal_complete,plan,patches), choice(title,question,options), deny(title,reason,location), open_location(reason,location), summary(title,summary,changed_files), error(title,message). choice.options items are {id,label,action} objects; action is one of follow|why|fix|goal|other_lead|retry|edit_prompt|open|run_check|stop. Use deny when you cannot or should not proceed. When limits.conversation_only is true, never return patch or summary. Goal turns are explicitly user-authorized. error is only for technical failures. limits.expected, when set, is the required op. patch.diff must be unified diff hunks starting with @@. Unused schema fields null.";

pub(crate) fn generic_prompt(req: &BackendRequest) -> String {
    let mut rules = vec![
        json!(
            "If a.kind is user and a.action is fix, return a patch op unless a patch is impossible."
        ),
        json!(
            "If s.mode is fix and a.kind is start, return a patch op unless a patch is impossible."
        ),
        json!(
            "For non-fix actions, do not return a patch op unless limits.goal_completion is true."
        ),
        json!(
            "A non-goal patch is one small local pair-programming step: one file, one hunk, and no more changed lines than the supplied limit; its plan is null."
        ),
        json!(crate::IMPLEMENTATION_GUIDELINES),
        json!(
            "Explain why the next coherent block matters and return control to the user after that step."
        ),
    ];
    // Goal rules append after the static base so the shared byte prefix of
    // the rules array survives across goal and non-goal turns.
    if req.card_contract.allow_goal_completion {
        rules.push(json!(
            "Goal turn: return one small, compilable hunk within limits.changed_lines plus plan {remaining:[{file,summary}],complete}. Remaining entries are coherent steps and may repeat a file. Return choice only when a genuine user decision blocks all safe progress; otherwise keep advancing with patch or summary."
        ));
        rules.push(json!(
            "On a goal continuation (a.action is goal), continue with the next planned coherent step."
        ));
        rules.push(json!(
            "If limits.goal_completion is true and limits.expected is finding, explain why the pending hunk is the right next step without replacing it or advancing the goal."
        ));
    }

    // Field order is byte-order: static contract first, session-stable next,
    // volatile per-turn data last, so a provider prompt cache can reuse the
    // longest possible prefix between one-shot requests. `ordered_json_object`
    // preserves this order; `json!` alone would sort keys alphabetically and
    // lead with the volatile action.
    let fields: Vec<(&str, serde_json::Value)> = vec![
        // Static: identical bytes across all turns and sessions.
        ("api", json!(GENERIC_API_CONTRACT)),
        (
            "stream",
            json!({
                "protocol": "ndjson",
                "progress": {"t": "loopbiotic_progress", "phase": "short phase", "message": "short user-visible activity summary"},
                "result": {"t": "loopbiotic_result", "result": "the final Loopbiotic op JSON object"},
                "rules": [
                    "You may emit loopbiotic_progress records before the result.",
                    "Progress messages must be concise user-visible summaries of work, never hidden reasoning or private chain-of-thought.",
                    "The final output may instead be a raw Loopbiotic op for backwards compatibility."
                ]
            }),
        ),
        ("rules", serde_json::Value::Array(rules)),
        // Session-stable: identical bytes on every turn of one session.
        (
            "s",
            json!({
                "id": req.session.id,
                "mode": req.session.mode,
                "p": req.session.prompt,
            }),
        ),
        // Turn-kind-stable: constant across all turns of the same kind.
        (
            "limits",
            json!({
                "one": req.card_contract.one_card_only,
                "max": req.card_contract.max_body_chars,
                "patch_files": req.card_contract.max_patch_files,
                "hunks_per_patch": req.card_contract.max_hunks_per_patch,
                "changed_lines": req.card_contract.max_changed_lines,
                "goal_completion": req.card_contract.allow_goal_completion,
                "conversation_only": req.card_contract.conversation_only,
                "expected": req.card_contract.expected_kind,
            }),
        ),
        // Append-only within a session.
        ("completed_steps", json!(req.session.completed_steps)),
        ("known_observations", json!(req.session.known_observations)),
        // Volatile: changes every turn.
        (
            "interaction_feedback",
            json!(req.session.interaction_feedback),
        ),
        ("a", action_value(&req.action)),
        ("last", json!(req.session.last_summary)),
        ("n", json!(req.session.card_count)),
        ("ctx", crate::backend_context(&req.context)),
    ];

    crate::support::ordered_json_object(&fields)
}

#[async_trait]
impl BackendAdapter for GenericCliBackend {
    async fn next_card(&self, req: BackendRequest) -> Result<BackendResponse> {
        self.next_card_with_progress(req, None).await
    }

    async fn next_card_with_progress(
        &self,
        req: BackendRequest,
        progress: Option<ProgressReporter>,
    ) -> Result<BackendResponse> {
        let prompt = self.prompt(&req);
        let mut command = Command::new(&self.command);
        let backend_name = backend_name(&self.command);

        report_progress(
            progress.as_ref(),
            &req.session.id,
            "starting",
            &format!("Starting {backend_name}"),
        );

        command
            .args(&self.args)
            .kill_on_drop(true)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());

        let mut child = command.spawn()?;

        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(prompt.as_bytes()).await?;
        }

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("backend stdout unavailable"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| anyhow!("backend stderr unavailable"))?;
        let stderr_task = tokio::spawn(async move {
            let mut output = String::new();
            BufReader::new(stderr).read_to_string(&mut output).await?;

            Ok::<_, std::io::Error>(output)
        });
        let mut stdout = BufReader::new(stdout).lines();
        let mut output = Vec::new();

        report_progress(
            progress.as_ref(),
            &req.session.id,
            "requesting",
            &format!("Sending the task to {backend_name}"),
        );

        let stream = async {
            while let Some(line) = stdout.next_line().await? {
                match parse_loopbiotic_stream_event(&line) {
                    Some(LoopbioticStreamEvent::Progress { phase, message }) => {
                        report_progress(progress.as_ref(), &req.session.id, &phase, &message);
                    }
                    Some(LoopbioticStreamEvent::Result(result)) => output.push(result_text(result)),
                    None => output.push(line),
                }
            }

            child.wait().await?;

            Ok(())
        };
        let stream_result = await_turn("The backend CLI", self.turn_timeout, stream).await;
        if stream_result
            .as_ref()
            .is_err_and(|error| error.is::<TurnTimedOut>())
        {
            // A one-shot process: kill the wedged CLI and surface the timeout
            // as a normal backend error.
            let _ = child.start_kill();
        }
        stream_result?;
        let stderr = stderr_task.await??;
        let stdout = output.join("\n");
        let raw_output = format!("{stdout}{stderr}");
        let card = parse_card(&stdout).unwrap_or_else(|error| {
            Self::error_card(format!("{}\n\n{}", error, excerpt(&raw_output)))
        });
        let card = enforce_card_contract(card, &req.card_contract, &backend_name, &raw_output);

        Ok(BackendResponse {
            card,
            raw_output: Some(raw_output),
            metadata: BackendMetadata {
                backend: "generic_cli".into(),
                model: None,
                token_usage: Some(TokenUsage::estimated(
                    estimate_tokens(&prompt),
                    estimate_tokens(&stdout),
                )),
                activities: vec![],
                attempts: vec![],
            },
        })
    }

    fn capabilities(&self) -> BackendInfo {
        BackendInfo {
            name: "generic_cli".into(),
            streaming: true,
            patches: true,
            reasoning: true,
            can_read_project: false,
            can_use_tools: false,
        }
    }
}

fn backend_name(command: &str) -> String {
    std::path::Path::new(command)
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("agent")
        .to_string()
}

pub(crate) fn parse_card(output: &str) -> Result<Card> {
    if let Ok(card) = parse_json_card(output.trim()) {
        return Ok(card);
    }

    let Some(json) = first_json_object(output) else {
        return Err(anyhow!("backend returned no Loopbiotic op"));
    };

    parse_json_card(json)
}

fn parse_json_card(json: &str) -> Result<Card> {
    let value = serde_json::from_str::<serde_json::Value>(json)?;

    // Dispatch on the discriminator so a malformed op reports what is wrong with
    // the op itself instead of the misleading Card error ("missing field kind").
    if value.get("op").is_some() {
        let op = serde_json::from_value::<AgentOp>(value)?;
        return Ok(op.into_card("c_agent"));
    }

    match serde_json::from_value::<Card>(value.clone()) {
        Ok(card) => Ok(card),
        // Agents sometimes name the discriminator "kind" while otherwise
        // emitting an op payload; retry the op parse under that reading.
        Err(card_error) => match value.get("kind").cloned() {
            Some(kind) => {
                let mut value = value;
                value["op"] = kind;
                if let Some(object) = value.as_object_mut() {
                    object.remove("kind");
                }

                match serde_json::from_value::<AgentOp>(value) {
                    Ok(op) => Ok(op.into_card("c_agent")),
                    Err(op_error) => Err(op_error.into()),
                }
            }
            None => Err(card_error.into()),
        },
    }
}

fn excerpt(output: &str) -> String {
    let output = output.trim();

    if output.is_empty() {
        return "Raw output was empty.".into();
    }

    format!("Raw output:\n{}", crate::excerpt(output, 800))
}

fn first_json_object(output: &str) -> Option<&str> {
    let mut start = None;
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;

    for (index, byte) in output.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }

        if byte == '\\' && in_string {
            escaped = true;
            continue;
        }

        if byte == '"' {
            in_string = !in_string;
            continue;
        }

        if in_string {
            continue;
        }

        if byte == '{' {
            if depth == 0 {
                start = Some(index);
            }

            depth += 1;
        }

        if byte == '}' && depth > 0 {
            depth -= 1;

            if depth == 0 {
                let start = start?;

                return output.get(start..=index);
            }
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generic_prompt_adds_the_slice_rule_only_on_goal_turns() {
        let mut req = crate::test_request();
        assert!(!generic_prompt(&req).contains("one small, compilable hunk"));
        assert!(generic_prompt(&req).contains("Compiler acceptance is a hard invariant"));
        assert!(generic_prompt(&req).contains("exactly one uninterrupted change block"));

        req.card_contract.allow_goal_completion = true;
        let goal = generic_prompt(&req);
        assert!(goal.contains("one small, compilable hunk"));
        assert!(goal.contains("plan {remaining:[{file,summary}],complete}"));
        assert!(goal.contains("next planned coherent step"));
    }

    #[test]
    fn generic_prompt_keeps_a_stable_prefix_across_turns_of_one_session() {
        let turn_a = crate::test_request();
        let mut turn_b = crate::test_request();
        turn_b.action = crate::BackendAction::User(loopbiotic_protocol::Action::Follow);
        turn_b
            .session
            .completed_steps
            .push("renamed the helper".into());
        turn_b
            .session
            .known_observations
            .push("the guard drops zero".into());
        turn_b.session.card_count = 4;
        turn_b.session.last_summary = Some("Renamed the helper".into());
        turn_b.context.buffer_text = "fn main() { changed() }".into();

        let a = generic_prompt(&turn_a);
        let b = generic_prompt(&turn_b);

        // The static contract (api, stream, rules), the session-stable `s`
        // block, and the turn-kind-stable `limits` block must stay
        // byte-identical between turns; only the trailing per-turn data may
        // differ, or one-shot requests lose every provider cache hit.
        let stable_block_len = a.find("\"completed_steps\"").expect("lists present");
        assert_eq!(Some(stable_block_len), b.find("\"completed_steps\""));
        let shared = crate::common_prefix_len(&a, &b);
        assert!(
            shared >= stable_block_len,
            "volatile bytes leaked into the stable prefix: shared {shared} < stable {stable_block_len}"
        );
    }

    #[test]
    fn generic_prompt_static_block_is_stable_across_sessions() {
        let session_a = crate::test_request();
        let mut session_b = crate::test_request();
        session_b.session.id = "s_2".into();
        session_b.session.prompt = "add retry logic to the fetcher".into();
        session_b.action = crate::BackendAction::User(loopbiotic_protocol::Action::Fix);
        session_b.context.buffer_text = "fn other() {}".into();

        let a = generic_prompt(&session_a);
        let b = generic_prompt(&session_b);

        // The whole static block — everything before the session-stable `s`
        // key — must be byte-identical across sessions of the same turn kind.
        let static_block_len = a.find(",\"s\":{").expect("session block present");
        assert_eq!(Some(static_block_len), b.find(",\"s\":{"));
        let shared = crate::common_prefix_len(&a, &b);
        assert!(
            shared >= static_block_len,
            "session bytes leaked into the static block: shared {shared} < static {static_block_len}"
        );
        assert!(a.starts_with("{\"api\":"));
    }

    #[test]
    fn extracts_json_card() {
        let output = "text {\"kind\":\"error\",\"id\":\"c_1\",\"title\":\"Nope\",\"message\":\"bad\",\"actions\":[\"retry\",\"stop\"]} tail";
        let card = parse_card(output).unwrap();

        assert!(matches!(card, Card::Error(_)));
    }

    #[test]
    fn parses_choice_op_with_string_options_and_null_fields() {
        let output = r#"{"op":"choice","title":"Clarify what to test","question":"What should we test?","options":["Add a spec","Extend the directive spec","Just a smoke test"],"claim":null,"evidence":null,"next":null,"finding":null,"location":null,"annotation":null,"explanation":null,"patches":null,"summary":null,"changed_files":null,"message":null}"#;
        let card = parse_card(output).unwrap();

        let Card::Choice(card) = card else {
            panic!("expected choice card");
        };
        assert_eq!(card.options.len(), 3);
    }

    #[test]
    fn parses_op_payload_with_kind_discriminator() {
        let output = r#"{"kind":"hypothesis","title":"T","claim":"C","evidence":"src/work.ts:2 — no value returned"}"#;
        let card = parse_card(output).unwrap();

        let Card::Hypothesis(card) = card else {
            panic!("expected hypothesis card");
        };
        assert_eq!(card.evidence.as_ref().unwrap().line, 2);
    }

    #[test]
    fn parses_deny_op() {
        let output =
            r#"{"op":"deny","title":"Ambiguous prompt","reason":"Say which spec to write."}"#;
        let card = parse_card(output).unwrap();

        assert!(matches!(card, Card::Deny(_)));
    }

    #[test]
    fn reports_op_error_instead_of_card_error() {
        let output = r#"{"op":"finding","title":"T"}"#;
        let error = parse_card(output).unwrap_err().to_string();

        assert!(error.contains("finding"), "unexpected error: {error}");
        assert!(!error.contains("kind"), "unexpected error: {error}");
    }

    #[test]
    fn extracts_agent_op() {
        let output =
            "text {\"op\":\"hypothesis\",\"title\":\"Maybe\",\"claim\":\"It may happen\"} tail";
        let card = parse_card(output).unwrap();

        assert!(matches!(card, Card::Hypothesis(_)));
    }

    #[tokio::test]
    async fn wedged_cli_times_out_and_is_killed() {
        // `sleep` swallows the prompt on stdin and never answers: a stand-in
        // for a CLI stuck on an auth prompt or deadlock.
        let backend = GenericCliBackend::with_turn_timeout(
            "sleep",
            vec!["30".into()],
            Some(std::time::Duration::from_millis(100)),
        );

        let error = backend.next_card(crate::test_request()).await.unwrap_err();

        assert!(
            error.is::<crate::support::TurnTimedOut>(),
            "unexpected error: {error}"
        );
    }
}
