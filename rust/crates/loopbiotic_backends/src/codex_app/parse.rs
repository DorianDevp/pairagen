//! Parsing of the model's final answer into Loopbiotic cards, including the
//! structured-hunk patch format the output schema demands.

use anyhow::{Result, anyhow};
use loopbiotic_protocol::{Action, AgentOp, Card};
use serde::Deserialize;
use serde_json::Value;

#[derive(Deserialize)]
struct StructuredPatchOp {
    op: String,
    title: String,
    explanation: Option<String>,
    #[serde(default)]
    goal_complete: Option<bool>,
    #[serde(default)]
    plan: Option<loopbiotic_protocol::GoalPlan>,
    patches: Vec<StructuredFilePatch>,
}

#[derive(Deserialize)]
struct StructuredFilePatch {
    id: Option<String>,
    file: std::path::PathBuf,
    explanation: String,
    hunks: Vec<StructuredHunk>,
}

#[derive(Deserialize)]
struct StructuredHunk {
    old_start: usize,
    new_start: usize,
    lines: Vec<StructuredLine>,
}

#[derive(Deserialize)]
struct StructuredLine {
    kind: StructuredLineKind,
    text: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum StructuredLineKind {
    Context,
    Remove,
    Add,
}

pub(super) fn parse_card(output: &str, _contract: &crate::CardContract) -> Result<Card> {
    let value = serde_json::from_str::<Value>(output.trim())?;
    if value.get("op").and_then(Value::as_str) == Some("patch") {
        return parse_structured_patch(output);
    }

    let op = serde_json::from_str::<AgentOp>(output.trim())?;

    Ok(op.into_card("c_agent"))
}

fn parse_structured_patch(output: &str) -> Result<Card> {
    let op = serde_json::from_str::<StructuredPatchOp>(output.trim())?;
    if op.op != "patch" {
        return Err(anyhow!("codex returned op {:?}, expected patch", op.op));
    }

    let explanation = op
        .explanation
        .filter(|explanation| !explanation.trim().is_empty())
        .or_else(|| op.patches.first().map(|patch| patch.explanation.clone()))
        .unwrap_or_else(|| op.title.clone());
    let patches = op
        .patches
        .into_iter()
        .enumerate()
        .map(|(index, patch)| {
            Ok(loopbiotic_protocol::FilePatch {
                id: patch.id.unwrap_or_else(|| format!("p_{}", index + 1)),
                file: patch.file,
                diff: render_structured_diff(&patch.hunks)?,
                explanation: patch.explanation,
            })
        })
        .collect::<Result<Vec<_>>>()?;

    Ok(Card::Patch(loopbiotic_protocol::PatchCard {
        id: "c_agent".into(),
        title: op.title,
        explanation,
        warnings: vec![],
        goal_complete: op.goal_complete.unwrap_or(false),
        plan: op.plan,
        patches,
        actions: vec![
            Action::Apply,
            Action::Why,
            Action::Retry,
            Action::EditPrompt,
            Action::Stop,
        ],
    }))
}

fn render_structured_diff(hunks: &[StructuredHunk]) -> Result<String> {
    let mut diff = String::new();

    for hunk in hunks {
        let old_len = hunk
            .lines
            .iter()
            .filter(|line| !matches!(line.kind, StructuredLineKind::Add))
            .count();
        let new_len = hunk
            .lines
            .iter()
            .filter(|line| !matches!(line.kind, StructuredLineKind::Remove))
            .count();

        if hunk.old_start == 0 || hunk.new_start == 0 {
            return Err(anyhow!("structured patch line numbers must start at 1"));
        }
        if hunk.lines.is_empty() {
            return Err(anyhow!("structured patch hunk has no lines"));
        }

        diff.push_str(&format!(
            "@@ -{},{} +{},{} @@\n",
            hunk.old_start, old_len, hunk.new_start, new_len
        ));

        for line in &hunk.lines {
            if line.text.contains(['\n', '\r']) {
                return Err(anyhow!("structured patch line contains a newline"));
            }

            let prefix = match line.kind {
                StructuredLineKind::Context => ' ',
                StructuredLineKind::Remove => '-',
                StructuredLineKind::Add => '+',
            };
            diff.push(prefix);
            diff.push_str(&line.text);
            diff.push('\n');
        }
    }

    Ok(diff)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn renders_typed_patch_hunks_as_unified_diff() {
        let output = json!({
            "op": "patch",
            "title": "Rename value",
            "explanation": "Use the new name.",
            "patches": [{
                "id": null,
                "file": "src/main.rs",
                "explanation": "Rename one line.",
                "hunks": [{
                    "old_start": 4,
                    "new_start": 4,
                    "lines": [
                        {"kind": "context", "text": "fn main() {"},
                        {"kind": "remove", "text": "    let old = 1;"},
                        {"kind": "add", "text": "    let new = 1;"},
                        {"kind": "context", "text": "}"}
                    ]
                }]
            }]
        });

        let Card::Patch(card) = parse_structured_patch(&output.to_string()).unwrap() else {
            panic!("expected patch card");
        };

        assert_eq!(
            card.patches[0].diff,
            "@@ -4,3 +4,3 @@\n fn main() {\n-    let old = 1;\n+    let new = 1;\n }\n"
        );
    }

    #[test]
    fn unconstrained_turn_dispatches_patch_and_tolerates_nullable_shared_fields() {
        let output = json!({
            "op": "patch",
            "title": "Add shell wrapper",
            "explanation": null,
            "goal_complete": null,
            "patches": [{
                "id": null,
                "file": "src/work.ts",
                "explanation": "Adds the requested router outlet.",
                "hunks": [{
                    "old_start": 1,
                    "new_start": 1,
                    "lines": [
                        {"kind": "remove", "text": "template: ''"},
                        {"kind": "add", "text": "template: '<router-outlet />'"}
                    ]
                }]
            }],
            "claim": null,
            "evidence": null,
            "next": null,
            "finding": null,
            "location": null,
            "annotation": null,
            "flow_path": null,
            "question": null,
            "options": null,
            "reason": null,
            "summary": null,
            "changed_files": null,
            "message": null
        });

        let Card::Patch(card) =
            parse_card(&output.to_string(), &crate::CardContract::default()).unwrap()
        else {
            panic!("expected unconstrained patch");
        };

        assert_eq!(card.explanation, "Adds the requested router outlet.");
        assert!(!card.goal_complete);
        assert_eq!(card.patches[0].id, "p_1");
    }

    #[test]
    fn goal_loop_dispatches_structured_patch_output() {
        let output = json!({
            "op": "patch",
            "title": "Continue the goal",
            "explanation": "Apply the next accepted requirement.",
            "goal_complete": true,
            "patches": [{
                "id": null,
                "file": "src/main.rs",
                "explanation": "Update the next local block.",
                "hunks": [{
                    "old_start": 1,
                    "new_start": 1,
                    "lines": [
                        {"kind": "remove", "text": "old"},
                        {"kind": "add", "text": "new"}
                    ]
                }]
            }],
            "claim": null,
            "evidence": null,
            "next": null,
            "finding": null,
            "location": null,
            "annotation": null,
            "question": null,
            "options": null,
            "reason": null,
            "summary": null,
            "changed_files": null,
            "message": null
        });
        let contract = crate::CardContract {
            allow_goal_completion: true,
            ..Default::default()
        };

        let Card::Patch(card) = parse_card(&output.to_string(), &contract).unwrap() else {
            panic!("expected patch card");
        };
        assert!(card.goal_complete);
    }

    #[test]
    fn goal_loop_accepts_open_location_target_in_next() {
        let output = json!({
            "op": "open_location",
            "title": "Open inactive-account exception",
            "reason": "Create the exception before referencing it.",
            "location": null,
            "next": {
                "file": "src/Exception/OAuth/OAuthAccountNotActiveException.php",
                "line": 1,
                "column": 1,
                "annotation": "New exception type."
            }
        });
        let contract = crate::CardContract {
            allow_goal_completion: true,
            ..Default::default()
        };

        let Card::OpenLocation(card) = parse_card(&output.to_string(), &contract).unwrap() else {
            panic!("expected open_location card");
        };
        assert!(
            card.location
                .file
                .ends_with("OAuthAccountNotActiveException.php")
        );
    }

    #[test]
    fn strict_parser_rejects_prose_around_json() {
        let output = r#"Here is the result: {"op":"finding","title":"T","finding":"F","location":null,"annotation":null}"#;
        let contract = crate::CardContract {
            expected_kind: Some(loopbiotic_protocol::CardKind::Finding),
            ..crate::CardContract::default()
        };

        assert!(parse_card(output, &contract).is_err());
    }
}
