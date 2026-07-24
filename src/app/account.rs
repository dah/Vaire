use super::*;

mod guards;
mod selection;

impl AppState {
    pub(in crate::app) fn reduce_account_event(&mut self, event: DomainEvent) -> Vec<Effect> {
        match event {
            DomainEvent::PreferencesLoaded(mut preferences) => {
                if let (Some(id), Some(scope)) = (
                    preferences.codex.auto_resume_thread_id.clone(),
                    preferences.codex.account_scope.clone(),
                ) {
                    preferences
                        .codex
                        .thread_account_scopes
                        .entry(id)
                        .or_insert(scope);
                }
                self.active_provider = preferences.active_provider;
                self.selected_model = match preferences.active_provider {
                    ProviderId::Codex => preferences
                        .codex
                        .model_id
                        .clone()
                        .and_then(|id| ModelKey::codex(id).ok()),
                    ProviderId::OpenRouter => preferences
                        .openrouter
                        .selected_model_id
                        .clone()
                        .and_then(|id| ModelKey::openrouter(id).ok()),
                };
                self.selected_reasoning = (preferences.active_provider == ProviderId::Codex)
                    .then(|| preferences.codex.reasoning_effort.clone())
                    .flatten();
                self.preferences = preferences;
                self.reset_context_window();
            }
            DomainEvent::Connecting => {
                self.connection = ConnectionState::Connecting;
                if self.active_provider == ProviderId::Codex {
                    self.reset_context_window();
                }
            }
            DomainEvent::Connected { generation } => {
                self.connection = ConnectionState::Ready { generation }
            }
            DomainEvent::ConnectionFailed(message) | DomainEvent::ProcessExited(message) => {
                self.connection = ConnectionState::Failed(message.clone());
                if self.active_provider == ProviderId::Codex {
                    self.pending_new_thread_scope = None;
                    self.pending_thread_deletions = None;
                    if let Some(picker) = self.conversation_popup_mut() {
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
            }
            DomainEvent::AccountLoaded(scope) => {
                let completed_login = matches!(self.auth, AuthState::SigningIn { .. });
                if self.active_provider == ProviderId::OpenRouter {
                    self.auth = AuthState::SignedIn { scope };
                    if completed_login {
                        self.notice = Some("Signed in to ChatGPT".to_owned());
                    }
                    return Vec::new();
                }
                let picker_was_open = self.conversation_popup().is_some();
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
                    self.close_conversation_popup();
                    self.pending_thread_deletions = None;
                }
                self.auth = AuthState::SignedIn {
                    scope: scope.clone(),
                };
                if completed_login {
                    self.notice = Some("Signed in to ChatGPT".to_owned());
                }
                if let Some(id) = self.preferences.codex.auto_resume_thread_id.clone() {
                    if scope.is_some() && scope == self.preferences.codex.account_scope {
                        // Account refresh notifications are not lifecycle requests. Only attach
                        // the saved thread when startup or login left us without a thread; an
                        // already-ready or in-flight resume must remain untouched.
                        if !picker_was_open && matches!(self.thread, ThreadState::None) {
                            self.thread = ThreadState::Resuming { id: id.clone() };
                            return vec![Effect::ResumeThread { id }];
                        }
                        return Vec::new();
                    }
                    self.close_conversation_popup();
                    self.thread = ThreadState::AccountMismatch { id };
                    self.notice =
                        Some("saved thread belongs to a different or unscoped account".to_owned());
                } else if switched_accounts {
                    self.thread = ThreadState::None;
                }
            }
            DomainEvent::UnsupportedAccount(message) => {
                self.auth = AuthState::Unsupported(message);
                if self.active_provider == ProviderId::OpenRouter {
                    return Vec::new();
                }
                self.pending_new_thread_scope = None;
                self.pending_thread_deletions = None;
                self.turn = TurnState::Idle;
                self.close_conversation_popup();
                self.thinking.clear_content();
                self.reset_context_window();
                self.thread = self
                    .preferences
                    .codex
                    .auto_resume_thread_id
                    .clone()
                    .map_or(ThreadState::None, |id| ThreadState::AccountMismatch { id });
            }
            DomainEvent::LoginStarted { login_id } => self.auth = AuthState::SigningIn { login_id },
            DomainEvent::LoginFailed(message) => {
                self.auth = AuthState::SignedOut;
                self.notice = Some(message);
                if self.active_provider == ProviderId::Codex {
                    self.pending_new_thread_scope = None;
                    self.pending_thread_deletions = None;
                    self.reset_context_window();
                }
            }
            DomainEvent::LoggedOut => {
                self.auth = AuthState::SignedOut;
                if self.active_provider == ProviderId::OpenRouter {
                    return Vec::new();
                }
                self.pending_new_thread_scope = None;
                self.pending_thread_deletions = None;
                self.thread = ThreadState::None;
                self.turn = TurnState::Idle;
                self.close_conversation_popup();
                self.thinking.clear_content();
                self.reset_context_window();
            }
            DomainEvent::CatalogLoaded(models) => {
                let previous_model = self.selected_model.clone();
                self.models = models;
                if self.active_provider == ProviderId::Codex {
                    self.validate_selection();
                    if previous_model != self.selected_model {
                        self.invalidate_context_for_current_turn();
                    }
                }
            }
            _ => unreachable!("event routed to the wrong reducer"),
        }
        Vec::new()
    }
}
