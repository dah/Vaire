use std::ffi::{OsStr, OsString};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use serde::Deserialize;
use thiserror::Error;
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tokio::time;
use tokio_util::sync::CancellationToken;

use crate::provider::ClaudeSessionId;
pub use crate::provider::{ClaudeEffort, ClaudeModelAlias};

use super::ClaudeCliAuthState;

pub const TESTED_CLAUDE_VERSION: &str = "2.1.218";
const MAX_VERSION_OUTPUT_BYTES: usize = 64 * 1024;
const MAX_AUTH_STATUS_OUTPUT_BYTES: usize = 64 * 1024;
pub(super) const SUBSCRIPTION_SETTINGS: &str = r#"{"forceLoginMethod":"claudeai"}"#;
const EMPTY_MCP_CONFIG: &str = r#"{"mcpServers":{}}"#;

pub const CLAUDE_MODEL_ALIASES: [ClaudeModelAlias; 5] = [
    ClaudeModelAlias::Default,
    ClaudeModelAlias::Fable,
    ClaudeModelAlias::Opus,
    ClaudeModelAlias::Sonnet,
    ClaudeModelAlias::Haiku,
];

pub const fn claude_model_selector(alias: ClaudeModelAlias) -> &'static str {
    alias.as_str()
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ClaudeInvocation {
    NewSession {
        session_id: ClaudeSessionId,
        model: ClaudeModelAlias,
        effort: Option<ClaudeEffort>,
    },
    ResumeSession {
        session_id: ClaudeSessionId,
        model: ClaudeModelAlias,
        effort: Option<ClaudeEffort>,
    },
}

#[derive(Clone, Debug)]
pub struct ClaudeCliPolicy {
    executable: PathBuf,
    home: PathBuf,
    cwd: PathBuf,
}

impl ClaudeCliPolicy {
    pub fn new(executable: PathBuf, home: PathBuf, cwd: PathBuf) -> Self {
        Self {
            executable,
            home,
            cwd,
        }
    }

    pub fn executable(&self) -> &Path {
        &self.executable
    }

    pub fn home(&self) -> &Path {
        &self.home
    }

    pub fn cwd(&self) -> &Path {
        &self.cwd
    }

    /// Constructs only documented CLI arguments. The prompt is written to stdin, so it cannot
    /// appear here. Authentication remains entirely owned by the installed Claude CLI.
    pub fn args(&self, invocation: &ClaudeInvocation) -> Vec<OsString> {
        let mut args = vec![
            OsString::from("--print"),
            OsString::from("--output-format"),
            OsString::from("stream-json"),
            OsString::from("--include-partial-messages"),
            OsString::from("--verbose"),
            OsString::from("--safe-mode"),
            OsString::from("--no-chrome"),
            OsString::from("--disable-slash-commands"),
            OsString::from("--strict-mcp-config"),
            OsString::from("--mcp-config"),
            OsString::from(EMPTY_MCP_CONFIG),
            OsString::from("--settings"),
            OsString::from(SUBSCRIPTION_SETTINGS),
            OsString::from("--setting-sources"),
            OsString::from(""),
            OsString::from("--prompt-suggestions"),
            OsString::from("false"),
        ];
        match invocation {
            ClaudeInvocation::NewSession {
                session_id,
                model,
                effort,
            } => {
                add_user_turn_policy(&mut args, *model, *effort);
                args.extend([
                    OsString::from("--session-id"),
                    OsString::from(session_id.as_str()),
                ]);
            }
            ClaudeInvocation::ResumeSession {
                session_id,
                model,
                effort,
            } => {
                add_user_turn_policy(&mut args, *model, *effort);
                args.extend([
                    OsString::from("--resume"),
                    OsString::from(session_id.as_str()),
                ]);
            }
        }
        args
    }
}

