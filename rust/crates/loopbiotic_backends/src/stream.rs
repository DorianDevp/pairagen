use serde_json::Value;

#[derive(Clone, Debug, PartialEq)]
pub enum LoopbioticStreamEvent {
    Progress { phase: String, message: String },
    Result(Value),
}

pub fn parse_loopbiotic_stream_event(line: &str) -> Option<LoopbioticStreamEvent> {
    let value = serde_json::from_str::<Value>(line).ok()?;
    let event_type = value.get("t")?.as_str()?;

    match event_type {
        "loopbiotic_progress" => Some(LoopbioticStreamEvent::Progress {
            phase: value
                .get("phase")
                .and_then(Value::as_str)
                .unwrap_or("working")
                .to_string(),
            message: value
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("Agent is working")
                .to_string(),
        }),
        "loopbiotic_result" => value
            .get("result")
            .cloned()
            .map(LoopbioticStreamEvent::Result),
        _ => None,
    }
}

pub fn result_text(value: Value) -> String {
    value
        .as_str()
        .map(str::to_string)
        .unwrap_or_else(|| serde_json::to_string(&value).unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn parses_user_visible_progress() {
        let event = parse_loopbiotic_stream_event(
            r#"{"t":"loopbiotic_progress","phase":"reviewing","message":"Reviewing context"}"#,
        );

        assert_eq!(
            event,
            Some(LoopbioticStreamEvent::Progress {
                phase: "reviewing".into(),
                message: "Reviewing context".into(),
            })
        );
    }

    #[test]
    fn extracts_final_result() {
        let result = result_text(json!({"op":"hypothesis","title":"T","claim":"C"}));

        assert!(result.contains("\"op\":\"hypothesis\""));
    }
}
