use serde_json::Value;

use crate::BackendPreview;

const PREVIEW_BODY_FIELDS: &[&str] = &[
    "claim",
    "finding",
    "question",
    "explanation",
    "reason",
    "summary",
    "message",
];
const PREVIEW_BUFFER_LIMIT: usize = 8_192;
const PREVIEW_BODY_MIN_CHARS: usize = 8;
const PREVIEW_BODY_STEP_CHARS: usize = 24;
const PREVIEW_BODY_MAX_CHARS: usize = 280;

/// Incrementally extracts safe, non-actionable card content from a streamed
/// JSON response. Patch payloads and actions are intentionally ignored: only
/// the title and explanatory body may reach the editor before validation.
#[derive(Default)]
pub(crate) struct StreamPreview {
    buffer: String,
    last_title: Option<String>,
    last_body_chars: usize,
}

impl StreamPreview {
    pub(crate) fn push(&mut self, delta: &str) -> Option<BackendPreview> {
        if self.buffer.len() >= PREVIEW_BUFFER_LIMIT {
            return None;
        }

        self.buffer.push_str(delta);
        if self.buffer.len() > PREVIEW_BUFFER_LIMIT {
            return None;
        }

        let title = extract_string_field(&self.buffer, "title")
            .filter(|(title, complete)| *complete && !title.trim().is_empty())
            .map(|(title, _)| compact_preview(&title, PREVIEW_BODY_MAX_CHARS));
        let title_changed = title
            .as_ref()
            .is_some_and(|title| self.last_title.as_ref() != Some(title));

        let body = PREVIEW_BODY_FIELDS
            .iter()
            .find_map(|field| extract_string_field(&self.buffer, field));
        let body_ready = body.as_ref().is_some_and(|(value, complete)| {
            *complete || value.chars().count() >= PREVIEW_BODY_MIN_CHARS
        });

        let body_changed = body.as_ref().is_some_and(|(body, complete)| {
            let body_chars = body.chars().count();
            body_ready
                && body_chars != self.last_body_chars
                && (*complete
                    || self.last_body_chars == 0
                    || body_chars >= self.last_body_chars.saturating_add(PREVIEW_BODY_STEP_CHARS))
        });
        if !title_changed && !body_changed {
            return None;
        }

        if let Some(title) = &title {
            self.last_title = Some(title.clone());
        }
        let body = body.and_then(|(body, _)| {
            if body_changed {
                self.last_body_chars = body.chars().count();
            }
            let body = compact_preview(&body, PREVIEW_BODY_MAX_CHARS);
            (!body.is_empty()).then_some(body)
        });
        Some(BackendPreview {
            title: title
                .or_else(|| self.last_title.clone())
                .unwrap_or_else(|| "Drafting response".into()),
            body,
        })
    }
}

fn compact_preview(value: &str, max_chars: usize) -> String {
    let compact = value.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut result = compact.chars().take(max_chars).collect::<String>();
    if compact.chars().count() > max_chars {
        result.push('…');
    }
    result
}

/// Returns the (possibly still streaming) value of `"field":"..."`, plus
/// whether its closing quote has arrived.
pub(crate) fn extract_string_field(json: &str, field: &str) -> Option<(String, bool)> {
    let needle = format!("\"{field}\"");
    let start = json.find(&needle)? + needle.len();
    let rest = json[start..].trim_start();
    let rest = rest.strip_prefix(':')?.trim_start();
    let rest = rest.strip_prefix('"')?;

    let mut value = String::new();
    let mut chars = rest.chars();
    while let Some(next) = chars.next() {
        match next {
            '"' => return Some((value, true)),
            '\\' => match chars.next() {
                Some('n') => value.push('\n'),
                Some('t') => value.push('\t'),
                Some('u') => {
                    // Preview fidelity is sufficient here; final JSON parsing
                    // remains authoritative for the validated card.
                    for _ in 0..4 {
                        chars.next();
                    }
                    value.push('?');
                }
                Some(escaped) => value.push(escaped),
                None => return Some((value, false)),
            },
            _ => value.push(next),
        }
    }

    Some((value, false))
}

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

    #[test]
    fn preview_reports_title_then_incremental_body_without_patch_data() {
        let mut preview = StreamPreview::default();

        assert_eq!(preview.push("{\"op\":\"hypothesis\",\"ti"), None);
        assert_eq!(
            preview.push("tle\":\"Falsy guard\","),
            Some(BackendPreview {
                title: "Falsy guard".into(),
                body: None,
            })
        );
        assert_eq!(
            preview.push("\"claim\":\"The guard rejects"),
            Some(BackendPreview {
                title: "Falsy guard".into(),
                body: Some("The guard rejects".into()),
            })
        );
        assert_eq!(
            preview.push(" 0, empty strings and false"),
            Some(BackendPreview {
                title: "Falsy guard".into(),
                body: Some("The guard rejects 0, empty strings and false".into()),
            })
        );
        assert_eq!(preview.push(", so callers lose data"), None);
        let completed = preview.push("\",\"diff\":\"secret patch\"").unwrap();
        assert!(
            completed
                .body
                .as_deref()
                .unwrap()
                .contains("callers lose data")
        );
        assert!(!format!("{completed:?}").contains("secret patch"));
    }

    #[test]
    fn preview_does_not_wait_for_a_late_title() {
        let mut preview = StreamPreview::default();

        let body = preview
            .push("{\"claim\":\"The useful explanation arrives before its title")
            .expect("body-first preview");
        assert_eq!(body.title, "Drafting response");
        assert!(body.body.unwrap().starts_with("The useful explanation"));

        let title = preview
            .push("\",\"title\":\"Late title\"")
            .expect("late title update");
        assert_eq!(title.title, "Late title");
        assert!(title.body.unwrap().starts_with("The useful explanation"));
    }

    #[test]
    fn extracts_escaped_and_partial_string_fields() {
        assert_eq!(
            extract_string_field(r#"{"title":"a \"quoted\" step""#, "title"),
            Some(("a \"quoted\" step".into(), true))
        );
        assert_eq!(
            extract_string_field(r#"{"title":"still stream"#, "title"),
            Some(("still stream".into(), false))
        );
        assert_eq!(extract_string_field(r#"{"titl"#, "title"), None);
    }
}
