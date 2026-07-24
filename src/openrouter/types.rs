mod catalog;
mod chat;
mod conversation;
mod failure;
mod limits;
mod store;

pub use catalog::OpenRouterModel;
pub use chat::{ChatMessage, ChatRequest, ChatRole, ChatStreamEvent, TokenUsage};
pub use conversation::{
    OpenRouterConversationSummary, OpenRouterConversationV2, OpenRouterTurnOutcome,
    OpenRouterTurnRecord,
};
pub use failure::{OpenRouterFailure, OpenRouterFailureCategory, OpenRouterStreamStage};
pub use store::{OpenRouterStoreError, OpenRouterStoreFailureCategory};

pub(in crate::openrouter) use conversation::OpenRouterConversationV1;
pub(in crate::openrouter) use limits::*;
