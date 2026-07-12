use std::path::PathBuf;

use serde::{Deserialize, Serialize};

fn default_true() -> bool {
    true
}

fn default_total_token_budget() -> usize {
    2_400
}

fn default_reserved_tokens() -> usize {
    700
}

fn default_primary_token_budget() -> usize {
    1_000
}

fn default_max_artifacts() -> usize {
    4
}

fn default_snippet_lines() -> usize {
    10
}

fn default_max_scan_files() -> usize {
    2_000
}

fn default_max_file_bytes() -> usize {
    512 * 1_024
}

fn default_cache_ttl_ms() -> usize {
    1_500
}

fn default_min_artifact_score() -> i32 {
    40
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    #[default]
    Auto,
    Investigate,
    Fix,
    Explain,
    Propose,
    Review,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Cursor {
    pub line: usize,
    pub column: usize,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Selection {
    pub start: Cursor,
    pub end: Cursor,
    pub text: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Diagnostic {
    pub file: PathBuf,
    pub line: usize,
    pub column: usize,
    pub severity: String,
    pub message: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ContextPolicy {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_total_token_budget")]
    pub total_token_budget: usize,
    #[serde(default = "default_reserved_tokens")]
    pub reserved_tokens: usize,
    #[serde(default = "default_primary_token_budget")]
    pub primary_token_budget: usize,
    #[serde(default = "default_max_artifacts")]
    pub max_artifacts: usize,
    #[serde(default = "default_snippet_lines")]
    pub snippet_lines: usize,
    #[serde(default = "default_max_scan_files")]
    pub max_scan_files: usize,
    #[serde(default = "default_max_file_bytes")]
    pub max_file_bytes: usize,
    #[serde(default = "default_cache_ttl_ms")]
    pub cache_ttl_ms: usize,
    #[serde(default = "default_min_artifact_score")]
    pub min_artifact_score: i32,
    #[serde(default)]
    pub exclude: Vec<String>,
}

impl Default for ContextPolicy {
    fn default() -> Self {
        Self {
            enabled: true,
            total_token_budget: default_total_token_budget(),
            reserved_tokens: default_reserved_tokens(),
            primary_token_budget: default_primary_token_budget(),
            max_artifacts: default_max_artifacts(),
            snippet_lines: default_snippet_lines(),
            max_scan_files: default_max_scan_files(),
            max_file_bytes: default_max_file_bytes(),
            cache_ttl_ms: default_cache_ttl_ms(),
            min_artifact_score: default_min_artifact_score(),
            exclude: vec![],
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextArtifactKind {
    Definition,
    #[default]
    Reference,
    Dependency,
    Diagnostic,
    Test,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextHintKind {
    Definition,
    Declaration,
    TypeDefinition,
    Implementation,
    Reference,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ContextHint {
    pub file: PathBuf,
    pub line: usize,
    pub column: usize,
    pub kind: ContextHintKind,
    pub source: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ContextArtifact {
    pub file: PathBuf,
    pub start_line: usize,
    pub end_line: usize,
    pub kind: ContextArtifactKind,
    pub reason: String,
    pub text: String,
    pub estimated_tokens: usize,
    pub score: i32,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct ContextCandidateReport {
    pub file: PathBuf,
    pub start_line: usize,
    pub kind: ContextArtifactKind,
    pub reason: String,
    pub estimated_tokens: usize,
    pub score: i32,
    pub selected: bool,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct ContextReport {
    pub enabled: bool,
    pub budget_tokens: usize,
    pub used_tokens: usize,
    pub primary_tokens: usize,
    pub artifact_tokens: usize,
    pub indexed_files: usize,
    pub candidate_count: usize,
    pub selected_count: usize,
    pub primary_truncated: bool,
    pub cache_hits: usize,
    pub cache_misses: usize,
    #[serde(default)]
    pub candidates: Vec<ContextCandidateReport>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ContextBundle {
    pub cwd: PathBuf,
    pub file: PathBuf,
    pub cursor: Cursor,
    pub selection: Option<Selection>,
    pub buffer_text: String,
    #[serde(default = "one")]
    pub buffer_start_line: usize,
    pub diagnostics: Vec<Diagnostic>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hints: Vec<ContextHint>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifacts: Vec<ContextArtifact>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub report: Option<ContextReport>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct StartSessionParams {
    pub cwd: PathBuf,
    pub file: PathBuf,
    pub cursor: Cursor,
    pub selection: Option<Selection>,
    pub prompt: String,
    pub mode: Mode,
    pub buffer_text: String,
    #[serde(default = "one")]
    pub buffer_start_line: usize,
    pub diagnostics: Vec<Diagnostic>,
    #[serde(default)]
    pub hints: Vec<ContextHint>,
    #[serde(default)]
    pub context_policy: ContextPolicy,
}

impl ContextBundle {
    pub fn from_start(params: StartSessionParams) -> Self {
        Self {
            cwd: params.cwd,
            file: params.file,
            cursor: params.cursor,
            selection: params.selection,
            buffer_text: params.buffer_text,
            buffer_start_line: params.buffer_start_line,
            diagnostics: params.diagnostics,
            hints: params.hints,
            artifacts: vec![],
            report: None,
        }
    }
}

fn one() -> usize {
    1
}
