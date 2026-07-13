use std::collections::HashMap;
use std::path::PathBuf;

use pair_protocol::{
    Card, CardKind, ContextBundle, ContextPolicy, Cursor, GoalStatus, Location, Mode,
    ObservationProgress, PatchId, Selection, StartSessionParams, TokenUsage,
};
use uuid::Uuid;

use crate::state::SessionState;

pub type SessionId = String;

#[derive(Clone, Debug)]
pub struct Session {
    pub id: SessionId,
    pub cwd: PathBuf,
    pub initial_file: PathBuf,
    pub initial_cursor: Cursor,
    pub initial_selection: Option<Selection>,
    pub original_prompt: String,
    // Card kind demanded by a "/{kind}" prefix on the prompt; None means the
    // agent may answer with whatever kind fits, including a clarifying choice.
    pub forced_kind: Option<CardKind>,
    pub mode: Mode,
    pub cards: Vec<Card>,
    pub accepted_patches: Vec<PatchId>,
    pub rejected_patches: Vec<PatchId>,
    pub opened_locations: Vec<Location>,
    pub constraints: Vec<String>,
    pub completed_steps: Vec<String>,
    pub goal_status: GoalStatus,
    pub next_step: Option<String>,
    pub known_observations: Vec<ObservationProgress>,
    pub observation_index: HashMap<String, usize>,
    pub state: SessionState,
    pub context: ContextBundle,
    pub token_usage: TokenUsage,
    pub context_policy: ContextPolicy,
}

impl Session {
    pub fn new(params: StartSessionParams) -> Self {
        let context = ContextBundle::from_start(params.clone());
        let (forced_kind, prompt) = parse_kind_prefix(&params.prompt);

        Self {
            id: format!("s_{}", Uuid::new_v4().simple()),
            cwd: params.cwd,
            initial_file: params.file,
            initial_cursor: params.cursor,
            initial_selection: params.selection,
            original_prompt: prompt,
            forced_kind,
            mode: params.mode,
            cards: vec![],
            accepted_patches: vec![],
            rejected_patches: vec![],
            opened_locations: vec![],
            constraints: vec!["one card only".into(), "patches require user apply".into()],
            completed_steps: vec![],
            goal_status: GoalStatus::Active,
            next_step: None,
            known_observations: vec![],
            observation_index: HashMap::new(),
            state: SessionState::Thinking,
            context,
            token_usage: TokenUsage::default(),
            context_policy: params.context_policy,
        }
    }

    pub fn next_card_id(&self, label: &str) -> String {
        format!("c_{}_{}", label, self.cards.len() + 1)
    }
}

/// Splits a "/{kind}" prefix off the prompt. Unknown words after "/" are left
/// untouched so prompts may legitimately start with paths like "/tmp/x".
pub fn parse_kind_prefix(prompt: &str) -> (Option<CardKind>, String) {
    let trimmed = prompt.trim_start();
    let Some(rest) = trimmed.strip_prefix('/') else {
        return (None, prompt.trim().to_string());
    };
    let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
    let (word, remainder) = rest.split_at(end);
    let kind = match word.to_ascii_lowercase().as_str() {
        "hypothesis" => CardKind::Hypothesis,
        "finding" => CardKind::Finding,
        "patch" | "fix" => CardKind::Patch,
        "choice" => CardKind::Choice,
        "summary" => CardKind::Summary,
        _ => return (None, prompt.trim().to_string()),
    };

    (Some(kind), remainder.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_kind_prefixes() {
        assert_eq!(
            parse_kind_prefix("/patch guard the payload"),
            (Some(CardKind::Patch), "guard the payload".into())
        );
        assert_eq!(
            parse_kind_prefix("/fix guard the payload"),
            (Some(CardKind::Patch), "guard the payload".into())
        );
        assert_eq!(
            parse_kind_prefix("/Choice how should icons scale?"),
            (Some(CardKind::Choice), "how should icons scale?".into())
        );
        assert_eq!(
            parse_kind_prefix("/patch"),
            (Some(CardKind::Patch), "".into())
        );
    }

    #[test]
    fn leaves_plain_and_unknown_prompts_untouched() {
        assert_eq!(
            parse_kind_prefix("why is payload empty"),
            (None, "why is payload empty".into())
        );
        assert_eq!(
            parse_kind_prefix("/tmp/project has a broken build"),
            (None, "/tmp/project has a broken build".into())
        );
        assert_eq!(
            parse_kind_prefix("/patchy naming is odd"),
            (None, "/patchy naming is odd".into())
        );
    }
}
