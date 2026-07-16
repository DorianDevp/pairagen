//! Structural validation of backend cards, patch targets, and editor apply
//! results before they are accepted into a session.

use anyhow::{Result, anyhow};
use loopbiotic_patch::PatchValidator;
use loopbiotic_protocol::{
    Card, ContextBundle, MAX_GOAL_CHANGED_LINES, MAX_GOAL_HUNKS_PER_PATCH, MAX_GOAL_PATCH_FILES,
    PatchApplyResult,
};

use crate::session::Session;
use crate::state::NextState;

pub(super) fn validate_backend_card(
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

pub(super) fn context_targets(context: &ContextBundle, file: &std::path::Path) -> bool {
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

pub(super) fn validate_one_card(card: &Card) -> Result<()> {
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
            if let Some(loopbiotic_protocol::NextMove::OpenLocation(location)) = &card.next_move {
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

pub(super) fn validate_apply_result(session: &Session, result: &PatchApplyResult) -> Result<()> {
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

#[cfg(test)]
mod tests {
    use loopbiotic_protocol::{Action, FindingCard};

    use super::*;

    #[test]
    fn rejects_card_with_invalid_location_coordinates() {
        let card = Card::Finding(FindingCard {
            id: "c_bad_location".into(),
            title: "Target".into(),
            finding: "The target is here.".into(),
            location: Some(loopbiotic_protocol::Location {
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
}
