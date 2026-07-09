use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{Result, anyhow};
use pair_backends::{BackendAction, BackendAdapter, BackendRequest, CardContract, SessionSnapshot};
use pair_patch::PatchValidator;
use pair_protocol::{
    Action, ActionResult, Card, ContextBundle, ErrorCard, PatchApplyResult, StartSessionParams,
    StartSessionResult, SummaryCard,
};

use crate::session::Session;
use crate::state::{NextState, SessionState};

pub struct Engine {
    backend: Arc<dyn BackendAdapter>,
    sessions: HashMap<String, Session>,
}

impl Engine {
    pub fn new(backend: Arc<dyn BackendAdapter>) -> Self {
        Self {
            backend,
            sessions: HashMap::new(),
        }
    }

    pub async fn start(&mut self, params: StartSessionParams) -> Result<StartSessionResult> {
        let mut session = Session::new(params.clone());
        let context = ContextBundle::from_start(params);
        let request = self.request(&session, BackendAction::Start, context);
        let response = self.backend.next_card(request).await?;
        self.add_usage(&mut session, &response.metadata.token_usage);

        let card = self.accept_card(&mut session, response.card)?;
        let session_id = session.id.clone();
        let token_usage = session.token_usage.clone();

        self.sessions.insert(session_id.clone(), session);

        Ok(StartSessionResult {
            session_id,
            card,
            token_usage,
        })
    }

    pub async fn action(&mut self, session_id: &str, action: Action) -> Result<ActionResult> {
        let mut session = self.take_session(session_id)?;
        let result = self.action_taken(session_id, &mut session, action).await;

        self.sessions.insert(session_id.into(), session);

        result
    }

    async fn action_taken(
        &self,
        session_id: &str,
        session: &mut Session,
        action: Action,
    ) -> Result<ActionResult> {
        let state = session.state.next(&action)?;
        let previous_state = session.state.clone();

        if action == Action::Stop {
            session.state = SessionState::Finished;
            let card = session.stop_card();
            let token_usage = session.token_usage.clone();

            session.cards.push(card.clone());

            return Ok(ActionResult {
                session_id: session_id.into(),
                card,
                token_usage,
            });
        }

        let context = session.context.clone();
        let request = self.request(&session, BackendAction::User(action), context);

        session.state = SessionState::Thinking;

        let response = match self.backend.next_card(request).await {
            Ok(response) => response,
            Err(error) => {
                session.state = previous_state;

                return Err(error);
            }
        };

        self.add_usage(session, &response.metadata.token_usage);

        let card = self.accept_card_with_state(session, response.card, state)?;
        let token_usage = session.token_usage.clone();

        Ok(ActionResult {
            session_id: session_id.into(),
            card,
            token_usage,
        })
    }

    pub fn apply_result(&mut self, result: PatchApplyResult) -> Result<ActionResult> {
        let mut session = self.take_session(&result.session_id)?;
        let session_id = result.session_id.clone();
        let output = self.apply_result_taken(&mut session, result);

        self.sessions.insert(session_id, session);

        output
    }

    fn apply_result_taken(
        &self,
        session: &mut Session,
        result: PatchApplyResult,
    ) -> Result<ActionResult> {
        session.state.require_patch()?;

        if result.accepted {
            session.accepted_patches.extend(result.patch_ids.clone());
            session.state = SessionState::Summary;
        } else {
            session.rejected_patches.extend(result.patch_ids.clone());
            session.state = SessionState::CardShown;
        }

        let card = session.apply_summary(&result);
        let token_usage = session.token_usage.clone();

        session.cards.push(card.clone());

        let session_id = result.session_id;

        Ok(ActionResult {
            session_id,
            card,
            token_usage,
        })
    }

    pub fn get(&self, session_id: &str) -> Option<&Session> {
        self.sessions.get(session_id)
    }

    fn request(
        &self,
        session: &Session,
        action: BackendAction,
        context: ContextBundle,
    ) -> BackendRequest {
        BackendRequest {
            session: SessionSnapshot {
                id: session.id.clone(),
                prompt: session.original_prompt.clone(),
                card_count: session.cards.len(),
                last_card: session.cards.last().cloned(),
                last_summary: session.cards.last().map(card_summary),
            },
            action,
            context,
            card_contract: CardContract::default(),
        }
    }

    fn accept_card(&self, session: &mut Session, card: Card) -> Result<Card> {
        self.accept_card_with_state(session, card, NextState::Any)
    }

