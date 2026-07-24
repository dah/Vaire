use super::*;
use crate::backend::claude_runtime::{
    claude_error, claude_store_error, now_ms, selected_claude_alias, validate_claude_key,
};

impl<P: PreferencesPort, B: BrowserOpener> BackendCoordinator<P, B> {
    pub(super) async fn refresh_claude_effect(&mut self) -> Vec<Effect> {
        let Some(runtime) = &mut self.claude else {
            self.record_claude_unavailable("Claude Code runtime is unavailable");
            return Vec::new();
        };
        let operation_id = runtime.operation_id();
        self.state.claude.credential_validation =
            crate::app::ClaudeCredentialValidation::Refreshing { operation_id };
        let credentials = runtime.credentials.clone();
        let key = match tokio::task::spawn_blocking(move || {
            credentials.load(CredentialAccount::AnthropicConsoleApiKey)
        })
        .await
        {
            Ok(Ok(Some(key))) => key,
            Ok(Ok(None)) => {
                return self
                    .state
                    .reduce(Action::Event(DomainEvent::ClaudeAuthChanged(
                        ClaudeAuthStatus::Missing,
                    )))
            }
            Ok(Err(_)) | Err(_) => {
                return self
                    .state
                    .reduce(Action::Event(DomainEvent::ClaudeOperationFailed(
                        claude_error(
                            ClaudeFailureStage::Credential,
                            ClaudeFailureCategory::Unavailable,
                        ),
                    )))
            }
        };
        let result = validate_claude_key(runtime, &key).await;
        match result {
            Ok(()) => {
                let mut effects = self
                    .state
                    .reduce(Action::Event(DomainEvent::ClaudeAuthChanged(
                        ClaudeAuthStatus::Valid,
                    )));
                effects.extend(self.restore_saved_claude_after_auth().await);
                effects
            }
            Err(error) if error.category == ClaudeFailureCategory::InvalidCredential => self
                .state
                .reduce(Action::Event(DomainEvent::ClaudeAuthChanged(
                    ClaudeAuthStatus::Invalid,
                ))),
            Err(error) => self
                .state
                .reduce(Action::Event(DomainEvent::ClaudeOperationFailed(error))),
        }
    }

    pub async fn accept_claude_credential(
        &mut self,
        value: crate::credentials::SecretValue,
    ) -> Result<(), crate::credentials::SecretValue> {
        if matches!(
            self.state.turn,
            crate::app::TurnState::ClaudeStreaming { .. }
        ) || (self.state.active_provider == ProviderId::Claude
            && self.state.turn == crate::app::TurnState::Starting)
        {
            self.state.notice = Some(
                "wait for or interrupt the active Claude turn before replacing its credential"
                    .to_owned(),
            );
            return Err(value);
        }
        let Some(runtime) = &mut self.claude else {
            self.state.notice = Some("Claude Code runtime is unavailable".to_owned());
            return Err(value);
        };
        let operation_id = runtime.operation_id();
        self.state.claude.credential_validation =
            crate::app::ClaudeCredentialValidation::Validating {
                operation_id,
                candidate_saved: false,
            };
        if let Err(error) = validate_claude_key(runtime, &value).await {
            self.state
                .reduce(Action::Event(DomainEvent::ClaudeCandidateRejected(error)));
            return Err(value);
        }
        let credentials = runtime.credentials.clone();
        match tokio::task::spawn_blocking(move || {
            credentials.replace_with_commit(CredentialAccount::AnthropicConsoleApiKey, value)
        })
        .await
        {
            Ok(Ok(crate::storage::CommitStatus::Verified)) => {
                let mut effects = self
                    .state
                    .reduce(Action::Event(DomainEvent::ClaudeAuthChanged(
                        ClaudeAuthStatus::Valid,
                    )));
                effects.extend(self.restore_saved_claude_after_auth().await);
                if let Err(error) = self.execute_effects(effects).await {
                    self.record_error(error.to_string());
                }
                Ok(())
            }
            Ok(Ok(crate::storage::CommitStatus::CommittedUnverified)) => {
                self.state
                    .reduce(Action::Event(DomainEvent::ClaudeAuthChanged(
                        ClaudeAuthStatus::CredentialUnavailable,
                    )));
                self.state.notice = Some(
                    "Claude credential storage changed, but directory durability could not be verified; refresh or replace the credential before using Claude"
                        .to_owned(),
                );
                Ok(())
            }
            Ok(Err(_)) | Err(_) => {
                self.state
                    .reduce(Action::Event(DomainEvent::ClaudeCandidateRejected(
                        claude_error(
                            ClaudeFailureStage::Credential,
                            ClaudeFailureCategory::Unavailable,
                        ),
                    )));
                // Ownership was consumed by the atomic credential-store attempt. The previous
                // durable credential remains intact on failure.
                Ok(())
            }
        }
    }

