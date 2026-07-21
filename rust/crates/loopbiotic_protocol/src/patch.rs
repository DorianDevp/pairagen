use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::context::ContextBundle;

pub type PatchId = String;

pub const MAX_PATCH_FILES: usize = 1;
/// Maximum unified-diff headers per patch. The patch validator additionally
/// requires one contiguous change run inside that header.
pub const MAX_HUNKS_PER_PATCH: usize = 1;
pub const MAX_CHANGED_LINES: usize = 32;
/// Maximum filesystem operations (moves/renames) in one reviewed proposal.
pub const MAX_FILE_OPS: usize = 16;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FilePatch {
    pub id: PatchId,
    pub file: PathBuf,
    pub diff: String,
    pub explanation: String,
}

/// One filesystem operation proposed inside a patch card. Like a diff hunk,
/// it is inert until the editor's explicit Accept applies it.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FileOp {
    pub id: PatchId,
    pub kind: FileOpKind,
    pub from: PathBuf,
    pub to: PathBuf,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FileOpKind {
    /// Rename/move a file or directory inside the workspace. Missing target
    /// parent directories are created by the editor on Accept.
    Move,
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
