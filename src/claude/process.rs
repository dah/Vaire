#[cfg(target_os = "macos")]
use std::collections::VecDeque;
use std::path::Path;
use std::process::{ExitStatus, Stdio};
use std::time::Duration;

use thiserror::Error;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdout, Command};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio::time;
use tokio_util::sync::CancellationToken;

use super::config::{apply_claude_environment, SUBSCRIPTION_SETTINGS};
use super::protocol::{ClaudeProtocolError, MAX_STDERR_BYTES, MAX_STREAM_LINE_BYTES};
use super::{
    ClaudeAuthAction, ClaudeCliPolicy, ClaudeInvocation, ClaudeRuntimeError, ClaudeStreamEvent,
    ClaudeStreamParser,
};

const MAX_PROMPT_BYTES: usize = 128 * 1024;
const SIGNAL_GRACE: Duration = Duration::from_millis(300);
const STDIN_WRITE_TIMEOUT: Duration = Duration::from_secs(5);
const FINAL_EXIT_TIMEOUT: Duration = Duration::from_secs(2);
const STDERR_DRAIN_TIMEOUT: Duration = Duration::from_secs(1);
const KILL_REAP_TIMEOUT: Duration = Duration::from_secs(1);
const AUTH_TREE_STOP_TIMEOUT: Duration = Duration::from_millis(500);
const AUTH_PROCESS_STOP_TIMEOUT: Duration = Duration::from_millis(50);
const MAX_AUTH_DESCENDANTS: usize = 1_024;

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

pub async fn run_claude_auth_action(
    executable: &Path,
    config_dir: &Path,
    cwd: &Path,
    action: ClaudeAuthAction,
    cancellation: CancellationToken,
) -> Result<(), ClaudeRuntimeError> {
    if cancellation.is_cancelled() {
        return Err(ClaudeRuntimeError::AuthCancelled);
    }
    let mut command = Command::new(executable);
    apply_claude_environment(&mut command, config_dir);
    command
        .kill_on_drop(true)
        .current_dir(cwd)
        .args([
            "--safe-mode",
            "--settings",
            SUBSCRIPTION_SETTINGS,
            "--setting-sources",
            "",
        ])
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    match action {
        ClaudeAuthAction::Login => {
            command.args(["auth", "login", "--claudeai"]);
        }
        ClaudeAuthAction::Logout => {
            command.args(["auth", "logout"]);
        }
    }
    let mut child = command
        .spawn()
        .map_err(|_| ClaudeRuntimeError::AuthAction)?;
    let Some(leader_pid) = child
        .id()
        .and_then(|id| i32::try_from(id).ok())
        .filter(|id| *id > 0)
    else {
        let _ = child.start_kill();
        let _ = child.wait().await;
        return Err(ClaudeRuntimeError::AuthAction);
    };
    tokio::select! {
        biased;
        _ = cancellation.cancelled() => {}
        status = child.wait() => {
            return status
                .map_err(|_| ClaudeRuntimeError::AuthAction)
                .and_then(|status| {
                    status
                        .success()
                        .then_some(())
                        .ok_or(ClaudeRuntimeError::AuthAction)
                })
        }
    }
    match terminate_auth_child(child, leader_pid).await {
        Ok(()) => Err(ClaudeRuntimeError::AuthCancelled),
        Err(error) => Err(error),
    }
}

async fn terminate_auth_child(child: Child, leader_pid: i32) -> Result<(), ClaudeRuntimeError> {
    terminate_auth_child_with_limit(child, leader_pid, MAX_AUTH_DESCENDANTS).await
}

