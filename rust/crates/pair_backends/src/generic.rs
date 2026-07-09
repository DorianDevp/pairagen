use std::time::Duration;

use anyhow::{Result, anyhow};
use async_trait::async_trait;
use pair_protocol::{Action, BackendInfo, Card, ErrorCard};
use serde_json::json;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tokio::time::timeout;

use crate::{BackendAdapter, BackendMetadata, BackendRequest, BackendResponse};

pub struct GenericCliBackend {
    command: String,
    args: Vec<String>,
    timeout: Duration,
}

impl GenericCliBackend {
    pub fn new(command: impl Into<String>, args: Vec<String>) -> Self {
        Self {
            command: command.into(),
            args,
            timeout: Duration::from_secs(60),
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

        Ok(Self::new(command, args))
    }

    fn prompt(&self, req: &BackendRequest) -> String {
        let value = json!({
            "contract": {
                "role": "interactive pair-programming stepper",
                "one_card_only": req.card_contract.one_card_only,
                "patch_only_on_fix": req.card_contract.patch_only_on_fix,
                "max_body_chars": req.card_contract.max_body_chars,
                "rules": [
                    "Return exactly one JSON card.",
                    "Do not return a full plan.",
                    "Do not write prose outside JSON.",
                    "Patch only when action asks for fix.",
                    "If uncertain, return hypothesis."
                ]
            },
            "session": {
                "id": req.session.id,
                "prompt": req.session.prompt,
                "card_count": req.session.card_count,
                "last_card": req.session.last_card
            },
            "action": format!("{:?}", req.action),
            "context": req.context
        });

        serde_json::to_string_pretty(&value).unwrap_or_default()
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

#[async_trait]
impl BackendAdapter for GenericCliBackend {
    async fn next_card(&self, req: BackendRequest) -> Result<BackendResponse> {
        let prompt = self.prompt(&req);
        let mut child = Command::new(&self.command)
            .args(&self.args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()?;

        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(prompt.as_bytes()).await?;
        }

        let output = timeout(self.timeout, child.wait_with_output()).await??;
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        let raw_output = format!("{stdout}{stderr}");
        let card = parse_card(&stdout).unwrap_or_else(|error| Self::error_card(error.to_string()));

        Ok(BackendResponse {
            card,
            raw_output: Some(raw_output),
            metadata: BackendMetadata {
                backend: "generic_cli".into(),
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
    if let Ok(card) = serde_json::from_str(output.trim()) {
        return Ok(card);
    }

    let Some(json) = first_json_object(output) else {
        return Err(anyhow!("backend returned no JSON card"));
    };

    Ok(serde_json::from_str(json)?)
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
}
