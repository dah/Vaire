use serde::{Deserialize, Deserializer, Serialize};

use crate::provider::{OpenRouterConversationId, OpenRouterTurnId};

use super::{ChatMessage, ChatRole};

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
pub(in crate::openrouter) struct OpenRouterTurnRecordV1 {
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
pub(in crate::openrouter) struct OpenRouterConversationV1 {
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
