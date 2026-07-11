use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{Result, anyhow};
use pair_backends::{
    BackendAction, BackendAdapter, BackendProgress, BackendRequest, BackendResponse, CardContract,
    ProgressReporter, SessionSnapshot,
};
use pair_patch::{PatchCoherence, PatchNormalizer, PatchValidator};
use pair_protocol::{
    Action, ActionResult, Card, CardKind, ContextBundle, ErrorCard, GoalProgress, Mode,
    ObservationKind, ObservationProgress, PatchApplyResult, StartSessionParams, StartSessionResult,
    SummaryCard,
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
        let response = self
            .next_distinct_response(
                &mut session,
                BackendAction::Start,
                context,
                &expected,
                progress,
            )
            .await;
        let turn_token_usage = response.metadata.token_usage.clone().unwrap_or_default();
        self.add_usage(&mut session, &response.metadata.token_usage);

        let card = self.accept_response(&mut session, response, expected)?;
        let session_id = session.id.clone();
        let goal = goal_progress(&session);
        let token_usage = session.token_usage.clone();

        self.sessions.insert(session_id.clone(), session);

        Ok(StartSessionResult {
            session_id,
            card,
            goal,
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
                goal: goal_progress(session),
                token_usage,
                turn_token_usage: Default::default(),
            });
        }

        let context = session.context.clone();

        session.state = SessionState::Thinking;

        let response = self
            .next_distinct_response(
                session,
                BackendAction::User(action),
                context,
                &state,
                progress,
            )
            .await;

        let turn_token_usage = response.metadata.token_usage.clone().unwrap_or_default();
        self.add_usage(session, &response.metadata.token_usage);

        let card = self.accept_response(session, response, state)?;
        let token_usage = session.token_usage.clone();

        Ok(ActionResult {
            session_id: session_id.into(),
            card,
            goal: goal_progress(session),
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

        session.state = SessionState::Thinking;

        let response = self
            .next_distinct_response(
                session,
                BackendAction::Reply(text),
                context,
                &expected,
                progress,
            )
            .await;

        let turn_token_usage = response.metadata.token_usage.clone().unwrap_or_default();
        self.add_usage(session, &response.metadata.token_usage);

        let card = self.accept_response(session, response, expected)?;
        let token_usage = session.token_usage.clone();

        Ok(ActionResult {
            session_id: session_id.into(),
            card,
            goal: goal_progress(session),
            token_usage,
            turn_token_usage,
        })
    }

    pub async fn apply_result(&mut self, result: PatchApplyResult) -> Result<ActionResult> {
        self.apply_result_with_progress(result, None).await
    }

    pub async fn apply_result_with_progress(
        &mut self,
        result: PatchApplyResult,
        progress: Option<ProgressReporter>,
    ) -> Result<ActionResult> {
        let mut session = self.take_session(&result.session_id)?;
        let session_id = result.session_id.clone();
        let output = self
            .apply_result_taken(&mut session, result, progress)
            .await;

        self.sessions.insert(session_id, session);

        output
    }

    async fn apply_result_taken(
        &self,
        session: &mut Session,
        result: PatchApplyResult,
        progress: Option<ProgressReporter>,
    ) -> Result<ActionResult> {
        session.state.require_patch()?;
        validate_apply_result(session, &result)?;
        session.context = result.context.clone();
        let session_id = result.session_id.clone();

        let next_action = if result.accepted {
            let completed_steps = completed_patch_steps(session);
            session.completed_steps.extend(completed_steps);
            session.accepted_patches.extend(result.patch_ids.clone());
            session.state = SessionState::Summary;
            Action::Next
        } else {
            session.rejected_patches.extend(result.patch_ids.clone());
            session.state = SessionState::PatchShown;
            Action::Retry
        };

        self.action_taken(&session_id, session, next_action, progress)
            .await
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
                known_observations: session
                    .known_observations
                    .iter()
                    .map(observation_prompt_line)
                    .collect(),
                mode: session.mode.clone(),
                card_count: session.cards.len(),
                last_card: session.cards.last().cloned(),
                last_summary: session.cards.last().map(card_summary),
            },
            action,
            context,
            card_contract: CardContract {
                expected_kind: Some(expected_kind),
                allow_goal_completion: matches!(expected, NextState::Continuation),
                ..CardContract::default()
            },
        }
    }

    async fn next_distinct_response(
        &self,
        session: &mut Session,
        action: BackendAction,
        context: ContextBundle,
        expected: &NextState,
        progress: Option<ProgressReporter>,
    ) -> BackendResponse {
        let mut action = action;
        let mut token_usage = None;

        for attempt in 0..3 {
            let request = self.request(session, action, context.clone(), expected);
            let mut response = match self
                .backend
                .next_card_with_progress(request, progress.clone())
                .await
            {
                Ok(response) => response,
                Err(error) => {
                    let mut response = backend_failure_response(session, error);
                    merge_usage(&mut token_usage, &response.metadata.token_usage);
                    response.metadata.token_usage = token_usage;
                    return response;
                }
            };
            merge_usage(&mut token_usage, &response.metadata.token_usage);

            if let Some((key, reason)) = duplicate_observation(session, &response.card) {
                activate_observation(session, &key);
                if attempt < 2 {
                    if let Some(progress) = &progress {
                        progress(BackendProgress {
                            session_id: session.id.clone(),
                            phase: "deduplicating".into(),
                            message: "Retaining repeated context and requesting a distinct step"
                                .into(),
                        });
                    }
                    action = BackendAction::ContractRetry(format!(
                        "{reason}. Return a distinct next observation; do not repeat known findings or signals."
                    ));
                    continue;
                }

                let mut rejected = duplicate_failure_response(session, reason);
                rejected.metadata.token_usage = token_usage;
                return rejected;
            }

            let mut candidate = response.card.clone();
            let validation =
                PatchNormalizer::normalize_card(&mut candidate, &context).and_then(|()| {
                    PatchCoherence::annotate(&mut candidate);
                    validate_backend_card(&candidate, expected, &context)
                });
            if let Err(error) = validation {
                if attempt < 2 {
                    if let Some(progress) = &progress {
                        progress(BackendProgress {
                            session_id: session.id.clone(),
                            phase: "repairing".into(),
                            message: "Patch contract failed; Codex is repairing the local step"
                                .into(),
                        });
                    }
                    action = BackendAction::ContractRetry(format!(
                        "The previous card failed the local patch contract: {error}. Rebuild the same step. Source context/remove lines must be exact and contiguous in the supplied buffer; added lines do not replace omitted source context. The resulting local step must remain type-correct without work deferred to a later card."
                    ));
                    continue;
                }

                response.card =
                    rejected_card(session, &candidate, error, response.raw_output.as_deref());
                response.metadata.token_usage = token_usage;
                return response;
            }

            response.card = candidate;
            response.metadata.token_usage = token_usage;
            return response;
        }

        unreachable!()
    }

    fn accept_response(
        &self,
        session: &mut Session,
        response: BackendResponse,
        next_state: NextState,
    ) -> Result<Card> {
        let mut received = response.card;
        prepare_observation_card(session, &mut received);
        let validation =
            PatchNormalizer::normalize_card(&mut received, &session.context).and_then(|()| {
                PatchCoherence::annotate(&mut received);
                validate_backend_card(&received, &next_state, &session.context)
            });
        let card = match validation {
            Ok(()) => {
                record_observations(session, &received);
                received
            }
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

fn goal_progress(session: &Session) -> GoalProgress {
    GoalProgress {
        statement: session.original_prompt.clone(),
        completed_steps: session.completed_steps.clone(),
        known_observations: session.known_observations.clone(),
    }
}

fn merge_usage(
    total: &mut Option<pair_protocol::TokenUsage>,
    turn: &Option<pair_protocol::TokenUsage>,
) {
    let Some(turn) = turn else {
        return;
    };

    if let Some(total) = total {
        total.add(turn);
    } else {
        *total = Some(turn.clone());
    }
}

fn duplicate_observation(session: &Session, card: &Card) -> Option<(String, String)> {
    let (key, _, _) = core_observation(card)?;
    session
        .observation_index
        .contains_key(&key)
        .then(|| (key, "backend repeated a retained observation".into()))
}

fn prepare_observation_card(session: &mut Session, card: &mut Card) {
    if core_observation(card).is_some() {
        for observation in &mut session.known_observations {
            observation.active = false;
        }
    }

    match card {
        Card::Hypothesis(card) => {
            if let Some(evidence) = &mut card.evidence
                && !evidence.annotation.trim().is_empty()
            {
                let key = observation_key(ObservationKind::Signal, &evidence.annotation);
                if session.observation_index.contains_key(&key) {
                    activate_observation(session, &key);
                    evidence.annotation.clear();
                }
            }
        }
        Card::Finding(card) => {
            if let Some(annotation) = card
                .annotation
                .clone()
                .filter(|text| !text.trim().is_empty())
            {
                let key = observation_key(ObservationKind::Signal, &annotation);
                if session.observation_index.contains_key(&key) {
                    activate_observation(session, &key);
                    card.annotation = None;
                }
            }
        }
        _ => {}
    }
}

fn record_observations(session: &mut Session, card: &Card) {
    if let Some((key, kind, label)) = core_observation(card) {
        record_observation(session, key, kind, label);
    }

    match card {
        Card::Hypothesis(card) => {
            if let Some(evidence) = &card.evidence
                && !evidence.annotation.trim().is_empty()
            {
                let label = evidence.annotation.clone();
                record_observation(
                    session,
                    observation_key(ObservationKind::Signal, &label),
                    ObservationKind::Signal,
                    label,
                );
            }
        }
        Card::Finding(card) => {
            if let Some(label) = card
                .annotation
                .clone()
                .filter(|text| !text.trim().is_empty())
            {
                record_observation(
                    session,
                    observation_key(ObservationKind::Signal, &label),
                    ObservationKind::Signal,
                    label,
                );
            }
        }
        _ => {}
    }
}

fn core_observation(card: &Card) -> Option<(String, ObservationKind, String)> {
    let (kind, label) = match card {
        Card::Hypothesis(card) => (ObservationKind::Hypothesis, card.claim.clone()),
        Card::Finding(card) => (ObservationKind::Finding, card.finding.clone()),
        _ => return None,
    };

    Some((observation_key(kind, &label), kind, label))
}

fn record_observation(session: &mut Session, key: String, kind: ObservationKind, label: String) {
    if let Some(index) = session.observation_index.get(&key).copied() {
        if let Some(observation) = session.known_observations.get_mut(index) {
            observation.occurrences += 1;
            observation.active = true;
        }
        return;
    }

    let index = session.known_observations.len();
    session.observation_index.insert(key, index);
    session.known_observations.push(ObservationProgress {
        id: format!("o_{}", index + 1),
        kind,
        label,
        occurrences: 1,
        active: true,
    });
}

fn activate_observation(session: &mut Session, key: &str) {
    let Some(index) = session.observation_index.get(key).copied() else {
        return;
    };
    if let Some(observation) = session.known_observations.get_mut(index) {
        observation.occurrences += 1;
        observation.active = true;
    }
}

fn observation_key(kind: ObservationKind, label: &str) -> String {
    format!(
        "{}:{}",
        observation_kind_name(kind),
        normalize_observation(label)
    )
}

fn normalize_observation(text: &str) -> String {
    text.to_lowercase()
        .chars()
        .map(|character| {
            if character.is_alphanumeric() {
                character
            } else {
                ' '
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn observation_kind_name(kind: ObservationKind) -> &'static str {
    match kind {
        ObservationKind::Hypothesis => "hypothesis",
        ObservationKind::Finding => "finding",
        ObservationKind::Signal => "signal",
    }
}

fn observation_prompt_line(observation: &ObservationProgress) -> String {
    format!(
        "{} {} (seen {}x): {}",
        observation.id,
        observation_kind_name(observation.kind),
        observation.occurrences,
        observation.label
    )
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

fn duplicate_failure_response(session: &Session, reason: String) -> BackendResponse {
    BackendResponse {
        card: Card::Error(ErrorCard {
            id: session.next_card_id("duplicate_error"),
            title: "Backend repeated retained context".into(),
            message: format!(
                "{reason}. The duplicate was retained in session memory but was not shown again."
            ),
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
        NextState::Continuation => return CardKind::Patch,
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
        BackendAction::ContractRetry(_) => CardKind::Finding,
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
    if matches!(card, Card::Error(_)) && matches!(next_state, NextState::Continuation) {
        return SessionState::ContinuationFailed;
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

    fn editor_context(buffer_text: &str) -> ContextBundle {
        ContextBundle {
            cwd: PathBuf::from("/tmp/project"),
            file: PathBuf::from("src/work.ts"),
            cursor: Cursor { line: 1, column: 1 },
            selection: None,
            buffer_text: buffer_text.into(),
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
    async fn accepted_patch_continues_directly_without_intermediate_summary() {
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
            context: editor_context("payload = payload or {}"),
        };

        let next = engine.apply_result(result).await.unwrap();

        assert!(matches!(next.card, Card::Patch(_)));
        assert_eq!(
            engine.get(&next.session_id).unwrap().completed_steps,
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
            context: editor_context("payload = payload or {}"),
        };

        let error = engine.apply_result(result).await.unwrap_err();

        assert!(error.to_string().contains("current patch card"));
        assert_eq!(
            engine.get(&start.session_id).unwrap().state,
            SessionState::PatchShown
        );
        assert_eq!(patch.card.id(), "c_patch");
    }

    #[tokio::test]
    async fn rejected_apply_returns_reworked_patch_without_summary() {
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
            context: editor_context("placeholder"),
        };

        let reworked = engine.apply_result(result).await.unwrap();

        assert!(matches!(reworked.card, Card::Patch(_)));
        assert_eq!(
            engine.get(&start.session_id).unwrap().state,
            SessionState::PatchShown
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
            context: editor_context("payload = payload or {}"),
        };

        let error = engine.apply_result(result).await.unwrap_err();

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
    async fn repairs_invalid_patch_before_showing_it_to_user() {
        let backend = Arc::new(RepairingPatchBackend::default());
        let mut engine = Engine::new(backend);
        let start = engine.start(params()).await.unwrap();

        let result = engine.action(&start.session_id, Action::Fix).await.unwrap();

        let Card::Patch(card) = result.card else {
            panic!("expected repaired patch card");
        };
        assert_eq!(
            card.patches[0].diff,
            "@@ -1,1 +1,1 @@\n-placeholder\n+repaired\n"
        );
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

    #[tokio::test]
    async fn retains_duplicate_observations_without_showing_them_again() {
        let backend = Arc::new(RepeatingObservationBackend::default());
        let mut engine = Engine::new(backend);
        let start = engine.start(params()).await.unwrap();

        let result = engine
            .action(&start.session_id, Action::Follow)
            .await
            .unwrap();

        let Card::Finding(card) = result.card else {
            panic!("expected distinct finding after automatic contract retry");
        };
        assert_eq!(card.finding, "The caller still consumes the old shape.");
        assert_eq!(card.annotation, None);

        let observations = &engine.get(&start.session_id).unwrap().known_observations;
        assert_eq!(observations.len(), 3);
        assert_eq!(observations[0].occurrences, 2);
        assert_eq!(observations[1].occurrences, 2);
        assert!(!observations[0].active);
        assert!(observations[1].active);
        assert!(observations[2].active);
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

    #[derive(Default)]
    struct RepairingPatchBackend {
        failed_once: AtomicBool,
    }

    struct WrongTypeBackend;

    struct AlwaysFailBackend;

    #[derive(Default)]
    struct RepeatingObservationBackend {
        calls: std::sync::atomic::AtomicUsize,
    }

    #[async_trait]
    impl BackendAdapter for RepeatingObservationBackend {
        async fn next_card(&self, req: BackendRequest) -> Result<BackendResponse> {
            let call = self.calls.fetch_add(1, Ordering::SeqCst);
            let card = match (call, req.action) {
                (0, BackendAction::Start) | (1, BackendAction::User(Action::Follow)) => {
                    Card::Hypothesis(HypothesisCard {
                        id: format!("c_repeat_{call}"),
                        title: "Repeated branch".into(),
                        claim: "The branch returns before building the preview.".into(),
                        evidence: Some(pair_protocol::LocationEvidence {
                            file: "src/work.ts".into(),
                            line: 1,
                            column: 1,
                            annotation: "The preview is skipped here.".into(),
                        }),
                        next_move: None,
                        actions: vec![Action::Follow, Action::Fix, Action::Stop],
                    })
                }
                (2, BackendAction::ContractRetry(_)) => Card::Finding(FindingCard {
                    id: "c_distinct".into(),
                    title: "Consumer remains".into(),
                    finding: "The caller still consumes the old shape.".into(),
                    location: Some(pair_protocol::Location {
                        file: "src/work.ts".into(),
                        line: 1,
                        column: 1,
                    }),
                    annotation: Some("The preview is skipped here.".into()),
                    actions: vec![Action::Fix, Action::Stop],
                }),
                _ => panic!("unexpected observation backend request"),
            };

            Ok(BackendResponse {
                card,
                raw_output: None,
                metadata: BackendMetadata {
                    backend: "repeating_observation".into(),
                    token_usage: Some(pair_protocol::TokenUsage::estimated(10, 5)),
                },
            })
        }

        fn capabilities(&self) -> BackendInfo {
            BackendInfo {
                name: "repeating_observation".into(),
                streaming: false,
                patches: true,
                reasoning: false,
                can_read_project: false,
                can_use_tools: false,
            }
        }
    }

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
    impl BackendAdapter for RepairingPatchBackend {
        async fn next_card(&self, req: BackendRequest) -> Result<BackendResponse> {
            let card = match req.action {
                BackendAction::Start => Card::Hypothesis(HypothesisCard {
                    id: "c_1".into(),
                    title: "Start".into(),
                    claim: "The local representation needs one change.".into(),
                    evidence: None,
                    next_move: None,
                    actions: vec![Action::Fix, Action::Stop],
                }),
                BackendAction::User(Action::Fix)
                    if !self.failed_once.swap(true, Ordering::SeqCst) =>
                {
                    Card::Patch(PatchCard {
                        id: "c_invalid".into(),
                        title: "Invalid first attempt".into(),
                        explanation: "This attempt has stale context.".into(),
                        warnings: vec![],
                        patches: vec![FilePatch {
                            id: "p_1".into(),
                            file: "src/work.ts".into(),
                            diff: "@@ -1,1 +1,1 @@\n-stale\n+new\n".into(),
                            explanation: "Stale attempt.".into(),
                        }],
                        actions: vec![Action::Apply, Action::Retry, Action::Stop],
                    })
                }
                BackendAction::ContractRetry(reason) => {
                    assert!(reason.contains("patch context was not found"));
                    Card::Patch(PatchCard {
                        id: "c_repaired".into(),
                        title: "Repaired local step".into(),
                        explanation: "Use exact current context.".into(),
                        warnings: vec![],
                        patches: vec![FilePatch {
                            id: "p_1".into(),
                            file: "src/work.ts".into(),
                            diff: "@@ -1,1 +1,1 @@\n-placeholder\n+repaired\n".into(),
                            explanation: "Repair one line.".into(),
                        }],
                        actions: vec![Action::Apply, Action::Retry, Action::Stop],
                    })
                }
                _ => panic!("unexpected repair backend request"),
            };

            Ok(BackendResponse {
                card,
                raw_output: None,
                metadata: BackendMetadata {
                    backend: "repairing_patch".into(),
                    token_usage: Some(pair_protocol::TokenUsage::estimated(10, 5)),
                },
            })
        }

        fn capabilities(&self) -> BackendInfo {
            BackendInfo {
                name: "repairing_patch".into(),
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
