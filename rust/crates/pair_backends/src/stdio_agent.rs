use std::process::Stdio;

use anyhow::{Result, anyhow};
use async_trait::async_trait;
use pair_protocol::{Action, AgentOp, BackendInfo, Card, ErrorCard, TokenUsage};
use serde_json::json;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::Mutex;

use crate::{
    BackendAction, BackendAdapter, BackendMetadata, BackendProgress, BackendRequest,
    BackendResponse, PairStreamEvent, ProgressReporter, enforce_card_contract, estimate_tokens,
    parse_pair_stream_event, result_text,
};

pub struct StdioAgentBackend {
    command: String,
    args: Vec<String>,
    process: Mutex<Option<AgentProcess>>,
}

struct AgentProcess {
    child: Child,
    stdin: ChildStdin,
    stdout: Lines<BufReader<ChildStdout>>,
}

impl StdioAgentBackend {
    pub fn from_env() -> Result<Self> {
        let command = std::env::var("PAIR_AGENT_COMMAND")
            .map_err(|_| anyhow!("PAIR_AGENT_COMMAND is required"))?;
        let args = args_from_env("PAIR_AGENT_ARGS_JSON", "PAIR_AGENT_ARGS")?;

        Ok(Self::new(command, args))
    }

    pub fn new(command: impl Into<String>, args: Vec<String>) -> Self {
        Self {
            command: command.into(),
            args,
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

        let mut process = self.process.lock().await;
        let process = process
            .as_mut()
            .ok_or_else(|| anyhow!("agent process unavailable"))?;
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

            match parse_pair_stream_event(&line) {
                Some(PairStreamEvent::Progress { phase, message }) => {
                    report_progress(progress, &req.session.id, &phase, &message);
                }
                Some(PairStreamEvent::Result(result)) => {
                    return Ok(AgentAnswer {
                        line: result_text(result),
                        input_tokens,
                    });
                }
                None => return Ok(AgentAnswer { line, input_tokens }),
            }
        }
    }

    fn error_card(message: impl Into<String>) -> Card {
        Card::Error(ErrorCard {
            id: "c_agent_error".into(),
            title: "Agent error".into(),
            message: message.into(),
            actions: vec![Action::Retry, Action::EditPrompt, Action::Stop],
        })
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
                token_usage: Some(TokenUsage::estimated(answer.input_tokens, output_tokens)),
                activities: vec![],
                attempts: vec![],
            },
        })
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

struct AgentAnswer {
    line: String,
    input_tokens: usize,
}

fn agent_event(req: &BackendRequest) -> serde_json::Value {
    json!({
        "t": "pair_event",
        "api": agent_api(),
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
        "ctx": crate::backend_context(&req.context),
        "limits": req.card_contract
    })
}

fn action_value(action: &BackendAction) -> serde_json::Value {
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

fn agent_api() -> serde_json::Value {
    json!(
        "Return one JSON Pair op only. Ops: hypothesis, finding, patch, choice, deny, summary, error. Use deny(title,reason) when you cannot or should not proceed, such as an ambiguous prompt or missing information; the reason is shown to the user. error is only for technical failures. Behave as an equal pair-programming partner: explain what you noticed and why the next coherent block matters, then return control to the user. Never plan or complete a whole refactor in one response. Return patch for user action fix or start mode fix unless impossible. When limits.allow_goal_completion is true, return a patch if the original goal is unresolved or a summary if it is complete; never restart discovery. A patch is one local step: exactly one file and one hunk within the supplied changed-line limit. patch.diff must be a unified diff hunk starting with @@. You may first emit newline-delimited {\"t\":\"pair_progress\",\"phase\":string,\"message\":string} records with concise user-visible activity summaries. Never emit hidden reasoning or private chain-of-thought. End with either a raw Pair op or {\"t\":\"pair_result\",\"result\":<Pair op>}."
    )
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
    fn serializes_user_action_as_protocol_value() {
        let value = action_value(&BackendAction::User(Action::Fix));

        assert_eq!(value["action"], "fix");
    }
}