#[cfg(target_os = "macos")]
async fn terminate_auth_child_with_limit(
    mut child: Child,
    leader_pid: i32,
    descendant_limit: usize,
) -> Result<(), ClaudeRuntimeError> {
    let mut descendants = Vec::new();
    let mut cleanup_failed = false;
    let stop_deadline = std::time::Instant::now() + AUTH_TREE_STOP_TIMEOUT;
    // SAFETY: getpid has no preconditions and returns the current Vairë process identity.
    let leader =
        match stop_and_pin_auth_process(leader_pid, unsafe { libc::getpid() }, stop_deadline) {
            Ok(Some(leader)) => Some(leader),
            Ok(None) => {
                cleanup_failed = true;
                None
            }
            Err(ClaudeRuntimeError::AuthAction) => {
                cleanup_failed = true;
                None
            }
            Err(error) => return Err(error),
        };
    if let Some(leader) = leader {
        if collect_stopped_auth_descendants(
            leader,
            &mut descendants,
            descendant_limit,
            stop_deadline,
        )
        .is_err()
        {
            cleanup_failed = true;
        }
        for identity in descendants.iter().rev().copied() {
            if kill_pinned_auth_process(identity).is_err() {
                cleanup_failed = true;
            }
        }
        if kill_pinned_auth_process(leader).is_err() {
            cleanup_failed = true;
        }
    }
    if cleanup_failed {
        let _ = child.start_kill();
    }
    let wait_result = reap_auth_child(child).await;
    if cleanup_failed || wait_result.is_err() {
        Err(ClaudeRuntimeError::AuthAction)
    } else {
        Ok(())
    }
}

async fn reap_auth_child(mut child: Child) -> Result<(), ClaudeRuntimeError> {
    match time::timeout(KILL_REAP_TIMEOUT, child.wait()).await {
        Ok(Ok(_)) => Ok(()),
        Ok(Err(_)) | Err(_) => {
            let _ = child.start_kill();
            // SIGKILL cannot be ignored. Finish waitpid ownership here rather than detaching a
            // reaper that Tokio shutdown could abort and turn into an unreaped zombie.
            let _ = child.wait().await;
            Err(ClaudeRuntimeError::AuthAction)
        }
    }
}

#[cfg(not(target_os = "macos"))]
async fn terminate_auth_child_with_limit(
    mut child: Child,
    leader_pid: i32,
    _descendant_limit: usize,
) -> Result<(), ClaudeRuntimeError> {
    let stop_result = signal_auth_process(leader_pid, libc::SIGSTOP);
    let signal_result = signal_auth_process(leader_pid, libc::SIGKILL);
    if stop_result.is_err() || signal_result.is_err() {
        let _ = child.start_kill();
    }
    let wait_result = reap_auth_child(child).await;
    if stop_result.is_err() || signal_result.is_err() || wait_result.is_err() {
        Err(ClaudeRuntimeError::AuthAction)
    } else {
        Ok(())
    }
}

fn signal_auth_process(pid: i32, signal: i32) -> Result<bool, ClaudeRuntimeError> {
    // SAFETY: kill targets one positive PID obtained directly from the spawned auth tree.
    if unsafe { libc::kill(pid, signal) } == 0 {
        return Ok(true);
    }
    let error = std::io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ESRCH) {
        Ok(false)
    } else {
        Err(ClaudeRuntimeError::AuthAction)
    }
}

#[cfg(target_os = "macos")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct AuthProcessIdentity {
    pid: i32,
    parent_pid: i32,
    start_seconds: u64,
    start_microseconds: u64,
}

#[cfg(target_os = "macos")]
impl AuthProcessIdentity {
    fn is_same_instance(self, other: Self) -> bool {
        self.pid == other.pid
            && self.start_seconds == other.start_seconds
            && self.start_microseconds == other.start_microseconds
    }
}

#[cfg(target_os = "macos")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct AuthProcessSnapshot {
    identity: AuthProcessIdentity,
    status: u32,
}

