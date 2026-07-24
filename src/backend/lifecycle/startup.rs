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
}
