use super::*;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AccountState {
    SignedOut,
    Chatgpt { scope: Option<AccountScope> },
    Unsupported(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LoginChallenge {
    pub login_id: String,
    pub auth_url: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeviceLoginChallenge {
    pub login_id: String,
    pub verification_url: String,
    pub user_code: String,
}

#[derive(Clone, Debug, PartialEq)]
pub enum SessionEvent {
    Protocol(ProtocolEvent),
    UnknownNotification(String),
    SafetyViolation(String),
    ConnectionClosed(String),
}

#[derive(Debug, Error)]
pub enum SessionError {
    #[error(transparent)]
    Transport(#[from] TransportError),
    #[error("app-server response for {method} did not match the tested protocol")]
    Decode { method: &'static str },
    #[error("app-server protocol violation: {0}")]
    Protocol(String),
}

pub struct SessionService {
    pub(in crate::codex::session) transport: AppServerTransport,
    pub(in crate::codex::session) paths: IsolationPaths,
    pub(in crate::codex::session) policy: FullAccessPolicy,
}

impl SessionService {
    pub fn new(
        transport: AppServerTransport,
        paths: IsolationPaths,
        policy: FullAccessPolicy,
    ) -> Self {
        Self {
            transport,
            paths,
            policy,
        }
    }

    pub fn generation(&self) -> u64 {
        self.transport.generation()
    }
}