#[cfg(target_os = "macos")]
fn read_auth_process_snapshot(pid: i32) -> Result<Option<AuthProcessSnapshot>, ClaudeRuntimeError> {
    let mut info = std::mem::MaybeUninit::<libc::proc_bsdinfo>::zeroed();
    let info_size = i32::try_from(std::mem::size_of::<libc::proc_bsdinfo>())
        .map_err(|_| ClaudeRuntimeError::AuthAction)?;
    // SAFETY: proc_pidinfo writes at most `info_size` bytes to this correctly aligned buffer and
    // retains no pointer. The value is read only after a full-size result.
    let read = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTBSDINFO,
            0,
            info.as_mut_ptr().cast::<libc::c_void>(),
            info_size,
        )
    };
    let read_error = std::io::Error::last_os_error().raw_os_error();
    if read == info_size {
        // SAFETY: an exact full-size result initializes the complete proc_bsdinfo structure.
        let info = unsafe { info.assume_init() };
        let reported_pid =
            i32::try_from(info.pbi_pid).map_err(|_| ClaudeRuntimeError::AuthAction)?;
        if reported_pid != pid {
            return Err(ClaudeRuntimeError::AuthAction);
        }
        return Ok(Some(AuthProcessSnapshot {
            identity: AuthProcessIdentity {
                pid,
                parent_pid: i32::try_from(info.pbi_ppid)
                    .map_err(|_| ClaudeRuntimeError::AuthAction)?,
                start_seconds: info.pbi_start_tvsec,
                start_microseconds: info.pbi_start_tvusec,
            },
            status: info.pbi_status,
        }));
    }

    // Apple's wrapper maps the underlying syscall failure to zero while preserving errno. Capture
    // it immediately: a vanished (including unreaped zombie) PID is absence; every other zero or
    // positive short read is an identity-inspection failure.
    if read == 0 && read_error == Some(libc::ESRCH) {
        Ok(None)
    } else {
        Err(ClaudeRuntimeError::AuthAction)
    }
}

#[cfg(target_os = "macos")]
fn resume_auth_process_if_same_instance(identity: AuthProcessIdentity) {
    let Ok(Some(current)) = read_auth_process_snapshot(identity.pid) else {
        return;
    };
    if current.identity.is_same_instance(identity) && current.status == libc::SSTOP {
        let _ = signal_auth_process(identity.pid, libc::SIGCONT);
    }
}

#[cfg(target_os = "macos")]
fn stop_and_pin_auth_process(
    pid: i32,
    expected_parent: i32,
    cleanup_deadline: std::time::Instant,
) -> Result<Option<AuthProcessIdentity>, ClaudeRuntimeError> {
    if std::time::Instant::now() >= cleanup_deadline {
        return Err(ClaudeRuntimeError::AuthAction);
    }
    let Some(before) = read_auth_process_snapshot(pid)? else {
        return Ok(None);
    };
    if before.identity.parent_pid != expected_parent {
        return Err(ClaudeRuntimeError::AuthAction);
    }
    if before.status == libc::SZOMB {
        return Ok(None);
    }
    if before.status == libc::SSTOP {
        return Ok(Some(before.identity));
    }
    if !signal_auth_process(pid, libc::SIGSTOP)? {
        return Ok(None);
    }

    let deadline = cleanup_deadline.min(std::time::Instant::now() + AUTH_PROCESS_STOP_TIMEOUT);
    loop {
        let current = match read_auth_process_snapshot(pid) {
            Ok(current) => current,
            Err(error) => {
                // The leader is held by Child and a descendant's parent remains stopped, so this
                // PID cannot normally be reused here. Best-effort resume avoids stranding a
                // process if identity inspection itself failed after our SIGSTOP.
                let _ = signal_auth_process(pid, libc::SIGCONT);
                return Err(error);
            }
        };
        match current {
            None => return Ok(None),
            Some(current)
                if !current.identity.is_same_instance(before.identity)
                    || current.identity.parent_pid != expected_parent =>
            {
                // Never kill a changed identity. If our immediately preceding SIGSTOP landed on
                // the newly observed instance, resume that exact stopped snapshot before failing.
                resume_auth_process_if_same_instance(current.identity);
                return Err(ClaudeRuntimeError::AuthAction);
            }
            Some(current) if current.status == libc::SZOMB => return Ok(None),
            Some(current) if current.status == libc::SSTOP => {
                return Ok(Some(current.identity));
            }
            Some(_) => {}
        }
        if std::time::Instant::now() >= deadline {
            resume_auth_process_if_same_instance(before.identity);
            return Err(ClaudeRuntimeError::AuthAction);
        }
        std::thread::yield_now();
    }
}

#[cfg(target_os = "macos")]
fn require_stopped_auth_process(identity: AuthProcessIdentity) -> Result<(), ClaudeRuntimeError> {
    let Some(current) = read_auth_process_snapshot(identity.pid)? else {
        return Err(ClaudeRuntimeError::AuthAction);
    };
    if current.identity == identity && current.status == libc::SSTOP {
        Ok(())
    } else {
        Err(ClaudeRuntimeError::AuthAction)
    }
}

