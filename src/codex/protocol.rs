use serde::de::{IgnoredAny, MapAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;
use std::path::PathBuf;

use crate::text::is_terminal_unsafe;

mod catalog;
mod conversation;
mod events;
mod initialize_auth;
mod parser;
mod rpc;
mod validation;

pub use catalog::{ModelInfo, ModelListParams, ModelListResponse, ReasoningEffortOption};
pub use conversation::{
    EmptyObjectResponse, ReasoningSummary, SessionSource, SessionSourceName, ThreadDeleteParams,
    ThreadDeleteResponse, ThreadItem, ThreadItemContent, ThreadListEntry, ThreadListParams,
    ThreadListResponse, ThreadReadParams, ThreadResponse, ThreadResumeParams, ThreadSnapshot,
    ThreadSourceKind, ThreadStartParams, TurnError, TurnInterruptParams, TurnInterruptResponse,
    TurnSnapshot, TurnStartParams, TurnStartResponse, TurnStatus, UserInput,
};
pub use events::{
    AgentMessageDeltaNotification, ErrorNotification, ItemCompletedNotification, ProtocolEvent,
    ReasoningSummaryPartAddedNotification, ReasoningSummaryTextDeltaNotification,
    ReasoningTextDeltaNotification, ThreadTokenUsage, ThreadTokenUsageUpdatedNotification,
    TokenUsageBreakdown, TurnNotification,
};
pub use initialize_auth::{
    AccountInfo, AccountLoginCompletedNotification, AccountReadResponse, CancelLoginAccountParams,
    CancelLoginAccountResponse, CancelLoginAccountStatus, ClientInfo, InitializeCapabilities,
    InitializeParams, InitializeResponse, LoginAccountParams, LoginAccountResponse,
    LogoutAccountResponse,
};
pub use parser::parse_notification;
pub(in crate::codex::protocol) use rpc::MAX_PROTOCOL_IDENTIFIER_BYTES;
pub use rpc::{classify_message, InboundEvent, InboundMessage, RequestId, RpcErrorObject};
pub(in crate::codex) use validation::{
    valid_identifier, validate_thread_snapshot, validate_turn_snapshot,
};
pub(in crate::codex::protocol) use validation::{validate_scope, validate_thread_item};

#[cfg(test)]
mod tests;
