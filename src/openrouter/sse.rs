use bytes::BytesMut;

use serde_json::{Map, Value};

use super::protocol::ChatChunk;
use super::types::{
    ChatStreamEvent, OpenRouterFailure, OpenRouterFailureCategory, OpenRouterStreamStage,
    TokenUsage, MAX_ASSISTANT_BYTES, MAX_SSE_EVENT_BYTES,
};

pub(crate) struct SseDecoder {
    buffered: BytesMut,
    data_lines: Vec<String>,
    event_bytes: usize,
}

impl SseDecoder {
    pub(crate) fn new() -> Self {
        Self {
            buffered: BytesMut::new(),
            data_lines: Vec::new(),
            event_bytes: 0,
        }
    }

    pub(crate) fn push(&mut self, bytes: &[u8]) -> Result<Vec<String>, OpenRouterFailure> {
        let mut events = Vec::new();
        for byte in bytes {
            if self.buffered.len() >= MAX_SSE_EVENT_BYTES {
                return Err(staged_limit(OpenRouterStreamStage::SseFrameLimit));
            }
            self.buffered.extend_from_slice(&[*byte]);
            if *byte == b'\n' {
                let mut line = self.buffered.split().to_vec();
                line.pop();
                if line.last() == Some(&b'\r') {
                    line.pop();
                }
                self.process_line(&line, &mut events)?;
            }
        }
        Ok(events)
    }

    pub(crate) fn finish(mut self) -> Result<Vec<String>, OpenRouterFailure> {
        let mut events = Vec::new();
        if !self.buffered.is_empty() {
            let line = self.buffered.split().to_vec();
            self.process_line(&line, &mut events)?;
        }
        if !self.data_lines.is_empty() {
            events.push(self.take_event());
        }
        Ok(events)
    }

    fn process_line(
        &mut self,
        line: &[u8],
        events: &mut Vec<String>,
    ) -> Result<(), OpenRouterFailure> {
        self.event_bytes = self.event_bytes.saturating_add(line.len() + 1);
        if self.event_bytes > MAX_SSE_EVENT_BYTES {
            return Err(staged_limit(OpenRouterStreamStage::SseFrameLimit));
        }
        if line.is_empty() {
            if !self.data_lines.is_empty() {
                events.push(self.take_event());
            } else {
                self.event_bytes = 0;
            }
            return Ok(());
        }
        if line[0] == b':' {
            return Ok(());
        }
        let Some(value) = line.strip_prefix(b"data:") else {
            return Ok(());
        };
        let value = value.strip_prefix(b" ").unwrap_or(value);
        let value = std::str::from_utf8(value)
            .map_err(|_| staged_invalid(OpenRouterStreamStage::SseUtf8))?;
        self.data_lines.push(value.to_owned());
        Ok(())
    }

    fn take_event(&mut self) -> String {
        let value = self.data_lines.join("\n");
        self.data_lines.clear();
        self.event_bytes = 0;
        value
    }
}

fn staged_limit(stage: OpenRouterStreamStage) -> OpenRouterFailure {
    OpenRouterFailure::new(OpenRouterFailureCategory::ResourceLimit).at_stage(stage)
}

fn staged_invalid(stage: OpenRouterStreamStage) -> OpenRouterFailure {
    OpenRouterFailure::new(OpenRouterFailureCategory::InvalidResponse).at_stage(stage)
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) struct ChatConsumeResult {
    pub(crate) events: Vec<ChatStreamEvent>,
    pub(crate) compatibility_stage: Option<OpenRouterStreamStage>,
}

pub(crate) struct ChatAccumulator {
    response_id: Option<String>,
    stream_model: Option<String>,
    terminal_choice: bool,
    done: bool,
    assistant: String,
    usage: Option<TokenUsage>,
    usage_drop_reported: bool,
}

impl ChatAccumulator {
    pub(crate) fn new() -> Self {
        Self {
            response_id: None,
            stream_model: None,
            terminal_choice: false,
            done: false,
            assistant: String::new(),
            usage: None,
            usage_drop_reported: false,
        }
    }

