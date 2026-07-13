use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::patch::{FilePatch, PatchId};

pub type CardId = String;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CardKind {
    Hypothesis,
    Finding,
    Patch,
    Choice,
    Deny,
    Summary,
    Error,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    Follow,
    Why,
    Fix,
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
    Choice(ChoiceCard),
    Deny(DenyCard),
    Summary(SummaryCard),
    Error(ErrorCard),
}

impl Card {
    pub fn kind(&self) -> CardKind {
        match self {
            Card::Hypothesis(_) => CardKind::Hypothesis,
            Card::Finding(_) => CardKind::Finding,
            Card::Patch(_) => CardKind::Patch,
            Card::Choice(_) => CardKind::Choice,
            Card::Deny(_) => CardKind::Deny,
            Card::Summary(_) => CardKind::Summary,
            Card::Error(_) => CardKind::Error,
        }
    }

    pub fn id(&self) -> &str {
        match self {
            Card::Hypothesis(card) => &card.id,
            Card::Finding(card) => &card.id,
            Card::Patch(card) => &card.id,
            Card::Choice(card) => &card.id,
            Card::Deny(card) => &card.id,
            Card::Summary(card) => &card.id,
            Card::Error(card) => &card.id,
        }
    }

    pub fn actions(&self) -> &[Action] {
        match self {
            Card::Hypothesis(card) => &card.actions,
            Card::Finding(card) => &card.actions,
            Card::Patch(card) => &card.actions,
            Card::Choice(_) => &[],
            Card::Deny(card) => &card.actions,
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
    pub actions: Vec<Action>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FindingCard {
    pub id: CardId,
    pub title: String,
    pub finding: String,
    pub location: Option<Location>,
    pub annotation: Option<String>,
    pub actions: Vec<Action>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PatchCard {
    pub id: CardId,
    pub title: String,
    pub explanation: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
    pub patches: Vec<FilePatch>,
    pub actions: Vec<Action>,
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
    pub actions: Vec<Action>,
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
            actions: vec![Action::Follow, Action::Fix, Action::Stop],
        });

        let json = serde_json::to_value(card).unwrap();

        assert_eq!(json["kind"], "hypothesis");
        assert_eq!(json["actions"][0], "follow");
    }
}
