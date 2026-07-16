//! Hand-built JSON output schemas handed to the app-server so the model's
//! final answer is constrained to the card kind the turn demands.

use serde_json::{Value, json};

use crate::BackendRequest;

pub(super) fn output_schema(req: &BackendRequest) -> Value {
    if req.card_contract.allow_goal_completion
        && req.card_contract.expected_kind == Some(loopbiotic_protocol::CardKind::Finding)
    {
        return finding_schema();
    }
    if req.card_contract.allow_goal_completion {
        return goal_loop_schema(&req.card_contract);
    }

    match req.card_contract.expected_kind {
        Some(loopbiotic_protocol::CardKind::Patch) => patch_schema(&req.card_contract),
        Some(loopbiotic_protocol::CardKind::Hypothesis) => hypothesis_schema(),
        Some(loopbiotic_protocol::CardKind::Finding) => finding_schema(),
        Some(loopbiotic_protocol::CardKind::Choice) => choice_schema(),
        Some(loopbiotic_protocol::CardKind::Deny) => deny_schema(),
        Some(loopbiotic_protocol::CardKind::Summary) => summary_schema(),
        Some(loopbiotic_protocol::CardKind::Error) => error_schema(),
        Some(loopbiotic_protocol::CardKind::OpenLocation) | None => any_op_schema(),
    }
}

/// Schema for turns without a demanded kind: the agent picks whichever op
/// fits, including a clarifying choice or a deny. Mirrors
/// schemas/loopbiotic-agent-op.schema.json (every field present, unused ones null).
fn any_op_schema() -> Value {
    object_schema(
        &[
            "op",
            "title",
            "claim",
            "evidence",
            "next",
            "finding",
            "location",
            "annotation",
            "explanation",
            "goal_complete",
            "patches",
            "question",
            "options",
            "reason",
            "summary",
            "changed_files",
            "message",
        ],
        json!({
            "op": {"type": "string", "enum": ["hypothesis", "finding", "patch", "choice", "deny", "open_location", "summary", "error"]},
            "title": {"type": "string"},
            "claim": {"type": ["string", "null"]},
            "evidence": nullable_location_schema(),
            "next": nullable_location_schema(),
            "finding": {"type": ["string", "null"]},
            "location": nullable_location_schema(),
            "annotation": {"type": ["string", "null"]},
            "explanation": {"type": ["string", "null"]},
            "goal_complete": {"type": ["boolean", "null"]},
            "patches": {
                "type": ["array", "null"],
                "items": object_schema(
                    &["id", "file", "diff", "explanation"],
                    json!({
                        "id": {"type": ["string", "null"]},
                        "file": {"type": "string"},
                        "diff": {"type": "string"},
                        "explanation": {"type": "string"}
                    })
                )
            },
            "question": {"type": ["string", "null"]},
            "options": {
                "type": ["array", "null"],
                "items": object_schema(
                    &["id", "label", "action"],
                    json!({
                        "id": {"type": "string"},
                        "label": {"type": "string"},
                        "action": {
                            "type": "string",
                            "enum": ["follow", "why", "fix", "other_lead", "retry", "edit_prompt", "open", "run_check", "next", "stop"]
                        }
                    })
                )
            },
            "reason": {"type": ["string", "null"]},
            "summary": {"type": ["string", "null"]},
            "changed_files": {"type": ["array", "null"], "items": {"type": "string"}},
            "message": {"type": ["string", "null"]}
        }),
    )
}

fn goal_loop_schema(contract: &crate::CardContract) -> Value {
    let mut schema = any_op_schema();
    schema["properties"]["op"]["enum"] = json!([
        "patch",
        "choice",
        "deny",
        "open_location",
        "summary",
        "error"
    ]);
    let mut patches = patch_schema(contract)["properties"]["patches"].clone();
    patches["type"] = json!(["array", "null"]);
    schema["properties"]["patches"] = patches;
    schema
}

fn object_schema(required: &[&str], properties: Value) -> Value {
    json!({
        "type": "object",
        "required": required,
        "properties": properties,
        "additionalProperties": false
    })
}

fn nullable_location_schema() -> Value {
    json!({
        "anyOf": [
            object_schema(
                &["file", "line", "column", "annotation"],
                json!({
                    "file": {"type": "string"},
                    "line": {"type": "integer"},
                    "column": {"type": "integer"},
                    "annotation": {"type": ["string", "null"]}
                })
            ),
            {"type": "null"}
        ]
    })
}

fn location_schema() -> Value {
    object_schema(
        &["file", "line", "column", "annotation"],
        json!({
            "file": {"type": "string"},
            "line": {"type": "integer", "minimum": 1},
            "column": {"type": "integer", "minimum": 1},
            "annotation": {"type": ["string", "null"]}
        }),
    )
}

fn hypothesis_schema() -> Value {
    object_schema(
        &["op", "title", "claim", "evidence", "next"],
        json!({
            "op": {"type": "string", "enum": ["hypothesis"]},
            "title": {"type": "string"},
            "claim": {"type": "string"},
            "evidence": nullable_location_schema(),
            "next": location_schema()
        }),
    )
}

