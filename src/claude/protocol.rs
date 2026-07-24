use serde_json::Value;
use thiserror::Error;

use crate::provider::ClaudeSessionId;

use super::ClaudeModelMetadata;

pub const MAX_STREAM_LINE_BYTES: usize = 1024 * 1024;
pub const MAX_STREAM_LINES: usize = 16_384;
pub const MAX_STREAM_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_STDERR_BYTES: usize = 1024 * 1024;
pub const MAX_UNKNOWN_EVENTS: usize = 1_024;
pub const MAX_ASSISTANT_BYTES: usize = 256 * 1024;
pub const EVENT_QUEUE_CAPACITY: usize = 64;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ClaudeStreamEvent {
    Initialized {
        session_id: ClaudeSessionId,
        model: ClaudeModelMetadata,
    },
    TextDelta {
        delta: String,
    },
    Terminal {
        success: bool,
        final_text: Option<String>,
    },
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum ClaudeProtocolError {
    #[error("Claude stream resource limit exceeded")]
    ResourceLimit,
    #[error("Claude stream contained malformed data")]
    Malformed,
    #[error("Claude stream violated event ordering")]
    Ordering,
    #[error("Claude stream ended without a terminal result")]
    UnexpectedEof,
    #[error("Claude stream final response contradicted streamed text")]
    ContradictoryFinal,
}

#[derive(Debug)]
pub struct ClaudeStreamParser {
    expected_session_id: ClaudeSessionId,
    initialized: bool,
    terminal: bool,
    lines: usize,
    bytes: usize,
    unknown: usize,
    assistant_text: String,
}

impl ClaudeStreamParser {
    pub fn new(expected_session_id: ClaudeSessionId) -> Self {
        Self {
            expected_session_id,
            initialized: false,
            terminal: false,
            lines: 0,
            bytes: 0,
            unknown: 0,
            assistant_text: String::new(),
        }
    }

    pub fn assistant_text(&self) -> &str {
        &self.assistant_text
    }

    pub fn is_terminal(&self) -> bool {
        self.terminal
    }

    pub fn parse_line(
        &mut self,
        line: &[u8],
    ) -> Result<Option<ClaudeStreamEvent>, ClaudeProtocolError> {
        if line.len() > MAX_STREAM_LINE_BYTES {
            return Err(ClaudeProtocolError::ResourceLimit);
        }
        self.lines = self.lines.saturating_add(1);
        self.bytes = self.bytes.saturating_add(line.len());
        if self.lines > MAX_STREAM_LINES || self.bytes > MAX_STREAM_BYTES {
            return Err(ClaudeProtocolError::ResourceLimit);
        }
        let line = trim_line_ending(line);
        if line.is_empty() {
            return Ok(None);
        }
        let value: Value =
            serde_json::from_slice(line).map_err(|_| ClaudeProtocolError::Malformed)?;
        let kind = value
            .get("type")
            .and_then(Value::as_str)
            .ok_or(ClaudeProtocolError::Malformed)?;
        match kind {
            "system" => self.parse_system(&value),
            "stream_event" => self.parse_stream_event(&value),
            "result" => self.parse_result(&value),
            // Full snapshots are semantic but not deltas. Correlate them and keep the terminal
            // result as the snapshot source of truth.
            "assistant" | "user" => self.parse_correlated_snapshot(&value),
            // Metadata-only lifecycle envelopes are bounded and may omit a session identifier.
            "tool" | "tool_result" | "rate_limit_event" | "prompt_suggestion" => {
                self.parse_correlated_metadata(&value)
            }
            _ => self.ignore_correlated_unknown(&value),
        }
    }

    pub fn finish_eof(&self) -> Result<(), ClaudeProtocolError> {
        if self.terminal {
            Ok(())
        } else {
            Err(ClaudeProtocolError::UnexpectedEof)
        }
    }

    fn parse_system(
        &mut self,
        value: &Value,
    ) -> Result<Option<ClaudeStreamEvent>, ClaudeProtocolError> {
        let subtype = value
            .get("subtype")
            .and_then(Value::as_str)
            .ok_or(ClaudeProtocolError::Malformed)?;
        if subtype != "init" {
            return self.ignore_correlated_unknown(value);
        }
        if self.initialized || self.terminal {
            return Err(ClaudeProtocolError::Ordering);
        }
        let session = required_session(value)?;
        if session != self.expected_session_id {
            return Err(ClaudeProtocolError::Ordering);
        }
        let model_id = value
            .get("model")
            .and_then(Value::as_str)
            .ok_or(ClaudeProtocolError::Malformed)?;
        validate_bounded_text(model_id, 512, false)?;
        let display_name = value
            .get("model_display_name")
            .and_then(Value::as_str)
            .map(|name| {
                validate_bounded_text(name, 1024, false)?;
                Ok(name.to_owned())
            })
            .transpose()?;
        self.initialized = true;
        Ok(Some(ClaudeStreamEvent::Initialized {
            session_id: session,
            model: ClaudeModelMetadata {
                id: model_id.to_owned(),
                display_name,
            },
        }))
    }

    fn parse_stream_event(
        &mut self,
        value: &Value,
    ) -> Result<Option<ClaudeStreamEvent>, ClaudeProtocolError> {
        if !self.initialized || self.terminal {
            return Err(ClaudeProtocolError::Ordering);
        }
        if required_session(value)? != self.expected_session_id {
            return Err(ClaudeProtocolError::Ordering);
        }
        let event = value
            .get("event")
            .and_then(Value::as_object)
            .ok_or(ClaudeProtocolError::Malformed)?;
        let kind = event
            .get("type")
            .and_then(Value::as_str)
            .ok_or(ClaudeProtocolError::Malformed)?;
        if kind != "content_block_delta" {
            return Ok(None);
        }
        let delta = event
            .get("delta")
            .and_then(Value::as_object)
            .ok_or(ClaudeProtocolError::Malformed)?;
        let delta_kind = delta
            .get("type")
            .and_then(Value::as_str)
            .ok_or(ClaudeProtocolError::Malformed)?;
        if delta_kind != "text_delta" {
            return Ok(None);
        }
        let text = delta
            .get("text")
            .and_then(Value::as_str)
            .ok_or(ClaudeProtocolError::Malformed)?;
        if text.is_empty() {
            return Ok(None);
        }
        if self.assistant_text.len().saturating_add(text.len()) > MAX_ASSISTANT_BYTES {
            return Err(ClaudeProtocolError::ResourceLimit);
        }
        self.assistant_text.push_str(text);
        Ok(Some(ClaudeStreamEvent::TextDelta {
            delta: text.to_owned(),
        }))
    }

    fn parse_result(
        &mut self,
        value: &Value,
    ) -> Result<Option<ClaudeStreamEvent>, ClaudeProtocolError> {
        if !self.initialized || self.terminal {
            return Err(ClaudeProtocolError::Ordering);
        }
        if required_session(value)? != self.expected_session_id {
            return Err(ClaudeProtocolError::Ordering);
        }
        let subtype = value
            .get("subtype")
            .and_then(Value::as_str)
            .ok_or(ClaudeProtocolError::Malformed)?;
        let is_error = value
            .get("is_error")
            .and_then(Value::as_bool)
            .ok_or(ClaudeProtocolError::Malformed)?;
        let success = subtype == "success" && !is_error;
        let final_text = value.get("result").and_then(Value::as_str);
        if let Some(final_text) = final_text {
            validate_bounded_text(final_text, MAX_ASSISTANT_BYTES, true)?;
            if success {
                if !final_text.starts_with(&self.assistant_text) {
                    return Err(ClaudeProtocolError::ContradictoryFinal);
                }
                self.assistant_text
                    .push_str(&final_text[self.assistant_text.len()..]);
            }
        } else if success {
            return Err(ClaudeProtocolError::Malformed);
        }
        self.terminal = true;
        Ok(Some(ClaudeStreamEvent::Terminal {
            success,
            final_text: success.then(|| self.assistant_text.clone()),
        }))
    }

    fn parse_correlated_snapshot(
        &self,
        value: &Value,
    ) -> Result<Option<ClaudeStreamEvent>, ClaudeProtocolError> {
        if !self.initialized
            || self.terminal
            || required_session(value)? != self.expected_session_id
        {
            return Err(ClaudeProtocolError::Ordering);
        }
        Ok(None)
    }

    fn parse_correlated_metadata(
        &self,
        value: &Value,
    ) -> Result<Option<ClaudeStreamEvent>, ClaudeProtocolError> {
        if !self.initialized || self.terminal {
            return Err(ClaudeProtocolError::Ordering);
        }
        if let Some(session) = value.get("session_id").and_then(Value::as_str) {
            let session = session
                .parse::<ClaudeSessionId>()
                .map_err(|_| ClaudeProtocolError::Malformed)?;
            if session != self.expected_session_id {
                return Err(ClaudeProtocolError::Ordering);
            }
        }
        Ok(None)
    }

    fn ignore_correlated_unknown(
        &mut self,
        value: &Value,
    ) -> Result<Option<ClaudeStreamEvent>, ClaudeProtocolError> {
        if !self.initialized || self.terminal {
            return Err(ClaudeProtocolError::Ordering);
        }
        if required_session(value)? != self.expected_session_id {
            return Err(ClaudeProtocolError::Ordering);
        }
        self.unknown = self.unknown.saturating_add(1);
        if self.unknown > MAX_UNKNOWN_EVENTS {
            Err(ClaudeProtocolError::ResourceLimit)
        } else {
            Ok(None)
        }
    }
}

fn required_session(value: &Value) -> Result<ClaudeSessionId, ClaudeProtocolError> {
    value
        .get("session_id")
        .and_then(Value::as_str)
        .ok_or(ClaudeProtocolError::Malformed)?
        .parse()
        .map_err(|_| ClaudeProtocolError::Malformed)
}

fn validate_bounded_text(
    value: &str,
    limit: usize,
    allow_empty: bool,
) -> Result<(), ClaudeProtocolError> {
    if (!allow_empty && value.is_empty())
        || value.len() > limit
        || value.chars().any(|character| character == '\0')
    {
        Err(ClaudeProtocolError::ResourceLimit)
    } else {
        Ok(())
    }
}

fn trim_line_ending(mut line: &[u8]) -> &[u8] {
    if line.last() == Some(&b'\n') {
        line = &line[..line.len() - 1];
    }
    if line.last() == Some(&b'\r') {
        line = &line[..line.len() - 1];
    }
    line
}
