use anyhow::{Result, anyhow};
use async_trait::async_trait;
use loopbiotic_protocol::{BackendInfo, Card, TokenUsage};
use serde::Deserialize;
use serde_json::json;

use crate::support::{error_card, optional_env, report_progress};
use crate::{
    BackendAdapter, BackendMetadata, BackendRequest, BackendResponse, ProgressReporter,
    enforce_card_contract, estimate_tokens,
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
        let model = std::env::var("LOOPBIOTIC_OLLAMA_MODEL")
            .map_err(|_| anyhow!("LOOPBIOTIC_OLLAMA_MODEL is required"))?;
        let host = optional_env("LOOPBIOTIC_OLLAMA_HOST")
            .unwrap_or_else(|| "http://127.0.0.1:11434".into());
        let keep_alive =
            optional_env("LOOPBIOTIC_OLLAMA_KEEP_ALIVE").unwrap_or_else(|| "30m".into());

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
        error_card("c_ollama_error", "Ollama error", message)
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
                model: Some(self.model.clone()),
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
