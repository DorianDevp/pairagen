use std::time::Duration;

use anyhow::{Result, anyhow};
use async_trait::async_trait;
use loopbiotic_protocol::{BackendInfo, Card, TokenUsage};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::support::{error_card, optional_env, report_progress, turn_timeout_from_env};
use crate::{
    BackendAdapter, BackendIdentity, BackendMetadata, BackendRequest, BackendResponse,
    ProgressReporter, enforce_card_contract, estimate_tokens,
};

const LIST_MODELS_TIMEOUT: Duration = Duration::from_secs(3);

/// OpenAI-compatible local HTTP backend, primarily used with LM Studio. It
/// keeps benchmark traffic inside the machine and uses the same typed patch
/// schema and Rust renderer as the Codex backend.
pub struct OpenAiCompatibleBackend {
    base_url: String,
    model: String,
    api_key: Option<String>,
    max_tokens: usize,
    client: reqwest::Client,
}

#[derive(Deserialize)]
struct CompletionResponse {
    choices: Vec<CompletionChoice>,
    #[serde(default)]
    usage: Option<CompletionUsage>,
}

#[derive(Deserialize)]
struct CompletionChoice {
    message: CompletionMessage,
}

#[derive(Deserialize)]
struct CompletionMessage {
    content: String,
}

#[derive(Deserialize)]
struct CompletionUsage {
    prompt_tokens: usize,
    completion_tokens: usize,
}

impl OpenAiCompatibleBackend {
    pub fn from_env() -> Result<Self> {
        let model = std::env::var("LOOPBIOTIC_OPENAI_MODEL")
            .map_err(|_| anyhow!("LOOPBIOTIC_OPENAI_MODEL is required"))?;
        let base_url = optional_env("LOOPBIOTIC_OPENAI_BASE_URL")
            .unwrap_or_else(|| "http://127.0.0.1:1234/v1".into());
        let api_key = optional_env("LOOPBIOTIC_OPENAI_API_KEY");
        let max_tokens = optional_env("LOOPBIOTIC_OPENAI_MAX_TOKENS")
            .map(|value| value.parse())
            .transpose()?
            .unwrap_or(4096);
        Ok(Self::new(base_url, model, api_key, max_tokens))
    }

    pub fn new(
        base_url: impl Into<String>,
        model: impl Into<String>,
        api_key: Option<String>,
        max_tokens: usize,
    ) -> Self {
        let mut builder = reqwest::Client::builder();
        if let Some(limit) = turn_timeout_from_env() {
            builder = builder.timeout(limit);
        }
        Self {
            base_url: base_url.into().trim_end_matches('/').to_string(),
            model: model.into(),
            api_key,
            max_tokens,
            client: builder.build().unwrap_or_else(|_| reqwest::Client::new()),
        }
    }

    async fn ask(&self, prompt: &str, req: &BackendRequest) -> Result<CompletionResponse> {
        let schema = crate::codex_app::schema::output_schema(req);
        let mut request = self
            .client
            .post(format!("{}/chat/completions", self.base_url))
            .json(&json!({
                "model": self.model,
                "temperature": 0,
                "seed": 42,
                "max_tokens": self.max_tokens,
                "response_format": {
                    "type": "json_schema",
                    "json_schema": {
                        "name": "loopbiotic_agent_op",
                        "strict": true,
                        "schema": schema
                    }
                },
                "messages": [{"role": "user", "content": prompt}]
            }));
        if let Some(api_key) = &self.api_key {
            request = request.bearer_auth(api_key);
        }
        let response = request.send().await.map_err(|error| {
            anyhow!(
                "could not reach OpenAI-compatible server at {}: {error}",
                self.base_url
            )
        })?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(anyhow!(
                "OpenAI-compatible server returned {status}: {}",
                body.trim()
            ));
        }
        Ok(response.json().await?)
    }

    async fn list_models(&self) -> Vec<String> {
        let mut request = self
            .client
            .get(format!("{}/models", self.base_url))
            .timeout(LIST_MODELS_TIMEOUT);
        if let Some(api_key) = &self.api_key {
            request = request.bearer_auth(api_key);
        }
        match request.send().await {
            Ok(response) if response.status().is_success() => response
                .json::<Value>()
                .await
                .map(|value| model_names(&value))
                .unwrap_or_default(),
            _ => vec![],
        }
    }

    fn error_card(message: impl Into<String>) -> Card {
        error_card("c_openai_compatible_error", "Local model error", message)
    }
}

#[async_trait]
impl BackendAdapter for OpenAiCompatibleBackend {
    async fn next_card(&self, req: BackendRequest) -> Result<BackendResponse> {
        self.next_card_with_progress(req, None).await
    }

    async fn next_card_with_progress(
        &self,
        req: BackendRequest,
        progress: Option<ProgressReporter>,
    ) -> Result<BackendResponse> {
        let prompt = crate::generic::structured_prompt(&req);
        report_progress(
            progress.as_ref(),
            &req.session.id,
            "requesting",
            &format!("Sending the task to {}", self.model),
        );
        let response = self.ask(&prompt, &req).await?;
        let text = response
            .choices
            .into_iter()
            .next()
            .ok_or_else(|| anyhow!("OpenAI-compatible response has no choices"))?
            .message
            .content;
        let card = crate::codex_app::parse::parse_card(&text, &req.card_contract)
            .unwrap_or_else(|error| Self::error_card(format!("{error}\n\nRaw output:\n{text}")));
        let card = enforce_card_contract(card, &req.card_contract, &self.model, &text);
        let token_usage = response
            .usage
            .map(|usage| TokenUsage::reported(usage.prompt_tokens, usage.completion_tokens))
            .unwrap_or_else(|| {
                TokenUsage::estimated(estimate_tokens(&prompt), estimate_tokens(&text))
            });
        Ok(BackendResponse {
            card,
            raw_output: Some(text),
            metadata: BackendMetadata {
                backend: "openai_compatible".into(),
                model: Some(self.model.clone()),
                token_usage: Some(token_usage),
                activities: vec![],
                attempts: vec![],
            },
        })
    }

    async fn identity(&self) -> BackendIdentity {
        BackendIdentity {
            backend: "openai_compatible".into(),
            model: Some(self.model.clone()),
            models: self.list_models().await,
            phases: None,
        }
    }

    fn capabilities(&self) -> BackendInfo {
        BackendInfo {
            name: "openai_compatible".into(),
            streaming: false,
            patches: true,
            reasoning: false,
            can_read_project: false,
            can_use_tools: false,
        }
    }
}

fn model_names(value: &Value) -> Vec<String> {
    value
        .get("data")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|model| model.get("id").and_then(Value::as_str))
        .map(str::to_string)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_openai_model_ids() {
        assert_eq!(
            model_names(&json!({"data": [{"id": "gemma"}, {"id": "qwen"}]})),
            vec!["gemma", "qwen"]
        );
    }

    #[test]
    fn patch_schema_uses_the_codex_typed_hunk_contract() {
        let mut req = crate::test_request();
        req.card_contract.expected_kind = Some(loopbiotic_protocol::CardKind::Patch);
        let schema = crate::codex_app::schema::output_schema(&req);
        let patch = &schema["properties"]["patches"]["items"];
        assert!(patch["properties"]["diff"].is_null());
        assert!(patch["properties"]["hunks"].is_object());
    }
}
