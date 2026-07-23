//! The retry loop for a single model turn: deduplication against session
//! memory, contract validation, automatic repair retries, and conversion of
//! failures into typed error cards.

use anyhow::{Result, anyhow};
use loopbiotic_backends::{
    BackendAction, BackendProgress, BackendResponse, ProgressReporter, UNPARSED_OUTPUT_CARD_ID,
};
use loopbiotic_patch::{
    PatchCoherence, PatchNormalizer, PatchValidator, violation, violation_class,
};
use loopbiotic_protocol::{
    Action, AgentAttempt, Card, ContextBundle, ErrorCard, TokenUsage, ViolationClass,
};

use crate::session::Session;
use crate::state::NextState;

use super::goal::rejected_card;
use super::observations::{activate_observation, core_observation, normalize_observation};
use super::validate::{context_targets, validate_backend_card, validate_one_card};
use super::{Engine, MAX_LOCATION_GRANTS};

impl Engine {
    pub(super) async fn next_distinct_response(
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
                        violation_class: None,
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
                            preview: None,
                        });
                    }

                    if let Some(granted) = granter(request.clone(), session.id.clone()).await {
                        attempts.push(agent_attempt(
                            attempt + 1,
                            &response,
                            "location_granted",
                            Some(request.location.file.display().to_string()),
                            attempt_usage,
                            None,
                            false,
                        ));
                        session.context = granted.clone();
                        context = granted;
                        action = BackendAction::LocationGranted(request.location.file.clone());
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
                    None,
                    false,
                ));
                response.card = Card::Deny(loopbiotic_protocol::DenyCard {
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

            // Deterministic fallback for a mis-worded permission request: a
            // patch-expected turn that denies while pointing at a different
            // workspace file is mechanically the same ask as open_location
            // ("confirm opening X"), so it runs the same permission gate
            // instead of dead-ending on the model's choice of op. Declining
            // returns the model's own deny card unchanged.
            if let Card::Deny(deny) = &response.card
                && matches!(expected, NextState::Patch | NextState::GoalLoop)
                && let Some(location) = deny.location.clone()
                && !context_targets(&context, &location.file)
                && grants < MAX_LOCATION_GRANTS
                && let Some(granter) = &self.location_granter
            {
                if let Some(progress) = &progress {
                    progress(BackendProgress {
                        session_id: session.id.clone(),
                        phase: "permission".into(),
                        message: format!("Agent asks to open {}", location.file.display()),
                        preview: None,
                    });
                }

                let request = loopbiotic_protocol::OpenLocationCard {
                    id: session.next_card_id("nav"),
                    reason: deny.reason.clone(),
                    location: location.clone(),
                };
                if let Some(granted) = granter(request, session.id.clone()).await {
                    attempts.push(agent_attempt(
                        attempt + 1,
                        &response,
                        "location_granted",
                        Some(location.file.display().to_string()),
                        attempt_usage,
                        None,
                        false,
                    ));
                    session.context = granted.clone();
                    context = granted;
                    action = BackendAction::LocationGranted(location.file);
                    grants += 1;
                    continue;
                }

                attempts.push(agent_attempt(
                    attempt + 1,
                    &response,
                    "location_declined",
                    Some(location.file.display().to_string()),
                    attempt_usage,
                    None,
                    false,
                ));
                response.metadata.token_usage = token_usage;
                response.metadata.attempts = attempts;
                return response;
            }

            // The backend surfaced output it could not parse as an op at all.
            // The model's previous text is already in its thread, so ask it to
            // re-emit strict JSON instead of accepting the error card on the
            // first try; after three failures the error card (with raw output)
            // stands, exactly as before.
            if let Card::Error(card) = &response.card
                && card.id == UNPARSED_OUTPUT_CARD_ID
            {
                let detail = card
                    .message
                    .lines()
                    .next()
                    .unwrap_or("the response was not a parseable Loopbiotic op")
                    .to_string();
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
                    Some(ViolationClass::UnparsedOutput),
                    true,
                ));
                if attempt < 2 {
                    if let Some(progress) = &progress {
                        progress(BackendProgress {
                            session_id: session.id.clone(),
                            phase: "repairing".into(),
                            message: "The reply was not machine-readable; requesting strict JSON"
                                .into(),
                            preview: None,
                        });
                    }
                    action = BackendAction::ContractRetry(format!(
                        "Your previous response could not be parsed as a Loopbiotic op: {detail}. \
                         Re-emit the complete op as exactly one strict JSON object with no prose \
                         or code fences around it, and escape every double-quote character inside \
                         string values (including typographic quotes such as \u{201e} and \u{201d})."
                    ));
                    attempt += 1;
                    continue;
                }

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
                    Some(ViolationClass::DuplicateStep),
                    true,
                ));
                if attempt < 2 {
                    if let Some(progress) = &progress {
                        progress(BackendProgress {
                            session_id: session.id.clone(),
                            phase: "deduplicating".into(),
                            message: "Retaining repeated context and requesting a distinct step"
                                .into(),
                            preview: None,
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
                    Some(ViolationClass::DuplicateStep),
                    true,
                ));
                if attempt < 2 {
                    if let Some(progress) = &progress {
                        progress(BackendProgress {
                            session_id: session.id.clone(),
                            phase: "deduplicating".into(),
                            message: "Rejecting a repeated patch step".into(),
                            preview: None,
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
            let validation = if matches!(candidate, Card::Patch(_)) {
                self.normalize_patch_batch(&mut candidate, &context, &session.id, expected)
                    .await
            } else {
                PatchNormalizer::normalize_card(&mut candidate, &context)
                    .and_then(|()| validate_backend_card(&candidate, expected, &context))
            }
            .map(|()| PatchCoherence::annotate(&mut candidate));
            if let Err(error) = validation {
                let class = violation_class(&error).unwrap_or(ViolationClass::Other);
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
                    Some(class),
                    true,
                ));
                if attempt < 2 {
                    if let Some(progress) = &progress {
                        progress(BackendProgress {
                            session_id: session.id.clone(),
                            phase: "repairing".into(),
                            message: "Patch contract failed; the agent is repairing the local step"
                                .into(),
                            preview: None,
                        });
                    }
                    let instruction = repair_instruction(expected, class);
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
                None,
                false,
            ));
            response.metadata.token_usage = token_usage;
            response.metadata.attempts = attempts;
            return response;
        }

        // Every branch of the final (third) attempt above returns, so this
        // should not run. The invariant is subtle, though — the location-grant
        // branch continues without incrementing `attempt` — so if a future
        // edit ever falls through, degrade to the same error card the
        // backend-failure path produces instead of panicking the daemon
        // mid-request.
        let mut response = backend_failure_response(
            session,
            anyhow!("the agent produced no acceptable card after {attempt} attempts"),
        );
        response.metadata.token_usage = token_usage;
        response.metadata.attempts = attempts;
        response
    }

    async fn normalize_patch_batch(
        &self,
        candidate: &mut Card,
        current: &ContextBundle,
        session_id: &str,
        expected: &NextState,
    ) -> Result<()> {
        validate_one_card(candidate)?;
        // Correct miscounted hunk headers before the count check rejects them.
        PatchNormalizer::normalize_hunk_headers(candidate)?;
        // Several `@@` hunks — or several separated change runs hidden under
        // one header — are a local review queue, not a model contract failure.
        PatchNormalizer::split_change_runs(candidate)?;
        PatchValidator::validate_card(candidate)?;
        let Card::Patch(card) = candidate else {
            // Guarded by the `matches!` early return above; degrade to a
            // contract failure (retry/rejected card) instead of panicking if
            // that guard ever drifts.
            return Err(violation(
                ViolationClass::IncoherentBatch,
                "goal batch candidate is no longer a patch card",
            ));
        };

        for index in 0..card.patches.len() {
            let file = card.patches[index].file.clone();
            // The action context is already the editor's fresh snapshot. Use
            // it directly for the active file instead of paying another RPC
            // round-trip (and risking a read timeout) for identical source.
            // The provider remains necessary for a goal step in another file.
            let source = if context_targets(current, &file) {
                Some(current.clone())
            } else if let Some(provider) = &self.source_context_provider {
                provider(file.clone(), session_id.to_string()).await
            } else {
                None
            }
            .ok_or_else(|| {
                violation(
                    ViolationClass::IncoherentBatch,
                    format!("editor source is unavailable for {}", file.display()),
                )
            })?;

            if !context_targets(&source, &file) {
                return Err(violation(
                    ViolationClass::IncoherentBatch,
                    format!(
                        "editor returned {} while validating {}",
                        source.file.display(),
                        file.display()
                    ),
                ));
            }

            let original = Card::Patch(loopbiotic_protocol::PatchCard {
                id: card.id.clone(),
                title: card.title.clone(),
                explanation: card.explanation.clone(),
                warnings: vec![],
                goal_complete: card.goal_complete,
                plan: None,
                patches: vec![card.patches[index].clone()],
                file_ops: vec![],
                actions: card.actions.clone(),
            });
            let mut single = original.clone();
            let mut validation = PatchNormalizer::normalize_card(&mut single, &source)
                .and_then(|()| PatchValidator::validate_card_against_context(&single, &source));

            // The active editor capture is cursor-bounded. If a valid hunk for
            // that same file sits just outside the excerpt, ask the editor for
            // its full live buffer before blaming the model or spending a
            // repair turn.
            if validation.as_ref().err().and_then(violation_class)
                == Some(ViolationClass::ContextMismatch)
                && context_targets(current, &file)
                && let Some(provider) = &self.source_context_provider
                && let Some(full_source) = provider(file.clone(), session_id.to_string()).await
            {
                single = original;
                validation =
                    PatchNormalizer::normalize_card(&mut single, &full_source).and_then(|()| {
                        PatchValidator::validate_card_against_context(&single, &full_source)
                    });
            }
            validation.map_err(|error| {
                let class = violation_class(&error).unwrap_or(ViolationClass::Other);
                violation(class, format!("{}: {error:#}", file.display()))
            })?;

            let Card::Patch(single) = single else {
                // Constructed as a patch card just above and normalization
                // never changes the card kind; degrade instead of panicking.
                return Err(violation(
                    ViolationClass::IncoherentBatch,
                    format!(
                        "{}: patch normalization changed the card kind",
                        file.display()
                    ),
                ));
            };
            card.patches[index] = single.patches.into_iter().next().ok_or_else(|| {
                violation(
                    ViolationClass::IncoherentBatch,
                    format!("{}: patch normalization dropped the hunk", file.display()),
                )
            })?;
        }

        expected.validate(candidate)
    }
}

fn repair_instruction(expected: &NextState, class: ViolationClass) -> &'static str {
    if class == ViolationClass::MultiFile {
        if matches!(expected, NextState::GoalLoop) {
            return "The response targeted multiple files. Return exactly ONE FILE. Keep that file's bounded hunks together, put declarations and dependency producers before consumers, and move every other file into plan.remaining. Every hunk must compile after the preceding hunks are accepted. Do not collapse a valid same-file multi-hunk batch into one arbitrary change.";
        }

        return "The response targeted multiple files. Return exactly ONE FILE, preferably the supplied active file. Keep its bounded hunks together, put declarations and dependency producers before consumers, and omit every other file. Every hunk must compile after the preceding hunks are accepted. Do not collapse a valid same-file multi-hunk batch into one arbitrary change.";
    }

    if class == ViolationClass::OversizedBatch {
        if matches!(expected, NextState::GoalLoop) {
            return "The one-file response exceeded a local review bound. Keep exactly ONE FILE and return only a bounded, dependency-ordered prefix of its hunks; move the overflow into plan.remaining. Every returned hunk must compile after the preceding hunks are accepted.";
        }

        return "The one-file response exceeded a local review bound. Keep exactly ONE FILE and return a smaller dependency-ordered hunk batch within the stated limits. Every returned hunk must compile after the preceding hunks are accepted.";
    }

    if class == ViolationClass::MultiHunk {
        return "Keep the response in exactly ONE FILE, but represent each disconnected change run as its own hunk. Preserve dependency order, with declarations and producers before consumers. Every hunk must compile after preceding hunks are accepted.";
    }

    if class == ViolationClass::WrongFile {
        return "The response targeted a file other than the supplied buffer. Do not retry the same patch and do not substitute a workaround change in the supplied buffer. If the correct change belongs in that other file, return an open_location op for it: the editor asks the user for permission and this same turn continues there with fresh ctx. Otherwise return the patch for exactly the supplied buffer.";
    }

    if class == ViolationClass::ContextMismatch {
        if matches!(expected, NextState::GoalLoop) {
            return "DO NOT repeat the malformed hunk and do not widen the corrected step. Re-read the supplied buffer and rebuild the same single change block plus its refreshed plan. Between the first and last context/remove record, include every existing source line exactly once and in order, including existing blank lines. An added blank line never replaces an omitted blank context line. Keep dependency ordering, compiler acceptance, goal_complete, and plan.remaining consistent with the same isolated step.";
        }

        return "DO NOT repeat the malformed hunk and do not widen the corrected step. Re-read the supplied buffer and rebuild the same single change block. Between the first and last context/remove record, include every existing source line exactly once and in order, including existing blank lines. An added blank line never replaces an omitted blank context line. Keep the patch independently compiling and type-checking.";
    }

    if matches!(expected, NextState::GoalLoop) {
        "Re-read the affected block with read-only tools and return the corrected same-file hunk batch with the same scope plus its refreshed plan. Every hunk contains one uninterrupted change block and must compile after preceding hunks are accepted. Order declarations or interfaces before every use or implementation. Context/remove lines must be exact and contiguous. Use open_location only if the required source cannot be inspected."
    } else {
        "Rebuild the same same-file hunk batch. Every hunk contains one uninterrupted change block; source context/remove lines must be exact and contiguous in the supplied buffer, and added lines do not replace omitted source context. Each hunk must compile after preceding hunks are accepted. Put declarations or interfaces before every use or implementation. If the change belongs in a different file than the supplied buffer, return an open_location op instead of another patch."
    }
}

