use std::collections::BTreeMap;

use anyhow::{Result, anyhow};
use loopbiotic_protocol::TokenUsage;
use serde_json::Value;
use tokio::sync::watch;

use crate::ProgressReporter;
use crate::stream::StreamPreview;
use crate::support::{report_preview, report_progress};

use super::SUBMIT_CARD_TOOL;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct FunctionCall {
    pub(super) call_id: String,
    pub(super) name: String,
    pub(super) arguments: String,
}

#[derive(Debug)]
pub(super) struct ResponseTurn {
    pub(super) response_id: String,
    pub(super) calls: Vec<FunctionCall>,
    pub(super) text: String,
    pub(super) token_usage: Option<TokenUsage>,
    pub(super) reasoning_seen: bool,
}

#[derive(Default)]
struct PendingCall {
    call_id: String,
    name: String,
    arguments: String,
}

#[derive(Default)]
struct Accumulator {
    calls: BTreeMap<String, PendingCall>,
    text: String,
    preview: StreamPreview,
    reasoning_seen: bool,
    streaming_reported: bool,
}

pub(super) async fn read_response_stream(
    mut response: reqwest::Response,
    session_id: &str,
    progress: Option<&ProgressReporter>,
    mut cancelled: watch::Receiver<bool>,
) -> Result<ResponseTurn> {
    let mut decoder = SseDecoder::default();
    let mut accumulator = Accumulator::default();

    loop {
        let chunk = tokio::select! {
            changed = cancelled.changed() => {
                if changed.is_ok() && *cancelled.borrow() {
                    return Err(anyhow!("local model turn was interrupted"));
                }
                continue;
            }
            chunk = response.chunk() => chunk?,
        };

        let Some(chunk) = chunk else {
            break;
        };
        for event in decoder.push(&chunk)? {
            if let Some(turn) = accumulator.handle(event, session_id, progress)? {
                return Ok(turn);
            }
        }
    }

    Err(anyhow!(
        "LM Studio response stream ended without response.completed"
    ))
}

impl Accumulator {
    fn handle(
        &mut self,
        event: Value,
        session_id: &str,
        progress: Option<&ProgressReporter>,
    ) -> Result<Option<ResponseTurn>> {
        match event.get("type").and_then(Value::as_str) {
            Some(
                "response.reasoning_text.delta"
                | "response.reasoning_summary_text.delta"
                | "response.reasoning.delta",
            ) => {
                if !self.reasoning_seen {
                    self.reasoning_seen = true;
                    report_progress(
                        progress,
                        session_id,
                        "reasoning",
                        "Local model is reasoning",
                    );
                }
            }
            Some("response.output_text.delta") => {
                self.report_streaming(session_id, progress);
                if let Some(delta) = event.get("delta").and_then(Value::as_str) {
                    self.text.push_str(delta);
                    if let Some(preview) = self.preview.push(delta) {
                        report_preview(progress, session_id, preview);
                    }
                }
            }
            Some("response.output_item.added") => {
                let Some(item) = event.get("item") else {
                    return Ok(None);
                };
                if item.get("type").and_then(Value::as_str) == Some("function_call") {
                    let item_id = item
                        .get("id")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    self.calls.insert(
                        item_id,
                        PendingCall {
                            call_id: item
                                .get("call_id")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_string(),
                            name: item
                                .get("name")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_string(),
                            arguments: item
                                .get("arguments")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_string(),
                        },
                    );
                }
            }
            Some("response.function_call_arguments.delta") => {
                self.report_streaming(session_id, progress);
                let item_id = event
                    .get("item_id")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let delta = event
                    .get("delta")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if let Some(call) = self.calls.get_mut(item_id) {
                    call.arguments.push_str(delta);
                    if call.name == SUBMIT_CARD_TOOL
                        && let Some(preview) = self.preview.push(delta)
                    {
                        report_preview(progress, session_id, preview);
                    }
                }
            }
            Some("response.function_call_arguments.done") => {
                let item_id = event
                    .get("item_id")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if let Some(call) = self.calls.get_mut(item_id)
                    && let Some(arguments) = event.get("arguments").and_then(Value::as_str)
                {
                    call.arguments = arguments.to_string();
                }
            }
            Some("response.completed") => {
                let response = event
                    .get("response")
                    .ok_or_else(|| anyhow!("response.completed omitted response"))?;
                if response.get("status").and_then(Value::as_str) == Some("incomplete") {
                    let reason = response
                        .pointer("/incomplete_details/reason")
                        .and_then(Value::as_str)
                        .unwrap_or("unknown reason");
                    return Err(anyhow!("local model response was incomplete: {reason}"));
                }
                let response_id = response
                    .get("id")
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow!("response.completed omitted id"))?
                    .to_string();
                let calls = completed_calls(response, &self.calls);
                let text = completed_text(response).unwrap_or_else(|| self.text.clone());
                return Ok(Some(ResponseTurn {
                    response_id,
                    calls,
                    text,
                    token_usage: parse_usage(response.get("usage")),
                    reasoning_seen: self.reasoning_seen,
                }));
            }
            Some("response.failed" | "response.incomplete") => {
                return Err(anyhow!(response_error(&event)));
            }
            Some("error") => return Err(anyhow!(response_error(&event))),
            _ => {}
        }

        Ok(None)
    }

    fn report_streaming(&mut self, session_id: &str, progress: Option<&ProgressReporter>) {
        if self.streaming_reported {
            return;
        }
        self.streaming_reported = true;
        report_progress(
            progress,
            session_id,
            "streaming",
            "Local model started streaming the response",
        );
    }
}

