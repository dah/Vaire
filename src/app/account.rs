use super::*;

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

impl AppState {
    pub(in crate::app) fn current_model(&self) -> Option<&ModelChoice> {
        if self.active_provider != ProviderId::Codex {
            return None;
        }
        self.selected_model
            .as_ref()
            .filter(|key| key.provider == ProviderId::Codex)
            .and_then(|key| self.models.iter().find(|model| model.id == key.id))
    }

    pub(in crate::app) fn thread_action_block_reason(
        &self,
        require_account_identity: bool,
    ) -> Option<String> {
        if self.pending_new_thread_scope.is_some() {
            return Some("wait for the new thread request to finish".to_owned());
        }
        if self.conversation_popup().is_some() {
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
        if self.models.is_empty()
            || !matches!(
                self.selected_model,
                Some(ModelKey {
                    provider: ProviderId::Codex,
                    ..
                })
            )
        {
            return Some("model catalog is not ready".to_owned());
        }
        None
    }

    pub(in crate::app) fn validate_selection(&mut self) {
        if self.active_provider != ProviderId::Codex {
            return;
        }
        let had_saved_selection = self.preferences.codex.model_id.is_some()
            || self.preferences.codex.reasoning_effort.is_some();
        let selected = self
            .selected_model
            .as_ref()
            .filter(|key| key.provider == ProviderId::Codex)
            .and_then(|key| self.models.iter().find(|model| model.id == key.id))
            .cloned()
            .or_else(|| self.models.iter().find(|model| model.is_default).cloned())
            .or_else(|| self.models.first().cloned());
        let Some(model) = selected else {
            self.selected_model = None;
            self.selected_reasoning = None;
            return;
        };
        self.selected_model = Some(model.key());
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
        self.preferences.codex.model_id = self
            .selected_model
            .as_ref()
            .filter(|key| key.provider == ProviderId::Codex)
            .map(|key| key.id.clone());
        self.preferences.codex.reasoning_effort = self.selected_reasoning.clone();
    }

    pub(in crate::app) fn sync_active_selection_preferences(&mut self) {
        self.preferences.active_provider = self.active_provider;
        match self.active_provider {
            ProviderId::Codex => self.sync_selection_preferences(),
            ProviderId::OpenRouter => {
                self.preferences.openrouter.selected_model_id = self
                    .selected_model
                    .as_ref()
                    .filter(|key| key.provider == ProviderId::OpenRouter)
                    .map(|key| key.id.clone());
            }
        }
    }

    pub(in crate::app) fn available_model_keys(&self) -> Vec<ModelKey> {
        let mut keys = self.models.iter().map(ModelChoice::key).collect::<Vec<_>>();
        keys.extend(self.openrouter.catalog.iter().filter_map(|model| {
            self.preferences
                .openrouter
                .enabled_model_ids
                .contains(&model.id)
                .then(|| ModelKey::openrouter(model.id.clone()).ok())
                .flatten()
        }));
        keys
    }

    pub(crate) fn model_key_is_available(&self, key: &ModelKey) -> bool {
        match key.provider {
            ProviderId::Codex => self.models.iter().any(|model| model.id == key.id),
            ProviderId::OpenRouter => {
                self.preferences
                    .openrouter
                    .enabled_model_ids
                    .contains(&key.id)
                    && self
                        .openrouter
                        .catalog
                        .iter()
                        .any(|model| model.id == key.id)
            }
        }
    }

    pub(crate) fn resolve_provider_selection(
        &self,
        provider: ProviderId,
    ) -> Option<(ModelKey, Option<String>)> {
        match provider {
            ProviderId::Codex => {
                let model = self
                    .preferences
                    .codex
                    .model_id
                    .as_ref()
                    .and_then(|id| self.models.iter().find(|model| &model.id == id))
                    .or_else(|| {
                        self.selected_model
                            .as_ref()
                            .filter(|key| key.provider == ProviderId::Codex)
                            .and_then(|key| self.models.iter().find(|model| model.id == key.id))
                    })
                    .or_else(|| self.models.iter().find(|model| model.is_default))
                    .or_else(|| self.models.first())?;
                let reasoning = self
                    .preferences
                    .codex
                    .reasoning_effort
                    .as_ref()
                    .filter(|effort| model.supported_reasoning_efforts.contains(*effort))
                    .cloned()
                    .unwrap_or_else(|| model.default_reasoning_effort.clone());
                Some((model.key(), Some(reasoning)))
            }
            ProviderId::OpenRouter => {
                let preferred = self
                    .preferences
                    .openrouter
                    .selected_model_id
                    .as_ref()
                    .and_then(|id| ModelKey::openrouter(id.clone()).ok())
                    .filter(|key| self.model_key_is_available(key))
                    .or_else(|| {
                        self.selected_model
                            .as_ref()
                            .filter(|key| {
                                key.provider == ProviderId::OpenRouter
                                    && self.model_key_is_available(key)
                            })
                            .cloned()
                    });
                if let Some(key) = preferred {
                    return Some((key, None));
                }
                let mut enabled = self
                    .openrouter
                    .catalog
                    .iter()
                    .filter(|model| {
                        self.preferences
                            .openrouter
                            .enabled_model_ids
                            .contains(&model.id)
                    })
                    .collect::<Vec<_>>();
                enabled.sort_by(|left, right| {
                    left.name
                        .as_deref()
                        .unwrap_or(&left.id)
                        .cmp(right.name.as_deref().unwrap_or(&right.id))
                        .then_with(|| left.id.cmp(&right.id))
                });
                enabled
                    .first()
                    .and_then(|model| ModelKey::openrouter(model.id.clone()).ok())
                    .map(|key| (key, None))
            }
        }
    }

    pub(in crate::app) fn commit_provider_selection(
        &mut self,
        provider: ProviderId,
        model: ModelKey,
        reasoning: Option<String>,
    ) -> bool {
        if model.provider != provider || !self.model_key_is_available(&model) {
            return false;
        }
        let reasoning = match provider {
            ProviderId::Codex => {
                let Some(choice) = self.models.iter().find(|choice| choice.id == model.id) else {
                    return false;
                };
                Some(
                    reasoning
                        .filter(|effort| choice.supported_reasoning_efforts.contains(effort))
                        .unwrap_or_else(|| choice.default_reasoning_effort.clone()),
                )
            }
            ProviderId::OpenRouter => None,
        };
        self.active_provider = provider;
        self.selected_model = Some(model);
        self.selected_reasoning = reasoning;
        self.sync_active_selection_preferences();
        true
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
        if self.active_provider == ProviderId::OpenRouter {
            if self.openrouter.auth != crate::openrouter::OpenRouterAuthStatus::Valid {
                return Some("configure OpenRouter with /login before sending".to_owned());
            }
            let Some(model) = self
                .selected_model
                .as_ref()
                .filter(|key| key.provider == ProviderId::OpenRouter)
            else {
                return Some("select an enabled OpenRouter model with /model".to_owned());
            };
            if !self.model_key_is_available(model) {
                return Some("select an enabled OpenRouter model with /model".to_owned());
            }
            if self.turn.is_active() {
                return Some("wait for or interrupt the active turn".to_owned());
            }
            if self.conversation_popup().is_some() {
                return Some("close the conversation picker before sending".to_owned());
            }
            return None;
        }
        if !matches!(self.connection, ConnectionState::Ready { .. }) {
            return Some("app-server is not connected".to_owned());
        }
        if !matches!(self.auth, AuthState::SignedIn { .. }) {
            return Some("sign in with /login before sending".to_owned());
        }
        if self.models.is_empty()
            || !matches!(
                self.selected_model,
                Some(ModelKey {
                    provider: ProviderId::Codex,
                    ..
                })
            )
        {
            return Some("model catalog is not ready".to_owned());
        }
        if self.pending_new_thread_scope.is_some() {
            return Some("wait for the new thread request to finish".to_owned());
        }
        if self.turn.is_active() {
            return Some("wait for or interrupt the active turn".to_owned());
        }
        if self.conversation_popup().is_some() {
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
