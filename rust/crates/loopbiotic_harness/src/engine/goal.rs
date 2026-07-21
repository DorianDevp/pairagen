//! Explicit-goal bookkeeping: tracking one reviewable hunk at a time, goal
//! progress/state, and final completion summaries.

use std::collections::VecDeque;

use anyhow::{Result, anyhow};
use loopbiotic_protocol::{
    Action, ActionResult, Card, ErrorCard, GoalProgress, SummaryCard, TokenUsage,
};

use crate::session::Session;
use crate::state::{NextState, SessionState};

use super::turn::normalize_step;

pub(super) fn goal_progress(session: &Session) -> GoalProgress {
    GoalProgress {
        statement: session.original_prompt.clone(),
        completed_steps: session.completed_steps.clone(),
        known_observations: session.known_observations.clone(),
        status: session.goal_status,
        next_step: session.next_step.clone(),
    }
}

pub(super) fn complete_goal_locally(session_id: &str, session: &mut Session) -> ActionResult {
    let mut changed_files = session
        .completed_step_signatures
        .iter()
        .map(|(file, _)| file.clone())
        .collect::<Vec<_>>();
    changed_files.sort();
    changed_files.dedup();

    session.state = SessionState::Summary;
    session.goal_status = loopbiotic_protocol::GoalStatus::Complete;
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

pub(super) fn update_goal_state(session: &mut Session, card: &Card, next_state: &NextState) {
    if !matches!(next_state, NextState::GoalLoop) {
        return;
    }

    match card {
        Card::Patch(card) => {
            session.goal_status = loopbiotic_protocol::GoalStatus::Active;
            session.next_step = Some(card.explanation.clone());
        }
        Card::Summary(_) => {
            session.goal_status = loopbiotic_protocol::GoalStatus::Complete;
            session.next_step = None;
        }
        Card::Choice(_) => {
            session.goal_status = loopbiotic_protocol::GoalStatus::Active;
            session.next_step = None;
        }
        _ => {}
    }
}

pub(super) fn completed_patch_steps(session: &Session) -> Vec<String> {
    let Some(Card::Patch(card)) = session.cards.last() else {
        return vec![];
    };

    card.patches
        .iter()
        .map(|patch| format!("{}: {}", patch.file.display(), patch.explanation))
        .collect()
}

pub(super) fn queue_goal_patch_cards(session: &mut Session, card: Card) -> Result<Card> {
    let Card::Patch(card) = card else {
        session.pending_patch_cards.clear();
        session.goal_slice_continues = false;
        return Ok(card);
    };

    // Validation already guarantees one file and one hunk. A plan describes
    // the remaining coherent steps; a planless card may only complete via
    // its explicit goal_complete flag.
    let plan = card.plan.clone();
    let completes = plan
        .as_ref()
        .map(|plan| plan.complete)
        .unwrap_or(card.goal_complete);
    session.goal_slice_continues = plan.as_ref().is_some_and(|plan| !plan.complete);

    let mut cards = Vec::new();
    for patch in card.patches {
        let diff = loopbiotic_patch::UnifiedDiff::parse(&patch.diff)?;
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
            cards.push(loopbiotic_protocol::PatchCard {
                id: format!("{}_h{}", card.id, cards.len() + 1),
                title: format!("{}{}", card.title, suffix),
                explanation: explanation.clone(),
                warnings: card.warnings.clone(),
                goal_complete: false,
                plan: plan.clone(),
                patches: vec![loopbiotic_protocol::FilePatch {
                    id: format!("{}_h{}", patch.id, index + 1),
                    file: patch.file.clone(),
                    diff: loopbiotic_patch::UnifiedDiff { hunks: vec![hunk] }.render(),
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
    if completes {
        if let Some(last) = pending.back_mut() {
            last.goal_complete = true;
        } else {
            first.goal_complete = true;
        }
    }
    session.pending_patch_cards = pending;

    Ok(Card::Patch(first))
}

pub(super) fn completed_patch_signatures(session: &Session) -> Vec<(std::path::PathBuf, String)> {
    let Some(Card::Patch(card)) = session.cards.last() else {
        return vec![];
    };

    card.patches
        .iter()
        .map(|patch| (patch.file.clone(), normalize_step(&patch.explanation)))
        .collect()
}

pub(super) fn rejected_card(
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
