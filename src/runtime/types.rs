use super::*;

#[derive(Clone, Debug)]
pub struct RuntimeConfig {
    pub paths: AppPaths,
    pub codex_override: Option<OsString>,
}

impl RuntimeConfig {
    pub fn discover() -> Result<Self, RuntimeError> {
        Ok(Self {
            paths: AppPaths::discover().map_err(RuntimeError::Paths)?,
            codex_override: std::env::var_os("AGENTHARNESS_CODEX_BIN"),
        })
    }
}

#[derive(Debug, Error)]
pub enum RuntimeError {
    #[error("could not prepare AgentHarness application data: {0}")]
    Paths(std::io::Error),
    #[error("Codex CLI was not found; install codex-cli {TESTED_CODEX_VERSION} or newer, or set AGENTHARNESS_CODEX_BIN")]
    CodexNotFound,
    #[error("Codex CLI version could not be checked: {0}")]
    VersionCheck(String),
    #[error("Codex CLI {0} is unsupported; upgrade to codex-cli {TESTED_CODEX_VERSION} or newer")]
    UnsupportedVersion(String),
    #[error("could not start the dedicated Codex app-server: {0}")]
    AppServer(String),
}

pub struct RuntimeHandle {
    intents: mpsc::Sender<Intent>,
    shutdowns: mpsc::Sender<()>,
    states: watch::Receiver<AppState>,
    task: JoinHandle<()>,
}

impl RuntimeHandle {
    pub fn spawn(config: RuntimeConfig) -> Self {
        let initial = AppState {
            connection: ConnectionState::Connecting,
            ..AppState::default()
        };
        let (state_tx, state_rx) = watch::channel(initial);
        let (intent_tx, intent_rx) = mpsc::channel(32);
        let (shutdown_tx, shutdown_rx) = mpsc::channel(1);
        let task = tokio::spawn(run_backend(config, intent_rx, shutdown_rx, state_tx));
        Self {
            intents: intent_tx,
            shutdowns: shutdown_tx,
            states: state_rx,
            task,
        }
    }

    pub fn subscribe(&self) -> watch::Receiver<AppState> {
        self.states.clone()
    }

    pub async fn send(&self, intent: Intent) {
        let _ = self.intents.send(intent).await;
    }

    pub fn try_send(&self, intent: Intent) -> Result<(), &'static str> {
        self.intents.try_send(intent).map_err(|error| match error {
            mpsc::error::TrySendError::Full(_) => "background backend is busy; try again",
            mpsc::error::TrySendError::Closed(_) => "background backend has stopped",
        })
    }

    pub fn request_shutdown(&self) {
        let _ = self.shutdowns.try_send(());
    }

    pub async fn shutdown(mut self) {
        let _ = time::timeout(Duration::from_millis(250), self.shutdowns.send(())).await;
        if time::timeout(SHUTDOWN_BOUND, &mut self.task).await.is_err() {
            self.task.abort();
            let _ = self.task.await;
        }
    }
}
