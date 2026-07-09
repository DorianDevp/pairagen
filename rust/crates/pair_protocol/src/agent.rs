use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::{
    Action, Card, ChoiceCard, ChoiceOption, ErrorCard, FilePatch, FindingCard, HypothesisCard,
    Location, LocationEvidence, NextMove, PatchCard, SummaryCard,
};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum AgentOp {
    Hypothesis {
        title: String,
        claim: String,
        evidence: Option<AgentLocation>,
        next: Option<AgentLocation>,
    },
    Finding {
        title: String,
        finding: String,
        location: Option<AgentLocation>,
        annotation: Option<String>,
    },
    Patch {
        title: String,
        explanation: String,
        patches: Vec<AgentPatch>,
    },
    Choice {
        title: String,
        question: String,
        options: Vec<AgentChoice>,
    },
    Summary {
        title: String,
        summary: String,
        changed_files: Vec<PathBuf>,
    },
    Error {
        title: String,
        message: String,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AgentLocation {
    pub file: PathBuf,
    pub line: usize,
    #[serde(default = "one")]
    pub column: usize,
    #[serde(default)]
    pub annotation: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AgentPatch {
    #[serde(default)]
    pub id: Option<String>,
    pub file: PathBuf,
    pub diff: String,
    pub explanation: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AgentChoice {
    pub id: String,
    pub label: String,
    pub action: Action,
}

impl AgentOp {
    pub fn into_card(self, id: impl Into<String>) -> Card {
        let id = id.into();

        match self {
            Self::Hypothesis {
                title,
                claim,
                evidence,
                next,
            } => Card::Hypothesis(HypothesisCard {
                id,
                title,
                claim,
                evidence: evidence.map(AgentLocation::evidence),
                next_move: next.map(|location| NextMove::OpenLocation(location.evidence())),
                actions: vec![
                    Action::Follow,
                    Action::Why,
                    Action::Fix,
                    Action::OtherLead,
                    Action::Stop,
                ],
            }),
            Self::Finding {
                title,
                finding,
                location,
                annotation,
            } => Card::Finding(FindingCard {
                id,
                title,
                finding,
                location: location.map(AgentLocation::location),
                annotation,
                actions: vec![
                    Action::Open,
                    Action::Why,
                    Action::Fix,
                    Action::OtherLead,
                    Action::Stop,
                ],
            }),
            Self::Patch {
                title,
                explanation,
                patches,
            } => Card::Patch(PatchCard {
                id,
                title,
                explanation,
                patches: patches
                    .into_iter()
                    .enumerate()
                    .map(|(index, patch)| patch.file_patch(index + 1))
                    .collect(),
                actions: vec![
                    Action::Apply,
                    Action::Retry,
                    Action::EditPrompt,
                    Action::Stop,
                ],
            }),
            Self::Choice {
                title,
                question,
                options,
            } => Card::Choice(ChoiceCard {
                id,
                title,
                question,
                options: options.into_iter().map(AgentChoice::choice).collect(),
            }),
            Self::Summary {
                title,
                summary,
                changed_files,
            } => Card::Summary(SummaryCard {
                id,
                title,
                summary,
                changed_files,
                next_actions: vec![Action::Next, Action::RunCheck, Action::Stop],
            }),
            Self::Error { title, message } => Card::Error(ErrorCard {
                id,
                title,
                message,
                actions: vec![Action::Retry, Action::EditPrompt, Action::Stop],
            }),
        }
    }
}

impl AgentLocation {
    fn location(self) -> Location {
        Location {
            file: self.file,
            line: self.line,
            column: self.column,
        }
    }

    fn evidence(self) -> LocationEvidence {
        LocationEvidence {
            file: self.file,
            line: self.line,
            column: self.column,
            annotation: self.annotation.unwrap_or_default(),
        }
    }
}

impl AgentPatch {
    fn file_patch(self, index: usize) -> FilePatch {
        FilePatch {
            id: self.id.unwrap_or_else(|| format!("p_{index}")),
            file: self.file,
            diff: self.diff,
            explanation: self.explanation,
        }
    }
}

impl AgentChoice {
    fn choice(self) -> ChoiceOption {
        ChoiceOption {
            id: self.id,
            label: self.label,
            action: self.action,
        }
    }
}

fn one() -> usize {
    1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_op_to_card() {
        let op = AgentOp::Hypothesis {
            title: "Maybe skipped".into(),
            claim: "The branch may return early.".into(),
            evidence: None,
            next: None,
        };
        let card = op.into_card("c_1");

        assert!(matches!(card, Card::Hypothesis(_)));
    }
}
