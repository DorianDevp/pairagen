use anyhow::Result;
use pair_protocol::{Action, BackendInfo, Card, HypothesisCard};

pub struct MockBackend;

impl MockBackend {
    pub fn info() -> BackendInfo {
        BackendInfo {
            name: "mock".into(),
            streaming: false,
            patches: true,
            reasoning: true,
            can_read_project: false,
            can_use_tools: false,
        }
    }

    pub fn first_card() -> Result<Card> {
        let card = HypothesisCard {
            id: "c_1".into(),
            title: "Payload may be skipped".into(),
            claim: "This path can return before the payload is built.".into(),
            evidence: None,
            next_move: None,
            actions: vec![
                Action::Follow,
                Action::Why,
                Action::Fix,
                Action::OtherLead,
                Action::Stop,
            ],
        };

        Ok(Card::Hypothesis(card))
    }
}
