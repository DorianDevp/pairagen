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
        call_hierarchy: None,
        context_policy: Default::default(),
        project_signals: Default::default(),
        skills: vec![],
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
        call_hierarchy: None,
    }
}

fn discovery_response(backend: &str) -> BackendResponse {
    BackendResponse {
        card: Card::Hypothesis(HypothesisCard {
            id: "c_discovery".into(),
            title: "Ready to collaborate".into(),
            claim: "The request is understood; goal execution remains opt-in.".into(),
            evidence: None,
            next_move: None,
            flow_path: vec![],
            actions: vec![Action::Follow, Action::Fix, Action::Goal, Action::Stop],
        }),
        raw_output: None,
        metadata: BackendMetadata {
            backend: backend.into(),
            model: None,
            token_usage: Some(TokenUsage::estimated(10, 10)),
            activities: vec![],
            attempts: vec![],
        },
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
async fn explicit_goal_repairs_two_change_blocks_to_declaration_first() {
    let backend = Arc::new(BatchGoalBackend::default());
    let reads = Arc::new(AtomicUsize::new(0));
    let mut engine = Engine::new(backend.clone());
    let observed_reads = reads.clone();
    engine.set_source_context_provider(Arc::new(move |_file, _session_id| {
        let observed_reads = observed_reads.clone();
        Box::pin(async move {
            observed_reads.fetch_add(1, Ordering::SeqCst);
            None
        })
    }));
    let mut goal = params();
    goal.mode = Mode::Investigate;
    goal.buffer_text = "first\nmiddle\nlast".into();
    let start = engine.start(goal).await.unwrap();
    assert!(matches!(start.card, Card::Hypothesis(_)));

    let result = engine
        .action(&start.session_id, Action::Goal)
        .await
        .unwrap();

    let Card::Patch(card) = result.card else {
        panic!("expected a repaired declaration-first patch");
    };
    assert_eq!(
        card.patches[0].diff,
        "@@ -1,1 +1,2 @@\n+interface Work {}\n first\n"
    );
    assert_eq!(result.attempts.len(), 2);
    assert_eq!(result.attempts[0].outcome, "contract_retry");
    assert_eq!(
        result.attempts[0].violation_class,
        Some(loopbiotic_protocol::ViolationClass::MultiHunk)
    );
    assert_eq!(result.attempts[1].outcome, "accepted");
    assert_eq!(
        reads.load(Ordering::SeqCst),
        0,
        "current-file validation must reuse the fresh action context"
    );
    assert!(engine.continuations.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn explicit_goal_rejects_a_multi_file_batch() {
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
    goal.mode = Mode::Investigate;
    goal.buffer_text = "first".into();

    let start = engine.start(goal).await.unwrap();
    assert!(matches!(start.card, Card::Hypothesis(_)));

    let result = engine
        .action(&start.session_id, Action::Goal)
        .await
        .unwrap();

    assert!(matches!(result.card, Card::Error(_)));
    assert!(result.attempts.iter().all(|attempt| {
        attempt.violation_class == Some(loopbiotic_protocol::ViolationClass::MultiHunk)
    }));
    assert_eq!(reads.load(Ordering::SeqCst), 0);
    assert!(engine.continuations.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn explicit_goal_reject_stops_without_another_model_turn() {
    let backend = Arc::new(MockBackend);
    let mut engine = Engine::new(backend);
    let mut goal = params();
    goal.mode = Mode::Investigate;
    let start = engine.start(goal).await.unwrap();
    let first = engine
        .action(&start.session_id, Action::Goal)
        .await
        .unwrap();
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
    goal.mode = Mode::Investigate;
    let start = engine.start(goal).await.unwrap();
    let first = engine
        .action(&start.session_id, Action::Goal)
        .await
        .unwrap();
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
async fn accepted_non_goal_patch_continues_until_the_goal_is_resolved() {
    let backend = Arc::new(CountingBackend::default());
    let mut engine = Engine::new(backend.clone());
    let start = engine.start(params()).await.unwrap();
    let patch = engine.action(&start.session_id, Action::Fix).await.unwrap();

    let result = engine
        .apply_result(accept(
            &patch.session_id,
            &patch.card,
            "payload = payload or {}",
        ))
        .await
        .unwrap();

    assert!(matches!(result.card, Card::Summary(_)));
    assert_eq!(
        result.goal.status,
        loopbiotic_protocol::GoalStatus::Complete
    );
    assert_eq!(
        engine.get(&result.session_id).unwrap().state,
        SessionState::Summary
    );
    let calls = backend.calls.lock().unwrap().clone();
    assert!(
        calls
            .iter()
            .any(|call| call.starts_with("progress:User(Goal)")),
        "accept did not continue solving the goal: {calls:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn cancelling_accept_continuation_keeps_the_patch_accepted() {
    let backend = Arc::new(HangingAcceptContinuationBackend);
    let mut engine = Engine::new(backend);
    let start = engine.start(params()).await.unwrap();
    let patch = engine.action(&start.session_id, Action::Fix).await.unwrap();
    let session_id = patch.session_id.clone();
    let mut turn = Box::pin(engine.apply_result(accept(
        &patch.session_id,
        &patch.card,
        "payload = payload or {}",
    )));

    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(25), &mut turn)
            .await
            .is_err(),
        "accepted-patch continuation unexpectedly completed"
    );
    drop(turn);

    let committed = engine.get(&session_id).unwrap();
    assert_eq!(committed.state, SessionState::CardShown);
    assert_eq!(committed.accepted_patches, vec!["p_1"]);

    let cancelled = engine.cancel_turn(&session_id).await.unwrap();
    let Card::Finding(card) = cancelled.card else {
        panic!("accepted patch was restored as pending");
    };
    assert_eq!(card.title, "Continuation cancelled");
    assert_eq!(
        cancelled.goal.status,
        loopbiotic_protocol::GoalStatus::Paused
    );
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
        .reply(&start.session_id, "that is not it".into(), Mode::Explain)
        .await
        .unwrap();

    let Card::Finding(card) = result.card else {
        panic!("expected finding card");
    };

    assert!(card.finding.contains("that is not it"));
}

#[test]
fn selected_instruction_skills_replace_the_session_snapshot_locally() {
    let backend = Arc::new(MockBackend);
    let mut engine = Engine::new(backend);
    let (session_id, _) = engine.reserve_start(params());
    let skill = loopbiotic_protocol::InstructionSkill {
        name: "ANGULAR.md".into(),
        path: "ANGULAR.md".into(),
        content: "Use Angular 22 APIs.".into(),
        provenance: "workspace_root".into(),
        auto: false,
        sha256: "abc".into(),
    };

    engine
        .update_skills(&session_id, vec![skill.clone()])
        .unwrap();

    assert_eq!(engine.get(&session_id).unwrap().skills, vec![skill]);
}

#[tokio::test(flavor = "multi_thread")]
async fn reply_prompt_mode_controls_the_backend_contract() {
    let backend = Arc::new(MockBackend);
    let mut engine = Engine::new(backend);
    let mut start_params = params();
    start_params.mode = Mode::Investigate;
    let start = engine.start(start_params).await.unwrap();
    let generation = engine.begin_turn(&start.session_id).unwrap();

    let result = engine
        .reply_with_progress_generation(
            &start.session_id,
            generation,
            "Napraw to".into(),
            Mode::Fix,
            None,
        )
        .await
        .unwrap();

    assert!(matches!(result.card, Card::Patch(_)));
    assert_eq!(engine.get(&start.session_id).unwrap().mode, Mode::Fix);
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
        call_hierarchy: None,
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
    goal.mode = Mode::Investigate;

    let start = engine.start(goal).await.unwrap();
    let first = engine
        .action(&start.session_id, Action::Goal)
        .await
        .unwrap();
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

    // One conversational start, one explicitly authorized goal turn, and two
    // consumed speculations; no accept action waited for a fresh model turn.
    let calls = backend.calls.lock().unwrap().clone();
    assert_eq!(calls.len(), 4, "unexpected backend calls: {calls:?}");
    assert!(calls[0].starts_with("progress:Start"));
    assert!(calls[1].starts_with("progress:User(Goal)"));
    assert!(calls[2].starts_with("plain:User(Goal)"));
    assert!(calls[3].starts_with("plain:User(Goal)"));
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
    goal.mode = Mode::Investigate;
    let discovery = engine.start(goal).await.unwrap();
    let start = engine
        .action(&discovery.session_id, Action::Goal)
        .await
        .unwrap();
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
        "already-finished speculation must fold into the session totals"
    );
    assert!(
        !engine.continuations.contains_key(&start.session_id),
        "rejecting must leave no continuation in flight"
    );

    let calls = backend.calls.lock().unwrap().clone();
    assert_eq!(calls.len(), 3, "reject triggered a backend turn: {calls:?}");
    assert!(calls[0].starts_with("progress:Start"));
    assert!(calls[1].starts_with("progress:User(Goal)"));
    assert!(calls[2].starts_with("plain:User(Goal)"));

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
    goal.mode = Mode::Investigate;
    let discovery = engine.start(goal).await.unwrap();
    let first = engine
        .action(&discovery.session_id, Action::Goal)
        .await
        .unwrap();
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
async fn retry_interrupts_an_inflight_goal_continuation() {
    let backend = Arc::new(HangingGoalContinuationBackend::default());
    let mut engine = Engine::new(backend.clone());
    engine.set_source_context_provider(sliced_goal_provider());
    let start = engine.start(params()).await.unwrap();
    let first = engine
        .action(&start.session_id, Action::Goal)
        .await
        .unwrap();
    assert!(matches!(first.card, Card::Patch(_)));
    assert!(engine.continuations.contains_key(&first.session_id));

    let redrafted = engine
        .action(&first.session_id, Action::Retry)
        .await
        .unwrap();

    assert!(matches!(redrafted.card, Card::Patch(_)));
    assert!(backend.cancelled.load(Ordering::SeqCst));
    assert!(!engine.continuations.contains_key(&first.session_id));
}

#[tokio::test(flavor = "multi_thread")]
async fn single_file_goal_without_plan_completes_as_a_batch_of_one() {
    let backend = Arc::new(SingleFileNoPlanBackend::default());
    let mut engine = Engine::new(backend.clone());
    let mut goal = params();
    goal.mode = Mode::Investigate;
    goal.buffer_text = "first".into();

    let discovery = engine.start(goal).await.unwrap();
    let first = engine
        .action(&discovery.session_id, Action::Goal)
        .await
        .unwrap();
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
    assert_eq!(backend.calls.load(Ordering::SeqCst), 2);
}

#[derive(Default)]
struct SingleFileNoPlanBackend {
    calls: AtomicUsize,
}

#[async_trait]
impl BackendAdapter for SingleFileNoPlanBackend {
    async fn next_card(&self, req: BackendRequest) -> Result<BackendResponse> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if !req.card_contract.allow_goal_completion {
            return Ok(discovery_response("single_file_no_plan"));
        }
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
        if !req.card_contract.allow_goal_completion {
            return Ok(discovery_response("batch_goal"));
        }
        assert!(req.card_contract.allow_goal_completion);
        assert_eq!(
            req.card_contract.max_patch_files,
            loopbiotic_protocol::MAX_PATCH_FILES
        );
        assert_eq!(
            req.card_contract.max_hunks_per_patch,
            loopbiotic_protocol::MAX_HUNKS_PER_PATCH
        );
        assert_eq!(
            req.card_contract.max_changed_lines,
            loopbiotic_protocol::MAX_CHANGED_LINES
        );

        let card = match req.action {
            BackendAction::User(Action::Goal) => Card::Patch(PatchCard {
                id: "c_batch".into(),
                title: "Complete local change".into(),
                explanation: "Prepare both independent edits.".into(),
                warnings: vec![],
                goal_complete: true,
                plan: None,
                patches: vec![FilePatch {
                    id: "p_batch".into(),
                    file: "src/work.ts".into(),
                    diff: "@@ -1,3 +1,4 @@\n+interface Work {}\n first\n middle\n-last\n+Work\n"
                        .into(),
                    explanation: "Adds a declaration and separately replaces its use.".into(),
                }],
                actions: vec![Action::Apply, Action::Why, Action::Retry, Action::Stop],
            }),
            BackendAction::ContractRetry(reason) => {
                assert!(reason.contains("Return ONLY one of its separated change blocks"));
                assert!(reason.contains("return ONLY the dependency-producing declaration block"));
                assert!(reason.contains("leave every consumer byte-for-byte unchanged"));
                assert!(reason.contains("Mark goal_complete=false"));
                Card::Patch(PatchCard {
                    id: "c_declaration".into(),
                    title: "Introduce the interface first".into(),
                    explanation: "Add only the dependency required by the later implementation."
                        .into(),
                    warnings: vec![],
                    goal_complete: false,
                    plan: None,
                    patches: vec![FilePatch {
                        id: "p_declaration".into(),
                        file: "src/work.ts".into(),
                        diff: "@@ -1,1 +1,2 @@\n+interface Work {}\n first\n".into(),
                        explanation: "Introduces the interface before any use.".into(),
                    }],
                    actions: vec![Action::Apply, Action::Why, Action::Retry, Action::Stop],
                })
            }
            action => panic!("unexpected batch-goal action {action:?}"),
        };

        Ok(BackendResponse {
            card,
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
        if !req.card_contract.allow_goal_completion {
            return Ok(discovery_response("multi_file_goal"));
        }
        assert_eq!(
            req.card_contract.max_patch_files,
            loopbiotic_protocol::MAX_PATCH_FILES
        );

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

struct HangingAcceptContinuationBackend;

#[async_trait]
impl BackendAdapter for HangingAcceptContinuationBackend {
    async fn next_card(&self, req: BackendRequest) -> Result<BackendResponse> {
        if matches!(req.action, BackendAction::User(Action::Goal))
            && matches!(req.session.last_card, Some(Card::Patch(_)))
        {
            return std::future::pending().await;
        }
        MockBackend.next_card(req).await
    }

    fn capabilities(&self) -> BackendInfo {
        MockBackend::info()
    }
}

#[derive(Default)]
struct HangingGoalContinuationBackend {
    inner: MockBackend,
    cancelled: AtomicBool,
}

#[async_trait]
impl BackendAdapter for HangingGoalContinuationBackend {
    async fn next_card(&self, req: BackendRequest) -> Result<BackendResponse> {
        if matches!(req.action, BackendAction::User(Action::Goal))
            && matches!(req.session.last_card, Some(Card::Patch(_)))
        {
            return std::future::pending().await;
        }
        self.inner.next_card(req).await
    }

    async fn cancel_turn(&self, _session_id: &str) -> Result<()> {
        self.cancelled.store(true, Ordering::SeqCst);
        Ok(())
    }

    fn capabilities(&self) -> BackendInfo {
        MockBackend::info()
    }
}

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
                    flow_path: vec![],
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
                flow_path: vec![],
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
                flow_path: vec![],
                actions: vec![Action::Follow, Action::Why, Action::Stop],
            }),
            _ => Card::Finding(FindingCard {
                id: "c_2".into(),
                title: "Recovered".into(),
                finding: "Session still works.".into(),
                location: None,
                annotation: None,
                flow_path: vec![],
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
                flow_path: vec![],
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
                flow_path: vec![],
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
                flow_path: vec![],
                actions: vec![Action::Fix, Action::Stop],
            }),
            _ => Card::Finding(FindingCard {
                id: "c_finding".into(),
                title: "Wrong type".into(),
                finding: "This is deliberately not a patch.".into(),
                location: None,
                annotation: None,
                flow_path: vec![],
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
async fn read_only_accept_continuation_is_consumed_without_another_turn() {
    let backend = Arc::new(CountingBackend::default());
    let mut engine = Engine::new(backend.clone());
    engine.set_prefetch_mode(PrefetchMode::ReadOnly);

    let start = engine.start(params()).await.unwrap();
    let patch = engine.action(&start.session_id, Action::Fix).await.unwrap();
    assert!(matches!(patch.card, Card::Patch(_)));
    assert!(engine.prefetches.contains_key(&start.session_id));

    let result = engine
        .apply_result(accept(
            &patch.session_id,
            &patch.card,
            "payload = payload or {}",
        ))
        .await
        .unwrap();

    assert!(matches!(result.card, Card::Summary(_)));
    assert_eq!(
        result.goal.status,
        loopbiotic_protocol::GoalStatus::Complete
    );
    assert!(result.turn_token_usage.total_tokens > 0);
    let calls = backend.calls.lock().unwrap().clone();
    assert_eq!(calls.len(), 3, "unexpected backend calls: {calls:?}");
    assert!(calls[0].starts_with("progress:Start"));
    assert!(calls[1].starts_with("progress:User(Fix)"));
    assert!(calls[2].starts_with("plain:User(Goal)"));
}

#[tokio::test(flavor = "multi_thread")]
async fn rejecting_a_patch_cancels_read_only_speculation_without_redrafting() {
    let backend = Arc::new(CountingBackend::default());
    let mut engine = Engine::new(backend.clone());
    engine.set_prefetch_mode(PrefetchMode::ReadOnly);

    let start = engine.start(params()).await.unwrap();
    let patch = engine.action(&start.session_id, Action::Fix).await.unwrap();
    let Card::Patch(card) = &patch.card else {
        panic!("expected patch card");
    };
    let rejected = engine
        .apply_result(PatchApplyResult {
            session_id: patch.session_id.clone(),
            card_id: patch.card.id().into(),
            accepted: false,
            patch_ids: vec![card.patches[0].id.clone()],
            changed_files: vec![],
            error: None,
            context: editor_context("placeholder"),
        })
        .await
        .unwrap();

    assert!(matches!(rejected.card, Card::Error(_)));
    assert!(engine.prefetches.is_empty());
    let calls = backend.calls.lock().unwrap().clone();
    assert!(calls.len() <= 3, "reject regenerated work: {calls:?}");
    assert!(
        !calls
            .iter()
            .any(|call| call.starts_with("progress:User(Goal)")),
        "reject started a replacement turn: {calls:?}"
    );
}

type RecordedExpectation = (Option<CardKind>, String, Vec<String>, bool, Vec<String>);

#[derive(Default)]
struct ExpectationRecorder {
    inner: MockBackend,
    requests: std::sync::Mutex<Vec<RecordedExpectation>>,
}

#[async_trait]
impl BackendAdapter for ExpectationRecorder {
    async fn next_card(&self, req: BackendRequest) -> Result<BackendResponse> {
        self.requests.lock().unwrap().push((
            req.card_contract.expected_kind,
            req.session.prompt.clone(),
            req.session.interaction_feedback.clone(),
            req.card_contract.conversation_only,
            req.session
                .project
                .as_ref()
                .map(|profile| profile.adapters.clone())
                .unwrap_or_default(),
        ));
        self.inner.next_card(req).await
    }

    fn capabilities(&self) -> BackendInfo {
        self.inner.capabilities()
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn interaction_feedback_is_injected_once_on_the_next_turn() {
    let backend = Arc::new(ExpectationRecorder::default());
    let mut engine = Engine::new(backend.clone());
    let start = engine.start(params()).await.unwrap();
    engine
        .record_interaction_feedback(
            &start.session_id,
            "Previous conversation exceeded the interaction deadline.",
        )
        .unwrap();

    engine
        .action(&start.session_id, Action::Follow)
        .await
        .unwrap();
    engine
        .action(&start.session_id, Action::OtherLead)
        .await
        .unwrap();

    let requests = backend.requests.lock().unwrap().clone();
    assert_eq!(
        requests[1].2,
        vec!["Previous conversation exceeded the interaction deadline."]
    );
    assert!(requests[2].2.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn investigate_prompt_requires_a_hypothesis() {
    let backend = Arc::new(ExpectationRecorder::default());
    let mut engine = Engine::new(backend.clone());

    let mut investigate = params();
    investigate.mode = Mode::Investigate;
    engine.start(investigate).await.unwrap();

    let requests = backend.requests.lock().unwrap().clone();
    assert_eq!(requests[0].0, Some(CardKind::Hypothesis));
    assert_eq!(requests[0].1, "payload is empty");
    assert!(!requests[0].3);
}

#[tokio::test(flavor = "multi_thread")]
async fn start_profiles_marker_activated_adapters_before_the_backend_turn() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(
        root.path().join("package.json"),
        r#"{"dependencies":{"@angular/core":"22.0.6"}}"#,
    )
    .unwrap();
    std::fs::write(
        root.path().join("deno.lock"),
        r#"{"specifiers":{"npm:@angular/core@22.0.6":"22.0.6"}}"#,
    )
    .unwrap();
    let backend = Arc::new(ExpectationRecorder::default());
    let mut engine = Engine::new(backend.clone());
    let mut start = params();
    start.cwd = root.path().to_path_buf();

    engine.start(start).await.unwrap();

    let requests = backend.requests.lock().unwrap();
    assert!(requests[0].4.contains(&"angular".into()));
    assert!(requests[0].4.contains(&"package-workspace".into()));
}

#[tokio::test(flavor = "multi_thread")]
async fn slash_prefixed_text_cannot_override_the_visible_mode() {
    let backend = Arc::new(ExpectationRecorder::default());
    let mut engine = Engine::new(backend.clone());
    let mut start_params = params();
    start_params.prompt = "/patch guard the payload".into();

    engine.start(start_params).await.unwrap();

    let requests = backend.requests.lock().unwrap().clone();
    assert_eq!(requests[0].0, Some(CardKind::Hypothesis));
    assert_eq!(requests[0].1, "/patch guard the payload");
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
                flow_path: vec![],
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
        call_hierarchy: None,
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
