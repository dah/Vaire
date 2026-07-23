use std::fmt;

use serde::{Deserialize, Deserializer, Serialize};
use thiserror::Error;

use crate::provider::{ModelKey, OpenRouterConversationId, OpenRouterTurnId, ProviderId};
use crate::text::sanitize_terminal_text;

pub const MAX_CATALOG_BODY_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_CATALOG_MODELS: usize = 10_000;
pub const MAX_CATALOG_TEXT_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_MODEL_ID_BYTES: usize = 512;
pub const MAX_MODEL_NAME_BYTES: usize = 1024;
pub const MAX_OUTBOUND_CHAT_BYTES: usize = 1024 * 1024;
pub const MAX_SSE_EVENT_BYTES: usize = 256 * 1024;
pub const MAX_ASSISTANT_BYTES: usize = 1024 * 1024;
pub const MAX_ERROR_BODY_BYTES: usize = 16 * 1024;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct OpenRouterModel {
    pub id: String,
    pub name: Option<String>,
    pub context_length: Option<u64>,
}

impl OpenRouterModel {
    pub(crate) fn validate(&self) -> bool {
        ModelKey::new(ProviderId::OpenRouter, self.id.clone()).is_ok()
            && self.id.len() <= MAX_MODEL_ID_BYTES
            && self.name.as_ref().is_none_or(|name| {
                name.len() <= MAX_MODEL_NAME_BYTES
                    && sanitize_terminal_text(name) == *name
                    && !name.contains(['\n', '\r'])
            })
            && self.context_length.is_none_or(|length| length > 0)
    }
}

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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OpenRouterFailureCategory {
    MissingCredential,
    CredentialStore,
    InvalidRequest,
    Unauthorized,
    RateLimited,
    Network,
    Timeout,
    Cancelled,
    InvalidResponse,
    ResourceLimit,
    Remote,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OpenRouterStreamStage {
    ContentType,
    SseFrameLimit,
    SseUtf8,
    ChunkJson,
    ProviderErrorShape,
    CompletionShape,
    ChoiceCardinality,
    ChoiceIndex,
    ResponseId,
    Model,
    PostTerminal,
    AfterDone,
    PrematureEof,
    AssistantLimit,
    UsageDropped,
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub struct OpenRouterFailure {
    category: OpenRouterFailureCategory,
    status: Option<u16>,
    stage: Option<OpenRouterStreamStage>,
}

impl OpenRouterFailure {
    pub fn new(category: OpenRouterFailureCategory) -> Self {
        Self {
            category,
            status: None,
            stage: None,
        }
    }

    pub(crate) fn with_status(category: OpenRouterFailureCategory, status: u16) -> Self {
        Self {
            category,
            status: Some(status),
            stage: None,
        }
    }

    pub(crate) fn at_stage(mut self, stage: OpenRouterStreamStage) -> Self {
        self.stage = Some(stage);
        self
    }

    pub fn category(self) -> OpenRouterFailureCategory {
        self.category
    }

    pub fn status(self) -> Option<u16> {
        self.status
    }

    pub fn stage(self) -> Option<OpenRouterStreamStage> {
        self.stage
    }
}

impl fmt::Debug for OpenRouterFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OpenRouterFailure")
            .field("category", &self.category)
            .field("status", &self.status)
            .field("stage", &self.stage)
            .finish()
    }
}

impl fmt::Display for OpenRouterFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "OpenRouter operation failed ({:?})",
            self.category
        )?;
        if let Some(stage) = self.stage {
            write!(formatter, " at stream stage {stage:?}")?;
        }
        Ok(())
    }
}

impl std::error::Error for OpenRouterFailure {}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OpenRouterTurnOutcome {
    InProgress,
    Completed,
    Interrupted,
    Failed,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct OpenRouterTurnRecord {
    pub id: OpenRouterTurnId,
    pub model_id: String,
    pub user_text: String,
    #[serde(deserialize_with = "deserialize_required_nullable_string")]
    pub assistant_text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub incomplete_assistant_text: Option<String>,
    pub outcome: OpenRouterTurnOutcome,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct OpenRouterConversationV2 {
    pub version: u32,
    pub id: OpenRouterConversationId,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
    pub title: String,
    pub turns: Vec<OpenRouterTurnRecord>,
}

impl OpenRouterConversationV2 {
    pub fn new(id: OpenRouterConversationId, now_ms: u64, title: impl Into<String>) -> Self {
        Self {
            version: 2,
            id,
            created_at_ms: now_ms,
            updated_at_ms: now_ms,
            title: title.into(),
            turns: Vec::new(),
        }
    }

    /// Returns every user message and only successfully completed assistant messages.
    pub fn canonical_messages(&self) -> Vec<ChatMessage> {
        let mut messages = Vec::new();
        for turn in &self.turns {
            messages.push(ChatMessage {
                role: ChatRole::User,
                content: turn.user_text.clone(),
            });
            if turn.outcome == OpenRouterTurnOutcome::Completed {
                if let Some(assistant) = &turn.assistant_text {
                    messages.push(ChatMessage {
                        role: ChatRole::Assistant,
                        content: assistant.clone(),
                    });
                }
            }
        }
        messages
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub(super) struct OpenRouterTurnRecordV1 {
    pub id: OpenRouterTurnId,
    pub model_id: String,
    pub user_text: String,
    #[serde(deserialize_with = "deserialize_required_nullable_string")]
    pub assistant_text: Option<String>,
    pub outcome: OpenRouterTurnOutcome,
}

fn deserialize_required_nullable_string<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: Deserializer<'de>,
{
    Option::<String>::deserialize(deserializer)
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub(super) struct OpenRouterConversationV1 {
    pub version: u32,
    pub id: OpenRouterConversationId,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
    pub title: String,
    pub turns: Vec<OpenRouterTurnRecordV1>,
}

impl From<OpenRouterConversationV1> for OpenRouterConversationV2 {
    fn from(legacy: OpenRouterConversationV1) -> Self {
        Self {
            version: 2,
            id: legacy.id,
            created_at_ms: legacy.created_at_ms,
            updated_at_ms: legacy.updated_at_ms,
            title: legacy.title,
            turns: legacy
                .turns
                .into_iter()
                .map(|turn| OpenRouterTurnRecord {
                    id: turn.id,
                    model_id: turn.model_id,
                    user_text: turn.user_text,
                    assistant_text: turn.assistant_text,
                    incomplete_assistant_text: None,
                    outcome: turn.outcome,
                })
                .collect(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct OpenRouterConversationSummary {
    pub id: OpenRouterConversationId,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
    pub title: String,
    pub turn_count: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OpenRouterStoreFailureCategory {
    Read,
    Write,
    Delete,
    Permissions,
    Corrupt,
    UnsupportedVersion,
    ResourceLimit,
    NotFound,
}

#[derive(Clone, Copy, Error, Eq, PartialEq)]
#[error("OpenRouter local storage failed ({category:?})")]
pub struct OpenRouterStoreError {
    category: OpenRouterStoreFailureCategory,
}

impl OpenRouterStoreError {
    pub fn new(category: OpenRouterStoreFailureCategory) -> Self {
        Self { category }
    }

    pub fn category(self) -> OpenRouterStoreFailureCategory {
        self.category
    }
}

impl fmt::Debug for OpenRouterStoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OpenRouterStoreError")
            .field("category", &self.category)
            .finish()
    }
}
