use super::*;

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
}
