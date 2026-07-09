use anyhow::Result;
use async_trait::async_trait;
use pair_protocol::{
    Action, BackendInfo, Card, ErrorCard, FilePatch, FindingCard, HypothesisCard, PatchCard,
    SummaryCard,
};

use crate::{BackendAction, BackendAdapter, BackendMetadata, BackendRequest, BackendResponse};

#[derive(Default)]
pub struct MockBackend;

#[async_trait]
impl BackendAdapter for MockBackend {
    async fn next_card(&self, req: BackendRequest) -> Result<BackendResponse> {
        let card = match req.action {
            BackendAction::Start => first_card(),
            BackendAction::User(Action::Follow) => finding_card(),
            BackendAction::User(Action::Why) => why_card(),
            BackendAction::User(Action::Fix) => patch_card(req.context.file.display().to_string()),
            BackendAction::User(Action::OtherLead) => other_card(),
            BackendAction::User(Action::Retry) => {
                patch_card(req.context.file.display().to_string())
            }
            BackendAction::User(Action::RunCheck) => check_card(),
            BackendAction::User(Action::Next) => other_card(),
            BackendAction::User(Action::Stop) => stop_card(),
            BackendAction::User(action) => unsupported_card(action),
        };

        Ok(BackendResponse {
            card,
            raw_output: None,
            metadata: BackendMetadata {
                backend: "mock".into(),
            },
        })
    }

    fn capabilities(&self) -> BackendInfo {
        Self::info()
    }
}

impl MockBackend {
    pub fn info() -> BackendInfo {
        BackendInfo {
            name: "mock".into(),
            streaming: false,
            patches: true,
            reasoning: true,
            can_read_project: false,
            can_use_tools: false,
        }
    }

    pub fn first_card() -> Result<Card> {
        Ok(first_card())
    }
}

fn first_card() -> Card {
    Card::Hypothesis(HypothesisCard {
        id: "c_1".into(),
        title: "Payload may be skipped".into(),
        claim: "This path can return before the payload is built.".into(),
        evidence: None,
        next_move: None,
        actions: vec![
            Action::Follow,
            Action::Why,
            Action::Fix,
            Action::OtherLead,
            Action::Stop,
        ],
    })
}

fn finding_card() -> Card {
    Card::Finding(FindingCard {
        id: "c_2".into(),
        title: "Early return confirmed".into(),
        finding: "The selected path leaves before payload construction.".into(),
        location: None,
        annotation: Some("payload construction is skipped here".into()),
        actions: vec![
            Action::Open,
            Action::Why,
            Action::Fix,
            Action::OtherLead,
            Action::Stop,
        ],
    })
}

fn why_card() -> Card {
    Card::Finding(FindingCard {
        id: "c_why".into(),
        title: "Why this matters".into(),
        finding: "Callers later read body.data, but this branch does not create body.".into(),
        location: None,
        annotation: None,
        actions: vec![Action::Follow, Action::Fix, Action::OtherLead, Action::Stop],
    })
}

fn other_card() -> Card {
    Card::Hypothesis(HypothesisCard {
        id: "c_other".into(),
        title: "Caller may drop payload".into(),
        claim: "A caller may replace the response before it reaches this code.".into(),
        evidence: None,
        next_move: None,
        actions: vec![Action::Follow, Action::Why, Action::Fix, Action::Stop],
    })
}

fn patch_card(file: String) -> Card {
    Card::Patch(PatchCard {
        id: "c_patch".into(),
        title: "Guard payload shape".into(),
        explanation: "Ensure the empty branch returns the same payload shape.".into(),
        patches: vec![FilePatch {
            id: "p_1".into(),
            file: file.into(),
            diff: "@@ -1,1 +1,1 @@\n-placeholder\n+payload = payload or {}\n".into(),
            explanation: "Keeps body present for callers.".into(),
        }],
        actions: vec![
            Action::Apply,
            Action::Retry,
            Action::EditPrompt,
            Action::Stop,
        ],
    })
}

fn check_card() -> Card {
    Card::Finding(FindingCard {
        id: "c_check".into(),
        title: "Check needed".into(),
        finding: "Run the project check command from the editor or shell.".into(),
        location: None,
        annotation: None,
        actions: vec![Action::Next, Action::Stop],
    })
}

fn stop_card() -> Card {
    Card::Summary(SummaryCard {
        id: "c_stop".into(),
        title: "Stopped".into(),
        summary: "Session stopped without applying a patch.".into(),
        changed_files: vec![],
        next_actions: vec![],
    })
}

fn unsupported_card(action: Action) -> Card {
    Card::Error(ErrorCard {
        id: "c_error".into(),
        title: "Unsupported action".into(),
        message: format!("Mock backend cannot handle {action:?}."),
        actions: vec![Action::Retry, Action::Stop],
    })
}
