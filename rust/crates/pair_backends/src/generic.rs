use anyhow::{Result, anyhow};
use async_trait::async_trait;
use pair_protocol::{Action, AgentOp, BackendInfo, Card, ErrorCard, TokenUsage};
use serde_json::json;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

use crate::{BackendAdapter, BackendMetadata, BackendRequest, BackendResponse, estimate_tokens};

pub struct GenericCliBackend {
    command: String,
    args: Vec<String>,
}

impl GenericCliBackend {
    pub fn new(command: impl Into<String>, args: Vec<String>) -> Self {
        Self {
            command: command.into(),
            args,
        }
    }

    pub fn from_env() -> Result<Self> {
        let command = std::env::var("PAIR_GENERIC_COMMAND")
            .map_err(|_| anyhow!("PAIR_GENERIC_COMMAND is required"))?;
        let args = std::env::var("PAIR_GENERIC_ARGS")
            .unwrap_or_default()
            .split_whitespace()
            .map(str::to_string)
            .collect();

        Ok(Self { command, args })
    }

    fn prompt(&self, req: &BackendRequest) -> String {
        let value = json!({
            "api": "Return one JSON Pair op only. No prose. Ops: hypothesis(title,claim,evidence,next), finding(title,finding,location,annotation), patch(title,explanation,patches), choice(title,question,options), summary(title,summary,changed_files), error(title,message). Patch only for fix. patch.diff must be unified diff hunks starting with @@. Unused schema fields null.",
            "limits": {
                "one": req.card_contract.one_card_only,
                "max": req.card_contract.max_body_chars
            },
            "s": {
                "id": req.session.id,
                "p": req.session.prompt,
                "n": req.session.card_count,
                "last": req.session.last_summary
            },
            "a": action_value(&req.action),
            "ctx": req.context
        });

        serde_json::to_string(&value).unwrap_or_default()
    }

    fn error_card(message: impl Into<String>) -> Card {
        Card::Error(ErrorCard {
            id: "c_backend_error".into(),
            title: "Backend error".into(),
            message: message.into(),
            actions: vec![Action::Retry, Action::EditPrompt, Action::Stop],
        })
    }
}

fn action_value(action: &crate::BackendAction) -> serde_json::Value {
    match action {
        crate::BackendAction::Start => json!({"kind": "start"}),
        crate::BackendAction::User(action) => {
            json!({"kind": "user", "action": format!("{action:?}")})
        }
        crate::BackendAction::Reply(text) => json!({"kind": "reply", "text": text}),
    }
}

#[async_trait]
impl BackendAdapter for GenericCliBackend {
    async fn next_card(&self, req: BackendRequest) -> Result<BackendResponse> {
        let prompt = self.prompt(&req);
        let mut command = Command::new(&self.command);

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

        let output = child.wait_with_output().await?;
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        let raw_output = format!("{stdout}{stderr}");
        let card = parse_card(&stdout).unwrap_or_else(|error| {
            Self::error_card(format!("{}\n\n{}", error, excerpt(&raw_output)))
        });

        Ok(BackendResponse {
            card,
            raw_output: Some(raw_output),
            metadata: BackendMetadata {
                backend: "generic_cli".into(),
                token_usage: Some(TokenUsage::estimated(
                    estimate_tokens(&prompt),
                    estimate_tokens(&stdout),
                )),
            },
        })
    }

    fn capabilities(&self) -> BackendInfo {
        BackendInfo {
            name: "generic_cli".into(),
            streaming: false,
            patches: true,
            reasoning: true,
            can_read_project: false,
            can_use_tools: false,
        }
    }
}

fn parse_card(output: &str) -> Result<Card> {
    if let Ok(op) = serde_json::from_str::<AgentOp>(output.trim()) {
        return Ok(op.into_card("c_agent"));
    }

    if let Ok(card) = serde_json::from_str(output.trim()) {
        return Ok(card);
    }

    let Some(json) = first_json_object(output) else {
        return Err(anyhow!("backend returned no Pair op"));
    };

    if let Ok(op) = serde_json::from_str::<AgentOp>(json) {
        return Ok(op.into_card("c_agent"));
    }

    Ok(serde_json::from_str(json)?)
}

fn excerpt(output: &str) -> String {
    let output = output.trim();

    if output.is_empty() {
        return "Raw output was empty.".into();
    }

    let mut text = output.chars().take(800).collect::<String>();

    if output.chars().count() > 800 {
        text.push_str("\n...");
    }

    format!("Raw output:\n{text}")
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
    fn extracts_json_card() {
        let output = "text {\"kind\":\"error\",\"id\":\"c_1\",\"title\":\"Nope\",\"message\":\"bad\",\"actions\":[\"retry\",\"stop\"]} tail";
        let card = parse_card(output).unwrap();

        assert!(matches!(card, Card::Error(_)));
    }

    #[test]
    fn extracts_agent_op() {
        let output =
            "text {\"op\":\"hypothesis\",\"title\":\"Maybe\",\"claim\":\"It may happen\"} tail";
        let card = parse_card(output).unwrap();

        assert!(matches!(card, Card::Hypothesis(_)));
    }
}
