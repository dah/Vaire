use std::ffi::{OsStr, OsString};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use thiserror::Error;
use tokio::process::Command;
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use tokio::time;

use crate::app::{Action, AppState, AuthState, ConnectionState, Intent};
use crate::backend::BackendCoordinator;
use crate::codex::safety::{ConversationSafetyPolicy, IsolationPaths};
use crate::codex::session::SessionService;
use crate::codex::transport::{AppServerTransport, ProcessSpec};
use crate::diagnostics::{DiagnosticSink, FileDiagnosticSink};
use crate::persistence::FilePreferences;
use crate::platform::{AppPaths, MacOsBrowser};

pub const TESTED_CODEX_VERSION: &str = "0.144.6";
const SHUTDOWN_BOUND: Duration = Duration::from_secs(15);

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
    #[error("could not start the isolated Codex app-server: {0}")]
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

async fn run_backend(
    config: RuntimeConfig,
    mut intents: mpsc::Receiver<Intent>,
    mut shutdowns: mpsc::Receiver<()>,
    states: watch::Sender<AppState>,
) {
    let mut backend = match build_backend(config).await {
        Ok(backend) => backend,
        Err(error) => {
            run_failed_backend(&mut intents, &mut shutdowns, &states, error.to_string()).await;
            return;
        }
    };

    enum Operation<T> {
        Shutdown,
        Finished(T),
    }

    let startup = tokio::select! {
        biased;
        _ = shutdowns.recv() => Operation::Shutdown,
        result = backend.startup() => Operation::Finished(result),
    };
    match startup {
        Operation::Shutdown => {
            shutdown_backend(&mut backend, &states).await;
            return;
        }
        Operation::Finished(Err(error)) => backend.record_error(error.to_string()),
        Operation::Finished(Ok(())) => {}
    }
    publish(&states, backend.state());
    let mut event_open = true;

    loop {
        enum Work {
            Shutdown,
            Intent(Option<Intent>),
            Event(Result<bool, crate::backend::BackendError>),
        }
        let work = if event_open {
            tokio::select! {
                biased;
                _ = shutdowns.recv() => Work::Shutdown,
                intent = intents.recv() => Work::Intent(intent),
                event = backend.pump_event() => Work::Event(event),
            }
        } else {
            tokio::select! {
                biased;
                _ = shutdowns.recv() => Work::Shutdown,
                intent = intents.recv() => Work::Intent(intent),
            }
        };

        match work {
            Work::Shutdown => {
                shutdown_backend(&mut backend, &states).await;
                break;
            }
            Work::Intent(Some(intent)) => {
                let quitting = matches!(intent, Intent::Quit);
                let effects = backend.accept_intent(intent);
                publish(&states, backend.state());
                let execution = if quitting {
                    Operation::Finished(backend.execute_pending(effects).await)
                } else {
                    tokio::select! {
                        biased;
                        _ = shutdowns.recv() => Operation::Shutdown,
                        result = backend.execute_pending(effects) => Operation::Finished(result),
                    }
                };
                match execution {
                    Operation::Shutdown => {
                        shutdown_backend(&mut backend, &states).await;
                        break;
                    }
                    Operation::Finished(Err(error)) => backend.record_error(error.to_string()),
                    Operation::Finished(Ok(())) => {}
                }
                publish(&states, backend.state());
                if quitting {
                    break;
                }
            }
            Work::Intent(None) => {
                let effects = backend.accept_intent(Intent::Quit);
                let _ = backend.execute_pending(effects).await;
                publish(&states, backend.state());
                break;
            }
            Work::Event(Ok(open)) => {
                event_open = open;
                publish(&states, backend.state());
            }
            Work::Event(Err(error)) => {
                backend.record_error(error.to_string());
                event_open = false;
                publish(&states, backend.state());
            }
        }
    }
}

async fn run_failed_backend(
    intents: &mut mpsc::Receiver<Intent>,
    shutdowns: &mut mpsc::Receiver<()>,
    states: &watch::Sender<AppState>,
    message: String,
) {
    let mut state = AppState {
        connection: ConnectionState::Failed(message.clone()),
        auth: AuthState::SignedOut,
        notice: Some(message),
        ..AppState::default()
    };
    publish(states, &state);
    loop {
        let intent = tokio::select! {
            biased;
            _ = shutdowns.recv() => Intent::Quit,
            intent = intents.recv() => match intent {
                Some(intent) => intent,
                None => Intent::Quit,
            },
        };
        let quitting = matches!(intent, Intent::Quit);
        let _ = state.reduce(Action::Intent(intent));
        publish(states, &state);
        if quitting {
            break;
        }
    }
}

async fn shutdown_backend(
    backend: &mut BackendCoordinator<FilePreferences, MacOsBrowser>,
    states: &watch::Sender<AppState>,
) {
    let effects = backend.accept_intent(Intent::Quit);
    publish(states, backend.state());
    if let Err(error) = backend.execute_pending(effects).await {
        backend.record_error(error.to_string());
    }
    publish(states, backend.state());
}

