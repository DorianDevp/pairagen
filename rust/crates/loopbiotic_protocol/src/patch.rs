use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::context::ContextBundle;

pub type PatchId = String;

/// A backend response may target only one file. Cross-file work remains a
/// sequence of separately generated and reviewed turns.
pub const MAX_PATCH_FILES: usize = 1;
/// Maximum unified-diff headers accepted for one file from a backend response.
/// The harness splits them into one-hunk review cards before anything is shown.
pub const MAX_HUNKS_PER_PATCH: usize = 16;
/// Maximum total review steps materialized from one backend response, including
/// separate change runs that arrived under one unified-diff header.
pub const MAX_REVIEW_QUEUE_HUNKS: usize = 16;
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
