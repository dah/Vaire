use super::*;
use crate::credentials::SecretValue;

pub enum RuntimeCommand {
    Intent(Intent),
    OpenRouterCredential(SecretValue),
    ClaudeAuthFinished {
        request: crate::app::ClaudeAuthRequest,
        result: Result<(), crate::claude::ClaudeRuntimeError>,
    },
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
    claude_auth_executable: Result<PathBuf, ClaudeRuntimeError>,
    claude_auth_home: PathBuf,
    claude_auth_cwd: PathBuf,
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
        let claude_auth_executable = resolve_claude(config.claude_override.as_deref());
        let claude_auth_home = config.paths.claude_cli_home_dir.clone();
        let claude_auth_cwd = config.paths.claude_conversation_dir.clone();
        let task = tokio::spawn(run_backend(
            config,
            Some(claude_auth_executable.clone()),
            intent_rx,
            shutdown_rx,
            state_tx,
        ));
        Self {
            intents: intent_tx,
            shutdowns: shutdown_tx,
            states: state_rx,
            task,
            claude_auth_executable,
            claude_auth_home,
            claude_auth_cwd,
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

    pub async fn run_claude_auth(
        &self,
        action: crate::claude::ClaudeAuthAction,
        cancellation: tokio_util::sync::CancellationToken,
    ) -> Result<(), crate::claude::ClaudeRuntimeError> {
        let executable = self
            .claude_auth_executable
            .as_ref()
            .map_err(|error| *error)?;
        verify_claude_version_cancellable(
            executable,
            &self.claude_auth_home,
            Duration::from_secs(3),
            &cancellation,
        )
        .await?;
        crate::claude::run_claude_auth_action(
            executable,
            &self.claude_auth_home,
            &self.claude_auth_cwd,
            action,
            cancellation,
        )
        .await
    }

    pub async fn finish_claude_auth(
        &self,
        request: crate::app::ClaudeAuthRequest,
        result: Result<(), crate::claude::ClaudeRuntimeError>,
    ) -> Result<(), &'static str> {
        self.intents
            .send(RuntimeCommand::ClaudeAuthFinished { request, result })
            .await
            .map_err(|_| "background backend has stopped")
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

#[cfg(test)]
mod claude_auth_tests {
    use std::fs;
    use std::os::unix::fs::{symlink, PermissionsExt};
    use std::path::Path;

    use super::*;

    fn write_fake_claude(path: &Path, version: &str, marker: &Path) {
        fs::write(
            path,
            format!(
                r#"#!/bin/sh
if [ "$1" = "--version" ]; then
  printf '%s\n' '{version}'
  exit 0
fi
case " $* " in
  *" auth status --json "*)
    printf '%s\n' '{{"loggedIn":true,"authMethod":"claude.ai","apiProvider":"firstParty"}}'
    ;;
  *" auth login --claudeai "*)
    : > '{marker}'
    ;;
esac
"#,
                marker = marker.display(),
            ),
        )
        .unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    }

    fn config(root: &Path, claude_override: &Path) -> RuntimeConfig {
        let paths = AppPaths::from_home(root);
        for directory in [&paths.claude_cli_home_dir, &paths.claude_conversation_dir] {
            fs::create_dir_all(directory).unwrap();
            fs::set_permissions(directory, fs::Permissions::from_mode(0o700)).unwrap();
        }
        RuntimeConfig {
            paths,
            codex_override: Some(root.join("missing-codex").into_os_string()),
            claude_override: Some(claude_override.as_os_str().to_owned()),
        }
    }

    #[tokio::test]
    async fn auth_reuses_the_executable_identity_pinned_for_backend_startup() {
        let root = tempfile::tempdir().unwrap();
        let first = root.path().join("claude-first");
        let second = root.path().join("claude-second");
        let first_marker = root.path().join("first-login");
        let second_marker = root.path().join("second-login");
        write_fake_claude(&first, crate::claude::TESTED_CLAUDE_VERSION, &first_marker);
        write_fake_claude(
            &second,
            crate::claude::TESTED_CLAUDE_VERSION,
            &second_marker,
        );
        let link = root.path().join("claude-current");
        symlink(&first, &link).unwrap();

        let runtime = RuntimeHandle::spawn(config(root.path(), &link));
        fs::remove_file(&link).unwrap();
        symlink(&second, &link).unwrap();

        runtime
            .run_claude_auth(
                crate::claude::ClaudeAuthAction::Login,
                tokio_util::sync::CancellationToken::new(),
            )
            .await
            .unwrap();

        assert!(first_marker.exists());
        assert!(!second_marker.exists());
        runtime.shutdown().await;
    }

    #[tokio::test]
    async fn auth_rejects_a_freshly_outdated_pinned_executable_before_login() {
        let root = tempfile::tempdir().unwrap();
        let executable = root.path().join("claude-outdated");
        let marker = root.path().join("outdated-login");
        write_fake_claude(&executable, "2.1.177", &marker);
        let runtime = RuntimeHandle::spawn(config(root.path(), &executable));

        let result = runtime
            .run_claude_auth(
                crate::claude::ClaudeAuthAction::Login,
                tokio_util::sync::CancellationToken::new(),
            )
            .await;

        assert_eq!(result, Err(ClaudeRuntimeError::UnsupportedVersion));
        assert!(!marker.exists());
        runtime.shutdown().await;
    }
}
