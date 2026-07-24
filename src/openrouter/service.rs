use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::credentials::{CredentialAccount, CredentialStore, SecretValue};
use crate::provider::{OpenRouterConversationId, OpenRouterTurnId};

use super::types::MAX_ASSISTANT_BYTES;
use super::{
    ChatRequest, ChatStreamEvent, OpenRouterClient, OpenRouterConversationStore,
    OpenRouterConversationSummary, OpenRouterConversationV2, OpenRouterFailure,
    OpenRouterFailureCategory, OpenRouterModel, OpenRouterStoreError,
    OpenRouterStoreFailureCategory, OpenRouterStreamStage, OpenRouterTurnOutcome,
    OpenRouterTurnRecord, TokenUsage,
};

const EVENT_QUEUE: usize = 64;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OpenRouterAuthStatus {
    Missing,
    Unverified,
    Valid,
    Invalid,
    CredentialUnavailable,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OpenRouterServiceStart {
    pub auth: OpenRouterAuthStatus,
    pub catalog: Vec<OpenRouterModel>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum OpenRouterServiceEvent {
    AuthValidated {
        operation_id: u64,
    },
    LoginSucceeded {
        operation_id: u64,
        catalog: Vec<OpenRouterModel>,
    },
    LoginFailed {
        operation_id: u64,
        category: OpenRouterFailureCategory,
    },
    CatalogLoaded {
        operation_id: u64,
        catalog: Vec<OpenRouterModel>,
    },
    CatalogFailed {
        operation_id: u64,
        category: OpenRouterFailureCategory,
    },
    TurnStarted {
        conversation_id: OpenRouterConversationId,
        turn_id: OpenRouterTurnId,
    },
    TextDelta {
        conversation_id: OpenRouterConversationId,
        turn_id: OpenRouterTurnId,
        delta: String,
    },
    Usage {
        conversation_id: OpenRouterConversationId,
        turn_id: OpenRouterTurnId,
        usage: TokenUsage,
    },
    TurnFinished {
        conversation_id: OpenRouterConversationId,
        turn_id: OpenRouterTurnId,
        outcome: OpenRouterTurnOutcome,
        assistant_text: Option<String>,
        incomplete_assistant_text: Option<String>,
        usage: Option<TokenUsage>,
        failure: Option<OpenRouterFailureCategory>,
        failure_stage: Option<OpenRouterStreamStage>,
    },
}

/// Owns the bounded OpenRouter control/chat tasks and emits only secret-free domain data.
pub struct OpenRouterService {
    client: OpenRouterClient,
    credentials: Arc<dyn CredentialStore>,
    store: Arc<dyn OpenRouterConversationStore>,
    control_events_tx: mpsc::Sender<OpenRouterServiceEvent>,
    control_events_rx: mpsc::Receiver<OpenRouterServiceEvent>,
    chat_events_tx: mpsc::Sender<OpenRouterServiceEvent>,
    chat_events_rx: mpsc::Receiver<OpenRouterServiceEvent>,
    prefer_chat_event: bool,
    next_control_operation_id: u64,
    control_cancel: Option<CancellationToken>,
    control_task: Option<JoinHandle<()>>,
    chat_cancel: Option<CancellationToken>,
    chat_task: Option<JoinHandle<()>>,
}

/// A durably-created local turn whose HTTP request has not started yet.
///
/// Keeping this type opaque outside the OpenRouter layer makes the backend
/// explicitly acknowledge and persist the conversation pointer before launch.
pub struct PreparedOpenRouterTurn {
    conversation_id: OpenRouterConversationId,
    turn_id: OpenRouterTurnId,
    conversation: OpenRouterConversationV2,
    request: ChatRequest,
}

impl PreparedOpenRouterTurn {
    pub fn conversation_id(&self) -> &OpenRouterConversationId {
        &self.conversation_id
    }

    pub fn turn_id(&self) -> &OpenRouterTurnId {
        &self.turn_id
    }
}

impl OpenRouterService {
    pub fn new(
        client: OpenRouterClient,
        credentials: Arc<dyn CredentialStore>,
        store: Arc<dyn OpenRouterConversationStore>,
    ) -> Self {
        let (control_events_tx, control_events_rx) = mpsc::channel(EVENT_QUEUE);
        let (chat_events_tx, chat_events_rx) = mpsc::channel(EVENT_QUEUE);
        Self {
            client,
            credentials,
            store,
            control_events_tx,
            control_events_rx,
            chat_events_tx,
            chat_events_rx,
            prefer_chat_event: true,
            next_control_operation_id: 1,
            control_cancel: None,
            control_task: None,
            chat_cancel: None,
            chat_task: None,
        }
    }
}

mod chat;
mod control;
mod conversations;
mod lifecycle;

use conversations::now_ms;
#[cfg(test)]
use conversations::title_for;

#[cfg(test)]
mod tests;
