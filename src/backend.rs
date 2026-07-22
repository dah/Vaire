//! Testable backend coordinator shared by the background runtime and integration tests.

use std::collections::{HashSet, VecDeque};

use thiserror::Error;

use crate::app::{
    Action, AppState, DomainEvent, Effect, Intent, ThinkingKind, ThreadDeletionFailure,
    ThreadState, TurnOutcome,
};
use crate::codex::protocol::CancelLoginAccountStatus;
use crate::codex::protocol::{ProtocolEvent, TurnStatus};
use crate::codex::session::{
    history_entries, model_choices, thread_choices, AccountState, SessionError, SessionEvent,
    SessionService,
};
use crate::codex::transport::TransportError;
use crate::persistence::{LoadNotice, PersistenceError, PreferencesPort};
use crate::platform::{BrowserError, BrowserOpener};

const MAX_TRACKED_COMPLETED_ITEMS_PER_TURN: usize = 1_024;
const MAX_TRACKED_COMPLETED_ITEM_ID_BYTES: usize = 64 * 1_024;

#[derive(Debug, Default)]
struct CompletedItemTracker {
    scope: Option<(String, String)>,
    ids: HashSet<String>,
    id_bytes: usize,
    saturated: bool,
}

impl CompletedItemTracker {
    fn begin_turn(&mut self, thread_id: &str, turn_id: &str) {
        self.scope = Some((thread_id.to_owned(), turn_id.to_owned()));
        self.ids.clear();
        self.id_bytes = 0;
        self.saturated = false;
    }

    fn observe_turn(&mut self, thread_id: &str, turn_id: &str) {
        if !self.is_scope(thread_id, turn_id) {
            self.begin_turn(thread_id, turn_id);
        }
    }

    fn should_ignore(&self, thread_id: &str, turn_id: &str, item_id: &str) -> bool {
        self.is_scope(thread_id, turn_id) && (self.saturated || self.ids.contains(item_id))
    }

    fn record(&mut self, thread_id: &str, turn_id: &str, item_id: &str) {
        if !self.is_scope(thread_id, turn_id) || self.saturated || self.ids.contains(item_id) {
            return;
        }
        if self.ids.len() >= MAX_TRACKED_COMPLETED_ITEMS_PER_TURN
            || self.id_bytes.saturating_add(item_id.len()) > MAX_TRACKED_COMPLETED_ITEM_ID_BYTES
        {
            // Once exact tracking cannot continue within its hard bounds, stop accepting all
            // subsequent item mutations for this turn. Dropping output is safer than allowing a
            // late delta to rewrite an item whose completion could not be retained.
            self.saturated = true;
            return;
        }
        self.ids.insert(item_id.to_owned());
        self.id_bytes = self.id_bytes.saturating_add(item_id.len());
    }

    fn reset(&mut self) {
        *self = Self::default();
    }

    fn is_scope(&self, thread_id: &str, turn_id: &str) -> bool {
        self.scope
            .as_ref()
            .is_some_and(|(active_thread, active_turn)| {
                active_thread == thread_id && active_turn == turn_id
            })
    }
}

