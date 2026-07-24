use std::process::{ExitStatus, Stdio};
use std::time::Duration;

use thiserror::Error;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdout, Command};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio::time;
use tokio_util::sync::CancellationToken;

use crate::credentials::SecretValue;

use super::config::apply_claude_environment;
use super::protocol::{ClaudeProtocolError, MAX_STDERR_BYTES, MAX_STREAM_LINE_BYTES};
use super::{ClaudeCliPolicy, ClaudeInvocation, ClaudeStreamEvent, ClaudeStreamParser};

const MAX_PROMPT_BYTES: usize = 128 * 1024;
const SIGNAL_GRACE: Duration = Duration::from_millis(300);
const STDIN_WRITE_TIMEOUT: Duration = Duration::from_secs(5);
const FINAL_EXIT_TIMEOUT: Duration = Duration::from_secs(2);
const STDERR_DRAIN_TIMEOUT: Duration = Duration::from_secs(1);
const KILL_REAP_TIMEOUT: Duration = Duration::from_secs(1);

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum ClaudeProcessError {
    #[error("Claude process could not be started")]
    Spawn,
    #[error("Claude process input failed")]
    Stdin,
    #[error("Claude process output failed")]
    Stdout,
    #[error("Claude process stderr exceeded its limit")]
    StderrLimit,
    #[error("Claude stream protocol failed")]
    Protocol(ClaudeProtocolError),
    #[error("Claude process exited unsuccessfully")]
    NonZeroExit,
    #[error("Claude process could not be reaped")]
    Reap,
    #[error("Claude process was interrupted")]
    Interrupted,
    #[error("Claude process was interrupted after it started")]
    InterruptedAfterSpawn,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CleanupMode {
    GracefulEscalation,
    LeaderAlreadyExited,
    IdentityUncertainNoSignal,
}

#[derive(Debug)]
struct ProcessGroupAnchor {
    id: i32,
    signaling_allowed: bool,
    #[cfg(test)]
    event_id: u64,
}

impl ProcessGroupAnchor {
    fn new(id: i32) -> Self {
        Self {
            id,
            signaling_allowed: true,
            #[cfg(test)]
            event_id: next_process_event_id(),
        }
    }

    fn signal(&self, signal: i32) -> Result<bool, ClaudeProcessError> {
        if !self.signaling_allowed {
            return Err(ClaudeProcessError::Reap);
        }
        #[cfg(test)]
        record_process_event(self.event_id, ProcessLifecycleEvent::Signal(signal));
        signal_process_group(self.id, signal)
    }

    fn begin_reap(&mut self) {
        self.signaling_allowed = false;
        #[cfg(test)]
        record_process_event(self.event_id, ProcessLifecycleEvent::ReapStarted);
    }
}

#[derive(Debug)]
struct UnreapedGroup {
    child: Child,
    anchor: ProcessGroupAnchor,
}

impl UnreapedGroup {
    fn new(child: Child, process_group: i32) -> Self {
        Self {
            child,
            anchor: ProcessGroupAnchor::new(process_group),
        }
    }

    fn process_group(&self) -> i32 {
        self.anchor.id
    }
}

pub struct ClaudeChild {
    process: UnreapedGroup,
    stdout: BufReader<ChildStdout>,
    stderr: Option<JoinHandle<Result<(), ClaudeProcessError>>>,
    stderr_limit: Option<oneshot::Receiver<()>>,
    parser: ClaudeStreamParser,
    stdout_eof: bool,
}

impl ClaudeChild {
    pub async fn spawn(
        policy: &ClaudeCliPolicy,
        invocation: &ClaudeInvocation,
        expected_session: crate::provider::ClaudeSessionId,
        prompt: &str,
        key: SecretValue,
        cancellation: &CancellationToken,
    ) -> Result<Self, ClaudeProcessError> {
        if prompt.is_empty() || prompt.len() > MAX_PROMPT_BYTES {
            return Err(ClaudeProcessError::Stdin);
        }
        if cancellation.is_cancelled() {
            return Err(ClaudeProcessError::Interrupted);
        }
        let args = policy.args(invocation);
        let mut command = Command::new(policy.executable());
        apply_claude_environment(&mut command, policy.home(), Some(&key));
        command
            .args(args)
            .current_dir(policy.cwd())
            .kill_on_drop(true)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        // SAFETY: this closure performs only async-signal-safe setpgid before exec and owns no
        // borrowed child-side memory.
        unsafe {
            command.pre_exec(|| {
                if libc::setpgid(0, 0) == 0 {
                    Ok(())
                } else {
                    Err(std::io::Error::last_os_error())
                }
            });
        }
        let child = command.spawn().map_err(|_| ClaudeProcessError::Spawn)?;
        let process_group = child.id().map_or(0, |id| id as i32);
        let mut process = UnreapedGroup::new(child, process_group);
        if process_group <= 0 {
            cleanup_child(process, None, CleanupMode::GracefulEscalation, None).await?;
            return Err(ClaudeProcessError::Spawn);
        }
        let mut stdin = match process.child.stdin.take() {
            Some(stdin) => stdin,
            None => {
                cleanup_child(process, None, CleanupMode::GracefulEscalation, None).await?;
                return Err(ClaudeProcessError::Stdin);
            }
        };
        let stdout = match process.child.stdout.take() {
            Some(stdout) => stdout,
            None => {
                drop(stdin);
                cleanup_child(process, None, CleanupMode::GracefulEscalation, None).await?;
                return Err(ClaudeProcessError::Stdout);
            }
        };
        let mut stderr_pipe = match process.child.stderr.take() {
            Some(stderr) => stderr,
            None => {
                drop(stdin);
                drop(stdout);
                cleanup_child(process, None, CleanupMode::GracefulEscalation, None).await?;
                return Err(ClaudeProcessError::Stdout);
            }
        };
        let (stderr_limit_tx, stderr_limit_rx) = oneshot::channel();
        let stderr = tokio::spawn(async move {
            let mut total = 0usize;
            let mut buffer = [0u8; 8192];
            loop {
                let read = stderr_pipe
                    .read(&mut buffer)
                    .await
                    .map_err(|_| ClaudeProcessError::Stdout)?;
                if read == 0 {
                    return Ok(());
                }
                total = total.saturating_add(read);
                if total > MAX_STDERR_BYTES {
                    let _ = stderr_limit_tx.send(());
                    return Err(ClaudeProcessError::StderrLimit);
                }
            }
        });

        let write_result = tokio::select! {
            biased;
            _ = cancellation.cancelled() => Err(ClaudeProcessError::InterruptedAfterSpawn),
            result = time::timeout(STDIN_WRITE_TIMEOUT, async {
                stdin
                    .write_all(prompt.as_bytes())
                    .await
                    .map_err(|_| ClaudeProcessError::Stdin)?;
                stdin
                    .write_all(b"\n")
                    .await
                    .map_err(|_| ClaudeProcessError::Stdin)?;
                stdin
                    .shutdown()
                    .await
                    .map_err(|_| ClaudeProcessError::Stdin)
            }) => match result {
                Ok(result) => result,
                Err(_) => Err(ClaudeProcessError::Stdin),
            },
        };
        drop(stdin);
        if let Err(error) = write_result {
            drop(stdout);
            cleanup_child(process, Some(stderr), CleanupMode::GracefulEscalation, None).await?;
            return Err(error);
        }
        Ok(Self {
            process,
            stdout: BufReader::new(stdout),
            stderr: Some(stderr),
            stderr_limit: Some(stderr_limit_rx),
            parser: ClaudeStreamParser::new(expected_session),
            stdout_eof: false,
        })
    }

    pub fn assistant_text(&self) -> &str {
        self.parser.assistant_text()
    }

    pub async fn next_event(&mut self) -> Result<Option<ClaudeStreamEvent>, ClaudeProcessError> {
        loop {
            if self.stdout_eof {
                return Ok(None);
            }
            let line = if let Some(limit) = &mut self.stderr_limit {
                tokio::select! {
                    line = read_line_limited(&mut self.stdout) => line?,
                    notification = limit => {
                        self.stderr_limit = None;
                        if notification.is_ok() {
                            return Err(ClaudeProcessError::StderrLimit);
                        }
                        continue;
                    }
                }
            } else {
                read_line_limited(&mut self.stdout).await?
            };
            if line.is_empty() {
                self.stdout_eof = true;
                self.parser
                    .finish_eof()
                    .map_err(ClaudeProcessError::Protocol)?;
                return Ok(None);
            }
            if let Some(event) = self
                .parser
                .parse_line(&line)
                .map_err(ClaudeProcessError::Protocol)?
            {
                return Ok(Some(event));
            }
        }
    }

    pub async fn finish(
        mut self,
        cancellation: &CancellationToken,
    ) -> Result<ExitStatus, ClaudeProcessError> {
        if !self.stdout_eof {
            loop {
                let event = tokio::select! {
                    biased;
                    _ = cancellation.cancelled() => {
                        let cleanup = self.terminate_and_drain_stderr(None).await;
                        return Err(cleanup.err().unwrap_or(ClaudeProcessError::Interrupted));
                    }
                    event = self.next_event() => event,
                };
                match event {
                    Ok(Some(_)) => {}
                    Ok(None) => break,
                    Err(error) => {
                        let cleanup = self.terminate_and_drain_stderr(None).await;
                        return Err(cleanup.err().unwrap_or(error));
                    }
                }
            }
        }

        if let Err(failure) =
            observe_leader_exit_without_reaping(&self.process.child, cancellation).await
        {
            let mode = if failure.identity_pinned {
                CleanupMode::GracefulEscalation
            } else {
                CleanupMode::IdentityUncertainNoSignal
            };
            let stderr = self.stderr.take();
            let cleanup = cleanup_child(self.process, stderr, mode, None).await;
            return Err(cleanup.err().unwrap_or(failure.error));
        }
        let stderr = self.stderr.take();
        let status = cleanup_child(
            self.process,
            stderr,
            CleanupMode::LeaderAlreadyExited,
            Some(cancellation),
        )
        .await?;
        if status.success() {
            Ok(status)
        } else {
            Err(ClaudeProcessError::NonZeroExit)
        }
    }

    pub async fn interrupt(self) -> Result<(), ClaudeProcessError> {
        self.terminate_and_drain_stderr(None).await.map(|_| ())
    }

    async fn terminate_and_drain_stderr(
        mut self,
        cancellation: Option<&CancellationToken>,
    ) -> Result<ExitStatus, ClaudeProcessError> {
        let stderr = self.stderr.take();
        cleanup_child(
            self.process,
            stderr,
            CleanupMode::GracefulEscalation,
            cancellation,
        )
        .await
    }
}

async fn read_line_limited(
    reader: &mut BufReader<ChildStdout>,
) -> Result<Vec<u8>, ClaudeProcessError> {
    let mut line = Vec::new();
    loop {
        let available = reader
            .fill_buf()
            .await
            .map_err(|_| ClaudeProcessError::Stdout)?;
        if available.is_empty() {
            return Ok(line);
        }
        let consumed = available
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(available.len(), |position| position + 1);
        if line.len().saturating_add(consumed) > MAX_STREAM_LINE_BYTES {
            return Err(ClaudeProcessError::Protocol(
                ClaudeProtocolError::ResourceLimit,
            ));
        }
        line.extend_from_slice(&available[..consumed]);
        reader.consume(consumed);
        if line.last() == Some(&b'\n') {
            return Ok(line);
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct ExitObservationFailure {
    error: ClaudeProcessError,
    identity_pinned: bool,
}

async fn observe_leader_exit_without_reaping(
    child: &Child,
    cancellation: &CancellationToken,
) -> Result<(), ExitObservationFailure> {
    let pid = child
        .id()
        .map(|id| id as i32)
        .ok_or(ExitObservationFailure {
            error: ClaudeProcessError::Reap,
            identity_pinned: false,
        })?;
    let deadline = time::Instant::now() + FINAL_EXIT_TIMEOUT;
    loop {
        if cancellation.is_cancelled() {
            return Err(ExitObservationFailure {
                error: ClaudeProcessError::Interrupted,
                identity_pinned: true,
            });
        }
        match leader_exit_ready(pid) {
            Ok(true) => return Ok(()),
            Ok(false) => {}
            Err(error) => {
                return Err(ExitObservationFailure {
                    error,
                    identity_pinned: false,
                });
            }
        }
        let now = time::Instant::now();
        if now >= deadline {
            return Err(ExitObservationFailure {
                error: ClaudeProcessError::Reap,
                identity_pinned: true,
            });
        }
        tokio::select! {
            biased;
            _ = cancellation.cancelled() => {
                return Err(ExitObservationFailure {
                    error: ClaudeProcessError::Interrupted,
                    identity_pinned: true,
                });
            }
            _ = time::sleep((deadline - now).min(Duration::from_millis(10))) => {}
        }
    }
}

fn leader_exit_ready(pid: i32) -> Result<bool, ClaudeProcessError> {
    let mut info = std::mem::MaybeUninit::<libc::siginfo_t>::zeroed();
    // SAFETY: waitid writes siginfo_t for this exact child PID. WNOWAIT observes exit without
    // reaping, preserving the leader as the process-group identity anchor.
    let result = unsafe {
        libc::waitid(
            libc::P_PID,
            pid as libc::id_t,
            info.as_mut_ptr(),
            libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
        )
    };
    if result != 0 {
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::EINTR) {
            return Ok(false);
        }
        return Err(ClaudeProcessError::Reap);
    }
    // SAFETY: successful waitid initializes the siginfo_t output structure.
    let info = unsafe { info.assume_init() };
    if info.si_pid == 0 {
        Ok(false)
    } else if info.si_pid == pid {
        Ok(true)
    } else {
        Err(ClaudeProcessError::Reap)
    }
}

async fn cleanup_child(
    mut process: UnreapedGroup,
    mut stderr: Option<JoinHandle<Result<(), ClaudeProcessError>>>,
    mode: CleanupMode,
    cancellation: Option<&CancellationToken>,
) -> Result<ExitStatus, ClaudeProcessError> {
    let signal_result = signal_for_cleanup(&mut process, mode).await;
    let stderr_result = join_stderr_task(&mut stderr, cancellation).await;
    let reap_result = reap_leader(process).await;
    signal_result?;
    stderr_result?;
    reap_result
}

async fn signal_for_cleanup(
    process: &mut UnreapedGroup,
    mode: CleanupMode,
) -> Result<(), ClaudeProcessError> {
    if mode == CleanupMode::IdentityUncertainNoSignal {
        return Ok(());
    }
    if process.process_group() <= 0 {
        return process
            .child
            .start_kill()
            .map_err(|_| ClaudeProcessError::Reap);
    }
    if mode == CleanupMode::LeaderAlreadyExited {
        process.anchor.signal(libc::SIGKILL)?;
        return Ok(());
    }

    let mut first_error = None;
    let interrupt = capture_signal_result(process.anchor.signal(libc::SIGINT), &mut first_error);
    if interrupt {
        time::sleep(SIGNAL_GRACE).await;
    }
    let terminate = capture_signal_result(process.anchor.signal(libc::SIGTERM), &mut first_error);
    if terminate {
        time::sleep(SIGNAL_GRACE).await;
    }
    let _ = capture_signal_result(process.anchor.signal(libc::SIGKILL), &mut first_error);
    first_error.map_or(Ok(()), Err)
}

fn capture_signal_result(
    result: Result<bool, ClaudeProcessError>,
    first_error: &mut Option<ClaudeProcessError>,
) -> bool {
    match result {
        Ok(signaled) => signaled,
        Err(error) => {
            if first_error.is_none() {
                *first_error = Some(error);
            }
            false
        }
    }
}

async fn join_stderr_task(
    stderr: &mut Option<JoinHandle<Result<(), ClaudeProcessError>>>,
    cancellation: Option<&CancellationToken>,
) -> Result<(), ClaudeProcessError> {
    let Some(mut task) = stderr.take() else {
        return Ok(());
    };
    let joined = if let Some(cancellation) = cancellation {
        tokio::select! {
            biased;
            _ = cancellation.cancelled() => None,
            result = time::timeout(STDERR_DRAIN_TIMEOUT, &mut task) => Some(result),
        }
    } else {
        Some(time::timeout(STDERR_DRAIN_TIMEOUT, &mut task).await)
    };
    match joined {
        Some(Ok(result)) => result.map_err(|_| ClaudeProcessError::Reap)?,
        Some(Err(_)) => {
            task.abort();
            let _ = task.await;
            Err(ClaudeProcessError::Reap)
        }
        None => {
            task.abort();
            let _ = task.await;
            Err(ClaudeProcessError::Interrupted)
        }
    }
}

async fn reap_leader(mut process: UnreapedGroup) -> Result<ExitStatus, ClaudeProcessError> {
    process.anchor.begin_reap();
    match time::timeout(KILL_REAP_TIMEOUT, process.child.wait()).await {
        Ok(Ok(status)) => Ok(status),
        Ok(Err(_)) | Err(_) => Err(ClaudeProcessError::Reap),
    }
}

fn signal_process_group(process_group: i32, signal: i32) -> Result<bool, ClaudeProcessError> {
    // SAFETY: kill accepts a negative process-group identifier and retains no pointer.
    if unsafe { libc::kill(-process_group, signal) } == 0 {
        return Ok(true);
    }
    let error = std::io::Error::last_os_error();
    if matches!(error.raw_os_error(), Some(libc::ESRCH | libc::EPERM)) {
        // Darwin reports EPERM when the pinned process group contains only the unreaped zombie
        // leader. Any live same-user member would be signalable and make the group kill succeed.
        Ok(false)
    } else {
        Err(ClaudeProcessError::Reap)
    }
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ProcessLifecycleEvent {
    Signal(i32),
    ReapStarted,
}

#[cfg(test)]
fn next_process_event_id() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_ID: AtomicU64 = AtomicU64::new(1);
    NEXT_ID.fetch_add(1, Ordering::Relaxed)
}

#[cfg(test)]
fn process_event_log() -> &'static std::sync::Mutex<Vec<(u64, ProcessLifecycleEvent)>> {
    static EVENTS: std::sync::OnceLock<std::sync::Mutex<Vec<(u64, ProcessLifecycleEvent)>>> =
        std::sync::OnceLock::new();
    EVENTS.get_or_init(|| std::sync::Mutex::new(Vec::new()))
}

#[cfg(test)]
fn record_process_event(event_id: u64, event: ProcessLifecycleEvent) {
    process_event_log().lock().unwrap().push((event_id, event));
}

#[cfg(test)]
fn recorded_process_events(event_id: u64) -> Vec<ProcessLifecycleEvent> {
    process_event_log()
        .lock()
        .unwrap()
        .iter()
        .filter_map(|(recorded_id, event)| (*recorded_id == event_id).then_some(*event))
        .collect()
}

#[cfg(test)]
pub(crate) fn argv_strings(policy: &ClaudeCliPolicy, invocation: &ClaudeInvocation) -> Vec<String> {
    policy
        .args(invocation)
        .into_iter()
        .map(|value: std::ffi::OsString| value.to_string_lossy().into_owned())
        .collect()
}

#[cfg(test)]
mod process_boundary_tests {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};

    use tempfile::TempDir;

    use super::*;
    use crate::provider::ClaudeSessionId;

    const TEST_SESSION_ID: &str = "00000000-0000-4000-8000-000000000099";
    const INIT_LINE: &str = "{\"type\":\"system\",\"subtype\":\"init\",\"session_id\":\"00000000-0000-4000-8000-000000000099\",\"model\":\"claude-test\"}";
    const RESULT_LINE: &str = "{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"session_id\":\"00000000-0000-4000-8000-000000000099\",\"result\":\"done\"}";

    fn write_cli(root: &Path, body: &str) -> PathBuf {
        let script = root.join("fake-claude");
        fs::write(&script, body).unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).unwrap();
        script
    }

    fn test_policy(script: PathBuf, root: &Path) -> ClaudeCliPolicy {
        let home = root.join("home");
        let cwd = root.join("cwd");
        fs::create_dir(&home).unwrap();
        fs::create_dir(&cwd).unwrap();
        ClaudeCliPolicy::new(script, home, cwd)
    }

    fn session() -> ClaudeSessionId {
        TEST_SESSION_ID.parse().unwrap()
    }

    fn invocation() -> ClaudeInvocation {
        ClaudeInvocation::NewSession {
            session_id: session(),
            model: super::super::ClaudeModelAlias::Sonnet,
        }
    }

    fn key() -> SecretValue {
        SecretValue::from_input("test-console-key").unwrap()
    }

    async fn wait_for_pid_gone(pid: i32) -> bool {
        time::timeout(Duration::from_secs(1), async {
            loop {
                // SAFETY: signal zero probes only the PID recorded by the fake CLI.
                if unsafe { libc::kill(pid, 0) } == -1
                    && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
                {
                    break;
                }
                time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .is_ok()
    }

    fn kill_test_pid(pid: i32) {
        // SAFETY: test-only fallback targets the exact PID recorded by the fake CLI, never a
        // numeric process group whose leader may already have been reaped.
        let _ = unsafe { libc::kill(pid, libc::SIGKILL) };
    }

    #[tokio::test]
    async fn cancellation_during_stdin_delivery_is_post_spawn() {
        let root = TempDir::new().unwrap();
        let script = write_cli(root.path(), "#!/bin/sh\n: > \"$0.started\"\nsleep 30\n");
        let started = root.path().join("fake-claude.started");
        let policy = test_policy(script, root.path());
        let cancellation = CancellationToken::new();
        let cancellation_driver = cancellation.clone();
        let driver = tokio::spawn(async move {
            time::timeout(Duration::from_secs(2), async {
                while !started.exists() {
                    time::sleep(Duration::from_millis(5)).await;
                }
            })
            .await
            .expect("fake CLI must start before cancellation");
            cancellation_driver.cancel();
        });
        let prompt = "x".repeat(MAX_PROMPT_BYTES);
        let result = time::timeout(
            Duration::from_secs(4),
            ClaudeChild::spawn(
                &policy,
                &invocation(),
                session(),
                &prompt,
                key(),
                &cancellation,
            ),
        )
        .await
        .expect("post-spawn cancellation must clean up promptly");
        driver.await.unwrap();
        assert_eq!(
            result.err(),
            Some(ClaudeProcessError::InterruptedAfterSpawn)
        );
    }

    #[tokio::test]
    async fn stderr_reader_stops_immediately_after_the_limit() {
        let root = TempDir::new().unwrap();
        let script = write_cli(
            root.path(),
            &format!(
                "#!/bin/sh\nprintf '%s\\n' '{}'\ndd if=/dev/zero bs=1048577 count=1 1>&2 2>/dev/null\nsleep 30\n",
                INIT_LINE
            ),
        );
        let policy = test_policy(script, root.path());
        let cancellation = CancellationToken::new();
        let mut child = ClaudeChild::spawn(
            &policy,
            &invocation(),
            session(),
            "hello",
            key(),
            &cancellation,
        )
        .await
        .unwrap();

        let error = time::timeout(Duration::from_secs(3), async {
            loop {
                match child.next_event().await {
                    Ok(Some(_)) => {}
                    Ok(None) => panic!("stream ended before stderr limit"),
                    Err(error) => break error,
                }
            }
        })
        .await
        .expect("stderr limit must surface promptly");
        assert_eq!(error, ClaudeProcessError::StderrLimit);
        time::timeout(Duration::from_millis(250), async {
            while child
                .stderr
                .as_ref()
                .is_some_and(|task| !task.is_finished())
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("stderr reader must return instead of draining unbounded output");
        assert!(matches!(
            child.interrupt().await,
            Err(ClaudeProcessError::StderrLimit)
        ));
    }

    #[tokio::test]
    async fn interrupt_kills_descendants_after_the_leader_exits() {
        let root = TempDir::new().unwrap();
        let script = write_cli(
            root.path(),
            &format!(
                "#!/bin/sh\ntrap 'exit 0' INT TERM\n(\n  trap '' INT TERM\n  exec </dev/null >/dev/null 2>&1\n  while :; do sleep 1; done\n) &\nprintf '%s\\n' \"$!\" > \"$0.descendant-pid\"\nprintf '%s\\n' '{}'\nwhile :; do sleep 1; done\n",
                INIT_LINE
            ),
        );
        let descendant_pid_path = root.path().join("fake-claude.descendant-pid");
        let policy = test_policy(script, root.path());
        let cancellation = CancellationToken::new();
        let mut child = ClaudeChild::spawn(
            &policy,
            &invocation(),
            session(),
            "hello",
            key(),
            &cancellation,
        )
        .await
        .unwrap();
        assert!(matches!(
            child.next_event().await.unwrap(),
            Some(ClaudeStreamEvent::Initialized { .. })
        ));
        let descendant_pid: i32 = fs::read_to_string(descendant_pid_path)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        let event_id = child.process.anchor.event_id;

        time::timeout(Duration::from_secs(4), child.interrupt())
            .await
            .expect("process-group cleanup must be bounded")
            .unwrap();
        let descendant_gone = wait_for_pid_gone(descendant_pid).await;
        if !descendant_gone {
            kill_test_pid(descendant_pid);
        }
        assert!(descendant_gone);
        assert_eq!(
            recorded_process_events(event_id),
            vec![
                ProcessLifecycleEvent::Signal(libc::SIGINT),
                ProcessLifecycleEvent::Signal(libc::SIGTERM),
                ProcessLifecycleEvent::Signal(libc::SIGKILL),
                ProcessLifecycleEvent::ReapStarted,
            ]
        );
    }

    #[tokio::test]
    async fn successful_finish_kills_closed_pipe_descendants() {
        let root = TempDir::new().unwrap();
        let script = write_cli(
            root.path(),
            &format!(
                "#!/bin/sh\n(\n  trap '' HUP INT TERM\n  exec </dev/null >/dev/null 2>&1\n  : > \"$0.descendant-ready\"\n  while :; do sleep 1; done\n) &\ndescendant=$!\nwhile [ ! -f \"$0.descendant-ready\" ]; do :; done\nprintf '%s\\n' \"$descendant\" > \"$0.descendant-pid\"\nprintf '%s\\n' '{}'\nprintf '%s\\n' '{}'\nexit 0\n",
                INIT_LINE, RESULT_LINE
            ),
        );
        let descendant_pid_path = root.path().join("fake-claude.descendant-pid");
        let policy = test_policy(script, root.path());
        let cancellation = CancellationToken::new();
        let mut child = ClaudeChild::spawn(
            &policy,
            &invocation(),
            session(),
            "hello",
            key(),
            &cancellation,
        )
        .await
        .unwrap();
        let mut saw_terminal = false;
        while let Some(event) = child.next_event().await.unwrap() {
            saw_terminal |= matches!(event, ClaudeStreamEvent::Terminal { success: true, .. });
        }
        assert!(saw_terminal);
        let descendant_pid: i32 = fs::read_to_string(descendant_pid_path)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        let event_id = child.process.anchor.event_id;

        let finish_result =
            time::timeout(Duration::from_secs(4), child.finish(&cancellation)).await;
        let status = finish_result
            .expect("successful finish cleanup must be bounded")
            .unwrap();
        assert!(status.success());
        let descendant_gone = wait_for_pid_gone(descendant_pid).await;
        if !descendant_gone {
            kill_test_pid(descendant_pid);
        }
        assert!(descendant_gone);
        let lifecycle = recorded_process_events(event_id);
        assert_eq!(
            lifecycle,
            vec![
                ProcessLifecycleEvent::Signal(libc::SIGKILL),
                ProcessLifecycleEvent::ReapStarted,
            ]
        );
    }
}
