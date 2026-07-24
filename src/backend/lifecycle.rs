use super::*;

mod claude_events;
mod openrouter_events;
mod restart;
mod runtime_events;
mod shutdown;
mod startup;

impl<P: PreferencesPort, B: BrowserOpener> BackendCoordinator<P, B> {
    pub fn new(session: SessionService, preferences: P, browser: B) -> Self {
        Self {
            state: AppState::default(),
            session: Some(session),
            openrouter: None,
            claude: None,
            preferences,
            browser,
            may_persist: false,
            pending_openrouter_auto_resume: None,
            completed_items: CompletedItemTracker::default(),
        }
    }

    pub fn without_codex(preferences: P, browser: B, message: String) -> Self {
        let state = AppState {
            connection: crate::app::ConnectionState::Failed(message),
            ..AppState::default()
        };
        Self {
            state,
            session: None,
            openrouter: None,
            claude: None,
            preferences,
            browser,
            may_persist: false,
            pending_openrouter_auto_resume: None,
            completed_items: CompletedItemTracker::default(),
        }
    }

    pub(in crate::backend) fn persist_preferences(
        &mut self,
        preferences: &crate::persistence::PreferencesV3,
    ) -> Result<Option<crate::storage::CommitStatus>, PersistenceError> {
        if !self.may_persist {
            return Ok(None);
        }
        let status = self.preferences.save_with_commit(preferences)?;
        if status == crate::storage::CommitStatus::CommittedUnverified {
            self.state.notice = Some(
                "preferences were updated, but directory durability could not be verified; a later save will retry"
                    .to_owned(),
            );
        }
        Ok(Some(status))
    }

    pub(in crate::backend) fn codex_mut(&mut self) -> Result<&mut SessionService, BackendError> {
        self.session.as_mut().ok_or(BackendError::CodexUnavailable)
    }

    pub fn with_openrouter(mut self, openrouter: OpenRouterService) -> Self {
        self.openrouter = Some(openrouter);
        self
    }

    pub fn with_claude(mut self, claude: ClaudeBackendRuntime) -> Self {
        self.claude = Some(claude);
        self
    }

    pub fn record_claude_unavailable(&mut self, message: impl Into<String>) {
        self.state.reduce(Action::Event(DomainEvent::ClaudeStartup {
            availability: crate::app::ClaudeAvailability::Unavailable(message.into()),
            auth: ClaudeAuthStatus::CliUnavailable,
        }));
    }

    pub fn state(&self) -> &AppState {
        &self.state
    }

    pub fn accept_intent(&mut self, intent: Intent) -> Vec<Effect> {
        self.state.reduce(Action::Intent(intent))
    }

    pub fn accept_openrouter_credential(
        &mut self,
        value: crate::credentials::SecretValue,
    ) -> Result<(), crate::credentials::SecretValue> {
        if matches!(
            self.state.turn,
            crate::app::TurnState::OpenRouterStreaming { .. }
        ) || (self.state.active_provider == ProviderId::OpenRouter
            && self.state.turn == crate::app::TurnState::Starting)
        {
            self.state.notice = Some(
                "wait for or interrupt the active OpenRouter turn before replacing its credential"
                    .to_owned(),
            );
            return Err(value);
        }
        let Some(openrouter) = &mut self.openrouter else {
            self.state.notice = Some("OpenRouter runtime is unavailable".to_owned());
            return Err(value);
        };
        match openrouter.replace_candidate(value) {
            Ok(operation_id) => {
                self.state.openrouter.credential_validation =
                    crate::app::OpenRouterCredentialValidation::Validating {
                        operation_id,
                        candidate_saved: false,
                    };
                Ok(())
            }
            Err(value) => {
                self.state.notice = Some(
                    "an OpenRouter authentication operation is already active; retry after it settles"
                        .to_owned(),
                );
                Err(value)
            }
        }
    }

    pub async fn execute_pending(&mut self, effects: Vec<Effect>) -> Result<(), BackendError> {
        self.execute_effects(effects).await
    }

    pub fn record_error(&mut self, message: impl Into<String>) {
        self.state.notice = Some(message.into());
    }

    pub fn record_openrouter_unavailable(&mut self) {
        self.state.openrouter.auth = OpenRouterAuthStatus::CredentialUnavailable;
    }

    pub async fn handle_intent(&mut self, intent: Intent) -> Result<(), BackendError> {
        let effects = self.accept_intent(intent);
        self.execute_pending(effects).await
    }
}

pub(in crate::backend) use openrouter_events::openrouter_history;
