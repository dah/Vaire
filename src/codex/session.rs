use std::collections::HashSet;

use serde::de::DeserializeOwned;
use serde_json::{json, Value};
use thiserror::Error;

use super::protocol::{
    parse_notification, valid_identifier, validate_thread_snapshot, validate_turn_snapshot,
    AccountReadResponse, CancelLoginAccountParams, CancelLoginAccountResponse,
    CancelLoginAccountStatus, InitializeParams, InitializeResponse, LoginAccountParams,
    LoginAccountResponse, LogoutAccountResponse, ModelInfo, ModelListParams, ModelListResponse,
    ProtocolEvent, ReasoningSummary, ThreadDeleteParams, ThreadDeleteResponse, ThreadItemContent,
    ThreadListEntry, ThreadListParams, ThreadListResponse, ThreadReadParams, ThreadResponse,
    ThreadResumeParams, ThreadSnapshot, ThreadSourceKind, ThreadStartParams, TurnInterruptParams,
    TurnInterruptResponse, TurnStartParams, TurnStartResponse, UserInput,
};
use super::safety::{FullAccessPolicy, IsolationPaths};
use super::transport::{AppServerTransport, TransportError};
use crate::app::{ModelChoice, ThreadChoice, TranscriptEntry, TranscriptRole};
use crate::persistence::AccountScope;

mod auth;
mod catalog;
mod events;
mod pagination;
mod presentation;
mod threads;
mod turns;
mod types;

pub(in crate::codex::session) use pagination::*;
pub use presentation::{history_entries, model_choices, thread_choices};
pub use types::{
    AccountState, DeviceLoginChallenge, LoginChallenge, SessionError, SessionEvent, SessionService,
};

#[cfg(test)]
mod tests;
