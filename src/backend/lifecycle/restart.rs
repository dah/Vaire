use super::*;

impl<P: PreferencesPort, B: BrowserOpener> BackendCoordinator<P, B> {
    pub async fn replace_session_and_restart(
        &mut self,
        session: SessionService,
    ) -> Result<(), BackendError> {
        if let Some(current) = &mut self.session {
            let _ = current.shutdown().await;
        }
        self.session = Some(session);
        self.completed_items.reset();
        self.state.connection = crate::app::ConnectionState::Disconnected;
        if self.state.active_provider == ProviderId::Codex {
            self.state.turn = crate::app::TurnState::Idle;
            return self.startup().await;
        }

        // A Codex-only reconnect must not replay shared startup and detach an active
        // OpenRouter conversation or turn.
        self.state.reduce(Action::Event(DomainEvent::Connecting));
        let initialized = match self.codex_mut()?.initialize().await {
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
        let generation = self.codex_mut()?.generation();
        self.state
            .reduce(Action::Event(DomainEvent::Connected { generation }));
        let account = self.codex_mut()?.read_account().await?;
        let models = self.codex_mut()?.list_models().await?;
        self.state
            .reduce(Action::Event(DomainEvent::CatalogLoaded(model_choices(
                &models,
            ))));
        let effects = self.reduce_account(account);
        self.execute_effects(effects).await
    }
}
