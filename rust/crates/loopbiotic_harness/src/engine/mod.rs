mod goal;
mod observations;
mod prefetch;
mod turn;
mod validate;

#[cfg(test)]
mod tests;

pub use prefetch::PrefetchMode;

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{Result, anyhow};
use loopbiotic_backends::{
    BackendAction, BackendAdapter, BackendRequest, CardContract, ProgressReporter, SessionSnapshot,
};
use loopbiotic_context::ContextOptimizer;
use loopbiotic_patch::PatchCoherence;
use loopbiotic_protocol::{
    Action, ActionResult, Card, CardKind, ContextBundle, ContextPolicy, ErrorCard,
    MAX_GOAL_CHANGED_LINES, MAX_GOAL_HUNKS_PER_PATCH, MAX_GOAL_PATCH_FILES, Mode, PatchApplyResult,
    StartSessionParams, StartSessionResult, SummaryCard, TokenUsage,
};

use crate::session::Session;
use crate::state::{NextState, SessionState};

use goal::{
    complete_goal_locally, completed_patch_signatures, completed_patch_steps, goal_progress,
    queue_goal_patch_cards, update_goal_state,
};
use observations::{observation_prompt_line, prepare_observation_card, record_observations};
use prefetch::{Continuation, Prefetch};
use validate::validate_apply_result;

pub struct Engine {
    backend: Arc<dyn BackendAdapter>,
    sessions: HashMap<String, Session>,
    context_optimizer: ContextOptimizer,
    prefetch_mode: PrefetchMode,
    prefetches: HashMap<String, Prefetch>,
    /// In-flight speculative goal-continuation turns, keyed by session.
    continuations: HashMap<String, Continuation>,
    /// Cancelled continuations still running on the backend; their usage is
    /// folded into the session totals once they finish.
    cancelled_continuations: Vec<(
        String,
        tokio::task::JoinHandle<Result<loopbiotic_backends::BackendResponse>>,
    )>,
    location_granter: Option<LocationGranter>,
    source_context_provider: Option<SourceContextProvider>,
}

/// Most open_location grants honored within a single turn before the request
/// is surfaced as a deny card instead.
pub const MAX_LOCATION_GRANTS: usize = 2;

/// Editor callback that asks the user to open a location mid-turn and, when
/// granted, returns freshly captured context for that buffer.
pub type LocationGranter = Arc<
    dyn Fn(
            loopbiotic_protocol::OpenLocationCard,
            String,
        )
            -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<ContextBundle>> + Send>>
        + Send
        + Sync,
>;

/// Editor callback that snapshots a patch target without changing the active
/// window. Goal batches use it to validate every file before review begins.
pub type SourceContextProvider = Arc<
    dyn Fn(
            std::path::PathBuf,
            String,
        )
            -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<ContextBundle>> + Send>>
        + Send
        + Sync,
>;

impl Engine {
    pub fn new(backend: Arc<dyn BackendAdapter>) -> Self {
        Self {
            backend,
            sessions: HashMap::new(),
            context_optimizer: ContextOptimizer::default(),
            prefetch_mode: PrefetchMode::Off,
            prefetches: HashMap::new(),
            continuations: HashMap::new(),
            cancelled_continuations: vec![],
            location_granter: None,
            source_context_provider: None,
        }
    }

    pub fn set_prefetch_mode(&mut self, mode: PrefetchMode) {
        self.prefetch_mode = mode;
    }

    pub fn set_location_granter(&mut self, granter: LocationGranter) {
        self.location_granter = Some(granter);
    }

