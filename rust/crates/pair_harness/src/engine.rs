use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{Result, anyhow};
use pair_backends::{BackendAction, BackendAdapter, BackendRequest, CardContract, SessionSnapshot};
use pair_patch::PatchValidator;
use pair_protocol::{
    Action, ActionResult, Card, ContextBundle, PatchApplyResult, StartSessionParams,
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
        let card = self.accept_card(&mut session, response.card)?;
        let session_id = session.id.clone();

        self.sessions.insert(session_id.clone(), session);

        Ok(StartSessionResult { session_id, card })
    }

    pub async fn action(&mut self, session_id: &str, action: Action) -> Result<ActionResult> {
        let mut session = self.take_session(session_id)?;
        let state = session.state.next(&action)?;

        if action == Action::Stop {
            session.state = SessionState::Finished;
            let card = session.stop_card();
            session.cards.push(card.clone());

            self.sessions.insert(session_id.into(), session);

            return Ok(ActionResult {
                session_id: session_id.into(),
                card,
            });
        }

        let context = session.context.clone();
        let request = self.request(&session, BackendAction::User(action), context);

        session.state = SessionState::Thinking;

        let response = self.backend.next_card(request).await?;
        let card = self.accept_card_with_state(&mut session, response.card, state)?;

        self.sessions.insert(session_id.into(), session);

        Ok(ActionResult {
            session_id: session_id.into(),
            card,
        })
    }

    pub fn apply_result(&mut self, result: PatchApplyResult) -> Result<ActionResult> {
        let mut session = self.take_session(&result.session_id)?;

        session.state.require_patch()?;

        if result.accepted {
            session.accepted_patches.extend(result.patch_ids.clone());
            session.state = SessionState::Summary;
        } else {
            session.rejected_patches.extend(result.patch_ids.clone());
            session.state = SessionState::CardShown;
        }

        let card = session.apply_summary(&result);

        session.cards.push(card.clone());

        let session_id = result.session_id;
        self.sessions.insert(session_id.clone(), session);

        Ok(ActionResult { session_id, card })
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
        validate_one_card(&card)?;
        PatchValidator::validate_card(&card)?;
        next_state.validate(&card)?;

        session.state = SessionState::from_card(&card);
        session.cards.push(card.clone());

        Ok(card)
    }

    fn take_session(&mut self, session_id: &str) -> Result<Session> {
        self.sessions
            .remove(session_id)
            .ok_or_else(|| anyhow!("unknown session {session_id}"))
    }
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

    use pair_backends::MockBackend;
    use pair_protocol::{Cursor, Mode};

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
}
