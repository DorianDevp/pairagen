use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    Auto,
    Investigate,
    Fix,
    Explain,
    Propose,
    Review,
}

impl Default for Mode {
    fn default() -> Self {
        Self::Auto
    }
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
pub struct ContextBundle {
    pub cwd: PathBuf,
    pub file: PathBuf,
    pub cursor: Cursor,
    pub selection: Option<Selection>,
    pub buffer_text: String,
    pub diagnostics: Vec<Diagnostic>,
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
    pub diagnostics: Vec<Diagnostic>,
}

impl ContextBundle {
    pub fn from_start(params: StartSessionParams) -> Self {
        Self {
            cwd: params.cwd,
            file: params.file,
            cursor: params.cursor,
            selection: params.selection,
            buffer_text: params.buffer_text,
            diagnostics: params.diagnostics,
        }
    }
}
