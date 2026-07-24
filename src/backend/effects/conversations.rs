use super::*;

impl<P: PreferencesPort, B: BrowserOpener> BackendCoordinator<P, B> {
    pub(super) async fn start_new_thread_effect(&mut self) -> Result<Vec<Effect>, BackendError> {
        Ok({
            let model = self.selected_model(ProviderId::Codex)?;
            match self.codex_mut()?.start_thread(&model.id).await {
                Ok(thread) => self
                    .state
                    .reduce(Action::Event(DomainEvent::NewThreadSucceeded {
                        id: thread.id,
                    })),
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
                                        "app-server connection became unusable while starting a new thread; restart Vairë"
                                            .to_owned(),
                                    ),
                                )));
                    }
                    effects
                }
            }
        })
    }
    pub(super) async fn start_new_openrouter_conversation_effect(&mut self) -> Vec<Effect> {
        {
            let Some(openrouter) = &self.openrouter else {
                self.state.notice = Some("OpenRouter runtime is unavailable".to_owned());
                return Vec::new();
            };
            match openrouter.create_conversation().await {
                Ok(conversation_id) => {
                    self.state
                        .reduce(Action::Event(DomainEvent::OpenRouterConversationStarted {
                            conversation_id,
                        }))
                }
                Err(error) => {
                    self.state.notice = Some(error.to_string());
                    Vec::new()
                }
            }
        }
    }
    pub(super) async fn list_threads_effect(&mut self) -> Vec<Effect> {
        {
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
                        updated_at: i64::try_from(summary.updated_at_ms).unwrap_or(i64::MAX),
                    }
                }));
                self.state
                    .reduce(Action::Event(DomainEvent::ThreadListLoaded(choices)))
            }
        }
    }
    pub(super) async fn resume_thread_effect(
        &mut self,
        id: String,
    ) -> Result<Vec<Effect>, BackendError> {
        Ok({
            self.state
                .reduce(Action::Event(DomainEvent::ResumeStarted { id: id.clone() }));
            let model = self.selected_model(ProviderId::Codex)?;
            match self.codex_mut()?.resume_thread(&id, &model.id).await {
                Ok(thread) => self
                    .state
                    .reduce(Action::Event(DomainEvent::ResumeSucceeded {
                        id,
                        history: history_entries(&thread),
                    })),
                Err(error) => self.state.reduce(Action::Event(DomainEvent::ResumeFailed {
                    id,
                    message: error.to_string(),
                })),
            }
        })
    }
    pub(super) async fn switch_thread_effect(
        &mut self,
        id: String,
        model: ModelKey,
        reasoning: String,
    ) -> Result<Vec<Effect>, BackendError> {
        Ok({
            if model.provider != ProviderId::Codex {
                self.state
                    .reduce(Action::Event(DomainEvent::ThreadSwitchFailed {
                        id,
                        message: "invalid Codex model selection".to_owned(),
                    }))
            } else {
                match self.codex_mut()?.resume_thread(&id, &model.id).await {
                    Ok(thread) => {
                        self.state
                            .reduce(Action::Event(DomainEvent::ThreadSwitchSucceeded {
                                id,
                                history: history_entries(&thread),
                                model,
                                reasoning,
                            }))
                    }
                    Err(error) => {
                        let fatal = is_fatal_transport(&error);
                        let mut effects =
                            self.state
                                .reduce(Action::Event(DomainEvent::ThreadSwitchFailed {
                                    id,
                                    message: error.to_string(),
                                }));
                        if fatal {
                            effects.extend(self.state.reduce(Action::Event(
                                    DomainEvent::ConnectionFailed(
                                        "app-server connection became unusable while resuming a thread; restart Vairë"
                                            .to_owned(),
                                    ),
                                )));
                        }
                        effects
                    }
                }
            }
        })
    }
    pub(super) async fn switch_openrouter_conversation_effect(
        &mut self,
        id: crate::provider::OpenRouterConversationId,
        model: ModelKey,
    ) -> Vec<Effect> {
        {
            let Some(openrouter) = &self.openrouter else {
                self.state.reduce(Action::Event(
                    DomainEvent::OpenRouterConversationSwitchFailed {
                        conversation_id: id,
                        message: "OpenRouter runtime is unavailable".to_owned(),
                    },
                ));
                return Vec::new();
            };
            if model.provider != ProviderId::OpenRouter {
                self.state.reduce(Action::Event(
                    DomainEvent::OpenRouterConversationSwitchFailed {
                        conversation_id: id,
                        message: "invalid OpenRouter model selection".to_owned(),
                    },
                ));
                return Vec::new();
            }
            match openrouter.load_conversation(id.clone()).await {
                Ok(conversation) => {
                    self.state
                        .reduce(Action::Event(DomainEvent::OpenRouterConversationRestored {
                            conversation_id: id,
                            history: super::lifecycle::openrouter_history(&conversation),
                            model,
                            automatic: false,
                        }))
                }
                Err(error) => self.state.reduce(Action::Event(
                    DomainEvent::OpenRouterConversationSwitchFailed {
                        conversation_id: id,
                        message: error.to_string(),
                    },
                )),
            }
        }
    }
}
