use anyhow::{Result, anyhow};
use async_trait::async_trait;
use pair_protocol::{Action, AgentOp, BackendInfo, Card, ErrorCard, TokenUsage};
use serde_json::json;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;

use crate::{
    BackendAdapter, BackendMetadata, BackendProgress, BackendRequest, BackendResponse,
    PairStreamEvent, ProgressReporter, enforce_card_contract, estimate_tokens,
    parse_pair_stream_event, result_text,
};

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
        let args = args_from_env("PAIR_GENERIC_ARGS_JSON", "PAIR_GENERIC_ARGS")?;

        Ok(Self { command, args })
    }

    fn prompt(&self, req: &BackendRequest) -> String {
        let value = json!({
            "api": "Return one JSON Pair op only. No prose. Ops: hypothesis(title,claim,evidence,next), finding(title,finding,location,annotation), patch(title,explanation,patches), choice(title,question,options), summary(title,summary,changed_files), error(title,message). Patch only for fix. patch.diff must be unified diff hunks starting with @@. Unused schema fields null.",
            "stream": {
                "protocol": "ndjson",
                "progress": {"t": "pair_progress", "phase": "short phase", "message": "short user-visible activity summary"},
                "result": {"t": "pair_result", "result": "the final Pair op JSON object"},
                "rules": [
                    "You may emit pair_progress records before the result.",
                    "Progress messages must be concise user-visible summaries of work, never hidden reasoning or private chain-of-thought.",
                    "The final output may instead be a raw Pair op for backwards compatibility."
                ]
            },
            "rules": [
                "If a.kind is user and a.action is fix, return a patch op unless a patch is impossible.",
                "If s.mode is fix and a.kind is start, return a patch op unless a patch is impossible.",
                "For non-fix actions, do not return a patch op.",
                "If limits.goal_completion is true, return one patch when the original goal is unresolved or one summary when it is complete; never restart discovery.",
                "A patch is one small local pair-programming step: one file, one hunk, and no more changed lines than the supplied limit.",
                "Do not complete or plan a whole refactor in one response.",
                "Explain why the next coherent block matters and return control to the user after that step."
            ],
            "limits": {
                "one": req.card_contract.one_card_only,
                "max": req.card_contract.max_body_chars,
                "patch_files": req.card_contract.max_patch_files,
                "hunks_per_patch": req.card_contract.max_hunks_per_patch,
                "changed_lines": req.card_contract.max_changed_lines,
                "goal_completion": req.card_contract.allow_goal_completion
            },
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
            "ctx": crate::backend_context(&req.context)
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
            json!({"kind": "user", "action": serde_json::to_value(action).unwrap_or_default()})
        }
        crate::BackendAction::Reply(text) => json!({"kind": "reply", "text": text}),
        crate::BackendAction::ContractRetry(reason) => {
            json!({"kind": "contract_retry", "reason": reason})
        }
    }
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

        while let Some(line) = stdout.next_line().await? {
            match parse_pair_stream_event(&line) {
                Some(PairStreamEvent::Progress { phase, message }) => {
                    report_progress(progress.as_ref(), &req.session.id, &phase, &message);
                }
                Some(PairStreamEvent::Result(result)) => output.push(result_text(result)),
                None => output.push(line),
            }
        }

        child.wait().await?;
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

fn backend_name(command: &str) -> String {
    std::path::Path::new(command)
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("agent")
        .to_string()
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

    #[test]
    fn serializes_user_action_as_protocol_value() {
        let value = action_value(&crate::BackendAction::User(Action::Fix));

        assert_eq!(value["action"], "fix");
    }
}
