use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{Action, Card, ContextBundle, Mode};

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct TokenUsage {
    pub input_tokens: usize,
    #[serde(default)]
    pub cached_input_tokens: usize,
    pub output_tokens: usize,
    pub total_tokens: usize,
    pub estimated: bool,
}

impl TokenUsage {
    pub fn estimated(input: usize, output: usize) -> Self {
        Self {
            input_tokens: input,
            cached_input_tokens: 0,
            output_tokens: output,
            total_tokens: input + output,
            estimated: true,
        }
    }

    pub fn reported(input: usize, output: usize) -> Self {
        Self {
            input_tokens: input,
            cached_input_tokens: 0,
            output_tokens: output,
            total_tokens: input + output,
            estimated: false,
        }
    }

    pub fn add(&mut self, other: &Self) {
        self.input_tokens += other.input_tokens;
        self.cached_input_tokens += other.cached_input_tokens;
        self.output_tokens += other.output_tokens;
        self.total_tokens += other.total_tokens;
        self.estimated = self.estimated || other.estimated;
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: String,
    pub id: Value,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: String,
    pub id: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

impl JsonRpcResponse {
    pub fn ok(id: Value, result: Value) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            id,
            result: Some(result),
            error: None,
        }
    }

    pub fn err(id: Value, code: i64, message: impl Into<String>) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            id,
            result: None,
            error: Some(JsonRpcError {
                code,
                message: message.into(),
            }),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct JsonRpcError {
    pub code: i64,
    pub message: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct JsonRpcNotification {
    pub jsonrpc: String,
    pub method: String,
    pub params: Value,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct StartSessionResult {
    pub session_id: String,
    pub card: Card,
    pub goal: GoalProgress,
    pub token_usage: TokenUsage,
    pub turn_token_usage: TokenUsage,
    pub context_report: Option<crate::ContextReport>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default)]
    pub attempts: Vec<AgentAttempt>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ActionParams {
    pub session_id: String,
    pub action: Action,
    #[serde(default)]
    pub context: Option<ContextBundle>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ReplyParams {
    pub session_id: String,
    pub text: String,
    /// Required because every PromptWindow owns an explicit user-selected mode.
    pub mode: Mode,
    #[serde(default)]
    pub context: Option<ContextBundle>,
    #[serde(default)]
    pub skills: Vec<crate::InstructionSkill>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ActionResult {
    pub session_id: String,
    pub card: Card,
    pub goal: GoalProgress,
    pub token_usage: TokenUsage,
    pub turn_token_usage: TokenUsage,
    pub context_report: Option<crate::ContextReport>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default)]
    pub attempts: Vec<AgentAttempt>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct AgentAttempt {
    pub number: usize,
    pub backend: String,
    pub outcome: String,
    pub token_usage: TokenUsage,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// Why the attempt violated the card contract, as a closed telemetry
    /// class. Present on contract-retry/rejected attempts; never carries
    /// buffer or card content, so trace redaction passes it through.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub violation_class: Option<ViolationClass>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub candidate_card: Option<Card>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub activities: Vec<String>,
}

/// A closed classification of contract violations, used to aggregate which
/// model mistakes still cost a retry turn. One class per distinct error
/// construction site; `Other` only covers errors nothing has classified yet.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ViolationClass {
    /// Context/remove lines were not found (or ambiguous) in the buffer.
    ContextMismatch,
    /// `@@` header coordinates or counts are inconsistent with the hunk body.
    HunkHeaderMismatch,
    /// The diff text is not a parseable unified diff.
    MalformedDiff,
    /// The patch targets a different file than the accepted source location.
    WrongFile,
    /// The card exceeds the file/hunk/changed-line limits.
    MultiHunk,
    /// A required card field is empty or structurally invalid.
    MissingField,
    /// The card kind is not allowed in the current session state.
    KindMismatch,
    /// The card repeats an observation or an already accepted patch step.
    DuplicateStep,
    /// A goal batch could not be validated coherently across its files.
    IncoherentBatch,
    /// A violation no construction site has classified.
    Other,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct GoalProgress {
    pub statement: String,
    pub completed_steps: Vec<String>,
    pub known_observations: Vec<ObservationProgress>,
    pub status: GoalStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_step: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GoalStatus {
    #[default]
    Idle,
    Active,
    Paused,
    NeedsReview,
    Complete,
    Stopped,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ObservationKind {
    Hypothesis,
    Finding,
    Signal,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ObservationProgress {
    pub id: String,
    pub kind: ObservationKind,
    pub label: String,
    pub occurrences: usize,
    pub active: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct BackendInfo {
    pub name: String,
    pub streaming: bool,
    pub patches: bool,
    pub reasoning: bool,
    pub can_read_project: bool,
    pub can_use_tools: bool,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn response_skips_empty_error() {
        let response = JsonRpcResponse::ok(json!(1), json!({"ok": true}));
        let json = serde_json::to_value(response).unwrap();

        assert!(json.get("error").is_none());
    }

    #[test]
    fn attempt_serializes_violation_class_as_snake_case() {
        let attempt = AgentAttempt {
            number: 2,
            backend: "mock".into(),
            outcome: "contract_retry".into(),
            violation_class: Some(ViolationClass::ContextMismatch),
            ..AgentAttempt::default()
        };
        let json = serde_json::to_value(&attempt).unwrap();

        assert_eq!(json["violation_class"], json!("context_mismatch"));

        let parsed: AgentAttempt = serde_json::from_value(json).unwrap();
        assert_eq!(
            parsed.violation_class,
            Some(ViolationClass::ContextMismatch)
        );
    }

    #[test]
    fn attempt_without_violation_class_omits_the_field_and_still_parses() {
        let attempt = AgentAttempt {
            number: 1,
            backend: "mock".into(),
            outcome: "accepted".into(),
            ..AgentAttempt::default()
        };
        let json = serde_json::to_value(&attempt).unwrap();

        assert!(json.get("violation_class").is_none());

        // Older traces without the field must keep deserializing.
        let parsed: AgentAttempt = serde_json::from_value(json).unwrap();
        assert_eq!(parsed.violation_class, None);
    }
}
