use super::*;

impl<P: PreferencesPort, B: BrowserOpener> BackendCoordinator<P, B> {
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
                incomplete_assistant_text,
                usage,
                failure,
                failure_stage,
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
                        incomplete_assistant_text,
                        failure_stage,
                    }))
            }
        }
    }
}

pub(in crate::backend) fn openrouter_history(
    conversation: &OpenRouterConversationV2,
) -> Vec<crate::app::TranscriptEntry> {
    let mut history = Vec::new();
    for turn in &conversation.turns {
        history.push(crate::app::TranscriptEntry {
            provider: crate::provider::ProviderId::OpenRouter,
            role: crate::app::TranscriptRole::User,
            status: crate::app::TranscriptEntryStatus::Normal,
            text: turn.user_text.clone(),
            item_id: None,
            turn_id: Some(turn.id.as_str().to_owned()),
        });
        let assistant = match turn.outcome {
            OpenRouterTurnOutcome::Completed => turn
                .assistant_text
                .as_ref()
                .map(|text| (text, crate::app::TranscriptEntryStatus::Normal)),
            OpenRouterTurnOutcome::Failed => turn
                .incomplete_assistant_text
                .as_ref()
                .map(|text| (text, crate::app::TranscriptEntryStatus::FailedIncomplete)),
            OpenRouterTurnOutcome::InProgress | OpenRouterTurnOutcome::Interrupted => None,
        };
        if let Some((text, status)) = assistant {
            history.push(crate::app::TranscriptEntry {
                provider: crate::provider::ProviderId::OpenRouter,
                role: crate::app::TranscriptRole::Assistant,
                status,
                text: text.clone(),
                item_id: Some("openrouter-assistant".to_owned()),
                turn_id: Some(turn.id.as_str().to_owned()),
            });
        }
    }
    history
}
