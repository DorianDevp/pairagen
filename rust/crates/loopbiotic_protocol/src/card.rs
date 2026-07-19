use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::patch::{FilePatch, PatchId};

pub type CardId = String;

fn is_false(value: &bool) -> bool {
    !*value
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CardKind {
    Hypothesis,
    Finding,
    Patch,
    Working,
    Choice,
    Deny,
    OpenLocation,
    Summary,
    Error,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    Follow,
    Why,
    ResumeDraft,
    Fix,
    Goal,
    CancelTurn,
    OtherLead,
    Apply,
    ApplyPatch { patch_id: PatchId },
    Retry,
    EditPrompt,
    Open,
    RunCheck,
    Next,
    Stop,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Card {
    Hypothesis(HypothesisCard),
    Finding(FindingCard),
    Patch(PatchCard),
    Working(WorkingCard),
    Choice(ChoiceCard),
    Deny(DenyCard),
    OpenLocation(OpenLocationCard),
    Summary(SummaryCard),
    Error(ErrorCard),
}

impl Card {
    pub fn kind(&self) -> CardKind {
        match self {
            Card::Hypothesis(_) => CardKind::Hypothesis,
            Card::Finding(_) => CardKind::Finding,
            Card::Patch(_) => CardKind::Patch,
            Card::Working(_) => CardKind::Working,
            Card::Choice(_) => CardKind::Choice,
            Card::Deny(_) => CardKind::Deny,
            Card::OpenLocation(_) => CardKind::OpenLocation,
            Card::Summary(_) => CardKind::Summary,
            Card::Error(_) => CardKind::Error,
        }
    }

    pub fn id(&self) -> &str {
        match self {
            Card::Hypothesis(card) => &card.id,
            Card::Finding(card) => &card.id,
            Card::Patch(card) => &card.id,
            Card::Working(card) => &card.id,
            Card::Choice(card) => &card.id,
            Card::Deny(card) => &card.id,
            Card::OpenLocation(card) => &card.id,
            Card::Summary(card) => &card.id,
            Card::Error(card) => &card.id,
        }
    }

    pub fn actions(&self) -> &[Action] {
        match self {
            Card::Hypothesis(card) => &card.actions,
            Card::Finding(card) => &card.actions,
            Card::Patch(card) => &card.actions,
            Card::Working(card) => &card.actions,
            Card::Choice(_) => &[],
            Card::Deny(card) => &card.actions,
            Card::OpenLocation(_) => &[],
            Card::Summary(card) => &card.next_actions,
            Card::Error(card) => &card.actions,
        }
    }

    pub fn location_move(&self) -> Option<&NextMove> {
        match self {
            Card::Hypothesis(card) => card.next_move.as_ref(),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct HypothesisCard {
    pub id: CardId,
    pub title: String,
    pub claim: String,
    pub evidence: Option<LocationEvidence>,
    pub next_move: Option<NextMove>,
    /// Ordered node ids from the editor-supplied static Flow graph.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub flow_path: Vec<String>,
    pub actions: Vec<Action>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FindingCard {
    pub id: CardId,
    pub title: String,
    pub finding: String,
    pub location: Option<Location>,
    pub annotation: Option<String>,
    /// Ordered node ids from the editor-supplied static Flow graph.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub flow_path: Vec<String>,
    pub actions: Vec<Action>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PatchCard {
    pub id: CardId,
    pub title: String,
    pub explanation: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
    /// This one reviewed hunk finishes the current explicit goal.
    #[serde(default, skip_serializing_if = "is_false")]
    pub goal_complete: bool,
    /// Explicit goal turns return one hunk plus a plan of coherent steps that
    /// remain. A later step may target the same file again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<GoalPlan>,
    pub patches: Vec<FilePatch>,
    pub actions: Vec<Action>,
}

/// A backend turn exceeded its interaction deadline but is still running.
/// The card keeps the editor interactive while the final result is produced
/// in the background.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct WorkingCard {
    pub id: CardId,
    pub turn_id: String,
    pub title: String,
    pub phase: String,
    pub message: String,
    pub elapsed_ms: u64,
    pub deadline_ms: u64,
    pub actions: Vec<Action>,
}

/// The remainder of an explicit goal: coherent steps still to review and
/// whether the current hunk is the final one.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct GoalPlan {
    #[serde(default)]
    pub remaining: Vec<PlannedStep>,
    /// True when the goal has no further slices after the current one.
    pub complete: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PlannedStep {
    pub file: String,
    pub summary: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ChoiceCard {
    pub id: CardId,
    pub title: String,
    pub question: String,
    pub options: Vec<ChoiceOption>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ChoiceOption {
    pub id: String,
    pub label: String,
    pub action: Action,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DenyCard {
    pub id: CardId,
    pub title: String,
    pub reason: String,
    // Where the agent needs the editor to be before it can proceed; the
    // editor offers to jump there and retry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<Location>,
    pub actions: Vec<Action>,
}

/// A mid-turn permission request: the agent can only proceed once the editor
/// has this location open. The harness intercepts it, asks the user, and
/// resumes the same turn with fresh context — it is never shown as a card.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct OpenLocationCard {
    pub id: CardId,
    pub reason: String,
    pub location: Location,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SummaryCard {
    pub id: CardId,
    pub title: String,
    pub summary: String,
    pub changed_files: Vec<PathBuf>,
    pub next_actions: Vec<Action>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ErrorCard {
    pub id: CardId,
    pub title: String,
    pub message: String,
    pub actions: Vec<Action>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Location {
    pub file: PathBuf,
    pub line: usize,
    pub column: usize,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LocationEvidence {
    pub file: PathBuf,
    pub line: usize,
    pub column: usize,
    pub annotation: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum NextMove {
    OpenLocation(LocationEvidence),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serializes_one_card() {
        let card = Card::Hypothesis(HypothesisCard {
            id: "c_1".into(),
            title: "Payload may be skipped".into(),
            claim: "The branch returns early.".into(),
            evidence: None,
            next_move: None,
            flow_path: vec![],
            actions: vec![Action::Follow, Action::Fix, Action::Stop],
        });

        let json = serde_json::to_value(card).unwrap();

        assert_eq!(json["kind"], "hypothesis");
        assert_eq!(json["actions"][0], "follow");
        assert!(json.get("flow_path").is_none());
    }

    #[test]
    fn deserializes_legacy_finding_without_flow_path() {
        let card: Card = serde_json::from_value(serde_json::json!({
            "kind": "finding",
            "id": "c_legacy",
            "title": "Legacy",
            "finding": "Still valid",
            "location": null,
            "annotation": null,
            "actions": ["stop"]
        }))
        .unwrap();

        let Card::Finding(card) = card else {
            panic!("expected finding");
        };
        assert!(card.flow_path.is_empty());
    }

    #[test]
    fn serializes_selected_flow_path_on_answer_cards() {
        let card = Card::Finding(FindingCard {
            id: "c_flow".into(),
            title: "Call path".into(),
            finding: "The command opens the prompt window.".into(),
            location: None,
            annotation: None,
            flow_path: vec!["command".into(), "prompt.open".into()],
            actions: vec![Action::Stop],
        });

        let json = serde_json::to_value(card).unwrap();
        assert_eq!(
            json["flow_path"],
            serde_json::json!(["command", "prompt.open"])
        );
    }

    fn patch_card(plan: Option<GoalPlan>) -> Card {
        Card::Patch(PatchCard {
            id: "c_p".into(),
            title: "Slice".into(),
            explanation: "One file.".into(),
            warnings: vec![],
            goal_complete: false,
            plan,
            patches: vec![],
            actions: vec![Action::Apply],
        })
    }

    #[test]
    fn serializes_goal_plan_on_sliced_patch_cards() {
        let card = patch_card(Some(GoalPlan {
            remaining: vec![PlannedStep {
                file: "src/caller.ts".into(),
                summary: "Update the consumer.".into(),
            }],
            complete: false,
        }));

        let json = serde_json::to_value(card.clone()).unwrap();

        assert_eq!(json["plan"]["complete"], false);
        assert_eq!(json["plan"]["remaining"][0]["file"], "src/caller.ts");
        assert_eq!(
            json["plan"]["remaining"][0]["summary"],
            "Update the consumer."
        );
        let round_trip: Card = serde_json::from_value(json).unwrap();
        assert_eq!(round_trip, card);
    }

    #[test]
    fn omits_absent_plan_and_accepts_planless_patch_cards() {
        let json = serde_json::to_value(patch_card(None)).unwrap();
        assert!(json.get("plan").is_none());

        // Cards serialized before the plan field existed must still parse.
        let legacy = serde_json::json!({
            "kind": "patch",
            "id": "c_p",
            "title": "Slice",
            "explanation": "One file.",
            "patches": [],
            "actions": ["apply"],
        });
        let card: Card = serde_json::from_value(legacy).unwrap();
        let Card::Patch(card) = card else {
            panic!("expected patch card");
        };
        assert_eq!(card.plan, None);
        assert!(!card.goal_complete);
    }

    #[test]
    fn goal_plan_remaining_defaults_to_empty() {
        let plan: GoalPlan = serde_json::from_value(serde_json::json!({"complete": true})).unwrap();

        assert!(plan.complete);
        assert!(plan.remaining.is_empty());
    }
}