    pub fn set_source_context_provider(&mut self, provider: SourceContextProvider) {
        self.source_context_provider = Some(provider);
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
        let context = self.optimize_context(
            ContextBundle::from_start(params),
            &session.original_prompt,
            &session.context_policy,
        );
        session.context = context.clone();
        let expected = expected_start_state(&session);
        let response = self
            .next_distinct_response(
                &mut session,
                BackendAction::Start,
                context,
                &expected,
                progress,
                None,
            )
            .await;
        let turn_token_usage = response.metadata.token_usage.clone().unwrap_or_default();
        let attempts = response.metadata.attempts.clone();
        let model = response.metadata.model.clone();
        self.add_usage(&mut session, &response.metadata.token_usage);

        let card = self.accept_response(&mut session, response, expected)?;
        let session_id = session.id.clone();
        let goal = goal_progress(&session);
        let token_usage = session.token_usage.clone();
        let context_report = session.context.report.clone();

        self.sessions.insert(session_id.clone(), session);
        self.schedule_prefetch(&session_id).await;
        self.schedule_goal_continuation(&session_id).await;

        Ok(StartSessionResult {
            session_id,
            card,
            goal,
            token_usage,
            turn_token_usage,
            context_report,
            model,
            attempts,
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
        if matches!(action, Action::Retry | Action::EditPrompt | Action::Stop) {
            // These actions abandon the pending slice, so a speculated
            // continuation built on top of it can only be wasted work.
            self.cancel_goal_continuation(&mut session).await;
        }
        let prefetched = self.take_prefetch(&mut session, &action).await;
        let result = self
            .action_taken(session_id, &mut session, action, progress, prefetched)
            .await;

        self.sessions.insert(session_id.into(), session);
        if result.is_ok() {
            self.schedule_prefetch(session_id).await;
            self.schedule_goal_continuation(session_id).await;
        }

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
        // A reply reworks or replaces whatever is pending, so a speculated
        // continuation of the current slice is stale.
        self.cancel_goal_continuation(&mut session).await;
        let result = self
            .reply_taken(session_id, &mut session, text, progress)
            .await;

        self.sessions.insert(session_id.into(), session);
        if result.is_ok() {
            self.schedule_prefetch(session_id).await;
            self.schedule_goal_continuation(session_id).await;
        }

        result
    }

    async fn action_taken(
        &self,
        session_id: &str,
        session: &mut Session,
        action: Action,
        progress: Option<ProgressReporter>,
        prefetched: Option<loopbiotic_backends::BackendResponse>,
    ) -> Result<ActionResult> {
        if action == Action::ResumeDraft {
            if session.state != SessionState::PatchExplained {
                return Err(anyhow!("no explained patch is waiting to resume"));
            }
            let card = session
                .cards
                .iter()
                .rev()
                .find(|card| matches!(card, Card::Patch(_)))
                .cloned()
                .ok_or_else(|| anyhow!("pending patch is unavailable"))?;
            session.state = SessionState::PatchShown;
            session.cards.push(card.clone());

            return Ok(ActionResult {
                session_id: session_id.into(),
                card,
                goal: goal_progress(session),
                token_usage: session.token_usage.clone(),
                turn_token_usage: TokenUsage::default(),
                context_report: session.context.report.clone(),
                model: None,
                attempts: vec![],
            });
        }

        if session.state == SessionState::PatchShown
            && matches!(action, Action::Retry | Action::EditPrompt)
        {
            // The redraft replaces the pending slice chain, so there is no
            // longer a planned continuation to speculate on.
            session.pending_patch_cards.clear();
            session.goal_slice_continues = false;
        }

        let state = session.state.next(&action)?;
        if action == Action::Stop {
            session.state = SessionState::Finished;
            session.goal_status = loopbiotic_protocol::GoalStatus::Stopped;
            session.next_step = None;
            let card = session.stop_card();
            let token_usage = session.token_usage.clone();

            session.cards.push(card.clone());

            return Ok(ActionResult {
                session_id: session_id.into(),
                card,
                goal: goal_progress(session),
                token_usage,
                turn_token_usage: Default::default(),
                context_report: session.context.report.clone(),
                model: None,
                attempts: vec![],
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
                prefetched,
            )
            .await;

        let turn_token_usage = response.metadata.token_usage.clone().unwrap_or_default();
        let attempts = response.metadata.attempts.clone();
        let model = response.metadata.model.clone();
        self.add_usage(session, &response.metadata.token_usage);

        let card = self.accept_response(session, response, state)?;
        let token_usage = session.token_usage.clone();

        Ok(ActionResult {
            session_id: session_id.into(),
            card,
            goal: goal_progress(session),
            token_usage,
            turn_token_usage,
            context_report: session.context.report.clone(),
            model,
            attempts,
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
        let expected = if session.state == SessionState::PatchExplained {
            NextState::GoalWhy
        } else if session.continuous_goal {
            NextState::GoalLoop
        } else {
            NextState::Any
        };

        session.state = SessionState::Thinking;

        let response = self
            .next_distinct_response(
                session,
                BackendAction::Reply(text),
                context,
                &expected,
                progress,
                None,
            )
            .await;

        let turn_token_usage = response.metadata.token_usage.clone().unwrap_or_default();
        let attempts = response.metadata.attempts.clone();
        let model = response.metadata.model.clone();
        self.add_usage(session, &response.metadata.token_usage);

        let card = self.accept_response(session, response, expected)?;
        let token_usage = session.token_usage.clone();

        Ok(ActionResult {
            session_id: session_id.into(),
            card,
            goal: goal_progress(session),
            token_usage,
            turn_token_usage,
            context_report: session.context.report.clone(),
            model,
            attempts,
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

        self.sessions.insert(session_id.clone(), session);
        if output.is_ok() {
            // Once an accepted result surfaces the next slice, speculate on
            // its continuation. Rejections clear goal_slice_continues, so
            // they stop here until the user explicitly retries.
            self.schedule_goal_continuation(&session_id).await;
        }

        output
    }

    async fn apply_result_taken(
        &mut self,
        session: &mut Session,
        result: PatchApplyResult,
        progress: Option<ProgressReporter>,
    ) -> Result<ActionResult> {
        session.state.require_patch()?;
        validate_apply_result(session, &result)?;
        self.context_optimizer
            .invalidate(&result.context.cwd, &result.changed_files);
        session.context = self.optimize_context(
            result.context.clone(),
            &session.original_prompt,
            &session.context_policy,
        );
        let session_id = result.session_id.clone();

        if result.accepted {
            let completes_goal = matches!(
                session.cards.last(),
                Some(Card::Patch(card)) if card.goal_complete
            );
            let completed_steps = completed_patch_steps(session);
            let completed_step_signatures = completed_patch_signatures(session);
            session.completed_steps.extend(completed_steps);
            session
                .completed_step_signatures
                .extend(completed_step_signatures);
            session.accepted_patches.extend(result.patch_ids.clone());
            session.state = SessionState::Summary;
            session.goal_status = loopbiotic_protocol::GoalStatus::NeedsReview;
            session.next_step = None;
            if session.continuous_goal {
                if let Some(next) = session.pending_patch_cards.pop_front() {
                    session.state = SessionState::PatchShown;
                    session.goal_status = loopbiotic_protocol::GoalStatus::Active;
                    session.next_step = Some(next.explanation.clone());
                    let card = Card::Patch(next);
                    session.cards.push(card.clone());

                    return Ok(ActionResult {
                        session_id,
                        card,
                        goal: goal_progress(session),
                        token_usage: session.token_usage.clone(),
                        turn_token_usage: TokenUsage::default(),
                        context_report: session.context.report.clone(),
                        model: None,
                        attempts: vec![],
                    });
                }
                if completes_goal {
                    return Ok(complete_goal_locally(&session_id, session));
                }
                // The last queued hunk was accepted: consume the slice that
                // was speculated while the user reviewed (awaiting it if it is
                // still generating); without one, run the turn for real.
                let speculated = self.take_goal_continuation(&session_id).await;
                return self
                    .goal_turn_taken(
                        &session_id,
                        session,
                        BackendAction::User(Action::Next),
                        progress,
                        speculated,
                    )
                    .await;
            }
            let changed_files = result.changed_files;
            let summary = if changed_files.is_empty() {
                "The local patch was applied. Continue only if the goal needs another change."
                    .into()
            } else {
                format!(
                    "Applied the local patch to {}. Continue only if the goal needs another change.",
                    changed_files
                        .iter()
                        .map(|file| file.display().to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            };
            let card = Card::Summary(SummaryCard {
                id: session.next_card_id("applied"),
                title: "Local step applied".into(),
                summary,
                changed_files,
                next_actions: vec![Action::Next, Action::RunCheck, Action::Stop],
            });
            session.cards.push(card.clone());

            return Ok(ActionResult {
                session_id,
                card,
                goal: goal_progress(session),
                token_usage: session.token_usage.clone(),
                turn_token_usage: TokenUsage::default(),
                context_report: session.context.report.clone(),
                model: None,
                attempts: vec![],
            });
        }

        session.rejected_patches.extend(result.patch_ids.clone());
        session.pending_patch_cards.clear();
        session.goal_slice_continues = false;
        session.next_step = None;
        // Rejecting is a local review decision, not permission to spend
        // another model turn. Drop the stale continuation and leave an
        // explicit Retry action if the user wants a replacement draft.
        self.cancel_goal_continuation(session).await;

        session.state = if session.continuous_goal {
            SessionState::GoalLoopFailed
        } else {
            SessionState::PatchFailed
        };
        let detail = result
            .error
            .filter(|error| !error.trim().is_empty())
            .map(|error| format!(" No changes were applied: {error}"))
            .unwrap_or_else(|| " No changes were applied.".into());
        let card = Card::Error(ErrorCard {
            id: session.next_card_id("rejected"),
            title: "Draft rejected".into(),
            message: format!(
                "The draft was rejected.{detail} Retry only if you want the agent to generate a replacement."
            ),
            actions: vec![Action::Retry, Action::EditPrompt, Action::Stop],
        });
        session.cards.push(card.clone());

        Ok(ActionResult {
            session_id,
            card,
            goal: goal_progress(session),
            token_usage: session.token_usage.clone(),
            turn_token_usage: TokenUsage::default(),
            context_report: session.context.report.clone(),
            model: None,
            attempts: vec![],
        })
    }

    async fn goal_turn_taken(
        &self,
        session_id: &str,
        session: &mut Session,
        action: BackendAction,
        progress: Option<ProgressReporter>,
        speculated: Option<loopbiotic_backends::BackendResponse>,
    ) -> Result<ActionResult> {
        let expected = NextState::GoalLoop;
        let context = session.context.clone();
        session.state = SessionState::Thinking;
        let response = self
            .next_distinct_response(session, action, context, &expected, progress, speculated)
            .await;
        let turn_token_usage = response.metadata.token_usage.clone().unwrap_or_default();
        let attempts = response.metadata.attempts.clone();
        let model = response.metadata.model.clone();
        self.add_usage(session, &response.metadata.token_usage);
        let card = self.accept_response(session, response, expected)?;

        Ok(ActionResult {
            session_id: session_id.into(),
            card,
            goal: goal_progress(session),
            token_usage: session.token_usage.clone(),
            turn_token_usage,
            context_report: session.context.report.clone(),
            model,
            attempts,
        })
    }

    pub fn get(&self, session_id: &str) -> Option<&Session> {
        self.sessions.get(session_id)
    }

    pub fn update_context(&mut self, session_id: &str, context: ContextBundle) -> Result<()> {
        let (prompt, policy) = self
            .sessions
            .get(session_id)
            .map(|session| {
                (
                    session.original_prompt.clone(),
                    session.context_policy.clone(),
                )
            })
            .ok_or_else(|| anyhow!("unknown session {session_id}"))?;
        let context = self.optimize_context(context, &prompt, &policy);
        self.sessions
            .get_mut(session_id)
            .expect("session checked above")
            .context = context;

        Ok(())
    }

    /// `ContextOptimizer::optimize` synchronously walks and reads up to
    /// ~2000 project files, so it must not stall an async worker thread.
    /// `spawn_blocking` would demand `'static` data, but the optimizer is
    /// borrowed `&mut` from `self`; `block_in_place` avoids that by moving
    /// this worker into blocking mode in place. NOTE: `block_in_place`
    /// panics on a `current_thread` runtime — the daemon runs the default
    /// multi-thread runtime, and the engine tests opt into
    /// `#[tokio::test(flavor = "multi_thread")]`.
    fn optimize_context(
        &mut self,
        context: ContextBundle,
        prompt: &str,
        policy: &ContextPolicy,
    ) -> ContextBundle {
        tokio::task::block_in_place(|| self.context_optimizer.optimize(context, prompt, policy))
    }

    fn request(
        &self,
        session: &Session,
        action: BackendAction,
        context: ContextBundle,
        expected: &NextState,
    ) -> BackendRequest {
        let expected_kind = expected_card_kind(session, &action, expected);

        let allow_goal_completion = matches!(expected, NextState::GoalLoop | NextState::GoalWhy);
        let mut card_contract = CardContract {
            expected_kind,
            allow_goal_completion,
            ..CardContract::default()
        };
        if allow_goal_completion && !matches!(expected, NextState::GoalWhy) {
            card_contract.max_patch_files = MAX_GOAL_PATCH_FILES;
            card_contract.max_hunks_per_patch = MAX_GOAL_HUNKS_PER_PATCH;
            card_contract.max_changed_lines = MAX_GOAL_CHANGED_LINES;
        }

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
            card_contract,
        }
    }

    fn accept_response(
        &self,
        session: &mut Session,
        response: loopbiotic_backends::BackendResponse,
        next_state: NextState,
    ) -> Result<Card> {
        let mut received = response.card;
        if let Card::Patch(patch) = &mut received
            && !patch.actions.contains(&Action::Why)
        {
            patch.actions.insert(1, Action::Why);
        }
        if matches!(next_state, NextState::GoalWhy)
            && let Card::Finding(finding) = &mut received
        {
            finding.actions = vec![Action::ResumeDraft, Action::Stop];
        }
        if matches!(next_state, NextState::GoalLoop)
            && let Card::Summary(summary) = &mut received
        {
            summary
                .next_actions
                .retain(|action| *action != Action::Next);
            if !summary.next_actions.contains(&Action::RunCheck) {
                summary.next_actions.insert(0, Action::RunCheck);
            }
            if !summary.next_actions.contains(&Action::Stop) {
                summary.next_actions.push(Action::Stop);
            }
        }
        if !matches!(next_state, NextState::GoalWhy) {
            prepare_observation_card(session, &mut received);
        }
        PatchCoherence::annotate(&mut received);
        if !matches!(next_state, NextState::GoalWhy) {
            record_observations(session, &received);
        }
        let card = if matches!(next_state, NextState::GoalLoop) {
            queue_goal_patch_cards(session, received)?
        } else {
            received
        };

        update_goal_state(session, &card, &next_state);
        session.state = state_after_card(&card, &next_state);
        session.cards.push(card.clone());

        Ok(card)
    }

    fn take_session(&mut self, session_id: &str) -> Result<Session> {
        self.sessions
            .remove(session_id)
            .ok_or_else(|| anyhow!("unknown session {session_id}"))
    }

    fn add_usage(&self, session: &mut Session, usage: &Option<loopbiotic_protocol::TokenUsage>) {
        if let Some(usage) = usage {
            session.token_usage.add(usage);
        }
    }
}

fn expected_start_state(session: &Session) -> NextState {
    if session.continuous_goal {
        NextState::GoalLoop
    } else if session.mode == Mode::Fix {
        NextState::Patch
    } else {
        NextState::Any
    }
}

/// None means the agent may answer with whichever card kind fits, including a
/// clarifying choice or a deny. A kind is only demanded when the user asked
/// for one (a "/{kind}" prompt prefix, an explicit mode, or a concrete action
/// such as Fix) or when the state machine requires it.
fn expected_card_kind(
    session: &Session,
    action: &BackendAction,
    next_state: &NextState,
) -> Option<CardKind> {
    match next_state {
        NextState::Patch => return Some(CardKind::Patch),
        NextState::GoalLoop => return None,
        NextState::GoalWhy => return Some(CardKind::Finding),
        NextState::Summary | NextState::Finished => return Some(CardKind::Summary),
        NextState::Any | NextState::Card => {}
    }

    match action {
        BackendAction::Start => session.forced_kind.or(match session.mode {
            Mode::Fix | Mode::Propose => Some(CardKind::Patch),
            Mode::Explain | Mode::Review => Some(CardKind::Finding),
            Mode::Investigate => Some(CardKind::Hypothesis),
            Mode::Auto => None,
        }),
        BackendAction::Reply(_) => None,
        BackendAction::ContractRetry(_) => None,
        BackendAction::LocationGranted => None,
        BackendAction::User(action) => match action {
            Action::Fix => Some(CardKind::Patch),
            Action::OtherLead => Some(CardKind::Hypothesis),
            Action::Follow | Action::Why | Action::Open | Action::RunCheck | Action::Next => {
                Some(CardKind::Finding)
            }
            Action::Retry | Action::EditPrompt => session
                .cards
                .iter()
                .rev()
                .find(|card| !matches!(card, Card::Error(_) | Card::Deny(_)))
                .map(Card::kind)
                .or(session.forced_kind),
            Action::Apply | Action::ApplyPatch { .. } | Action::ResumeDraft | Action::Stop => {
                Some(CardKind::Summary)
            }
        },
    }
}

fn state_after_card(card: &Card, next_state: &NextState) -> SessionState {
    let refused = matches!(card, Card::Error(_) | Card::Deny(_));
    if refused && matches!(next_state, NextState::Patch) {
        return SessionState::PatchFailed;
    }
    if refused && matches!(next_state, NextState::GoalLoop) {
        return SessionState::GoalLoopFailed;
    }
    if refused && matches!(next_state, NextState::GoalWhy) {
        return SessionState::PatchExplained;
    }
    if matches!(next_state, NextState::GoalWhy) && matches!(card, Card::Finding(_)) {
        return SessionState::PatchExplained;
    }

    SessionState::from_card(card)
}

fn card_summary(card: &Card) -> String {
    match card {
        Card::Hypothesis(card) => format!("hypothesis: {}", card.claim),
        Card::Finding(card) => format!("finding: {}", card.finding),
        Card::Patch(card) => format!("patch: {}", card.explanation),
        Card::Choice(card) => format!("choice: {}", card.question),
        Card::Deny(card) => format!("deny: {}", card.reason),
        Card::OpenLocation(card) => format!("open_location: {}", card.reason),
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
