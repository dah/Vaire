use super::*;

impl<P: PreferencesPort, B: BrowserOpener> BackendCoordinator<P, B> {
    pub(super) async fn start_login_effect(&mut self) -> Result<Vec<Effect>, BackendError> {
        Ok(match self.codex_mut()?.start_login().await {
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
        })
    }
    pub(super) async fn start_device_login_effect(&mut self) -> Result<Vec<Effect>, BackendError> {
        Ok(match self.codex_mut()?.start_device_login().await {
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
        })
    }
    pub(super) async fn cancel_login_effect(
        &mut self,
        login_id: String,
    ) -> Result<Vec<Effect>, BackendError> {
        Ok(self.cancel_login(&login_id).await)
    }
    pub(super) async fn logout_effect(&mut self) -> Result<Vec<Effect>, BackendError> {
        Ok(match self.codex_mut()?.logout().await {
            Ok(()) => self.state.reduce(Action::Event(DomainEvent::LoggedOut)),
            Err(error) => {
                self.state.notice = Some(error.to_string());
                Vec::new()
            }
        })
    }
}