    fn accept_card_with_state(
        &self,
        session: &mut Session,
        card: Card,
        next_state: NextState,
    ) -> Result<Card> {
        let card = match validate_backend_card(&card, &next_state) {
            Ok(()) => card,
            Err(error) => rejected_card(session, error),
        };

        session.state = SessionState::from_card(&card);
        session.cards.push(card.clone());

        Ok(card)
    }

    fn take_session(&mut self, session_id: &str) -> Result<Session> {
        self.sessions
            .remove(session_id)
            .ok_or_else(|| anyhow!("unknown session {session_id}"))
    }

    fn add_usage(&self, session: &mut Session, usage: &Option<pair_protocol::TokenUsage>) {
        if let Some(usage) = usage {
            session.token_usage.add(usage);
        }
    }
}

fn validate_backend_card(card: &Card, next_state: &NextState) -> Result<()> {
    validate_one_card(card)?;
    PatchValidator::validate_card(card)?;
    next_state.validate(card)?;

    Ok(())
}

fn validate_one_card(card: &Card) -> Result<()> {
    if card.id().trim().is_empty() {
        return Err(anyhow!("card id is empty"));
    }

    if matches!(card, Card::Choice(_)) {
        return Ok(());
    }

    if card.actions().is_empty() {
        return Err(anyhow!("card has no actions"));
    }

    Ok(())
}

fn rejected_card(session: &Session, error: anyhow::Error) -> Card {
    Card::Error(ErrorCard {
        id: session.next_card_id("rejected"),
        title: "Backend card rejected".into(),
        message: error.to_string(),
        actions: vec![Action::Retry, Action::EditPrompt, Action::Stop],
    })
}

fn card_summary(card: &Card) -> String {
    match card {
        Card::Hypothesis(card) => format!("hypothesis: {}", card.claim),
        Card::Finding(card) => format!("finding: {}", card.finding),
        Card::Patch(card) => format!("patch: {}", card.explanation),
        Card::Choice(card) => format!("choice: {}", card.question),
        Card::Summary(card) => format!("summary: {}", card.summary),
        Card::Error(card) => format!("error: {}", card.message),
    }
}

impl Session {
    fn stop_card(&self) -> Card {
        Card::Summary(SummaryCard {
            id: self.next_card_id("stop"),
            title: "Stopped".into(),
            summary: "Session stopped.".into(),
            changed_files: vec![],
            next_actions: vec![],
        })
    }

    fn apply_summary(&self, result: &PatchApplyResult) -> Card {
        let summary = if result.accepted {
            "Patch accepted.".into()
        } else {
            "Patch rejected.".into()
        };

        Card::Summary(SummaryCard {
            id: self.next_card_id("summary"),
            title: "Summary".into(),
            summary,
            changed_files: result.changed_files.clone(),
            next_actions: vec![Action::Next, Action::RunCheck, Action::Stop],
        })
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicBool, Ordering};

    use async_trait::async_trait;
    use pair_backends::{
        BackendAction, BackendAdapter, BackendMetadata, BackendRequest, BackendResponse,
        MockBackend,
    };
    use pair_protocol::{
        BackendInfo, Cursor, FilePatch, FindingCard, HypothesisCard, Mode, PatchCard,
    };

    use super::*;

    fn params() -> StartSessionParams {
        StartSessionParams {
            cwd: PathBuf::from("/tmp/project"),
            file: PathBuf::from("src/work.ts"),
            cursor: Cursor { line: 1, column: 1 },
            selection: None,
            prompt: "payload is empty".into(),
            mode: Mode::Auto,
            buffer_text: "placeholder".into(),
            diagnostics: vec![],
        }
    }

    #[tokio::test]
    async fn starts_with_hypothesis() {
        let backend = Arc::new(MockBackend);
        let mut engine = Engine::new(backend);

        let result = engine.start(params()).await.unwrap();

        assert!(matches!(result.card, Card::Hypothesis(_)));
    }

    #[tokio::test]
    async fn rejects_apply_before_patch() {
        let backend = Arc::new(MockBackend);
        let mut engine = Engine::new(backend);
        let result = engine.start(params()).await.unwrap();

        let error = engine
            .action(&result.session_id, Action::Apply)
            .await
            .unwrap_err();

        assert!(error.to_string().contains("invalid action"));
    }