    pub(crate) fn consume(&mut self, data: &str) -> Result<ChatConsumeResult, OpenRouterFailure> {
        if self.done {
            return Err(staged_invalid(OpenRouterStreamStage::AfterDone));
        }
        if data.trim() == "[DONE]" {
            self.done = true;
            return Ok(ChatConsumeResult {
                events: Vec::new(),
                compatibility_stage: None,
            });
        }

        let envelope: Value = serde_json::from_str(data)
            .map_err(|_| staged_invalid(OpenRouterStreamStage::ChunkJson))?;
        let object = envelope
            .as_object()
            .ok_or_else(|| staged_invalid(OpenRouterStreamStage::ChunkJson))?;

        if let Some(error) = object.get("error").filter(|error| !error.is_null()) {
            let error = error
                .as_object()
                .ok_or_else(|| staged_invalid(OpenRouterStreamStage::ProviderErrorShape))?;
            return Err(stream_failure(error));
        }

        let semantic = match object.get("choices") {
            None | Some(Value::Null) => false,
            Some(Value::Array(choices)) if choices.is_empty() => false,
            Some(Value::Array(choices)) => {
                if choices.len() != 1 {
                    return Err(staged_invalid(OpenRouterStreamStage::ChoiceCardinality));
                }
                true
            }
            Some(_) => return Err(staged_invalid(OpenRouterStreamStage::CompletionShape)),
        };

        let mut events = Vec::new();
        if semantic {
            let chunk: ChatChunk = serde_json::from_value(envelope.clone())
                .map_err(|_| staged_invalid(OpenRouterStreamStage::CompletionShape))?;
            let ChatChunk {
                id,
                model,
                mut choices,
            } = chunk;
            if choices.len() != 1 {
                return Err(staged_invalid(OpenRouterStreamStage::ChoiceCardinality));
            }
            let choice = choices
                .pop()
                .ok_or_else(|| staged_invalid(OpenRouterStreamStage::ChoiceCardinality))?;
            if choice.index != 0 {
                return Err(staged_invalid(OpenRouterStreamStage::ChoiceIndex));
            }

            let mut response_id = self.response_id.clone();
            if let Some(id) = id {
                if id.is_empty() || response_id.as_ref().is_some_and(|known| known != &id) {
                    return Err(staged_invalid(OpenRouterStreamStage::ResponseId));
                }
                response_id.get_or_insert(id);
            }

            let mut stream_model = self.stream_model.clone();
            if let Some(model) = model {
                if model.is_empty()
                    || stream_model
                        .as_ref()
                        .is_some_and(|established| established != &model)
                {
                    return Err(staged_invalid(OpenRouterStreamStage::Model));
                }
                stream_model.get_or_insert(model);
            }

            let content = choice.delta.content.unwrap_or_default();
            if choice.finish_reason.as_deref() == Some("error") {
                return Err(OpenRouterFailure::new(OpenRouterFailureCategory::Remote));
            }
            if self.terminal_choice && !content.is_empty() {
                return Err(staged_invalid(OpenRouterStreamStage::PostTerminal));
            }
            if self.assistant.len().saturating_add(content.len()) > MAX_ASSISTANT_BYTES {
                return Err(staged_limit(OpenRouterStreamStage::AssistantLimit));
            }

            self.response_id = response_id;
            self.stream_model = stream_model;
            if !content.is_empty() {
                self.assistant.push_str(&content);
                events.push(ChatStreamEvent::TextDelta(content));
            }
            if choice.finish_reason.is_some() {
                self.terminal_choice = true;
            }
        }

        let compatibility_stage = self.consume_usage(object.get("usage"), &mut events);
        Ok(ChatConsumeResult {
            events,
            compatibility_stage,
        })
    }

    fn consume_usage(
        &mut self,
        raw: Option<&Value>,
        events: &mut Vec<ChatStreamEvent>,
    ) -> Option<OpenRouterStreamStage> {
        let raw = raw.filter(|usage| !usage.is_null())?;
        let parsed = raw
            .is_object()
            .then(|| serde_json::from_value::<TokenUsage>(raw.clone()))
            .and_then(Result::ok);
        if let Some(usage) = parsed {
            self.usage = Some(usage);
            events.push(ChatStreamEvent::Usage(usage));
            return None;
        }
        if self.usage_drop_reported {
            None
        } else {
            self.usage_drop_reported = true;
            Some(OpenRouterStreamStage::UsageDropped)
        }
    }

    pub(crate) fn is_done(&self) -> bool {
        self.done
    }

    pub(crate) fn finish(self) -> Result<(String, Option<TokenUsage>), OpenRouterFailure> {
        if !self.done && !self.terminal_choice {
            return Err(staged_invalid(OpenRouterStreamStage::PrematureEof));
        }
        Ok((self.assistant, self.usage))
    }
}

