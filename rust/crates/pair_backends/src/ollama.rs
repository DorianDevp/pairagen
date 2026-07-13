use anyhow::{Result, anyhow};
use async_trait::async_trait;
use pair_protocol::{Action, BackendInfo, Card, ErrorCard, TokenUsage};
use serde::Deserialize;
use serde_json::json;

use crate::{
    BackendAdapter, BackendMetadata, BackendProgress, BackendRequest, BackendResponse,
    ProgressReporter, enforce_card_contract, estimate_tokens,
};

/// Talks to a local Ollama server over its HTTP API instead of spawning
/// `ollama run` per card. The server keeps the model loaded between turns
/// (`keep_alive`), and `format: json` forces the model to emit parseable ops.
pub struct OllamaBackend {
    host: String,
    model: String,
    keep_alive: String,
    client: reqwest::Client,
}

#[derive(Deserialize)]
struct ChatResponse {
    message: ChatMessage,
    #[serde(default)]
    prompt_eval_count: Option<usize>,
    #[serde(default)]
    eval_count: Option<usize>,
}

#[derive(Deserialize)]
struct ChatMessage {
    content: String,
}

impl OllamaBackend {
    pub fn from_env() -> Result<Self> {
        let model = std::env::var("PAIR_OLLAMA_MODEL")
            .map_err(|_| anyhow!("PAIR_OLLAMA_MODEL is required"))?;
        let host = std::env::var("PAIR_OLLAMA_HOST")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| "http://127.0.0.1:11434".into());
        let keep_alive = std::env::var("PAIR_OLLAMA_KEEP_ALIVE")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| "30m".into());

        Ok(Self::new(host, model, keep_alive))
    }

    pub fn new(
        host: impl Into<String>,
        model: impl Into<String>,
        keep_alive: impl Into<String>,
    ) -> Self {
        Self {
            host: host.into().trim_end_matches('/').to_string(),
            model: model.into(),
            keep_alive: keep_alive.into(),
            client: reqwest::Client::new(),
        }
    }

    async fn ask(&self, prompt: &str) -> Result<ChatResponse> {
        let response = self
            .client
            .post(format!("{}/api/chat", self.host))
            .json(&json!({
                "model": self.model,
                "stream": false,
                "format": "json",
                "keep_alive": self.keep_alive,
                "messages": [
                    {"role": "user", "content": prompt}
                ]
            }))
            .send()
            .await
            .map_err(|error| anyhow!("could not reach ollama at {}: {error}", self.host))?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(anyhow!("ollama returned {status}: {}", body.trim()));
        }

        Ok(response.json().await?)
    }

    fn error_card(message: impl Into<String>) -> Card {
        Card::Error(ErrorCard {
            id: "c_ollama_error".into(),
            title: "Ollama error".into(),
            message: message.into(),
            actions: vec![Action::Retry, Action::EditPrompt, Action::Stop],
        })
    }
}

#[async_trait]
impl BackendAdapter for OllamaBackend {
    async fn next_card(&self, req: BackendRequest) -> Result<BackendResponse> {
        self.next_card_with_progress(req, None).await
    }

    async fn next_card_with_progress(
        &self,
        req: BackendRequest,
        progress: Option<ProgressReporter>,
    ) -> Result<BackendResponse> {
        let prompt = crate::generic_prompt(&req);

        report_progress(
            progress.as_ref(),
            &req.session.id,
            "requesting",
            &format!("Sending the task to {}", self.model),
        );

        let response = self.ask(&prompt).await?;

        let text = response.message.content;
        let card = crate::parse_card(&text)
            .unwrap_or_else(|error| Self::error_card(format!("{error}\n\nRaw output:\n{text}")));
        let card = enforce_card_contract(card, &req.card_contract, &self.model, &text);
        let token_usage = match (response.prompt_eval_count, response.eval_count) {
            (Some(input), Some(output)) => TokenUsage::reported(input, output),
            _ => TokenUsage::estimated(estimate_tokens(&prompt), estimate_tokens(&text)),
        };

        Ok(BackendResponse {
            card,
            raw_output: Some(text),
            metadata: BackendMetadata {
                backend: "ollama".into(),
                token_usage: Some(token_usage),
                activities: vec![],
                attempts: vec![],
            },
        })
    }

    fn capabilities(&self) -> BackendInfo {
        BackendInfo {
            name: "ollama".into(),
            streaming: false,
            patches: true,
            reasoning: false,
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
