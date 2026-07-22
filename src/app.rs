use crate::command::HELP_TEXT;
use crate::persistence::{AccountScope, PreferencesV1};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Intent {
    SendMessage(String),
    Login,
    LoginDevice,
    Logout,
    ShowModels,
    SelectModel(String),
    ShowReasoning,
    SelectReasoning(String),
    Resume,
    Help,
    Quit,
    Interrupt,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModelChoice {
    pub id: String,
    pub display_name: String,
    pub is_default: bool,
    pub default_reasoning_effort: String,
    pub supported_reasoning_efforts: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ConnectionState {
    Disconnected,
    Connecting,
    Ready { generation: u64 },
    Failed(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AuthState {
    Unknown,
    SignedOut,
    SigningIn { login_id: String },
    SignedIn { scope: Option<AccountScope> },
    Unsupported(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ThreadState {
    None,
    Resuming { id: String },
    Ready { id: String },
    ResumeFailed { id: String, message: String },
    AccountMismatch { id: String },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TurnState {
    Idle,
    Starting,
    Streaming {
        thread_id: String,
        turn_id: String,
    },
    Completed {
        turn_id: String,
    },
    Interrupted {
        turn_id: String,
    },
    Failed {
        turn_id: Option<String>,
        message: String,
    },
}

impl TurnState {
    pub fn is_active(&self) -> bool {
        matches!(self, Self::Starting | Self::Streaming { .. })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TranscriptRole {
    User,
    Assistant,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TranscriptEntry {
    pub role: TranscriptRole,
    pub text: String,
    pub item_id: Option<String>,
    pub turn_id: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Effect {
    StartLogin,
    StartDeviceLogin,
    CancelLogin { login_id: String },
    Logout,
    ResumeThread { id: String },
    SendMessage { text: String },
    InterruptTurn { thread_id: String, turn_id: String },
    Persist(PreferencesV1),
    Shutdown,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TurnOutcome {
    Completed,
    Interrupted,
    Failed(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DomainEvent {
    PreferencesLoaded(PreferencesV1),
    Connecting,
    Connected {
        generation: u64,
    },
    ConnectionFailed(String),
    AccountLoaded(Option<AccountScope>),
    UnsupportedAccount(String),
    LoginStarted {
        login_id: String,
    },
    LoginFailed(String),
    LoggedOut,
    CatalogLoaded(Vec<ModelChoice>),
    ResumeStarted {
        id: String,
    },
    ResumeSucceeded {
        id: String,
        history: Vec<TranscriptEntry>,
    },
    ResumeFailed {
        id: String,
        message: String,
    },
    ThreadStarted {
        id: String,
    },
    TurnStarted {
        thread_id: String,
        turn_id: String,
    },
    AgentDelta {
        thread_id: String,
        turn_id: String,
        item_id: String,
        delta: String,
    },
    AgentCompleted {
        thread_id: String,
        turn_id: String,
        item_id: String,
        text: String,
    },
    TurnFinished {
        thread_id: String,
        turn_id: String,
        outcome: TurnOutcome,
    },
    TurnOperationFailed(String),
    ProcessExited(String),
    SafetyViolation(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Action {
    Intent(Intent),
    Event(DomainEvent),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AppState {
    pub connection: ConnectionState,
    pub auth: AuthState,
    pub thread: ThreadState,
    pub turn: TurnState,
    pub models: Vec<ModelChoice>,
    pub selected_model: Option<String>,
    pub selected_reasoning: Option<String>,
    pub transcript: Vec<TranscriptEntry>,
    pub preferences: PreferencesV1,
    pub notice: Option<String>,
    pub shutting_down: bool,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            connection: ConnectionState::Disconnected,
            auth: AuthState::Unknown,
            thread: ThreadState::None,
            turn: TurnState::Idle,
            models: Vec::new(),
            selected_model: None,
            selected_reasoning: None,
            transcript: Vec::new(),
            preferences: PreferencesV1::default(),
            notice: None,
            shutting_down: false,
        }
    }
}

impl AppState {
    pub fn reduce(&mut self, action: Action) -> Vec<Effect> {
        match action {
            Action::Intent(intent) => self.reduce_intent(intent),
            Action::Event(event) => self.reduce_event(event),
        }
    }

    fn reduce_intent(&mut self, intent: Intent) -> Vec<Effect> {
        self.notice = None;
        match intent {
            Intent::Help => self.notice = Some(HELP_TEXT.to_owned()),
            Intent::Quit => {
                self.shutting_down = true;
                let mut effects = Vec::new();
                if let TurnState::Streaming { thread_id, turn_id } = &self.turn {
                    effects.push(Effect::InterruptTurn {
                        thread_id: thread_id.clone(),
                        turn_id: turn_id.clone(),
                    });
                }
                effects.push(Effect::Shutdown);
                return effects;
            }
            Intent::ShowModels => {
                self.notice = Some(if self.models.is_empty() {
                    "model catalog is not available".to_owned()
                } else {
                    self.models
                        .iter()
                        .map(|model| model.id.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                });
            }
            Intent::ShowReasoning => {
                self.notice = Some(self.current_model().map_or_else(
                    || "select a model first".to_owned(),
                    |model| model.supported_reasoning_efforts.join(", "),
                ));
            }
            Intent::SelectModel(id) => {
                let Some(model) = self.models.iter().find(|model| model.id == id).cloned() else {
                    self.notice = Some(format!("unknown model {id}; use /model"));
                    return Vec::new();
                };
                let old_reasoning = self.selected_reasoning.clone();
                self.selected_model = Some(model.id.clone());
                self.selected_reasoning = old_reasoning
                    .clone()
                    .filter(|effort| model.supported_reasoning_efforts.contains(effort))
                    .or_else(|| Some(model.default_reasoning_effort.clone()));
                if old_reasoning.is_some() && old_reasoning != self.selected_reasoning {
                    self.notice =
                        Some("reasoning was reset to the selected model's default".to_owned());
                }
                self.sync_selection_preferences();
                return vec![Effect::Persist(self.preferences.clone())];
            }
            Intent::SelectReasoning(effort) => {
                let Some(model) = self.current_model() else {
                    self.notice = Some("select a model first".to_owned());
                    return Vec::new();
                };
                if !model.supported_reasoning_efforts.contains(&effort) {
                    self.notice = Some(format!("unsupported reasoning {effort}; use /reasoning"));
                    return Vec::new();
                }
                self.selected_reasoning = Some(effort);
                self.sync_selection_preferences();
                return vec![Effect::Persist(self.preferences.clone())];
            }
            Intent::Login => return self.begin_login(Effect::StartLogin),
            Intent::LoginDevice => return self.begin_login(Effect::StartDeviceLogin),
            Intent::Logout => {
                match &self.auth {
                    AuthState::SigningIn { login_id } => {
                        return vec![Effect::CancelLogin {
                            login_id: login_id.clone(),
                        }];
                    }
                    AuthState::SignedIn { .. } => return vec![Effect::Logout],
                    _ => {}
                }
                self.notice = Some("not signed in".to_owned());
            }
            Intent::Resume => {
                let Some(id) = self.preferences.thread_id.clone() else {
                    self.notice = Some("there is no saved thread to resume".to_owned());
                    return Vec::new();
                };
                match &self.auth {
                    AuthState::SignedIn {
                        scope: Some(current_scope),
                    } if self.preferences.account_scope.as_ref() == Some(current_scope) => {
                        self.thread = ThreadState::Resuming { id: id.clone() };
                        return vec![Effect::ResumeThread { id }];
                    }
                    AuthState::SignedIn { .. } => {
                        self.notice = Some(
                            "saved thread belongs to a different or unscoped account; sign in with the matching ChatGPT account"
                                .to_owned(),
                        );
                    }
                    _ => {
                        self.notice = Some("sign in with /login before resuming".to_owned());
                    }
                }
            }
            Intent::SendMessage(text) => {
                if let Some(reason) = self.send_block_reason() {
                    self.notice = Some(reason);
                } else {
                    self.turn = TurnState::Starting;
                    self.transcript.push(TranscriptEntry {
                        role: TranscriptRole::User,
                        text: text.clone(),
                        item_id: None,
                        turn_id: None,
                    });
                    return vec![Effect::SendMessage { text }];
                }
            }
            Intent::Interrupt => {
                if let TurnState::Streaming { thread_id, turn_id } = &self.turn {
                    return vec![Effect::InterruptTurn {
                        thread_id: thread_id.clone(),
                        turn_id: turn_id.clone(),
                    }];
                }
                self.notice = Some("there is no active turn to interrupt".to_owned());
            }
        }
        Vec::new()
    }

    fn reduce_event(&mut self, event: DomainEvent) -> Vec<Effect> {
        match event {
            DomainEvent::PreferencesLoaded(preferences) => {
                self.selected_model = preferences.model_id.clone();
                self.selected_reasoning = preferences.reasoning_effort.clone();
                self.preferences = preferences;
            }
            DomainEvent::Connecting => self.connection = ConnectionState::Connecting,
            DomainEvent::Connected { generation } => {
                self.connection = ConnectionState::Ready { generation }
            }
            DomainEvent::ConnectionFailed(message) | DomainEvent::ProcessExited(message) => {
                self.connection = ConnectionState::Failed(message.clone());
                if self.turn.is_active() {
                    self.turn = TurnState::Failed {
                        turn_id: None,
                        message,
                    };
                }
            }
            DomainEvent::AccountLoaded(scope) => {
                let completed_login = matches!(self.auth, AuthState::SigningIn { .. });
                self.auth = AuthState::SignedIn {
                    scope: scope.clone(),
                };
                if completed_login {
                    self.notice = Some("Signed in to ChatGPT".to_owned());
                }
                if let Some(id) = self.preferences.thread_id.clone() {
                    if scope.is_some() && scope == self.preferences.account_scope {
                        self.thread = ThreadState::Resuming { id: id.clone() };
                        return vec![Effect::ResumeThread { id }];
                    }
                    self.thread = ThreadState::AccountMismatch { id };
                    self.notice =
                        Some("saved thread belongs to a different or unscoped account".to_owned());
                }
            }
            DomainEvent::UnsupportedAccount(message) => self.auth = AuthState::Unsupported(message),
            DomainEvent::LoginStarted { login_id } => self.auth = AuthState::SigningIn { login_id },
            DomainEvent::LoginFailed(message) => {
                self.auth = AuthState::SignedOut;
                self.notice = Some(message);
            }
            DomainEvent::LoggedOut => {
                self.auth = AuthState::SignedOut;
                self.thread = ThreadState::None;
                self.turn = TurnState::Idle;
            }
            DomainEvent::CatalogLoaded(models) => {
                self.models = models;
                self.validate_selection();
            }
            DomainEvent::ResumeStarted { id } => self.thread = ThreadState::Resuming { id },
            DomainEvent::ResumeSucceeded { id, history } => {
                self.thread = ThreadState::Ready { id: id.clone() };
                self.transcript = history;
                self.preferences.thread_id = Some(id);
                return vec![Effect::Persist(self.preferences.clone())];
            }
            DomainEvent::ResumeFailed { id, message } => {
                self.thread = ThreadState::ResumeFailed {
                    id,
                    message: message.clone(),
                };
                self.notice = Some(message);
            }
            DomainEvent::ThreadStarted { id } => {
                self.thread = ThreadState::Ready { id: id.clone() };
                self.preferences.thread_id = Some(id);
                if let AuthState::SignedIn { scope } = &self.auth {
                    self.preferences.account_scope = scope.clone();
                }
                return vec![Effect::Persist(self.preferences.clone())];
            }
            DomainEvent::TurnStarted { thread_id, turn_id } => {
                let thread_matches = matches!(
                    &self.thread,
                    ThreadState::Ready { id } if id == &thread_id
                );
                let turn_matches = match &self.turn {
                    TurnState::Starting => true,
                    TurnState::Streaming {
                        thread_id: active_thread,
                        turn_id: active_turn,
                    } => active_thread == &thread_id && active_turn == &turn_id,
                    _ => false,
                };
                if thread_matches && turn_matches {
                    self.turn = TurnState::Streaming { thread_id, turn_id };
                }
            }
            DomainEvent::AgentDelta {
                thread_id,
                turn_id,
                item_id,
                delta,
            } => {
                if self.matches_active(&thread_id, &turn_id) {
                    self.append_delta(&turn_id, &item_id, &delta);
                }
            }
            DomainEvent::AgentCompleted {
                thread_id,
                turn_id,
                item_id,
                text,
            } => {
                if self.matches_active(&thread_id, &turn_id) {
                    if let Err(message) = self.reconcile_final(&turn_id, &item_id, &text) {
                        self.turn = TurnState::Failed {
                            turn_id: Some(turn_id),
                            message: message.clone(),
                        };
                        self.notice = Some(message);
                    }
                }
            }
            DomainEvent::TurnFinished {
                thread_id,
                turn_id,
                outcome,
            } => {
                if self.matches_active(&thread_id, &turn_id) {
                    self.turn = match outcome {
                        TurnOutcome::Completed => TurnState::Completed { turn_id },
                        TurnOutcome::Interrupted => TurnState::Interrupted { turn_id },
                        TurnOutcome::Failed(message) => TurnState::Failed {
                            turn_id: Some(turn_id),
                            message,
                        },
                    };
                }
            }
            DomainEvent::TurnOperationFailed(message) => {
                let turn_id = match &self.turn {
                    TurnState::Streaming { turn_id, .. } => Some(turn_id.clone()),
                    _ => None,
                };
                self.turn = TurnState::Failed {
                    turn_id,
                    message: message.clone(),
                };
                self.notice = Some(message);
            }
            DomainEvent::SafetyViolation(method) => {
                self.connection = ConnectionState::Failed(
                    "conversation safety boundary was triggered".to_owned(),
                );
                self.turn = TurnState::Failed {
                    turn_id: None,
                    message: "unexpected server request was denied".to_owned(),
                };
                self.notice = Some(format!("unexpected server request denied: {method}"));
            }
        }
        Vec::new()
    }

    fn current_model(&self) -> Option<&ModelChoice> {
        self.selected_model
            .as_ref()
            .and_then(|id| self.models.iter().find(|model| &model.id == id))
    }

    fn validate_selection(&mut self) {
        let had_saved_selection =
            self.preferences.model_id.is_some() || self.preferences.reasoning_effort.is_some();
        let selected = self
            .selected_model
            .as_ref()
            .and_then(|id| self.models.iter().find(|model| &model.id == id))
            .cloned()
            .or_else(|| self.models.iter().find(|model| model.is_default).cloned())
            .or_else(|| self.models.first().cloned());
        let Some(model) = selected else {
            self.selected_model = None;
            self.selected_reasoning = None;
            return;
        };
        self.selected_model = Some(model.id.clone());
        if !self
            .selected_reasoning
            .as_ref()
            .is_some_and(|effort| model.supported_reasoning_efforts.contains(effort))
        {
            self.selected_reasoning = Some(model.default_reasoning_effort.clone());
            if had_saved_selection {
                self.notice = Some(
                    "saved model or reasoning was unavailable; using the server default".to_owned(),
                );
            }
        }
        self.sync_selection_preferences();
    }

    fn sync_selection_preferences(&mut self) {
        self.preferences.model_id = self.selected_model.clone();
        self.preferences.reasoning_effort = self.selected_reasoning.clone();
    }

    fn begin_login(&mut self, effect: Effect) -> Vec<Effect> {
        if !matches!(self.connection, ConnectionState::Ready { .. }) {
            self.notice = Some("app-server is not connected".to_owned());
        } else if matches!(self.auth, AuthState::SignedOut) {
            return vec![effect];
        } else if matches!(self.auth, AuthState::SigningIn { .. }) {
            self.notice =
                Some("sign-in is already in progress; use /logout to cancel it".to_owned());
        } else {
            self.notice = Some("logout before starting another login".to_owned());
        }
        Vec::new()
    }

    fn send_block_reason(&self) -> Option<String> {
        if !matches!(self.connection, ConnectionState::Ready { .. }) {
            return Some("app-server is not connected".to_owned());
        }
        if !matches!(self.auth, AuthState::SignedIn { .. }) {
            return Some("sign in with /login before sending".to_owned());
        }
        if self.models.is_empty() || self.selected_model.is_none() {
            return Some("model catalog is not ready".to_owned());
        }
        if self.turn.is_active() {
            return Some("wait for or interrupt the active turn".to_owned());
        }
        if matches!(self.thread, ThreadState::Resuming { .. }) {
            return Some("wait for thread resume to finish".to_owned());
        }
        if matches!(
            self.thread,
            ThreadState::ResumeFailed { .. } | ThreadState::AccountMismatch { .. }
        ) {
            return Some(
                "resolve the saved thread with /resume or the matching account".to_owned(),
            );
        }
        None
    }

    fn matches_active(&self, thread_id: &str, turn_id: &str) -> bool {
        matches!(&self.turn, TurnState::Streaming { thread_id: expected_thread, turn_id: expected_turn } if expected_thread == thread_id && expected_turn == turn_id)
    }

    fn append_delta(&mut self, turn_id: &str, item_id: &str, delta: &str) {
        if let Some(entry) = self.transcript.iter_mut().find(|entry| {
            entry.item_id.as_deref() == Some(item_id) && entry.turn_id.as_deref() == Some(turn_id)
        }) {
            entry.text.push_str(delta);
        } else {
            self.transcript.push(TranscriptEntry {
                role: TranscriptRole::Assistant,
                text: delta.to_owned(),
                item_id: Some(item_id.to_owned()),
                turn_id: Some(turn_id.to_owned()),
            });
        }
    }

    fn reconcile_final(
        &mut self,
        turn_id: &str,
        item_id: &str,
        final_text: &str,
    ) -> Result<(), String> {
        if let Some(entry) = self.transcript.iter_mut().find(|entry| {
            entry.item_id.as_deref() == Some(item_id) && entry.turn_id.as_deref() == Some(turn_id)
        }) {
            if final_text.starts_with(&entry.text) {
                entry.text.push_str(&final_text[entry.text.len()..]);
                Ok(())
            } else {
                Err("assistant final snapshot contradicted streamed text".to_owned())
            }
        } else {
            self.transcript.push(TranscriptEntry {
                role: TranscriptRole::Assistant,
                text: final_text.to_owned(),
                item_id: Some(item_id.to_owned()),
                turn_id: Some(turn_id.to_owned()),
            });
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model(id: &str, default: bool, efforts: &[&str], default_effort: &str) -> ModelChoice {
        ModelChoice {
            id: id.to_owned(),
            display_name: id.to_owned(),
            is_default: default,
            default_reasoning_effort: default_effort.to_owned(),
            supported_reasoning_efforts: efforts.iter().map(|value| (*value).to_owned()).collect(),
        }
    }

    #[test]
    fn signed_out_send_is_local_and_same_account_auto_resumes() {
        let mut state = AppState::default();
        state.reduce(Action::Event(DomainEvent::Connected { generation: 1 }));
        let effects = state.reduce(Action::Intent(Intent::SendMessage("hello".to_owned())));
        assert!(effects.is_empty());
        assert!(state.notice.as_deref().unwrap().contains("sign in"));
        let scope = AccountScope::from_chatgpt_email("a@example.com");
        state.reduce(Action::Event(DomainEvent::PreferencesLoaded(
            PreferencesV1 {
                account_scope: scope.clone(),
                thread_id: Some("thr-old".to_owned()),
                ..PreferencesV1::default()
            },
        )));
        assert_eq!(
            state.reduce(Action::Event(DomainEvent::AccountLoaded(scope))),
            vec![Effect::ResumeThread {
                id: "thr-old".to_owned()
            }]
        );
    }

    #[test]
    fn signing_in_logout_cancels_the_active_login() {
        let mut state = AppState {
            connection: ConnectionState::Ready { generation: 1 },
            auth: AuthState::SigningIn {
                login_id: "login-active".to_owned(),
            },
            ..AppState::default()
        };

        assert_eq!(
            state.reduce(Action::Intent(Intent::Logout)),
            vec![Effect::CancelLogin {
                login_id: "login-active".to_owned(),
            }]
        );
        assert!(matches!(state.auth, AuthState::SigningIn { .. }));
    }

    #[test]
    fn completed_login_replaces_the_pending_browser_notice() {
        let mut state = AppState {
            auth: AuthState::SigningIn {
                login_id: "login-active".to_owned(),
            },
            notice: Some(
                "Complete sign-in in the browser; if it fails, use /logout then /login device"
                    .to_owned(),
            ),
            ..AppState::default()
        };

        state.reduce(Action::Event(DomainEvent::AccountLoaded(
            AccountScope::from_chatgpt_email("user@example.com"),
        )));

        assert_eq!(
            state.auth,
            AuthState::SignedIn {
                scope: AccountScope::from_chatgpt_email("user@example.com"),
            }
        );
        assert_eq!(state.notice.as_deref(), Some("Signed in to ChatGPT"));
    }

    #[test]
    fn account_switch_and_logout_replace_and_remove_the_runtime_identity() {
        let mut state = AppState::default();

        state.reduce(Action::Event(DomainEvent::AccountLoaded(
            AccountScope::from_chatgpt_email("first@example.com"),
        )));
        assert_eq!(
            state.auth,
            AuthState::SignedIn {
                scope: AccountScope::from_chatgpt_email("first@example.com"),
            }
        );

        state.reduce(Action::Event(DomainEvent::AccountLoaded(
            AccountScope::from_chatgpt_email("second@example.com"),
        )));
        assert_eq!(
            state.auth,
            AuthState::SignedIn {
                scope: AccountScope::from_chatgpt_email("second@example.com"),
            }
        );

        state.reduce(Action::Event(DomainEvent::LoggedOut));
        assert_eq!(state.auth, AuthState::SignedOut);
    }

    #[test]
    fn account_change_and_resume_failure_preserve_saved_id() {
        let mut state = AppState::default();
        state.preferences.thread_id = Some("thr-old".to_owned());
        state.preferences.account_scope = AccountScope::from_chatgpt_email("old@example.com");
        state.reduce(Action::Event(DomainEvent::AccountLoaded(
            AccountScope::from_chatgpt_email("new@example.com"),
        )));
        assert!(matches!(state.thread, ThreadState::AccountMismatch { .. }));
        state.reduce(Action::Event(DomainEvent::ResumeFailed {
            id: "thr-old".to_owned(),
            message: "stale".to_owned(),
        }));
        assert_eq!(state.preferences.thread_id.as_deref(), Some("thr-old"));
    }

    #[test]
    fn manual_resume_requires_matching_scope_and_retries_same_account_failure() {
        let saved_scope = AccountScope::from_chatgpt_email("saved@example.com");
        let mut state = AppState {
            auth: AuthState::SignedIn {
                scope: AccountScope::from_chatgpt_email("other@example.com"),
            },
            thread: ThreadState::AccountMismatch {
                id: "thr-saved".to_owned(),
            },
            preferences: PreferencesV1 {
                account_scope: saved_scope.clone(),
                thread_id: Some("thr-saved".to_owned()),
                ..PreferencesV1::default()
            },
            ..AppState::default()
        };

        assert!(state.reduce(Action::Intent(Intent::Resume)).is_empty());
        assert_eq!(state.preferences.thread_id.as_deref(), Some("thr-saved"));
        assert!(state
            .notice
            .as_deref()
            .unwrap()
            .contains("matching ChatGPT"));

        state.auth = AuthState::SignedIn { scope: None };
        assert!(state.reduce(Action::Intent(Intent::Resume)).is_empty());

        state.auth = AuthState::SignedIn { scope: saved_scope };
        state.thread = ThreadState::ResumeFailed {
            id: "thr-saved".to_owned(),
            message: "temporary failure".to_owned(),
        };
        assert_eq!(
            state.reduce(Action::Intent(Intent::Resume)),
            vec![Effect::ResumeThread {
                id: "thr-saved".to_owned()
            }]
        );
        assert!(matches!(
            state.thread,
            ThreadState::Resuming { ref id } if id == "thr-saved"
        ));
    }

    #[test]
    fn catalog_falls_back_and_model_change_revalidates_reasoning() {
        let mut state = AppState {
            selected_model: Some("missing".to_owned()),
            selected_reasoning: Some("max".to_owned()),
            ..AppState::default()
        };
        state.reduce(Action::Event(DomainEvent::CatalogLoaded(vec![
            model("m1", true, &["low", "high"], "high"),
            model("m2", false, &["low"], "low"),
        ])));
        assert_eq!(state.selected_model.as_deref(), Some("m1"));
        assert_eq!(state.selected_reasoning.as_deref(), Some("high"));
        state.reduce(Action::Intent(Intent::SelectModel("m2".to_owned())));
        assert_eq!(state.selected_reasoning.as_deref(), Some("low"));
    }

    #[test]
    fn first_catalog_default_does_not_claim_a_saved_selection_failed() {
        let mut state = AppState::default();
        state.reduce(Action::Event(DomainEvent::CatalogLoaded(vec![model(
            "m1",
            true,
            &["low"],
            "low",
        )])));
        assert_eq!(state.selected_model.as_deref(), Some("m1"));
        assert_eq!(state.notice, None);
    }

    #[test]
    fn scoped_streaming_reconciles_utf8_suffix_without_duplication() {
        let mut state = AppState {
            turn: TurnState::Streaming {
                thread_id: "thr".to_owned(),
                turn_id: "turn".to_owned(),
            },
            ..AppState::default()
        };
        state.reduce(Action::Event(DomainEvent::AgentDelta {
            thread_id: "other".to_owned(),
            turn_id: "turn".to_owned(),
            item_id: "item".to_owned(),
            delta: "ignored".to_owned(),
        }));
        state.reduce(Action::Event(DomainEvent::AgentDelta {
            thread_id: "thr".to_owned(),
            turn_id: "turn".to_owned(),
            item_id: "item".to_owned(),
            delta: "hé".to_owned(),
        }));
        state.reduce(Action::Event(DomainEvent::AgentCompleted {
            thread_id: "thr".to_owned(),
            turn_id: "turn".to_owned(),
            item_id: "item".to_owned(),
            text: "héllo".to_owned(),
        }));
        assert_eq!(state.transcript[0].text, "héllo");
        state.reduce(Action::Event(DomainEvent::TurnFinished {
            thread_id: "thr".to_owned(),
            turn_id: "turn".to_owned(),
            outcome: TurnOutcome::Interrupted,
        }));
        assert!(matches!(state.turn, TurnState::Interrupted { .. }));
    }

    #[test]
    fn unrelated_turn_started_cannot_replace_the_active_turn() {
        let mut state = AppState {
            thread: ThreadState::Ready {
                id: "thr-active".to_owned(),
            },
            turn: TurnState::Streaming {
                thread_id: "thr-active".to_owned(),
                turn_id: "turn-active".to_owned(),
            },
            ..AppState::default()
        };
        state.reduce(Action::Event(DomainEvent::TurnStarted {
            thread_id: "thr-other".to_owned(),
            turn_id: "turn-other".to_owned(),
        }));
        assert_eq!(
            state.turn,
            TurnState::Streaming {
                thread_id: "thr-active".to_owned(),
                turn_id: "turn-active".to_owned(),
            }
        );
    }

    #[test]
    fn contradictory_final_snapshot_fails_the_turn() {
        let mut state = AppState {
            turn: TurnState::Streaming {
                thread_id: "thr".to_owned(),
                turn_id: "turn".to_owned(),
            },
            ..AppState::default()
        };
        state.append_delta("turn", "item", "alpha");
        state.reduce(Action::Event(DomainEvent::AgentCompleted {
            thread_id: "thr".to_owned(),
            turn_id: "turn".to_owned(),
            item_id: "item".to_owned(),
            text: "beta".to_owned(),
        }));
        assert!(matches!(state.turn, TurnState::Failed { .. }));
    }

    #[test]
    fn quitting_interrupts_before_the_ordered_shutdown_path() {
        let mut state = AppState {
            turn: TurnState::Streaming {
                thread_id: "thr".to_owned(),
                turn_id: "turn".to_owned(),
            },
            ..AppState::default()
        };
        let effects = state.reduce(Action::Intent(Intent::Quit));
        assert!(matches!(
            effects.as_slice(),
            [Effect::InterruptTurn { .. }, Effect::Shutdown]
        ));
        assert!(state.shutting_down);
    }
}
