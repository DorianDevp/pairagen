use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{Result, anyhow};
use pair_backends::{
    BackendAction, BackendAdapter, BackendRequest, BackendResponse, CardContract, ProgressReporter,
    SessionSnapshot,
};
use pair_patch::{PatchCoherence, PatchNormalizer, PatchValidator};
use pair_protocol::{
    Action, ActionResult, Card, CardKind, ContextBundle, ErrorCard, Mode, PatchApplyResult,
    StartSessionParams, StartSessionResult, SummaryCard,
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
        self.start_with_progress(params, None).await
    }

    pub async fn start_with_progress(
        &mut self,
        params: StartSessionParams,
        progress: Option<ProgressReporter>,
    ) -> Result<StartSessionResult> {
        let mut session = Session::new(params.clone());
        let context = ContextBundle::from_start(params);
        let expected = expected_start_state(&session.mode);
        let request = self.request(&session, BackendAction::Start, context, &expected);
        let response = match self
            .backend
            .next_card_with_progress(request, progress)
            .await
        {
            Ok(response) => response,
            Err(error) => backend_failure_response(&session, error),
        };
        let turn_token_usage = response.metadata.token_usage.clone().unwrap_or_default();
        self.add_usage(&mut session, &response.metadata.token_usage);

        let card = self.accept_response(&mut session, response, expected)?;
        let session_id = session.id.clone();
        let token_usage = session.token_usage.clone();

        self.sessions.insert(session_id.clone(), session);

        Ok(StartSessionResult {
            session_id,
            card,
            token_usage,
            turn_token_usage,
        })
    }

    pub async fn action(&mut self, session_id: &str, action: Action) -> Result<ActionResult> {
        self.action_with_progress(session_id, action, None).await
    }

    pub async fn action_with_progress(
        &mut self,
        session_id: &str,
        action: Action,
        progress: Option<ProgressReporter>,
    ) -> Result<ActionResult> {
        let mut session = self.take_session(session_id)?;
        let result = self
            .action_taken(session_id, &mut session, action, progress)
            .await;

        self.sessions.insert(session_id.into(), session);

        result
    }

    pub async fn reply(&mut self, session_id: &str, text: String) -> Result<ActionResult> {
        self.reply_with_progress(session_id, text, None).await
    }

    pub async fn reply_with_progress(
        &mut self,
        session_id: &str,
        text: String,
        progress: Option<ProgressReporter>,
    ) -> Result<ActionResult> {
        let mut session = self.take_session(session_id)?;
        let result = self
            .reply_taken(session_id, &mut session, text, progress)
            .await;

        self.sessions.insert(session_id.into(), session);

        result
    }

    async fn action_taken(
        &self,
        session_id: &str,
        session: &mut Session,
        action: Action,
        progress: Option<ProgressReporter>,
    ) -> Result<ActionResult> {
        let state = session.state.next(&action)?;
        if action == Action::Stop {
            session.state = SessionState::Finished;
            let card = session.stop_card();
            let token_usage = session.token_usage.clone();

            session.cards.push(card.clone());

            return Ok(ActionResult {
                session_id: session_id.into(),
                card,
                token_usage,
                turn_token_usage: Default::default(),
            });
        }

        let context = session.context.clone();
        let request = self.request(&session, BackendAction::User(action), context, &state);

        session.state = SessionState::Thinking;

        let response = match self
            .backend
            .next_card_with_progress(request, progress)
            .await
        {
            Ok(response) => response,
            Err(error) => backend_failure_response(session, error),
        };

        let turn_token_usage = response.metadata.token_usage.clone().unwrap_or_default();
        self.add_usage(session, &response.metadata.token_usage);

        let card = self.accept_response(session, response, state)?;
        let token_usage = session.token_usage.clone();

        Ok(ActionResult {
            session_id: session_id.into(),
            card,
            token_usage,
            turn_token_usage,
        })
    }

    async fn reply_taken(
        &self,
        session_id: &str,
        session: &mut Session,
        text: String,
        progress: Option<ProgressReporter>,
    ) -> Result<ActionResult> {
        if text.trim().is_empty() {
            return Err(anyhow!("reply is empty"));
        }

        let context = session.context.clone();
        let expected = NextState::Any;
        let request = self.request(session, BackendAction::Reply(text), context, &expected);

        session.state = SessionState::Thinking;

        let response = match self
            .backend
            .next_card_with_progress(request, progress)
            .await
        {
            Ok(response) => response,
            Err(error) => backend_failure_response(session, error),
        };

        let turn_token_usage = response.metadata.token_usage.clone().unwrap_or_default();
        self.add_usage(session, &response.metadata.token_usage);

        let card = self.accept_response(session, response, expected)?;
        let token_usage = session.token_usage.clone();

        Ok(ActionResult {
            session_id: session_id.into(),
            card,
            token_usage,
            turn_token_usage,
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
        validate_apply_result(session, &result)?;

        if result.accepted {
            let completed_steps = completed_patch_steps(session);
            session.completed_steps.extend(completed_steps);
            session.accepted_patches.extend(result.patch_ids.clone());
        } else {
            session.rejected_patches.extend(result.patch_ids.clone());
        }
        session.state = SessionState::Summary;

        let card = session.apply_summary(&result);
        let token_usage = session.token_usage.clone();

        session.cards.push(card.clone());

        let session_id = result.session_id;

        Ok(ActionResult {
            session_id,
            card,
            token_usage,
            turn_token_usage: Default::default(),
        })
    }

    pub fn get(&self, session_id: &str) -> Option<&Session> {
        self.sessions.get(session_id)
    }

    pub fn update_context(&mut self, session_id: &str, context: ContextBundle) -> Result<()> {
        let session = self
            .sessions
            .get_mut(session_id)
            .ok_or_else(|| anyhow!("unknown session {session_id}"))?;
        session.context = context;

        Ok(())
    }

    fn request(
        &self,
        session: &Session,
        action: BackendAction,
        context: ContextBundle,
        expected: &NextState,
    ) -> BackendRequest {
        let expected_kind = expected_card_kind(session, &action, expected);

        BackendRequest {
            session: SessionSnapshot {
                id: session.id.clone(),
                prompt: session.original_prompt.clone(),
                completed_steps: session.completed_steps.clone(),
                mode: session.mode.clone(),
                card_count: session.cards.len(),
                last_card: session.cards.last().cloned(),
                last_summary: session.cards.last().map(card_summary),
            },
            action,
            context,
            card_contract: CardContract {
                expected_kind: Some(expected_kind),
                ..CardContract::default()
            },
        }
    }

    fn accept_response(
        &self,
        session: &mut Session,
        response: BackendResponse,
        next_state: NextState,
    ) -> Result<Card> {
        let mut received = response.card;
        let validation =
            PatchNormalizer::normalize_card(&mut received, &session.context).and_then(|()| {
                PatchCoherence::annotate(&mut received);
                validate_backend_card(&received, &next_state, &session.context)
            });
        let card = match validation {
            Ok(()) => received,
            Err(error) => rejected_card(session, &received, error, response.raw_output.as_deref()),
        };

        session.state = state_after_card(&card, &next_state);
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

fn validate_backend_card(
    card: &Card,
    next_state: &NextState,
    context: &ContextBundle,
) -> Result<()> {
    // Backend errors must reach the editor unchanged instead of being replaced by
    // a generic state-machine error such as "expected patch card".
    if matches!(card, Card::Error(_)) {
        return Ok(());
    }

    validate_one_card(card)?;
    PatchValidator::validate_card(card)?;
    validate_patch_target(card, context)?;
    PatchValidator::validate_card_against_context(card, context)?;
    next_state.validate(card)?;

    Ok(())
}

fn validate_patch_target(card: &Card, context: &ContextBundle) -> Result<()> {
    let Card::Patch(card) = card else {
        return Ok(());
    };
    let expected = if context.file.is_absolute() {
        context
            .file
            .strip_prefix(&context.cwd)
            .unwrap_or(&context.file)
    } else {
        &context.file
    };

    if let Some(patch) = card.patches.first()
        && patch.file != expected
    {
        return Err(anyhow!(
            "patch targets {}, but the accepted source location is {}; open that location before Fix",
            patch.file.display(),
            expected.display()
        ));
    }

    Ok(())
}

fn validate_one_card(card: &Card) -> Result<()> {
    if card.id().trim().is_empty() {
        return Err(anyhow!("card id is empty"));
    }

    match card {
        Card::Hypothesis(card) => {
            require_text("card title", &card.title)?;
            require_text("hypothesis claim", &card.claim)?;
            if let Some(location) = &card.evidence {
                validate_location(
                    &location.file,
                    location.line,
                    location.column,
                    "hypothesis evidence",
                )?;
            }
            if let Some(pair_protocol::NextMove::OpenLocation(location)) = &card.next_move {
                validate_location(
                    &location.file,
                    location.line,
                    location.column,
                    "hypothesis next move",
                )?;
            }
        }
        Card::Finding(card) => {
            require_text("card title", &card.title)?;
            require_text("finding", &card.finding)?;
            if let Some(location) = &card.location {
                validate_location(
                    &location.file,
                    location.line,
                    location.column,
                    "finding location",
                )?;
            }
        }
        Card::Patch(card) => {
            require_text("card title", &card.title)?;
            require_text("patch explanation", &card.explanation)?;
            for patch in &card.patches {
                require_text("file patch explanation", &patch.explanation)?;
            }
        }
        Card::Choice(card) => {
            require_text("card title", &card.title)?;
            require_text("choice question", &card.question)?;
            if card.options.is_empty() {
                return Err(anyhow!("choice card has no options"));
            }
            for option in &card.options {
                require_text("choice option id", &option.id)?;
                require_text("choice option label", &option.label)?;
            }
        }
        Card::Summary(card) => {
            require_text("card title", &card.title)?;
            require_text("summary", &card.summary)?;
        }
        Card::Error(card) => {
            require_text("card title", &card.title)?;
            require_text("error message", &card.message)?;
        }
    }

    if !matches!(card, Card::Choice(_) | Card::Summary(_)) && card.actions().is_empty() {
        return Err(anyhow!("card has no actions"));
    }

    Ok(())
}

fn require_text(field: &str, value: &str) -> Result<()> {
    if value.trim().is_empty() {
        return Err(anyhow!("{field} is empty"));
    }

    Ok(())
}

fn validate_location(
    file: &std::path::Path,
    line: usize,
    column: usize,
    label: &str,
) -> Result<()> {
    if file.as_os_str().is_empty() {
        return Err(anyhow!("{label} file is empty"));
    }
    if line == 0 || column == 0 {
        return Err(anyhow!("{label} line and column must start at 1"));
    }

    Ok(())
}

fn validate_apply_result(session: &Session, result: &PatchApplyResult) -> Result<()> {
    let Some(Card::Patch(card)) = session.cards.last() else {
        return Err(anyhow!("patch state has no current patch card"));
    };

    if result.card_id != card.id {
        return Err(anyhow!(
            "apply result targets card {}, but current patch card is {}",
            result.card_id,
            card.id
        ));
    }

    let expected_patch_ids = card
        .patches
        .iter()
        .map(|patch| patch.id.clone())
        .collect::<Vec<_>>();
    if result.patch_ids != expected_patch_ids {
        return Err(anyhow!(
            "apply result patch ids do not match the current patch card"
        ));
    }

    let expected_files = card
        .patches
        .iter()
        .map(|patch| patch.file.clone())
        .collect::<Vec<_>>();
    if result.accepted && result.changed_files != expected_files {
        return Err(anyhow!(
            "accepted apply result changed files do not match the current patch card"
        ));
    }
    if !result.accepted && !result.changed_files.is_empty() {
        return Err(anyhow!(
            "rejected apply result cannot contain changed files"
        ));
    }

    Ok(())
}

fn completed_patch_steps(session: &Session) -> Vec<String> {
    let Some(Card::Patch(card)) = session.cards.last() else {
        return vec![];
    };

    card.patches
        .iter()
        .map(|patch| format!("{}: {}", patch.file.display(), patch.explanation))
        .collect()
}

fn rejected_card(
    session: &Session,
    received: &Card,
    error: anyhow::Error,
    raw_output: Option<&str>,
) -> Card {
    let mut message = format!("{error}\nReceived card kind: {:?}.", received.kind());

    if let Some(raw_output) = raw_output
        .map(str::trim)
        .filter(|output| !output.is_empty())
    {
        let raw_output = raw_output.chars().take(1_200).collect::<String>();
        message.push_str("\n\nRaw backend response:\n");
        message.push_str(&raw_output);
    }

    Card::Error(ErrorCard {
        id: session.next_card_id("rejected"),
        title: "Backend card rejected".into(),
        message,
        actions: vec![Action::Retry, Action::EditPrompt, Action::Stop],
    })
}

fn backend_failure_response(session: &Session, error: anyhow::Error) -> BackendResponse {
    BackendResponse {
        card: Card::Error(ErrorCard {
            id: session.next_card_id("backend_error"),
            title: "Backend request failed".into(),
            message: format!("{error:#}"),
            actions: vec![Action::Retry, Action::EditPrompt, Action::Stop],
        }),
        raw_output: None,
        metadata: pair_backends::BackendMetadata {
            backend: "harness".into(),
            token_usage: None,
        },
    }
}

fn expected_start_state(mode: &Mode) -> NextState {
    if *mode == Mode::Fix {
        NextState::Patch
    } else {
        NextState::Any
    }
}

fn expected_card_kind(
    session: &Session,
    action: &BackendAction,
    next_state: &NextState,
) -> CardKind {
    match next_state {
        NextState::Patch => return CardKind::Patch,
        NextState::Summary | NextState::Finished => return CardKind::Summary,
        NextState::Any | NextState::Card => {}
    }

    match action {
        BackendAction::Start => match session.mode {
            Mode::Fix | Mode::Propose => CardKind::Patch,
            Mode::Explain | Mode::Review => CardKind::Finding,
            Mode::Auto | Mode::Investigate => CardKind::Hypothesis,
        },
        BackendAction::Reply(_) => CardKind::Finding,
        BackendAction::User(action) => match action {
            Action::Fix => CardKind::Patch,
            Action::OtherLead => CardKind::Hypothesis,
            Action::Follow | Action::Why | Action::Open | Action::RunCheck | Action::Next => {
                CardKind::Finding
            }
            Action::Retry | Action::EditPrompt => session
                .cards
                .iter()
                .rev()
                .find(|card| !matches!(card, Card::Error(_)))
                .map(Card::kind)
                .unwrap_or(CardKind::Hypothesis),
            Action::Apply | Action::ApplyPatch { .. } | Action::Stop => CardKind::Summary,
        },
    }
}

fn state_after_card(card: &Card, next_state: &NextState) -> SessionState {
    if matches!(card, Card::Error(_)) && matches!(next_state, NextState::Patch) {
        return SessionState::PatchFailed;
    }

    SessionState::from_card(card)
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
        } else if let Some(error) = result.error.as_deref() {
            format!("Patch was not applied: {error}")
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
            buffer_start_line: 1,
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
        assert_eq!(
            engine.get(&summary.session_id).unwrap().completed_steps,
            vec!["src/work.ts: Keeps body present for callers."]
        );
    }

    #[tokio::test]
    async fn rejects_apply_result_for_another_patch_card() {
        let backend = Arc::new(MockBackend);
        let mut engine = Engine::new(backend);
        let start = engine.start(params()).await.unwrap();
        let patch = engine.action(&start.session_id, Action::Fix).await.unwrap();
        let result = PatchApplyResult {
            session_id: start.session_id.clone(),
            card_id: "c_stale".into(),
            accepted: true,
            patch_ids: vec!["p_1".into()],
            changed_files: vec![PathBuf::from("src/work.ts")],
            error: None,
        };

        let error = engine.apply_result(result).unwrap_err();

        assert!(error.to_string().contains("current patch card"));
        assert_eq!(
            engine.get(&start.session_id).unwrap().state,
            SessionState::PatchShown
        );
        assert_eq!(patch.card.id(), "c_patch");
    }

    #[tokio::test]
    async fn rejected_apply_preserves_error_and_moves_to_summary() {
        let backend = Arc::new(MockBackend);
        let mut engine = Engine::new(backend);
        let start = engine.start(params()).await.unwrap();
        let patch = engine.action(&start.session_id, Action::Fix).await.unwrap();
        let result = PatchApplyResult {
            session_id: start.session_id.clone(),
            card_id: patch.card.id().into(),
            accepted: false,
            patch_ids: vec!["p_1".into()],
            changed_files: vec![],
            error: Some("patch context is ambiguous".into()),
        };

        let summary = engine.apply_result(result).unwrap();
        let Card::Summary(card) = summary.card else {
            panic!("expected summary card");
        };

        assert!(card.summary.contains("patch context is ambiguous"));
        assert_eq!(
            engine.get(&start.session_id).unwrap().state,
            SessionState::Summary
        );
    }

    #[tokio::test]
    async fn rejects_apply_result_with_unreported_target_file() {
        let backend = Arc::new(MockBackend);
        let mut engine = Engine::new(backend);
        let start = engine.start(params()).await.unwrap();
        let patch = engine.action(&start.session_id, Action::Fix).await.unwrap();
        let result = PatchApplyResult {
            session_id: start.session_id.clone(),
            card_id: patch.card.id().into(),
            accepted: true,
            patch_ids: vec!["p_1".into()],
            changed_files: vec![PathBuf::from("src/other.ts")],
            error: None,
        };

        let error = engine.apply_result(result).unwrap_err();

        assert!(error.to_string().contains("changed files"));
        assert!(
            engine
                .get(&start.session_id)
                .unwrap()
                .accepted_patches
                .is_empty()
        );
    }

    #[tokio::test]
    async fn returns_typed_card_and_keeps_session_after_backend_error() {
        let backend = Arc::new(FlakyBackend::default());
        let mut engine = Engine::new(backend);
        let start = engine.start(params()).await.unwrap();
        let failed = engine
            .action(&start.session_id, Action::Follow)
            .await
            .unwrap();

        let Card::Error(error) = failed.card else {
            panic!("expected error card");
        };
        assert!(error.message.contains("backend failed"));

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

    #[tokio::test]
    async fn preserves_wrong_card_type_and_raw_backend_output_for_fix() {
        let backend = Arc::new(WrongTypeBackend);
        let mut engine = Engine::new(backend);
        let start = engine.start(params()).await.unwrap();
        let result = engine.action(&start.session_id, Action::Fix).await.unwrap();

        let Card::Error(card) = result.card else {
            panic!("expected error card");
        };

        assert!(card.message.contains("expected patch card"));
        assert!(card.message.contains("Received card kind: Finding"));
        assert!(card.message.contains("raw finding from backend"));

        let retry = engine
            .action(&start.session_id, Action::Retry)
            .await
            .unwrap();
        assert!(matches!(retry.card, Card::Error(_)));
    }

    #[tokio::test]
    async fn start_in_fix_mode_requires_a_patch_card() {
        let backend = Arc::new(WrongTypeBackend);
        let mut engine = Engine::new(backend);
        let mut fix_params = params();
        fix_params.mode = Mode::Fix;

        let result = engine.start(fix_params).await.unwrap();
        let Card::Error(card) = result.card else {
            panic!("expected error card");
        };

        assert!(card.message.contains("expected patch card"));
        assert_eq!(
            engine.get(&result.session_id).unwrap().state,
            SessionState::PatchFailed
        );
        let apply_error = engine
            .action(&result.session_id, Action::Apply)
            .await
            .unwrap_err();
        assert!(apply_error.to_string().contains("invalid action"));
    }

    #[tokio::test]
    async fn start_returns_typed_card_when_backend_fails() {
        let backend = Arc::new(AlwaysFailBackend);
        let mut engine = Engine::new(backend);

        let result = engine.start(params()).await.unwrap();
        let Card::Error(card) = result.card else {
            panic!("expected error card");
        };

        assert!(card.message.contains("backend unavailable"));
        assert_eq!(result.turn_token_usage, Default::default());
        assert!(engine.get(&result.session_id).is_some());
    }

    #[test]
    fn rejects_card_with_invalid_location_coordinates() {
        let card = Card::Finding(FindingCard {
            id: "c_bad_location".into(),
            title: "Target".into(),
            finding: "The target is here.".into(),
            location: Some(pair_protocol::Location {
                file: "src/main.rs".into(),
                line: 0,
                column: 1,
            }),
            annotation: None,
            actions: vec![Action::Open, Action::Stop],
        });

        let error = validate_one_card(&card).unwrap_err();

        assert!(error.to_string().contains("must start at 1"));
    }

    #[test]
    fn rejects_card_with_empty_semantic_body() {
        let card = Card::Finding(FindingCard {
            id: "c_empty".into(),
            title: "Target".into(),
            finding: "   ".into(),
            location: None,
            annotation: None,
            actions: vec![Action::Stop],
        });

        let error = validate_one_card(&card).unwrap_err();

        assert!(error.to_string().contains("finding is empty"));
    }

    #[tokio::test]
    async fn replies_inside_session() {
        let backend = Arc::new(MockBackend);
        let mut engine = Engine::new(backend);
        let start = engine.start(params()).await.unwrap();
        let result = engine
            .reply(&start.session_id, "that is not it".into())
            .await
            .unwrap();

        let Card::Finding(card) = result.card else {
            panic!("expected finding card");
        };

        assert!(card.finding.contains("that is not it"));
    }

    #[tokio::test]
    async fn action_uses_refreshed_editor_context() {
        let backend = Arc::new(MockBackend);
        let mut engine = Engine::new(backend);
        let start = engine.start(params()).await.unwrap();
        let context = ContextBundle {
            cwd: PathBuf::from("/tmp/project"),
            file: PathBuf::from("templates/layout.html"),
            cursor: Cursor {
                line: 12,
                column: 1,
            },
            selection: None,
            buffer_text: "{{ block.preview_html|safe }}".into(),
            buffer_start_line: 1,
            diagnostics: vec![],
        };

        engine.update_context(&start.session_id, context).unwrap();
        let result = engine.action(&start.session_id, Action::Fix).await.unwrap();
        let Card::Patch(card) = result.card else {
            panic!("expected patch card");
        };

        assert_eq!(card.patches[0].file, PathBuf::from("templates/layout.html"));
    }

    #[derive(Default)]
    struct FlakyBackend {
        failed: AtomicBool,
    }

    struct BadPatchBackend;

    struct WrongTypeBackend;

    struct AlwaysFailBackend;

    #[async_trait]
    impl BackendAdapter for AlwaysFailBackend {
        async fn next_card(&self, _req: BackendRequest) -> Result<BackendResponse> {
            Err(anyhow!("backend unavailable: token limit reached"))
        }

        fn capabilities(&self) -> BackendInfo {
            BackendInfo {
                name: "always_fail".into(),
                streaming: false,
                patches: false,
                reasoning: false,
                can_read_project: false,
                can_use_tools: false,
            }
        }
    }

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
                    warnings: vec![],
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

    #[async_trait]
    impl BackendAdapter for WrongTypeBackend {
        async fn next_card(&self, req: BackendRequest) -> Result<BackendResponse> {
            let card = match req.action {
                BackendAction::Start => Card::Hypothesis(HypothesisCard {
                    id: "c_1".into(),
                    title: "Start".into(),
                    claim: "Wrong type for this test.".into(),
                    evidence: None,
                    next_move: None,
                    actions: vec![Action::Fix, Action::Stop],
                }),
                _ => Card::Finding(FindingCard {
                    id: "c_finding".into(),
                    title: "Wrong type".into(),
                    finding: "This is deliberately not a patch.".into(),
                    location: None,
                    annotation: None,
                    actions: vec![Action::Fix, Action::Stop],
                }),
            };

            Ok(BackendResponse {
                card,
                raw_output: Some("raw finding from backend".into()),
                metadata: BackendMetadata {
                    backend: "wrong_type".into(),
                    token_usage: None,
                },
            })
        }

        fn capabilities(&self) -> BackendInfo {
            BackendInfo {
                name: "wrong_type".into(),
                streaming: false,
                patches: false,
                reasoning: false,
                can_read_project: false,
                can_use_tools: false,
            }
        }
    }
}