#[cfg(target_os = "macos")]
fn kill_pinned_auth_process(identity: AuthProcessIdentity) -> Result<(), ClaudeRuntimeError> {
    let Some(current) = read_auth_process_snapshot(identity.pid)? else {
        return Ok(());
    };
    if current.identity != identity || current.status != libc::SSTOP {
        if current.identity.is_same_instance(identity) {
            resume_auth_process_if_same_instance(identity);
        }
        return Err(ClaudeRuntimeError::AuthAction);
    }
    signal_auth_process(identity.pid, libc::SIGKILL).map(|_| ())
}

#[cfg(target_os = "macos")]
fn list_stopped_auth_children(
    parent: AuthProcessIdentity,
    allowance: usize,
) -> Result<(Vec<i32>, bool), ClaudeRuntimeError> {
    require_stopped_auth_process(parent)?;
    // libproc's null-buffer sizing result is global process-table headroom, not a filtered child
    // count. Read one PID beyond the remaining retention budget instead: that sentinel proves
    // overflow while every retained slot still receives a concrete PID to clean.
    let capacity = allowance
        .checked_add(1)
        .ok_or(ClaudeRuntimeError::AuthAction)?;
    let mut children = vec![0_i32; capacity];
    let buffer_bytes = capacity
        .checked_mul(std::mem::size_of::<i32>())
        .and_then(|bytes| i32::try_from(bytes).ok())
        .ok_or(ClaudeRuntimeError::AuthAction)?;
    // SAFETY: the buffer is writable for exactly `buffer_bytes`; libproc retains no pointer.
    let filled = unsafe {
        libc::proc_listchildpids(
            parent.pid,
            children.as_mut_ptr().cast::<libc::c_void>(),
            buffer_bytes,
        )
    };
    if filled < 0 {
        return Err(ClaudeRuntimeError::AuthAction);
    }
    let filled = usize::try_from(filled).map_err(|_| ClaudeRuntimeError::AuthAction)?;
    if filled > capacity {
        return Err(ClaudeRuntimeError::AuthAction);
    }
    children.truncate(filled.min(allowance));
    require_stopped_auth_process(parent)?;
    Ok((children, filled > allowance))
}

