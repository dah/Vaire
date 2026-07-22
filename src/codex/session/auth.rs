use super::*;

impl SessionService {
    pub async fn read_account(&self) -> Result<AccountState, SessionError> {
        let response: AccountReadResponse = decode(
            "account/read",
            self.transport
                .request_default("account/read", json!({"refreshToken": false}))
                .await?,
        )?;
        let Some(account) = response.account else {
            return Ok(AccountState::SignedOut);
        };
        if account.kind != "chatgpt" {
            return Ok(AccountState::Unsupported(account.kind));
        }
        Ok(AccountState::Chatgpt {
            scope: account
                .email
                .as_deref()
                .and_then(AccountScope::from_chatgpt_email),
        })
    }

    pub async fn start_login(&self) -> Result<LoginChallenge, SessionError> {
        let response: LoginAccountResponse = decode(
            "account/login/start",
            self.transport
                .request_default("account/login/start", LoginAccountParams::chatgpt())
                .await?,
        )?;
        if response.kind != "chatgpt" {
            return Err(SessionError::Protocol(
                "login did not return the ChatGPT browser flow".to_owned(),
            ));
        }
        let login_id = response
            .login_id
            .filter(|value| valid_identifier(value))
            .ok_or_else(|| {
                SessionError::Protocol(
                    "login response omitted or returned invalid loginId".to_owned(),
                )
            })?;
        let auth_url = response
            .auth_url
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| SessionError::Protocol("login response omitted authUrl".to_owned()))?;
        Ok(LoginChallenge { login_id, auth_url })
    }

    pub async fn start_device_login(&self) -> Result<DeviceLoginChallenge, SessionError> {
        let response: LoginAccountResponse = decode(
            "account/login/start",
            self.transport
                .request_default(
                    "account/login/start",
                    LoginAccountParams::chatgpt_device_code(),
                )
                .await?,
        )?;
        if response.kind != "chatgptDeviceCode" {
            return Err(SessionError::Protocol(
                "login did not return the ChatGPT device-code flow".to_owned(),
            ));
        }
        let login_id = response
            .login_id
            .filter(|value| valid_identifier(value))
            .ok_or_else(|| {
                SessionError::Protocol(
                    "device login response omitted or returned invalid loginId".to_owned(),
                )
            })?;
        let verification_url = response
            .verification_url
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| {
                SessionError::Protocol("device login response omitted verificationUrl".to_owned())
            })?;
        let user_code = response
            .user_code
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| {
                SessionError::Protocol("device login response omitted userCode".to_owned())
            })?;
        Ok(DeviceLoginChallenge {
            login_id,
            verification_url,
            user_code,
        })
    }

    pub async fn cancel_login(
        &self,
        login_id: &str,
    ) -> Result<CancelLoginAccountStatus, SessionError> {
        let response: CancelLoginAccountResponse = decode(
            "account/login/cancel",
            self.transport
                .request_default(
                    "account/login/cancel",
                    CancelLoginAccountParams::new(login_id),
                )
                .await?,
        )?;
        Ok(response.status)
    }

    pub async fn logout(&self) -> Result<(), SessionError> {
        let _: LogoutAccountResponse = decode(
            "account/logout",
            self.transport
                .request_default("account/logout", json!({}))
                .await?,
        )?;
        Ok(())
    }
}