fn add_user_turn_policy(
    args: &mut Vec<OsString>,
    model: ClaudeModelAlias,
    effort: Option<ClaudeEffort>,
) {
    args.extend([
        OsString::from("--dangerously-skip-permissions"),
        OsString::from("--permission-mode"),
        OsString::from("bypassPermissions"),
        OsString::from("--tools"),
        OsString::from("default"),
        OsString::from("--disallowedTools"),
        OsString::from("Agent,Task,AskUserQuestion,WebFetch,WebSearch"),
        OsString::from("--model"),
        OsString::from(claude_model_selector(model)),
    ]);
    if let Some(effort) = effort {
        args.extend([OsString::from("--effort"), OsString::from(effort.as_str())]);
    }
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum ClaudeRuntimeError {
    #[error("Claude CLI was not found")]
    NotFound,
    #[error("Claude CLI version check failed")]
    VersionCheck,
    #[error("Claude CLI version is unsupported")]
    UnsupportedVersion,
    #[error("Claude authentication status check failed")]
    AuthStatus,
    #[error("Claude authentication action failed")]
    AuthAction,
    #[error("Claude authentication action was cancelled")]
    AuthCancelled,
}

pub fn resolve_claude(override_name: Option<&OsStr>) -> Result<PathBuf, ClaudeRuntimeError> {
    let name = override_name.unwrap_or_else(|| OsStr::new("claude"));
    let candidate = PathBuf::from(name);
    if candidate.components().count() > 1 {
        return canonical_executable(&candidate).ok_or(ClaudeRuntimeError::NotFound);
    }
    let path = std::env::var_os("PATH").ok_or(ClaudeRuntimeError::NotFound)?;
    std::env::split_paths(&path)
        .map(|directory| directory.join(name))
        .find_map(|candidate| canonical_executable(&candidate))
        .ok_or(ClaudeRuntimeError::NotFound)
}

fn canonical_executable(path: &Path) -> Option<PathBuf> {
    fs::metadata(path)
        .ok()
        .filter(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
        .and_then(|_| fs::canonicalize(path).ok())
}

pub async fn verify_claude_version(
    executable: &Path,
    home: &Path,
    timeout: Duration,
) -> Result<(), ClaudeRuntimeError> {
    let cancellation = CancellationToken::new();
    verify_claude_version_cancellable(executable, home, timeout, &cancellation).await
}

pub(crate) async fn verify_claude_version_cancellable(
    executable: &Path,
    home: &Path,
    timeout: Duration,
    cancellation: &CancellationToken,
) -> Result<(), ClaudeRuntimeError> {
    if cancellation.is_cancelled() {
        return Err(ClaudeRuntimeError::AuthCancelled);
    }
    let mut command = Command::new(executable);
    apply_claude_environment(&mut command, home);
    command
        .kill_on_drop(true)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
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
    let mut child = command
        .spawn()
        .map_err(|_| ClaudeRuntimeError::VersionCheck)?;
    let Some(process_group) = child
        .id()
        .and_then(|id| i32::try_from(id).ok())
        .filter(|id| *id > 0)
    else {
        let _ = child.start_kill();
        let _ = child.wait().await;
        return Err(ClaudeRuntimeError::VersionCheck);
    };
    let Some(mut stdout) = child.stdout.take() else {
        terminate_probe(&mut child, process_group).await;
        return Err(ClaudeRuntimeError::VersionCheck);
    };
    let mut reaped = false;
    let probe = async {
        let mut bytes = Vec::new();
        (&mut stdout)
            .take((MAX_VERSION_OUTPUT_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .await
            .map_err(|_| ClaudeRuntimeError::VersionCheck)?;
        if bytes.len() > MAX_VERSION_OUTPUT_BYTES {
            return Err(ClaudeRuntimeError::VersionCheck);
        }
        let status = child
            .wait()
            .await
            .map_err(|_| ClaudeRuntimeError::VersionCheck)?;
        reaped = true;
        if !status.success() {
            return Err(ClaudeRuntimeError::VersionCheck);
        }
        let output = std::str::from_utf8(&bytes).map_err(|_| ClaudeRuntimeError::VersionCheck)?;
        let found = output
            .split_whitespace()
            .find_map(|word| parse_version(word.trim_start_matches('v')))
            .ok_or(ClaudeRuntimeError::VersionCheck)?;
        let minimum = parse_version(TESTED_CLAUDE_VERSION).expect("tested Claude version is valid");
        (found >= minimum)
            .then_some(())
            .ok_or(ClaudeRuntimeError::UnsupportedVersion)
    };
    let outcome = tokio::select! {
        biased;
        _ = cancellation.cancelled() => None,
        result = time::timeout(timeout, probe) => Some(result),
    };
    match outcome {
        Some(Ok(Ok(()))) => Ok(()),
        Some(Ok(Err(error))) => {
            if !reaped {
                terminate_probe(&mut child, process_group).await;
            }
            Err(error)
        }
        Some(Err(_)) => {
            if !reaped {
                terminate_probe(&mut child, process_group).await;
            }
            Err(ClaudeRuntimeError::VersionCheck)
        }
        None => {
            if !reaped {
                terminate_probe(&mut child, process_group).await;
            }
            Err(ClaudeRuntimeError::AuthCancelled)
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ClaudeAuthStatusPayload {
    logged_in: bool,
    #[serde(default)]
    auth_method: Option<String>,
    #[serde(default)]
    api_provider: Option<String>,
}

pub async fn inspect_claude_auth(
    executable: &Path,
    config_dir: &Path,
    cwd: &Path,
    timeout: Duration,
) -> Result<ClaudeCliAuthState, ClaudeRuntimeError> {
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
            "auth",
            "status",
            "--json",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
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
    let mut child = command
        .spawn()
        .map_err(|_| ClaudeRuntimeError::AuthStatus)?;
    let Some(process_group) = child
        .id()
        .and_then(|id| i32::try_from(id).ok())
        .filter(|id| *id > 0)
    else {
        let _ = child.start_kill();
        let _ = child.wait().await;
        return Err(ClaudeRuntimeError::AuthStatus);
    };
    let Some(mut stdout) = child.stdout.take() else {
        terminate_probe(&mut child, process_group).await;
        return Err(ClaudeRuntimeError::AuthStatus);
    };
    let mut reaped = false;
    let probe = async {
        let mut bytes = Vec::new();
        (&mut stdout)
            .take((MAX_AUTH_STATUS_OUTPUT_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .await
            .map_err(|_| ClaudeRuntimeError::AuthStatus)?;
        if bytes.len() > MAX_AUTH_STATUS_OUTPUT_BYTES {
            return Err(ClaudeRuntimeError::AuthStatus);
        }
        let payload: ClaudeAuthStatusPayload =
            serde_json::from_slice(&bytes).map_err(|_| ClaudeRuntimeError::AuthStatus)?;
        let status = child
            .wait()
            .await
            .map_err(|_| ClaudeRuntimeError::AuthStatus)?;
        reaped = true;
        if status.code() == Some(1) && !payload.logged_in {
            return Ok(ClaudeCliAuthState::SignedOut);
        }
        if !status.success() {
            return Err(ClaudeRuntimeError::AuthStatus);
        }
        if !payload.logged_in {
            Ok(ClaudeCliAuthState::SignedOut)
        } else if payload.auth_method.as_deref() == Some("claude.ai")
            && payload.api_provider.as_deref() == Some("firstParty")
        {
            Ok(ClaudeCliAuthState::Subscription)
        } else {
            Ok(ClaudeCliAuthState::Unsupported)
        }
    };
    match time::timeout(timeout, probe).await {
        Ok(Ok(state)) => Ok(state),
        Ok(Err(error)) => {
            if !reaped {
                terminate_probe(&mut child, process_group).await;
            }
            Err(error)
        }
        Err(_) => {
            if !reaped {
                terminate_probe(&mut child, process_group).await;
            }
            Err(ClaudeRuntimeError::AuthStatus)
        }
    }
}

async fn terminate_probe(child: &mut tokio::process::Child, process_group: i32) {
    // SAFETY: kill accepts a negative process-group identifier and retains no pointer.
    if unsafe { libc::kill(-process_group, libc::SIGKILL) } != 0 {
        let _ = child.start_kill();
    }
    let _ = child.wait().await;
}

pub(super) fn apply_claude_environment(command: &mut Command, config_dir: &Path) {
    for (name, _) in std::env::vars_os() {
        let name_text = name.to_string_lossy();
        if name_text.starts_with("ANTHROPIC_") || name_text.starts_with("CLAUDE") {
            command.env_remove(name);
        }
    }
    command
        .env("CLAUDE_CONFIG_DIR", config_dir)
        .env("NO_COLOR", "1");
}

fn parse_version(value: &str) -> Option<(u64, u64, u64)> {
    let value = value.split(['-', '+']).next()?;
    let mut parts = value.split('.');
    let parsed = (
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
    );
    parts.next().is_none().then_some(parsed)
}
