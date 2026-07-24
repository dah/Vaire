use super::*;

impl<P: PreferencesPort, B: BrowserOpener> BackendCoordinator<P, B> {
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

        self.startup_claude().await;

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
            self.state.notice = Some(
                "Codex is unavailable; configured OpenRouter and Claude providers remain usable"
                    .to_owned(),
            );
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

    async fn startup_claude(&mut self) {
        let saved_session = (self.state.active_provider == ProviderId::Claude)
            .then(|| self.state.preferences.claude.auto_resume_session_id.clone())
            .flatten();
        let Some(runtime) = &mut self.claude else {
            if let Some(session_id) = saved_session {
                self.state
                    .reduce(Action::Event(DomainEvent::ClaudeResumeFailed {
                        session_id,
                        message: "Claude Code runtime is unavailable".to_owned(),
                    }));
            }
            return;
        };
        let credentials = runtime.credentials.clone();
        let key = tokio::task::spawn_blocking(move || {
            credentials.load(CredentialAccount::AnthropicConsoleApiKey)
        })
        .await;
        let auth = match key {
            Ok(Ok(None)) => ClaudeAuthStatus::Missing,
            Ok(Err(_)) | Err(_) => ClaudeAuthStatus::CredentialUnavailable,
            Ok(Ok(Some(key))) => {
                match crate::backend::claude_runtime::validate_claude_key(runtime, &key).await {
                    Ok(()) => ClaudeAuthStatus::Valid,
                    Err(error) if error.category == ClaudeFailureCategory::InvalidCredential => {
                        ClaudeAuthStatus::Invalid
                    }
                    Err(_) => ClaudeAuthStatus::Unverified,
                }
            }
        };
        self.state.reduce(Action::Event(DomainEvent::ClaudeStartup {
            availability: crate::app::ClaudeAvailability::Ready,
            auth,
        }));
        let Some(session_id) = saved_session else {
            return;
        };
        if auth != ClaudeAuthStatus::Valid {
            self.state.reduce(Action::Event(DomainEvent::ClaudeResumeFailed {
                session_id,
                message: "the saved Claude session could not be restored until its Console API key is valid"
                    .to_owned(),
            }));
            return;
        }
        match runtime.service.load_session(session_id.clone()).await {
            Ok(session) => {
                self.state
                    .reduce(Action::Event(DomainEvent::ClaudeSessionRestored {
                        session,
                        automatic: true,
                    }));
            }
            Err(error) => {
                self.state
                    .reduce(Action::Event(DomainEvent::ClaudeResumeFailed {
                        session_id,
                        message: error.to_string(),
                    }));
            }
        }
    }
}
