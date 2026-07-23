use super::*;

impl<P: PreferencesPort, B: BrowserOpener> BackendCoordinator<P, B> {
    pub fn new(session: SessionService, preferences: P, browser: B) -> Self {
        Self {
            state: AppState::default(),
            session: Some(session),
            openrouter: None,
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
            preferences,
            browser,
            may_persist: false,
            pending_openrouter_auto_resume: None,
            completed_items: CompletedItemTracker::default(),
        }
    }

    pub(in crate::backend) fn persist_preferences(
        &mut self,
        preferences: &crate::persistence::PreferencesV2,
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

    pub fn state(&self) -> &AppState {
        &self.state
    }

    pub async fn startup(&mut self) -> Result<(), BackendError> {
        let loaded = self.preferences.load()?;
        self.may_persist = loaded.may_overwrite;
        let migration_status = loaded
            .needs_save
            .then(|| self.persist_preferences(&loaded.preferences));
        let migration_save_failed = matches!(&migration_status, Some(Err(_)) | Some(Ok(None)));
        let migration_unverified = matches!(
            &migration_status,
            Some(Ok(Some(crate::storage::CommitStatus::CommittedUnverified)))
        );
        self.state
            .reduce(Action::Event(DomainEvent::PreferencesLoaded(
                loaded.preferences,
            )));
        if migration_save_failed {
            self.state.notice = Some(
                "preferences were upgraded in memory but could not be saved; will retry on shutdown"
                    .to_owned(),
            );
        } else if migration_unverified {
            self.state.notice = Some(
                "preferences were upgraded, but directory durability could not be verified; a later save will retry"
                    .to_owned(),
            );
        } else if let Some(message) = load_notice_message(loaded.notice) {
            self.state.notice = Some(message);
        }

        self.pending_openrouter_auto_resume = None;
        if let Some(openrouter) = &mut self.openrouter {
            let started = openrouter.startup().await;
            self.state
                .reduce(Action::Event(DomainEvent::OpenRouterStartup {
                    auth: started.auth,
                    catalog: started.catalog,
                }));
            let saved_conversation = (self.state.active_provider == ProviderId::OpenRouter)
                .then(|| {
                    self.state
                        .preferences
                        .openrouter
                        .auto_resume_conversation_id
                        .clone()
                })
                .flatten();
            if started.auth == OpenRouterAuthStatus::Unverified {
                match openrouter.revalidate_and_refresh() {
                    Ok(operation_id) => {
                        self.state.openrouter.credential_validation =
                            crate::app::OpenRouterCredentialValidation::Refreshing { operation_id };
                        if let Some(conversation_id) = saved_conversation {
                            self.pending_openrouter_auto_resume =
                                Some(PendingOpenRouterAutoResume {
                                    operation_id,
                                    conversation_id,
                                    model_id: self
                                        .state
                                        .preferences
                                        .openrouter
                                        .selected_model_id
                                        .clone(),
                                });
                        }
                    }
                    Err(_) => {
                        if let Some(conversation_id) = saved_conversation {
                            self.state
                                .reduce(Action::Event(DomainEvent::OpenRouterResumeFailed {
                                    conversation_id,
                                }));
                        }
                    }
                }
            } else if let Some(conversation_id) = saved_conversation {
                self.state
                    .reduce(Action::Event(DomainEvent::OpenRouterResumeFailed {
                        conversation_id,
                    }));
            }
        }
        if self.session.is_none() {
            self.state.notice =
                Some("Codex is unavailable; configured OpenRouter chat remains usable".to_owned());
            return Ok(());
        }
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

        let account = match self.codex_mut()?.read_account().await {
            Ok(account) => account,
            Err(error) => {
                self.state
                    .reduce(Action::Event(DomainEvent::ConnectionFailed(
                        error.to_string(),
                    )));
                return Err(error.into());
            }
        };
        let models = match self.codex_mut()?.list_models().await {
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

    /// Receives and parses exactly one transport event without starting any follow-up RPCs.
    ///
    /// The runtime selects this cancellation-safe boundary against user input. Once it returns,
    /// `process_received_event` must be allowed to finish so an already-consumed event cannot be
    /// lost while, for example, an account update waits for `account/read`.
    pub async fn receive_event(&mut self) -> BackendRuntimeEvent {
        if let (Some(openrouter), Some(session)) = (&mut self.openrouter, &mut self.session) {
            tokio::select! {
                event = session.next_event() => BackendRuntimeEvent::Codex(event),
                event = openrouter.next_event() => match event {
                    Some(event) => BackendRuntimeEvent::OpenRouter(event),
                    None => BackendRuntimeEvent::Codex(None),
                },
            }
        } else if let Some(session) = &mut self.session {
            BackendRuntimeEvent::Codex(session.next_event().await)
        } else if let Some(openrouter) = &mut self.openrouter {
            match openrouter.next_event().await {
                Some(event) => BackendRuntimeEvent::OpenRouter(event),
                None => BackendRuntimeEvent::Codex(None),
            }
        } else {
            BackendRuntimeEvent::Codex(None)
        }
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
        event: BackendRuntimeEvent,
    ) -> Result<bool, BackendError> {
        if let BackendRuntimeEvent::OpenRouter(event) = event {
            let effects = self.process_openrouter_service_event(event).await;
            self.execute_effects(effects).await?;
            return Ok(true);
        }
        let BackendRuntimeEvent::Codex(event) = event else {
            unreachable!("OpenRouter event returned above")
        };
        let Some(event) = event else {
            self.state.reduce(Action::Event(DomainEvent::ProcessExited(
                "app-server event stream closed".to_owned(),
            )));
            self.session = None;
            return Ok(self.openrouter.is_some());
        };
        let event = match event {
            Ok(event) => event,
            Err(error) => {
                self.state
                    .reduce(Action::Event(DomainEvent::ConnectionFailed(
                        error.to_string(),
                    )));
                self.session = None;
                return Ok(self.openrouter.is_some());
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
                    return Ok(self.openrouter.is_some());
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
        Ok(self.session.is_some() || self.openrouter.is_some())
    }

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

    pub async fn shutdown(&mut self) -> Result<(), BackendError> {
        let active_openrouter_turn = match &self.state.turn {
            crate::app::TurnState::OpenRouterStreaming {
                conversation_id,
                turn_id,
            } => Some((conversation_id.clone(), turn_id.clone())),
            _ => None,
        };
        let drained = if let Some(openrouter) = &mut self.openrouter {
            openrouter.shutdown().await
        } else {
            Vec::new()
        };
        for event in drained {
            let _ = self.reduce_openrouter_service_event(event);
        }
        if let Some((conversation_id, turn_id)) =
            active_openrouter_turn.filter(|(conversation_id, turn_id)| {
                matches!(
                    &self.state.turn,
                    crate::app::TurnState::OpenRouterStreaming {
                        conversation_id: active_conversation,
                        turn_id: active_turn,
                    } if active_conversation == conversation_id && active_turn == turn_id
                )
            })
        {
            self.state
                .reduce(Action::Event(DomainEvent::OpenRouterTurnFinished {
                    conversation_id,
                    turn_id,
                    outcome: TurnOutcome::Interrupted,
                    assistant_text: None,
                }));
        }
        let settled_preferences = self.state.preferences.clone();
        let persistence_result = self
            .persist_preferences(&settled_preferences)
            .map(|_| ())
            .map_err(BackendError::from);
        let session_result = if let Some(session) = &mut self.session {
            session.shutdown().await.map_err(BackendError::from)
        } else {
            Ok(())
        };
        persistence_result?;
        session_result
    }

    fn pending_openrouter_resume_is_current(&self, pending: &PendingOpenRouterAutoResume) -> bool {
        self.state.active_provider == ProviderId::OpenRouter
            && self
                .state
                .preferences
                .openrouter
                .auto_resume_conversation_id
                .as_ref()
                == Some(&pending.conversation_id)
            && self.state.preferences.openrouter.selected_model_id == pending.model_id
    }

    pub(in crate::backend) async fn process_openrouter_service_event(
        &mut self,
        event: OpenRouterServiceEvent,
    ) -> Vec<Effect> {
        let operation_id = match &event {
            OpenRouterServiceEvent::CatalogLoaded { operation_id, .. }
            | OpenRouterServiceEvent::CatalogFailed { operation_id, .. } => *operation_id,
            _ => return self.reduce_openrouter_service_event(event),
        };
        if self
            .pending_openrouter_auto_resume
            .as_ref()
            .is_none_or(|pending| pending.operation_id != operation_id)
        {
            return self.reduce_openrouter_service_event(event);
        }
        let pending = self
            .pending_openrouter_auto_resume
            .take()
            .expect("matching pending OpenRouter resume checked above");
        let refresh_is_current = matches!(
            self.state.openrouter.credential_validation,
            crate::app::OpenRouterCredentialValidation::Refreshing {
                operation_id: active,
            } if active == operation_id
        );
        let snapshot_is_current = self.pending_openrouter_resume_is_current(&pending);
        if !refresh_is_current || !snapshot_is_current {
            return self.reduce_openrouter_service_event(event);
        }

        match event {
            OpenRouterServiceEvent::CatalogLoaded { catalog, .. } => {
                let exact_model = pending
                    .model_id
                    .as_ref()
                    .filter(|model_id| {
                        self.state
                            .preferences
                            .openrouter
                            .enabled_model_ids
                            .contains(*model_id)
                            && catalog.iter().any(|model| &model.id == *model_id)
                    })
                    .and_then(|model_id| ModelKey::openrouter(model_id.clone()).ok());
                let mut effects = self.state.reduce(Action::Event(
                    DomainEvent::OpenRouterCatalogLoadedForAutomaticResume(catalog),
                ));
                let restored = if let (Some(model), Some(openrouter)) =
                    (exact_model, self.openrouter.as_ref())
                {
                    match openrouter
                        .load_conversation(pending.conversation_id.clone())
                        .await
                    {
                        Ok(conversation) if self.pending_openrouter_resume_is_current(&pending) => {
                            Some((model, conversation))
                        }
                        Ok(_) | Err(_) => None,
                    }
                } else {
                    None
                };
                if let Some((model, conversation)) = restored {
                    effects.extend(self.state.reduce(Action::Event(
                        DomainEvent::OpenRouterConversationRestored {
                            conversation_id: pending.conversation_id,
                            history: openrouter_history(&conversation),
                            model,
                            automatic: true,
                        },
                    )));
                } else if self.pending_openrouter_resume_is_current(&pending) {
                    effects.extend(self.state.reduce(Action::Event(
                        DomainEvent::OpenRouterResumeFailed {
                            conversation_id: pending.conversation_id,
                        },
                    )));
                }
                effects
            }
            failed @ OpenRouterServiceEvent::CatalogFailed { .. } => {
                let mut effects = self.reduce_openrouter_service_event(failed);
                if self.pending_openrouter_resume_is_current(&pending) {
                    effects.extend(self.state.reduce(Action::Event(
                        DomainEvent::OpenRouterResumeFailed {
                            conversation_id: pending.conversation_id,
                        },
                    )));
                }
                effects
            }
            _ => unreachable!("only catalog terminal events reach pending resume settlement"),
        }
    }

    pub(in crate::backend) fn reduce_openrouter_service_event(
        &mut self,
        event: OpenRouterServiceEvent,
    ) -> Vec<Effect> {
        match event {
            OpenRouterServiceEvent::AuthValidated { operation_id } => {
                match &mut self.state.openrouter.credential_validation {
                    crate::app::OpenRouterCredentialValidation::Refreshing {
                        operation_id: active,
                    } if *active == operation_id => {}
                    crate::app::OpenRouterCredentialValidation::Validating {
                        operation_id: active,
                        candidate_saved,
                    } if *active == operation_id => *candidate_saved = true,
                    _ => return Vec::new(),
                }
                self.state
                    .reduce(Action::Event(DomainEvent::OpenRouterAuthChanged(
                        OpenRouterAuthStatus::Valid,
                    )))
            }
            OpenRouterServiceEvent::LoginSucceeded {
                operation_id,
                catalog,
            } => {
                if !matches!(
                    self.state.openrouter.credential_validation,
                    crate::app::OpenRouterCredentialValidation::Validating {
                        operation_id: active,
                        ..
                    } if active == operation_id
                ) {
                    return Vec::new();
                }
                self.state
                    .reduce(Action::Event(DomainEvent::OpenRouterCatalogLoaded(catalog)))
            }
            OpenRouterServiceEvent::CatalogLoaded {
                operation_id,
                catalog,
            } => {
                if !matches!(
                    self.state.openrouter.credential_validation,
                    crate::app::OpenRouterCredentialValidation::Refreshing {
                        operation_id: active,
                    } if active == operation_id
                ) {
                    return Vec::new();
                }
                self.state
                    .reduce(Action::Event(DomainEvent::OpenRouterCatalogLoaded(catalog)))
            }
            OpenRouterServiceEvent::LoginFailed {
                operation_id,
                category,
            } => {
                if !matches!(
                    self.state.openrouter.credential_validation,
                    crate::app::OpenRouterCredentialValidation::Validating {
                        operation_id: active,
                        ..
                    } if active == operation_id
                ) {
                    return Vec::new();
                }
                self.state
                    .reduce(Action::Event(DomainEvent::OpenRouterCandidateRejected(
                        category,
                    )))
            }
            OpenRouterServiceEvent::CatalogFailed {
                operation_id,
                category,
            } => {
                let matches = match self.state.openrouter.credential_validation {
                    crate::app::OpenRouterCredentialValidation::Refreshing {
                        operation_id: active,
                    }
                    | crate::app::OpenRouterCredentialValidation::Validating {
                        operation_id: active,
                        ..
                    } => active == operation_id,
                    crate::app::OpenRouterCredentialValidation::Idle => false,
                };
                if !matches {
                    return Vec::new();
                }
                self.state
                    .reduce(Action::Event(DomainEvent::OpenRouterOperationFailed(
                        category,
                    )))
            }
            OpenRouterServiceEvent::TurnStarted {
                conversation_id,
                turn_id,
            } => self
                .state
                .reduce(Action::Event(DomainEvent::OpenRouterTurnStarted {
                    conversation_id,
                    turn_id,
                })),
            OpenRouterServiceEvent::TextDelta {
                conversation_id,
                turn_id,
                delta,
            } => self
                .state
                .reduce(Action::Event(DomainEvent::OpenRouterDelta {
                    conversation_id,
                    turn_id,
                    delta,
                })),
            OpenRouterServiceEvent::Usage {
                conversation_id,
                turn_id,
                usage,
            } => self
                .state
                .reduce(Action::Event(DomainEvent::OpenRouterUsage {
                    conversation_id,
                    turn_id,
                    usage,
                })),
            OpenRouterServiceEvent::TurnFinished {
                conversation_id,
                turn_id,
                outcome,
                assistant_text,
                usage,
                failure,
            } => {
                let authoritative_turn = matches!(
                    &self.state.turn,
                    crate::app::TurnState::OpenRouterStreaming {
                        conversation_id: active_conversation,
                        turn_id: active_turn,
                    } if active_conversation == &conversation_id && active_turn == &turn_id
                );
                if authoritative_turn && failure == Some(OpenRouterFailureCategory::Unauthorized) {
                    self.state
                        .reduce(Action::Event(DomainEvent::OpenRouterAuthChanged(
                            OpenRouterAuthStatus::Invalid,
                        )));
                }
                if let Some(usage) = usage {
                    self.state
                        .reduce(Action::Event(DomainEvent::OpenRouterUsage {
                            conversation_id: conversation_id.clone(),
                            turn_id: turn_id.clone(),
                            usage,
                        }));
                }
                let outcome = match outcome {
                    OpenRouterTurnOutcome::Completed => TurnOutcome::Completed,
                    OpenRouterTurnOutcome::Interrupted => TurnOutcome::Interrupted,
                    OpenRouterTurnOutcome::Failed | OpenRouterTurnOutcome::InProgress => {
                        TurnOutcome::Failed(format!(
                            "OpenRouter turn failed ({:?})",
                            failure.unwrap_or(OpenRouterFailureCategory::InvalidResponse)
                        ))
                    }
                };
                self.state
                    .reduce(Action::Event(DomainEvent::OpenRouterTurnFinished {
                        conversation_id,
                        turn_id,
                        outcome,
                        assistant_text,
                    }))
            }
        }
    }
}

pub(in crate::backend) fn openrouter_history(
    conversation: &OpenRouterConversationV1,
) -> Vec<crate::app::TranscriptEntry> {
    let mut history = Vec::new();
    for turn in &conversation.turns {
        history.push(crate::app::TranscriptEntry {
            provider: crate::provider::ProviderId::OpenRouter,
            role: crate::app::TranscriptRole::User,
            text: turn.user_text.clone(),
            item_id: None,
            turn_id: Some(turn.id.as_str().to_owned()),
        });
        if turn.outcome == OpenRouterTurnOutcome::Completed {
            if let Some(text) = &turn.assistant_text {
                history.push(crate::app::TranscriptEntry {
                    provider: crate::provider::ProviderId::OpenRouter,
                    role: crate::app::TranscriptRole::Assistant,
                    text: text.clone(),
                    item_id: Some("openrouter-assistant".to_owned()),
                    turn_id: Some(turn.id.as_str().to_owned()),
                });
            }
        }
    }
    history
}
