mod client;
mod protocol;
mod service;
mod sse;
mod store;
mod types;

pub use client::{OpenRouterClient, OpenRouterTimeouts};
pub use service::{
    OpenRouterAuthStatus, OpenRouterService, OpenRouterServiceEvent, OpenRouterServiceStart,
    PreparedOpenRouterTurn,
};
pub use store::{
    FileOpenRouterStore, OpenRouterConversationCommit, OpenRouterConversationStore,
    OpenRouterIndexMaintenance,
};
pub use types::{
    ChatMessage, ChatRequest, ChatRole, ChatStreamEvent, OpenRouterConversationSummary,
    OpenRouterConversationV1, OpenRouterFailure, OpenRouterFailureCategory, OpenRouterModel,
    OpenRouterStoreError, OpenRouterStoreFailureCategory, OpenRouterTurnOutcome,
    OpenRouterTurnRecord, TokenUsage,
};

#[cfg(test)]
mod tests;
