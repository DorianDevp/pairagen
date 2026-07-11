use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::context::ContextBundle;

pub type PatchId = String;

pub const MAX_PATCH_FILES: usize = 1;
pub const MAX_HUNKS_PER_PATCH: usize = 1;
pub const MAX_CHANGED_LINES: usize = 32;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FilePatch {
    pub id: PatchId,
    pub file: PathBuf,
    pub diff: String,
    pub explanation: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PatchApplyResult {
    pub session_id: String,
    pub card_id: String,
    pub accepted: bool,
    pub patch_ids: Vec<PatchId>,
    pub changed_files: Vec<PathBuf>,
    pub error: Option<String>,
    pub context: ContextBundle,
}