fn stream_failure(error: &Map<String, Value>) -> OpenRouterFailure {
    let status = error.get("code").and_then(stream_error_status);
    let code_category = error
        .get("code")
        .and_then(Value::as_str)
        .filter(|code| code.parse::<u16>().is_err())
        .and_then(stream_error_category);
    let metadata_category = error
        .get("metadata")
        .and_then(Value::as_object)
        .and_then(|metadata| metadata.get("error_type"))
        .and_then(Value::as_str)
        .and_then(stream_error_category);
    let category = status
        .map(status_category)
        .or(code_category)
        .or(metadata_category)
        .unwrap_or(OpenRouterFailureCategory::Remote);
    status.map_or_else(
        || OpenRouterFailure::new(category),
        |status| OpenRouterFailure::with_status(category, status),
    )
}

fn stream_error_status(value: &Value) -> Option<u16> {
    value
        .as_u64()
        .and_then(|status| u16::try_from(status).ok())
        .or_else(|| value.as_str()?.parse::<u16>().ok())
}

fn status_category(status: u16) -> OpenRouterFailureCategory {
    match status {
        401 | 403 => OpenRouterFailureCategory::Unauthorized,
        408 | 504 => OpenRouterFailureCategory::Timeout,
        429 => OpenRouterFailureCategory::RateLimited,
        _ => OpenRouterFailureCategory::Remote,
    }
}

