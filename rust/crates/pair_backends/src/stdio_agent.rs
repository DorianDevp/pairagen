use std::process::Stdio;

use anyhow::{Result, anyhow};
use async_trait::async_trait;
use pair_protocol::{Action, AgentOp, BackendInfo, Card, ErrorCard, TokenUsage};
use serde_json::json;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::Mutex;

use crate::{
    BackendAction, BackendAdapter, BackendMetadata, BackendRequest, BackendResponse,
    estimate_tokens,
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
        let args = std::env::var("PAIR_AGENT_ARGS")
            .unwrap_or_default()
            .split_whitespace()
            .map(str::to_string)
            .collect();

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

    async fn ask(&self, req: &BackendRequest) -> Result<AgentAnswer> {
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

        let Some(line) = process.stdout.next_line().await? else {
            return Err(anyhow!("agent closed stdout"));
        };

        Ok(AgentAnswer { line, input_tokens })
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
        let answer = self.ask(&req).await?;
        let raw_output = answer.line;
        let output_tokens = estimate_tokens(&raw_output);
        let card = parse_agent_output(&raw_output)
            .unwrap_or_else(|error| Self::error_card(format!("{}\n\n{}", error, raw_output)));

        Ok(BackendResponse {
            card,
            raw_output: Some(raw_output),
            metadata: BackendMetadata {
                backend: "agent_stdio".into(),
                token_usage: Some(TokenUsage::estimated(answer.input_tokens, output_tokens)),
            },
        })
    }

    fn capabilities(&self) -> BackendInfo {
        BackendInfo {
            name: "agent_stdio".into(),
            streaming: false,
            patches: true,
            reasoning: true,
            can_read_project: false,
            can_use_tools: false,
        }
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
            "n": req.session.card_count,
            "last": req.session.last_summary
        },
        "a": action_value(&req.action),
        "ctx": req.context,
        "limits": req.card_contract
    })
}

fn action_value(action: &BackendAction) -> serde_json::Value {
    match action {
        BackendAction::Start => json!({"kind": "start"}),
        BackendAction::User(action) => json!({"kind": "user", "action": format!("{action:?}")}),
        BackendAction::Reply(text) => json!({"kind": "reply", "text": text}),
    }
}

fn agent_api() -> serde_json::Value {
    json!(
        "Return one JSON Pair op only. Ops: hypothesis, finding, patch, choice, summary, error. Patch only for fix. patch.diff must be unified diff hunks starting with @@."
    )
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
}
