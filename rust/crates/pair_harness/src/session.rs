use std::collections::HashMap;
use std::path::PathBuf;

use pair_protocol::{
    Card, ContextBundle, ContextPolicy, Cursor, Location, Mode, ObservationProgress, PatchId,
    Selection, StartSessionParams, TokenUsage,
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
    pub mode: Mode,
    pub cards: Vec<Card>,
    pub accepted_patches: Vec<PatchId>,
    pub rejected_patches: Vec<PatchId>,
    pub opened_locations: Vec<Location>,
    pub constraints: Vec<String>,
    pub completed_steps: Vec<String>,
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

        Self {
            id: format!("s_{}", Uuid::new_v4().simple()),
            cwd: params.cwd,
            initial_file: params.file,
            initial_cursor: params.cursor,
            initial_selection: params.selection,
            original_prompt: params.prompt,
            mode: params.mode,
            cards: vec![],
            accepted_patches: vec![],
            rejected_patches: vec![],
            opened_locations: vec![],
            constraints: vec!["one card only".into(), "patches require user apply".into()],
            completed_steps: vec![],
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