    pub(super) async fn logout_claude_effect(&mut self) -> Vec<Effect> {
        let Some(runtime) = &mut self.claude else {
            self.record_claude_unavailable("Claude Code runtime is unavailable");
            return Vec::new();
        };
        let drained = runtime.service.interrupt_and_drain().await;
        let credentials = runtime.credentials.clone();
        let mut produced = Vec::new();
        for event in drained {
            produced.extend(self.reduce_claude_service_event(event));
        }
        match tokio::task::spawn_blocking(move || {
            credentials.delete_with_commit(CredentialAccount::AnthropicConsoleApiKey)
        })
        .await
        {
            Ok(Ok(crate::storage::CommitStatus::Verified)) => produced.extend(self.state.reduce(
                Action::Event(DomainEvent::ClaudeAuthChanged(ClaudeAuthStatus::Missing)),
            )),
            Ok(Ok(crate::storage::CommitStatus::CommittedUnverified)) => {
                produced.extend(
                    self.state
                        .reduce(Action::Event(DomainEvent::ClaudeAuthChanged(
                            ClaudeAuthStatus::CredentialUnavailable,
                        ))),
                );
                self.state.notice = Some(
                    "Claude credential removal was committed, but directory durability could not be verified; sign-out status is uncertain"
                        .to_owned(),
                );
            }
            Ok(Err(_)) | Err(_) => produced.extend(self.state.reduce(Action::Event(
                DomainEvent::ClaudeOperationFailed(claude_error(
                    ClaudeFailureStage::Credential,
                    ClaudeFailureCategory::Unavailable,
                )),
            ))),
        }
        produced
    }

    pub(super) async fn start_new_claude_session_effect(&mut self) -> Vec<Effect> {
        let alias = selected_claude_alias(&self.state);
        let Some(runtime) = &self.claude else {
            return self
                .state
                .reduce(Action::Event(DomainEvent::ClaudeOperationFailed(
                    claude_error(
                        ClaudeFailureStage::Store,
                        ClaudeFailureCategory::Unavailable,
                    ),
                )));
        };
        match runtime.service.create_session(alias, now_ms()).await {
            Ok((session_id, commit))
                if commit.source == crate::storage::CommitStatus::Verified =>
            {
                match self.persist_claude_pointer(&session_id) {
                    Ok(Some(crate::storage::CommitStatus::Verified)) => self
                        .state
                        .reduce(Action::Event(DomainEvent::ClaudeSessionStarted {
                            session_id,
                        }))
                        .into_iter()
                        .filter(|effect| !matches!(effect, Effect::Persist(_)))
                        .collect(),
                    Ok(Some(crate::storage::CommitStatus::CommittedUnverified)) => self
                        .state
                        .reduce(Action::Event(
                            DomainEvent::ClaudeSessionCreationUncertain {
                                session_id,
                                message: "Vairë committed the new session pointer, but could not verify its directory durability".to_owned(),
                            },
                        ))
                        .into_iter()
                        .filter(|effect| !matches!(effect, Effect::Persist(_)))
                        .collect(),
                    Ok(None) | Err(_) => self.state.reduce(Action::Event(
                        DomainEvent::ClaudeNewSessionFailed(
                            "the new session pointer could not be durably committed".to_owned(),
                        ),
                    )),
                }
            }
            Ok((_session_id, _commit)) => self.state.reduce(Action::Event(
                DomainEvent::ClaudeNewSessionFailed(
                    "the new Vairë session registration was committed, but its directory durability could not be verified; use /resume to inspect registered sessions"
                        .to_owned(),
                ),
            )),
            Err(_) => self
                .state
                .reduce(Action::Event(DomainEvent::ClaudeOperationFailed(
                    claude_store_error(),
                ))),
        }
    }

