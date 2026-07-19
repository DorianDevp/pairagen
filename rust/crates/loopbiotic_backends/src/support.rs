//! Helpers shared by the backend adapters: turn deadlines, env parsing, and
//! the request/progress plumbing that every adapter repeats.

use std::future::Future;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::time::Duration;

use anyhow::Result;
use loopbiotic_protocol::{Action, Card, ErrorCard};
use serde_json::{Value, json};

use crate::{BackendAction, BackendPreview, BackendProgress, BackendRequest, ProgressReporter};

pub(crate) const TURN_TIMEOUT_ENV: &str = "LOOPBIOTIC_TURN_TIMEOUT_SECS";
const DEFAULT_TURN_TIMEOUT: Duration = Duration::from_secs(600);

/// Reads the per-turn deadline once at backend construction. `None` means the
/// deadline is disabled (`LOOPBIOTIC_TURN_TIMEOUT_SECS=0`).
pub(crate) fn turn_timeout_from_env() -> Option<Duration> {
    parse_turn_timeout(std::env::var(TURN_TIMEOUT_ENV).ok().as_deref())
}

/// LLM turns are long; the deadline is a wedge-breaker, not latency control,
/// so the default is generous and unparseable values fall back to it rather
/// than silently removing the protection.
pub(crate) fn parse_turn_timeout(raw: Option<&str>) -> Option<Duration> {
    match raw.map(str::trim).map(str::parse::<u64>) {
        Some(Ok(0)) => None,
        Some(Ok(secs)) => Some(Duration::from_secs(secs)),
        Some(Err(_)) | None => Some(DEFAULT_TURN_TIMEOUT),
    }
}

/// Marker error for an expired turn deadline. Callers detect it with
/// `error.is::<TurnTimedOut>()` to kill/invalidate their cached process and to
/// skip in-turn retries (a wedged CLI would only wedge again).
#[derive(Debug)]
pub(crate) struct TurnTimedOut {
    backend: &'static str,
    limit: Duration,
}

impl std::fmt::Display for TurnTimedOut {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} produced no result within {:?}; the process was killed and the next turn starts fresh. Set {TURN_TIMEOUT_ENV} to adjust the deadline (0 disables it).",
            self.backend, self.limit
        )
    }
}

impl std::error::Error for TurnTimedOut {}

/// Awaits one whole backend turn under the optional deadline. On expiry the
/// turn future is dropped and a [`TurnTimedOut`] error is returned; the caller
/// is responsible for killing its child process and invalidating any cached
/// session state so the next turn spawns fresh.
pub(crate) async fn await_turn<T>(
    backend: &'static str,
    limit: Option<Duration>,
    turn: impl Future<Output = Result<T>>,
) -> Result<T> {
    let Some(limit) = limit else {
        return turn.await;
    };

    match tokio::time::timeout(limit, turn).await {
        Ok(result) => result,
        Err(_) => Err(anyhow::Error::new(TurnTimedOut { backend, limit })),
    }
}

/// The two process lanes the app-server backends keep per session: discovery
/// turns must not block behind a running (possibly speculative) patch turn.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum Phase {
    Discovery,
    Patch,
}

pub(crate) fn turn_phase(req: &BackendRequest) -> Phase {
    if req.card_contract.expected_kind == Some(loopbiotic_protocol::CardKind::Patch)
        || req.card_contract.allow_goal_completion
    {
        Phase::Patch
    } else {
        Phase::Discovery
    }
}

/// Hash of everything the model sees about the source context; used to skip
/// re-sending an unchanged buffer to a persistent process.
pub(crate) fn context_fingerprint(req: &BackendRequest) -> u64 {
    let mut hasher = DefaultHasher::new();
    req.context.file.hash(&mut hasher);
    req.context.cursor.line.hash(&mut hasher);
    req.context.cursor.column.hash(&mut hasher);
    req.context.buffer_start_line.hash(&mut hasher);
    req.context.buffer_text.hash(&mut hasher);
    for diagnostic in &req.context.diagnostics {
        diagnostic.file.hash(&mut hasher);
        diagnostic.line.hash(&mut hasher);
        diagnostic.message.hash(&mut hasher);
    }
    for artifact in &req.context.artifacts {
        artifact.file.hash(&mut hasher);
        artifact.start_line.hash(&mut hasher);
        artifact.text.hash(&mut hasher);
    }
    if let Some(call_hierarchy) = &req.context.call_hierarchy {
        serde_json::to_string(call_hierarchy)
            .unwrap_or_default()
            .hash(&mut hasher);
    }
    hasher.finish()
}

/// Serializes `fields` as one JSON object with exactly the given key order.
/// serde_json's default map sorts keys alphabetically, so `json!` alone cannot
/// put stable keys first. Provider prompt caches key on byte-stable prefixes:
/// every byte before the first difference is cacheable, so session-stable
/// fields must serialize before every volatile byte.
pub(crate) fn ordered_json_object(fields: &[(&str, Value)]) -> String {
    let mut out = String::from("{");
    for (index, (key, value)) in fields.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        // Keys are internal identifiers; values come from serde_json itself,
        // so serialization cannot fail in practice.
        out.push_str(&serde_json::to_string(key).unwrap_or_default());
        out.push(':');
        out.push_str(&serde_json::to_string(value).unwrap_or_default());
    }
    out.push('}');

    out
}

