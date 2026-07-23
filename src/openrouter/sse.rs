use bytes::BytesMut;

use super::protocol::ChatChunk;
use super::types::{
    ChatStreamEvent, OpenRouterFailure, OpenRouterFailureCategory, TokenUsage, MAX_ASSISTANT_BYTES,
    MAX_SSE_EVENT_BYTES,
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
                return Err(limit());
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
            return Err(limit());
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
            .map_err(|_| OpenRouterFailure::new(OpenRouterFailureCategory::InvalidResponse))?;
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

fn limit() -> OpenRouterFailure {
    OpenRouterFailure::new(OpenRouterFailureCategory::ResourceLimit)
}

pub(crate) struct ChatAccumulator {
    expected_model: String,
    response_id: Option<String>,
    terminal_choice: bool,
    done: bool,
    assistant: String,
    usage: Option<TokenUsage>,
}

impl ChatAccumulator {
    pub(crate) fn new(expected_model: String) -> Self {
        Self {
            expected_model,
            response_id: None,
            terminal_choice: false,
            done: false,
            assistant: String::new(),
            usage: None,
        }
    }

    pub(crate) fn consume(
        &mut self,
        data: &str,
    ) -> Result<Vec<ChatStreamEvent>, OpenRouterFailure> {
        if self.done {
            return Err(invalid());
        }
        if data.trim() == "[DONE]" {
            self.done = true;
            return Ok(Vec::new());
        }
        let chunk: ChatChunk = serde_json::from_str(data).map_err(|_| invalid())?;
        if let Some(model) = chunk.model.as_deref() {
            if model != self.expected_model {
                return Err(invalid());
            }
        }
        if let Some(id) = chunk.id {
            if id.is_empty() || self.response_id.as_ref().is_some_and(|known| known != &id) {
                return Err(invalid());
            }
            self.response_id.get_or_insert(id);
        }
        if chunk.choices.len() > 1 {
            return Err(invalid());
        }

        let mut events = Vec::new();
        if let Some(choice) = chunk.choices.into_iter().next() {
            if choice.index != 0 {
                return Err(invalid());
            }
            let content = choice.delta.content.unwrap_or_default();
            if self.terminal_choice && (!content.is_empty() || choice.finish_reason.is_some()) {
                return Err(invalid());
            }
            if !content.is_empty() {
                if self.assistant.len().saturating_add(content.len()) > MAX_ASSISTANT_BYTES {
                    return Err(limit());
                }
                self.assistant.push_str(&content);
                events.push(ChatStreamEvent::TextDelta(content));
            }
            if choice.finish_reason.is_some() {
                self.terminal_choice = true;
            }
        }
        if let Some(usage) = chunk.usage {
            self.usage = Some(usage);
            events.push(ChatStreamEvent::Usage(usage));
        }
        Ok(events)
    }

    pub(crate) fn is_done(&self) -> bool {
        self.done
    }

    pub(crate) fn finish(self) -> Result<(String, Option<TokenUsage>), OpenRouterFailure> {
        if !self.done && !self.terminal_choice {
            return Err(invalid());
        }
        Ok((self.assistant, self.usage))
    }
}

fn invalid() -> OpenRouterFailure {
    OpenRouterFailure::new(OpenRouterFailureCategory::InvalidResponse)
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
        assert_eq!(
            decoder.push(&oversized).unwrap_err().category(),
            OpenRouterFailureCategory::ResourceLimit
        );
    }

    #[test]
    fn accumulator_accepts_finish_then_usage_and_eof() {
        let mut state = ChatAccumulator::new("vendor/model".to_owned());
        let first = state
            .consume(
                r#"{"id":"x","model":"vendor/model","choices":[{"index":0,"delta":{"content":"ok"},"finish_reason":"stop"}]}"#,
            )
            .unwrap();
        assert_eq!(first, vec![ChatStreamEvent::TextDelta("ok".to_owned())]);
        state
            .consume(r#"{"id":"x","model":"vendor/model","choices":[],"usage":{"total_tokens":3}}"#)
            .unwrap();
        assert_eq!(state.finish().unwrap().0, "ok");
    }

    #[test]
    fn accumulator_rejects_content_after_finish_and_unfinished_eof() {
        let mut state = ChatAccumulator::new("m".to_owned());
        state
            .consume(r#"{"choices":[{"delta":{},"finish_reason":"stop"}]}"#)
            .unwrap();
        assert_eq!(
            state
                .consume(r#"{"choices":[{"delta":{"content":"late"}}]}"#)
                .unwrap_err()
                .category(),
            OpenRouterFailureCategory::InvalidResponse
        );
        assert_eq!(
            ChatAccumulator::new("m".to_owned())
                .finish()
                .unwrap_err()
                .category(),
            OpenRouterFailureCategory::InvalidResponse
        );
    }
}