fn stream_error_category(value: &str) -> Option<OpenRouterFailureCategory> {
    match value {
        "authentication" | "invalid_api_key" | "unauthorized" => {
            Some(OpenRouterFailureCategory::Unauthorized)
        }
        "rate_limit_exceeded" | "rate_limited" => Some(OpenRouterFailureCategory::RateLimited),
        "timeout" | "provider_timeout" => Some(OpenRouterFailureCategory::Timeout),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decoder_handles_fragmentation_crlf_and_multiline_data() {
        let mut decoder = SseDecoder::new();
        assert!(decoder.push(b"da").unwrap().is_empty());
        assert!(decoder
            .push(b"ta: {\"a\":\r\ndata: 1}\r\n\r\n")
            .unwrap()
            .contains(&"{\"a\":\n1}".to_owned()));
    }

    #[test]
    fn decoder_accepts_a_large_transport_chunk_of_small_events() {
        let event = b"data: {}\n\n";
        let count = MAX_SSE_EVENT_BYTES / event.len() + 100;
        let chunk = event.repeat(count);
        assert!(chunk.len() > MAX_SSE_EVENT_BYTES);
        let mut decoder = SseDecoder::new();
        let events = decoder.push(&chunk).unwrap();
        assert_eq!(events.len(), count);
        assert!(events.iter().all(|event| event == "{}"));
    }

    #[test]
    fn decoder_rejects_an_oversize_line_without_unbounded_growth() {
        let mut decoder = SseDecoder::new();
        let oversized = vec![b'x'; MAX_SSE_EVENT_BYTES + 1];
        let error = decoder.push(&oversized).unwrap_err();
        assert_eq!(error.category(), OpenRouterFailureCategory::ResourceLimit);
        assert_eq!(error.stage(), Some(OpenRouterStreamStage::SseFrameLimit));
    }

    #[test]
    fn decoder_rejects_invalid_data_utf8_with_a_static_stage() {
        let mut decoder = SseDecoder::new();
        let error = decoder.push(b"data: \xff\n\n").unwrap_err();
        assert_eq!(error.category(), OpenRouterFailureCategory::InvalidResponse);
        assert_eq!(error.stage(), Some(OpenRouterStreamStage::SseUtf8));
    }

    #[test]
    fn accumulator_accepts_finish_then_usage_and_eof() {
        let mut state = ChatAccumulator::new();
        let first = state
            .consume(
                r#"{"id":"x","model":"vendor/model","choices":[{"index":0,"delta":{"content":"ok"},"finish_reason":"stop"}]}"#,
            )
            .unwrap();
        assert_eq!(
            first.events,
            vec![ChatStreamEvent::TextDelta("ok".to_owned())]
        );
        let usage = state
            .consume(r#"{"id":"x","model":"vendor/model","choices":[],"usage":{"total_tokens":3}}"#)
            .unwrap();
        assert_eq!(
            usage.events,
            vec![ChatStreamEvent::Usage(TokenUsage {
                total_tokens: 3,
                ..TokenUsage::default()
            })]
        );
        assert_eq!(
            state.finish().unwrap(),
            (
                "ok".to_owned(),
                Some(TokenUsage {
                    total_tokens: 3,
                    ..TokenUsage::default()
                })
            )
        );
    }

    #[test]
    fn provider_error_precedes_malformed_completion_and_usage_siblings() {
        for payload in [
            r#"{"error":{"code":429,"message":"SECRET-REMOTE"},"choices":null,"usage":{"total_tokens":"bad"}}"#,
            r#"{"error":{"code":"429","metadata":{"error_type":"rate_limit_exceeded"}},"choices":[{"index":0,"delta":null}]}"#,
            r#"{"error":{"code":429,"message":"SECRET-CONFLICT","metadata":{"error_type":"authentication"}},"choices":null}"#,
            r#"{"error":{"code":"429","message":"SECRET-CONFLICT","metadata":{"error_type":"authentication"}},"choices":[]}"#,
        ] {
            let error = ChatAccumulator::new().consume(payload).unwrap_err();
            assert_eq!(error.category(), OpenRouterFailureCategory::RateLimited);
            assert_eq!(error.status(), Some(429));
            assert_eq!(error.stage(), None);
            let debug = format!("{error:?}");
            let display = error.to_string();
            for secret in ["SECRET-REMOTE", "SECRET-CONFLICT"] {
                assert!(!debug.contains(secret));
                assert!(!display.contains(secret));
            }
        }

        let error = ChatAccumulator::new()
            .consume(r#"{"error":[],"choices":[]}"#)
            .unwrap_err();
        assert_eq!(
            error.stage(),
            Some(OpenRouterStreamStage::ProviderErrorShape)
        );
    }

    #[test]
    fn semantic_model_is_established_by_the_server_not_the_request() {
        let mut state = ChatAccumulator::new();
        state
            .consume(r#"{"id":"x","model":"vendor/resolved-v2","choices":[{"index":0,"delta":{"content":"ok"}}]}"#)
            .unwrap();
        state
            .consume(r#"{"id":"metadata-only","model":"vendor/other-metadata","choices":[],"usage":null}"#)
            .unwrap();
        state
            .consume(r#"{"id":"x","model":"vendor/resolved-v2","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}"#)
            .unwrap();
        assert_eq!(state.finish().unwrap().0, "ok");

        let mut conflicting = ChatAccumulator::new();
        conflicting
            .consume(r#"{"model":"vendor/resolved-v1","choices":[{"delta":{"content":"kept"}}]}"#)
            .unwrap();
        let error = conflicting
            .consume(
                r#"{"model":"vendor/resolved-v2","choices":[{"delta":{"content":"discarded"}}]}"#,
            )
            .unwrap_err();
        assert_eq!(error.stage(), Some(OpenRouterStreamStage::Model));
        conflicting
            .consume(
                r#"{"model":"vendor/resolved-v1","choices":[{"delta":{},"finish_reason":"stop"}]}"#,
            )
            .unwrap();
        assert_eq!(conflicting.finish().unwrap().0, "kept");
    }

    #[test]
    fn metadata_choices_and_usage_are_independently_tolerant() {
        let mut state = ChatAccumulator::new();
        for payload in [
            r#"{"id":"ignored-a","model":"ignored-a"}"#,
            r#"{"id":null,"model":null,"choices":null,"usage":null}"#,
            r#"{"id":"","model":"","choices":[],"usage":{"prompt_tokens":2}}"#,
        ] {
            state.consume(payload).unwrap();
        }
        let (_, usage) = state
            .consume("[DONE]")
            .and_then(|_| state.finish())
            .unwrap();
        assert_eq!(usage.unwrap().prompt_tokens, 2);
    }

    #[test]
    fn malformed_usage_is_dropped_atomically_once_and_preserves_prior_usage() {
        let mut state = ChatAccumulator::new();
        state
            .consume(r#"{"choices":[{"delta":{"content":"answer"},"finish_reason":"stop"}],"usage":{"prompt_tokens":1,"completion_tokens":2,"total_tokens":3}}"#)
            .unwrap();
        for (index, usage) in [
            r#"{"total_tokens":null}"#,
            r#"{"total_tokens":"3"}"#,
            r#"{"total_tokens":-1}"#,
            r#"{"total_tokens":1.5}"#,
            r#"{"total_tokens":18446744073709551616}"#,
            r#"[]"#,
        ]
        .into_iter()
        .enumerate()
        {
            let result = state
                .consume(&format!(r#"{{"choices":[],"usage":{usage}}}"#))
                .unwrap();
            assert!(result.events.is_empty());
            assert_eq!(
                result.compatibility_stage,
                (index == 0).then_some(OpenRouterStreamStage::UsageDropped)
            );
        }
        let (answer, usage) = state.finish().unwrap();
        assert_eq!(answer, "answer");
        assert_eq!(usage.unwrap().total_tokens, 3);
    }

    #[test]
    fn repeated_empty_non_error_finish_markers_are_idempotent_and_strict() {
        for repeated_reason in ["stop", "length"] {
            let mut state = ChatAccumulator::new();
            state
                .consume(
                    r#"{"id":"x","model":"m","choices":[{"index":0,"delta":{"content":"ok"},"finish_reason":"stop"}]}"#,
                )
                .unwrap();
            let duplicate = state
                .consume(&format!(
                    r#"{{"id":"x","model":"m","choices":[{{"index":0,"delta":{{}},"finish_reason":"{repeated_reason}"}}],"usage":{{"total_tokens":3}}}}"#
                ))
                .unwrap();
            assert_eq!(
                duplicate.events,
                vec![ChatStreamEvent::Usage(TokenUsage {
                    total_tokens: 3,
                    ..TokenUsage::default()
                })]
            );
            state.consume("[DONE]").unwrap();
            assert_eq!(
                state.finish().unwrap(),
                (
                    "ok".to_owned(),
                    Some(TokenUsage {
                        total_tokens: 3,
                        ..TokenUsage::default()
                    })
                )
            );
        }

        for (payload, stage) in [
            (
                r#"{"id":"other","model":"m","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}"#,
                OpenRouterStreamStage::ResponseId,
            ),
            (
                r#"{"id":"x","model":"other","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}"#,
                OpenRouterStreamStage::Model,
            ),
            (
                r#"{"id":"x","model":"m","choices":[{"index":0,"delta":{"content":"late"},"finish_reason":"length"}]}"#,
                OpenRouterStreamStage::PostTerminal,
            ),
        ] {
            let mut state = ChatAccumulator::new();
            state
                .consume(
                    r#"{"id":"x","model":"m","choices":[{"index":0,"delta":{"content":"ok"},"finish_reason":"stop"}]}"#,
                )
                .unwrap();
            assert_eq!(state.consume(payload).unwrap_err().stage(), Some(stage));
        }

        let mut bare_error = ChatAccumulator::new();
        bare_error
            .consume(
                r#"{"id":"x","model":"m","choices":[{"index":0,"delta":{"content":"ok"},"finish_reason":"stop"}]}"#,
            )
            .unwrap();
        let error = bare_error
            .consume(
                r#"{"id":"x","model":"m","choices":[{"index":0,"delta":{},"finish_reason":"error"}]}"#,
            )
            .unwrap_err();
        assert_eq!(error.category(), OpenRouterFailureCategory::Remote);
        assert_eq!(error.stage(), None);

        for payload in [
            r#"{"error":{"code":429},"choices":[{"index":0,"delta":{"content":"late"}}]}"#,
            r#"{"error":{"code":"429"},"choices":[{"index":0,"delta":null}]}"#,
        ] {
            let mut state = ChatAccumulator::new();
            state
                .consume(r#"{"choices":[{"delta":{},"finish_reason":"stop"}]}"#)
                .unwrap();
            let error = state.consume(payload).unwrap_err();
            assert_eq!(error.category(), OpenRouterFailureCategory::RateLimited);
            assert_eq!(error.status(), Some(429));
            assert_eq!(error.stage(), None);
        }

        let mut malformed_usage = ChatAccumulator::new();
        malformed_usage
            .consume(
                r#"{"id":"x","model":"m","choices":[{"index":0,"delta":{"content":"ok"},"finish_reason":"stop"}]}"#,
            )
            .unwrap();
        let duplicate = malformed_usage
            .consume(
                r#"{"id":"x","model":"m","choices":[{"index":0,"delta":{},"finish_reason":"length"}],"usage":{"total_tokens":"bad"}}"#,
            )
            .unwrap();
        assert!(duplicate.events.is_empty());
        assert_eq!(
            duplicate.compatibility_stage,
            Some(OpenRouterStreamStage::UsageDropped)
        );
        assert_eq!(malformed_usage.finish().unwrap(), ("ok".to_owned(), None));
    }

    #[test]
    fn completion_shape_and_invariant_failures_have_exact_stages() {
        for (payload, stage) in [
            ("not-json", OpenRouterStreamStage::ChunkJson),
            ("[]", OpenRouterStreamStage::ChunkJson),
            (r#"{"choices":{}}"#, OpenRouterStreamStage::CompletionShape),
            (
                r#"{"choices":[{},{}]}"#,
                OpenRouterStreamStage::ChoiceCardinality,
            ),
            (
                r#"{"choices":[{"delta":null}]}"#,
                OpenRouterStreamStage::CompletionShape,
            ),
            (
                r#"{"choices":[{"delta":{"content":7}}]}"#,
                OpenRouterStreamStage::CompletionShape,
            ),
            (
                r#"{"choices":[{"delta":{},"finish_reason":{}}]}"#,
                OpenRouterStreamStage::CompletionShape,
            ),
            (
                r#"{"choices":[{"index":1,"delta":{}}]}"#,
                OpenRouterStreamStage::ChoiceIndex,
            ),
            (
                r#"{"id":"","choices":[{"delta":{}}]}"#,
                OpenRouterStreamStage::ResponseId,
            ),
            (
                r#"{"model":"","choices":[{"delta":{}}]}"#,
                OpenRouterStreamStage::Model,
            ),
        ] {
            let error = ChatAccumulator::new().consume(payload).unwrap_err();
            assert_eq!(error.stage(), Some(stage), "payload: {payload}");
        }

        let mut terminal = ChatAccumulator::new();
        terminal
            .consume(r#"{"choices":[{"delta":{},"finish_reason":"stop"}]}"#)
            .unwrap();
        assert_eq!(
            terminal
                .consume(r#"{"choices":[{"delta":{"content":"late"}}]}"#)
                .unwrap_err()
                .stage(),
            Some(OpenRouterStreamStage::PostTerminal)
        );
        assert!(terminal
            .consume(r#"{"choices":[{"delta":{},"finish_reason":"stop"}]}"#)
            .unwrap()
            .events
            .is_empty());
        assert_eq!(
            ChatAccumulator::new().finish().unwrap_err().stage(),
            Some(OpenRouterStreamStage::PrematureEof)
        );

        let mut identity = ChatAccumulator::new();
        identity
            .consume(r#"{"id":"first","choices":[{"delta":{"content":"kept"}}]}"#)
            .unwrap();
        assert_eq!(
            identity
                .consume(r#"{"id":"second","choices":[{"delta":{"content":"discarded"}}]}"#)
                .unwrap_err()
                .stage(),
            Some(OpenRouterStreamStage::ResponseId)
        );
        identity
            .consume(r#"{"id":"first","choices":[{"delta":{},"finish_reason":"stop"}]}"#)
            .unwrap();
        assert_eq!(identity.finish().unwrap().0, "kept");

        let mut done = ChatAccumulator::new();
        done.consume("[DONE]").unwrap();
        assert_eq!(
            done.consume(r#"{"choices":[]}"#).unwrap_err().stage(),
            Some(OpenRouterStreamStage::AfterDone)
        );
    }

    #[test]
    fn reasoning_only_deltas_are_semantic_and_assistant_limit_is_staged() {
        let mut state = ChatAccumulator::new();
        assert!(state
            .consume(r#"{"model":"resolved","choices":[{"delta":{"reasoning":"private"}}]}"#)
            .unwrap()
            .events
            .is_empty());
        state
            .consume(r#"{"model":"resolved","choices":[{"delta":{"content":null},"finish_reason":"stop"}]}"#)
            .unwrap();
        assert_eq!(state.finish().unwrap().0, "");

        let content = "x".repeat(MAX_ASSISTANT_BYTES + 1);
        let payload = serde_json::json!({
            "choices": [{"delta": {"content": content}}]
        })
        .to_string();
        let error = ChatAccumulator::new().consume(&payload).unwrap_err();
        assert_eq!(error.category(), OpenRouterFailureCategory::ResourceLimit);
        assert_eq!(error.stage(), Some(OpenRouterStreamStage::AssistantLimit));
    }
}
