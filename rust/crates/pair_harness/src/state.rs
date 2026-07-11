use anyhow::{Result, anyhow};
use pair_protocol::{Action, Card};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SessionState {
    Idle,
    Thinking,
    CardShown,
    PatchShown,
    PatchFailed,
    ContinuationFailed,
    Applying,
    Applied,
    Summary,
    Checking,
    Finished,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum NextState {
    Any,
    Card,
    Patch,
    Continuation,
    Summary,
    Finished,
}

impl SessionState {
    pub fn next(&self, action: &Action) -> Result<NextState> {
        match (self, action) {
            (Self::CardShown, Action::Follow | Action::Why | Action::OtherLead | Action::Open) => {
                Ok(NextState::Card)
            }
            (Self::CardShown, Action::Retry | Action::EditPrompt) => Ok(NextState::Any),
            (Self::CardShown, Action::Fix) => Ok(NextState::Patch),
            (Self::CardShown, Action::Stop) => Ok(NextState::Finished),
            (Self::PatchShown, Action::Apply | Action::ApplyPatch { .. }) => Ok(NextState::Summary),
            (Self::PatchShown, Action::Retry | Action::EditPrompt) => Ok(NextState::Patch),
            (Self::PatchShown, Action::Stop) => Ok(NextState::Finished),
            (Self::PatchFailed, Action::Retry | Action::EditPrompt) => Ok(NextState::Patch),
            (Self::PatchFailed, Action::Stop) => Ok(NextState::Finished),
            (Self::ContinuationFailed, Action::Retry | Action::EditPrompt) => {
                Ok(NextState::Continuation)
            }
            (Self::ContinuationFailed, Action::Stop) => Ok(NextState::Finished),
            (Self::Summary, Action::Next) => Ok(NextState::Continuation),
            (Self::Summary, Action::RunCheck) => Ok(NextState::Card),
            (Self::Summary, Action::Stop) => Ok(NextState::Finished),
            (Self::Finished, _) => Err(anyhow!("session is finished")),
            _ => Err(anyhow!("invalid action {action:?} for state {self:?}")),
        }
    }

    pub fn from_card(card: &Card) -> Self {
        match card {
            Card::Patch(_) => Self::PatchShown,
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
            (Self::Card, Card::Patch(_)) => Err(anyhow!("patch card is not allowed here")),
            (Self::Card, Card::Summary(_)) => Err(anyhow!("summary card is not allowed here")),
            (Self::Card, _) => Ok(()),
            (Self::Patch, Card::Patch(_)) => Ok(()),
            (Self::Patch, _) => Err(anyhow!("expected patch card")),
            (Self::Continuation, Card::Patch(_) | Card::Summary(_)) => Ok(()),
            (Self::Continuation, _) => Err(anyhow!(
                "expected a continuation patch or completed goal summary"
            )),
            (Self::Summary, Card::Summary(_)) => Ok(()),
            (Self::Summary, _) => Err(anyhow!("expected summary card")),
            (Self::Finished, Card::Summary(_)) => Ok(()),
            (Self::Finished, _) => Err(anyhow!("expected final summary")),
        }
    }
}
