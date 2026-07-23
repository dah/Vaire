use super::*;

impl<P: PreferencesPort, B: BrowserOpener> BackendCoordinator<P, B> {
    pub(in crate::backend) async fn execute_effects(
        &mut self,
        initial: Vec<Effect>,
    ) -> Result<(), BackendError> {
        let mut effects = VecDeque::from(initial);
        while let Some(effect) = effects.pop_front() {
            let produced = match effect {
                Effect::StartLogin => match self.codex_mut()?.start_login().await {
                    Ok(challenge) => {
                        let effects = self.state.reduce(Action::Event(DomainEvent::LoginStarted {
                            login_id: challenge.login_id,
                        }));
                        match self.browser.open_login_url(&challenge.auth_url) {
                            Ok(()) => {
                                self.state.notice = Some(
                                "Complete sign-in in the browser; if it fails, use /logout then /login device"
                                    .to_owned(),
                            );
                            }
                            Err(error) => {
                                self.state.notice = Some(format!(
                                    "{error}; use /logout to cancel this pending sign-in"
                                ));
                            }
                        }
                        effects
                    }
                    Err(error) => self
                        .state
                        .reduce(Action::Event(DomainEvent::LoginFailed(error.to_string()))),
                },
                Effect::StartDeviceLogin => match self.codex_mut()?.start_device_login().await {
                    Ok(challenge) => {
                        let effects = self.state.reduce(Action::Event(DomainEvent::LoginStarted {
                            login_id: challenge.login_id,
                        }));
                        match self.browser.open_login_url(&challenge.verification_url) {
                            Ok(()) => {
                                self.state.notice = Some(format!(
                                    "Enter code {} in the browser; use /logout to cancel",
                                    challenge.user_code
                                ));
                            }
                            Err(error) => {
                                self.state.notice = Some(format!(
                                    "{error}; use /logout to cancel this pending sign-in"
                                ));
                            }
                        }
                        effects
                    }
                    Err(error) => self
                        .state
                        .reduce(Action::Event(DomainEvent::LoginFailed(error.to_string()))),
                },
                Effect::CancelLogin { login_id } => self.cancel_login(&login_id).await,
                Effect::Logout => match self.codex_mut()?.logout().await {
                    Ok(()) => self.state.reduce(Action::Event(DomainEvent::LoggedOut)),
                    Err(error) => {
                        self.state.notice = Some(error.to_string());
                        Vec::new()
                    }
                },
                Effect::StartNewThread => {
                    let model = self.selected_model(ProviderId::Codex)?;
                    match self.codex_mut()?.start_thread(&model.id).await {
                        Ok(thread) => {
                            self.state
                                .reduce(Action::Event(DomainEvent::NewThreadSucceeded {
                                    id: thread.id,
                                }))
                        }
                        Err(error) => {
                            let fatal = is_fatal_transport(&error);
                            let mut effects =
                                self.state
                                    .reduce(Action::Event(DomainEvent::NewThreadFailed(
                                        error.to_string(),
                                    )));
                            if fatal {
                                effects.extend(self.state.reduce(Action::Event(
                                    DomainEvent::ConnectionFailed(
                                        "app-server connection became unusable while starting a new thread; restart AgentHarness"
                                            .to_owned(),
                                    ),
                                )));
                            }
                            effects
                        }
                    }
                }
                Effect::StartNewOpenRouterConversation => {
                    let Some(openrouter) = &self.openrouter else {
                        self.state.notice = Some("OpenRouter runtime is unavailable".to_owned());
                        continue;
                    };
                    match openrouter.create_conversation().await {
                        Ok(conversation_id) => self.state.reduce(Action::Event(
                            DomainEvent::OpenRouterConversationStarted { conversation_id },
                        )),
                        Err(error) => {
                            self.state.notice = Some(error.to_string());
                            Vec::new()
                        }
                    }
                }
                Effect::ListThreads => {
                    let codex = match &mut self.session {
                        Some(session) => session.list_threads().await,
                        None => Err(SessionError::Protocol(
                            "Codex provider is unavailable".to_owned(),
                        )),
                    };
                    let openrouter = match &self.openrouter {
                        Some(openrouter) => openrouter.list_conversations().await.ok(),
                        None => None,
                    };
                    if codex.is_err() && openrouter.is_none() {
                        self.state
                            .reduce(Action::Event(DomainEvent::ThreadListFailed(
                                codex.expect_err("checked error").to_string(),
                            )))
                    } else {
                        let mut choices = codex.map(thread_choices).unwrap_or_default();
                        choices.extend(openrouter.unwrap_or_default().into_iter().map(|summary| {
                            crate::app::ThreadChoice {
                                provider: ProviderId::OpenRouter,
                                id: summary.id.as_str().to_owned(),
                                title: summary.title,
                                updated_at: i64::try_from(summary.updated_at_ms)
                                    .unwrap_or(i64::MAX),
                            }
                        }));
                        self.state
                            .reduce(Action::Event(DomainEvent::ThreadListLoaded(choices)))
                    }
                }
                Effect::ResumeThread { id } => {
                    self.state
                        .reduce(Action::Event(DomainEvent::ResumeStarted { id: id.clone() }));
                    let model = self.selected_model(ProviderId::Codex)?;
                    match self.codex_mut()?.resume_thread(&id, &model.id).await {
                        Ok(thread) => {
                            self.state
                                .reduce(Action::Event(DomainEvent::ResumeSucceeded {
                                    id,
                                    history: history_entries(&thread),
                                }))
                        }
                        Err(error) => self.state.reduce(Action::Event(DomainEvent::ResumeFailed {
                            id,
                            message: error.to_string(),
                        })),
                    }
                }
                Effect::SwitchThread {
                    id,
                    model,
                    reasoning,
                } => {
                    if model.provider != ProviderId::Codex {
                        self.state
                            .reduce(Action::Event(DomainEvent::ThreadSwitchFailed {
                                id,
                                message: "invalid Codex model selection".to_owned(),
                            }))
                    } else {
                        match self.codex_mut()?.resume_thread(&id, &model.id).await {
                            Ok(thread) => self.state.reduce(Action::Event(
                                DomainEvent::ThreadSwitchSucceeded {
                                    id,
                                    history: history_entries(&thread),
                                    model,
                                    reasoning,
                                },
                            )),
                            Err(error) => {
                                let fatal = is_fatal_transport(&error);
                                let mut effects = self.state.reduce(Action::Event(
                                    DomainEvent::ThreadSwitchFailed {
                                        id,
                                        message: error.to_string(),
                                    },
                                ));
                                if fatal {
                                    effects.extend(self.state.reduce(Action::Event(
                                    DomainEvent::ConnectionFailed(
                                        "app-server connection became unusable while resuming a thread; restart AgentHarness"
                                            .to_owned(),
                                    ),
                                )));
                                }
                                effects
                            }
                        }
                    }
                }
                Effect::SwitchOpenRouterConversation { id, model } => {
                    let Some(openrouter) = &self.openrouter else {
                        self.state.reduce(Action::Event(
                            DomainEvent::OpenRouterConversationSwitchFailed {
                                conversation_id: id,
                                message: "OpenRouter runtime is unavailable".to_owned(),
                            },
                        ));
                        continue;
                    };
                    if model.provider != ProviderId::OpenRouter {
                        self.state.reduce(Action::Event(
                            DomainEvent::OpenRouterConversationSwitchFailed {
                                conversation_id: id,
                                message: "invalid OpenRouter model selection".to_owned(),
                            },
                        ));
                        continue;
                    }
                    match openrouter.load_conversation(id.clone()).await {
                        Ok(conversation) => self.state.reduce(Action::Event(
                            DomainEvent::OpenRouterConversationRestored {
                                conversation_id: id,
                                history: super::lifecycle::openrouter_history(&conversation),
                                model,
                                automatic: false,
                            },
                        )),
                        Err(error) => self.state.reduce(Action::Event(
                            DomainEvent::OpenRouterConversationSwitchFailed {
                                conversation_id: id,
                                message: error.to_string(),
                            },
                        )),
                    }
                }
                Effect::DeleteThreads { ids } => self.delete_threads(ids).await,
                Effect::DeleteOpenRouterConversations { ids } => {
                    self.delete_conversations(Vec::new(), ids).await
                }
                Effect::DeleteConversations {
                    codex_ids,
                    openrouter_ids,
                } => self.delete_conversations(codex_ids, openrouter_ids).await,
                Effect::SendMessage { text } => match self.send_message(&text).await {
                    Ok(effects) => effects,
                    Err(error) => self.reduce_mutating_error(error),
                },
                Effect::SendOpenRouterMessage { text } => {
                    let Ok(model) = self.selected_model(ProviderId::OpenRouter) else {
                        self.state.notice =
                            Some("select an OpenRouter model with /model".to_owned());
                        continue;
                    };
                    let conversation_id = match &self.state.openrouter.conversation {
                        crate::app::OpenRouterConversationState::Ready { id } => Some(id.clone()),
                        crate::app::OpenRouterConversationState::None => None,
                        crate::app::OpenRouterConversationState::ResumeFailed { .. } => {
                            self.state.notice = Some(
                                "resolve the saved OpenRouter conversation with /resume".to_owned(),
                            );
                            continue;
                        }
                    };
                    if self.openrouter.is_none() {
                        self.state.notice = Some("OpenRouter runtime is unavailable".to_owned());
                        continue;
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
                                let produced = self.state.reduce(Action::Event(
                                    DomainEvent::OpenRouterTurnStarted {
                                        conversation_id,
                                        turn_id,
                                    },
                                ));
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
                                    message.push_str(
                                        "; the local turn also could not be marked failed",
                                    );
                                }
                                self.state
                                    .reduce(Action::Event(DomainEvent::TurnOperationFailed(
                                        message,
                                    )))
                            }
                        }
                        Err(error) => {
                            self.state
                                .reduce(Action::Event(DomainEvent::TurnOperationFailed(
                                    error.to_string(),
                                )))
                        }
                    }
                }
                Effect::RefreshOpenRouter => {
                    let Some(openrouter) = &mut self.openrouter else {
                        self.state.notice = Some("OpenRouter runtime is unavailable".to_owned());
                        continue;
                    };
                    match openrouter.revalidate_and_refresh() {
                        Ok(operation_id) => {
                            self.state.openrouter.credential_validation =
                                crate::app::OpenRouterCredentialValidation::Refreshing {
                                    operation_id,
                                };
                        }
                        Err(message) => self.state.notice = Some(message.to_owned()),
                    }
                    Vec::new()
                }
                Effect::LogoutOpenRouter => {
                    let active_turn = match &self.state.turn {
                        crate::app::TurnState::OpenRouterStreaming {
                            conversation_id,
                            turn_id,
                        } => Some((conversation_id.clone(), turn_id.clone())),
                        _ => None,
                    };
                    let Some(openrouter) = &mut self.openrouter else {
                        self.state.notice = Some("OpenRouter runtime is unavailable".to_owned());
                        continue;
                    };
                    let (drained, logout) = openrouter.logout().await;
                    let mut effects = Vec::new();
                    for event in drained {
                        effects.extend(self.reduce_openrouter_service_event(event));
                    }
                    if let Some((conversation_id, turn_id)) = active_turn.filter(|(conversation_id, turn_id)| {
                        matches!(
                            &self.state.turn,
                            crate::app::TurnState::OpenRouterStreaming {
                                conversation_id: active_conversation,
                                turn_id: active_turn,
                            } if active_conversation == conversation_id && active_turn == turn_id
                        )
                    }) {
                        effects.extend(self.state.reduce(Action::Event(
                            DomainEvent::OpenRouterTurnFinished {
                                conversation_id,
                                turn_id,
                                outcome: TurnOutcome::Interrupted,
                                assistant_text: None,
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
                Effect::InterruptOpenRouterTurn => {
                    if let Some(openrouter) = &self.openrouter {
                        openrouter.interrupt_turn();
                    }
                    Vec::new()
                }
                Effect::InterruptTurn { thread_id, turn_id } => {
                    match self.codex_mut()?.interrupt_turn(&thread_id, &turn_id).await {
                        Ok(()) => Vec::new(),
                        Err(error) => self.reduce_mutating_error(error.into()),
                    }
                }
                Effect::Persist(preferences) => {
                    let _ = self.persist_preferences(&preferences)?;
                    Vec::new()
                }
                Effect::Shutdown => {
                    self.shutdown().await?;
                    Vec::new()
                }
            };
            effects.extend(produced);
        }
        Ok(())
    }

    async fn send_message(&mut self, text: &str) -> Result<Vec<Effect>, BackendError> {
        let model = self.selected_model(ProviderId::Codex)?;
        let effort =
            self.state.selected_reasoning.clone().ok_or_else(|| {
                SessionError::Protocol("no reasoning effort is selected".to_owned())
            })?;
        let thread_id = match &self.state.thread {
            ThreadState::Ready { id } => id.clone(),
            ThreadState::None => {
                let thread = self.codex_mut()?.start_thread(&model.id).await?;
                let id = thread.id;
                let effects = self
                    .state
                    .reduce(Action::Event(DomainEvent::ThreadStarted { id: id.clone() }));
                for effect in effects {
                    if let Effect::Persist(preferences) = effect {
                        let _ = self.persist_preferences(&preferences)?;
                    }
                }
                id
            }
            _ => {
                return Err(SessionError::Protocol(
                    "message effect reached a non-sendable thread state".to_owned(),
                )
                .into())
            }
        };
        let response = self
            .codex_mut()?
            .start_turn(&thread_id, text, &model.id, &effort)
            .await?;
        let turn_id = response.turn.id;
        self.completed_items.begin_turn(&thread_id, &turn_id);
        Ok(self.state.reduce(Action::Event(DomainEvent::TurnStarted {
            thread_id,
            turn_id,
        })))
    }
}