    #[tokio::test]
    async fn fix_shows_patch_then_apply_summarizes() {
        let backend = Arc::new(MockBackend);
        let mut engine = Engine::new(backend);
        let start = engine.start(params()).await.unwrap();
        let patch = engine.action(&start.session_id, Action::Fix).await.unwrap();

        assert!(matches!(patch.card, Card::Patch(_)));

        let result = PatchApplyResult {
            session_id: start.session_id,
            card_id: patch.card.id().into(),
            accepted: true,
            patch_ids: vec!["p_1".into()],
            changed_files: vec![PathBuf::from("src/work.ts")],
            error: None,
        };

        let summary = engine.apply_result(result).unwrap();

        assert!(matches!(summary.card, Card::Summary(_)));
    }

    #[tokio::test]
    async fn keeps_session_after_backend_error() {
        let backend = Arc::new(FlakyBackend::default());
        let mut engine = Engine::new(backend);
        let start = engine.start(params()).await.unwrap();
        let error = engine
            .action(&start.session_id, Action::Follow)
            .await
            .unwrap_err();

        assert!(error.to_string().contains("backend failed"));

        let next = engine.action(&start.session_id, Action::Why).await.unwrap();

        assert!(matches!(next.card, Card::Finding(_)));
    }

    #[tokio::test]
    async fn converts_bad_patch_to_error_card() {
        let backend = Arc::new(BadPatchBackend);
        let mut engine = Engine::new(backend);
        let start = engine.start(params()).await.unwrap();
        let result = engine.action(&start.session_id, Action::Fix).await.unwrap();

        let Card::Error(card) = result.card else {
            panic!("expected error card");
        };

        assert!(card.message.contains("diff has no hunks"));

        let retry = engine
            .action(&start.session_id, Action::Retry)
            .await
            .unwrap();

        assert!(matches!(retry.card, Card::Error(_)));
    }

    #[derive(Default)]
    struct FlakyBackend {
        failed: AtomicBool,
    }

    struct BadPatchBackend;

    #[async_trait]
    impl BackendAdapter for FlakyBackend {
        async fn next_card(&self, req: BackendRequest) -> Result<BackendResponse> {
            if matches!(req.action, BackendAction::User(Action::Follow))
                && !self.failed.swap(true, Ordering::SeqCst)
            {
                return Err(anyhow!("backend failed"));
            }

            let card = match req.action {
                BackendAction::Start => Card::Hypothesis(HypothesisCard {
                    id: "c_1".into(),
                    title: "Start".into(),
                    claim: "Initial claim.".into(),
                    evidence: None,
                    next_move: None,
                    actions: vec![Action::Follow, Action::Why, Action::Stop],
                }),
                _ => Card::Finding(FindingCard {
                    id: "c_2".into(),
                    title: "Recovered".into(),
                    finding: "Session still works.".into(),
                    location: None,
                    annotation: None,
                    actions: vec![Action::Stop],
                }),
            };

            Ok(BackendResponse {
                card,
                raw_output: None,
                metadata: BackendMetadata {
                    backend: "flaky".into(),
                    token_usage: None,
                },
            })
        }

        fn capabilities(&self) -> BackendInfo {
            BackendInfo {
                name: "flaky".into(),
                streaming: false,
                patches: false,
                reasoning: false,
                can_read_project: false,
                can_use_tools: false,
            }
        }
    }

    #[async_trait]
    impl BackendAdapter for BadPatchBackend {
        async fn next_card(&self, req: BackendRequest) -> Result<BackendResponse> {
            let card = match req.action {
                BackendAction::Start => Card::Hypothesis(HypothesisCard {
                    id: "c_1".into(),
                    title: "Start".into(),
                    claim: "Initial claim.".into(),
                    evidence: None,
                    next_move: None,
                    actions: vec![Action::Fix, Action::Stop],
                }),
                _ => Card::Patch(PatchCard {
                    id: "c_patch".into(),
                    title: "Bad patch".into(),
                    explanation: "Invalid patch.".into(),
                    patches: vec![FilePatch {
                        id: "p_1".into(),
                        file: "src/work.ts".into(),
                        diff: "not a unified diff".into(),
                        explanation: "Broken.".into(),
                    }],
                    actions: vec![Action::Apply, Action::Retry, Action::Stop],
                }),
            };

            Ok(BackendResponse {
                card,
                raw_output: None,
                metadata: BackendMetadata {
                    backend: "bad_patch".into(),
                    token_usage: None,
                },
            })
        }

        fn capabilities(&self) -> BackendInfo {
            BackendInfo {
                name: "bad_patch".into(),
                streaming: false,
                patches: true,
                reasoning: false,
                can_read_project: false,
                can_use_tools: false,
            }
        }
    }
}
