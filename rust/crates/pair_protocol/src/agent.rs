use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::{
    Action, Card, ChoiceCard, ChoiceOption, DenyCard, ErrorCard, FilePatch, FindingCard,
    HypothesisCard, Location, LocationEvidence, NextMove, PatchCard, SummaryCard,
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
    Deny {
        title: String,
        reason: String,
        #[serde(default)]
        location: Option<AgentLocation>,
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
#[serde(from = "AgentLocationInput")]
pub struct AgentLocation {
    pub file: PathBuf,
    pub line: usize,
    #[serde(default = "one")]
    pub column: usize,
    #[serde(default)]
    pub annotation: Option<String>,
}

// Agents occasionally emit locations as "file:line[:column][ — note]" strings
// instead of the location object; accept both so the op still parses.
#[derive(Deserialize)]
#[serde(untagged)]
enum AgentLocationInput {
    Object {
        file: PathBuf,
        line: usize,
        #[serde(default = "one")]
        column: usize,
        #[serde(default)]
        annotation: Option<String>,
    },
    Text(String),
}

impl From<AgentLocationInput> for AgentLocation {
    fn from(input: AgentLocationInput) -> Self {
        match input {
            AgentLocationInput::Object {
                file,
                line,
                column,
                annotation,
            } => Self {
                file,
                line,
                column,
                annotation,
            },
            AgentLocationInput::Text(text) => parse_location_text(&text),
        }
    }
}

fn parse_location_text(text: &str) -> AgentLocation {
    let (place, annotation) = [" — ", " – ", " - "]
        .iter()
        .find_map(|separator| text.split_once(separator))
        .map(|(place, annotation)| (place.trim(), Some(annotation.trim().to_string())))
        .unwrap_or((text.trim(), None));

    let mut file = place;
    let mut numbers = Vec::new();
    while let Some((head, tail)) = file.rsplit_once(':') {
        let Ok(number) = tail.parse::<usize>() else {
            break;
        };
        if numbers.len() == 2 {
            break;
        }
        numbers.push(number);
        file = head;
    }
    numbers.reverse();

    AgentLocation {
        file: PathBuf::from(file),
        line: numbers.first().copied().unwrap_or(1).max(1),
        column: numbers.get(1).copied().unwrap_or(1).max(1),
        annotation,
    }
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
#[serde(from = "AgentChoiceInput")]
pub struct AgentChoice {
    pub id: String,
    pub label: String,
    pub action: Action,
}

// Agents frequently emit choice options as plain strings instead of the full
// {id,label,action} object; accept both so a valid choice op never fails to parse.
#[derive(Deserialize)]
#[serde(untagged)]
enum AgentChoiceInput {
    Object {
        #[serde(default)]
        id: Option<String>,
        label: String,
        #[serde(default)]
        action: Option<Action>,
    },
    Label(String),
}

impl From<AgentChoiceInput> for AgentChoice {
    fn from(input: AgentChoiceInput) -> Self {
        match input {
            AgentChoiceInput::Object { id, label, action } => Self {
                id: id.unwrap_or_default(),
                label,
                action: action.unwrap_or(Action::EditPrompt),
            },
            AgentChoiceInput::Label(label) => Self {
                id: String::new(),
                label,
                action: Action::EditPrompt,
            },
        }
    }
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
                    Action::Open,
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
                warnings: vec![],
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
                options: options
                    .into_iter()
                    .enumerate()
                    .map(|(index, option)| option.choice(index + 1))
                    .collect(),
            }),
            Self::Deny {
                title,
                reason,
                location,
            } => Card::Deny(DenyCard {
                id,
                title,
                reason,
                location: location.map(AgentLocation::location),
                actions: vec![Action::Retry, Action::EditPrompt, Action::Stop],
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
    fn choice(self, index: usize) -> ChoiceOption {
        ChoiceOption {
            id: if self.id.trim().is_empty() {
                format!("o_{index}")
            } else {
                self.id
            },
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
    fn parses_location_given_as_string() {
        let op: AgentOp = serde_json::from_str(
            r#"{"op":"hypothesis","title":"T","claim":"C","evidence":"src/work.ts:2:5 — the return has no value"}"#,
        )
        .unwrap();

        let AgentOp::Hypothesis { evidence, .. } = op else {
            panic!("expected hypothesis");
        };
        let evidence = evidence.unwrap();
        assert_eq!(evidence.file, PathBuf::from("src/work.ts"));
        assert_eq!(evidence.line, 2);
        assert_eq!(evidence.column, 5);
        assert_eq!(
            evidence.annotation.as_deref(),
            Some("the return has no value")
        );
    }

    #[test]
    fn parses_location_string_without_line() {
        let op: AgentOp = serde_json::from_str(
            r#"{"op":"finding","title":"T","finding":"F","location":"src/work.ts"}"#,
        )
        .unwrap();

        let AgentOp::Finding { location, .. } = op else {
            panic!("expected finding");
        };
        let location = location.unwrap();
        assert_eq!(location.file, PathBuf::from("src/work.ts"));
        assert_eq!(location.line, 1);
    }

    #[test]
    fn parses_choice_options_given_as_plain_strings() {
        let op: AgentOp = serde_json::from_str(
            r#"{"op":"choice","title":"T","question":"Q","options":["First","Second"]}"#,
        )
        .unwrap();
        let card = op.into_card("c_1");

        let Card::Choice(card) = card else {
            panic!("expected choice card");
        };
        assert_eq!(card.options.len(), 2);
        assert_eq!(card.options[0].id, "o_1");
        assert_eq!(card.options[0].label, "First");
        assert_eq!(card.options[1].id, "o_2");
    }

    #[test]
    fn parses_choice_options_missing_id_and_action() {
        let op: AgentOp = serde_json::from_str(
            r#"{"op":"choice","title":"T","question":"Q","options":[{"label":"Only label"},{"id":"custom","label":"Full","action":"fix"}]}"#,
        )
        .unwrap();
        let card = op.into_card("c_1");

        let Card::Choice(card) = card else {
            panic!("expected choice card");
        };
        assert_eq!(card.options[0].id, "o_1");
        assert_eq!(card.options[1].id, "custom");
        assert_eq!(card.options[1].action, Action::Fix);
    }

    #[test]
    fn maps_deny_op_to_deny_card() {
        let op: AgentOp = serde_json::from_str(
            r#"{"op":"deny","title":"Ambiguous prompt","reason":"The prompt does not say what to test."}"#,
        )
        .unwrap();
        let card = op.into_card("c_1");

        let Card::Deny(card) = card else {
            panic!("expected deny card");
        };
        assert_eq!(card.title, "Ambiguous prompt");
        assert_eq!(
            card.actions,
            vec![Action::Retry, Action::EditPrompt, Action::Stop]
        );
    }

    #[test]
    fn maps_deny_location_for_editor_navigation() {
        let op: AgentOp = serde_json::from_str(
            r#"{"op":"deny","title":"Wrong buffer","reason":"Open the component file first.","location":{"file":"libs/app/util/src/lib/vw-icon-button.component.ts","line":12,"column":1}}"#,
        )
        .unwrap();
        let card = op.into_card("c_1");

        let Card::Deny(card) = card else {
            panic!("expected deny card");
        };
        let location = card.location.expect("deny location");
        assert_eq!(location.line, 12);
        assert!(location.file.ends_with("vw-icon-button.component.ts"));
    }

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
