use super::*;

impl<P: PreferencesPort, B: BrowserOpener> BackendCoordinator<P, B> {
    pub(in crate::backend) async fn delete_conversations(
        &mut self,
        codex_ids: Vec<String>,
        openrouter_ids: Vec<crate::provider::OpenRouterConversationId>,
    ) -> Vec<Effect> {
        self.delete_all_conversations(codex_ids, openrouter_ids, Vec::new())
            .await
    }

    pub(in crate::backend) async fn delete_all_conversations(
        &mut self,
        codex_ids: Vec<String>,
        openrouter_ids: Vec<crate::provider::OpenRouterConversationId>,
        claude_ids: Vec<crate::provider::ClaudeSessionId>,
    ) -> Vec<Effect> {
        let requested = codex_ids
            .len()
            .saturating_add(openrouter_ids.len())
            .saturating_add(claude_ids.len());
        let mut deleted = Vec::new();
        let mut failures = Vec::new();
        for id in codex_ids {
            let protected = self.state.active_provider == ProviderId::Codex
                && self.state.active_saved_thread_id() == Some(id.as_str());
            if protected {
                failures.push(ThreadDeletionFailure {
                    id,
                    message: "active saved conversation is protected".to_owned(),
                });
                continue;
            }
            let result = match &mut self.session {
                Some(session) => session.delete_thread(&id).await,
                None => Err(SessionError::Protocol(
                    "Codex provider is unavailable".to_owned(),
                )),
            };
            match result {
                Ok(()) => deleted.push(id),
                Err(error) => failures.push(ThreadDeletionFailure {
                    id,
                    message: error.to_string(),
                }),
            }
        }
        for id in openrouter_ids {
            let id_text = id.as_str().to_owned();
            let protected = self.state.active_provider == ProviderId::OpenRouter
                && self.state.active_saved_thread_id() == Some(id.as_str());
            if protected {
                failures.push(ThreadDeletionFailure {
                    id: id_text,
                    message: "active saved conversation is protected".to_owned(),
                });
                continue;
            }
            let Some(openrouter) = &self.openrouter else {
                failures.push(ThreadDeletionFailure {
                    id: id_text,
                    message: "OpenRouter runtime is unavailable".to_owned(),
                });
                continue;
            };
            match openrouter.delete_conversation(id).await {
                Ok(()) => deleted.push(id_text),
                Err(error) => failures.push(ThreadDeletionFailure {
                    id: id_text,
                    message: error.to_string(),
                }),
            }
        }
        for id in claude_ids {
            let id_text = id.as_str().to_owned();
            let protected = self.state.active_provider == ProviderId::Claude
                && self.state.active_saved_thread_id() == Some(id.as_str());
            if protected {
                failures.push(ThreadDeletionFailure {
                    id: id_text,
                    message: "active saved conversation is protected".to_owned(),
                });
                continue;
            }
            let Some(claude) = &self.claude else {
                failures.push(ThreadDeletionFailure {
                    id: id_text,
                    message: "Claude Code runtime is unavailable".to_owned(),
                });
                continue;
            };
            match claude.service.delete_session(id).await {
                Ok(()) => deleted.push(id_text),
                Err(error) => failures.push(ThreadDeletionFailure {
                    id: id_text,
                    message: error.to_string(),
                }),
            }
        }
        self.state
            .reduce(Action::Event(DomainEvent::ThreadDeletionFinished {
                requested,
                deleted,
                failures,
            }))
    }

    pub(in crate::backend) async fn delete_threads(&mut self, ids: Vec<String>) -> Vec<Effect> {
        let requested = ids.len();
        let mut protected_ids = Vec::new();
        if let Some(id) = self.state.preferences.codex.auto_resume_thread_id.clone() {
            protected_ids.push(id);
        }
        if let ThreadState::Ready { id } = &self.state.thread {
            if !protected_ids.contains(id) {
                protected_ids.push(id.clone());
            }
        }
        let mut deleted = Vec::new();
        let mut failures = Vec::new();
        let mut fatal_message = None;
        let mut pending = ids.into_iter();
        while let Some(id) = pending.next() {
            if protected_ids.contains(&id) {
                failures.push(ThreadDeletionFailure {
                    id,
                    message: "active saved thread is protected".to_owned(),
                });
                continue;
            }
            let result = match &mut self.session {
                Some(session) => session.delete_thread(&id).await,
                None => Err(SessionError::Protocol(
                    "Codex provider is unavailable".to_owned(),
                )),
            };
            match result {
                Ok(()) => deleted.push(id),
                Err(error) => {
                    let fatal = is_fatal_transport(&error);
                    failures.push(ThreadDeletionFailure {
                        id,
                        message: error.to_string(),
                    });
                    if fatal {
                        for skipped in pending {
                            failures.push(ThreadDeletionFailure {
                                id: skipped,
                                message: "not attempted because the app-server connection became unusable"
                                    .to_owned(),
                            });
                        }
                        fatal_message = Some(
                            "app-server connection became unusable during thread deletion; restart Vairë"
                                .to_owned(),
                        );
                        break;
                    }
                }
            }
        }
        let mut effects = self
            .state
            .reduce(Action::Event(DomainEvent::ThreadDeletionFinished {
                requested,
                deleted,
                failures,
            }));
        if let Some(message) = fatal_message {
            effects.extend(
                self.state
                    .reduce(Action::Event(DomainEvent::ConnectionFailed(message))),
            );
        }
        effects
    }

    pub(in crate::backend) async fn cancel_login(&mut self, login_id: &str) -> Vec<Effect> {
        let status = match self.codex_mut() {
            Ok(session) => session.cancel_login(login_id).await,
            Err(_) => {
                self.state.notice = Some("Codex provider is unavailable".to_owned());
                return Vec::new();
            }
        };
        let status = match status {
            Ok(status) => status,
            Err(error) => {
                self.state.notice = Some(format!(
                    "could not cancel ChatGPT sign-in: {error}; use /logout to retry"
                ));
                return Vec::new();
            }
        };

        let account = match self.codex_mut() {
            Ok(session) => session.read_account().await,
            Err(_) => {
                self.state.notice = Some("Codex provider is unavailable".to_owned());
                return Vec::new();
            }
        };
        match account {
            Ok(account) => {
                let effects = self.reduce_account(account);
                if matches!(self.state.auth, crate::app::AuthState::SignedOut) {
                    self.state.notice = Some(match status {
                        CancelLoginAccountStatus::Canceled => {
                            "ChatGPT sign-in cancelled; use /login to try again".to_owned()
                        }
                        CancelLoginAccountStatus::NotFound => {
                            "no pending ChatGPT sign-in was found; use /login to try again"
                                .to_owned()
                        }
                    });
                }
                effects
            }
            Err(error) => self.state.reduce(Action::Event(DomainEvent::LoginFailed(
                format!(
                    "ChatGPT sign-in was cancelled, but account state could not be refreshed: {error}; use /login to retry"
                ),
            ))),
        }
    }
}