    pub(super) async fn switch_claude_session_effect(
        &mut self,
        session_id: crate::provider::ClaudeSessionId,
    ) -> Vec<Effect> {
        let Some(runtime) = &self.claude else {
            return self
                .state
                .reduce(Action::Event(DomainEvent::ClaudeSessionSwitchFailed {
                    session_id,
                    message: "Claude Code runtime is unavailable".to_owned(),
                }));
        };
        match runtime.service.load_session(session_id.clone()).await {
            Ok(session) => self
                .state
                .reduce(Action::Event(DomainEvent::ClaudeSessionRestored {
                    session,
                    automatic: false,
                })),
            Err(error) => {
                self.state
                    .reduce(Action::Event(DomainEvent::ClaudeSessionSwitchFailed {
                        session_id,
                        message: error.to_string(),
                    }))
            }
        }
    }

    pub(super) async fn send_claude_message_effect(&mut self, text: String) -> Vec<Effect> {
        let session_id = match &self.state.claude.conversation {
            crate::app::ClaudeConversationState::Ready { id } => id.clone(),
            crate::app::ClaudeConversationState::None => {
                let alias = selected_claude_alias(&self.state);
                let Some(runtime) = &self.claude else {
                    return self
                        .state
                        .reduce(Action::Event(DomainEvent::ClaudeOperationFailed(
                            claude_error(
                                ClaudeFailureStage::Store,
                                ClaudeFailureCategory::Unavailable,
                            ),
                        )));
                };
                return match runtime.service.create_session(alias, now_ms()).await {
                    Ok((session_id, commit)) => {
                        if commit.source != crate::storage::CommitStatus::Verified {
                            let mut effects = self.state.reduce(Action::Event(
                                DomainEvent::ClaudeOperationFailed(claude_store_error()),
                            ));
                            effects.extend(self.state.reduce(Action::Event(
                                DomainEvent::ClaudeSessionCreationUncertain {
                                    session_id,
                                    message: "Vairë committed the new session registration but could not verify its directory durability".to_owned(),
                                },
                            )));
                            return effects;
                        }
                        match self.persist_claude_pointer(&session_id) {
                            Ok(Some(crate::storage::CommitStatus::Verified)) => {}
                            Ok(Some(crate::storage::CommitStatus::CommittedUnverified)) => {
                                let mut effects = self.state.reduce(Action::Event(
                                    DomainEvent::ClaudeOperationFailed(claude_store_error()),
                                ));
                                effects.extend(self.state.reduce(Action::Event(
                                    DomainEvent::ClaudeSessionCreationUncertain {
                                        session_id,
                                        message: "Vairë committed the new session pointer, but could not verify its directory durability".to_owned(),
                                    },
                                )));
                                return effects
                                    .into_iter()
                                    .filter(|effect| !matches!(effect, Effect::Persist(_)))
                                    .collect();
                            }
                            Ok(None) | Err(_) => {
                                let mut effects = self.state.reduce(Action::Event(
                                    DomainEvent::ClaudeOperationFailed(claude_store_error()),
                                ));
                                effects.extend(self.state.reduce(Action::Event(
                                    DomainEvent::ClaudeSessionCreationUncertain {
                                        session_id,
                                        message: "Vairë could not durably commit the new session pointer".to_owned(),
                                    },
                                )));
                                return effects;
                            }
                        }
                        let mut effects = self
                            .state
                            .reduce(Action::Event(DomainEvent::ClaudeSessionStarted {
                                session_id,
                            }))
                            .into_iter()
                            .filter(|effect| !matches!(effect, Effect::Persist(_)))
                            .collect::<Vec<_>>();
                        // The UUID registration and pointer are verified before the child can
                        // be prepared or launched.
                        effects.push(Effect::SendClaudeMessage { text });
                        effects
                    }
                    Err(_) => self
                        .state
                        .reduce(Action::Event(DomainEvent::ClaudeOperationFailed(
                            claude_store_error(),
                        ))),
                };
            }
            _ => {
                return self
                    .state
                    .reduce(Action::Event(DomainEvent::ClaudeOperationFailed(
                        claude_error(
                            ClaudeFailureStage::Store,
                            ClaudeFailureCategory::Unavailable,
                        ),
                    )))
            }
        };
        let alias = selected_claude_alias(&self.state);
        let Some(runtime) = &mut self.claude else {
            return self
                .state
                .reduce(Action::Event(DomainEvent::ClaudeOperationFailed(
                    claude_error(
                        ClaudeFailureStage::Spawn,
                        ClaudeFailureCategory::Unavailable,
                    ),
                )));
        };
        let prepared = match runtime
            .service
            .prepare_turn(session_id, alias, text, now_ms())
            .await
        {
            Ok(prepared) => prepared,
            Err(error) => {
                return self
                    .state
                    .reduce(Action::Event(DomainEvent::ClaudeOperationFailed(error)))
            }
        };
        match runtime
            .service
            .launch_prepared_turn(prepared, now_ms())
            .await
        {
            Ok(()) => Vec::new(),
            Err(error) => self
                .state
                .reduce(Action::Event(DomainEvent::ClaudeOperationFailed(error))),
        }
    }

