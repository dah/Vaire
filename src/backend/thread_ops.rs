use super::*;

impl<P: PreferencesPort, B: BrowserOpener> BackendCoordinator<P, B> {
    pub(in crate::backend) async fn delete_threads(&mut self, ids: Vec<String>) -> Vec<Effect> {
        let requested = ids.len();
        let mut protected_ids = Vec::new();
        if let Some(id) = self.state.preferences.thread_id.clone() {
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
            match self.session.delete_thread(&id).await {
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
                            "app-server connection became unusable during thread deletion; restart AgentHarness"
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
        let status = match self.session.cancel_login(login_id).await {
            Ok(status) => status,
            Err(error) => {
                self.state.notice = Some(format!(
                    "could not cancel ChatGPT sign-in: {error}; use /logout to retry"
                ));
                return Vec::new();
            }
        };

        match self.session.read_account().await {
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