async fn build_backend(
    config: RuntimeConfig,
) -> Result<BackendCoordinator<FilePreferences, MacOsBrowser>, RuntimeError> {
    fs::create_dir_all(&config.paths.support_dir).map_err(RuntimeError::Paths)?;
    fs::set_permissions(&config.paths.support_dir, fs::Permissions::from_mode(0o700))
        .map_err(RuntimeError::Paths)?;
    let isolation =
        IsolationPaths::prepare(&config.paths.runtime_dir).map_err(RuntimeError::Paths)?;
    let diagnostics_path = config.paths.diagnostics_dir.join("agentharness.log");
    let diagnostics: Arc<dyn DiagnosticSink> =
        Arc::new(FileDiagnosticSink::create(&diagnostics_path).map_err(RuntimeError::Paths)?);
    let executable = resolve_codex(config.codex_override.as_deref())?;
    verify_codex_version(&executable, &isolation.codex_home).await?;
    let spec = ProcessSpec::codex(executable, &isolation, &ConversationSafetyPolicy);
    let transport = AppServerTransport::spawn_with_diagnostics(spec, diagnostics)
        .await
        .map_err(|error| RuntimeError::AppServer(error.to_string()))?;
    let session = SessionService::new(transport, isolation, ConversationSafetyPolicy);
    Ok(BackendCoordinator::new(
        session,
        FilePreferences::new(config.paths.preferences_file),
        MacOsBrowser,
    ))
}

fn publish(states: &watch::Sender<AppState>, state: &AppState) {
    states.send_replace(state.clone());
}

pub fn resolve_codex(override_name: Option<&OsStr>) -> Result<PathBuf, RuntimeError> {
    let name = override_name.unwrap_or_else(|| OsStr::new("codex"));
    let candidate = PathBuf::from(name);
    if candidate.components().count() > 1 {
        return canonical_executable(&candidate).ok_or(RuntimeError::CodexNotFound);
    }
    let path = std::env::var_os("PATH").ok_or(RuntimeError::CodexNotFound)?;
    std::env::split_paths(&path)
        .map(|directory| directory.join(name))
        .find_map(|candidate| canonical_executable(&candidate))
        .ok_or(RuntimeError::CodexNotFound)
}

fn canonical_executable(path: &Path) -> Option<PathBuf> {
    is_executable(path)
        .then(|| fs::canonicalize(path).ok())
        .flatten()
}

fn is_executable(path: &Path) -> bool {
    fs::metadata(path)
        .map(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

async fn verify_codex_version(executable: &Path, codex_home: &Path) -> Result<(), RuntimeError> {
    let mut command = Command::new(executable);
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("CODEX_") {
            command.env_remove(key);
        }
    }
    command.env("CODEX_HOME", codex_home).arg("--version");
    let output = time::timeout(Duration::from_secs(3), command.output())
        .await
        .map_err(|_| RuntimeError::VersionCheck("version command timed out".to_owned()))?
        .map_err(|error| RuntimeError::VersionCheck(error.to_string()))?;
    if !output.status.success() {
        return Err(RuntimeError::VersionCheck(format!(
            "version command exited with {}",
            output.status
        )));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let version = find_version(&stdout)
        .ok_or_else(|| RuntimeError::VersionCheck("unrecognized version output".to_owned()))?;
    let minimum = parse_version(TESTED_CODEX_VERSION).expect("tested version is valid");
    if version < minimum {
        return Err(RuntimeError::UnsupportedVersion(format!(
            "{}.{}.{}",
            version.0, version.1, version.2
        )));
    }
    Ok(())
}

fn find_version(value: &str) -> Option<(u64, u64, u64)> {
    value
        .split_whitespace()
        .find_map(|word| parse_version(word.trim_start_matches('v')))
}

fn parse_version(value: &str) -> Option<(u64, u64, u64)> {
    let value = value.split(['-', '+']).next()?;
    let mut parts = value.split('.');
    let version = (
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
    );
    parts.next().is_none().then_some(version)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    use tempfile::tempdir;

    use super::{find_version, resolve_codex, verify_codex_version, RuntimeError};

    #[test]
    fn parses_tested_cli_version_output() {
        assert_eq!(find_version("codex-cli 0.144.6\n"), Some((0, 144, 6)));
        assert_eq!(find_version("codex-cli v0.145.0-beta"), Some((0, 145, 0)));
        assert!(find_version("codex-cli 0.144.5").unwrap() < (0, 144, 6));
        assert_eq!(find_version("not-a-version"), None);
    }

    #[test]
    fn explicit_executable_path_must_exist_and_be_executable() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("codex");
        fs::write(&path, "#!/bin/sh\n").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(
            resolve_codex(Some(path.as_os_str())).unwrap(),
            fs::canonicalize(&path).unwrap()
        );
        assert!(matches!(
            resolve_codex(Some(temp.path().join("missing").as_os_str())),
            Err(RuntimeError::CodexNotFound)
        ));
    }

    #[test]
    fn relative_executable_path_is_made_absolute_before_the_child_changes_cwd() {
        let temp = tempfile::tempdir_in("target").unwrap();
        let path = temp.path().join("codex");
        fs::write(&path, "#!/bin/sh\n").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        let resolved = resolve_codex(Some(path.as_os_str())).unwrap();
        assert!(resolved.is_absolute());
        assert_eq!(resolved, fs::canonicalize(path).unwrap());
    }

    #[tokio::test]
    async fn version_probe_uses_the_dedicated_codex_home() {
        let temp = tempdir().unwrap();
        let executable = temp.path().join("codex");
        fs::write(
            &executable,
            "#!/bin/sh\nprintf '%s' \"$CODEX_HOME\" > \"$CODEX_HOME/probed\"\nprintf 'codex-cli 0.144.6\\n'\n",
        )
        .unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
        let codex_home = temp.path().join("dedicated");
        fs::create_dir(&codex_home).unwrap();
        verify_codex_version(&executable, &codex_home)
            .await
            .unwrap();
        assert_eq!(
            fs::read_to_string(codex_home.join("probed")).unwrap(),
            codex_home.to_string_lossy()
        );
    }
}
