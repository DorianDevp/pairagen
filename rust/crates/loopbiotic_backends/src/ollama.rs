use std::time::Duration;

use anyhow::{Result, anyhow};
use async_trait::async_trait;
use loopbiotic_protocol::{BackendInfo, TokenUsage};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::support::{error_card, optional_env, report_progress, turn_timeout_from_env};
use crate::{
    BackendAdapter, BackendIdentity, BackendMetadata, BackendRequest, BackendResponse,
    ProgressReporter, enforce_card_contract, estimate_tokens,
};

/// Listing installed models is identity garnish, not a turn; a hung server
/// must not stall the warmup RPC for the full turn deadline.
const LIST_MODELS_TIMEOUT: Duration = Duration::from_secs(3);

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
        // There is no child process to kill here, so a hung Ollama server is
        // bounded by the same turn deadline the process backends use.
        let mut builder = reqwest::Client::builder();
        if let Some(limit) = turn_timeout_from_env() {
            builder = builder.timeout(limit);
        }
        Self {
            host: host.into().trim_end_matches('/').to_string(),
            model: model.into(),
            keep_alive: keep_alive.into(),
            client: builder.build().unwrap_or_else(|_| reqwest::Client::new()),
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

    /// Names the models installed on the server via `GET /api/tags`. Any
    /// failure yields an empty list: identity must never fail because the
    /// listing did.
    async fn list_models(&self) -> Vec<String> {
        let response = self
            .client
            .get(format!("{}/api/tags", self.host))
            .timeout(LIST_MODELS_TIMEOUT)
            .send()
            .await;

        match response {
            Ok(response) if response.status().is_success() => response
                .json::<Value>()
                .await
                .map(|tags| model_names(&tags))
                .unwrap_or_default(),
            _ => vec![],
        }
    }
}

/// Extracts `models[].name` from an `/api/tags` response body.
fn model_names(tags: &Value) -> Vec<String> {
    tags.get("models")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|model| model.get("name").and_then(Value::as_str))
        .map(str::to_string)
        .collect()
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
        let card = crate::parse_card(&text).unwrap_or_else(|error| {
            error_card(
                crate::UNPARSED_OUTPUT_CARD_ID,
                "Ollama error",
                format!("{error}\n\nRaw output:\n{text}"),
            )
        });
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

    async fn identity(&self) -> BackendIdentity {
        BackendIdentity {
            backend: "ollama".into(),
            // The model env is required, so the next turn's model is always
            // known without asking the server.
            model: Some(self.model.clone()),
            models: self.list_models().await,
            phases: None,
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_names_extracts_names_from_a_tags_response() {
        let tags = json!({
            "models": [
                {"name": "qwen3:8b", "size": 5},
                {"name": "llama3.2:3b"},
                {"size": 7}
            ]
        });

        assert_eq!(model_names(&tags), vec!["qwen3:8b", "llama3.2:3b"]);
    }

    #[test]
    fn model_names_tolerates_malformed_responses() {
        assert!(model_names(&json!({})).is_empty());
        assert!(model_names(&json!({"models": "nope"})).is_empty());
        assert!(model_names(&json!(null)).is_empty());
    }

    #[tokio::test]
    async fn identity_reports_the_required_model_when_listing_fails() {
        // Nothing listens on this port; the listing must fail quietly.
        let backend = OllamaBackend::new("http://127.0.0.1:9", "qwen3:8b", "30m");

        let identity = backend.identity().await;

        assert_eq!(identity.backend, "ollama");
        assert_eq!(identity.model.as_deref(), Some("qwen3:8b"));
        assert!(identity.models.is_empty());
    }
}
