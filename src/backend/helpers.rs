use super::*;

impl<P: PreferencesPort, B: BrowserOpener> BackendCoordinator<P, B> {
    pub(in crate::backend) fn reduce_account(&mut self, account: AccountState) -> Vec<Effect> {
        match account {
            AccountState::SignedOut => self.state.reduce(Action::Event(DomainEvent::LoggedOut)),
            AccountState::Chatgpt { scope } => self
                .state
                .reduce(Action::Event(DomainEvent::AccountLoaded(scope))),
            AccountState::Unsupported(kind) => {
                self.state
                    .reduce(Action::Event(DomainEvent::UnsupportedAccount(format!(
                        "unsupported account type {kind}; use ChatGPT login"
                    ))))
            }
        }
    }

    pub(in crate::backend) fn selected_model(&self) -> Result<String, BackendError> {
        self.state
            .selected_model
            .clone()
            .ok_or_else(|| SessionError::Protocol("no model is selected".to_owned()).into())
    }

    pub(in crate::backend) fn reduce_mutating_error(&mut self, error: BackendError) -> Vec<Effect> {
        if matches!(
            &error,
            BackendError::Session(SessionError::Transport(TransportError::Timeout))
        ) {
            self.state
                .reduce(Action::Event(DomainEvent::ConnectionFailed(
                    "app-server timed out during a thread or turn change; restart AgentHarness before retrying"
                        .to_owned(),
                )))
        } else if let BackendError::Session(SessionError::Transport(
            TransportError::SafetyViolation(method),
        )) = error
        {
            self.state
                .reduce(Action::Event(DomainEvent::SafetyViolation(method)))
        } else {
            self.state
                .reduce(Action::Event(DomainEvent::TurnOperationFailed(
                    error.to_string(),
                )))
        }
    }
}

pub(in crate::backend) fn is_fatal_transport(error: &SessionError) -> bool {
    matches!(
        error,
        SessionError::Transport(TransportError::Timeout | TransportError::SafetyViolation(_))
    )
}

pub(in crate::backend) fn load_notice_message(notice: Option<LoadNotice>) -> Option<String> {
    match notice {
        None | Some(LoadNotice::Missing) => None,
        Some(notice) => Some(format!("preferences were not restored: {notice:?}")),
    }
}
