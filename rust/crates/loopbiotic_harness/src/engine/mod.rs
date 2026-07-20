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
use loopbiotic_context::{ContextOptimizer, project::ProjectProfiler};
use loopbiotic_patch::PatchCoherence;
use loopbiotic_protocol::{
    Action, ActionResult, Card, CardKind, ContextBundle, ContextPolicy, ErrorCard, FindingCard,
    InstructionSkill, Mode, PatchApplyResult, StartSessionParams, StartSessionResult, SummaryCard,
    TokenUsage, WorkingCard,
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
    project_profiler: ProjectProfiler,
    project_intelligence_enabled: bool,
    prefetch_mode: PrefetchMode,
    prefetches: HashMap<String, Prefetch>,
    /// In-flight speculative goal-continuation turns, keyed by session.
    continuations: HashMap<String, Continuation>,
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
            project_profiler: ProjectProfiler,
            project_intelligence_enabled: true,
            prefetch_mode: PrefetchMode::Off,
            prefetches: HashMap::new(),
            continuations: HashMap::new(),
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

    /// Benchmark/control seam for comparing the pre-profile harness with the
    /// marker-adapter path while keeping every other engine behavior identical.
    pub fn set_project_intelligence(&mut self, enabled: bool) {
        self.project_intelligence_enabled = enabled;
    }

    pub fn record_interaction_feedback(
        &mut self,
        session_id: &str,
        feedback: impl Into<String>,
    ) -> Result<()> {
        let session = self
            .sessions
            .get_mut(session_id)
            .ok_or_else(|| anyhow!("unknown session {session_id}"))?;
        let feedback = feedback.into();
        if !session.interaction_feedback.contains(&feedback) {
            session.interaction_feedback.push(feedback);
            if session.interaction_feedback.len() > 3 {
                session.interaction_feedback.remove(0);
            }
        }

        Ok(())
    }

    pub fn update_skills(&mut self, session_id: &str, skills: Vec<InstructionSkill>) -> Result<()> {
        let session = self
            .sessions
            .get_mut(session_id)
            .ok_or_else(|| anyhow!("unknown session {session_id}"))?;
        session.skills = skills;
        Ok(())
    }

    pub async fn start(&mut self, params: StartSessionParams) -> Result<StartSessionResult> {
        self.start_with_progress(params, None).await
    }

    pub fn reserve_start(&mut self, params: StartSessionParams) -> (String, u64) {
        let mut session = Session::new(params);
        session.turn_generation = 1;
        let session_id = session.id.clone();
        self.sessions.insert(session_id.clone(), session);

        (session_id, 1)
    }

    pub async fn start_with_progress(
        &mut self,
        params: StartSessionParams,
        progress: Option<ProgressReporter>,
    ) -> Result<StartSessionResult> {
        let (session_id, generation) = self.reserve_start(params);
        self.complete_start_with_progress(&session_id, generation, progress)
            .await
    }

    pub async fn complete_start_with_progress(
        &mut self,
        session_id: &str,
        generation: u64,
        progress: Option<ProgressReporter>,
    ) -> Result<StartSessionResult> {
        let mut session = self.session_for_turn(session_id, generation)?;
        if self.project_intelligence_enabled {
            session.project = Some(self.profile_project(&session.cwd, &session.project_signals));
        }
        let context = self.optimize_context(
            session.context.clone(),
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
        session.interaction_feedback.clear();
        let turn_token_usage = response.metadata.token_usage.clone().unwrap_or_default();
        let attempts = response.metadata.attempts.clone();
        let model = response.metadata.model.clone();
        self.add_usage(&mut session, &response.metadata.token_usage);

        let card = self.accept_response(&mut session, response, expected)?;
        let session_id = session.id.clone();
        let goal = goal_progress(&session);
        let token_usage = session.token_usage.clone();
        let context_report = session.context.report.clone();

        self.commit_session(session, generation)?;
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
        let generation = self.begin_turn(session_id)?;
        self.action_with_progress_generation(session_id, generation, action, progress)
            .await
    }

    pub async fn action_with_progress_generation(
        &mut self,
        session_id: &str,
        generation: u64,
        action: Action,
        progress: Option<ProgressReporter>,
    ) -> Result<ActionResult> {
        let mut session = self.session_for_turn(session_id, generation)?;
        if matches!(action, Action::Retry | Action::EditPrompt | Action::Stop) {
            // These actions abandon the pending slice, so a speculated
            // continuation built on top of it can only be wasted work.
            self.cancel_goal_continuation(&mut session).await;
            self.cancel_accept_continuation(&mut session).await;
        }
        let result = self
            .action_taken(session_id, &mut session, action, progress, None)
            .await;

        self.commit_session(session, generation)?;
        if result.is_ok() {
            self.schedule_prefetch(session_id).await;
            self.schedule_goal_continuation(session_id).await;
        }

        result
    }

    pub async fn reply(
        &mut self,
        session_id: &str,
        text: String,
        mode: Mode,
    ) -> Result<ActionResult> {
        self.reply_with_progress(session_id, text, mode, None).await
    }

    pub async fn reply_with_progress(
        &mut self,
        session_id: &str,
        text: String,
        mode: Mode,
        progress: Option<ProgressReporter>,
    ) -> Result<ActionResult> {
        let generation = self.begin_turn(session_id)?;
        self.reply_with_progress_generation(session_id, generation, text, mode, progress)
            .await
    }

    pub async fn reply_with_progress_generation(
        &mut self,
        session_id: &str,
        generation: u64,
        text: String,
        mode: Mode,
        progress: Option<ProgressReporter>,
    ) -> Result<ActionResult> {
        let mut session = self.session_for_turn(session_id, generation)?;
        // A reply reworks or replaces whatever is pending, so a speculated
        // continuation of the current slice is stale.
        self.cancel_goal_continuation(&mut session).await;
        self.cancel_accept_continuation(&mut session).await;
        let result = self
            .reply_taken(session_id, &mut session, text, mode, progress)
            .await;

        self.commit_session(session, generation)?;
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

        if action == Action::Goal {
            session.goal_active = true;
            session.goal_paused = false;
            session.goal_status = loopbiotic_protocol::GoalStatus::Active;
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
        session.interaction_feedback.clear();

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
        selected_mode: Mode,
        progress: Option<ProgressReporter>,
    ) -> Result<ActionResult> {
        if text.trim().is_empty() {
            return Err(anyhow!("reply is empty"));
        }

        let context = session.context.clone();
        if session.goal_active || session.goal_paused {
            session.goal_active = false;
            session.goal_paused = true;
            session.goal_status = loopbiotic_protocol::GoalStatus::Paused;
        }
        session.mode = selected_mode;
        let expected = if matches!(session.mode, Mode::Fix | Mode::Propose) {
            NextState::Patch
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
        session.interaction_feedback.clear();

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
        let session_id = result.session_id.clone();
        let generation = self.begin_turn(&session_id)?;
        self.apply_result_with_progress_generation(result, generation, progress)
            .await
    }

    pub async fn apply_result_with_progress_generation(
        &mut self,
        result: PatchApplyResult,
        generation: u64,
        progress: Option<ProgressReporter>,
    ) -> Result<ActionResult> {
        let session_id = result.session_id.clone();
        let mut effective_generation = generation;
        let mut session = self.session_for_turn(&session_id, generation)?;
        let output = self
            .apply_result_taken(&mut session, result, &mut effective_generation, progress)
            .await;

        self.commit_session(session, effective_generation)?;
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
        generation: &mut u64,
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
            let was_goal_active = session.goal_active;
            let completed_steps = completed_patch_steps(session);
            let completed_step_signatures = completed_patch_signatures(session);
            session.completed_steps.extend(completed_steps);
            session
                .completed_step_signatures
                .extend(completed_step_signatures);
            session.accepted_patches.extend(result.patch_ids.clone());
            // Accept means “continue solving”, not “show me a receipt”. The
            // patch card already explained the reviewed change, so every
            // accepted patch enters the goal loop and either surfaces the next
            // proposal or completes silently in the editor.
            session.goal_active = true;
            session.goal_paused = false;
            session.goal_status = loopbiotic_protocol::GoalStatus::Active;
            session.next_step = None;
            if let Some(next) = session.pending_patch_cards.pop_front() {
                session.state = SessionState::PatchShown;
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
                if !was_goal_active {
                    self.cancel_accept_continuation(session).await;
                }
                return Ok(complete_goal_locally(&session_id, session));
            }

            // Persist the accepted patch before awaiting any speculative or
            // live continuation. The daemon may return a Working card and
            // later cancel that task; cancellation must never make an
            // already-applied patch look pending again.
            session.state = SessionState::CardShown;
            self.commit_session(session.clone(), *generation)?;
            *generation = self.begin_turn(&session_id)?;
            session.turn_generation = *generation;
            session.state = SessionState::Thinking;

            // Explicit goal slices use their planned continuation. An
            // ordinary patch consumes the acceptance continuation prepared
            // while the user reviewed it. Either path is revalidated against
            // the freshly applied editor context before it can surface.
            let speculated = if was_goal_active {
                self.take_goal_continuation(&session_id).await
            } else {
                self.take_accept_continuation(session).await
            };
            return self
                .goal_turn_taken(
                    &session_id,
                    session,
                    BackendAction::User(Action::Goal),
                    progress,
                    speculated,
                )
                .await;
        }

        session.rejected_patches.extend(result.patch_ids.clone());
        self.cancel_accept_continuation(session).await;
        session.pending_patch_cards.clear();
        session.goal_slice_continues = false;
        session.next_step = None;
        // Rejecting is a local review decision, not permission to spend
        // another model turn. Drop the stale continuation and leave an
        // explicit Retry action if the user wants a replacement draft.
        self.cancel_goal_continuation(session).await;

        session.state = if session.goal_active {
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
        session.interaction_feedback.clear();
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

    pub fn working_result(
        &self,
        session_id: &str,
        turn_id: &str,
        deadline_ms: u64,
    ) -> Result<ActionResult> {
        let session = self
            .sessions
            .get(session_id)
            .ok_or_else(|| anyhow!("unknown session {session_id}"))?;
        let card = Card::Working(WorkingCard {
            id: format!("c_working_{}", session.cards.len() + 1),
            turn_id: turn_id.into(),
            title: "Agent still working".into(),
            phase: "working".into(),
            message:
                "The response exceeded its interaction budget and is continuing in the background."
                    .into(),
            elapsed_ms: deadline_ms,
            deadline_ms,
            actions: vec![Action::CancelTurn, Action::Stop],
        });

        Ok(ActionResult {
            session_id: session_id.into(),
            card,
            goal: goal_progress(session),
            token_usage: session.token_usage.clone(),
            turn_token_usage: TokenUsage::default(),
            context_report: session.context.report.clone(),
            model: None,
            attempts: vec![],
        })
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

    fn profile_project(
        &self,
        root: &std::path::Path,
        signals: &loopbiotic_protocol::ProjectSignals,
    ) -> loopbiotic_protocol::ProjectProfile {
        tokio::task::block_in_place(|| self.project_profiler.inspect(root, signals))
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
        let card_contract = CardContract {
            expected_kind,
            allow_goal_completion,
            conversation_only: matches!(expected, NextState::Conversation),
            ..CardContract::default()
        };

        BackendRequest {
            session: SessionSnapshot {
                id: session.id.clone(),
                prompt: session.original_prompt.clone(),
                interaction_feedback: session.interaction_feedback.clone(),
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
                project: session.project.clone(),
                skills: session.skills.clone(),
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
        match &mut received {
            Card::Hypothesis(card) if !card.actions.contains(&Action::Goal) => {
                let index = card
                    .actions
                    .iter()
                    .position(|action| *action == Action::Stop)
                    .unwrap_or(card.actions.len());
                card.actions.insert(index, Action::Goal);
            }
            Card::Finding(card) if !card.actions.contains(&Action::Goal) => {
                let index = card
                    .actions
                    .iter()
                    .position(|action| *action == Action::Stop)
                    .unwrap_or(card.actions.len());
                card.actions.insert(index, Action::Goal);
            }
            _ => {}
        }
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

    pub fn begin_turn(&mut self, session_id: &str) -> Result<u64> {
        let session = self
            .sessions
            .get_mut(session_id)
            .ok_or_else(|| anyhow!("unknown session {session_id}"))?;
        session.turn_generation += 1;

        Ok(session.turn_generation)
    }

    fn session_for_turn(&self, session_id: &str, generation: u64) -> Result<Session> {
        let session = self
            .sessions
            .get(session_id)
            .ok_or_else(|| anyhow!("unknown session {session_id}"))?;
        if session.turn_generation != generation {
            return Err(anyhow!("turn {generation} for {session_id} was superseded"));
        }

        Ok(session.clone())
    }

    fn commit_session(&mut self, session: Session, generation: u64) -> Result<()> {
        let current = self
            .sessions
            .get(&session.id)
            .ok_or_else(|| anyhow!("unknown session {}", session.id))?;
        if current.turn_generation != generation {
            return Err(anyhow!(
                "turn {generation} for {} was superseded",
                session.id
            ));
        }
        self.sessions.insert(session.id.clone(), session);

        Ok(())
    }

    pub async fn cancel_turn(&mut self, session_id: &str) -> Result<ActionResult> {
        let mut session = self
            .sessions
            .get(session_id)
            .cloned()
            .ok_or_else(|| anyhow!("unknown session {session_id}"))?;
        session.turn_generation += 1;
        self.cancel_goal_continuation(&mut session).await;
        self.cancel_accept_continuation(&mut session).await;
        if session.goal_active {
            session.goal_active = false;
            session.goal_paused = true;
            session.goal_status = loopbiotic_protocol::GoalStatus::Paused;
        }
        session.state = session
            .cards
            .last()
            .map(SessionState::from_card)
            .unwrap_or(SessionState::CardShown);
        let accepted_patch_waiting_for_conversation = session.state == SessionState::PatchShown
            && session.cards.last().is_some_and(|card| {
                let Card::Patch(card) = card else {
                    return false;
                };
                card.patches
                    .iter()
                    .all(|patch| session.accepted_patches.contains(&patch.id))
            });
        let card = if accepted_patch_waiting_for_conversation {
            session.state = SessionState::CardShown;
            let card = Card::Finding(FindingCard {
                id: session.next_card_id("accepted_cancelled"),
                title: "Continuation cancelled".into(),
                finding: "The automatic continuation was cancelled. The reviewed change remains applied; send a message or explicitly choose the next step.".into(),
                location: None,
                annotation: None,
                flow_path: vec![],
                actions: vec![
                    Action::Follow,
                    Action::Fix,
                    Action::Goal,
                    Action::RunCheck,
                    Action::Stop,
                ],
            });
            session.cards.push(card.clone());
            card
        } else {
            session.cards.last().cloned().unwrap_or_else(|| {
                Card::Error(ErrorCard {
                    id: session.next_card_id("cancelled"),
                    title: "Turn cancelled".into(),
                    message: "Agent thinking was cancelled. Send a message or retry when ready."
                        .into(),
                    actions: vec![Action::Retry, Action::EditPrompt, Action::Stop],
                })
            })
        };
        let result = ActionResult {
            session_id: session_id.into(),
            card,
            goal: goal_progress(&session),
            token_usage: session.token_usage.clone(),
            turn_token_usage: TokenUsage::default(),
            context_report: session.context.report.clone(),
            model: None,
            attempts: vec![],
        };
        self.sessions.insert(session_id.into(), session);

        Ok(result)
    }

    fn add_usage(&self, session: &mut Session, usage: &Option<loopbiotic_protocol::TokenUsage>) {
        if let Some(usage) = usage {
            session.token_usage.add(usage);
        }
    }
}

fn expected_start_state(session: &Session) -> NextState {
    if matches!(session.mode, Mode::Fix | Mode::Propose) {
        NextState::Patch
    } else {
        NextState::Any
    }
}

/// None means the agent may answer with whichever card kind fits, including a
/// clarifying choice or a deny. A kind is demanded by the visible user-selected
/// mode, a concrete action such as Fix, or the state machine.
fn expected_card_kind(
    session: &Session,
    action: &BackendAction,
    next_state: &NextState,
) -> Option<CardKind> {
    match next_state {
        NextState::Patch => return Some(CardKind::Patch),
        NextState::Conversation => return None,
        NextState::GoalLoop => return None,
        NextState::GoalWhy => return Some(CardKind::Finding),
        NextState::Summary | NextState::Finished => return Some(CardKind::Summary),
        NextState::Any | NextState::Card => {}
    }

    match action {
        BackendAction::Start => match session.mode {
            Mode::Fix | Mode::Propose => Some(CardKind::Patch),
            Mode::Explain | Mode::Review => Some(CardKind::Finding),
            Mode::Investigate => Some(CardKind::Hypothesis),
        },
        BackendAction::Reply(_) => match session.mode {
            Mode::Fix | Mode::Propose => Some(CardKind::Patch),
            Mode::Explain | Mode::Review => Some(CardKind::Finding),
            Mode::Investigate => Some(CardKind::Hypothesis),
        },
        BackendAction::ContractRetry(_) => None,
        BackendAction::LocationGranted => None,
        BackendAction::User(action) => match action {
            Action::Fix => Some(CardKind::Patch),
            Action::Goal | Action::CancelTurn => None,
            Action::OtherLead => Some(CardKind::Hypothesis),
            Action::Follow | Action::Why | Action::Open | Action::RunCheck | Action::Next => {
                Some(CardKind::Finding)
            }
            Action::Retry | Action::EditPrompt => session
                .cards
                .iter()
                .rev()
                .find(|card| !matches!(card, Card::Error(_) | Card::Deny(_)))
                .map(Card::kind),
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
        Card::Working(card) => format!("working: {}", card.message),
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
