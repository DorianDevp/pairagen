//! Engine-level tests that drive full turns across the turn/prefetch/goal/
//! validate submodules, plus the shared fixtures they use.
//!
//! All async tests use the multi-thread runtime flavor because
//! `Engine::optimize_context` calls `tokio::task::block_in_place`, which
//! panics on the current-thread runtime `#[tokio::test]` defaults to.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use async_trait::async_trait;
use loopbiotic_backends::{
    BackendAction, BackendAdapter, BackendMetadata, BackendRequest, BackendResponse, MockBackend,
};
use loopbiotic_protocol::{
    BackendInfo, Cursor, FilePatch, FindingCard, HypothesisCard, Mode, PatchCard,
};

use super::prefetch::request_fingerprint;
use super::*;

pub(super) fn params() -> StartSessionParams {
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

#[tokio::test(flavor = "multi_thread")]
async fn starts_with_hypothesis() {
    let backend = Arc::new(MockBackend);
    let mut engine = Engine::new(backend);

    let result = engine.start(params()).await.unwrap();

    assert!(matches!(result.card, Card::Hypothesis(_)));
}

#[tokio::test(flavor = "multi_thread")]
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

#[tokio::test(flavor = "multi_thread")]
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
    assert_eq!(second.goal.status, loopbiotic_protocol::GoalStatus::Active);
    assert_eq!(
        engine
            .get(&second.session_id)
            .unwrap()
            .completed_steps
            .len(),
        1
    );

    let Card::Patch(second_patch) = &second.card else {
        panic!("expected second review hunk");
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
    assert_eq!(
        complete.goal.status,
        loopbiotic_protocol::GoalStatus::Complete
    );
    assert_eq!(complete.goal.completed_steps.len(), 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn continuous_goal_reviews_a_multi_file_batch_without_more_model_turns() {
    let backend = Arc::new(MultiFileGoalBackend::default());
    let reads = Arc::new(AtomicUsize::new(0));
    let mut engine = Engine::new(backend.clone());
    let observed_reads = reads.clone();
    engine.set_source_context_provider(Arc::new(move |file, _session_id| {
        let observed_reads = observed_reads.clone();
        Box::pin(async move {
            observed_reads.fetch_add(1, Ordering::SeqCst);
            let text = if file == std::path::Path::new("src/work.ts") {
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
    assert!(
        engine.continuations.is_empty(),
        "a legacy full batch must not trigger slice speculation"
    );

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
    assert_eq!(
        complete.goal.status,
        loopbiotic_protocol::GoalStatus::Complete
    );
    assert_eq!(complete.turn_token_usage.total_tokens, 0);
    assert_eq!(backend.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn continuous_goal_reject_stops_without_another_model_turn() {
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

    let rejected = engine.apply_result(result).await.unwrap();

    let Card::Error(card) = &rejected.card else {
        panic!("expected a local rejection card, got {:?}", rejected.card);
    };
    assert_eq!(card.title, "Draft rejected");
    assert!(card.actions.contains(&Action::Retry));
    assert_eq!(rejected.turn_token_usage, TokenUsage::default());
    assert!(rejected.goal.completed_steps.is_empty());
    assert_eq!(
        rejected.goal.status,
        loopbiotic_protocol::GoalStatus::Active
    );
    assert_eq!(
        engine.get(&rejected.session_id).unwrap().state,
        SessionState::GoalLoopFailed
    );
}

#[tokio::test(flavor = "multi_thread")]
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

#[tokio::test(flavor = "multi_thread")]
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

#[tokio::test(flavor = "multi_thread")]
async fn rejected_apply_waits_for_explicit_retry() {
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

    let rejected = engine.apply_result(result).await.unwrap();

    let Card::Error(card) = &rejected.card else {
        panic!("expected a local rejection card, got {:?}", rejected.card);
    };
    assert!(card.message.contains("patch context is ambiguous"));
    assert_eq!(rejected.turn_token_usage, TokenUsage::default());
    assert_eq!(
        engine.get(&start.session_id).unwrap().state,
        SessionState::PatchFailed
    );

    let retried = engine
        .action(&start.session_id, Action::Retry)
        .await
        .unwrap();
    assert!(matches!(retried.card, Card::Patch(_)));
    assert!(retried.turn_token_usage.total_tokens > 0);
}

#[tokio::test(flavor = "multi_thread")]
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

#[tokio::test(flavor = "multi_thread")]
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

#[tokio::test(flavor = "multi_thread")]
async fn converts_bad_patch_to_error_card() {
    let backend = Arc::new(BadPatchBackend);
    let mut engine = Engine::new(backend);
    let start = engine.start(params()).await.unwrap();
    let result = engine.action(&start.session_id, Action::Fix).await.unwrap();

    let Card::Error(card) = result.card else {
        panic!("expected error card");
    };

    assert!(
        card.message
            .contains("unexpected content before first diff hunk")
    );
    assert!(result.attempts.iter().all(|attempt| attempt.violation_class
        == Some(loopbiotic_protocol::ViolationClass::MalformedDiff)));

    let retry = engine
        .action(&start.session_id, Action::Retry)
        .await
        .unwrap();

    assert!(matches!(retry.card, Card::Error(_)));
}

#[tokio::test(flavor = "multi_thread")]
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
    assert_eq!(
        result.attempts[0].violation_class,
        Some(loopbiotic_protocol::ViolationClass::ContextMismatch)
    );
    assert_eq!(result.attempts[1].outcome, "accepted");
    assert_eq!(result.attempts[1].violation_class, None);
    assert_eq!(result.turn_token_usage.total_tokens, 30);
}

#[tokio::test(flavor = "multi_thread")]
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
    assert!(result.attempts.iter().all(|attempt| attempt.violation_class
        == Some(loopbiotic_protocol::ViolationClass::KindMismatch)));

    let retry = engine
        .action(&start.session_id, Action::Retry)
        .await
        .unwrap();
    assert!(matches!(retry.card, Card::Error(_)));
}

#[tokio::test(flavor = "multi_thread")]
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

#[tokio::test(flavor = "multi_thread")]
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
    assert_eq!(result.attempts[0].violation_class, None);
    assert!(
        result.attempts[0]
            .detail
            .as_deref()
            .unwrap()
            .contains("token limit reached")
    );
    assert!(engine.get(&result.session_id).is_some());
}

#[tokio::test(flavor = "multi_thread")]
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
    assert_eq!(
        result.attempts[0].violation_class,
        Some(loopbiotic_protocol::ViolationClass::DuplicateStep)
    );
    assert!(result.attempts[0].candidate_card.is_some());
    assert_eq!(result.attempts[1].outcome, "accepted");
    assert_eq!(result.attempts[1].violation_class, None);

    let observations = &engine.get(&start.session_id).unwrap().known_observations;
    assert_eq!(observations.len(), 3);
    assert_eq!(observations[0].occurrences, 2);
    assert_eq!(observations[1].occurrences, 2);
    assert!(!observations[0].active);
    assert!(observations[1].active);
    assert!(observations[2].active);
}

#[tokio::test(flavor = "multi_thread")]
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

#[tokio::test(flavor = "multi_thread")]
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

fn accept(session_id: &str, card: &Card, buffer: &str) -> PatchApplyResult {
    let Card::Patch(card) = card else {
        panic!("expected a pending patch card, got {card:?}");
    };
    let mut context = editor_context(buffer);
    context.file = card.patches[0].file.clone();

    PatchApplyResult {
        session_id: session_id.into(),
        card_id: card.id.clone(),
        accepted: true,
        patch_ids: vec![card.patches[0].id.clone()],
        changed_files: vec![card.patches[0].file.clone()],
        error: None,
        context,
    }
}

fn sliced_goal_provider() -> SourceContextProvider {
    Arc::new(|file, _session_id| {
        Box::pin(async move {
            let text = match file.to_str() {
                Some("src/work.ts") => "placeholder",
                Some("src/caller.ts") => "caller",
                Some("src/shape.ts") => "shape",
                _ => return None,
            };
            let mut context = editor_context(text);
            context.file = file;
            Some(context)
        })
    })
}

#[tokio::test(flavor = "multi_thread")]
async fn sliced_goal_speculates_and_consumes_each_next_slice_on_accept() {
    let backend = Arc::new(CountingBackend::default());
    let mut engine = Engine::new(backend.clone());
    engine.set_source_context_provider(sliced_goal_provider());
    let mut goal = params();
    goal.mode = Mode::Auto;

    let first = engine.start(goal).await.unwrap();
    let Card::Patch(first_patch) = &first.card else {
        panic!(
            "expected the first slice, got {:?}; attempts {:?}",
            first.card, first.attempts
        );
    };
    assert_eq!(first_patch.patches[0].file, PathBuf::from("src/work.ts"));
    assert!(!first_patch.goal_complete);
    let plan = first_patch
        .plan
        .as_ref()
        .expect("first slice carries a plan");
    assert!(!plan.complete);
    assert_eq!(plan.remaining.len(), 2);
    assert!(
        engine.continuations.contains_key(&first.session_id),
        "the next slice must be speculated while the first is reviewed"
    );

    let second = engine
        .apply_result(accept(
            &first.session_id,
            &first.card,
            "payload = payload or {}",
        ))
        .await
        .unwrap();
    let Card::Patch(second_patch) = &second.card else {
        panic!("expected the second slice, got {:?}", second.card);
    };
    assert_eq!(second_patch.patches[0].file, PathBuf::from("src/caller.ts"));
    assert!(!second_patch.goal_complete);
    assert!(
        second.turn_token_usage.total_tokens > 0,
        "the consumed speculation's usage must stay visible"
    );
    assert_eq!(second.goal.status, loopbiotic_protocol::GoalStatus::Active);
    assert!(
        engine.continuations.contains_key(&second.session_id),
        "the final slice must be speculated while the second is reviewed"
    );

    let third = engine
        .apply_result(accept(&second.session_id, &second.card, "CALLER"))
        .await
        .unwrap();
    let Card::Patch(third_patch) = &third.card else {
        panic!("expected the final slice, got {:?}", third.card);
    };
    assert_eq!(third_patch.patches[0].file, PathBuf::from("src/shape.ts"));
    assert!(third_patch.goal_complete, "final slice completes the goal");
    assert!(
        !engine.continuations.contains_key(&third.session_id),
        "a complete plan leaves nothing to speculate"
    );

    let complete = engine
        .apply_result(accept(&third.session_id, &third.card, "SHAPE"))
        .await
        .unwrap();
    assert!(matches!(complete.card, Card::Summary(_)));
    assert_eq!(
        complete.goal.status,
        loopbiotic_protocol::GoalStatus::Complete
    );
    assert_eq!(complete.goal.completed_steps.len(), 3);
    assert_eq!(complete.turn_token_usage.total_tokens, 0);

    // One real turn plus two consumed speculations; no user action waited for
    // a fresh model turn after the first card.
    let calls = backend.calls.lock().unwrap().clone();
    assert_eq!(calls.len(), 3, "unexpected backend calls: {calls:?}");
    assert!(calls[0].starts_with("progress:Start"));
    assert!(calls[1].starts_with("plain:User(Next)"));
    assert!(calls[2].starts_with("plain:User(Next)"));
}

/// Waits until the session's scheduled continuation turn has finished on the
/// backend. `schedule_goal_continuation` inserts the map entry synchronously,
/// so a pending speculation can never be missed here; only its background
/// completion is awaited.
async fn settle_continuation(engine: &Engine, session_id: &str) {
    for _ in 0..2_000 {
        if engine.continuations[session_id].is_finished() {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    panic!("speculated continuation for {session_id} never finished");
}

#[tokio::test(flavor = "multi_thread")]
async fn rejected_slice_cancels_speculation_and_waits_for_explicit_retry() {
    let backend = Arc::new(CountingBackend::default());
    let mut engine = Engine::new(backend.clone());
    engine.set_source_context_provider(sliced_goal_provider());
    let mut goal = params();
    goal.mode = Mode::Auto;
    let start = engine.start(goal).await.unwrap();
    let Card::Patch(first_patch) = &start.card else {
        panic!("expected the first slice, got {:?}", start.card);
    };
    let usage_after_start = start.token_usage.total_tokens;

    // Let the speculation finish so cancelling it folds usage immediately.
    settle_continuation(&engine, &start.session_id).await;

    let rejected = engine
        .apply_result(PatchApplyResult {
            session_id: start.session_id.clone(),
            card_id: start.card.id().into(),
            accepted: false,
            patch_ids: vec![first_patch.patches[0].id.clone()],
            changed_files: vec![],
            error: Some("wrong shape".into()),
            context: editor_context("placeholder"),
        })
        .await
        .unwrap();

    let Card::Error(card) = &rejected.card else {
        panic!("expected a local rejection card, got {:?}", rejected.card);
    };
    assert_eq!(card.title, "Draft rejected");
    assert_eq!(rejected.turn_token_usage, TokenUsage::default());
    assert!(
        rejected.token_usage.total_tokens > usage_after_start,
        "the cancelled speculation's usage must fold into the session totals"
    );
    assert!(
        !engine.continuations.contains_key(&start.session_id),
        "rejecting must leave no continuation in flight"
    );

    let calls = backend.calls.lock().unwrap().clone();
    assert_eq!(calls.len(), 2, "reject triggered a backend turn: {calls:?}");
    assert!(calls[0].starts_with("progress:Start"));
    assert!(calls[1].starts_with("plain:User(Next)"));

    let redrafted = engine
        .action(&start.session_id, Action::Retry)
        .await
        .unwrap();
    let Card::Patch(redraft) = &redrafted.card else {
        panic!(
            "expected an explicitly requested redraft, got {:?}",
            redrafted.card
        );
    };
    assert_eq!(redraft.patches[0].file, PathBuf::from("src/work.ts"));
    let calls = backend.calls.lock().unwrap().clone();
    assert!(
        calls
            .iter()
            .any(|call| call.starts_with("progress:User(Retry)")),
        "explicit retry did not trigger a backend turn: {calls:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn user_retry_on_a_slice_cancels_speculation_and_stops_speculating() {
    let backend = Arc::new(CountingBackend::default());
    let mut engine = Engine::new(backend.clone());
    engine.set_source_context_provider(sliced_goal_provider());
    let mut goal = params();
    goal.mode = Mode::Auto;
    let first = engine.start(goal).await.unwrap();
    assert!(engine.continuations.contains_key(&first.session_id));

    let redrafted = engine
        .action(&first.session_id, Action::Retry)
        .await
        .unwrap();

    assert!(matches!(redrafted.card, Card::Patch(_)));
    assert!(
        !engine.continuations.contains_key(&first.session_id),
        "a redraft abandons the slice chain, so nothing may be speculated"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn single_file_goal_without_plan_completes_as_a_batch_of_one() {
    let backend = Arc::new(SingleFileNoPlanBackend::default());
    let mut engine = Engine::new(backend.clone());
    let mut goal = params();
    goal.mode = Mode::Auto;
    goal.buffer_text = "first".into();

    let first = engine.start(goal).await.unwrap();
    let Card::Patch(card) = &first.card else {
        panic!("expected the whole batch as one card, got {:?}", first.card);
    };
    assert!(
        card.goal_complete,
        "legacy goal_complete governs completion"
    );
    assert_eq!(card.plan, None);
    assert!(
        engine.continuations.is_empty(),
        "a planless response must not trigger slice speculation"
    );

    let complete = engine
        .apply_result(accept(&first.session_id, &first.card, "FIRST"))
        .await
        .unwrap();

    assert!(matches!(complete.card, Card::Summary(_)));
    assert_eq!(
        complete.goal.status,
        loopbiotic_protocol::GoalStatus::Complete
    );
    assert_eq!(backend.calls.load(Ordering::SeqCst), 1);
}

#[derive(Default)]
struct SingleFileNoPlanBackend {
    calls: AtomicUsize,
}

#[async_trait]
impl BackendAdapter for SingleFileNoPlanBackend {
    async fn next_card(&self, req: BackendRequest) -> Result<BackendResponse> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        assert!(req.card_contract.allow_goal_completion);

        Ok(BackendResponse {
            card: Card::Patch(PatchCard {
                id: "c_single".into(),
                title: "Complete local change".into(),
                explanation: "One edit finishes the goal.".into(),
                warnings: vec![],
                goal_complete: true,
                plan: None,
                patches: vec![FilePatch {
                    id: "p_single".into(),
                    file: "src/work.ts".into(),
                    diff: "@@ -1,1 +1,1 @@\n-first\n+FIRST\n".into(),
                    explanation: "Updates the only required location.".into(),
                }],
                actions: vec![Action::Apply, Action::Why, Action::Retry, Action::Stop],
            }),
            raw_output: None,
            metadata: BackendMetadata {
                backend: "single_file_no_plan".into(),
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
                plan: None,
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
                plan: None,
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
                    evidence: Some(loopbiotic_protocol::LocationEvidence {
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
                location: Some(loopbiotic_protocol::Location {
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
                token_usage: Some(loopbiotic_protocol::TokenUsage::estimated(10, 5)),
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
                plan: None,
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
            BackendAction::User(Action::Fix) if !self.failed_once.swap(true, Ordering::SeqCst) => {
                Card::Patch(PatchCard {
                    id: "c_invalid".into(),
                    title: "Invalid first attempt".into(),
                    explanation: "This attempt has stale context.".into(),
                    warnings: vec![],
                    goal_complete: false,
                    plan: None,
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
                    plan: None,
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
                token_usage: Some(loopbiotic_protocol::TokenUsage::estimated(10, 5)),
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

#[tokio::test(flavor = "multi_thread")]
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

#[tokio::test(flavor = "multi_thread")]
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

#[tokio::test(flavor = "multi_thread")]
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
    noisy.context.report = Some(loopbiotic_protocol::ContextReport {
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

#[tokio::test(flavor = "multi_thread")]
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

#[tokio::test(flavor = "multi_thread")]
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
                Card::OpenLocation(loopbiotic_protocol::OpenLocationCard {
                    id: "c_nav".into(),
                    reason: "The change belongs in the component file.".into(),
                    location: loopbiotic_protocol::Location {
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
                    plan: None,
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
                token_usage: Some(loopbiotic_protocol::TokenUsage::estimated(10, 5)),
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

#[tokio::test(flavor = "multi_thread")]
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

#[tokio::test(flavor = "multi_thread")]
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

#[tokio::test(flavor = "multi_thread")]
async fn open_location_without_granter_becomes_a_deny_card() {
    let backend = Arc::new(NavigatingBackend::default());
    let mut engine = Engine::new(backend);

    let start = engine.start(params()).await.unwrap();
    let result = engine.action(&start.session_id, Action::Fix).await.unwrap();

    assert!(matches!(result.card, Card::Deny(_)));
}
