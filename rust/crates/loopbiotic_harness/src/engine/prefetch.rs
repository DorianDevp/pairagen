//! Speculative prefetch of the likely next card while the user is still
//! reading the current one.

use anyhow::Result;
use loopbiotic_backends::{BackendAction, BackendRequest, BackendResponse};
use loopbiotic_protocol::Action;

use crate::session::Session;
use crate::state::SessionState;

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
