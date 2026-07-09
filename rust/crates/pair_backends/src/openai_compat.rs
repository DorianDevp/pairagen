use std::collections::HashMap;
use std::time::Duration;

use anyhow::{Result, anyhow};
use async_trait::async_trait;
use pair_protocol::{Action, AgentOp, BackendInfo, Card, ErrorCard};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::Mutex;

use crate::{BackendAction, BackendAdapter, BackendMetadata, BackendRequest, BackendResponse};

pub struct OpenAiCompatBackend {
    client: Client,
    base_url: String,
    api_key: Option<String>,
    model: String,
    sessions: Mutex<HashMap<String, Vec<Message>>>,
}

impl OpenAiCompatBackend {
    pub fn from_env() -> Result<Self> {
        let base_url =
            std::env::var("PAIR_API_BASE").unwrap_or_else(|_| "http://127.0.0.1:11434/v1".into());
        let model = std::env::var("PAIR_API_MODEL")
            .or_else(|_| std::env::var("PAIR_MODEL"))
            .unwrap_or_else(|_| "qwen2.5-coder:7b".into());
        let api_key = std::env::var("PAIR_API_KEY").ok();

        Self::new(base_url, api_key, model)
    }

    pub fn new(base_url: String, api_key: Option<String>, model: String) -> Result<Self> {
        let client = Client::builder()
            .timeout(Duration::from_secs(120))
            .build()?;

        Ok(Self {
            client,
            base_url: base_url.trim_end_matches('/').into(),
            api_key,
            model,
            sessions: Mutex::new(HashMap::new()),
        })
    }

    async fn messages(&self, req: &BackendRequest) -> Vec<Message> {
        let mut sessions = self.sessions.lock().await;
        let messages = sessions
            .entry(req.session.id.clone())
            .or_insert_with(|| vec![Message::system(system_prompt())]);

        messages.push(Message::user(user_prompt(req)));

        messages.clone()
    }

    async fn remember(&self, session_id: &str, content: String) {
        let mut sessions = self.sessions.lock().await;

        if let Some(messages) = sessions.get_mut(session_id) {
            messages.push(Message::assistant(content));
        }
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
impl BackendAdapter for OpenAiCompatBackend {
    async fn next_card(&self, req: BackendRequest) -> Result<BackendResponse> {
        let session_id = req.session.id.clone();
        let body = ChatRequest {
            model: self.model.clone(),
            messages: self.messages(&req).await,
            temperature: 0.2,
            stream: false,
            response_format: Some(json!({ "type": "json_object" })),
        };
        let mut request = self
            .client
            .post(format!("{}/chat/completions", self.base_url))
            .json(&body);

        if let Some(api_key) = &self.api_key {
            request = request.bearer_auth(api_key);
        }

        let response = request.send().await?;
        let status = response.status();
        let text = response.text().await?;

        if !status.is_success() {
            return Ok(BackendResponse {
                card: Self::error_card(format!("model returned {status}\n\n{}", excerpt(&text))),
                raw_output: Some(text),
                metadata: BackendMetadata {
                    backend: "openai_compat".into(),
                },
            });
        }

        let content = content_from_response(&text)?;
        self.remember(&session_id, content.clone()).await;

        let card = parse_op(&content).unwrap_or_else(|error| {
            Self::error_card(format!("{}\n\n{}", error, excerpt(&content)))
        });

        Ok(BackendResponse {
            card,
            raw_output: Some(text),
            metadata: BackendMetadata {
                backend: "openai_compat".into(),
            },
        })
    }

    fn capabilities(&self) -> BackendInfo {
        BackendInfo {
            name: "openai_compat".into(),
            streaming: false,
            patches: true,
            reasoning: true,
            can_read_project: false,
            can_use_tools: false,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct ChatRequest {
    model: String,
    messages: Vec<Message>,
    temperature: f32,
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    response_format: Option<serde_json::Value>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Message {
    role: String,
    content: String,
}

impl Message {
    fn system(content: String) -> Self {
        Self {
            role: "system".into(),
            content,
        }
    }

    fn user(content: String) -> Self {
        Self {
            role: "user".into(),
            content,
        }
    }

    fn assistant(content: String) -> Self {
        Self {
            role: "assistant".into(),
            content,
        }
    }
}

fn system_prompt() -> String {
    [
        "You are Pair Agent API.",
        "Return exactly one JSON object.",
        "Return one Pair op, not a card.",
        "Do not return prose, markdown, code fences, or a plan.",
        "Use one of: hypothesis, finding, patch, choice, summary, error.",
        "Patch only when action is Fix or Retry.",
        "If uncertain, return hypothesis.",
        "All file paths must be relative to cwd.",
    ]
    .join("\n")
}

fn user_prompt(req: &BackendRequest) -> String {
    json!({
        "api": agent_api(),
        "session": {
            "id": req.session.id,
            "prompt": req.session.prompt,
            "card_count": req.session.card_count,
            "last_card": req.session.last_card
        },
        "action": action_label(&req.action),
        "context": req.context,
        "contract": req.card_contract
    })
    .to_string()
}

fn agent_api() -> serde_json::Value {
    json!({
        "hypothesis": {
            "required": ["op", "title", "claim"],
            "optional": ["evidence", "next"]
        },
        "finding": {
            "required": ["op", "title", "finding"],
            "optional": ["location", "annotation"]
        },
        "patch": {
            "required": ["op", "title", "explanation", "patches"],
            "patch": ["file", "diff", "explanation"]
        },
        "choice": {
            "required": ["op", "title", "question", "options"]
        },
        "summary": {
            "required": ["op", "title", "summary", "changed_files"]
        },
        "error": {
            "required": ["op", "title", "message"]
        }
    })
}

fn action_label(action: &BackendAction) -> String {
    match action {
        BackendAction::Start => "start".into(),
        BackendAction::User(action) => format!("{action:?}"),
    }
}

fn content_from_response(text: &str) -> Result<String> {
    let value = serde_json::from_str::<serde_json::Value>(text)?;
    let content = value
        .get("choices")
        .and_then(|choices| choices.get(0))
        .and_then(|choice| choice.get("message"))
        .and_then(|message| message.get("content"))
        .and_then(|content| content.as_str())
        .ok_or_else(|| anyhow!("response missing choices[0].message.content"))?;

    Ok(content.into())
}

fn parse_op(content: &str) -> Result<Card> {
    let op = serde_json::from_str::<AgentOp>(content.trim())?;

    Ok(op.into_card("c_agent"))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_chat_content() {
        let text = r#"{"choices":[{"message":{"content":"{\"op\":\"hypothesis\",\"title\":\"T\",\"claim\":\"C\"}"}}]}"#;
        let content = content_from_response(text).unwrap();
        let card = parse_op(&content).unwrap();

        assert!(matches!(card, Card::Hypothesis(_)));
    }
}
