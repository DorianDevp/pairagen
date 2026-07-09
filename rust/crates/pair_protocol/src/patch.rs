use std::path::PathBuf;

use serde::{Deserialize, Serialize};

pub type PatchId = String;

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
}
