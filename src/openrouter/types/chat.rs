use serde::{Deserialize, Serialize};

use crate::provider::{ModelKey, ProviderId};

use super::{OpenRouterFailure, OpenRouterFailureCategory, MAX_OUTBOUND_CHAT_BYTES};

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ChatRole {
    User,
    Assistant,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ChatMessage {
    pub role: ChatRole,
    pub content: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChatRequest {
    model_id: String,
    messages: Vec<ChatMessage>,
}

impl ChatRequest {
    pub fn new(
        model_id: impl Into<String>,
        messages: Vec<ChatMessage>,
    ) -> Result<Self, OpenRouterFailure> {
        let model_id = model_id.into();
        ModelKey::new(ProviderId::OpenRouter, model_id.clone())
            .map_err(|_| OpenRouterFailure::new(OpenRouterFailureCategory::InvalidRequest))?;
        if messages.is_empty()
            || messages.iter().any(|message| message.content.is_empty())
            || messages
                .last()
                .is_none_or(|message| message.role != ChatRole::User)
        {
            return Err(OpenRouterFailure::new(
                OpenRouterFailureCategory::InvalidRequest,
            ));
        }
        let request = Self { model_id, messages };
        let encoded = serde_json::to_vec(&request.to_wire())
            .map_err(|_| OpenRouterFailure::new(OpenRouterFailureCategory::InvalidRequest))?;
        if encoded.len() > MAX_OUTBOUND_CHAT_BYTES {
            return Err(OpenRouterFailure::new(
                OpenRouterFailureCategory::ResourceLimit,
            ));
        }
        Ok(request)
    }

    pub fn model_id(&self) -> &str {
        &self.model_id
    }

    pub fn messages(&self) -> &[ChatMessage] {
        &self.messages
    }

    pub(crate) fn to_wire(&self) -> ChatRequestWire<'_> {
        ChatRequestWire {
            model: &self.model_id,
            messages: &self.messages,
            stream: true,
            stream_options: StreamOptions {
                include_usage: true,
            },
        }
    }
}

#[derive(Serialize)]
pub(crate) struct ChatRequestWire<'a> {
    model: &'a str,
    messages: &'a [ChatMessage],
    stream: bool,
    stream_options: StreamOptions,
}

#[derive(Serialize)]
pub(crate) struct StreamOptions {
    include_usage: bool,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct TokenUsage {
    #[serde(default)]
    pub prompt_tokens: u64,
    #[serde(default)]
    pub completion_tokens: u64,
    #[serde(default)]
    pub total_tokens: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ChatStreamEvent {
    TextDelta(String),
    Usage(TokenUsage),
    Finished {
        assistant_text: String,
        usage: Option<TokenUsage>,
    },
}
