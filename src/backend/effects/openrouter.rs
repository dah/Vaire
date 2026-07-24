use super::*;

impl<P: PreferencesPort, B: BrowserOpener> BackendCoordinator<P, B> {
    pub(super) async fn send_openrouter_message_effect(&mut self, text: String) -> Vec<Effect> {
        {
            let Ok(model) = self.selected_model(ProviderId::OpenRouter) else {
                self.state.notice = Some("select an OpenRouter model with /model".to_owned());
                return Vec::new();
            };
            let conversation_id = match &self.state.openrouter.conversation {
                crate::app::OpenRouterConversationState::Ready { id } => Some(id.clone()),
                crate::app::OpenRouterConversationState::None => None,
                crate::app::OpenRouterConversationState::ResumeFailed { .. } => {
                    self.state.notice =
                        Some("resolve the saved OpenRouter conversation with /resume".to_owned());
                    return Vec::new();
                }
            };
            if self.openrouter.is_none() {
                self.state.notice = Some("OpenRouter runtime is unavailable".to_owned());
                return Vec::new();
            }
            let prepared = self
                .openrouter
                .as_mut()
                .expect("OpenRouter runtime checked above")
                .prepare_turn(conversation_id, model.id, text)
                .await;
            match prepared {
                Ok(prepared) => {
                    let conversation_id = prepared.conversation_id().clone();
                    let turn_id = prepared.turn_id().clone();
                    let mut preferences = self.state.preferences.clone();
                    preferences.set_auto_resume_conversation(Some(conversation_id.clone()));
                    let persisted = self.persist_preferences(&preferences);
                    if matches!(persisted, Ok(Some(_))) {
                        let produced =
                            self.state
                                .reduce(Action::Event(DomainEvent::OpenRouterTurnStarted {
                                    conversation_id,
                                    turn_id,
                                }));
                        debug_assert!(produced
                            .iter()
                            .all(|effect| { matches!(effect, Effect::Persist(_)) }));
                        self.openrouter
                            .as_mut()
                            .expect("OpenRouter runtime checked above")
                            .launch_prepared_turn(prepared);
                        Vec::new()
                    } else {
                        let cleanup = self
                            .openrouter
                            .as_ref()
                            .expect("OpenRouter runtime checked above")
                            .abandon_prepared_turn(prepared)
                            .await;
                        let mut message = match persisted {
                                    Ok(None) => "preferences are read-only because their version is unsupported; the OpenRouter turn was not started".to_owned(),
                                    Ok(Some(_)) => unreachable!("handled above"),
                                    Err(error) => format!(
                                        "could not save the OpenRouter conversation pointer: {error}"
                                    ),
                                };
                        if cleanup.is_err() {
                            message.push_str("; the local turn also could not be marked failed");
                        }
                        self.state
                            .reduce(Action::Event(DomainEvent::TurnOperationFailed(message)))
                    }
                }
                Err(error) => self
                    .state
                    .reduce(Action::Event(DomainEvent::TurnOperationFailed(
                        error.to_string(),
                    ))),
            }
        }
    }
    pub(super) fn refresh_openrouter_effect(&mut self) -> Vec<Effect> {
        {
            let Some(openrouter) = &mut self.openrouter else {
                self.state.notice = Some("OpenRouter runtime is unavailable".to_owned());
                return Vec::new();
            };
            match openrouter.revalidate_and_refresh() {
                Ok(operation_id) => {
                    self.state.openrouter.credential_validation =
                        crate::app::OpenRouterCredentialValidation::Refreshing { operation_id };
                }
                Err(message) => self.state.notice = Some(message.to_owned()),
            }
            Vec::new()
        }
    }
    pub(super) async fn logout_openrouter_effect(&mut self) -> Vec<Effect> {
        {
            let active_turn = match &self.state.turn {
                crate::app::TurnState::OpenRouterStreaming {
                    conversation_id,
                    turn_id,
                } => Some((conversation_id.clone(), turn_id.clone())),
                _ => None,
            };
            let Some(openrouter) = &mut self.openrouter else {
                self.state.notice = Some("OpenRouter runtime is unavailable".to_owned());
                return Vec::new();
            };
            let (drained, logout) = openrouter.logout().await;
            let mut effects = Vec::new();
            for event in drained {
                effects.extend(self.reduce_openrouter_service_event(event));
            }
            if let Some((conversation_id, turn_id)) =
                active_turn.filter(|(conversation_id, turn_id)| {
                    matches!(
                        &self.state.turn,
                        crate::app::TurnState::OpenRouterStreaming {
                            conversation_id: active_conversation,
                            turn_id: active_turn,
                        } if active_conversation == conversation_id && active_turn == turn_id
                    )
                })
            {
                effects.extend(self.state.reduce(Action::Event(
                    DomainEvent::OpenRouterTurnFinished {
                        conversation_id,
                        turn_id,
                        outcome: TurnOutcome::Interrupted,
                        assistant_text: None,
                        incomplete_assistant_text: None,
                        failure_stage: None,
                    },
                )));
            }
            let auth = if logout.is_ok() {
                OpenRouterAuthStatus::Missing
            } else {
                OpenRouterAuthStatus::CredentialUnavailable
            };
            effects.extend(
                self.state
                    .reduce(Action::Event(DomainEvent::OpenRouterAuthChanged(auth))),
            );
            effects
        }
    }
    pub(super) fn interrupt_openrouter_turn_effect(&mut self) -> Vec<Effect> {
        {
            if let Some(openrouter) = &self.openrouter {
                openrouter.interrupt_turn();
            }
            Vec::new()
        }
    }
}