#[derive(Debug, Error)]
pub enum BackendError {
    #[error(transparent)]
    Session(#[from] SessionError),
    #[error(transparent)]
    Persistence(#[from] PersistenceError),
    #[error(transparent)]
    Browser(#[from] BrowserError),
    #[error("the connected app-server reported unsupported platform {0}")]
    UnsupportedPlatform(String),
}

pub struct BackendCoordinator<P, B> {
    state: AppState,
    session: SessionService,
    preferences: P,
    browser: B,
    may_persist: bool,
    completed_items: CompletedItemTracker,
}

impl<P: PreferencesPort, B: BrowserOpener> BackendCoordinator<P, B> {
    pub fn new(session: SessionService, preferences: P, browser: B) -> Self {
        Self {
            state: AppState::default(),
            session,
            preferences,
            browser,
            may_persist: true,
            completed_items: CompletedItemTracker::default(),
        }
    }

    pub fn state(&self) -> &AppState {
        &self.state
    }

    pub async fn startup(&mut self) -> Result<(), BackendError> {
        let loaded = self.preferences.load()?;
        self.may_persist = loaded.may_overwrite;
        self.state
            .reduce(Action::Event(DomainEvent::PreferencesLoaded(
                loaded.preferences,
            )));
        if let Some(message) = load_notice_message(loaded.notice) {
            self.state.notice = Some(message);
        }
        self.state.reduce(Action::Event(DomainEvent::Connecting));

        let initialized = match self.session.initialize().await {
            Ok(initialized) => initialized,
            Err(error) => {
                self.state
                    .reduce(Action::Event(DomainEvent::ConnectionFailed(
                        error.to_string(),
                    )));
                return Err(error.into());
            }
        };
        if initialized.platform_os == "windows" {
            let error = BackendError::UnsupportedPlatform(initialized.platform_os);
            self.state
                .reduce(Action::Event(DomainEvent::ConnectionFailed(
                    error.to_string(),
                )));
            return Err(error);
        }
        self.state.reduce(Action::Event(DomainEvent::Connected {
            generation: self.session.generation(),
        }));

        let account = match self.session.read_account().await {
            Ok(account) => account,
            Err(error) => {
                self.state
                    .reduce(Action::Event(DomainEvent::ConnectionFailed(
                        error.to_string(),
                    )));
                return Err(error.into());
            }
        };
        let models = match self.session.list_models().await {
            Ok(models) => models,
            Err(error) => {
                self.state
                    .reduce(Action::Event(DomainEvent::ConnectionFailed(
                        error.to_string(),
                    )));
                return Err(error.into());
            }
        };
        self.state
            .reduce(Action::Event(DomainEvent::CatalogLoaded(model_choices(
                &models,
            ))));
        let effects = self.reduce_account(account);
        self.execute_effects(effects).await
    }

    pub fn accept_intent(&mut self, intent: Intent) -> Vec<Effect> {
        self.state.reduce(Action::Intent(intent))
    }

    pub async fn execute_pending(&mut self, effects: Vec<Effect>) -> Result<(), BackendError> {
        self.execute_effects(effects).await
    }

    pub fn record_error(&mut self, message: impl Into<String>) {
        self.state.notice = Some(message.into());
    }

    pub async fn handle_intent(&mut self, intent: Intent) -> Result<(), BackendError> {
        let effects = self.accept_intent(intent);
        self.execute_pending(effects).await
    }

    /// Receives and parses exactly one transport event without starting any follow-up RPCs.
    ///
    /// The runtime selects this cancellation-safe boundary against user input. Once it returns,
    /// `process_received_event` must be allowed to finish so an already-consumed event cannot be
    /// lost while, for example, an account update waits for `account/read`.
    pub async fn receive_event(&mut self) -> Option<Result<SessionEvent, SessionError>> {
        self.session.next_event().await
    }

    /// Convenience path for sequential tests and callers.
    ///
    /// Do not race this combined future against unrelated work: use `receive_event` followed by
    /// `process_received_event` so cancellation cannot land between receipt and processing.
    pub async fn pump_event(&mut self) -> Result<bool, BackendError> {
        let event = self.receive_event().await;
        self.process_received_event(event).await
    }

    pub async fn process_received_event(
        &mut self,
        event: Option<Result<SessionEvent, SessionError>>,
    ) -> Result<bool, BackendError> {
        let Some(event) = event else {
            self.state.reduce(Action::Event(DomainEvent::ProcessExited(
                "app-server event stream closed".to_owned(),
            )));
            return Ok(false);
        };
        let event = match event {
            Ok(event) => event,
            Err(error) => {
                self.state
                    .reduce(Action::Event(DomainEvent::ConnectionFailed(
                        error.to_string(),
                    )));
                return Err(error.into());
            }
        };
        let effects = match event {
            SessionEvent::Protocol(event) => match self.reduce_protocol_event(event).await {
                Ok(effects) => effects,
                Err(error) => {
                    self.state
                        .reduce(Action::Event(DomainEvent::ConnectionFailed(
                            error.to_string(),
                        )));
                    return Err(error);
                }
            },
            SessionEvent::UnknownNotification(_) => Vec::new(),
            SessionEvent::SafetyViolation(method) => self
                .state
                .reduce(Action::Event(DomainEvent::SafetyViolation(method))),
            SessionEvent::ConnectionClosed(category) => {
                self.state
                    .reduce(Action::Event(DomainEvent::ProcessExited(format!(
                        "app-server connection closed ({category})"
                    ))))
            }
        };
        self.execute_effects(effects).await?;
        Ok(true)
    }

    pub async fn replace_session_and_restart(
        &mut self,
        session: SessionService,
    ) -> Result<(), BackendError> {
        let _ = self.session.shutdown().await;
        self.session = session;
        self.completed_items.reset();
        self.state.connection = crate::app::ConnectionState::Disconnected;
        self.state.turn = crate::app::TurnState::Idle;
        self.startup().await
    }

    pub async fn shutdown(&mut self) -> Result<(), BackendError> {
        let persistence_result = if self.may_persist {
            self.preferences
                .save(&self.state.preferences)
                .map_err(BackendError::from)
        } else {
            Ok(())
        };
        let session_result = self.session.shutdown().await.map_err(BackendError::from);
        persistence_result?;
        session_result
    }

    async fn execute_effects(&mut self, initial: Vec<Effect>) -> Result<(), BackendError> {
        let mut effects = VecDeque::from(initial);
        while let Some(effect) = effects.pop_front() {
            let produced = match effect {
                Effect::StartLogin => match self.session.start_login().await {
                    Ok(challenge) => {
                        let effects = self.state.reduce(Action::Event(DomainEvent::LoginStarted {
                            login_id: challenge.login_id,
                        }));
                        match self.browser.open_login_url(&challenge.auth_url) {
                            Ok(()) => {
                                self.state.notice = Some(
                                "Complete sign-in in the browser; if it fails, use /logout then /login device"
                                    .to_owned(),
                            );
                            }
                            Err(error) => {
                                self.state.notice = Some(format!(
                                    "{error}; use /logout to cancel this pending sign-in"
                                ));
                            }
                        }
                        effects
                    }
                    Err(error) => self
                        .state
                        .reduce(Action::Event(DomainEvent::LoginFailed(error.to_string()))),
                },
                Effect::StartDeviceLogin => match self.session.start_device_login().await {
                    Ok(challenge) => {
                        let effects = self.state.reduce(Action::Event(DomainEvent::LoginStarted {
                            login_id: challenge.login_id,
                        }));
                        match self.browser.open_login_url(&challenge.verification_url) {
                            Ok(()) => {
                                self.state.notice = Some(format!(
                                    "Enter code {} in the browser; use /logout to cancel",
                                    challenge.user_code
                                ));
                            }
                            Err(error) => {
                                self.state.notice = Some(format!(
                                    "{error}; use /logout to cancel this pending sign-in"
                                ));
                            }
                        }
                        effects
                    }
                    Err(error) => self
                        .state
                        .reduce(Action::Event(DomainEvent::LoginFailed(error.to_string()))),
                },
                Effect::CancelLogin { login_id } => self.cancel_login(&login_id).await,
                Effect::Logout => match self.session.logout().await {
                    Ok(()) => self.state.reduce(Action::Event(DomainEvent::LoggedOut)),
                    Err(error) => {
                        self.state.notice = Some(error.to_string());
                        Vec::new()
                    }
                },
                Effect::StartNewThread => {
                    let model = self.selected_model()?;
                    match self.session.start_thread(&model).await {
                        Ok(thread) => {
                            self.state
                                .reduce(Action::Event(DomainEvent::NewThreadSucceeded {
                                    id: thread.id,
                                }))
                        }
                        Err(error) => {
                            let fatal = is_fatal_transport(&error);
                            let mut effects =
                                self.state
                                    .reduce(Action::Event(DomainEvent::NewThreadFailed(
                                        error.to_string(),
                                    )));
                            if fatal {
                                effects.extend(self.state.reduce(Action::Event(
                                    DomainEvent::ConnectionFailed(
                                        "app-server connection became unusable while starting a new thread; restart AgentHarness"
                                            .to_owned(),
                                    ),
                                )));
                            }
                            effects
                        }
                    }
                }
                Effect::ListThreads => match self.session.list_threads().await {
                    Ok(threads) => self
                        .state
                        .reduce(Action::Event(DomainEvent::ThreadListLoaded(
                            thread_choices(threads),
                        ))),
                    Err(error) => self
                        .state
                        .reduce(Action::Event(DomainEvent::ThreadListFailed(
                            error.to_string(),
                        ))),
                },
                Effect::ResumeThread { id } => {
                    self.state
                        .reduce(Action::Event(DomainEvent::ResumeStarted { id: id.clone() }));
                    let model = self.selected_model()?;
                    match self.session.resume_thread(&id, &model).await {
                        Ok(thread) => {
                            self.state
                                .reduce(Action::Event(DomainEvent::ResumeSucceeded {
                                    id,
                                    history: history_entries(&thread),
                                }))
                        }
                        Err(error) => self.state.reduce(Action::Event(DomainEvent::ResumeFailed {
                            id,
                            message: error.to_string(),
                        })),
                    }
                }
                Effect::SwitchThread { id } => {
                    let model = self.selected_model()?;
                    match self.session.resume_thread(&id, &model).await {
                        Ok(thread) => {
                            self.state
                                .reduce(Action::Event(DomainEvent::ThreadSwitchSucceeded {
                                    id,
                                    history: history_entries(&thread),
                                }))
                        }
                        Err(error) => {
                            let fatal = is_fatal_transport(&error);
                            let mut effects =
                                self.state
                                    .reduce(Action::Event(DomainEvent::ThreadSwitchFailed {
                                        id,
                                        message: error.to_string(),
                                    }));
                            if fatal {
                                effects.extend(self.state.reduce(Action::Event(
                                    DomainEvent::ConnectionFailed(
                                        "app-server connection became unusable while resuming a thread; restart AgentHarness"
                                            .to_owned(),
                                    ),
                                )));
                            }
                            effects
                        }
                    }
                }
                Effect::DeleteThreads { ids } => self.delete_threads(ids).await,
                Effect::SendMessage { text } => match self.send_message(&text).await {
                    Ok(effects) => effects,
                    Err(error) => self.reduce_mutating_error(error),
                },
                Effect::InterruptTurn { thread_id, turn_id } => {
                    match self.session.interrupt_turn(&thread_id, &turn_id).await {
                        Ok(()) => Vec::new(),
                        Err(error) => self.reduce_mutating_error(error.into()),
                    }
                }
                Effect::Persist(preferences) => {
                    self.may_persist = true;
                    self.preferences.save(&preferences)?;
                    Vec::new()
                }
                Effect::Shutdown => {
                    self.shutdown().await?;
                    Vec::new()
                }
            };
            effects.extend(produced);
        }
        Ok(())
    }

    async fn send_message(&mut self, text: &str) -> Result<Vec<Effect>, BackendError> {
        let model = self.selected_model()?;
        let effort =
            self.state.selected_reasoning.clone().ok_or_else(|| {
                SessionError::Protocol("no reasoning effort is selected".to_owned())
            })?;
        let thread_id = match &self.state.thread {
            ThreadState::Ready { id } => id.clone(),
            ThreadState::None => {
                let thread = self.session.start_thread(&model).await?;
                let id = thread.id;
                let effects = self
                    .state
                    .reduce(Action::Event(DomainEvent::ThreadStarted { id: id.clone() }));
                for effect in effects {
                    if let Effect::Persist(preferences) = effect {
                        self.may_persist = true;
                        self.preferences.save(&preferences)?;
                    }
                }
                id
            }
            _ => {
                return Err(SessionError::Protocol(
                    "message effect reached a non-sendable thread state".to_owned(),
                )
                .into())
            }
        };
        let response = self
            .session
            .start_turn(&thread_id, text, &model, &effort)
            .await?;
        let turn_id = response.turn.id;
        self.completed_items.begin_turn(&thread_id, &turn_id);
        Ok(self.state.reduce(Action::Event(DomainEvent::TurnStarted {
            thread_id,
            turn_id,
        })))
    }

    async fn delete_threads(&mut self, ids: Vec<String>) -> Vec<Effect> {
        let requested = ids.len();
        let mut protected_ids = Vec::new();
        if let Some(id) = self.state.preferences.thread_id.clone() {
            protected_ids.push(id);
        }
        if let ThreadState::Ready { id } = &self.state.thread {
            if !protected_ids.contains(id) {
                protected_ids.push(id.clone());
            }
        }
        let mut deleted = Vec::new();
        let mut failures = Vec::new();
        let mut fatal_message = None;
        let mut pending = ids.into_iter();
        while let Some(id) = pending.next() {
            if protected_ids.contains(&id) {
                failures.push(ThreadDeletionFailure {
                    id,
                    message: "active saved thread is protected".to_owned(),
                });
                continue;
            }
            match self.session.delete_thread(&id).await {
                Ok(()) => deleted.push(id),
                Err(error) => {
                    let fatal = is_fatal_transport(&error);
                    failures.push(ThreadDeletionFailure {
                        id,
                        message: error.to_string(),
                    });
                    if fatal {
                        for skipped in pending {
                            failures.push(ThreadDeletionFailure {
                                id: skipped,
                                message: "not attempted because the app-server connection became unusable"
                                    .to_owned(),
                            });
                        }
                        fatal_message = Some(
                            "app-server connection became unusable during thread deletion; restart AgentHarness"
                                .to_owned(),
                        );
                        break;
                    }
                }
            }
        }
        let mut effects = self
            .state
            .reduce(Action::Event(DomainEvent::ThreadDeletionFinished {
                requested,
                deleted,
                failures,
            }));
        if let Some(message) = fatal_message {
            effects.extend(
                self.state
                    .reduce(Action::Event(DomainEvent::ConnectionFailed(message))),
            );
        }
        effects
    }

    async fn cancel_login(&mut self, login_id: &str) -> Vec<Effect> {
        let status = match self.session.cancel_login(login_id).await {
            Ok(status) => status,
            Err(error) => {
                self.state.notice = Some(format!(
                    "could not cancel ChatGPT sign-in: {error}; use /logout to retry"
                ));
                return Vec::new();
            }
        };

        match self.session.read_account().await {
            Ok(account) => {
                let effects = self.reduce_account(account);
                if matches!(self.state.auth, crate::app::AuthState::SignedOut) {
                    self.state.notice = Some(match status {
                        CancelLoginAccountStatus::Canceled => {
                            "ChatGPT sign-in cancelled; use /login to try again".to_owned()
                        }
                        CancelLoginAccountStatus::NotFound => {
                            "no pending ChatGPT sign-in was found; use /login to try again"
                                .to_owned()
                        }
                    });
                }
                effects
            }
            Err(error) => self.state.reduce(Action::Event(DomainEvent::LoginFailed(
                format!(
                    "ChatGPT sign-in was cancelled, but account state could not be refreshed: {error}; use /login to retry"
                ),
            ))),
        }
    }

    async fn reduce_protocol_event(
        &mut self,
        event: ProtocolEvent,
    ) -> Result<Vec<Effect>, BackendError> {
        Ok(match event {
            ProtocolEvent::AccountLoginCompleted(completed) => {
                let expected_login_id = match &self.state.auth {
                    crate::app::AuthState::SigningIn { login_id } => login_id.clone(),
                    _ => return Ok(Vec::new()),
                };
                let correlated = completed
                    .login_id
                    .as_deref()
                    .is_none_or(|completed| completed == expected_login_id.as_str());
                if !correlated {
                    self.state.notice =
                        Some("ignored a stale or mismatched ChatGPT login completion".to_owned());
                    Vec::new()
                } else if completed.success {
                    let account = self.session.read_account().await?;
                    self.reduce_account(account)
                } else {
                    self.state.reduce(Action::Event(DomainEvent::LoginFailed(
                        completed
                            .error
                            .unwrap_or_else(|| "ChatGPT login was cancelled".to_owned()),
                    )))
                }
            }
            ProtocolEvent::AccountUpdated => {
                let account = self.session.read_account().await?;
                self.reduce_account(account)
            }
            ProtocolEvent::ThreadStarted(_) => Vec::new(),
            ProtocolEvent::TurnStarted(notification) => {
                let thread_id = notification.thread_id;
                let turn_id = notification.turn.id;
                let effects = self.state.reduce(Action::Event(DomainEvent::TurnStarted {
                    thread_id: thread_id.clone(),
                    turn_id: turn_id.clone(),
                }));
                if matches!(
                    &self.state.turn,
                    crate::app::TurnState::Streaming {
                        thread_id: active_thread,
                        turn_id: active_turn,
                    } if active_thread == &thread_id && active_turn == &turn_id
                ) {
                    self.completed_items.observe_turn(&thread_id, &turn_id);
                }
                effects
            }
            ProtocolEvent::AgentMessageDelta(delta) => {
                if self.completed_items.should_ignore(
                    &delta.thread_id,
                    &delta.turn_id,
                    &delta.item_id,
                ) {
                    return Ok(Vec::new());
                }
                self.state.reduce(Action::Event(DomainEvent::AgentDelta {
                    thread_id: delta.thread_id,
                    turn_id: delta.turn_id,
                    item_id: delta.item_id,
                    delta: delta.delta,
                }))
            }
            ProtocolEvent::ReasoningSummaryTextDelta(delta) => {
                if self.completed_items.should_ignore(
                    &delta.thread_id,
                    &delta.turn_id,
                    &delta.item_id,
                ) {
                    return Ok(Vec::new());
                }
                self.state.reduce(Action::Event(DomainEvent::ThinkingDelta {
                    thread_id: delta.thread_id,
                    turn_id: delta.turn_id,
                    item_id: delta.item_id,
                    kind: ThinkingKind::Summary,
                    index: delta.summary_index,
                    delta: delta.delta,
                }))
            }
            ProtocolEvent::ReasoningSummaryPartAdded(part) => {
                if self
                    .completed_items
                    .should_ignore(&part.thread_id, &part.turn_id, &part.item_id)
                {
                    return Ok(Vec::new());
                }
                self.state
                    .reduce(Action::Event(DomainEvent::ThinkingSummaryPartAdded {
                        thread_id: part.thread_id,
                        turn_id: part.turn_id,
                        item_id: part.item_id,
                        summary_index: part.summary_index,
                    }))
            }
            ProtocolEvent::ReasoningTextDelta(delta) => {
                if self.completed_items.should_ignore(
                    &delta.thread_id,
                    &delta.turn_id,
                    &delta.item_id,
                ) {
                    return Ok(Vec::new());
                }
                self.state.reduce(Action::Event(DomainEvent::ThinkingDelta {
                    thread_id: delta.thread_id,
                    turn_id: delta.turn_id,
                    item_id: delta.item_id,
                    kind: ThinkingKind::EmittedText,
                    index: delta.content_index,
                    delta: delta.delta,
                }))
            }
            ProtocolEvent::ItemCompleted(completed) => {
                let recognized =
                    matches!(completed.item.kind.as_str(), "agentMessage" | "reasoning");
                if recognized
                    && self.completed_items.should_ignore(
                        &completed.thread_id,
                        &completed.turn_id,
                        &completed.item.id,
                    )
                {
                    return Ok(Vec::new());
                }
                if recognized {
                    self.completed_items.record(
                        &completed.thread_id,
                        &completed.turn_id,
                        &completed.item.id,
                    );
                }
                if completed.item.kind == "agentMessage" {
                    self.state
                        .reduce(Action::Event(DomainEvent::AgentCompleted {
                            thread_id: completed.thread_id,
                            turn_id: completed.turn_id,
                            item_id: completed.item.id,
                            text: completed.item.text.unwrap_or_default(),
                        }))
                } else if completed.item.kind == "reasoning" {
                    let content = completed
                        .item
                        .content
                        .into_iter()
                        .filter_map(|content| match content {
                            crate::codex::protocol::ThreadItemContent::Text(text) => Some(text),
                            _ => None,
                        })
                        .collect();
                    self.state
                        .reduce(Action::Event(DomainEvent::ThinkingCompleted {
                            thread_id: completed.thread_id,
                            turn_id: completed.turn_id,
                            item_id: completed.item.id,
                            summary: completed.item.summary,
                            content,
                        }))
                } else {
                    Vec::new()
                }
            }
            ProtocolEvent::TurnCompleted(completed) => {
                let outcome = match completed.turn.status {
                    TurnStatus::Completed => TurnOutcome::Completed,
                    TurnStatus::Interrupted => TurnOutcome::Interrupted,
                    TurnStatus::Failed => TurnOutcome::Failed(
                        completed
                            .turn
                            .error
                            .map_or_else(|| "turn failed".to_owned(), |error| error.message),
                    ),
                    TurnStatus::InProgress | TurnStatus::Unknown => {
                        TurnOutcome::Failed("turn/completed had a non-terminal status".to_owned())
                    }
                };
                self.state.reduce(Action::Event(DomainEvent::TurnFinished {
                    thread_id: completed.thread_id,
                    turn_id: completed.turn.id,
                    outcome,
                }))
            }
            ProtocolEvent::ThreadTokenUsageUpdated(updated) => {
                self.state
                    .reduce(Action::Event(DomainEvent::TokenUsageUpdated {
                        thread_id: updated.thread_id,
                        turn_id: updated.turn_id,
                        context_tokens: updated.token_usage.last.total_tokens,
                        model_context_window: updated.token_usage.model_context_window,
                    }))
            }
            ProtocolEvent::Error(error) => {
                if error.will_retry {
                    Vec::new()
                } else {
                    self.state.reduce(Action::Event(DomainEvent::TurnFinished {
                        thread_id: error.thread_id,
                        turn_id: error.turn_id,
                        outcome: TurnOutcome::Failed(error.error.message),
                    }))
                }
            }
        })
    }

    fn reduce_account(&mut self, account: AccountState) -> Vec<Effect> {
        match account {
            AccountState::SignedOut => self.state.reduce(Action::Event(DomainEvent::LoggedOut)),
            AccountState::Chatgpt { scope } => self
                .state
                .reduce(Action::Event(DomainEvent::AccountLoaded(scope))),
            AccountState::Unsupported(kind) => {
                self.state
                    .reduce(Action::Event(DomainEvent::UnsupportedAccount(format!(
                        "unsupported account type {kind}; use ChatGPT login"
                    ))))
            }
        }
    }

    fn selected_model(&self) -> Result<String, BackendError> {
        self.state
            .selected_model
            .clone()
            .ok_or_else(|| SessionError::Protocol("no model is selected".to_owned()).into())
    }

    fn reduce_mutating_error(&mut self, error: BackendError) -> Vec<Effect> {
        if matches!(
            &error,
            BackendError::Session(SessionError::Transport(TransportError::Timeout))
        ) {
            self.state
                .reduce(Action::Event(DomainEvent::ConnectionFailed(
                    "app-server timed out during a thread or turn change; restart AgentHarness before retrying"
                        .to_owned(),
                )))
        } else if let BackendError::Session(SessionError::Transport(
            TransportError::SafetyViolation(method),
        )) = error
        {
            self.state
                .reduce(Action::Event(DomainEvent::SafetyViolation(method)))
        } else {
            self.state
                .reduce(Action::Event(DomainEvent::TurnOperationFailed(
                    error.to_string(),
                )))
        }
    }
}

fn is_fatal_transport(error: &SessionError) -> bool {
    matches!(
        error,
        SessionError::Transport(TransportError::Timeout | TransportError::SafetyViolation(_))
    )
}

fn load_notice_message(notice: Option<LoadNotice>) -> Option<String> {
    match notice {
        None | Some(LoadNotice::Missing) => None,
        Some(notice) => Some(format!("preferences were not restored: {notice:?}")),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        load_notice_message, CompletedItemTracker, MAX_TRACKED_COMPLETED_ITEMS_PER_TURN,
        MAX_TRACKED_COMPLETED_ITEM_ID_BYTES,
    };
    use crate::persistence::LoadNotice;

    #[test]
    fn missing_preferences_are_a_quiet_first_run() {
        assert_eq!(load_notice_message(Some(LoadNotice::Missing)), None);
        assert!(load_notice_message(Some(LoadNotice::Corrupt)).is_some());
    }

    #[test]
    fn completed_items_reset_for_every_local_turn_even_when_server_ids_repeat() {
        let mut tracker = CompletedItemTracker::default();
        tracker.begin_turn("thread", "turn-one");
        tracker.record("thread", "turn-one", "reused-item");
        tracker.observe_turn("thread", "turn-one");
        assert!(tracker.should_ignore("thread", "turn-one", "reused-item"));

        tracker.begin_turn("thread", "turn-one");
        assert!(!tracker.should_ignore("thread", "turn-one", "reused-item"));

        tracker.record("thread", "turn-one", "reused-item");
        tracker.begin_turn("thread", "turn-two");
        assert!(!tracker.should_ignore("thread", "turn-two", "reused-item"));
        assert!(!tracker.should_ignore("thread", "turn-one", "reused-item"));
    }

    #[test]
    fn completed_item_tracking_saturates_closed_at_count_and_byte_bounds() {
        let mut tracker = CompletedItemTracker::default();
        tracker.begin_turn("thread", "count-bound");
        for index in 0..=MAX_TRACKED_COMPLETED_ITEMS_PER_TURN {
            tracker.record("thread", "count-bound", &format!("item-{index}"));
        }
        assert_eq!(tracker.ids.len(), MAX_TRACKED_COMPLETED_ITEMS_PER_TURN);
        assert!(tracker.should_ignore("thread", "count-bound", "untracked-late-item"));
        assert!(!tracker.should_ignore("other-thread", "count-bound", "untracked-late-item"));

        tracker.begin_turn("thread", "byte-bound");
        let at_limit = "x".repeat(MAX_TRACKED_COMPLETED_ITEM_ID_BYTES);
        tracker.record("thread", "byte-bound", &at_limit);
        tracker.record("thread", "byte-bound", "over-limit");
        assert_eq!(tracker.ids.len(), 1);
        assert_eq!(tracker.id_bytes, MAX_TRACKED_COMPLETED_ITEM_ID_BYTES);
        assert!(tracker.should_ignore("thread", "byte-bound", "another-untracked-item"));

        tracker.begin_turn("thread", "fresh-turn");
        assert!(!tracker.should_ignore("thread", "fresh-turn", "new-item"));
    }
}
