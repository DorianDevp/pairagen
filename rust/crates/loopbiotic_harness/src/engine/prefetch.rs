//! Speculative work while the user is still reading the current card: the
//! Fix prefetch of the likely next card and the goal-continuation prefetch of
//! the next sliced goal turn. Both are session-keyed background turns whose
//! usage folds into the session totals even when the speculation is wasted.

use anyhow::Result;
use loopbiotic_backends::{BackendAction, BackendRequest, BackendResponse};
use loopbiotic_protocol::Action;

use crate::session::Session;
use crate::state::{NextState, SessionState};

use super::Engine;

/// Speculative prefetch of the likely next card. `Fix` requests the patch
/// card in the background while the user is still reading a discovery card,
/// so pressing Fix returns (near-)instantly on a hit.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PrefetchMode {
    Off,
    Fix,
}

pub(super) struct Prefetch {
    action: Action,
    fingerprint: u64,
    handle: tokio::task::JoinHandle<Result<BackendResponse>>,
}

/// A speculative goal-continuation turn: while the user reviews the hunks of
/// one goal slice, the next slice is already being generated on the same
/// backend session. Unlike the Fix prefetch it is not fingerprint-matched —
/// accepting hunks necessarily changes the context, and the consumed response
/// still passes every validation gate against the live buffers.
pub(super) struct Continuation {
    handle: tokio::task::JoinHandle<Result<BackendResponse>>,
}

#[cfg(test)]
impl Continuation {
    pub(super) fn is_finished(&self) -> bool {
        self.handle.is_finished()
    }
}

impl Engine {
    /// Requests the likely next card in the background while the user reads
    /// the one just shown. Only Fix is predicted: it is the most common and
    /// slowest follow-up, and on backends with a separate patch process a
    /// misprediction never blocks the user's real next request.
    pub(super) async fn schedule_prefetch(&mut self, session_id: &str) {
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
    pub(super) async fn take_prefetch(
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

    /// Speculatively requests the next goal slice in the background while the
    /// user still reviews the current slice's hunks. Only fires when the
    /// pending slice's plan says more slices follow; consumed by
    /// `take_goal_continuation` once the review queue drains.
    pub(super) async fn schedule_goal_continuation(&mut self, session_id: &str) {
        self.reap_cancelled_continuations().await;

        let Some(session) = self.sessions.get(session_id) else {
            return;
        };
        if !session.continuous_goal
            || !session.goal_slice_continues
            || session.state != SessionState::PatchShown
        {
            return;
        }
        if self.continuations.contains_key(session_id) {
            // The running speculation already targets the next slice of this
            // chain; queueing another would only pile up turns.
            return;
        }

        // The same request the engine would send once the last queued hunk is
        // accepted, built from the pre-accept snapshot: sliced backends keep
        // per-session threads, so the continuation lands on the same
        // conversation the slice came from.
        let request = self.request(
            session,
            BackendAction::User(Action::Next),
            session.context.clone(),
            &NextState::GoalLoop,
        );
        let backend = self.backend.clone();
        let handle = tokio::spawn(async move { backend.next_card(request).await });

        self.continuations
            .insert(session_id.to_string(), Continuation { handle });
    }

    /// Consumes the speculated next slice when the last queued hunk was
    /// accepted. A finished speculation returns instantly; an in-flight one is
    /// awaited — the only wait, bounded by one slice's generation. Failures
    /// fall back to the real request path.
    pub(super) async fn take_goal_continuation(
        &mut self,
        session_id: &str,
    ) -> Option<BackendResponse> {
        let entry = self.continuations.remove(session_id)?;

        match entry.handle.await {
            Ok(Ok(response)) => Some(response),
            _ => None,
        }
    }

    /// Aborts the speculation after a reject/retry/reply invalidated the slice
    /// it continued from. A finished speculation folds its usage into the
    /// session totals right away; an unfinished one keeps running detached and
    /// is folded once it completes — wasted turns stay visible either way,
    /// the same policy as the Fix prefetch.
    pub(super) async fn cancel_goal_continuation(&mut self, session: &mut Session) {
        let Some(entry) = self.continuations.remove(&session.id) else {
            return;
        };

        if entry.handle.is_finished() {
            if let Ok(Ok(response)) = entry.handle.await {
                fold_usage(session, &response.metadata.token_usage);
            }
            return;
        }

        self.cancelled_continuations
            .push((session.id.clone(), entry.handle));
    }

    /// Folds cancelled speculations that have since finished into their
    /// sessions' token totals; still-running ones stay queued for a later
    /// sweep.
    async fn reap_cancelled_continuations(&mut self) {
        let pending = std::mem::take(&mut self.cancelled_continuations);

        for (session_id, handle) in pending {
            if !handle.is_finished() {
                self.cancelled_continuations.push((session_id, handle));
                continue;
            }
            if let Ok(Ok(response)) = handle.await
                && let Some(session) = self.sessions.get_mut(&session_id)
            {
                fold_usage(session, &response.metadata.token_usage);
            }
        }
    }
}

fn fold_usage(session: &mut Session, usage: &Option<loopbiotic_protocol::TokenUsage>) {
    if let Some(usage) = usage {
        session.token_usage.add(usage);
    }
}

pub(super) fn request_fingerprint(request: &BackendRequest) -> u64 {
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
