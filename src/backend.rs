//! Testable backend coordinator shared by the background runtime and integration tests.

use std::collections::{HashSet, VecDeque};

use thiserror::Error;

use crate::app::{
    Action, AppState, DomainEvent, Effect, Intent, ThinkingKind, ThreadDeletionFailure,
    ThreadState, TurnOutcome,
};
use crate::claude::{
    ClaudeAuthStatus, ClaudeCliPolicy, ClaudeError, ClaudeFailureCategory, ClaudeFailureStage,
    ClaudeService, ClaudeServiceEvent,
};
use crate::codex::protocol::CancelLoginAccountStatus;
use crate::codex::protocol::{ProtocolEvent, TurnStatus};
use crate::codex::session::{
    history_entries, model_choices, thread_choices, AccountState, SessionError, SessionEvent,
    SessionService,
};
use crate::codex::transport::TransportError;
use crate::credentials::{CredentialAccount, CredentialStore};
use crate::openrouter::{
    OpenRouterAuthStatus, OpenRouterConversationV2, OpenRouterFailureCategory, OpenRouterService,
    OpenRouterServiceEvent, OpenRouterTurnOutcome,
};
use crate::persistence::{LoadNotice, PersistenceError, PreferencesPort};
use crate::platform::{BrowserError, BrowserOpener};
use crate::provider::{ModelKey, ProviderId};

mod claude_runtime;
mod effects;
mod helpers;
mod lifecycle;
mod protocol_events;
mod thread_ops;
mod types;

pub(in crate::backend) use helpers::{is_fatal_transport, load_notice_message};
pub use types::{BackendCoordinator, BackendError, BackendRuntimeEvent, ClaudeBackendRuntime};
pub(in crate::backend) use types::{CompletedItemTracker, PendingOpenRouterAutoResume};
#[cfg(test)]
pub(in crate::backend) use types::{
    MAX_TRACKED_COMPLETED_ITEMS_PER_TURN, MAX_TRACKED_COMPLETED_ITEM_ID_BYTES,
};

#[cfg(test)]
mod tests;
