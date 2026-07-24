use super::*;
use crate::credentials::SecretValue;

pub enum RuntimeCommand {
    Intent(Intent),
    OpenRouterCredential(SecretValue),
    ClaudeCredential(SecretValue),
}

#[derive(Clone, Debug)]
pub struct RuntimeConfig {
    pub paths: AppPaths,
    pub codex_override: Option<OsString>,
    pub claude_override: Option<OsString>,
}

impl RuntimeConfig {
    pub fn discover() -> Result<Self, RuntimeError> {
        let home = std::env::var_os("HOME").map(PathBuf::from).ok_or_else(|| {
            RuntimeError::Paths(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "HOME is not set",
            ))
        })?;
        Self::discover_with_home(
            &home,
            || std::env::var_os("VAIRE_CODEX_BIN"),
            || std::env::var_os("VAIRE_CLAUDE_BIN"),
        )
    }

    fn discover_with_home(
        home: &Path,
        codex_lookup: impl FnOnce() -> Option<OsString>,
        claude_lookup: impl FnOnce() -> Option<OsString>,
    ) -> Result<Self, RuntimeError> {
        #[cfg(target_os = "macos")]
        crate::platform::migrate_support_root(home)
            .map_err(|error| RuntimeError::SupportRootMigration(error.to_string()))?;
        Ok(Self {
            paths: AppPaths::from_home(home),
            codex_override: codex_lookup(),
            claude_override: claude_lookup(),
        })
    }
}

#[derive(Debug, Error)]
pub enum RuntimeError {
    #[error("could not migrate Vairë application data: {0}")]
    SupportRootMigration(String),
    #[error("could not prepare Vairë application data: {0}")]
    Paths(std::io::Error),
    #[error("Codex CLI was not found; install codex-cli {TESTED_CODEX_VERSION} or newer, or set VAIRE_CODEX_BIN")]
    CodexNotFound,
    #[error("Codex CLI version could not be checked: {0}")]
    VersionCheck(String),
    #[error("Codex CLI {0} is unsupported; upgrade to codex-cli {TESTED_CODEX_VERSION} or newer")]
    UnsupportedVersion(String),
    #[error("could not start the dedicated Codex app-server: {0}")]
    AppServer(String),
    #[error("could not prepare OpenRouter support: {0}")]
    OpenRouter(String),
    #[error("could not prepare Claude Code support: {0}")]
    Claude(String),
}

#[cfg(all(test, target_os = "macos"))]
mod discovery_tests {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::*;

    #[test]
    fn migration_failure_precedes_codex_override_lookup() {
        let home = tempfile::tempdir().unwrap();
        let paths = AppPaths::from_home(home.path());
        let parent = paths.support_dir.parent().unwrap();
        fs::create_dir_all(parent).unwrap();
        for name in ["AgentHarness", "vaire"] {
            let root = parent.join(name);
            fs::create_dir(&root).unwrap();
            fs::set_permissions(root, fs::Permissions::from_mode(0o700)).unwrap();
        }
        let looked_up = AtomicBool::new(false);
        let result = RuntimeConfig::discover_with_home(
            home.path(),
            || {
                looked_up.store(true, Ordering::SeqCst);
                None
            },
            || None,
        );
        assert!(matches!(result, Err(RuntimeError::SupportRootMigration(_))));
        assert!(!looked_up.load(Ordering::SeqCst));
    }
}

pub struct RuntimeHandle {
    intents: mpsc::Sender<RuntimeCommand>,
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
        let _ = self.intents.send(RuntimeCommand::Intent(intent)).await;
    }

    pub fn try_send(&self, intent: Intent) -> Result<(), &'static str> {
        self.intents
            .try_send(RuntimeCommand::Intent(intent))
            .map_err(|error| match error {
                mpsc::error::TrySendError::Full(_) => "background backend is busy; try again",
                mpsc::error::TrySendError::Closed(_) => "background backend has stopped",
            })
    }

    pub fn try_send_openrouter_credential(
        &self,
        value: SecretValue,
    ) -> Result<(), (SecretValue, &'static str)> {
        self.intents
            .try_send(RuntimeCommand::OpenRouterCredential(value))
            .map_err(|error| {
                let (command, message) = match error {
                    mpsc::error::TrySendError::Full(command) => {
                        (command, "background backend is busy; try again")
                    }
                    mpsc::error::TrySendError::Closed(command) => {
                        (command, "background backend has stopped")
                    }
                };
                let RuntimeCommand::OpenRouterCredential(value) = command else {
                    unreachable!("submitted an OpenRouter credential command")
                };
                (value, message)
            })
    }

    pub fn try_send_provider_credential(
        &self,
        provider: crate::provider::ProviderId,
        value: SecretValue,
    ) -> Result<(), (SecretValue, &'static str)> {
        match provider {
            crate::provider::ProviderId::OpenRouter => self.try_send_openrouter_credential(value),
            crate::provider::ProviderId::Claude => self.try_send_claude_credential(value),
            crate::provider::ProviderId::Codex => {
                Err((value, "Codex authentication does not accept an API key"))
            }
        }
    }

    pub fn try_send_claude_credential(
        &self,
        value: SecretValue,
    ) -> Result<(), (SecretValue, &'static str)> {
        self.intents
            .try_send(RuntimeCommand::ClaudeCredential(value))
            .map_err(|error| {
                let (command, message) = match error {
                    mpsc::error::TrySendError::Full(command) => {
                        (command, "background backend is busy; try again")
                    }
                    mpsc::error::TrySendError::Closed(command) => {
                        (command, "background backend has stopped")
                    }
                };
                let RuntimeCommand::ClaudeCredential(value) = command else {
                    unreachable!("submitted a Claude credential command")
                };
                (value, message)
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
