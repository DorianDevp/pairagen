use anyhow::Result;
use async_trait::async_trait;
use loopbiotic_protocol::{
    Action, BackendInfo, Card, ErrorCard, FilePatch, FindingCard, HypothesisCard, PatchCard,
    SummaryCard, TokenUsage,
};
use serde_json::to_string;

use crate::{
    BackendAction, BackendAdapter, BackendIdentity, BackendMetadata, BackendRequest,
    BackendResponse, estimate_tokens,
};

#[derive(Default)]
pub struct MockBackend;

#[async_trait]
impl BackendAdapter for MockBackend {
    async fn next_card(&self, req: BackendRequest) -> Result<BackendResponse> {
        let card = if req.card_contract.allow_goal_completion
            && req.card_contract.expected_kind == Some(loopbiotic_protocol::CardKind::Finding)
        {
            why_card()
        } else if req.card_contract.allow_goal_completion {
            goal_card(&req)
        } else {
            match req.action {
                BackendAction::Start => first_card(),
                BackendAction::User(Action::Follow) => finding_card(),
                BackendAction::User(Action::Why) => why_card(),
                BackendAction::User(Action::Fix) => patch_card(&req),
                BackendAction::User(Action::OtherLead) => other_card(),
                BackendAction::User(Action::Retry) => patch_card(&req),
                BackendAction::User(Action::RunCheck) => check_card(),
                BackendAction::User(Action::Next) => next_step_card(),
                BackendAction::User(Action::Stop) => stop_card(),
                BackendAction::User(action) => unsupported_card(action),
                BackendAction::Reply(text) => reply_card(text),
                BackendAction::ContractRetry(_) => finding_card(),
                BackendAction::LocationGranted => patch_card(&req),
            }
        };

        Ok(BackendResponse {
            metadata: BackendMetadata {
                backend: "mock".into(),
                model: None,
                token_usage: Some(TokenUsage::estimated(
                    estimate_tokens(&req.session.prompt),
                    estimate_tokens(&to_string(&card).unwrap_or_default()),
                )),
                activities: vec![],
                attempts: vec![],
            },
            card,
            raw_output: None,
        })
    }

    /// Fixed identity so the warmup identity contract is testable end to end
    /// without a real model backend.
    async fn identity(&self) -> BackendIdentity {
        BackendIdentity {
            backend: "mock".into(),
            model: Some("mock-model".into()),
            models: vec!["mock-model".into(), "mock-mini".into()],
        }
    }

    fn capabilities(&self) -> BackendInfo {
        Self::info()
    }
}

fn goal_card(req: &BackendRequest) -> Card {
    if req.session.completed_steps.len() >= 2 {
        Card::Summary(SummaryCard {
            id: "c_complete".into(),
            title: "Goal complete".into(),
            summary: "The payload and its caller now preserve the required shape.".into(),
            changed_files: vec!["src/work.ts".into()],
            next_actions: vec![Action::RunCheck, Action::Stop],
        })
    } else {
        let mut card = patch_card(req);
        if req.session.completed_steps.len() == 1
            && let Card::Patch(patch) = &mut card
        {
            patch.title = "Complete payload shape".into();
            patch.explanation = "Add the data member required by the caller.".into();
            patch.patches[0].explanation = "Provides the caller-visible data member.".into();
        }
        card
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

fn next_step_card() -> Card {
    Card::Finding(FindingCard {
        id: "c_next".into(),
        title: "Next goal step".into(),
        finding: "The payload producer is fixed; inspect the caller that consumes body.data next."
            .into(),
        location: Some(loopbiotic_protocol::Location {
            file: "src/caller.ts".into(),
            line: 1,
            column: 1,
        }),
        annotation: Some("This is the next unresolved part of the original goal.".into()),
        actions: vec![Action::Open, Action::Fix, Action::Stop],
    })
}

fn reply_card(text: String) -> Card {
    Card::Finding(FindingCard {
        id: "c_reply".into(),
        title: "Reply received".into(),
        finding: format!("You said: {text}"),
        location: None,
        annotation: None,
        actions: vec![Action::Follow, Action::Why, Action::Fix, Action::Stop],
    })
}

fn patch_card(req: &BackendRequest) -> Card {
    let old_line = req.context.buffer_text.lines().next().unwrap_or_default();
    let replacement = if old_line == "payload = payload or {}" {
        "payload = payload or { data = {} }"
    } else {
        "payload = payload or {}"
    };
    Card::Patch(PatchCard {
        id: "c_patch".into(),
        title: "Guard payload shape".into(),
        explanation: "Ensure the empty branch returns the same payload shape.".into(),
        warnings: vec![],
        goal_complete: req.card_contract.allow_goal_completion,
        patches: vec![FilePatch {
            id: "p_1".into(),
            file: relative_file(req).into(),
            diff: format!(
                "@@ -{0},1 +{0},1 @@\n-{old_line}\n+{replacement}\n",
                req.context.buffer_start_line
            ),
            explanation: "Keeps body present for callers.".into(),
        }],
        actions: vec![
            Action::Apply,
            Action::Why,
            Action::Retry,
            Action::EditPrompt,
            Action::Stop,
        ],
    })
}

fn relative_file(req: &BackendRequest) -> String {
    if !req.context.file.is_absolute() {
        return req.context.file.display().to_string();
    }

    if let Ok(file) = req.context.file.strip_prefix(&req.context.cwd) {
        return file.display().to_string();
    }

    req.context
        .file
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| "buffer".into())
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