fn completed_calls(response: &Value, pending: &BTreeMap<String, PendingCall>) -> Vec<FunctionCall> {
    let calls = response
        .get("output")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|item| item.get("type").and_then(Value::as_str) == Some("function_call"))
        .filter_map(|item| {
            Some(FunctionCall {
                call_id: item.get("call_id")?.as_str()?.to_string(),
                name: item.get("name")?.as_str()?.to_string(),
                arguments: item
                    .get("arguments")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
            })
        })
        .collect::<Vec<_>>();
    if !calls.is_empty() {
        return calls;
    }

    pending
        .values()
        .filter(|call| !call.call_id.is_empty() && !call.name.is_empty())
        .map(|call| FunctionCall {
            call_id: call.call_id.clone(),
            name: call.name.clone(),
            arguments: call.arguments.clone(),
        })
        .collect()
}

fn completed_text(response: &Value) -> Option<String> {
    let parts = response
        .get("output")
        .and_then(Value::as_array)?
        .iter()
        .filter(|item| item.get("type").and_then(Value::as_str) == Some("message"))
        .filter_map(|item| item.get("content").and_then(Value::as_array))
        .flatten()
        .filter(|part| part.get("type").and_then(Value::as_str) == Some("output_text"))
        .filter_map(|part| part.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>();
    (!parts.is_empty()).then(|| parts.concat())
}

fn parse_usage(value: Option<&Value>) -> Option<TokenUsage> {
    let value = value?;
    let input_tokens = value.get("input_tokens")?.as_u64()? as usize;
    let output_tokens = value.get("output_tokens")?.as_u64()? as usize;
    let cached_input_tokens = value
        .get("input_tokens_details")
        .and_then(|details| details.get("cached_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or_default() as usize;
    Some(TokenUsage {
        input_tokens,
        cached_input_tokens,
        output_tokens,
        total_tokens: value
            .get("total_tokens")
            .and_then(Value::as_u64)
            .unwrap_or((input_tokens + output_tokens) as u64) as usize,
        estimated: false,
    })
}

fn response_error(event: &Value) -> String {
    event
        .pointer("/error/message")
        .or_else(|| event.pointer("/response/error/message"))
        .and_then(Value::as_str)
        .unwrap_or("LM Studio response failed")
        .to_string()
}

#[derive(Default)]
struct SseDecoder {
    buffer: Vec<u8>,
}

impl SseDecoder {
    fn push(&mut self, chunk: &[u8]) -> Result<Vec<Value>> {
        self.buffer.extend_from_slice(chunk);
        let mut events = Vec::new();
        while let Some((end, separator_len)) = frame_boundary(&self.buffer) {
            let frame = self.buffer.drain(..end).collect::<Vec<_>>();
            self.buffer.drain(..separator_len);
            let frame = std::str::from_utf8(&frame)?;
            let data = frame
                .lines()
                .filter_map(|line| line.strip_prefix("data:"))
                .map(str::trim_start)
                .collect::<Vec<_>>()
                .join("\n");
            if data.is_empty() || data == "[DONE]" {
                continue;
            }
            events.push(serde_json::from_str(&data)?);
        }
        Ok(events)
    }
}

fn frame_boundary(bytes: &[u8]) -> Option<(usize, usize)> {
    bytes
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|index| (index, 4))
        .or_else(|| {
            bytes
                .windows(2)
                .position(|window| window == b"\n\n")
                .map(|index| (index, 2))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_fragmented_crlf_sse_frames() {
        let mut decoder = SseDecoder::default();
        assert!(
            decoder
                .push(b"event: response.created\r\nda")
                .unwrap()
                .is_empty()
        );
        let events = decoder
            .push(b"ta: {\"type\":\"response.created\"}\r\n\r\n")
            .unwrap();
        assert_eq!(events[0]["type"], "response.created");
    }

    #[test]
    fn parses_completed_function_call_and_cached_usage() {
        let response = serde_json::json!({
            "output": [{
                "type": "function_call",
                "call_id": "call_1",
                "name": "submit_card",
                "arguments": "{\"op\":\"finding\"}"
            }],
            "usage": {
                "input_tokens": 100,
                "output_tokens": 20,
                "total_tokens": 120,
                "input_tokens_details": {"cached_tokens": 60}
            }
        });
        let calls = completed_calls(&response, &BTreeMap::new());
        let usage = parse_usage(response.get("usage")).unwrap();

        assert_eq!(calls[0].name, SUBMIT_CARD_TOOL);
        assert_eq!(usage.cached_input_tokens, 60);
        assert_eq!(usage.total_tokens, 120);
    }

    #[test]
    fn extracts_only_final_message_text() {
        let response = serde_json::json!({
            "output": [
                {"type":"reasoning","content":[{"type":"reasoning_text","text":"private"}]},
                {"type":"message","content":[{"type":"output_text","text":"visible"}]}
            ]
        });

        assert_eq!(completed_text(&response).as_deref(), Some("visible"));
    }

    #[test]
    fn reports_incomplete_completion_reason() {
        let mut accumulator = Accumulator::default();
        let error = accumulator
            .handle(
                serde_json::json!({
                    "type": "response.completed",
                    "response": {
                        "id": "resp_1",
                        "status": "incomplete",
                        "incomplete_details": {"reason": "max_output_tokens"}
                    }
                }),
                "s_1",
                None,
            )
            .unwrap_err();

        assert!(error.to_string().contains("max_output_tokens"));
    }
}
