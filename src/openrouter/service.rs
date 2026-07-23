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
    OpenRouterFailureCategory, OpenRouterModel, OpenRouterStoreError, OpenRouterStreamStage,
    OpenRouterTurnOutcome, OpenRouterTurnRecord, TokenUsage,
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

    pub async fn startup(&self) -> OpenRouterServiceStart {
        let store = self.store.clone();
        let catalog = tokio::task::spawn_blocking(move || store.load_catalog())
            .await
            .ok()
            .and_then(Result::ok)
            .flatten()
            .map(|(_, catalog)| catalog)
            .unwrap_or_default();
        let credentials = self.credentials.clone();
        let configured = tokio::task::spawn_blocking(move || {
            credentials.load(CredentialAccount::OpenRouterApiKey)
        })
        .await;
        let auth = match configured {
            Ok(Ok(Some(_))) => OpenRouterAuthStatus::Unverified,
            Ok(Ok(None)) => OpenRouterAuthStatus::Missing,
            Ok(Err(_)) | Err(_) => OpenRouterAuthStatus::CredentialUnavailable,
        };
        OpenRouterServiceStart { auth, catalog }
    }

    pub fn revalidate_and_refresh(&mut self) -> Result<u64, &'static str> {
        self.start_control_task(None)
    }

    /// Validates before replacing the durable credential. The old credential is untouched on
    /// validation failure; after replacement, catalog refresh uses the newly stored value.
    pub fn replace_candidate(&mut self, candidate: SecretValue) -> Result<u64, SecretValue> {
        if self
            .chat_task
            .as_ref()
            .is_some_and(|task| !task.is_finished())
        {
            return Err(candidate);
        }
        if self
            .control_task
            .as_ref()
            .is_some_and(|task| !task.is_finished())
        {
            return Err(candidate);
        }
        self.reap_control();
        let operation_id = self.next_control_operation_id;
        let Some(next_operation_id) = operation_id.checked_add(1) else {
            return Err(candidate);
        };
        self.next_control_operation_id = next_operation_id;
        let cancel = CancellationToken::new();
        let task_cancel = cancel.clone();
        let client = self.client.clone();
        let credentials = self.credentials.clone();
        let store = self.store.clone();
        let events = self.control_events_tx.clone();
        self.control_task = Some(tokio::spawn(async move {
            let result = client
                .validate_candidate(&candidate, task_cancel.clone())
                .await;
            if let Err(error) = result {
                let _ = events
                    .send(OpenRouterServiceEvent::LoginFailed {
                        operation_id,
                        category: error.category(),
                    })
                    .await;
                return;
            }
            let replaced = tokio::task::spawn_blocking(move || {
                credentials.replace_with_commit(CredentialAccount::OpenRouterApiKey, candidate)
            })
            .await;
            if !matches!(replaced, Ok(Ok(_))) {
                let _ = events
                    .send(OpenRouterServiceEvent::LoginFailed {
                        operation_id,
                        category: OpenRouterFailureCategory::CredentialStore,
                    })
                    .await;
                return;
            }
            let _ = events
                .send(OpenRouterServiceEvent::AuthValidated { operation_id })
                .await;
            match client.fetch_catalog(task_cancel).await {
                Ok(catalog) => {
                    let saved_catalog = catalog.clone();
                    let saved = tokio::task::spawn_blocking(move || {
                        store.save_catalog_with_commit(now_ms(), &saved_catalog)
                    })
                    .await;
                    if !matches!(saved, Ok(Ok(_))) {
                        let _ = events
                            .send(OpenRouterServiceEvent::CatalogFailed {
                                operation_id,
                                category: OpenRouterFailureCategory::CredentialStore,
                            })
                            .await;
                        return;
                    }
                    let _ = events
                        .send(OpenRouterServiceEvent::LoginSucceeded {
                            operation_id,
                            catalog,
                        })
                        .await;
                }
                Err(error) => {
                    let _ = events
                        .send(OpenRouterServiceEvent::CatalogFailed {
                            operation_id,
                            category: error.category(),
                        })
                        .await;
                }
            }
        }));
        self.control_cancel = Some(cancel);
        Ok(operation_id)
    }

    fn start_control_task(&mut self, _unused: Option<SecretValue>) -> Result<u64, &'static str> {
        if self
            .control_task
            .as_ref()
            .is_some_and(|task| !task.is_finished())
        {
            return Err("an OpenRouter authentication operation is already active");
        }
        self.reap_control();
        let operation_id = self.next_control_operation_id;
        self.next_control_operation_id = operation_id
            .checked_add(1)
            .ok_or("OpenRouter authentication operation IDs are exhausted")?;
        let cancel = CancellationToken::new();
        let task_cancel = cancel.clone();
        let client = self.client.clone();
        let store = self.store.clone();
        let events = self.control_events_tx.clone();
        self.control_task = Some(tokio::spawn(async move {
            if let Err(error) = client.validate_stored_key(task_cancel.clone()).await {
                let _ = events
                    .send(OpenRouterServiceEvent::CatalogFailed {
                        operation_id,
                        category: error.category(),
                    })
                    .await;
                return;
            }
            let _ = events
                .send(OpenRouterServiceEvent::AuthValidated { operation_id })
                .await;
            match client.fetch_catalog(task_cancel).await {
                Ok(catalog) => {
                    let saved_catalog = catalog.clone();
                    let saved = tokio::task::spawn_blocking(move || {
                        store.save_catalog_with_commit(now_ms(), &saved_catalog)
                    })
                    .await;
                    if matches!(saved, Ok(Ok(_))) {
                        let _ = events
                            .send(OpenRouterServiceEvent::CatalogLoaded {
                                operation_id,
                                catalog,
                            })
                            .await;
                    } else {
                        let _ = events
                            .send(OpenRouterServiceEvent::CatalogFailed {
                                operation_id,
                                category: OpenRouterFailureCategory::CredentialStore,
                            })
                            .await;
                    }
                }
                Err(error) => {
                    let _ = events
                        .send(OpenRouterServiceEvent::CatalogFailed {
                            operation_id,
                            category: error.category(),
                        })
                        .await;
                }
            }
        }));
        self.control_cancel = Some(cancel);
        Ok(operation_id)
    }

    pub async fn list_conversations(
        &self,
    ) -> Result<Vec<OpenRouterConversationSummary>, OpenRouterStoreError> {
        let store = self.store.clone();
        tokio::task::spawn_blocking(move || store.list_conversations())
            .await
            .map_err(|_| {
                super::OpenRouterStoreError::new(super::OpenRouterStoreFailureCategory::Read)
            })?
    }

    pub async fn load_conversation(
        &self,
        id: OpenRouterConversationId,
    ) -> Result<OpenRouterConversationV2, OpenRouterStoreError> {
        let store = self.store.clone();
        tokio::task::spawn_blocking(move || store.load_conversation(&id))
            .await
            .map_err(|_| {
                super::OpenRouterStoreError::new(super::OpenRouterStoreFailureCategory::Read)
            })?
    }

    pub async fn create_conversation(
        &self,
    ) -> Result<OpenRouterConversationId, OpenRouterStoreError> {
        let store = self.store.clone();
        let id = OpenRouterConversationId::new();
        let saved_id = id.clone();
        tokio::task::spawn_blocking(move || {
            store.save_conversation_with_commit(&OpenRouterConversationV2::new(
                saved_id,
                now_ms(),
                "New conversation",
            ))
        })
        .await
        .map_err(|_| {
            super::OpenRouterStoreError::new(super::OpenRouterStoreFailureCategory::Write)
        })??;
        Ok(id)
    }

    pub async fn delete_conversation(
        &self,
        id: OpenRouterConversationId,
    ) -> Result<(), OpenRouterStoreError> {
        let store = self.store.clone();
        tokio::task::spawn_blocking(move || store.delete_conversation_with_commit(&id))
            .await
            .map_err(|_| {
                super::OpenRouterStoreError::new(super::OpenRouterStoreFailureCategory::Delete)
            })?
            .map(|_| ())
    }

    pub async fn logout(
        &mut self,
    ) -> (
        Vec<OpenRouterServiceEvent>,
        Result<(), crate::credentials::CredentialStoreError>,
    ) {
        if let Some(cancel) = self.control_cancel.take() {
            cancel.cancel();
        }
        self.join_control_draining().await;
        self.interrupt_turn();
        let drained = self.join_chat_draining().await;
        let credentials = self.credentials.clone();
        let result = tokio::task::spawn_blocking(move || {
            credentials.delete_with_commit(CredentialAccount::OpenRouterApiKey)
        })
        .await
        .map_err(|_| {
            crate::credentials::CredentialStoreError::new(
                crate::credentials::CredentialFailureCategory::Delete,
            )
        })
        .and_then(|result| result)
        .map(|_| ());
        (drained, result)
    }

    pub async fn prepare_turn(
        &mut self,
        conversation_id: Option<OpenRouterConversationId>,
        model_id: String,
        user_text: String,
    ) -> Result<PreparedOpenRouterTurn, OpenRouterFailure> {
        if self
            .chat_task
            .as_ref()
            .is_some_and(|task| !task.is_finished())
        {
            return Err(OpenRouterFailure::new(
                OpenRouterFailureCategory::InvalidRequest,
            ));
        }
        self.reap_chat();
        let conversation_id = conversation_id.unwrap_or_default();
        let turn_id = OpenRouterTurnId::new();
        let store = self.store.clone();
        let saved_id = conversation_id.clone();
        let saved_turn = turn_id.clone();
        let saved_model = model_id.clone();
        let saved_text = user_text.clone();
        let (conversation, request) = tokio::task::spawn_blocking(move || {
            let mut conversation = match store.load_conversation(&saved_id) {
                Ok(value) => value,
                Err(error)
                    if error.category() == super::OpenRouterStoreFailureCategory::NotFound =>
                {
                    OpenRouterConversationV2::new(
                        saved_id.clone(),
                        now_ms(),
                        title_for(&saved_text),
                    )
                }
                Err(error) => return Err(error),
            };
            conversation.updated_at_ms = now_ms();
            conversation.turns.push(OpenRouterTurnRecord {
                id: saved_turn,
                model_id: saved_model,
                user_text: saved_text,
                assistant_text: None,
                incomplete_assistant_text: None,
                outcome: OpenRouterTurnOutcome::InProgress,
            });
            let request = ChatRequest::new(
                saved_model_for_request(&conversation),
                conversation.canonical_messages(),
            )
            .map_err(|_| {
                super::OpenRouterStoreError::new(
                    super::OpenRouterStoreFailureCategory::ResourceLimit,
                )
            })?;
            store.save_conversation_with_commit(&conversation)?;
            Ok((conversation, request))
        })
        .await
        .map_err(|_| OpenRouterFailure::new(OpenRouterFailureCategory::CredentialStore))?
        .map_err(|_| OpenRouterFailure::new(OpenRouterFailureCategory::CredentialStore))?;
        Ok(PreparedOpenRouterTurn {
            conversation_id,
            turn_id,
            conversation,
            request,
        })
    }

    pub async fn abandon_prepared_turn(
        &self,
        mut prepared: PreparedOpenRouterTurn,
    ) -> Result<(), OpenRouterFailure> {
        if let Some(record) = prepared.conversation.turns.last_mut() {
            record.outcome = OpenRouterTurnOutcome::Failed;
            record.assistant_text = None;
            record.incomplete_assistant_text = None;
        }
        prepared.conversation.updated_at_ms = now_ms();
        let store = self.store.clone();
        tokio::task::spawn_blocking(move || {
            store.save_conversation_with_commit(&prepared.conversation)
        })
        .await
        .map_err(|_| OpenRouterFailure::new(OpenRouterFailureCategory::CredentialStore))?
        .map_err(|_| OpenRouterFailure::new(OpenRouterFailureCategory::CredentialStore))
        .map(|_| ())
    }

    pub fn launch_prepared_turn(&mut self, prepared: PreparedOpenRouterTurn) {
        let PreparedOpenRouterTurn {
            conversation_id,
            turn_id,
            mut conversation,
            request,
        } = prepared;

        let cancel = CancellationToken::new();
        let task_cancel = cancel.clone();
        let callback_cancel = cancel.clone();
        let client = self.client.clone();
        let store = self.store.clone();
        let events = self.chat_events_tx.clone();
        let task_conversation_id = conversation_id.clone();
        let task_turn_id = turn_id.clone();
        self.chat_task = Some(tokio::spawn(async move {
            let mut final_text = None;
            let mut final_usage = None;
            let mut streamed_text = String::new();
            let mut stream_bound_exceeded = false;
            let mut delivery_failed = false;
            let result = client
                .chat(&request, task_cancel, |event| match event {
                    ChatStreamEvent::TextDelta(delta) => {
                        if streamed_text
                            .len()
                            .checked_add(delta.len())
                            .is_none_or(|length| length > MAX_ASSISTANT_BYTES)
                        {
                            stream_bound_exceeded = true;
                            callback_cancel.cancel();
                            return;
                        }
                        streamed_text.push_str(&delta);
                        if events
                            .try_send(OpenRouterServiceEvent::TextDelta {
                                conversation_id: task_conversation_id.clone(),
                                turn_id: task_turn_id.clone(),
                                delta,
                            })
                            .is_err()
                        {
                            delivery_failed = true;
                            callback_cancel.cancel();
                        }
                    }
                    ChatStreamEvent::Usage(usage) => {
                        final_usage = Some(usage);
                        let _ = events.try_send(OpenRouterServiceEvent::Usage {
                            conversation_id: task_conversation_id.clone(),
                            turn_id: task_turn_id.clone(),
                            usage,
                        });
                    }
                    ChatStreamEvent::Finished {
                        assistant_text,
                        usage,
                    } => {
                        final_text = Some(assistant_text);
                        final_usage = usage.or(final_usage);
                    }
                })
                .await;
            let (outcome, failure, failure_stage) = match result {
                _ if delivery_failed => (OpenRouterTurnOutcome::Interrupted, None, None),
                _ if stream_bound_exceeded => (
                    OpenRouterTurnOutcome::Failed,
                    Some(OpenRouterFailureCategory::ResourceLimit),
                    None,
                ),
                Ok(()) => (OpenRouterTurnOutcome::Completed, None, None),
                Err(error) if error.category() == OpenRouterFailureCategory::Cancelled => {
                    (OpenRouterTurnOutcome::Interrupted, None, None)
                }
                Err(error) => (
                    OpenRouterTurnOutcome::Failed,
                    Some(error.category()),
                    error.stage(),
                ),
            };
            let assistant_text = (outcome == OpenRouterTurnOutcome::Completed)
                .then(|| final_text.take())
                .flatten();
            let incomplete_assistant_text = (outcome == OpenRouterTurnOutcome::Failed
                && !streamed_text.is_empty())
            .then_some(streamed_text);
            if let Some(record) = conversation.turns.last_mut() {
                record.outcome = outcome;
                record.assistant_text = assistant_text.clone();
                record.incomplete_assistant_text = incomplete_assistant_text.clone();
            }
            conversation.updated_at_ms = now_ms();
            let persisted = tokio::task::spawn_blocking(move || {
                store.save_conversation_with_commit(&conversation)
            })
            .await;
            let (outcome, failure, failure_stage, assistant_text, incomplete_assistant_text) =
                if matches!(persisted, Ok(Ok(_))) {
                    (
                        outcome,
                        failure,
                        failure_stage,
                        assistant_text,
                        incomplete_assistant_text,
                    )
                } else {
                    (
                        OpenRouterTurnOutcome::Failed,
                        Some(OpenRouterFailureCategory::CredentialStore),
                        None,
                        None,
                        None,
                    )
                };
            let _ = events
                .send(OpenRouterServiceEvent::TurnFinished {
                    conversation_id: task_conversation_id,
                    turn_id: task_turn_id,
                    outcome,
                    assistant_text,
                    incomplete_assistant_text,
                    usage: final_usage,
                    failure,
                    failure_stage,
                })
                .await;
        }));
        self.chat_cancel = Some(cancel);
    }

    #[cfg(test)]
    pub async fn start_turn(
        &mut self,
        conversation_id: Option<OpenRouterConversationId>,
        model_id: String,
        user_text: String,
    ) -> Result<(OpenRouterConversationId, OpenRouterTurnId), OpenRouterFailure> {
        let prepared = self
            .prepare_turn(conversation_id, model_id, user_text)
            .await?;
        let conversation_id = prepared.conversation_id().clone();
        let turn_id = prepared.turn_id().clone();
        let _ = self
            .chat_events_tx
            .send(OpenRouterServiceEvent::TurnStarted {
                conversation_id: conversation_id.clone(),
                turn_id: turn_id.clone(),
            })
            .await;
        self.launch_prepared_turn(prepared);
        Ok((conversation_id, turn_id))
    }

    pub fn interrupt_turn(&self) {
        if let Some(cancel) = &self.chat_cancel {
            cancel.cancel();
        }
    }

    pub async fn next_event(&mut self) -> Option<OpenRouterServiceEvent> {
        let event = if self.prefer_chat_event {
            tokio::select! {
                biased;
                event = self.chat_events_rx.recv() => event,
                event = self.control_events_rx.recv() => event,
            }
        } else {
            tokio::select! {
                biased;
                event = self.control_events_rx.recv() => event,
                event = self.chat_events_rx.recv() => event,
            }
        };
        self.prefer_chat_event = !self.prefer_chat_event;
        event
    }

    pub async fn shutdown(&mut self) -> Vec<OpenRouterServiceEvent> {
        if let Some(cancel) = self.control_cancel.take() {
            cancel.cancel();
        }
        self.interrupt_turn();
        self.join_control_draining().await;
        self.join_chat_draining().await
    }

    async fn join_control_draining(&mut self) {
        if let Some(mut task) = self.control_task.take() {
            loop {
                tokio::select! {
                    _ = &mut task => break,
                    _ = self.control_events_rx.recv() => {}
                }
            }
        }
        self.control_cancel = None;
    }

    async fn join_chat_draining(&mut self) -> Vec<OpenRouterServiceEvent> {
        let mut drained = Vec::new();
        if let Some(mut task) = self.chat_task.take() {
            loop {
                tokio::select! {
                    biased;
                    event = self.chat_events_rx.recv() => {
                        if let Some(event) = event {
                            drained.push(event);
                        }
                    }
                    _ = &mut task => break,
                }
            }
        }
        while let Ok(event) = self.chat_events_rx.try_recv() {
            drained.push(event);
        }
        self.chat_cancel = None;
        drained
    }

    fn reap_control(&mut self) {
        if self
            .control_task
            .as_ref()
            .is_some_and(JoinHandle::is_finished)
        {
            self.control_task = None;
            self.control_cancel = None;
        }
    }

    fn reap_chat(&mut self) {
        if self.chat_task.as_ref().is_some_and(JoinHandle::is_finished) {
            self.chat_task = None;
            self.chat_cancel = None;
        }
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

fn title_for(text: &str) -> String {
    const MAX_TITLE_BYTES: usize = 80;
    let sanitized = crate::text::sanitize_terminal_text(text);
    let mut title = String::new();
    for character in sanitized.chars() {
        let character = if matches!(character, '\n' | '\r' | '\t') {
            ' '
        } else {
            character
        };
        if title.len().saturating_add(character.len_utf8()) > MAX_TITLE_BYTES {
            break;
        }
        title.push(character);
    }
    let title = title.trim().to_owned();
    if title.is_empty() {
        "New conversation".to_owned()
    } else {
        title
    }
}

fn saved_model_for_request(conversation: &OpenRouterConversationV2) -> String {
    conversation
        .turns
        .last()
        .map(|turn| turn.model_id.clone())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use tempfile::{tempdir, TempDir};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use url::Url;

    use crate::credentials::{CredentialAccount, FakeCredentialStore, SecretValue};

    use super::*;
    use crate::openrouter::{FileOpenRouterStore, OpenRouterTimeouts};

    fn service(
        credentials: Arc<FakeCredentialStore>,
        store: Arc<FileOpenRouterStore>,
    ) -> OpenRouterService {
        let client = OpenRouterClient::with_loopback_base_url(
            Url::parse("http://127.0.0.1:9").unwrap(),
            credentials.clone(),
            OpenRouterTimeouts {
                connect: Duration::from_millis(20),
                get_attempt: Duration::from_millis(50),
                chat_headers: Duration::from_millis(50),
                sse_idle: Duration::from_millis(50),
                chat_total: Duration::from_millis(100),
                retry_delay: Duration::ZERO,
            },
        )
        .unwrap();
        OpenRouterService::new(client, credentials, store)
    }

    async fn sse_service(
        body: &'static str,
        keep_open: bool,
    ) -> (
        OpenRouterService,
        Arc<FileOpenRouterStore>,
        tokio::task::JoinHandle<()>,
        TempDir,
    ) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = Url::parse(&format!("http://{}", listener.local_addr().unwrap())).unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = vec![0_u8; 16 * 1024];
            let _ = stream.read(&mut request).await.unwrap();
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n",
                )
                .await
                .unwrap();
            stream.write_all(body.as_bytes()).await.unwrap();
            if keep_open {
                std::future::pending::<()>().await;
            }
        });
        let directory = tempdir().unwrap();
        let store =
            Arc::new(FileOpenRouterStore::new(directory.path().join("openrouter")).unwrap());
        let credentials = Arc::new(FakeCredentialStore::with_openrouter_key(
            SecretValue::from_input("offline-test-key").unwrap(),
        ));
        let client = OpenRouterClient::with_loopback_base_url(
            base,
            credentials.clone(),
            OpenRouterTimeouts {
                connect: Duration::from_secs(1),
                get_attempt: Duration::from_secs(1),
                chat_headers: Duration::from_secs(1),
                sse_idle: Duration::from_secs(2),
                chat_total: Duration::from_secs(2),
                retry_delay: Duration::ZERO,
            },
        )
        .unwrap();
        (
            OpenRouterService::new(client, credentials, store.clone()),
            store,
            server,
            directory,
        )
    }

    #[test]
    fn persisted_title_is_control_free_and_unicode_byte_bounded() {
        let title = title_for(
            "  hello\n\u{1b}[31m界界界界界界界界界界界界界界界界界界界界界界界界界界界界界界  ",
        );
        assert!(!title.chars().any(char::is_control));
        assert!(!title.contains('\n'));
        assert!(title.len() <= 80);
        assert!(std::str::from_utf8(title.as_bytes()).is_ok());
    }

    #[tokio::test]
    async fn request_preflight_failure_leaves_no_in_progress_conversation() {
        let directory = tempdir().unwrap();
        let store =
            Arc::new(FileOpenRouterStore::new(directory.path().join("openrouter")).unwrap());
        let credentials = Arc::new(FakeCredentialStore::with_openrouter_key(
            SecretValue::from_input("offline-test-key").unwrap(),
        ));
        let mut service = service(credentials, store.clone());
        let result = service
            .start_turn(None, "vendor/model".to_owned(), "x".repeat(1024 * 1024))
            .await;
        assert!(result.is_err());
        assert!(store.list_conversations().unwrap().is_empty());
    }

    #[tokio::test]
    async fn prepared_turn_does_not_post_until_the_pointer_can_be_persisted() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = Url::parse(&format!("http://{}", listener.local_addr().unwrap())).unwrap();
        let directory = tempdir().unwrap();
        let store =
            Arc::new(FileOpenRouterStore::new(directory.path().join("openrouter")).unwrap());
        let credentials = Arc::new(FakeCredentialStore::with_openrouter_key(
            SecretValue::from_input("offline-test-key").unwrap(),
        ));
        let client = OpenRouterClient::with_loopback_base_url(
            base,
            credentials.clone(),
            OpenRouterTimeouts {
                connect: Duration::from_millis(20),
                get_attempt: Duration::from_millis(50),
                chat_headers: Duration::from_millis(50),
                sse_idle: Duration::from_millis(50),
                chat_total: Duration::from_millis(100),
                retry_delay: Duration::ZERO,
            },
        )
        .unwrap();
        let mut service = OpenRouterService::new(client, credentials, store.clone());

        let prepared = service
            .prepare_turn(None, "vendor/model".to_owned(), "hello".to_owned())
            .await
            .unwrap();
        let id = prepared.conversation_id().clone();
        assert!(
            tokio::time::timeout(Duration::from_millis(30), listener.accept())
                .await
                .is_err()
        );
        assert_eq!(
            store.load_conversation(&id).unwrap().turns[0].outcome,
            OpenRouterTurnOutcome::InProgress
        );

        service.abandon_prepared_turn(prepared).await.unwrap();
        assert_eq!(
            store.load_conversation(&id).unwrap().turns[0].outcome,
            OpenRouterTurnOutcome::Failed
        );
    }

    #[tokio::test]
    async fn logout_joins_candidate_validation_before_deleting_credential() {
        let directory = tempdir().unwrap();
        let store =
            Arc::new(FileOpenRouterStore::new(directory.path().join("openrouter")).unwrap());
        let credentials = Arc::new(FakeCredentialStore::with_openrouter_key(
            SecretValue::from_input("old-offline-test-key").unwrap(),
        ));
        let mut service = service(credentials.clone(), store);
        service
            .replace_candidate(SecretValue::from_input("candidate-offline-test-key").unwrap())
            .unwrap();
        service.logout().await.1.unwrap();
        assert!(!credentials.is_configured(CredentialAccount::OpenRouterApiKey));
    }

    #[tokio::test]
    async fn saturated_control_queue_cannot_deadlock_logout_or_recreate_credential() {
        let directory = tempdir().unwrap();
        let store =
            Arc::new(FileOpenRouterStore::new(directory.path().join("openrouter")).unwrap());
        let credentials = Arc::new(FakeCredentialStore::with_openrouter_key(
            SecretValue::from_input("old-offline-test-key").unwrap(),
        ));
        let mut service = service(credentials.clone(), store);
        for _ in 0..EVENT_QUEUE {
            service
                .control_events_tx
                .try_send(OpenRouterServiceEvent::AuthValidated { operation_id: 0 })
                .unwrap();
        }
        service
            .replace_candidate(SecretValue::from_input("candidate-offline-test-key").unwrap())
            .unwrap();

        tokio::time::timeout(Duration::from_secs(1), service.logout())
            .await
            .expect("logout must drain a saturated control queue")
            .1
            .unwrap();
        assert!(!credentials.is_configured(CredentialAccount::OpenRouterApiKey));
    }

    #[tokio::test]
    async fn saturated_chat_queue_cannot_deadlock_interrupting_shutdown() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = Url::parse(&format!("http://{}", listener.local_addr().unwrap())).unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = vec![0_u8; 16 * 1024];
            let _ = stream.read(&mut request).await.unwrap();
            let body = concat!(
                "data: {\"choices\":[{\"delta\":{\"content\":\"hello\"}}]}\n\n",
                "data: [DONE]\n\n"
            );
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream.write_all(response.as_bytes()).await.unwrap();
        });
        let directory = tempdir().unwrap();
        let store =
            Arc::new(FileOpenRouterStore::new(directory.path().join("openrouter")).unwrap());
        let credentials = Arc::new(FakeCredentialStore::with_openrouter_key(
            SecretValue::from_input("offline-test-key").unwrap(),
        ));
        let client = OpenRouterClient::with_loopback_base_url(
            base,
            credentials,
            OpenRouterTimeouts {
                connect: Duration::from_secs(1),
                get_attempt: Duration::from_secs(1),
                chat_headers: Duration::from_secs(1),
                sse_idle: Duration::from_secs(1),
                chat_total: Duration::from_secs(1),
                retry_delay: Duration::ZERO,
            },
        )
        .unwrap();
        let mut service = OpenRouterService::new(
            client,
            Arc::new(FakeCredentialStore::default()),
            store.clone(),
        );
        for _ in 0..EVENT_QUEUE {
            service
                .chat_events_tx
                .try_send(OpenRouterServiceEvent::TextDelta {
                    conversation_id: OpenRouterConversationId::default(),
                    turn_id: OpenRouterTurnId::new(),
                    delta: "queued".to_owned(),
                })
                .unwrap();
        }
        let prepared = service
            .prepare_turn(None, "vendor/model".to_owned(), "hello".to_owned())
            .await
            .unwrap();
        let conversation_id = prepared.conversation_id().clone();
        service.launch_prepared_turn(prepared);
        tokio::time::sleep(Duration::from_millis(30)).await;

        tokio::time::timeout(Duration::from_secs(1), service.shutdown())
            .await
            .expect("shutdown must drain a saturated chat queue");
        let record = &store.load_conversation(&conversation_id).unwrap().turns[0];
        assert_eq!(record.outcome, OpenRouterTurnOutcome::Interrupted);
        assert_eq!(record.assistant_text, None);
        assert_eq!(record.incomplete_assistant_text, None);
        server.abort();
        let _ = server.await;
    }

    #[tokio::test]
    async fn failed_stream_persists_and_reopens_only_the_nonempty_partial() {
        let body = concat!(
            "data: {\"id\":\"chat-1\",\"model\":\"vendor/model\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"partial\"},\"finish_reason\":null}]}\n\n",
            "data: {\"error\":{\"code\":429,\"message\":\"private detail\",\"metadata\":{\"error_type\":\"rate_limit_exceeded\"}},\"choices\":null,\"usage\":{\"total_tokens\":\"bad\"}}\n\n",
        );
        let (mut service, store, server, directory) = sse_service(body, false).await;
        let (conversation_id, _) = service
            .start_turn(None, "vendor/model".to_owned(), "hello".to_owned())
            .await
            .unwrap();
        let mut saw_partial = false;
        loop {
            let event = tokio::time::timeout(Duration::from_secs(1), service.next_event())
                .await
                .unwrap()
                .unwrap();
            match event {
                OpenRouterServiceEvent::TextDelta { delta, .. } => {
                    saw_partial |= delta == "partial";
                }
                OpenRouterServiceEvent::TurnFinished {
                    outcome,
                    assistant_text,
                    incomplete_assistant_text,
                    failure,
                    failure_stage,
                    ..
                } => {
                    assert_eq!(outcome, OpenRouterTurnOutcome::Failed);
                    assert_eq!(assistant_text, None);
                    assert_eq!(incomplete_assistant_text.as_deref(), Some("partial"));
                    assert_eq!(failure, Some(OpenRouterFailureCategory::RateLimited));
                    assert_eq!(failure_stage, None);
                    break;
                }
                _ => {}
            }
        }
        assert!(saw_partial);
        server.await.unwrap();

        let record = &store.load_conversation(&conversation_id).unwrap().turns[0];
        assert_eq!(record.outcome, OpenRouterTurnOutcome::Failed);
        assert_eq!(record.assistant_text, None);
        assert_eq!(record.incomplete_assistant_text.as_deref(), Some("partial"));
        drop(service);
        drop(store);

        let reopened = FileOpenRouterStore::new(directory.path().join("openrouter")).unwrap();
        let conversation = reopened.load_conversation(&conversation_id).unwrap();
        assert_eq!(
            conversation.turns[0].incomplete_assistant_text.as_deref(),
            Some("partial")
        );
        assert!(conversation
            .canonical_messages()
            .iter()
            .all(|message| message.content != "partial"));
    }

    #[tokio::test]
    async fn staged_parser_failure_reaches_service_terminal_but_not_conversation_schema() {
        let body = concat!(
            "data: {\"model\":\"vendor/model\",\"choices\":[{\"delta\":{\"content\":\"partial\"}}]}\n\n",
            "data: {\"model\":\"vendor/model\",\"choices\":[{\"delta\":null}]}\n\n",
        );
        let (mut service, store, server, _directory) = sse_service(body, false).await;
        let (conversation_id, _) = service
            .start_turn(None, "vendor/model".to_owned(), "hello".to_owned())
            .await
            .unwrap();
        loop {
            let event = tokio::time::timeout(Duration::from_secs(1), service.next_event())
                .await
                .unwrap()
                .unwrap();
            if let OpenRouterServiceEvent::TurnFinished {
                outcome,
                incomplete_assistant_text,
                failure,
                failure_stage,
                ..
            } = event
            {
                assert_eq!(outcome, OpenRouterTurnOutcome::Failed);
                assert_eq!(incomplete_assistant_text.as_deref(), Some("partial"));
                assert_eq!(failure, Some(OpenRouterFailureCategory::InvalidResponse));
                assert_eq!(failure_stage, Some(OpenRouterStreamStage::CompletionShape));
                break;
            }
        }
        server.await.unwrap();

        let conversation = store.load_conversation(&conversation_id).unwrap();
        assert_eq!(conversation.turns[0].outcome, OpenRouterTurnOutcome::Failed);
        assert_eq!(
            conversation.turns[0].incomplete_assistant_text.as_deref(),
            Some("partial")
        );
        let persisted = serde_json::to_string(&conversation).unwrap();
        assert!(!persisted.contains("failure_stage"));
        assert!(!persisted.contains("CompletionShape"));
    }

    #[tokio::test]
    async fn malformed_terminal_usage_persists_completed_answer_and_reopens_canonically() {
        let body = concat!(
            "data: {\"id\":\"chat-1\",\"model\":\"vendor/resolved-model\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"answer\"}}]}\n\n",
            "data: {\"id\":\"chat-1\",\"model\":\"vendor/resolved-model\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
            "data: {\"id\":\"metadata-only\",\"model\":\"vendor/resolved-usage\",\"choices\":[],\"usage\":{\"total_tokens\":null}}\n\n",
            "data: [DONE]\n\n",
        );
        let (mut service, store, server, directory) = sse_service(body, false).await;
        let (conversation_id, _) = service
            .start_turn(
                None,
                "vendor/requested-alias".to_owned(),
                "hello".to_owned(),
            )
            .await
            .unwrap();
        loop {
            let event = tokio::time::timeout(Duration::from_secs(1), service.next_event())
                .await
                .unwrap()
                .unwrap();
            if let OpenRouterServiceEvent::TurnFinished {
                outcome,
                assistant_text,
                incomplete_assistant_text,
                usage,
                failure,
                ..
            } = event
            {
                assert_eq!(outcome, OpenRouterTurnOutcome::Completed);
                assert_eq!(assistant_text.as_deref(), Some("answer"));
                assert_eq!(incomplete_assistant_text, None);
                assert_eq!(usage, None);
                assert_eq!(failure, None);
                break;
            }
        }
        server.await.unwrap();
        let record = &store.load_conversation(&conversation_id).unwrap().turns[0];
        assert_eq!(record.outcome, OpenRouterTurnOutcome::Completed);
        assert_eq!(record.model_id, "vendor/requested-alias");
        assert_eq!(record.assistant_text.as_deref(), Some("answer"));
        assert_eq!(record.incomplete_assistant_text, None);
        drop(service);
        drop(store);

        let reopened = FileOpenRouterStore::new(directory.path().join("openrouter")).unwrap();
        let conversation = reopened.load_conversation(&conversation_id).unwrap();
        assert_eq!(
            conversation.turns[0].outcome,
            OpenRouterTurnOutcome::Completed
        );
        assert_eq!(
            conversation.turns[0].assistant_text.as_deref(),
            Some("answer")
        );
        assert_eq!(conversation.turns[0].incomplete_assistant_text, None);
        let canonical = conversation.canonical_messages();
        assert_eq!(canonical.len(), 2);
        assert_eq!(canonical[0].content, "hello");
        assert_eq!(canonical[1].content, "answer");
    }

    #[tokio::test]
    async fn failed_stream_without_a_delta_persists_no_incomplete_text() {
        let body = "data: {\"error\":{\"code\":429,\"message\":\"private detail\",\"metadata\":{\"error_type\":\"rate_limit_exceeded\"}},\"choices\":[]}\n\n";
        let (mut service, store, server, _directory) = sse_service(body, false).await;
        let (conversation_id, _) = service
            .start_turn(None, "vendor/model".to_owned(), "hello".to_owned())
            .await
            .unwrap();
        loop {
            let event = tokio::time::timeout(Duration::from_secs(1), service.next_event())
                .await
                .unwrap()
                .unwrap();
            if let OpenRouterServiceEvent::TurnFinished {
                outcome,
                incomplete_assistant_text,
                ..
            } = event
            {
                assert_eq!(outcome, OpenRouterTurnOutcome::Failed);
                assert_eq!(incomplete_assistant_text, None);
                break;
            }
        }
        server.await.unwrap();
        assert_eq!(
            store.load_conversation(&conversation_id).unwrap().turns[0].incomplete_assistant_text,
            None
        );
    }

    #[tokio::test]
    async fn interruption_after_a_delta_discards_incomplete_text() {
        let body = "data: {\"id\":\"chat-1\",\"model\":\"vendor/model\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"partial\"},\"finish_reason\":null}]}\n\n";
        let (mut service, store, server, _directory) = sse_service(body, true).await;
        let (conversation_id, _) = service
            .start_turn(None, "vendor/model".to_owned(), "hello".to_owned())
            .await
            .unwrap();
        loop {
            let event = tokio::time::timeout(Duration::from_secs(1), service.next_event())
                .await
                .unwrap()
                .unwrap();
            if matches!(event, OpenRouterServiceEvent::TextDelta { .. }) {
                service.interrupt_turn();
                break;
            }
        }
        loop {
            let event = tokio::time::timeout(Duration::from_secs(1), service.next_event())
                .await
                .unwrap()
                .unwrap();
            if let OpenRouterServiceEvent::TurnFinished {
                outcome,
                assistant_text,
                incomplete_assistant_text,
                ..
            } = event
            {
                assert_eq!(outcome, OpenRouterTurnOutcome::Interrupted);
                assert_eq!(assistant_text, None);
                assert_eq!(incomplete_assistant_text, None);
                break;
            }
        }
        server.abort();
        let _ = server.await;
        let record = &store.load_conversation(&conversation_id).unwrap().turns[0];
        assert_eq!(record.outcome, OpenRouterTurnOutcome::Interrupted);
        assert_eq!(record.incomplete_assistant_text, None);
    }

    #[tokio::test]
    async fn logout_preserves_a_real_completed_terminal_event_and_final_text() {
        let directory = tempdir().unwrap();
        let store =
            Arc::new(FileOpenRouterStore::new(directory.path().join("openrouter")).unwrap());
        let credentials = Arc::new(FakeCredentialStore::with_openrouter_key(
            SecretValue::from_input("offline-test-key").unwrap(),
        ));
        let mut service = service(credentials, store);
        let conversation_id = OpenRouterConversationId::default();
        let turn_id = OpenRouterTurnId::new();
        let events = service.chat_events_tx.clone();
        let expected_conversation = conversation_id.clone();
        let expected_turn = turn_id.clone();
        service.chat_task = Some(tokio::spawn(async move {
            events
                .send(OpenRouterServiceEvent::TurnFinished {
                    conversation_id: expected_conversation,
                    turn_id: expected_turn,
                    outcome: OpenRouterTurnOutcome::Completed,
                    assistant_text: Some("final text".to_owned()),
                    incomplete_assistant_text: None,
                    usage: None,
                    failure: None,
                    failure_stage: None,
                })
                .await
                .unwrap();
        }));

        let (drained, result) = service.logout().await;
        result.unwrap();
        assert!(drained.iter().any(|event| matches!(
            event,
            OpenRouterServiceEvent::TurnFinished {
                conversation_id: id,
                turn_id: turn,
                outcome: OpenRouterTurnOutcome::Completed,
                assistant_text: Some(text),
                ..
            } if id == &conversation_id && turn == &turn_id && text == "final text"
        )));
    }

    #[tokio::test]
    async fn logout_interrupts_a_hanging_chat_and_preserves_its_terminal_event() {
        let directory = tempdir().unwrap();
        let store =
            Arc::new(FileOpenRouterStore::new(directory.path().join("openrouter")).unwrap());
        let credentials = Arc::new(FakeCredentialStore::with_openrouter_key(
            SecretValue::from_input("offline-test-key").unwrap(),
        ));
        let mut service = service(credentials, store);
        let conversation_id = OpenRouterConversationId::default();
        let turn_id = OpenRouterTurnId::new();
        let cancel = CancellationToken::new();
        let task_cancel = cancel.clone();
        let events = service.chat_events_tx.clone();
        let expected_conversation = conversation_id.clone();
        let expected_turn = turn_id.clone();
        service.chat_cancel = Some(cancel);
        service.chat_task = Some(tokio::spawn(async move {
            task_cancel.cancelled().await;
            events
                .send(OpenRouterServiceEvent::TurnFinished {
                    conversation_id: expected_conversation,
                    turn_id: expected_turn,
                    outcome: OpenRouterTurnOutcome::Interrupted,
                    assistant_text: None,
                    incomplete_assistant_text: None,
                    usage: None,
                    failure: None,
                    failure_stage: None,
                })
                .await
                .unwrap();
        }));

        let (drained, result) = service.logout().await;
        result.unwrap();
        assert!(drained.iter().any(|event| matches!(
            event,
            OpenRouterServiceEvent::TurnFinished {
                conversation_id: id,
                turn_id: turn,
                outcome: OpenRouterTurnOutcome::Interrupted,
                ..
            } if id == &conversation_id && turn == &turn_id
        )));
    }
}
