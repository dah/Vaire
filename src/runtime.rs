use std::ffi::{OsStr, OsString};
use std::fs;
use std::future::Future;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use thiserror::Error;
use tokio::io::AsyncReadExt;
use tokio::process::{Child, ChildStdout, Command};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use tokio::time;

use crate::app::{Action, AppState, AuthState, ConnectionState, Intent};
use crate::backend::BackendCoordinator;
use crate::codex::safety::{FullAccessPolicy, IsolationPaths};
use crate::codex::session::{SessionError, SessionEvent, SessionService};
use crate::codex::transport::{AppServerTransport, ProcessSpec};
use crate::diagnostics::{DiagnosticSink, FileDiagnosticSink};
use crate::persistence::FilePreferences;
use crate::platform::{AppPaths, MacOsBrowser};

pub const TESTED_CODEX_VERSION: &str = "0.144.6";
const SHUTDOWN_BOUND: Duration = Duration::from_secs(15);
const MAX_VERSION_OUTPUT_BYTES: usize = 64 * 1024;

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

enum RuntimeWork {
    Shutdown,
    Intent(Option<Intent>),
    Event(Option<Result<SessionEvent, SessionError>>),
}

enum EventCompletion {
    Shutdown,
    Finished(Result<bool, crate::backend::BackendError>),
}

async fn next_open_work<Event>(
    shutdowns: &mut mpsc::Receiver<()>,
    intents: &mut mpsc::Receiver<Intent>,
    event: Event,
    prefer_event: bool,
) -> RuntimeWork
where
    Event: Future<Output = Option<Result<SessionEvent, SessionError>>>,
{
    if prefer_event {
        tokio::select! {
            biased;
            _ = shutdowns.recv() => RuntimeWork::Shutdown,
            event = event => RuntimeWork::Event(event),
            intent = intents.recv() => RuntimeWork::Intent(intent),
        }
    } else {
        tokio::select! {
            biased;
            _ = shutdowns.recv() => RuntimeWork::Shutdown,
            intent = intents.recv() => RuntimeWork::Intent(intent),
            event = event => RuntimeWork::Event(event),
        }
    }
}