    pub(super) fn interrupt_claude_turn_effect(&mut self) -> Vec<Effect> {
        if self
            .claude
            .as_ref()
            .is_some_and(|runtime| runtime.service.interrupt_turn())
        {
            Vec::new()
        } else {
            self.state
                .reduce(Action::Event(DomainEvent::ClaudeOperationFailed(
                    claude_error(
                        ClaudeFailureStage::Shutdown,
                        ClaudeFailureCategory::Unavailable,
                    ),
                )))
        }
    }

    fn persist_claude_pointer(
        &mut self,
        session_id: &crate::provider::ClaudeSessionId,
    ) -> Result<Option<crate::storage::CommitStatus>, PersistenceError> {
        let mut preferences = self.state.preferences.clone();
        preferences.set_auto_resume_claude_session(Some(session_id.clone()));
        self.persist_preferences(&preferences)
    }

    async fn restore_saved_claude_after_auth(&mut self) -> Vec<Effect> {
        if self.state.active_provider != ProviderId::Claude {
            return Vec::new();
        }
        let Some(session_id) = self.state.preferences.claude.auto_resume_session_id.clone() else {
            return Vec::new();
        };
        if matches!(
            &self.state.claude.conversation,
            crate::app::ClaudeConversationState::Ready { id } if id == &session_id
        ) {
            return Vec::new();
        }
        let Some(runtime) = &self.claude else {
            return Vec::new();
        };
        match runtime.service.load_session(session_id.clone()).await {
            Ok(session) => self
                .state
                .reduce(Action::Event(DomainEvent::ClaudeSessionRestored {
                    session,
                    automatic: true,
                })),
            Err(error) => self
                .state
                .reduce(Action::Event(DomainEvent::ClaudeResumeFailed {
                    session_id,
                    message: error.to_string(),
                })),
        }
    }
}
