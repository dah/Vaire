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
mod tests;
