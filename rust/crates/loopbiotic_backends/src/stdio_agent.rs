use std::process::Stdio;
use std::time::Duration;

use anyhow::{Result, anyhow};
use async_trait::async_trait;
use loopbiotic_protocol::{AgentOp, BackendInfo, Card, TokenUsage};
use serde_json::json;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::Mutex;

use crate::support::{
    TurnTimedOut, action_value, args_from_env, await_turn, error_card, report_progress,
    turn_timeout_from_env,
};
use crate::{
    BackendAdapter, BackendMetadata, BackendRequest, BackendResponse, LoopbioticStreamEvent,
    ProgressReporter, enforce_card_contract, estimate_tokens, parse_loopbiotic_stream_event,
    result_text,
};

pub struct StdioAgentBackend {
    command: String,
    args: Vec<String>,
    turn_timeout: Option<Duration>,
    process: Mutex<Option<AgentProcess>>,
}

struct AgentProcess {
    child: Child,
    stdin: ChildStdin,
    stdout: Lines<BufReader<ChildStdout>>,
}

impl StdioAgentBackend {
    pub fn from_env() -> Result<Self> {
        let command = std::env::var("LOOPBIOTIC_AGENT_COMMAND")
            .map_err(|_| anyhow!("LOOPBIOTIC_AGENT_COMMAND is required"))?;
        let args = args_from_env("LOOPBIOTIC_AGENT_ARGS_JSON", "LOOPBIOTIC_AGENT_ARGS", "")?;

        Ok(Self::new(command, args))
    }

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
            process: Mutex::new(None),
        }
    }

    async fn ensure(&self) -> Result<()> {
        let mut process = self.process.lock().await;

        if process.is_some() {
            return Ok(());
        }

        let mut child = Command::new(&self.command)
            .args(&self.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow!("agent stdin unavailable"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("agent stdout unavailable"))?;

        *process = Some(AgentProcess {
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
    ) -> Result<AgentAnswer> {
        report_progress(
            progress,
            &req.session.id,
            "starting",
            "Starting agent process",
        );
        self.ensure().await?;

        let mut guard = self.process.lock().await;
        let process = guard
            .as_mut()
            .ok_or_else(|| anyhow!("agent process unavailable"))?;

        let result = await_turn(
            "The agent",
            self.turn_timeout,
            exchange(process, req, progress),
        )
        .await;

        if result
            .as_ref()
            .is_err_and(|error| error.is::<TurnTimedOut>())
        {
            // Kill the wedged agent and forget it so the next turn respawns.
            if let Some(process) = guard.as_mut() {
                let _ = process.child.start_kill();
            }
            *guard = None;
        }

        result
    }

    fn error_card(message: impl Into<String>) -> Card {
        error_card("c_agent_error", "Agent error", message)
    }
}

/// Sends one turn to the agent and reads its stream until the result line.
async fn exchange(
    process: &mut AgentProcess,
    req: &BackendRequest,
    progress: Option<&ProgressReporter>,
) -> Result<AgentAnswer> {
    let event = agent_event(req);
    let line = serde_json::to_string(&event)?;
    let input_tokens = estimate_tokens(&line);

    process.stdin.write_all(line.as_bytes()).await?;
    process.stdin.write_all(b"\n").await?;
    process.stdin.flush().await?;

    report_progress(
        progress,
        &req.session.id,
        "working",
        "Agent is processing the request",
    );

    loop {
        let Some(line) = process.stdout.next_line().await? else {
            return Err(anyhow!("agent closed stdout"));
        };

        match parse_loopbiotic_stream_event(&line) {
            Some(LoopbioticStreamEvent::Progress { phase, message }) => {
                report_progress(progress, &req.session.id, &phase, &message);
            }
            Some(LoopbioticStreamEvent::Result(result)) => {
                return Ok(AgentAnswer {
                    line: result_text(result),
                    input_tokens,
                });
            }
            None => return Ok(AgentAnswer { line, input_tokens }),
        }
    }
}

impl Drop for AgentProcess {
    fn drop(&mut self) {
        let _ = self.child.start_kill();
    }
}

#[async_trait]
impl BackendAdapter for StdioAgentBackend {
    async fn next_card(&self, req: BackendRequest) -> Result<BackendResponse> {
        self.next_card_with_progress(req, None).await
    }

    async fn next_card_with_progress(
        &self,
        req: BackendRequest,
        progress: Option<ProgressReporter>,
    ) -> Result<BackendResponse> {
        let answer = self.ask(&req, progress.as_ref()).await?;
        let raw_output = answer.line;
        let output_tokens = estimate_tokens(&raw_output);
        let card = parse_agent_output(&raw_output)
            .unwrap_or_else(|error| Self::error_card(format!("{}\n\n{}", error, raw_output)));
        let card = enforce_card_contract(card, &req.card_contract, "Agent", &raw_output);

        Ok(BackendResponse {
            card,
            raw_output: Some(raw_output),
            metadata: BackendMetadata {
                backend: "agent_stdio".into(),
                model: None,
                token_usage: Some(TokenUsage::estimated(answer.input_tokens, output_tokens)),
                activities: vec![],
                attempts: vec![],
            },
        })
    }

    async fn cancel_turn(&self, _session_id: &str) -> Result<()> {
        let mut process = self.process.lock().await;
        if let Some(active) = process.as_mut() {
            let _ = active.child.start_kill();
        }
        *process = None;

        Ok(())
    }

    fn capabilities(&self) -> BackendInfo {
        BackendInfo {
            name: "agent_stdio".into(),
            streaming: true,
            patches: true,
            reasoning: true,
            can_read_project: false,
            can_use_tools: false,
        }
    }
}

#[derive(Debug)]
struct AgentAnswer {
    line: String,
    input_tokens: usize,
}

fn agent_event(req: &BackendRequest) -> serde_json::Value {
    json!({
        "t": "loopbiotic_event",
        "api": agent_api(),
        "s": {
            "id": req.session.id,
            "p": req.session.prompt,
            "interaction_feedback": req.session.interaction_feedback,
            "completed_steps": req.session.completed_steps,
            "known_observations": req.session.known_observations,
            "mode": req.session.mode,
            "n": req.session.card_count,
            "last": req.session.last_summary,
            "project": req.session.project,
            "skills": req.session.skills
        },
        "a": action_value(&req.action),
        "ctx": crate::backend_context(&req.context),
        "limits": req.card_contract
    })
}

fn agent_api() -> serde_json::Value {
    json!(format!(
        "Return one JSON Loopbiotic op only. Ops: hypothesis, finding, patch, choice, deny, open_location, summary, error. When limits.conversation_only is true, never return patch or summary. Return patch for user action fix or mode fix/propose unless impossible. The user-selected mode and limits.expected define the response contract; never infer or replace the mode. Goal execution is explicit and advances one small, compilable hunk per turn with a plan of remaining coherent steps. A patch is exactly one file and one hunk within the supplied changed-line limit. You may emit loopbiotic_progress records before the result. Never emit hidden reasoning. End with either a raw Loopbiotic op or a loopbiotic_result record. Implementation guidelines: {} Flow guidelines: {}",
        crate::IMPLEMENTATION_GUIDELINES,
        crate::FLOW_GUIDELINES
    ))
}

fn parse_agent_output(output: &str) -> Result<Card> {
    let op = serde_json::from_str::<AgentOp>(output.trim())?;

    Ok(op.into_card("c_agent"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_agent_op() {
        let card = parse_agent_output(r#"{"op":"hypothesis","title":"T","claim":"C"}"#).unwrap();

        assert!(matches!(card, Card::Hypothesis(_)));
    }

    #[test]
    fn agent_api_requires_dependency_first_compiler_safe_patches() {
        let api = agent_api().as_str().unwrap().to_string();

        assert!(api.contains("Compiler acceptance is a hard invariant"));
        assert!(api.contains("before any later patch first references, implements"));
        assert!(api.contains("exactly one uninterrupted change block"));
        assert!(api.contains("Do not use tools or searches to re-enumerate"));
    }

    #[tokio::test]
    async fn wedged_agent_times_out_and_respawns_next_turn() {
        // `sleep` swallows the event on stdin and never answers: a stand-in
        // for an agent stuck on an auth prompt or deadlock.
        let backend = StdioAgentBackend::with_turn_timeout(
            "sleep",
            vec!["30".into()],
            Some(Duration::from_millis(100)),
        );

        let error = backend.ask(&crate::test_request(), None).await.unwrap_err();

        assert!(error.is::<TurnTimedOut>(), "unexpected error: {error}");
        assert!(
            backend.process.lock().await.is_none(),
            "timed-out process must be cleared so the next turn respawns"
        );
    }
}
