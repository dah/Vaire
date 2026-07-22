use super::*;

impl<P: PreferencesPort, B: BrowserOpener> BackendCoordinator<P, B> {
    pub(in crate::backend) async fn execute_effects(
        &mut self,
        initial: Vec<Effect>,
    ) -> Result<(), BackendError> {
        let mut effects = VecDeque::from(initial);
        while let Some(effect) = effects.pop_front() {
            let produced = match effect {
                Effect::StartLogin => match self.session.start_login().await {
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
                Effect::StartDeviceLogin => match self.session.start_device_login().await {
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
                Effect::Logout => match self.session.logout().await {
                    Ok(()) => self.state.reduce(Action::Event(DomainEvent::LoggedOut)),
                    Err(error) => {
                        self.state.notice = Some(error.to_string());
                        Vec::new()
                    }
                },
                Effect::StartNewThread => {
                    let model = self.selected_model()?;
                    match self.session.start_thread(&model).await {
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
                Effect::ListThreads => match self.session.list_threads().await {
                    Ok(threads) => self
                        .state
                        .reduce(Action::Event(DomainEvent::ThreadListLoaded(
                            thread_choices(threads),
                        ))),
                    Err(error) => self
                        .state
                        .reduce(Action::Event(DomainEvent::ThreadListFailed(
                            error.to_string(),
                        ))),
                },
                Effect::ResumeThread { id } => {
                    self.state
                        .reduce(Action::Event(DomainEvent::ResumeStarted { id: id.clone() }));
                    let model = self.selected_model()?;
                    match self.session.resume_thread(&id, &model).await {
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
                Effect::SwitchThread { id } => {
                    let model = self.selected_model()?;
                    match self.session.resume_thread(&id, &model).await {
                        Ok(thread) => {
                            self.state
                                .reduce(Action::Event(DomainEvent::ThreadSwitchSucceeded {
                                    id,
                                    history: history_entries(&thread),
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
                                        "app-server connection became unusable while resuming a thread; restart AgentHarness"
                                            .to_owned(),
                                    ),
                                )));
                            }
                            effects
                        }
                    }
                }
                Effect::DeleteThreads { ids } => self.delete_threads(ids).await,
                Effect::SendMessage { text } => match self.send_message(&text).await {
                    Ok(effects) => effects,
                    Err(error) => self.reduce_mutating_error(error),
                },
                Effect::InterruptTurn { thread_id, turn_id } => {
                    match self.session.interrupt_turn(&thread_id, &turn_id).await {
                        Ok(()) => Vec::new(),
                        Err(error) => self.reduce_mutating_error(error.into()),
                    }
                }
                Effect::Persist(preferences) => {
                    self.may_persist = true;
                    self.preferences.save(&preferences)?;
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
        let model = self.selected_model()?;
        let effort =
            self.state.selected_reasoning.clone().ok_or_else(|| {
                SessionError::Protocol("no reasoning effort is selected".to_owned())
            })?;
        let thread_id = match &self.state.thread {
            ThreadState::Ready { id } => id.clone(),
            ThreadState::None => {
                let thread = self.session.start_thread(&model).await?;
                let id = thread.id;
                let effects = self
                    .state
                    .reduce(Action::Event(DomainEvent::ThreadStarted { id: id.clone() }));
                for effect in effects {
                    if let Effect::Persist(preferences) = effect {
                        self.may_persist = true;
                        self.preferences.save(&preferences)?;
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
            .session
            .start_turn(&thread_id, text, &model, &effort)
            .await?;
        let turn_id = response.turn.id;
        self.completed_items.begin_turn(&thread_id, &turn_id);
        Ok(self.state.reduce(Action::Event(DomainEvent::TurnStarted {
            thread_id,
            turn_id,
        })))
    }
}
