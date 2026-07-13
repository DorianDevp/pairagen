pub mod claude_app;
pub mod codex_app;
pub mod generic;
pub mod mock;
pub mod ollama;
pub mod stdio_agent;
pub mod stream;

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use pair_protocol::{
    Action, BackendInfo, Card, CardKind, ContextBundle, ErrorCard, MAX_CHANGED_LINES,
    MAX_HUNKS_PER_PATCH, MAX_PATCH_FILES, Mode, TokenUsage,
};
use serde::Serialize;
use serde_json::json;

pub use claude_app::*;
pub use codex_app::*;
pub use generic::*;
pub use mock::*;
pub use ollama::*;
pub use stdio_agent::*;
pub use stream::*;

#[async_trait]
pub trait BackendAdapter: Send + Sync {
    async fn next_card(&self, req: BackendRequest) -> Result<BackendResponse>;

    async fn next_card_with_progress(
        &self,
        req: BackendRequest,
        _progress: Option<ProgressReporter>,
    ) -> Result<BackendResponse> {
        self.next_card(req).await
    }

    fn capabilities(&self) -> BackendInfo;
}

pub type ProgressReporter = Arc<dyn Fn(BackendProgress) + Send + Sync>;

#[derive(Clone, Debug, Serialize)]
pub struct BackendProgress {
    pub session_id: String,
    pub phase: String,
    pub message: String,
}

#[derive(Clone, Debug)]
pub struct BackendRequest {
    pub session: SessionSnapshot,
    pub action: BackendAction,
    pub context: ContextBundle,
    pub card_contract: CardContract,
}

pub fn backend_context(context: &ContextBundle) -> serde_json::Value {
    let artifacts = context
        .artifacts
        .iter()
        .map(|artifact| {
            json!({
                "file": artifact.file,
                "start_line": artifact.start_line,
                "end_line": artifact.end_line,
                "kind": artifact.kind,
                "reason": artifact.reason,
                "text": artifact.text,
            })
        })
        .collect::<Vec<_>>();

    json!({
        "cwd": context.cwd,
        "file": context.file,
        "cursor": context.cursor,
        "selection": context.selection,
        "buffer_text": context.buffer_text,
        "buffer_start_line": context.buffer_start_line,
        "diagnostics": context.diagnostics,
        "artifacts": artifacts,
    })
}

#[derive(Clone, Debug)]
pub enum BackendAction {
    Start,
    User(Action),
    Reply(String),
    ContractRetry(String),
}

#[derive(Clone, Debug, Serialize)]
pub struct CardContract {
    pub one_card_only: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected_kind: Option<CardKind>,
    pub allow_goal_completion: bool,
    pub max_body_chars: usize,
    pub max_patch_files: usize,
    pub max_hunks_per_patch: usize,
    pub max_changed_lines: usize,
}

impl Default for CardContract {
    fn default() -> Self {
        Self {
            one_card_only: true,
            expected_kind: None,
            allow_goal_completion: false,
            max_body_chars: 1_200,
            max_patch_files: MAX_PATCH_FILES,
            max_hunks_per_patch: MAX_HUNKS_PER_PATCH,
            max_changed_lines: MAX_CHANGED_LINES,
        }
    }
}

pub fn enforce_card_contract(
    card: Card,
    contract: &CardContract,
    backend: &str,
    raw_output: &str,
) -> Card {
    let Some(expected_kind) = contract.expected_kind else {
        return card;
    };

    if matches!(card, Card::Error(_) | Card::Deny(_))
        || card.kind() == expected_kind
        || (contract.allow_goal_completion && matches!(card, Card::Summary(_)))
    {
        return card;
    }

    let received_kind = card.kind();
    let raw_output = excerpt(raw_output, contract.max_body_chars);
    let mut message = format!(
        "{backend} returned a {received_kind:?} card, but this request requires a {expected_kind:?} card."
    );

    if !raw_output.is_empty() {
        message.push_str("\n\nRaw backend response:\n");
        message.push_str(&raw_output);
    }

    Card::Error(ErrorCard {
        id: "c_backend_contract_error".into(),
        title: "Backend returned the wrong card type".into(),
        message,
        actions: vec![Action::Retry, Action::EditPrompt, Action::Stop],
    })
}

pub fn excerpt(text: &str, max_chars: usize) -> String {
    let text = text.trim();
    let mut result = text.chars().take(max_chars).collect::<String>();

    if text.chars().count() > max_chars {
        result.push_str("\n...");
    }

    result
}

#[derive(Clone, Debug)]
pub struct BackendResponse {
    pub card: Card,
    pub raw_output: Option<String>,
    pub metadata: BackendMetadata,
}

