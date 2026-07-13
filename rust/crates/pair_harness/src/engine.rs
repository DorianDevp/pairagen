use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use anyhow::{Result, anyhow};
use pair_backends::{
    BackendAction, BackendAdapter, BackendProgress, BackendRequest, BackendResponse, CardContract,
    ProgressReporter, SessionSnapshot,
};
use pair_context::ContextOptimizer;
use pair_patch::{PatchCoherence, PatchNormalizer, PatchValidator};
use pair_protocol::{
    Action, ActionResult, AgentAttempt, Card, CardKind, ContextBundle, ErrorCard, GoalProgress,
    MAX_GOAL_CHANGED_LINES, MAX_GOAL_HUNKS_PER_PATCH, MAX_GOAL_PATCH_FILES, Mode, ObservationKind,
    ObservationProgress, PatchApplyResult, StartSessionParams, StartSessionResult, SummaryCard,
    TokenUsage,
};

use crate::session::Session;
use crate::state::{NextState, SessionState};

pub struct Engine {
    backend: Arc<dyn BackendAdapter>,
    sessions: HashMap<String, Session>,
    context_optimizer: ContextOptimizer,
    prefetch_mode: PrefetchMode,
    prefetches: HashMap<String, Prefetch>,
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
            pair_protocol::OpenLocationCard,
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

/// Speculative prefetch of the likely next card. `Fix` requests the patch
/// card in the background while the user is still reading a discovery card,
/// so pressing Fix returns (near-)instantly on a hit.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PrefetchMode {
    Off,
    Fix,
}

struct Prefetch {
    action: Action,
    fingerprint: u64,
    handle: tokio::task::JoinHandle<Result<BackendResponse>>,
}

