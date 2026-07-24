use super::*;

impl<P: PreferencesPort, B: BrowserOpener> BackendCoordinator<P, B> {
    /// Receives and parses exactly one transport event without starting any follow-up RPCs.
    ///
    /// The runtime selects this cancellation-safe boundary against user input. Once it returns,
    /// `process_received_event` must be allowed to finish so an already-consumed event cannot be
    /// lost while, for example, an account update waits for `account/read`.
    /// Convenience path for sequential tests and callers.
    ///
    /// Do not race this combined future against unrelated work: use `receive_event` followed by
    /// `process_received_event` so cancellation cannot land between receipt and processing.
    pub async fn receive_event(&mut self) -> BackendRuntimeEvent {
        match (&mut self.session, &mut self.openrouter, &mut self.claude) {
            (Some(session), Some(openrouter), Some(claude)) => tokio::select! {
                event = session.next_event() => BackendRuntimeEvent::Codex(event),
                event = openrouter.next_event() => BackendRuntimeEvent::OpenRouter(event),
                event = claude.service.next_event() => BackendRuntimeEvent::Claude(event),
            },
            (Some(session), Some(openrouter), None) => tokio::select! {
                event = session.next_event() => BackendRuntimeEvent::Codex(event),
                event = openrouter.next_event() => BackendRuntimeEvent::OpenRouter(event),
            },
            (Some(session), None, Some(claude)) => tokio::select! {
                event = session.next_event() => BackendRuntimeEvent::Codex(event),
                event = claude.service.next_event() => BackendRuntimeEvent::Claude(event),
            },
            (None, Some(openrouter), Some(claude)) => tokio::select! {
                event = openrouter.next_event() => BackendRuntimeEvent::OpenRouter(event),
                event = claude.service.next_event() => BackendRuntimeEvent::Claude(event),
            },
            (Some(session), None, None) => BackendRuntimeEvent::Codex(session.next_event().await),
            (None, Some(openrouter), None) => {
                BackendRuntimeEvent::OpenRouter(openrouter.next_event().await)
            }
            (None, None, Some(claude)) => {
                BackendRuntimeEvent::Claude(claude.service.next_event().await)
            }
            (None, None, None) => BackendRuntimeEvent::Codex(None),
        }
    }

    pub async fn pump_event(&mut self) -> Result<bool, BackendError> {
        let event = self.receive_event().await;
        self.process_received_event(event).await
    }

    pub async fn process_received_event(
        &mut self,
        event: BackendRuntimeEvent,
    ) -> Result<bool, BackendError> {
        if let BackendRuntimeEvent::OpenRouter(event) = event {
            let Some(event) = event else {
                self.openrouter = None;
                return Ok(self.session.is_some() || self.claude.is_some());
            };
            let effects = self.process_openrouter_service_event(event).await;
            self.execute_effects(effects).await?;
            return Ok(true);
        }
        if let BackendRuntimeEvent::Claude(event) = event {
            let Some(event) = event else {
                self.record_claude_unavailable("Claude Code event stream closed");
                self.claude = None;
                return Ok(self.session.is_some() || self.openrouter.is_some());
            };
            let effects = self.reduce_claude_service_event(event);
            self.execute_effects(effects).await?;
            return Ok(true);
        }
        let BackendRuntimeEvent::Codex(event) = event else {
            unreachable!("provider events returned above")
        };
        let Some(event) = event else {
            self.state.reduce(Action::Event(DomainEvent::ProcessExited(
                "app-server event stream closed".to_owned(),
            )));
            self.session = None;
            return Ok(self.openrouter.is_some() || self.claude.is_some());
        };
        let event = match event {
            Ok(event) => event,
            Err(error) => {
                self.state
                    .reduce(Action::Event(DomainEvent::ConnectionFailed(
                        error.to_string(),
                    )));
                self.session = None;
                return Ok(self.openrouter.is_some() || self.claude.is_some());
            }
        };
        let connection_closed = matches!(event, SessionEvent::ConnectionClosed(_));
        let effects = match event {
            SessionEvent::Protocol(event) => match self.reduce_protocol_event(event).await {
                Ok(effects) => effects,
                Err(error) => {
                    self.state
                        .reduce(Action::Event(DomainEvent::ConnectionFailed(
                            error.to_string(),
                        )));
                    self.session = None;
                    return Ok(self.openrouter.is_some() || self.claude.is_some());
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
        if connection_closed {
            self.session = None;
        }
        self.execute_effects(effects).await?;
        Ok(self.session.is_some() || self.openrouter.is_some() || self.claude.is_some())
    }
}