#[derive(Clone, Debug)]
pub struct BackendMetadata {
    pub backend: String,
    pub model: Option<String>,
    pub token_usage: Option<TokenUsage>,
    pub activities: Vec<String>,
    pub attempts: Vec<pair_protocol::AgentAttempt>,
}

pub fn estimate_tokens(text: &str) -> usize {
    let chars = text.chars().count();
    let words = text.split_whitespace().count();
    let estimate = (chars / 4).max(words);

    estimate.max(1)
}

#[derive(Clone, Debug, Serialize)]
pub struct SessionSnapshot {
    pub id: String,
    pub prompt: String,
    pub completed_steps: Vec<String>,
    pub known_observations: Vec<String>,
    pub mode: Mode,
    pub card_count: usize,
    pub last_card: Option<Card>,
    pub last_summary: Option<String>,
}

#[cfg(test)]
pub(crate) fn test_request() -> BackendRequest {
    BackendRequest {
        session: SessionSnapshot {
            id: "s_1".into(),
            prompt: "payload is empty".into(),
            completed_steps: vec![],
            known_observations: vec![],
            mode: Mode::Auto,
            card_count: 0,
            last_card: None,
            last_summary: None,
        },
        action: BackendAction::Start,
        context: ContextBundle {
            cwd: "/tmp/project".into(),
            file: "src/main.rs".into(),
            cursor: pair_protocol::Cursor { line: 1, column: 1 },
            selection: None,
            buffer_text: "fn main() {}".into(),
            buffer_start_line: 1,
            diagnostics: vec![],
            hints: vec![],
            artifacts: vec![],
            report: None,
        },
        card_contract: CardContract::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pair_protocol::{HypothesisCard, SummaryCard};

    fn hypothesis() -> Card {
        Card::Hypothesis(HypothesisCard {
            id: "c_hypothesis".into(),
            title: "Hypothesis".into(),
            claim: "The response has the wrong type.".into(),
            evidence: None,
            next_move: None,
            actions: vec![Action::Fix],
        })
    }

    #[test]
    fn rejects_non_patch_when_patch_is_required() {
        let contract = CardContract {
            expected_kind: Some(CardKind::Patch),
            ..CardContract::default()
        };
        let card =
            enforce_card_contract(hypothesis(), &contract, "Codex", "{\"op\":\"hypothesis\"}");

        let Card::Error(error) = card else {
            panic!("expected contract error card");
        };

        assert!(error.message.contains("Hypothesis card"));
        assert!(error.message.contains("Patch card"));
        assert!(error.message.contains("Raw backend response"));
    }

    #[test]
    fn allows_the_required_card_type() {
        let contract = CardContract {
            expected_kind: Some(CardKind::Hypothesis),
            ..CardContract::default()
        };

        assert!(matches!(
            enforce_card_contract(hypothesis(), &contract, "Codex", "{}"),
            Card::Hypothesis(_)
        ));
    }

    #[test]
    fn allows_summary_for_goal_completion_contract() {
        let contract = CardContract {
            expected_kind: Some(CardKind::Patch),
            allow_goal_completion: true,
            ..CardContract::default()
        };
        let summary = Card::Summary(SummaryCard {
            id: "c_done".into(),
            title: "Goal complete".into(),
            summary: "The goal is resolved.".into(),
            changed_files: vec![],
            next_actions: vec![Action::Stop],
        });

        assert!(matches!(
            enforce_card_contract(summary, &contract, "test", "{}"),
            Card::Summary(_)
        ));
    }

    #[test]
    fn backend_context_excludes_optimizer_telemetry() {
        let context = ContextBundle {
            cwd: "/tmp/project".into(),
            file: "src/main.rs".into(),
            cursor: pair_protocol::Cursor { line: 1, column: 1 },
            selection: None,
            buffer_text: "fn main() {}".into(),
            buffer_start_line: 1,
            diagnostics: vec![],
            hints: vec![],
            artifacts: vec![pair_protocol::ContextArtifact {
                file: "src/user.rs".into(),
                start_line: 3,
                end_line: 3,
                kind: pair_protocol::ContextArtifactKind::Definition,
                reason: "definition".into(),
                text: "struct User;".into(),
                estimated_tokens: 9,
                score: 240,
            }],
            report: Some(pair_protocol::ContextReport {
                enabled: true,
                candidate_count: 99,
                ..Default::default()
            }),
        };

        let value = backend_context(&context);

        assert!(value.get("report").is_none());
        assert!(value.get("hints").is_none());
        assert_eq!(value["artifacts"][0]["text"], "struct User;");
        assert!(value["artifacts"][0].get("score").is_none());
    }
}
