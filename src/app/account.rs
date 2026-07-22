use super::*;

impl AppState {
    pub(in crate::app) fn reduce_account_event(&mut self, event: DomainEvent) -> Vec<Effect> {
        match event {
            DomainEvent::PreferencesLoaded(mut preferences) => {
                if let (Some(id), Some(scope)) = (
                    preferences.thread_id.clone(),
                    preferences.account_scope.clone(),
                ) {
                    preferences.thread_account_scopes.entry(id).or_insert(scope);
                }
                self.selected_model = preferences.model_id.clone();
                self.selected_reasoning = preferences.reasoning_effort.clone();
                self.preferences = preferences;
                self.reset_context_window();
            }
            DomainEvent::Connecting => {
                self.connection = ConnectionState::Connecting;
                self.reset_context_window();
            }
            DomainEvent::Connected { generation } => {
                self.connection = ConnectionState::Ready { generation }
            }
            DomainEvent::ConnectionFailed(message) | DomainEvent::ProcessExited(message) => {
                self.connection = ConnectionState::Failed(message.clone());
                self.pending_new_thread_scope = None;
                self.pending_thread_deletions = None;
                if let Some(picker) = &mut self.thread_picker {
                    picker.phase = ThreadPickerPhase::Failed;
                    picker.confirmation = None;
                    picker.message = Some(message.clone());
                }
                if self.turn.is_active() {
                    self.turn = TurnState::Failed {
                        turn_id: None,
                        message,
                    };
                }
            }
            DomainEvent::AccountLoaded(scope) => {
                let completed_login = matches!(self.auth, AuthState::SigningIn { .. });
                let picker_was_open = self.thread_picker.is_some();
                let same_account = matches!(
                    &self.auth,
                    AuthState::SignedIn { scope: current } if current == &scope
                );
                let switched_accounts = matches!(
                    &self.auth,
                    AuthState::SignedIn { scope: current } if current != &scope
                );
                if !same_account {
                    self.reset_context_window();
                    self.thinking.clear_content();
                }
                if self.pending_new_thread_scope.as_ref() != Some(&scope) {
                    self.pending_new_thread_scope = None;
                }
                if switched_accounts {
                    // An account update can arrive while a turn is active. Detach the old turn
                    // before changing the displayed identity so every subsequently queued event
                    // from that turn becomes stale.
                    self.turn = TurnState::Idle;
                    self.thread_picker = None;
                    self.pending_thread_deletions = None;
                }
                self.auth = AuthState::SignedIn {
                    scope: scope.clone(),
                };
                if completed_login {
                    self.notice = Some("Signed in to ChatGPT".to_owned());
                }
                if let Some(id) = self.preferences.thread_id.clone() {
                    if scope.is_some() && scope == self.preferences.account_scope {
                        // Account refresh notifications are not lifecycle requests. Only attach
                        // the saved thread when startup or login left us without a thread; an
                        // already-ready or in-flight resume must remain untouched.
                        if !picker_was_open && matches!(self.thread, ThreadState::None) {
                            self.thread = ThreadState::Resuming { id: id.clone() };
                            return vec![Effect::ResumeThread { id }];
                        }
                        return Vec::new();
                    }
                    self.thread_picker = None;
                    self.thread = ThreadState::AccountMismatch { id };
                    self.notice =
                        Some("saved thread belongs to a different or unscoped account".to_owned());
                } else if switched_accounts {
                    self.thread = ThreadState::None;
                }
            }
            DomainEvent::UnsupportedAccount(message) => {
                self.auth = AuthState::Unsupported(message);
                self.pending_new_thread_scope = None;
                self.pending_thread_deletions = None;
                self.turn = TurnState::Idle;
                self.thread_picker = None;
                self.thinking.clear_content();
                self.reset_context_window();
                self.thread = self
                    .preferences
                    .thread_id
                    .clone()
                    .map_or(ThreadState::None, |id| ThreadState::AccountMismatch { id });
            }
            DomainEvent::LoginStarted { login_id } => self.auth = AuthState::SigningIn { login_id },
            DomainEvent::LoginFailed(message) => {
                self.auth = AuthState::SignedOut;
                self.pending_new_thread_scope = None;
                self.pending_thread_deletions = None;
                self.notice = Some(message);
                self.reset_context_window();
            }
            DomainEvent::LoggedOut => {
                self.auth = AuthState::SignedOut;
                self.pending_new_thread_scope = None;
                self.pending_thread_deletions = None;
                self.thread = ThreadState::None;
                self.turn = TurnState::Idle;
                self.thread_picker = None;
                self.thinking.clear_content();
                self.reset_context_window();
            }
            DomainEvent::CatalogLoaded(models) => {
                let previous_model = self.selected_model.clone();
                self.models = models;
                self.validate_selection();
                if previous_model != self.selected_model {
                    self.invalidate_context_for_current_turn();
                }
            }
            _ => unreachable!("event routed to the wrong reducer"),
        }
        Vec::new()
    }
}

impl AppState {
    pub(in crate::app) fn current_model(&self) -> Option<&ModelChoice> {
        self.selected_model
            .as_ref()
            .and_then(|id| self.models.iter().find(|model| &model.id == id))
    }

    pub(in crate::app) fn thread_action_block_reason(
        &self,
        require_account_identity: bool,
    ) -> Option<String> {
        if self.pending_new_thread_scope.is_some() {
            return Some("wait for the new thread request to finish".to_owned());
        }
        if self.thread_picker.is_some() {
            return Some(
                "close the thread picker before starting another thread action".to_owned(),
            );
        }
        if !matches!(self.connection, ConnectionState::Ready { .. }) {
            return Some("app-server is not connected".to_owned());
        }
        let AuthState::SignedIn { scope } = &self.auth else {
            return Some("sign in with /login before managing threads".to_owned());
        };
        if self.turn.is_active() {
            return Some("wait for or interrupt the active turn".to_owned());
        }
        if require_account_identity && scope.is_none() {
            return Some(
                "ChatGPT account identity is unavailable; thread history cannot be opened safely"
                    .to_owned(),
            );
        }
        if self.models.is_empty() || self.selected_model.is_none() {
            return Some("model catalog is not ready".to_owned());
        }
        None
    }

    pub(in crate::app) fn validate_selection(&mut self) {
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

    pub(in crate::app) fn sync_selection_preferences(&mut self) {
        self.preferences.model_id = self.selected_model.clone();
        self.preferences.reasoning_effort = self.selected_reasoning.clone();
    }

    pub(in crate::app) fn begin_login(&mut self, effect: Effect) -> Vec<Effect> {
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

    pub(in crate::app) fn send_block_reason(&self) -> Option<String> {
        if !matches!(self.connection, ConnectionState::Ready { .. }) {
            return Some("app-server is not connected".to_owned());
        }
        if !matches!(self.auth, AuthState::SignedIn { .. }) {
            return Some("sign in with /login before sending".to_owned());
        }
        if self.models.is_empty() || self.selected_model.is_none() {
            return Some("model catalog is not ready".to_owned());
        }
        if self.pending_new_thread_scope.is_some() {
            return Some("wait for the new thread request to finish".to_owned());
        }
        if self.turn.is_active() {
            return Some("wait for or interrupt the active turn".to_owned());
        }
        if self.thread_picker.is_some() {
            return Some("close the thread picker before sending".to_owned());
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
}
