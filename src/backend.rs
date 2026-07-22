//! Testable backend coordinator shared by the background runtime and integration tests.

use std::collections::VecDeque;

use thiserror::Error;

use crate::app::{Action, AppState, DomainEvent, Effect, Intent, ThreadState, TurnOutcome};
use crate::codex::protocol::CancelLoginAccountStatus;
use crate::codex::protocol::{ProtocolEvent, TurnStatus};
use crate::codex::session::{
    history_entries, model_choices, AccountState, SessionError, SessionEvent, SessionService,
};
use crate::codex::transport::TransportError;
use crate::persistence::{LoadNotice, PersistenceError, PreferencesPort};
use crate::platform::{BrowserError, BrowserOpener};

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
}

impl<P: PreferencesPort, B: BrowserOpener> BackendCoordinator<P, B> {
    pub fn new(session: SessionService, preferences: P, browser: B) -> Self {
        Self {
            state: AppState::default(),
            session,
            preferences,
            browser,
            may_persist: true,
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

    pub async fn pump_event(&mut self) -> Result<bool, BackendError> {
        let Some(event) = self.session.next_event().await else {
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
        Ok(self.state.reduce(Action::Event(DomainEvent::TurnStarted {
            thread_id,
            turn_id: response.turn.id,
        })))
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
                self.state.reduce(Action::Event(DomainEvent::TurnStarted {
                    thread_id: notification.thread_id,
                    turn_id: notification.turn.id,
                }))
            }
            ProtocolEvent::AgentMessageDelta(delta) => {
                self.state.reduce(Action::Event(DomainEvent::AgentDelta {
                    thread_id: delta.thread_id,
                    turn_id: delta.turn_id,
                    item_id: delta.item_id,
                    delta: delta.delta,
                }))
            }
            ProtocolEvent::ItemCompleted(completed) => {
                if completed.item.kind == "agentMessage" {
                    self.state
                        .reduce(Action::Event(DomainEvent::AgentCompleted {
                            thread_id: completed.thread_id,
                            turn_id: completed.turn_id,
                            item_id: completed.item.id,
                            text: completed.item.text.unwrap_or_default(),
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
                        total_tokens: updated.token_usage.total.total_tokens,
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

fn load_notice_message(notice: Option<LoadNotice>) -> Option<String> {
    match notice {
        None | Some(LoadNotice::Missing) => None,
        Some(notice) => Some(format!("preferences were not restored: {notice:?}")),
    }
}

#[cfg(test)]
mod tests {
    use super::load_notice_message;
    use crate::persistence::LoadNotice;

    #[test]
    fn missing_preferences_are_a_quiet_first_run() {
        assert_eq!(load_notice_message(Some(LoadNotice::Missing)), None);
        assert!(load_notice_message(Some(LoadNotice::Corrupt)).is_some());
    }
}