fn agent_attempt(
    number: usize,
    response: &BackendResponse,
    outcome: &str,
    detail: Option<String>,
    token_usage: TokenUsage,
    violation_class: Option<ViolationClass>,
    include_candidate: bool,
) -> AgentAttempt {
    AgentAttempt {
        number,
        backend: response.metadata.backend.clone(),
        outcome: outcome.into(),
        token_usage,
        detail,
        violation_class,
        candidate_card: include_candidate.then(|| response.card.clone()),
        activities: response.metadata.activities.clone(),
    }
}

fn merge_usage(
    total: &mut Option<loopbiotic_protocol::TokenUsage>,
    turn: &Option<loopbiotic_protocol::TokenUsage>,
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

pub(super) fn normalize_step(text: &str) -> String {
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

fn backend_failure_response(session: &Session, error: anyhow::Error) -> BackendResponse {
    BackendResponse {
        card: Card::Error(ErrorCard {
            id: session.next_card_id("backend_error"),
            title: "Backend request failed".into(),
            message: format!("{error:#}"),
            actions: vec![Action::Retry, Action::EditPrompt, Action::Stop],
        }),
        raw_output: None,
        metadata: loopbiotic_backends::BackendMetadata {
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
        metadata: loopbiotic_backends::BackendMetadata {
            backend: "harness".into(),
            model: None,
            token_usage: None,
            activities: vec![],
            attempts: vec![],
        },
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use loopbiotic_protocol::{FilePatch, PatchCard};

    use super::super::tests::params;
    use super::*;

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
            plan: None,
            patches: vec![FilePatch {
                id: "p_repeat".into(),
                file: PathBuf::from("src/work.ts"),
                diff: "@@ -1,1 +1,1 @@\n-old\n+new\n".into(),
                explanation: "Extract the local helper used for payload validation".into(),
            }],
            file_ops: vec![],
            actions: vec![Action::Apply],
        });

        assert!(duplicate_completed_step(&session, &card).is_some());
    }

    #[test]
    fn multi_file_repair_preserves_same_file_hunks() {
        let instruction = repair_instruction(&NextState::GoalLoop, ViolationClass::MultiFile);

        assert!(instruction.contains("targeted multiple files"));
        assert!(instruction.contains("Return exactly ONE FILE"));
        assert!(instruction.contains("Keep that file's bounded hunks together"));
        assert!(instruction.contains("dependency producers before consumers"));
        assert!(instruction.contains("plan.remaining"));
        assert!(instruction.contains("Do not collapse a valid same-file multi-hunk batch"));
    }

    #[test]
    fn oversized_batch_repair_keeps_the_file_and_defers_only_overflow() {
        let instruction = repair_instruction(&NextState::GoalLoop, ViolationClass::OversizedBatch);

        assert!(instruction.contains("one-file response exceeded"));
        assert!(instruction.contains("dependency-ordered prefix"));
        assert!(instruction.contains("overflow into plan.remaining"));
    }

    #[test]
    fn context_repair_preserves_blank_source_lines_and_corrected_scope() {
        let instruction = repair_instruction(&NextState::GoalLoop, ViolationClass::ContextMismatch);

        assert!(instruction.contains("do not widen the corrected step"));
        assert!(instruction.contains("include every existing source line exactly once"));
        assert!(instruction.contains("including existing blank lines"));
        assert!(instruction.contains("never replaces an omitted blank context line"));
    }
}