pub(crate) fn action_value(action: &BackendAction) -> Value {
    match action {
        BackendAction::Start => json!({"kind": "start"}),
        BackendAction::User(action) => {
            // Action is a plain protocol enum whose derived Serialize emits
            // strings/objects only; it cannot fail, and sending a silent null
            // here would corrupt the turn.
            let action =
                serde_json::to_value(action).expect("protocol Action serialization is infallible");
            json!({"kind": "user", "action": action})
        }
        BackendAction::Reply(text) => json!({"kind": "reply", "text": text}),
        BackendAction::ContractRetry(reason) => {
            json!({"kind": "contract_retry", "reason": reason})
        }
        BackendAction::LocationGranted => json!({"kind": "location_granted"}),
    }
}

pub(crate) fn report_progress(
    progress: Option<&ProgressReporter>,
    session_id: &str,
    phase: &str,
    message: &str,
) {
    if let Some(progress) = progress {
        progress(BackendProgress {
            session_id: session_id.into(),
            phase: phase.into(),
            message: message.into(),
            preview: None,
        });
    }
}

pub(crate) fn report_preview(
    progress: Option<&ProgressReporter>,
    session_id: &str,
    preview: BackendPreview,
) {
    if let Some(progress) = progress {
        progress(BackendProgress {
            session_id: session_id.into(),
            phase: "drafting".into(),
            message: "Drafting a response".into(),
            preview: Some(preview),
        });
    }
}

pub(crate) fn error_card(id: &str, title: &str, message: impl Into<String>) -> Card {
    Card::Error(ErrorCard {
        id: id.into(),
        title: title.into(),
        message: message.into(),
        actions: vec![Action::Retry, Action::EditPrompt, Action::Stop],
    })
}

/// Reads backend args from `json_name` (a JSON array) or `plain_name`
/// (whitespace-separated), falling back to `default_args`.
pub(crate) fn args_from_env(
    json_name: &str,
    plain_name: &str,
    default_args: &str,
) -> Result<Vec<String>> {
    if let Ok(value) = std::env::var(json_name)
        && !value.trim().is_empty()
    {
        return Ok(serde_json::from_str(&value)?);
    }

    Ok(std::env::var(plain_name)
        .unwrap_or_else(|_| default_args.to_string())
        .split_whitespace()
        .map(str::to_string)
        .collect())
}

pub(crate) fn optional_env(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordered_json_object_preserves_field_order() {
        // serde_json's default map would sort these keys as a, b, z; caching
        // needs the caller's stable-first order to survive serialization.
        let out = ordered_json_object(&[
            ("z", json!({"k": 1})),
            ("a", json!("v")),
            ("b", json!([1, 2])),
        ]);

        assert_eq!(out, r#"{"z":{"k":1},"a":"v","b":[1,2]}"#);
        let value: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(value["z"]["k"], 1);
    }

    #[test]
    fn serializes_user_action_as_protocol_value() {
        let value = action_value(&BackendAction::User(Action::Fix));

        assert_eq!(value["action"], "fix");
    }

    #[test]
    fn routes_patch_turns_to_the_patch_phase() {
        let mut req = crate::test_request();
        assert_eq!(turn_phase(&req), Phase::Discovery);

        req.card_contract.expected_kind = Some(loopbiotic_protocol::CardKind::Patch);
        assert_eq!(turn_phase(&req), Phase::Patch);

        req.card_contract.expected_kind = None;
        req.card_contract.allow_goal_completion = true;
        assert_eq!(turn_phase(&req), Phase::Patch);
    }

    #[test]
    fn flow_graph_participates_in_context_fingerprinting() {
        let mut req = crate::test_request();
        let without = context_fingerprint(&req);
        req.context.call_hierarchy = Some(loopbiotic_protocol::CallHierarchy {
            root: None,
            nodes: vec![],
            edges: vec![],
            partial: false,
            truncated: false,
            unavailable: true,
        });

        assert_ne!(context_fingerprint(&req), without);
    }

    #[test]
    fn turn_timeout_defaults_and_supports_disabling() {
        assert_eq!(parse_turn_timeout(None), Some(DEFAULT_TURN_TIMEOUT));
        assert_eq!(parse_turn_timeout(Some("")), Some(DEFAULT_TURN_TIMEOUT));
        assert_eq!(
            parse_turn_timeout(Some("not a number")),
            Some(DEFAULT_TURN_TIMEOUT)
        );
        assert_eq!(
            parse_turn_timeout(Some(" 45 ")),
            Some(Duration::from_secs(45))
        );
        assert_eq!(parse_turn_timeout(Some("0")), None);
    }

    #[tokio::test]
    async fn await_turn_reports_a_timed_out_turn() {
        let result: Result<()> = await_turn(
            "Test backend",
            Some(Duration::from_millis(10)),
            std::future::pending(),
        )
        .await;

        let error = result.unwrap_err();
        assert!(error.is::<TurnTimedOut>());
        assert!(error.to_string().contains(TURN_TIMEOUT_ENV));
    }

    #[tokio::test]
    async fn await_turn_without_deadline_runs_to_completion() {
        let result = await_turn("Test backend", None, async { Ok(7) }).await;

        assert_eq!(result.unwrap(), 7);
    }
}
