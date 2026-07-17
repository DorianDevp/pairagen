use anyhow::{Result, anyhow};
use loopbiotic_protocol::{Action, Card};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SessionState {
    Idle,
    Thinking,
    CardShown,
    Working,
    PatchShown,
    PatchExplained,
    PatchFailed,
    GoalLoopFailed,
    Applying,
    Applied,
    Summary,
    Checking,
    Finished,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum NextState {
    Any,
    Conversation,
    Card,
    Patch,
    GoalLoop,
    GoalWhy,
    Summary,
    Finished,
}

impl SessionState {
    pub fn next(&self, action: &Action) -> Result<NextState> {
        match (self, action) {
            (Self::CardShown, Action::Follow | Action::Why | Action::OtherLead | Action::Open) => {
                Ok(NextState::Conversation)
            }
            (Self::CardShown, Action::Goal) => Ok(NextState::GoalLoop),
            (Self::CardShown, Action::Retry | Action::EditPrompt) => Ok(NextState::Any),
            (Self::CardShown, Action::Fix) => Ok(NextState::Patch),
            (Self::CardShown, Action::Stop) => Ok(NextState::Finished),
            (Self::Working, Action::CancelTurn) => Ok(NextState::Conversation),
            (Self::Working, Action::Stop) => Ok(NextState::Finished),
            (Self::PatchShown, Action::Apply | Action::ApplyPatch { .. }) => Ok(NextState::Summary),
            (Self::PatchShown, Action::Why) => Ok(NextState::GoalWhy),
            (Self::PatchShown, Action::Retry | Action::EditPrompt) => Ok(NextState::Patch),
            (Self::PatchShown, Action::Stop) => Ok(NextState::Finished),
            (Self::PatchFailed, Action::Retry | Action::EditPrompt) => Ok(NextState::Patch),
            (Self::PatchFailed, Action::Stop) => Ok(NextState::Finished),
            (Self::PatchExplained, Action::ResumeDraft) => Ok(NextState::Patch),
            (Self::PatchExplained, Action::Why | Action::Retry | Action::EditPrompt) => {
                Ok(NextState::GoalWhy)
            }
            (Self::PatchExplained, Action::Stop) => Ok(NextState::Finished),
            (Self::GoalLoopFailed, Action::Retry | Action::EditPrompt) => Ok(NextState::GoalLoop),
            (Self::GoalLoopFailed, Action::Stop) => Ok(NextState::Finished),
            (Self::Summary, Action::RunCheck) => Ok(NextState::Card),
            (Self::Summary, Action::Stop) => Ok(NextState::Finished),
            (Self::Finished, _) => Err(anyhow!("session is finished")),
            _ => Err(anyhow!("invalid action {action:?} for state {self:?}")),
        }
    }

    pub fn from_card(card: &Card) -> Self {
        match card {
            Card::Patch(_) => Self::PatchShown,
            Card::Working(_) => Self::Working,
            Card::Summary(_) => Self::Summary,
            Card::Error(_) => Self::CardShown,
            _ => Self::CardShown,
        }
    }

    pub fn require_patch(&self) -> Result<()> {
        if *self != Self::PatchShown {
            return Err(anyhow!("patch apply requires patch state"));
        }

        Ok(())
    }
}

impl NextState {
    pub fn validate(&self, card: &Card) -> Result<()> {
        match (self, card) {
            (Self::Any, _) => Ok(()),
            (
                Self::Conversation,
                Card::Hypothesis(_)
                | Card::Finding(_)
                | Card::Choice(_)
                | Card::Deny(_)
                | Card::OpenLocation(_)
                | Card::Error(_),
            ) => Ok(()),
            (Self::Conversation, _) => {
                Err(anyhow!("conversation turn cannot return code or a summary"))
            }
            (Self::Card, Card::Patch(_)) => Err(anyhow!("patch card is not allowed here")),
            (Self::Card, Card::Summary(_)) => Err(anyhow!("summary card is not allowed here")),
            (Self::Card, _) => Ok(()),
            (Self::Patch, Card::Patch(_)) => Ok(()),
            (Self::Patch, _) => Err(anyhow!("expected patch card")),
            (
                Self::GoalLoop,
                Card::Patch(_)
                | Card::Summary(_)
                | Card::Choice(_)
                | Card::Deny(_)
                | Card::Error(_),
            ) => Ok(()),
            (Self::GoalLoop, _) => Err(anyhow!(
                "expected the next goal patch, a blocking choice, or a completed goal summary"
            )),
            (Self::GoalWhy, Card::Finding(_)) => Ok(()),
            (Self::GoalWhy, _) => Err(anyhow!("expected an explanation of the pending patch")),
            (Self::Summary, Card::Summary(_)) => Ok(()),
            (Self::Summary, _) => Err(anyhow!("expected summary card")),
            (Self::Finished, Card::Summary(_)) => Ok(()),
            (Self::Finished, _) => Err(anyhow!("expected final summary")),
        }
    }
}

#[cfg(test)]
mod tests {
    use loopbiotic_protocol::{
        ChoiceCard, DenyCard, ErrorCard, FindingCard, HypothesisCard, Location, OpenLocationCard,
        PatchCard, SummaryCard,
    };

    use super::*;

    fn apply_patch() -> Action {
        Action::ApplyPatch {
            patch_id: "p_1".into(),
        }
    }

    fn all_actions() -> Vec<Action> {
        vec![
            Action::Follow,
            Action::Why,
            Action::ResumeDraft,
            Action::Fix,
            Action::Goal,
            Action::CancelTurn,
            Action::OtherLead,
            Action::Apply,
            apply_patch(),
            Action::Retry,
            Action::EditPrompt,
            Action::Open,
            Action::RunCheck,
            Action::Next,
            Action::Stop,
        ]
    }

    #[test]
    fn every_legal_transition_yields_its_next_state() {
        use Action as A;
        use NextState as N;
        use SessionState as S;

        let table: Vec<(S, A, N)> = vec![
            (S::CardShown, A::Follow, N::Conversation),
            (S::CardShown, A::Why, N::Conversation),
            (S::CardShown, A::OtherLead, N::Conversation),
            (S::CardShown, A::Open, N::Conversation),
            (S::CardShown, A::Goal, N::GoalLoop),
            (S::CardShown, A::Retry, N::Any),
            (S::CardShown, A::EditPrompt, N::Any),
            (S::CardShown, A::Fix, N::Patch),
            (S::CardShown, A::Stop, N::Finished),
            (S::PatchShown, A::Apply, N::Summary),
            (S::PatchShown, apply_patch(), N::Summary),
            (S::PatchShown, A::Why, N::GoalWhy),
            (S::PatchShown, A::Retry, N::Patch),
            (S::PatchShown, A::EditPrompt, N::Patch),
            (S::PatchShown, A::Stop, N::Finished),
            (S::PatchFailed, A::Retry, N::Patch),
            (S::PatchFailed, A::EditPrompt, N::Patch),
            (S::PatchFailed, A::Stop, N::Finished),
            (S::PatchExplained, A::ResumeDraft, N::Patch),
            (S::PatchExplained, A::Why, N::GoalWhy),
            (S::PatchExplained, A::Retry, N::GoalWhy),
            (S::PatchExplained, A::EditPrompt, N::GoalWhy),
            (S::PatchExplained, A::Stop, N::Finished),
            (S::GoalLoopFailed, A::Retry, N::GoalLoop),
            (S::GoalLoopFailed, A::EditPrompt, N::GoalLoop),
            (S::GoalLoopFailed, A::Stop, N::Finished),
            (S::Working, A::CancelTurn, N::Conversation),
            (S::Working, A::Stop, N::Finished),
            (S::Summary, A::RunCheck, N::Card),
            (S::Summary, A::Stop, N::Finished),
        ];

        for (state, action, expected) in table {
            let next = state
                .next(&action)
                .unwrap_or_else(|error| panic!("{state:?} + {action:?} should be legal: {error}"));
            assert_eq!(next, expected, "{state:?} + {action:?}");
        }
    }

    #[test]
    fn illegal_transitions_are_rejected() {
        use Action as A;
        use SessionState as S;

        let table: Vec<(S, A)> = vec![
            (S::Idle, A::Follow),
            (S::Idle, A::Stop),
            (S::Thinking, A::Fix),
            (S::Thinking, A::Stop),
            (S::CardShown, A::Apply),
            (S::CardShown, apply_patch()),
            (S::CardShown, A::ResumeDraft),
            (S::CardShown, A::Next),
            (S::CardShown, A::CancelTurn),
            (S::CardShown, A::RunCheck),
            (S::PatchShown, A::Follow),
            (S::PatchShown, A::Fix),
            (S::PatchShown, A::OtherLead),
            (S::PatchShown, A::Next),
            (S::PatchFailed, A::Apply),
            (S::PatchFailed, A::Fix),
            (S::PatchFailed, A::Why),
            (S::PatchExplained, A::Apply),
            (S::PatchExplained, A::Fix),
            (S::GoalLoopFailed, A::Apply),
            (S::GoalLoopFailed, A::Fix),
            (S::GoalLoopFailed, A::Next),
            (S::Summary, A::Apply),
            (S::Summary, A::Fix),
            (S::Summary, A::Retry),
            (S::Summary, A::Next),
            (S::Applying, A::Stop),
            (S::Applied, A::Stop),
            (S::Checking, A::Stop),
        ];

        for (state, action) in table {
            let error = state.next(&action).unwrap_err();
            assert!(
                error.to_string().contains("invalid action"),
                "{state:?} + {action:?}: {error}"
            );
        }
    }

    #[test]
    fn finished_rejects_every_action() {
        for action in all_actions() {
            let error = SessionState::Finished.next(&action).unwrap_err();
            assert!(
                error.to_string().contains("session is finished"),
                "{action:?}: {error}"
            );
        }
    }

    fn hypothesis_card() -> Card {
        Card::Hypothesis(HypothesisCard {
            id: "c_h".into(),
            title: "t".into(),
            claim: "c".into(),
            evidence: None,
            next_move: None,
            actions: vec![Action::Stop],
        })
    }

    fn finding_card() -> Card {
        Card::Finding(FindingCard {
            id: "c_f".into(),
            title: "t".into(),
            finding: "f".into(),
            location: None,
            annotation: None,
            actions: vec![Action::Stop],
        })
    }

    fn patch_card() -> Card {
        Card::Patch(PatchCard {
            id: "c_p".into(),
            title: "t".into(),
            explanation: "e".into(),
            warnings: vec![],
            goal_complete: false,
            plan: None,
            patches: vec![],
            actions: vec![Action::Apply],
        })
    }

    fn choice_card() -> Card {
        Card::Choice(ChoiceCard {
            id: "c_c".into(),
            title: "t".into(),
            question: "q".into(),
            options: vec![],
        })
    }

    fn deny_card() -> Card {
        Card::Deny(DenyCard {
            id: "c_d".into(),
            title: "t".into(),
            reason: "r".into(),
            location: None,
            actions: vec![Action::Stop],
        })
    }

    fn open_location_card() -> Card {
        Card::OpenLocation(OpenLocationCard {
            id: "c_o".into(),
            reason: "r".into(),
            location: Location {
                file: "src/work.ts".into(),
                line: 1,
                column: 1,
            },
        })
    }

    fn summary_card() -> Card {
        Card::Summary(SummaryCard {
            id: "c_s".into(),
            title: "t".into(),
            summary: "s".into(),
            changed_files: vec![],
            next_actions: vec![],
        })
    }

    fn error_card() -> Card {
        Card::Error(ErrorCard {
            id: "c_e".into(),
            title: "t".into(),
            message: "m".into(),
            actions: vec![Action::Stop],
        })
    }

    #[test]
    fn next_state_validates_expected_card_kinds() {
        use NextState as N;

        let table: Vec<(N, Card, std::result::Result<(), &str>)> = vec![
            (N::Any, patch_card(), Ok(())),
            (N::Any, summary_card(), Ok(())),
            (N::Any, error_card(), Ok(())),
            (N::Conversation, hypothesis_card(), Ok(())),
            (N::Conversation, finding_card(), Ok(())),
            (N::Conversation, choice_card(), Ok(())),
            (
                N::Conversation,
                patch_card(),
                Err("conversation turn cannot return code or a summary"),
            ),
            (
                N::Conversation,
                summary_card(),
                Err("conversation turn cannot return code or a summary"),
            ),
            (N::Card, hypothesis_card(), Ok(())),
            (N::Card, finding_card(), Ok(())),
            (N::Card, choice_card(), Ok(())),
            (N::Card, deny_card(), Ok(())),
            (N::Card, open_location_card(), Ok(())),
            (N::Card, error_card(), Ok(())),
            (N::Card, patch_card(), Err("patch card is not allowed here")),
            (
                N::Card,
                summary_card(),
                Err("summary card is not allowed here"),
            ),
            (N::Patch, patch_card(), Ok(())),
            (N::Patch, finding_card(), Err("expected patch card")),
            (N::Patch, summary_card(), Err("expected patch card")),
            (N::GoalLoop, patch_card(), Ok(())),
            (N::GoalLoop, summary_card(), Ok(())),
            (N::GoalLoop, choice_card(), Ok(())),
            (N::GoalLoop, deny_card(), Ok(())),
            (N::GoalLoop, error_card(), Ok(())),
            (
                N::GoalLoop,
                finding_card(),
                Err("expected the next goal patch, a blocking choice, or a completed goal summary"),
            ),
            (
                N::GoalLoop,
                hypothesis_card(),
                Err("expected the next goal patch, a blocking choice, or a completed goal summary"),
            ),
            (N::GoalWhy, finding_card(), Ok(())),
            (
                N::GoalWhy,
                patch_card(),
                Err("expected an explanation of the pending patch"),
            ),
            (N::Summary, summary_card(), Ok(())),
            (N::Summary, patch_card(), Err("expected summary card")),
            (N::Finished, summary_card(), Ok(())),
            (N::Finished, finding_card(), Err("expected final summary")),
        ];

        for (next_state, card, expected) in table {
            let result = next_state.validate(&card);
            match expected {
                Ok(()) => assert!(
                    result.is_ok(),
                    "{next_state:?} should accept {:?}: {result:?}",
                    card.kind()
                ),
                Err(message) => match result {
                    Ok(()) => panic!("{next_state:?} should reject {:?}", card.kind()),
                    Err(error) => assert!(
                        error.to_string().contains(message),
                        "{next_state:?} + {:?}: {error}",
                        card.kind()
                    ),
                },
            }
        }
    }
}