impl Engine {
    pub fn new(backend: Arc<dyn BackendAdapter>) -> Self {
        Self {
            backend,
            sessions: HashMap::new(),
            context_optimizer: ContextOptimizer::default(),
            prefetch_mode: PrefetchMode::Off,
            prefetches: HashMap::new(),
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
        let context = self.context_optimizer.optimize(
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
        let prefetched = self.take_prefetch(&mut session, &action).await;
        let result = self
            .action_taken(session_id, &mut session, action, progress, prefetched)
            .await;

        self.sessions.insert(session_id.into(), session);
        if result.is_ok() {
            self.schedule_prefetch(session_id).await;
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
        let result = self
            .reply_taken(session_id, &mut session, text, progress)
            .await;

        self.sessions.insert(session_id.into(), session);
        if result.is_ok() {
            self.schedule_prefetch(session_id).await;
        }

        result
    }

    async fn action_taken(
        &self,
        session_id: &str,
        session: &mut Session,
        action: Action,
        progress: Option<ProgressReporter>,
        prefetched: Option<BackendResponse>,
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
            session.pending_patch_cards.clear();
        }

        let state = session.state.next(&action)?;
        if action == Action::Stop {
            session.state = SessionState::Finished;
            session.goal_status = pair_protocol::GoalStatus::Stopped;
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

        self.sessions.insert(session_id, session);

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
        session.context = self.context_optimizer.optimize(
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
            session.goal_status = pair_protocol::GoalStatus::NeedsReview;
            session.next_step = None;
            if session.continuous_goal {
                if let Some(next) = session.pending_patch_cards.pop_front() {
                    session.state = SessionState::PatchShown;
                    session.goal_status = pair_protocol::GoalStatus::Active;
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
                return self
                    .goal_turn_taken(
                        &session_id,
                        session,
                        BackendAction::User(Action::Next),
                        progress,
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
        session.state = SessionState::PatchShown;

        if session.continuous_goal {
            return self
                .goal_turn_taken(
                    &session_id,
                    session,
                    BackendAction::User(Action::Retry),
                    progress,
                )
                .await;
        }

        self.action_taken(&session_id, session, Action::Retry, progress, None)
            .await
    }

    async fn goal_turn_taken(
        &self,
        session_id: &str,
        session: &mut Session,
        action: BackendAction,
        progress: Option<ProgressReporter>,
    ) -> Result<ActionResult> {
        let expected = NextState::GoalLoop;
        let context = session.context.clone();
        session.state = SessionState::Thinking;
        let response = self
            .next_distinct_response(session, action, context, &expected, progress, None)
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
        let context = self.context_optimizer.optimize(context, &prompt, &policy);
        self.sessions
            .get_mut(session_id)
            .expect("session checked above")
            .context = context;

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

    async fn next_distinct_response(
        &self,
        session: &mut Session,
        action: BackendAction,
        context: ContextBundle,
        expected: &NextState,
        progress: Option<ProgressReporter>,
        mut prefetched: Option<BackendResponse>,
    ) -> BackendResponse {
        let mut action = action;
        let mut context = context;
        let mut token_usage = None;
        let mut attempts = Vec::new();
        let mut attempt = 0;
        let mut grants = 0;

        while attempt < 3 {
            let attempt_response = match prefetched.take() {
                // A matching speculative response was computed for this exact
                // request while the user was reading the previous card; it
                // still goes through every dedup/validation gate below.
                Some(response) => Ok(response),
                None => {
                    let request = self.request(session, action, context.clone(), expected);
                    self.backend
                        .next_card_with_progress(request, progress.clone())
                        .await
                }
            };
            let mut response = match attempt_response {
                Ok(response) => response,
                Err(error) => {
                    let detail = format!("{error:#}");
                    let mut response = backend_failure_response(session, error);
                    merge_usage(&mut token_usage, &response.metadata.token_usage);
                    response.metadata.token_usage = token_usage;
                    attempts.push(AgentAttempt {
                        number: attempt + 1,
                        backend: response.metadata.backend.clone(),
                        outcome: "backend_error".into(),
                        token_usage: TokenUsage::default(),
                        detail: Some(detail),
                        candidate_card: None,
                        activities: vec![],
                    });
                    response.metadata.attempts = attempts;
                    return response;
                }
            };
            let attempt_usage = response.metadata.token_usage.clone().unwrap_or_default();
            merge_usage(&mut token_usage, &response.metadata.token_usage);

            // A mid-turn permission request: the agent needs another file open
            // before it can produce the real card. Ask the editor; on grant the
            // same turn continues with the freshly captured context. Grants do
            // not consume retry attempts.
            if let Card::OpenLocation(request) = &response.card {
                let request = request.clone();

                if grants < MAX_LOCATION_GRANTS
                    && let Some(granter) = &self.location_granter
                {
                    if let Some(progress) = &progress {
                        progress(BackendProgress {
                            session_id: session.id.clone(),
                            phase: "permission".into(),
                            message: format!(
                                "Agent asks to open {}",
                                request.location.file.display()
                            ),
                        });
                    }

                    if let Some(granted) = granter(request.clone(), session.id.clone()).await {
                        attempts.push(agent_attempt(
                            attempt + 1,
                            &response,
                            "location_granted",
                            Some(request.location.file.display().to_string()),
                            attempt_usage,
                            false,
                        ));
                        session.context = granted.clone();
                        context = granted;
                        action = BackendAction::LocationGranted;
                        grants += 1;
                        continue;
                    }
                }

                attempts.push(agent_attempt(
                    attempt + 1,
                    &response,
                    "location_declined",
                    Some(request.location.file.display().to_string()),
                    attempt_usage,
                    false,
                ));
                response.card = Card::Deny(pair_protocol::DenyCard {
                    id: session.next_card_id("deny"),
                    title: "Agent needs another file".into(),
                    reason: request.reason,
                    location: Some(request.location),
                    actions: vec![Action::Retry, Action::EditPrompt, Action::Stop],
                });
                response.metadata.token_usage = token_usage;
                response.metadata.attempts = attempts;
                return response;
            }

            if !matches!(expected, NextState::GoalWhy)
                && let Some((key, reason)) = duplicate_observation(session, &response.card)
            {
                activate_observation(session, &key);
                attempts.push(agent_attempt(
                    attempt + 1,
                    &response,
                    if attempt < 2 {
                        "duplicate_retry"
                    } else {
                        "rejected"
                    },
                    Some(reason.clone()),
                    attempt_usage,
                    true,
                ));
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
                    attempt += 1;
                    continue;
                }

                let mut rejected = duplicate_failure_response(session, reason);
                rejected.metadata.token_usage = token_usage;
                rejected.metadata.attempts = attempts;
                return rejected;
            }

            if let Some(reason) = duplicate_completed_step(session, &response.card) {
                attempts.push(agent_attempt(
                    attempt + 1,
                    &response,
                    if attempt < 2 {
                        "duplicate_step_retry"
                    } else {
                        "rejected"
                    },
                    Some(reason.clone()),
                    attempt_usage,
                    true,
                ));
                if attempt < 2 {
                    if let Some(progress) = &progress {
                        progress(BackendProgress {
                            session_id: session.id.clone(),
                            phase: "deduplicating".into(),
                            message: "Rejecting a repeated patch step".into(),
                        });
                    }
                    action = BackendAction::ContractRetry(format!(
                        "{reason}. Draft a materially different unresolved requirement. Do not merely rename, extract, or rephrase the accepted step. If this location is already resolved, return open_location for the actual next target or deny instead of inventing another patch here."
                    ));
                    attempt += 1;
                    continue;
                }

                let mut rejected = duplicate_failure_response(session, reason);
                rejected.metadata.token_usage = token_usage;
                rejected.metadata.attempts = attempts;
                return rejected;
            }

            let mut candidate = response.card.clone();
            let validation = if matches!(expected, NextState::GoalLoop) {
                self.normalize_goal_batch(&mut candidate, &context, &session.id)
                    .await
            } else {
                PatchNormalizer::normalize_card(&mut candidate, &context)
                    .and_then(|()| validate_backend_card(&candidate, expected, &context))
            }
            .map(|()| PatchCoherence::annotate(&mut candidate));
            if let Err(error) = validation {
                let detail = error.to_string();
                attempts.push(agent_attempt(
                    attempt + 1,
                    &response,
                    if attempt < 2 {
                        "contract_retry"
                    } else {
                        "rejected"
                    },
                    Some(detail.clone()),
                    attempt_usage,
                    true,
                ));
                if attempt < 2 {
                    if let Some(progress) = &progress {
                        progress(BackendProgress {
                            session_id: session.id.clone(),
                            phase: "repairing".into(),
                            message: "Patch contract failed; Codex is repairing the local step"
                                .into(),
                        });
                    }
                    let instruction = if matches!(expected, NextState::GoalLoop) {
                        "Re-read every affected file with read-only tools and return the corrected complete multi-file batch. Context/remove lines must be exact and contiguous in each corresponding source. Do not split the goal into another model turn; use open_location only if a required source cannot be inspected."
                    } else {
                        "Rebuild the same step. Source context/remove lines must be exact and contiguous in the supplied buffer; added lines do not replace omitted source context. The resulting local step must remain type-correct without work deferred to a later card. If the change belongs in a different file than the supplied buffer, return an open_location op with that place instead of another patch."
                    };
                    action = BackendAction::ContractRetry(format!(
                        "The previous card failed the local patch contract: {detail}. {instruction}"
                    ));
                    attempt += 1;
                    continue;
                }

                response.card =
                    rejected_card(session, &candidate, error, response.raw_output.as_deref());
                response.metadata.token_usage = token_usage;
                response.metadata.attempts = attempts;
                return response;
            }

            response.card = candidate;
            attempts.push(agent_attempt(
                attempt + 1,
                &response,
                "accepted",
                None,
                attempt_usage,
                false,
            ));
            response.metadata.token_usage = token_usage;
            response.metadata.attempts = attempts;
            return response;
        }

        unreachable!()
    }

    async fn normalize_goal_batch(
        &self,
        candidate: &mut Card,
        current: &ContextBundle,
        session_id: &str,
    ) -> Result<()> {
        if !matches!(candidate, Card::Patch(_)) {
            return validate_backend_card(candidate, &NextState::GoalLoop, current);
        }

        validate_one_card(candidate)?;
        PatchValidator::validate_card_with_limits(
            candidate,
            MAX_GOAL_PATCH_FILES,
            MAX_GOAL_HUNKS_PER_PATCH,
            MAX_GOAL_CHANGED_LINES,
        )?;
        let Card::Patch(card) = candidate else {
            unreachable!();
        };

        for index in 0..card.patches.len() {
            let file = card.patches[index].file.clone();
            let source = if let Some(provider) = &self.source_context_provider {
                provider(file.clone(), session_id.to_string()).await
            } else if context_targets(current, &file) {
                Some(current.clone())
            } else {
                None
            }
            .ok_or_else(|| anyhow!("editor source is unavailable for {}", file.display()))?;

            if !context_targets(&source, &file) {
                return Err(anyhow!(
                    "editor returned {} while validating {}",
                    source.file.display(),
                    file.display()
                ));
            }

            let mut single = Card::Patch(pair_protocol::PatchCard {
                id: card.id.clone(),
                title: card.title.clone(),
                explanation: card.explanation.clone(),
                warnings: vec![],
                goal_complete: card.goal_complete,
                patches: vec![card.patches[index].clone()],
                actions: card.actions.clone(),
            });
            PatchNormalizer::normalize_card(&mut single, &source)
                .map_err(|error| anyhow!("{}: {error}", file.display()))?;
            PatchValidator::validate_card_against_context(&single, &source)
                .map_err(|error| anyhow!("{}: {error}", file.display()))?;

            let Card::Patch(single) = single else {
                unreachable!();
            };
            card.patches[index] = single.patches.into_iter().next().unwrap();
        }

        NextState::GoalLoop.validate(candidate)
    }

    fn accept_response(
        &self,
        session: &mut Session,
        response: BackendResponse,
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

    fn add_usage(&self, session: &mut Session, usage: &Option<pair_protocol::TokenUsage>) {
        if let Some(usage) = usage {
            session.token_usage.add(usage);
        }
    }

    /// Requests the likely next card in the background while the user reads
    /// the one just shown. Only Fix is predicted: it is the most common and
    /// slowest follow-up, and on backends with a separate patch process a
    /// misprediction never blocks the user's real next request.
    async fn schedule_prefetch(&mut self, session_id: &str) {
        if self.prefetch_mode != PrefetchMode::Fix {
            return;
        }

        if let Some(existing) = self.prefetches.get(session_id) {
            if !existing.handle.is_finished() {
                // An earlier speculation is still running on the backend;
                // queueing another would only pile up turns.
                return;
            }
            // Fold the finished-but-unconsumed speculation into the session's
            // token totals so wasted turns stay visible to the user.
            if let Some(stale) = self.prefetches.remove(session_id)
                && let Ok(Ok(response)) = stale.handle.await
                && let Some(session) = self.sessions.get_mut(session_id)
            {
                fold_usage(session, &response.metadata.token_usage);
            }
        }

        let Some(session) = self.sessions.get(session_id) else {
            return;
        };
        if session.state != SessionState::CardShown {
            return;
        }
        let Some(card) = session.cards.last() else {
            return;
        };
        if !card.actions().contains(&Action::Fix) {
            return;
        }
        let Ok(expected) = session.state.next(&Action::Fix) else {
            return;
        };

        let request = self.request(
            session,
            BackendAction::User(Action::Fix),
            session.context.clone(),
            &expected,
        );
        let fingerprint = request_fingerprint(&request);
        let backend = self.backend.clone();
        let handle = tokio::spawn(async move { backend.next_card(request).await });

        self.prefetches.insert(
            session_id.to_string(),
            Prefetch {
                action: Action::Fix,
                fingerprint,
                handle,
            },
        );
    }

    /// Consumes a pending speculation if it was computed for exactly the
    /// request this action would produce; otherwise leaves the real path
    /// untouched and keeps the wasted tokens accounted for.
    async fn take_prefetch(
        &mut self,
        session: &mut Session,
        action: &Action,
    ) -> Option<BackendResponse> {
        let entry = self.prefetches.remove(&session.id)?;

        if entry.action == *action
            && let Ok(expected) = session.state.next(action)
        {
            let request = self.request(
                session,
                BackendAction::User(action.clone()),
                session.context.clone(),
                &expected,
            );
            if request_fingerprint(&request) == entry.fingerprint {
                return match entry.handle.await {
                    Ok(Ok(response)) => Some(response),
                    _ => None,
                };
            }
            if entry.handle.is_finished() {
                if let Ok(Ok(response)) = entry.handle.await {
                    fold_usage(session, &response.metadata.token_usage);
                }
                return None;
            }
            self.prefetches.insert(session.id.clone(), entry);
            return None;
        }

        if entry.handle.is_finished() {
            if let Ok(Ok(response)) = entry.handle.await {
                fold_usage(session, &response.metadata.token_usage);
            }
        } else {
            self.prefetches.insert(session.id.clone(), entry);
        }

        None
    }
}

fn fold_usage(session: &mut Session, usage: &Option<pair_protocol::TokenUsage>) {
    if let Some(usage) = usage {
        session.token_usage.add(usage);
    }
}

fn request_fingerprint(request: &BackendRequest) -> u64 {
    use std::hash::{DefaultHasher, Hash, Hasher};

    // Only model-visible data may decide whether a speculative response
    // matches: the optimizer report (cache counters vary run to run) and raw
    // LSP hints are telemetry that backend_context strips before the model
    // ever sees them.
    let mut request = request.clone();
    request.context.report = None;
    request.context.hints = vec![];

    let mut hasher = DefaultHasher::new();
    serde_json::to_string(&request)
        .unwrap_or_default()
        .hash(&mut hasher);
    hasher.finish()
}

fn agent_attempt(
    number: usize,
    response: &BackendResponse,
    outcome: &str,
    detail: Option<String>,
    token_usage: TokenUsage,
    include_candidate: bool,
) -> AgentAttempt {
    AgentAttempt {
        number,
        backend: response.metadata.backend.clone(),
        outcome: outcome.into(),
        token_usage,
        detail,
        candidate_card: include_candidate.then(|| response.card.clone()),
        activities: response.metadata.activities.clone(),
    }
}

fn goal_progress(session: &Session) -> GoalProgress {
    GoalProgress {
        statement: session.original_prompt.clone(),
        completed_steps: session.completed_steps.clone(),
        known_observations: session.known_observations.clone(),
        status: session.goal_status,
        next_step: session.next_step.clone(),
    }
}

fn complete_goal_locally(session_id: &str, session: &mut Session) -> ActionResult {
    let mut changed_files = session
        .completed_step_signatures
        .iter()
        .map(|(file, _)| file.clone())
        .collect::<Vec<_>>();
    changed_files.sort();
    changed_files.dedup();

    session.state = SessionState::Summary;
    session.goal_status = pair_protocol::GoalStatus::Complete;
    session.next_step = None;
    let card = Card::Summary(SummaryCard {
        id: session.next_card_id("complete"),
        title: "Goal complete".into(),
        summary: format!(
            "Completed {} reviewed change{} for: {}",
            session.completed_steps.len(),
            if session.completed_steps.len() == 1 {
                ""
            } else {
                "s"
            },
            session.original_prompt
        ),
        changed_files,
        next_actions: vec![Action::RunCheck, Action::Stop],
    });
    session.cards.push(card.clone());

    ActionResult {
        session_id: session_id.into(),
        card,
        goal: goal_progress(session),
        token_usage: session.token_usage.clone(),
        turn_token_usage: TokenUsage::default(),
        context_report: session.context.report.clone(),
        model: None,
        attempts: vec![],
    }
}

fn update_goal_state(session: &mut Session, card: &Card, next_state: &NextState) {
    if !matches!(next_state, NextState::GoalLoop) {
        return;
    }

    match card {
        Card::Patch(card) => {
            session.goal_status = pair_protocol::GoalStatus::Active;
            session.next_step = Some(card.explanation.clone());
        }
        Card::Summary(_) => {
            session.goal_status = pair_protocol::GoalStatus::Complete;
            session.next_step = None;
        }
        Card::Finding(card) => {
            session.goal_status = pair_protocol::GoalStatus::Active;
            session.next_step = Some(card.finding.clone());
        }
        Card::Hypothesis(card) => {
            session.goal_status = pair_protocol::GoalStatus::Active;
            session.next_step = Some(card.claim.clone());
        }
        Card::Choice(_) => {
            session.goal_status = pair_protocol::GoalStatus::Active;
            session.next_step = None;
        }
        _ => {}
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

fn duplicate_completed_step(session: &Session, card: &Card) -> Option<String> {
    let Card::Patch(card) = card else {
        return None;
    };

    card.patches.iter().find_map(|patch| {
        let candidate = normalize_step(&patch.explanation);
        session
            .completed_step_signatures
            .iter()
            .filter(|(file, _)| file == &patch.file)
            .find(|(_, completed)| step_similarity(completed, &candidate) >= 0.72)
            .map(|_| {
                format!(
                    "backend proposed a patch semantically overlapping an accepted step in {}",
                    patch.file.display()
                )
            })
    })
}

fn normalize_step(text: &str) -> String {
    const STOP_WORDS: &[&str] = &[
        "add", "adds", "and", "dla", "dodaje", "do", "for", "from", "into", "oraz", "the", "this",
        "that", "to", "use", "uses", "with", "zmienia",
    ];

    normalize_observation(text)
        .split_whitespace()
        .filter(|word| word.chars().count() > 2 && !STOP_WORDS.contains(word))
        .collect::<Vec<_>>()
        .join(" ")
}

fn step_similarity(left: &str, right: &str) -> f32 {
    let left = left
        .split_whitespace()
        .collect::<std::collections::HashSet<_>>();
    let right = right
        .split_whitespace()
        .collect::<std::collections::HashSet<_>>();
    if left.is_empty() || right.is_empty() {
        return 0.0;
    }

    let shared = left.intersection(&right).count() as f32;
    shared / left.len().min(right.len()) as f32
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

    // A denial is valid in any state: the agent is telling the user it cannot
    // produce the expected card, so only the card text itself is checked.
    if matches!(card, Card::Deny(_)) {
        return validate_one_card(card);
    }

    validate_one_card(card)?;
    if matches!(next_state, NextState::GoalLoop) {
        PatchValidator::validate_card_with_limits(
            card,
            MAX_GOAL_PATCH_FILES,
            MAX_GOAL_HUNKS_PER_PATCH,
            MAX_GOAL_CHANGED_LINES,
        )?;
    } else {
        PatchValidator::validate_card(card)?;
    }
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

fn context_targets(context: &ContextBundle, file: &std::path::Path) -> bool {
    let actual = if context.file.is_absolute() {
        context
            .file
            .strip_prefix(&context.cwd)
            .unwrap_or(&context.file)
    } else {
        &context.file
    };

    actual == file
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
        Card::Deny(card) => {
            require_text("card title", &card.title)?;
            require_text("deny reason", &card.reason)?;
            if let Some(location) = &card.location {
                validate_location(
                    &location.file,
                    location.line,
                    location.column,
                    "deny location",
                )?;
            }
        }
        Card::OpenLocation(card) => {
            require_text("open_location reason", &card.reason)?;
            validate_location(
                &card.location.file,
                card.location.line,
                card.location.column,
                "open_location",
            )?;
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

fn queue_goal_patch_cards(session: &mut Session, card: Card) -> Result<Card> {
    let Card::Patch(card) = card else {
        session.pending_patch_cards.clear();
        return Ok(card);
    };

    let mut cards = Vec::new();
    for patch in card.patches {
        let diff = pair_patch::UnifiedDiff::parse(&patch.diff)?;
        let hunk_count = diff.hunks.len();
        for (index, hunk) in diff.hunks.into_iter().enumerate() {
            let suffix = if hunk_count == 1 {
                String::new()
            } else {
                format!(" ({}/{hunk_count})", index + 1)
            };
            let explanation = if hunk_count == 1 {
                patch.explanation.clone()
            } else {
                format!("{} Hunk {}/{}.", patch.explanation, index + 1, hunk_count)
            };
            cards.push(pair_protocol::PatchCard {
                id: format!("{}_h{}", card.id, cards.len() + 1),
                title: format!("{}{}", card.title, suffix),
                explanation: explanation.clone(),
                warnings: card.warnings.clone(),
                goal_complete: false,
                patches: vec![pair_protocol::FilePatch {
                    id: format!("{}_h{}", patch.id, index + 1),
                    file: patch.file.clone(),
                    diff: pair_patch::UnifiedDiff { hunks: vec![hunk] }.render(),
                    explanation,
                }],
                actions: card.actions.clone(),
            });
        }
    }

    let mut cards = cards.into_iter();
    let mut first = cards
        .next()
        .ok_or_else(|| anyhow!("goal patch contains no reviewable hunks"))?;
    let mut pending = cards.collect::<VecDeque<_>>();
    if card.goal_complete {
        if let Some(last) = pending.back_mut() {
            last.goal_complete = true;
        } else {
            first.goal_complete = true;
        }
    }
    session.pending_patch_cards = pending;

    Ok(Card::Patch(first))
}

fn completed_patch_signatures(session: &Session) -> Vec<(std::path::PathBuf, String)> {
    let Some(Card::Patch(card)) = session.cards.last() else {
        return vec![];
    };

    card.patches
        .iter()
        .map(|patch| (patch.file.clone(), normalize_step(&patch.explanation)))
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
            model: None,
            token_usage: None,
            activities: vec![],
            attempts: vec![],
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
            model: None,
            token_usage: None,
            activities: vec![],
            attempts: vec![],
        },
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

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

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
            mode: Mode::Investigate,
            buffer_text: "placeholder".into(),
            buffer_start_line: 1,
            diagnostics: vec![],
            hints: vec![],
            context_policy: Default::default(),
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
            hints: vec![],
            artifacts: vec![],
            report: None,
        }
    }

    #[test]
    fn detects_rephrased_completed_patch_in_the_same_file() {
        let mut session = Session::new(params());
        session.completed_step_signatures.push((
            PathBuf::from("src/work.ts"),
            normalize_step("Extract payload validation into a local helper"),
        ));
        let card = Card::Patch(PatchCard {
            id: "c_repeat".into(),
            title: "Extract validation".into(),
            explanation: "Keep validation local.".into(),
            warnings: vec![],
            goal_complete: false,
            patches: vec![FilePatch {
                id: "p_repeat".into(),
                file: PathBuf::from("src/work.ts"),
                diff: "@@ -1,1 +1,1 @@\n-old\n+new\n".into(),
                explanation: "Extract the local helper used for payload validation".into(),
            }],
            actions: vec![Action::Apply],
        });

        assert!(duplicate_completed_step(&session, &card).is_some());
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
    async fn continuous_goal_reviews_a_complete_batch_without_more_model_turns() {
        let backend = Arc::new(BatchGoalBackend::default());
        let mut engine = Engine::new(backend.clone());
        let mut goal = params();
        goal.mode = Mode::Auto;
        goal.buffer_text = "first\nmiddle\nlast".into();
        let first = engine.start(goal).await.unwrap();

        let Card::Patch(first_patch) = &first.card else {
            panic!("expected first review hunk");
        };
        assert_eq!(backend.calls.load(Ordering::SeqCst), 1);

        let result = PatchApplyResult {
            session_id: first.session_id,
            card_id: first.card.id().into(),
            accepted: true,
            patch_ids: vec![first_patch.patches[0].id.clone()],
            changed_files: vec![PathBuf::from("src/work.ts")],
            error: None,
            context: editor_context("FIRST\nmiddle\nlast"),
        };

        let second = engine.apply_result(result).await.unwrap();

        assert!(matches!(second.card, Card::Patch(_)));
        assert_eq!(second.turn_token_usage.total_tokens, 0);
        assert_eq!(backend.calls.load(Ordering::SeqCst), 1);
        assert_eq!(second.goal.status, pair_protocol::GoalStatus::Active);
        assert_eq!(
            engine
                .get(&second.session_id)
                .unwrap()
                .completed_steps
                .len(),
            1
        );

        let Card::Patch(second_patch) = &second.card else {
            unreachable!();
        };
        let result = PatchApplyResult {
            session_id: second.session_id,
            card_id: second.card.id().into(),
            accepted: true,
            patch_ids: vec![second_patch.patches[0].id.clone()],
            changed_files: vec![PathBuf::from("src/work.ts")],
            error: None,
            context: editor_context("FIRST\nmiddle\nLAST"),
        };
        let complete = engine.apply_result(result).await.unwrap();

        assert!(matches!(complete.card, Card::Summary(_)));
        assert_eq!(complete.turn_token_usage.total_tokens, 0);
        assert_eq!(backend.calls.load(Ordering::SeqCst), 1);
        assert_eq!(complete.goal.status, pair_protocol::GoalStatus::Complete);
        assert_eq!(complete.goal.completed_steps.len(), 2);
    }

    #[tokio::test]
    async fn continuous_goal_reviews_a_multi_file_batch_without_more_model_turns() {
        let backend = Arc::new(MultiFileGoalBackend::default());
        let reads = Arc::new(AtomicUsize::new(0));
        let mut engine = Engine::new(backend.clone());
        let observed_reads = reads.clone();
        engine.set_source_context_provider(Arc::new(move |file, _session_id| {
            let observed_reads = observed_reads.clone();
            Box::pin(async move {
                observed_reads.fetch_add(1, Ordering::SeqCst);
                let text = if file == PathBuf::from("src/work.ts") {
                    "first"
                } else {
                    "other"
                };
                let mut context = editor_context(text);
                context.file = file;
                Some(context)
            })
        }));
        let mut goal = params();
        goal.mode = Mode::Auto;
        goal.buffer_text = "first".into();

        let first = engine.start(goal).await.unwrap();
        let Card::Patch(first_patch) = &first.card else {
            panic!(
                "expected first file hunk, got {:?}; attempts {:?}",
                first.card, first.attempts
            );
        };
        assert_eq!(first_patch.patches[0].file, PathBuf::from("src/work.ts"));
        assert!(!first_patch.goal_complete);
        assert_eq!(backend.calls.load(Ordering::SeqCst), 1);
        assert_eq!(reads.load(Ordering::SeqCst), 2);

        let second = engine
            .apply_result(PatchApplyResult {
                session_id: first.session_id,
                card_id: first.card.id().into(),
                accepted: true,
                patch_ids: vec![first_patch.patches[0].id.clone()],
                changed_files: vec![PathBuf::from("src/work.ts")],
                error: None,
                context: editor_context("FIRST"),
            })
            .await
            .unwrap();
        let Card::Patch(second_patch) = &second.card else {
            panic!("expected second file hunk");
        };
        assert_eq!(second_patch.patches[0].file, PathBuf::from("src/other.ts"));
        assert!(second_patch.goal_complete);
        assert_eq!(second.turn_token_usage.total_tokens, 0);
        assert_eq!(backend.calls.load(Ordering::SeqCst), 1);

        let mut other_context = editor_context("OTHER");
        other_context.file = PathBuf::from("src/other.ts");
        let complete = engine
            .apply_result(PatchApplyResult {
                session_id: second.session_id,
                card_id: second.card.id().into(),
                accepted: true,
                patch_ids: vec![second_patch.patches[0].id.clone()],
                changed_files: vec![PathBuf::from("src/other.ts")],
                error: None,
                context: other_context,
            })
            .await
            .unwrap();

        assert!(matches!(complete.card, Card::Summary(_)));
        assert_eq!(complete.goal.status, pair_protocol::GoalStatus::Complete);
        assert_eq!(complete.turn_token_usage.total_tokens, 0);
        assert_eq!(backend.calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn continuous_goal_reworks_a_rejected_hunk_without_leaving_the_loop() {
        let backend = Arc::new(MockBackend);
        let mut engine = Engine::new(backend);
        let mut goal = params();
        goal.mode = Mode::Auto;
        let first = engine.start(goal).await.unwrap();
        let Card::Patch(first_patch) = &first.card else {
            panic!("expected patch card");
        };
        let result = PatchApplyResult {
            session_id: first.session_id,
            card_id: first.card.id().into(),
            accepted: false,
            patch_ids: vec![first_patch.patches[0].id.clone()],
            changed_files: vec![],
            error: None,
            context: editor_context("placeholder"),
        };

        let reworked = engine.apply_result(result).await.unwrap();

        assert!(matches!(reworked.card, Card::Patch(_)));
        assert!(reworked.turn_token_usage.total_tokens > 0);
        assert!(reworked.goal.completed_steps.is_empty());
        assert_eq!(reworked.goal.status, pair_protocol::GoalStatus::Active);
    }

    #[tokio::test]
    async fn why_explains_and_restores_the_same_pending_hunk() {
        let backend = Arc::new(MockBackend);
        let mut engine = Engine::new(backend);
        let mut goal = params();
        goal.mode = Mode::Auto;
        let first = engine.start(goal).await.unwrap();
        let patch_id = first.card.id().to_string();

        let explained = engine.action(&first.session_id, Action::Why).await.unwrap();

        let Card::Finding(explanation) = explained.card else {
            panic!("expected patch explanation");
        };
        assert!(explanation.actions.contains(&Action::ResumeDraft));
        assert_eq!(
            engine.get(&first.session_id).unwrap().state,
            SessionState::PatchExplained
        );

        let resumed = engine
            .action(&first.session_id, Action::ResumeDraft)
            .await
            .unwrap();

        assert!(matches!(resumed.card, Card::Patch(_)));
        assert_eq!(resumed.card.id(), patch_id);
        assert_eq!(resumed.turn_token_usage, TokenUsage::default());
        assert_eq!(
            engine.get(&first.session_id).unwrap().state,
            SessionState::PatchShown
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
        assert_eq!(result.attempts.len(), 2);
        assert_eq!(result.attempts[0].outcome, "contract_retry");
        assert!(
            result.attempts[0]
                .detail
                .as_deref()
                .unwrap()
                .contains("patch context was not found")
        );
        assert!(result.attempts[0].candidate_card.is_some());
        assert_eq!(result.attempts[0].token_usage.total_tokens, 15);
        assert_eq!(result.attempts[1].outcome, "accepted");
        assert_eq!(result.turn_token_usage.total_tokens, 30);
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

        assert!(card.message.contains("expected the next goal patch"));
        assert_eq!(
            engine.get(&result.session_id).unwrap().state,
            SessionState::GoalLoopFailed
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
        assert_eq!(result.attempts[0].outcome, "backend_error");
        assert!(
            result.attempts[0]
                .detail
                .as_deref()
                .unwrap()
                .contains("token limit reached")
        );
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
        assert_eq!(result.attempts.len(), 2);
        assert_eq!(result.attempts[0].outcome, "duplicate_retry");
        assert!(result.attempts[0].candidate_card.is_some());
        assert_eq!(result.attempts[1].outcome, "accepted");

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
            hints: vec![],
            artifacts: vec![],
            report: None,
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

    #[derive(Default)]
    struct BatchGoalBackend {
        calls: AtomicUsize,
    }

    #[async_trait]
    impl BackendAdapter for BatchGoalBackend {
        async fn next_card(&self, req: BackendRequest) -> Result<BackendResponse> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            assert!(req.card_contract.allow_goal_completion);
            assert_eq!(req.card_contract.max_patch_files, MAX_GOAL_PATCH_FILES);
            assert_eq!(
                req.card_contract.max_hunks_per_patch,
                MAX_GOAL_HUNKS_PER_PATCH
            );
            assert_eq!(req.card_contract.max_changed_lines, MAX_GOAL_CHANGED_LINES);

            Ok(BackendResponse {
                card: Card::Patch(PatchCard {
                    id: "c_batch".into(),
                    title: "Complete local change".into(),
                    explanation: "Prepare both independent edits.".into(),
                    warnings: vec![],
                    goal_complete: true,
                    patches: vec![FilePatch {
                        id: "p_batch".into(),
                        file: "src/work.ts".into(),
                        diff: "@@ -1,2 +1,2 @@\n-first\n+FIRST\n middle\n@@ -2,2 +2,2 @@\n middle\n-last\n+LAST\n".into(),
                        explanation: "Updates both required locations.".into(),
                    }],
                    actions: vec![Action::Apply, Action::Why, Action::Retry, Action::Stop],
                }),
                raw_output: None,
                metadata: BackendMetadata {
                    backend: "batch_goal".into(),
                    model: None,
                    token_usage: Some(TokenUsage::estimated(100, 20)),
                    activities: vec![],
                    attempts: vec![],
                },
            })
        }

        fn capabilities(&self) -> BackendInfo {
            MockBackend::info()
        }
    }

    #[derive(Default)]
    struct MultiFileGoalBackend {
        calls: AtomicUsize,
    }

    #[async_trait]
    impl BackendAdapter for MultiFileGoalBackend {
        async fn next_card(&self, req: BackendRequest) -> Result<BackendResponse> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            assert_eq!(req.card_contract.max_patch_files, MAX_GOAL_PATCH_FILES);

            Ok(BackendResponse {
                card: Card::Patch(PatchCard {
                    id: "c_multi".into(),
                    title: "Complete workspace change".into(),
                    explanation: "Update both required files.".into(),
                    warnings: vec![],
                    goal_complete: true,
                    patches: vec![
                        FilePatch {
                            id: "p_work".into(),
                            file: "src/work.ts".into(),
                            diff: "@@ -1,1 +1,1 @@\n-first\n+FIRST\n".into(),
                            explanation: "Update the producer.".into(),
                        },
                        FilePatch {
                            id: "p_other".into(),
                            file: "src/other.ts".into(),
                            diff: "@@ -1,1 +1,1 @@\n-other\n+OTHER\n".into(),
                            explanation: "Update the consumer.".into(),
                        },
                    ],
                    actions: vec![Action::Apply, Action::Why, Action::Retry, Action::Stop],
                }),
                raw_output: None,
                metadata: BackendMetadata {
                    backend: "multi_file_goal".into(),
                    model: None,
                    token_usage: Some(TokenUsage::estimated(100, 20)),
                    activities: vec![],
                    attempts: vec![],
                },
            })
        }

        fn capabilities(&self) -> BackendInfo {
            MockBackend::info()
        }
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
                    model: None,
                    token_usage: Some(pair_protocol::TokenUsage::estimated(10, 5)),
                    activities: vec![],
                    attempts: vec![],
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
                    model: None,
                    token_usage: None,
                    activities: vec![],
                    attempts: vec![],
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
                    goal_complete: false,
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
                    model: None,
                    token_usage: None,
                    activities: vec![],
                    attempts: vec![],
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
                        goal_complete: false,
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
                        goal_complete: false,
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
                    model: None,
                    token_usage: Some(pair_protocol::TokenUsage::estimated(10, 5)),
                    activities: vec![],
                    attempts: vec![],
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
                    model: None,
                    token_usage: None,
                    activities: vec![],
                    attempts: vec![],
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

    #[derive(Default)]
    struct CountingBackend {
        inner: MockBackend,
        calls: std::sync::Mutex<Vec<String>>,
    }

    impl CountingBackend {
        fn record(&self, path: &str, action: &BackendAction) {
            self.calls
                .lock()
                .unwrap()
                .push(format!("{path}:{action:?}"));
        }
    }

    #[async_trait]
    impl BackendAdapter for CountingBackend {
        async fn next_card(&self, req: BackendRequest) -> Result<BackendResponse> {
            self.record("plain", &req.action);
            self.inner.next_card(req).await
        }

        async fn next_card_with_progress(
            &self,
            req: BackendRequest,
            _progress: Option<ProgressReporter>,
        ) -> Result<BackendResponse> {
            self.record("progress", &req.action);
            self.inner.next_card(req).await
        }

        fn capabilities(&self) -> BackendInfo {
            self.inner.capabilities()
        }
    }

    #[tokio::test]
    async fn prefetched_fix_is_consumed_without_a_second_backend_call() {
        let backend = Arc::new(CountingBackend::default());
        let mut engine = Engine::new(backend.clone());
        engine.set_prefetch_mode(PrefetchMode::Fix);

        let start = engine.start(params()).await.unwrap();
        assert!(matches!(start.card, Card::Hypothesis(_)));

        let result = engine.action(&start.session_id, Action::Fix).await.unwrap();
        assert!(matches!(result.card, Card::Patch(_)));

        let calls = backend.calls.lock().unwrap().clone();
        assert_eq!(calls.len(), 2, "unexpected backend calls: {calls:?}");
        assert!(calls[0].starts_with("progress:Start"));
        assert!(calls[1].starts_with("plain:User(Fix)"));
    }

    #[tokio::test]
    async fn stale_prefetch_is_discarded_after_context_change() {
        let backend = Arc::new(CountingBackend::default());
        let mut engine = Engine::new(backend.clone());
        engine.set_prefetch_mode(PrefetchMode::Fix);

        let start = engine.start(params()).await.unwrap();
        let context = ContextBundle {
            cwd: PathBuf::from("/tmp/project"),
            file: PathBuf::from("src/work.ts"),
            cursor: Cursor { line: 1, column: 1 },
            selection: None,
            buffer_text: "const edited = true".into(),
            buffer_start_line: 1,
            diagnostics: vec![],
            hints: vec![],
            artifacts: vec![],
            report: None,
        };
        engine.update_context(&start.session_id, context).unwrap();

        let result = engine.action(&start.session_id, Action::Fix).await.unwrap();
        let Card::Patch(card) = result.card else {
            panic!("expected patch card");
        };

        // The mock builds the diff from the buffer's first line, so a patch
        // produced from the fresh request must reference the edited buffer.
        assert!(
            card.patches[0].diff.contains("const edited = true"),
            "patch was built from stale context: {}",
            card.patches[0].diff
        );
        let calls = backend.calls.lock().unwrap().clone();
        assert!(
            calls
                .iter()
                .any(|call| call.starts_with("progress:User(Fix)")),
            "real fix call missing: {calls:?}"
        );
    }

    #[tokio::test]
    async fn fingerprint_ignores_optimizer_telemetry() {
        let backend = Arc::new(CountingBackend::default());
        let mut engine = Engine::new(backend);
        engine.set_prefetch_mode(PrefetchMode::Fix);
        let start = engine.start(params()).await.unwrap();
        let session = engine.get(&start.session_id).unwrap();

        let request = engine.request(
            session,
            BackendAction::User(Action::Fix),
            session.context.clone(),
            &NextState::Patch,
        );
        let mut noisy = request.clone();
        noisy.context.report = Some(pair_protocol::ContextReport {
            enabled: true,
            cache_hits: 42,
            ..Default::default()
        });

        assert_eq!(request_fingerprint(&request), request_fingerprint(&noisy));
    }

    #[derive(Default)]
    struct ExpectationRecorder {
        inner: MockBackend,
        requests: std::sync::Mutex<Vec<(Option<CardKind>, String)>>,
    }

    #[async_trait]
    impl BackendAdapter for ExpectationRecorder {
        async fn next_card(&self, req: BackendRequest) -> Result<BackendResponse> {
            self.requests
                .lock()
                .unwrap()
                .push((req.card_contract.expected_kind, req.session.prompt.clone()));
            self.inner.next_card(req).await
        }

        fn capabilities(&self) -> BackendInfo {
            self.inner.capabilities()
        }
    }

    #[tokio::test]
    async fn plain_auto_prompt_expects_any_kind() {
        let backend = Arc::new(ExpectationRecorder::default());
        let mut engine = Engine::new(backend.clone());

        let mut auto = params();
        auto.mode = Mode::Auto;
        engine.start(auto).await.unwrap();

        let requests = backend.requests.lock().unwrap().clone();
        assert_eq!(requests[0].0, None);
        assert_eq!(requests[0].1, "payload is empty");
    }

    #[tokio::test]
    async fn kind_prefix_forces_the_expected_card() {
        let backend = Arc::new(ExpectationRecorder::default());
        let mut engine = Engine::new(backend.clone());
        let mut start_params = params();
        start_params.prompt = "/patch guard the payload".into();

        engine.start(start_params).await.unwrap();

        let requests = backend.requests.lock().unwrap().clone();
        assert_eq!(requests[0].0, Some(CardKind::Patch));
        assert_eq!(requests[0].1, "guard the payload");
    }

    #[derive(Default)]
    struct NavigatingBackend {
        calls: std::sync::Mutex<Vec<String>>,
    }

    #[async_trait]
    impl BackendAdapter for NavigatingBackend {
        async fn next_card(&self, req: BackendRequest) -> Result<BackendResponse> {
            self.calls.lock().unwrap().push(format!("{:?}", req.action));

            let card = match req.action {
                BackendAction::Start => Card::Hypothesis(HypothesisCard {
                    id: "c_1".into(),
                    title: "Wrong buffer suspected".into(),
                    claim: "The sizing lives in the component file.".into(),
                    evidence: None,
                    next_move: None,
                    actions: vec![Action::Fix, Action::Stop],
                }),
                BackendAction::User(Action::Fix) => {
                    Card::OpenLocation(pair_protocol::OpenLocationCard {
                        id: "c_nav".into(),
                        reason: "The change belongs in the component file.".into(),
                        location: pair_protocol::Location {
                            file: "src/component.ts".into(),
                            line: 3,
                            column: 1,
                        },
                    })
                }
                BackendAction::LocationGranted => {
                    let old_line = req.context.buffer_text.lines().next().unwrap_or_default();
                    Card::Patch(PatchCard {
                        id: "c_patch".into(),
                        title: "Adjust icon size".into(),
                        explanation: "Sets the icon size on the component.".into(),
                        warnings: vec![],
                        goal_complete: false,
                        patches: vec![FilePatch {
                            id: "p_1".into(),
                            file: req.context.file.clone(),
                            diff: format!("@@ -1,1 +1,1 @@\n-{old_line}\n+const size = 16\n"),
                            explanation: "Shrinks the icon.".into(),
                        }],
                        actions: vec![
                            Action::Apply,
                            Action::Retry,
                            Action::EditPrompt,
                            Action::Stop,
                        ],
                    })
                }
                other => panic!("unexpected action {other:?}"),
            };

            Ok(BackendResponse {
                card,
                raw_output: None,
                metadata: BackendMetadata {
                    backend: "navigating".into(),
                    model: None,
                    token_usage: Some(pair_protocol::TokenUsage::estimated(10, 5)),
                    activities: vec![],
                    attempts: vec![],
                },
            })
        }

        fn capabilities(&self) -> BackendInfo {
            BackendInfo {
                name: "navigating".into(),
                streaming: false,
                patches: true,
                reasoning: false,
                can_read_project: false,
                can_use_tools: false,
            }
        }
    }

    fn granted_context() -> ContextBundle {
        ContextBundle {
            cwd: PathBuf::from("/tmp/project"),
            file: PathBuf::from("src/component.ts"),
            cursor: Cursor { line: 3, column: 1 },
            selection: None,
            buffer_text: "const size = 24".into(),
            buffer_start_line: 1,
            diagnostics: vec![],
            hints: vec![],
            artifacts: vec![],
            report: None,
        }
    }

    #[tokio::test]
    async fn open_location_grant_continues_the_same_turn() {
        let backend = Arc::new(NavigatingBackend::default());
        let granted: Arc<std::sync::Mutex<Vec<String>>> = Arc::default();
        let granted_log = granted.clone();
        let mut engine = Engine::new(backend.clone());
        engine.set_location_granter(Arc::new(move |request, _session| {
            granted_log
                .lock()
                .unwrap()
                .push(request.location.file.display().to_string());
            Box::pin(async move { Some(granted_context()) })
        }));

        let start = engine.start(params()).await.unwrap();
        let result = engine.action(&start.session_id, Action::Fix).await.unwrap();

        let Card::Patch(card) = result.card else {
            panic!("expected patch card, got {:?}", result.card);
        };
        assert_eq!(card.patches[0].file, PathBuf::from("src/component.ts"));
        assert!(card.patches[0].diff.contains("const size = 24"));
        assert_eq!(granted.lock().unwrap().as_slice(), ["src/component.ts"]);
        assert_eq!(
            engine.get(&start.session_id).unwrap().context.file,
            PathBuf::from("src/component.ts")
        );

        let calls = backend.calls.lock().unwrap().clone();
        assert!(calls.iter().any(|call| call.contains("LocationGranted")));
    }

    #[tokio::test]
    async fn declined_open_location_surfaces_a_deny_card() {
        let backend = Arc::new(NavigatingBackend::default());
        let mut engine = Engine::new(backend);
        engine.set_location_granter(Arc::new(|_, _| Box::pin(async { None })));

        let start = engine.start(params()).await.unwrap();
        let result = engine.action(&start.session_id, Action::Fix).await.unwrap();

        let Card::Deny(card) = result.card else {
            panic!("expected deny card, got {:?}", result.card);
        };
        assert_eq!(
            card.location.as_ref().map(|l| l.file.clone()),
            Some(PathBuf::from("src/component.ts"))
        );
    }

    #[tokio::test]
    async fn open_location_without_granter_becomes_a_deny_card() {
        let backend = Arc::new(NavigatingBackend::default());
        let mut engine = Engine::new(backend);

        let start = engine.start(params()).await.unwrap();
        let result = engine.action(&start.session_id, Action::Fix).await.unwrap();

        assert!(matches!(result.card, Card::Deny(_)));
    }
}
