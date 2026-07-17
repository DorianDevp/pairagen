//! The retry loop for a single model turn: deduplication against session
//! memory, contract validation, automatic repair retries, and conversion of
//! failures into typed error cards.

use anyhow::{Result, anyhow};
use loopbiotic_backends::{BackendAction, BackendProgress, BackendResponse, ProgressReporter};
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
            let validation = if matches!(expected, NextState::GoalLoop) {
                self.normalize_goal_batch(&mut candidate, &context, &session.id)
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
                            message: "Patch contract failed; Codex is repairing the local step"
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
        // Correct miscounted hunk headers before the count check rejects them.
        PatchNormalizer::normalize_hunk_headers(candidate)?;
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

            let mut single = Card::Patch(loopbiotic_protocol::PatchCard {
                id: card.id.clone(),
                title: card.title.clone(),
                explanation: card.explanation.clone(),
                warnings: vec![],
                goal_complete: card.goal_complete,
                plan: None,
                patches: vec![card.patches[index].clone()],
                actions: card.actions.clone(),
            });
            PatchNormalizer::normalize_card(&mut single, &source)
                .map_err(|error| error.context(file.display().to_string()))?;
            PatchValidator::validate_card_against_context(&single, &source)
                .map_err(|error| error.context(file.display().to_string()))?;

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

        NextState::GoalLoop.validate(candidate)
    }
}

fn repair_instruction(expected: &NextState, class: ViolationClass) -> &'static str {
    if class == ViolationClass::MultiHunk {
        if matches!(expected, NextState::GoalLoop) {
            return "DO NOT repeat or reformat the batch. Return ONLY one of its separated change blocks. If one block declares an interface, type, function, field, import, or compatibility shim and another block uses it, return ONLY the dependency-producing declaration block now and leave every consumer byte-for-byte unchanged. Mark goal_complete=false and put the consumer change in plan.remaining. In structured hunk lines, after the first add/remove record, context ends the change block: no later add/remove record is allowed. This patch alone must compile and type-check. If no isolated block is compiler-valid, return choice or deny instead of a patch.";
        }

        return "DO NOT repeat or reformat the batch. Return ONLY one of its separated change blocks. If one block declares an interface, type, function, field, import, or compatibility shim and another block uses it, return ONLY the dependency-producing declaration block now and leave every consumer byte-for-byte unchanged. In structured hunk lines, after the first add/remove record, context ends the change block: no later add/remove record is allowed. This patch alone must compile and type-check. If no isolated block is compiler-valid, return choice or deny instead of a patch.";
    }

    if class == ViolationClass::ContextMismatch {
        if matches!(expected, NextState::GoalLoop) {
            return "DO NOT repeat the malformed hunk and do not widen the corrected step. Re-read the supplied buffer and rebuild the same single change block plus its refreshed plan. Between the first and last context/remove record, include every existing source line exactly once and in order, including existing blank lines. An added blank line never replaces an omitted blank context line. Keep dependency ordering, compiler acceptance, goal_complete, and plan.remaining consistent with the same isolated step.";
        }

        return "DO NOT repeat the malformed hunk and do not widen the corrected step. Re-read the supplied buffer and rebuild the same single change block. Between the first and last context/remove record, include every existing source line exactly once and in order, including existing blank lines. An added blank line never replaces an omitted blank context line. Keep the patch independently compiling and type-checking.";
    }

    if matches!(expected, NextState::GoalLoop) {
        "Re-read the affected block with read-only tools and return the corrected patch with the same small scope plus its refreshed plan. It must contain exactly one uninterrupted change block. Preserve compiler acceptance after this patch alone and order declarations or interfaces before every later use or implementation. Context/remove lines must be exact and contiguous. Use open_location only if the required source cannot be inspected."
    } else {
        "Rebuild the same step as exactly one uninterrupted change block. Source context/remove lines must be exact and contiguous in the supplied buffer; added lines do not replace omitted source context. The resulting local step must compile and type-check by itself without work deferred to a later card. Introduce declarations or interfaces in an independently valid patch before any later use or implementation. If the change belongs in a different file than the supplied buffer, return an open_location op with that place instead of another patch."
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
            actions: vec![Action::Apply],
        });

        assert!(duplicate_completed_step(&session, &card).is_some());
    }

    #[test]
    fn multi_hunk_repair_demands_dependency_only_before_consumer() {
        let instruction = repair_instruction(&NextState::GoalLoop, ViolationClass::MultiHunk);

        assert!(instruction.contains("Return ONLY one of its separated change blocks"));
        assert!(instruction.contains("ONLY the dependency-producing declaration block"));
        assert!(instruction.contains("leave every consumer byte-for-byte unchanged"));
        assert!(instruction.contains("goal_complete=false"));
        assert!(instruction.contains("no later add/remove record is allowed"));
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