#[cfg(target_os = "macos")]
fn collect_stopped_auth_descendants(
    root: AuthProcessIdentity,
    descendants: &mut Vec<AuthProcessIdentity>,
    limit: usize,
    stop_deadline: std::time::Instant,
) -> Result<(), ClaudeRuntimeError> {
    let mut parents = VecDeque::from([root]);
    let mut failed = false;
    while let Some(parent) = parents.pop_front() {
        if std::time::Instant::now() >= stop_deadline {
            failed = true;
            break;
        }
        let allowance = limit.saturating_sub(descendants.len());
        let children = match list_stopped_auth_children(parent, allowance) {
            Ok((children, overflow)) => {
                failed |= overflow;
                children
            }
            Err(_) => {
                failed = true;
                continue;
            }
        };

        // Stop every direct sibling that fits before descending into any one subtree. With the
        // parent already stopped this reaches a stable fixed point and minimizes fork/reparent
        // races. Parents are retained before descendants, so reverse-order cleanup remains
        // leaf-first.
        let mut stopped_children = Vec::new();
        for child_pid in children.into_iter().filter(|pid| *pid > 0) {
            if std::time::Instant::now() >= stop_deadline {
                failed = true;
                break;
            }
            if descendants.iter().any(|identity| identity.pid == child_pid) {
                failed = true;
                continue;
            }
            match stop_and_pin_auth_process(child_pid, parent.pid, stop_deadline) {
                Ok(Some(identity)) => {
                    descendants.push(identity);
                    stopped_children.push(identity);
                }
                // An enumerated child that exits before it can be pinned may already have
                // reparented descendants. Preserve the retained set but fail the complete proof.
                Ok(None) | Err(_) => {
                    failed = true;
                }
            }
        }
        parents.extend(stopped_children);
    }
    if failed {
        Err(ClaudeRuntimeError::AuthAction)
    } else {
        Ok(())
    }
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
        apply_claude_environment(&mut command, policy.home());
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
            effort: None,
        }
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

    #[cfg(target_os = "macos")]
    async fn wait_for_auth_instance_terminated(identity: AuthProcessIdentity) -> bool {
        time::timeout(Duration::from_secs(2), async {
            loop {
                match read_auth_process_snapshot(identity.pid) {
                    Ok(None) => break,
                    Ok(Some(current))
                        if !current.identity.is_same_instance(identity)
                            || current.status == libc::SZOMB =>
                    {
                        break;
                    }
                    Ok(Some(_)) | Err(_) => time::sleep(Duration::from_millis(10)).await,
                }
            }
        })
        .await
        .is_ok()
    }

    #[cfg(target_os = "macos")]
    fn kill_test_auth_instance(identity: AuthProcessIdentity) {
        if read_auth_process_snapshot(identity.pid)
            .ok()
            .flatten()
            .is_some_and(|current| current.identity.is_same_instance(identity))
        {
            kill_test_pid(identity.pid);
        }
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
            ClaudeChild::spawn(&policy, &invocation(), session(), &prompt, &cancellation),
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
        let mut child =
            ClaudeChild::spawn(&policy, &invocation(), session(), "hello", &cancellation)
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
        let mut child =
            ClaudeChild::spawn(&policy, &invocation(), session(), "hello", &cancellation)
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
        let mut child =
            ClaudeChild::spawn(&policy, &invocation(), session(), "hello", &cancellation)
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

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn pinned_auth_cleanup_refuses_changed_process_identity() {
        let root = TempDir::new().unwrap();
        let script = write_cli(root.path(), "#!/bin/sh\nexec sleep 30\n");
        let mut child = Command::new(script)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let pid = i32::try_from(child.id().unwrap()).unwrap();
        // SAFETY: getpid has no preconditions and identifies this test process, the direct parent.
        let parent_pid = unsafe { libc::getpid() };
        let stop_deadline = std::time::Instant::now() + AUTH_TREE_STOP_TIMEOUT;
        let identity = stop_and_pin_auth_process(pid, parent_pid, stop_deadline)
            .unwrap()
            .expect("live child must be stopped and pinned");

        let mut wrong_parent = identity;
        wrong_parent.parent_pid = wrong_parent.parent_pid.saturating_add(1);
        let mut wrong_start = identity;
        wrong_start.start_microseconds = wrong_start.start_microseconds.wrapping_add(1);
        let wrong_start_result = kill_pinned_auth_process(wrong_start);
        let still_pinned_after_start_mismatch = read_auth_process_snapshot(pid).unwrap();
        let wrong_parent_result = kill_pinned_auth_process(wrong_parent);
        let still_same_process_after_parent_mismatch = read_auth_process_snapshot(pid).unwrap();
        let repinned = stop_and_pin_auth_process(
            pid,
            parent_pid,
            std::time::Instant::now() + AUTH_TREE_STOP_TIMEOUT,
        )
        .unwrap()
        .expect("the rejected process must remain available for exact cleanup");

        let cleanup_result = kill_pinned_auth_process(repinned);
        if cleanup_result.is_err() {
            let _ = signal_auth_process(pid, libc::SIGCONT);
            let _ = child.start_kill();
        }
        let wait_result = time::timeout(Duration::from_secs(2), child.wait()).await;

        assert_eq!(wrong_parent_result, Err(ClaudeRuntimeError::AuthAction));
        assert_eq!(wrong_start_result, Err(ClaudeRuntimeError::AuthAction));
        assert!(still_pinned_after_start_mismatch.is_some_and(|snapshot| {
            snapshot.identity == identity && snapshot.status == libc::SSTOP
        }));
        assert!(still_same_process_after_parent_mismatch
            .is_some_and(|snapshot| snapshot.identity.is_same_instance(identity)));
        assert_eq!(repinned, identity);
        assert_eq!(cleanup_result, Ok(()));
        assert!(wait_result.is_ok_and(|result| result.is_ok()));
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn auth_descendant_exact_limit_is_complete_and_kills_every_child() {
        let root = TempDir::new().unwrap();
        let script = write_cli(
            root.path(),
            "#!/bin/sh\nsleep 30 &\nfirst=$!\nsleep 30 &\nsecond=$!\nprintf '%s %s\\n' \"$first\" \"$second\" > \"$0.descendant-pids\"\n: > \"$0.ready\"\nwait\n",
        );
        let ready = root.path().join("fake-claude.ready");
        let pids_path = root.path().join("fake-claude.descendant-pids");
        let child = Command::new(&script)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let leader_pid = i32::try_from(child.id().unwrap()).unwrap();
        time::timeout(Duration::from_secs(2), async {
            while !ready.exists() {
                time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("fake auth CLI must create both descendants");
        let pids = fs::read_to_string(pids_path)
            .unwrap()
            .split_whitespace()
            .map(|pid| pid.parse::<i32>().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(pids.len(), 2);
        let identities = pids
            .iter()
            .map(|pid| {
                read_auth_process_snapshot(*pid)
                    .unwrap()
                    .expect("fake descendant must still be live")
                    .identity
            })
            .collect::<Vec<_>>();

        let cleanup_result = time::timeout(
            Duration::from_secs(3),
            terminate_auth_child_with_limit(child, leader_pid, 2),
        )
        .await
        .expect("exact-cap auth cleanup must remain bounded");
        let (leader_gone, first_terminated, second_terminated) = tokio::join!(
            wait_for_pid_gone(leader_pid),
            wait_for_auth_instance_terminated(identities[0]),
            wait_for_auth_instance_terminated(identities[1])
        );
        if !leader_gone {
            kill_test_pid(leader_pid);
        }
        for (identity, terminated) in identities
            .into_iter()
            .zip([first_terminated, second_terminated])
        {
            if !terminated {
                kill_test_auth_instance(identity);
            }
        }

        assert_eq!(cleanup_result, Ok(()));
        assert!(leader_gone, "the auth leader must be reaped");
        assert!(
            first_terminated && second_terminated,
            "an exact-cap traversal must clean every descendant"
        );
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn auth_descendant_limit_cleans_retained_prefix_and_reports_failure() {
        let root = TempDir::new().unwrap();
        let script = write_cli(
            root.path(),
            "#!/bin/sh\nsleep 30 &\nfirst=$!\nsleep 30 &\nsecond=$!\nprintf '%s %s\\n' \"$first\" \"$second\" > \"$0.descendant-pids\"\n: > \"$0.ready\"\nwait\n",
        );
        let ready = root.path().join("fake-claude.ready");
        let pids_path = root.path().join("fake-claude.descendant-pids");
        let child = Command::new(&script)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let leader_pid = i32::try_from(child.id().unwrap()).unwrap();
        time::timeout(Duration::from_secs(2), async {
            while !ready.exists() {
                time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("fake auth CLI must create both descendants");
        let pids = fs::read_to_string(pids_path)
            .unwrap()
            .split_whitespace()
            .map(|pid| pid.parse::<i32>().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(pids.len(), 2);
        let identities = pids
            .iter()
            .map(|pid| {
                read_auth_process_snapshot(*pid)
                    .unwrap()
                    .expect("fake descendant must still be live")
                    .identity
            })
            .collect::<Vec<_>>();

        let cleanup_result = time::timeout(
            Duration::from_secs(3),
            terminate_auth_child_with_limit(child, leader_pid, 1),
        )
        .await
        .expect("capped auth cleanup must remain bounded");
        let (leader_gone, first_terminated, second_terminated) = tokio::join!(
            wait_for_pid_gone(leader_pid),
            wait_for_auth_instance_terminated(identities[0]),
            wait_for_auth_instance_terminated(identities[1])
        );
        if !leader_gone {
            kill_test_pid(leader_pid);
        }
        for (identity, terminated) in identities
            .into_iter()
            .zip([first_terminated, second_terminated])
        {
            if !terminated {
                kill_test_auth_instance(identity);
            }
        }

        assert_eq!(cleanup_result, Err(ClaudeRuntimeError::AuthAction));
        assert!(
            leader_gone,
            "the auth leader must be reaped even on overflow"
        );
        assert!(
            first_terminated || second_terminated,
            "the safely retained descendant prefix must still be killed"
        );
    }
}
