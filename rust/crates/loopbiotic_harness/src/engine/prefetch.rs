//! Speculative work while the user is still reading the current card:
//! read-only post-accept conversation for ordinary drafts, and the next small
//! patch only inside an explicitly authorized goal.

use anyhow::Result;
use loopbiotic_backends::{BackendAction, BackendResponse};
use loopbiotic_protocol::Action;

use crate::session::Session;
use crate::state::{NextState, SessionState};

use super::Engine;

/// Ordinary speculation is read-only. While a non-goal patch is reviewed,
/// prepare the conversational follow-up that should appear if it is accepted.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PrefetchMode {
    Off,
    ReadOnly,
}

pub(super) struct Prefetch {
    handle: tokio::task::JoinHandle<Result<BackendResponse>>,
}

/// A speculative goal-continuation turn: while the user reviews one explicit
/// goal hunk, the next small hunk is generated on the same backend session.
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
    /// Prepares the read-only card that follows acceptance of an ordinary
    /// draft. Patch speculation is reserved for explicit goals below.
    pub(super) async fn schedule_prefetch(&mut self, session_id: &str) {
        if self.prefetch_mode != PrefetchMode::ReadOnly {
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
        if session.state != SessionState::PatchShown || session.goal_active {
            return;
        }
        if !matches!(
            session.cards.last(),
            Some(loopbiotic_protocol::Card::Patch(_))
        ) {
            return;
        }

        let request = self.request(
            session,
            BackendAction::PostAccept,
            session.context.clone(),
            &NextState::Conversation,
        );
        let backend = self.backend.clone();
        let handle = tokio::spawn(async move { backend.next_card(request).await });

        self.prefetches
            .insert(session_id.to_string(), Prefetch { handle });
    }

    pub(super) async fn take_post_accept_prefetch(
        &mut self,
        session: &mut Session,
    ) -> Option<BackendResponse> {
        let entry = self.prefetches.remove(&session.id)?;

        match entry.handle.await {
            Ok(Ok(response)) => Some(response),
            _ => None,
        }
    }

    pub(super) async fn cancel_post_accept_prefetch(&mut self, session: &mut Session) {
        let Some(entry) = self.prefetches.remove(&session.id) else {
            return;
        };
        if entry.handle.is_finished() {
            if let Ok(Ok(response)) = entry.handle.await {
                fold_usage(session, &response.metadata.token_usage);
            }
            return;
        }

        entry.handle.abort();
        let _ = self.backend.cancel_turn(&session.id).await;
    }

    /// Speculatively requests the next goal slice in the background while the
    /// user still reviews the current slice's hunks. Only fires when the
    /// pending slice's plan says more slices follow; consumed by
    /// `take_goal_continuation` once the review queue drains.
    pub(super) async fn schedule_goal_continuation(&mut self, session_id: &str) {
        let Some(session) = self.sessions.get(session_id) else {
            return;
        };
        if !session.goal_active
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
            BackendAction::User(Action::Goal),
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

    /// Aborts speculation after reject/retry/reply invalidated the hunk it
    /// continued from. Finished work stays accounted; in-flight work is
    /// interrupted at the real backend and cannot surface a replacement.
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

        entry.handle.abort();
        let _ = self.backend.cancel_turn(&session.id).await;
    }
}

fn fold_usage(session: &mut Session, usage: &Option<loopbiotic_protocol::TokenUsage>) {
    if let Some(usage) = usage {
        session.token_usage.add(usage);
    }
}