async fn finish_event_or_shutdown<Process>(
    shutdowns: &mut mpsc::Receiver<()>,
    process: Process,
) -> EventCompletion
where
    Process: Future<Output = Result<bool, crate::backend::BackendError>>,
{
    tokio::select! {
        biased;
        _ = shutdowns.recv() => EventCompletion::Shutdown,
        result = process => EventCompletion::Finished(result),
    }
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
    let mut prefer_event = true;

    loop {
        let work = if event_open {
            next_open_work(
                &mut shutdowns,
                &mut intents,
                backend.receive_event(),
                prefer_event,
            )
            .await
        } else {
            tokio::select! {
                biased;
                _ = shutdowns.recv() => RuntimeWork::Shutdown,
                intent = intents.recv() => RuntimeWork::Intent(intent),
            }
        };

        match work {
            RuntimeWork::Shutdown => {
                shutdown_backend(&mut backend, &states).await;
                break;
            }
            RuntimeWork::Intent(Some(intent)) => {
                prefer_event = true;
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
            RuntimeWork::Intent(None) => {
                let effects = backend.accept_intent(Intent::Quit);
                let _ = backend.execute_pending(effects).await;
                publish(&states, backend.state());
                break;
            }
            RuntimeWork::Event(event) => {
                prefer_event = false;
                match finish_event_or_shutdown(
                    &mut shutdowns,
                    backend.process_received_event(event),
                )
                .await
                {
                    EventCompletion::Shutdown => {
                        shutdown_backend(&mut backend, &states).await;
                        break;
                    }
                    EventCompletion::Finished(Ok(open)) => event_open = open,
                    EventCompletion::Finished(Err(error)) => {
                        backend.record_error(error.to_string());
                        event_open = false;
                    }
                }
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
    let spec = ProcessSpec::codex(executable, &isolation, &FullAccessPolicy);
    let transport = AppServerTransport::spawn_with_diagnostics(spec, diagnostics)
        .await
        .map_err(|error| RuntimeError::AppServer(error.to_string()))?;
    let session = SessionService::new(transport, isolation, FullAccessPolicy);
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
    verify_codex_version_with_timeout(executable, codex_home, Duration::from_secs(3)).await
}

async fn verify_codex_version_with_timeout(
    executable: &Path,
    codex_home: &Path,
    timeout: Duration,
) -> Result<(), RuntimeError> {
    let mut command = Command::new(executable);
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("CODEX_") {
            command.env_remove(key);
        }
    }
    command
        .kill_on_drop(true)
        .env("CODEX_HOME", codex_home)
        .arg("--version")
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = command
        .spawn()
        .map_err(|error| RuntimeError::VersionCheck(error.to_string()))?;
    let stdout = child.stdout.take().ok_or_else(|| {
        RuntimeError::VersionCheck("version command stdout was unavailable".to_owned())
    })?;
    let (status, stdout) = collect_version_output(child, stdout, timeout).await?;
    if !status.success() {
        return Err(RuntimeError::VersionCheck(format!(
            "version command exited with {}",
            status
        )));
    }
    let stdout = String::from_utf8_lossy(&stdout);
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

async fn collect_version_output(
    mut child: Child,
    mut stdout: ChildStdout,
    timeout: Duration,
) -> Result<(std::process::ExitStatus, Vec<u8>), RuntimeError> {
    let probe = async {
        let mut bytes = Vec::new();
        (&mut stdout)
            .take((MAX_VERSION_OUTPUT_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .await
            .map_err(|error| RuntimeError::VersionCheck(error.to_string()))?;
        if bytes.len() > MAX_VERSION_OUTPUT_BYTES {
            return Err(RuntimeError::VersionCheck(
                "version command output exceeded safe limit".to_owned(),
            ));
        }
        let status = child
            .wait()
            .await
            .map_err(|error| RuntimeError::VersionCheck(error.to_string()))?;
        Ok((status, bytes))
    };
    let outcome = time::timeout(timeout, probe).await;
    match outcome {
        Ok(Ok(output)) => Ok(output),
        Ok(Err(error)) => {
            terminate_version_child(&mut child).await;
            Err(error)
        }
        Err(_) => {
            terminate_version_child(&mut child).await;
            Err(RuntimeError::VersionCheck(
                "version command timed out".to_owned(),
            ))
        }
    }
}

async fn terminate_version_child(child: &mut Child) {
    if child.try_wait().ok().flatten().is_none() {
        let _ = child.start_kill();
    }
    let _ = time::timeout(Duration::from_secs(1), child.wait()).await;
}

fn find_version(value: &str) -> Option<(u64, u64, u64)> {
    let mut words = value.split_whitespace();
    while let Some(word) = words.next() {
        if word == "codex-cli" {
            return words
                .next()
                .and_then(|version| parse_version(version.trim_start_matches('v')));
        }
    }
    None
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

    use super::{
        collect_version_output, find_version, finish_event_or_shutdown, next_open_work,
        resolve_codex, verify_codex_version, verify_codex_version_with_timeout, EventCompletion,
        RuntimeError, RuntimeWork,
    };
    use crate::app::Intent;
    use crate::codex::session::SessionEvent;

    #[test]
    fn parses_tested_cli_version_output() {
        assert_eq!(find_version("codex-cli 0.144.6\n"), Some((0, 144, 6)));
        assert_eq!(find_version("codex-cli v0.145.0-beta"), Some((0, 145, 0)));
        assert!(find_version("codex-cli 0.144.5").unwrap() < (0, 144, 6));
        assert_eq!(
            find_version("dependency 999.0.0\ncodex-cli 0.144.5\n"),
            Some((0, 144, 5))
        );
        assert_eq!(find_version("999.0.0"), None);
        assert_eq!(find_version("not-a-version"), None);
    }

    #[tokio::test]
    async fn ready_intents_and_events_follow_rotating_priority() {
        let (_shutdown_tx, mut shutdowns) = tokio::sync::mpsc::channel(1);
        let (intent_tx, mut intents) = tokio::sync::mpsc::channel(2);
        intent_tx.send(Intent::Help).await.unwrap();

        let event_first = next_open_work(
            &mut shutdowns,
            &mut intents,
            std::future::ready(Some(Ok(SessionEvent::UnknownNotification(
                "ready-event".to_owned(),
            )))),
            true,
        )
        .await;
        assert!(matches!(
            event_first,
            RuntimeWork::Event(Some(Ok(SessionEvent::UnknownNotification(method))))
                if method == "ready-event"
        ));

        let intent_next = next_open_work(
            &mut shutdowns,
            &mut intents,
            std::future::ready(Some(Ok(SessionEvent::UnknownNotification(
                "second-event".to_owned(),
            )))),
            false,
        )
        .await;
        assert!(matches!(
            intent_next,
            RuntimeWork::Intent(Some(Intent::Help))
        ));
    }

    #[tokio::test]
    async fn queued_intent_cannot_cancel_processing_of_an_already_received_event() {
        let (_shutdown_tx, mut shutdowns) = tokio::sync::mpsc::channel(1);
        let (intent_tx, mut intents) = tokio::sync::mpsc::channel(1);
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();

        // This delayed processor models account/updated waiting for its follow-up account/read.
        // User input becomes ready only after the event has already been consumed.
        let process = async move {
            let _ = started_tx.send(());
            let _ = release_rx.await;
            Ok(true)
        };
        let queue_intent = async move {
            let _ = started_rx.await;
            intent_tx.send(Intent::Help).await.unwrap();
            let _ = release_tx.send(());
        };

        let (completion, ()) = tokio::join!(
            finish_event_or_shutdown(&mut shutdowns, process),
            queue_intent
        );
        assert!(matches!(completion, EventCompletion::Finished(Ok(true))));

        let next =
            next_open_work(&mut shutdowns, &mut intents, std::future::pending(), false).await;
        assert!(matches!(next, RuntimeWork::Intent(Some(Intent::Help))));
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

    #[tokio::test]
    async fn timed_out_version_probe_kills_and_reaps_an_already_started_child() {
        let temp = tempdir().unwrap();
        let executable = temp.path().join("codex");
        fs::write(&executable, "#!/bin/sh\nexec sleep 10\n").unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
        let mut command = tokio::process::Command::new(&executable);
        command
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null());
        let mut child = command.spawn().unwrap();
        let pid = child.id().expect("spawned child has a process id");
        let stdout = child.stdout.take().unwrap();

        let error = collect_version_output(child, stdout, std::time::Duration::from_millis(20))
            .await
            .unwrap_err();
        assert!(matches!(error, RuntimeError::VersionCheck(_)));
        let status = std::process::Command::new("/bin/kill")
            .args(["-0", &pid.to_string()])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap();
        assert!(!status.success(), "timed-out version child still exists");
    }

    #[tokio::test]
    async fn version_probe_rejects_resource_exhausting_output() {
        let temp = tempdir().unwrap();
        let executable = temp.path().join("codex");
        fs::write(
            &executable,
            "#!/bin/sh\nhead -c 131072 /dev/zero\nprintf 'codex-cli 0.144.6\\n'\n",
        )
        .unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
        let codex_home = temp.path().join("dedicated");
        fs::create_dir(&codex_home).unwrap();

        let error = verify_codex_version_with_timeout(
            &executable,
            &codex_home,
            std::time::Duration::from_secs(1),
        )
        .await
        .unwrap_err();

        assert!(matches!(
            error,
            RuntimeError::VersionCheck(message) if message.contains("exceeded safe limit")
        ));
    }
}
