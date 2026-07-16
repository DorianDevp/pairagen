//! Session observation memory: recording hypothesis/finding/signal
//! observations, deduplicating repeats, and rendering them for prompts.

use loopbiotic_protocol::{Card, ObservationKind, ObservationProgress};

use crate::session::Session;

pub(super) fn prepare_observation_card(session: &mut Session, card: &mut Card) {
    if core_observation(card).is_some() {
        for observation in &mut session.known_observations {
            observation.active = false;
        }
    }

    match card {
        Card::Hypothesis(card) => {
            if let Some(evidence) = &mut card.evidence
                && !evidence.annotation.trim().is_empty()
            {
                let key = observation_key(ObservationKind::Signal, &evidence.annotation);
                if session.observation_index.contains_key(&key) {
                    activate_observation(session, &key);
                    evidence.annotation.clear();
                }
            }
        }
        Card::Finding(card) => {
            if let Some(annotation) = card
                .annotation
                .clone()
                .filter(|text| !text.trim().is_empty())
            {
                let key = observation_key(ObservationKind::Signal, &annotation);
                if session.observation_index.contains_key(&key) {
                    activate_observation(session, &key);
                    card.annotation = None;
                }
            }
        }
        _ => {}
    }
}

pub(super) fn record_observations(session: &mut Session, card: &Card) {
    if let Some((key, kind, label)) = core_observation(card) {
        record_observation(session, key, kind, label);
    }

    match card {
        Card::Hypothesis(card) => {
            if let Some(evidence) = &card.evidence
                && !evidence.annotation.trim().is_empty()
            {
                let label = evidence.annotation.clone();
                record_observation(
                    session,
                    observation_key(ObservationKind::Signal, &label),
                    ObservationKind::Signal,
                    label,
                );
            }
        }
        Card::Finding(card) => {
            if let Some(label) = card
                .annotation
                .clone()
                .filter(|text| !text.trim().is_empty())
            {
                record_observation(
                    session,
                    observation_key(ObservationKind::Signal, &label),
                    ObservationKind::Signal,
                    label,
                );
            }
        }
        _ => {}
    }
}

pub(super) fn core_observation(card: &Card) -> Option<(String, ObservationKind, String)> {
    let (kind, label) = match card {
        Card::Hypothesis(card) => (ObservationKind::Hypothesis, card.claim.clone()),
        Card::Finding(card) => (ObservationKind::Finding, card.finding.clone()),
        _ => return None,
    };

    Some((observation_key(kind, &label), kind, label))
}

fn record_observation(session: &mut Session, key: String, kind: ObservationKind, label: String) {
    if let Some(index) = session.observation_index.get(&key).copied() {
        if let Some(observation) = session.known_observations.get_mut(index) {
            observation.occurrences += 1;
            observation.active = true;
        }
        return;
    }

    let index = session.known_observations.len();
    session.observation_index.insert(key, index);
    session.known_observations.push(ObservationProgress {
        id: format!("o_{}", index + 1),
        kind,
        label,
        occurrences: 1,
        active: true,
    });
}

pub(super) fn activate_observation(session: &mut Session, key: &str) {
    let Some(index) = session.observation_index.get(key).copied() else {
        return;
    };
    if let Some(observation) = session.known_observations.get_mut(index) {
        observation.occurrences += 1;
        observation.active = true;
    }
}

fn observation_key(kind: ObservationKind, label: &str) -> String {
    format!(
        "{}:{}",
        observation_kind_name(kind),
        normalize_observation(label)
    )
}

pub(super) fn normalize_observation(text: &str) -> String {
    text.to_lowercase()
        .chars()
        .map(|character| {
            if character.is_alphanumeric() {
                character
            } else {
                ' '
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn observation_kind_name(kind: ObservationKind) -> &'static str {
    match kind {
        ObservationKind::Hypothesis => "hypothesis",
        ObservationKind::Finding => "finding",
        ObservationKind::Signal => "signal",
    }
}

pub(super) fn observation_prompt_line(observation: &ObservationProgress) -> String {
    format!(
        "{} {} (seen {}x): {}",
        observation.id,
        observation_kind_name(observation.kind),
        observation.occurrences,
        observation.label
    )
}