fn finding_schema() -> Value {
    object_schema(
        &["op", "title", "finding", "location", "annotation"],
        json!({
            "op": {"type": "string", "enum": ["finding"]},
            "title": {"type": "string"},
            "finding": {"type": "string"},
            "location": location_schema(),
            "annotation": {"type": ["string", "null"]}
        }),
    )
}

fn patch_schema(contract: &crate::CardContract) -> Value {
    object_schema(
        &["op", "title", "explanation", "goal_complete", "patches"],
        json!({
            "op": {"type": "string", "enum": ["patch"]},
            "title": {"type": "string"},
            "explanation": {"type": "string"},
            "goal_complete": {"type": "boolean"},
            "patches": {
                "type": "array",
                "minItems": 1,
                "maxItems": contract.max_patch_files,
                "items": object_schema(
                    &["id", "file", "explanation", "hunks"],
                    json!({
                        "id": {"type": ["string", "null"]},
                        "file": {"type": "string"},
                        "explanation": {"type": "string"},
                        "hunks": {
                            "type": "array",
                            "minItems": 1,
                            "maxItems": contract.max_hunks_per_patch,
                            "items": object_schema(
                                &["old_start", "new_start", "lines"],
                                json!({
                                    "old_start": {"type": "integer", "minimum": 1},
                                    "new_start": {"type": "integer", "minimum": 1},
                                    "lines": {
                                        "type": "array",
                                        "minItems": 1,
                                        "maxItems": contract.max_changed_lines + 8,
                                        "items": object_schema(
                                            &["kind", "text"],
                                            json!({
                                                "kind": {"type": "string", "enum": ["context", "remove", "add"]},
                                                "text": {"type": "string"}
                                            })
                                        )
                                    }
                                })
                            )
                        }
                    })
                )
            }
        }),
    )
}

fn choice_schema() -> Value {
    object_schema(
        &["op", "title", "question", "options"],
        json!({
            "op": {"type": "string", "enum": ["choice"]},
            "title": {"type": "string"},
            "question": {"type": "string"},
            "options": {
                "type": "array",
                "items": object_schema(
                    &["id", "label", "action"],
                    json!({
                        "id": {"type": "string"},
                        "label": {"type": "string"},
                        "action": {
                            "type": "string",
                            "enum": ["follow", "why", "fix", "other_lead", "retry", "edit_prompt", "open", "run_check", "next", "stop"]
                        }
                    })
                )
            }
        }),
    )
}

fn summary_schema() -> Value {
    object_schema(
        &["op", "title", "summary", "changed_files"],
        json!({
            "op": {"type": "string", "enum": ["summary"]},
            "title": {"type": "string"},
            "summary": {"type": "string"},
            "changed_files": {"type": "array", "items": {"type": "string"}}
        }),
    )
}

fn deny_schema() -> Value {
    object_schema(
        &["op", "title", "reason", "location"],
        json!({
            "op": {"type": "string", "enum": ["deny"]},
            "title": {"type": "string"},
            "reason": {"type": "string"},
            "location": nullable_location_schema()
        }),
    )
}

fn error_schema() -> Value {
    object_schema(
        &["op", "title", "message"],
        json!({
            "op": {"type": "string", "enum": ["error"]},
            "title": {"type": "string"},
            "message": {"type": "string"}
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patch_schema_exposes_hunks_instead_of_raw_diff() {
        let schema = patch_schema(&crate::CardContract::default());
        let patch = &schema["properties"]["patches"]["items"];

        assert!(patch["properties"].get("diff").is_none());
        assert_eq!(patch["properties"]["hunks"]["type"], "array");
        assert_eq!(schema["properties"]["patches"]["maxItems"], 1);
        assert_eq!(patch["properties"]["hunks"]["maxItems"], 1);
    }

    #[test]
    fn goal_loop_schema_allows_structured_patch_or_summary() {
        let contract = crate::CardContract {
            max_patch_files: loopbiotic_protocol::MAX_GOAL_PATCH_FILES,
            max_hunks_per_patch: loopbiotic_protocol::MAX_GOAL_HUNKS_PER_PATCH,
            max_changed_lines: loopbiotic_protocol::MAX_GOAL_CHANGED_LINES,
            ..Default::default()
        };
        let schema = goal_loop_schema(&contract);
        let ops = schema["properties"]["op"]["enum"].as_array().unwrap();

        assert!(ops.contains(&json!("patch")));
        assert!(ops.contains(&json!("summary")));
        assert!(!ops.contains(&json!("finding")));
        assert_eq!(
            schema["properties"]["patches"]["maxItems"],
            loopbiotic_protocol::MAX_GOAL_PATCH_FILES
        );
        assert!(schema["properties"]["patches"]["items"]["properties"]["hunks"].is_object());
        assert_eq!(
            schema["properties"]["patches"]["items"]["properties"]["hunks"]["maxItems"],
            loopbiotic_protocol::MAX_GOAL_HUNKS_PER_PATCH
        );
        assert!(schema["properties"]["goal_complete"].is_object());
    }

    #[test]
    fn discovery_schema_requires_a_concrete_next_location() {
        let schema = hypothesis_schema();

        assert_eq!(schema["properties"]["next"]["type"], "object");
        assert_eq!(
            schema["properties"]["next"]["properties"]["line"]["minimum"],
            1
        );
    }
}
