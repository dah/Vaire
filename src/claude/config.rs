use std::ffi::{OsStr, OsString};
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use serde::Deserialize;
use thiserror::Error;
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tokio::time;

use crate::credentials::SecretValue;
pub use crate::provider::ClaudeModelAlias;
use crate::provider::ClaudeSessionId;

pub const TESTED_CLAUDE_VERSION: &str = "2.1.178";
const MAX_VERSION_OUTPUT_BYTES: usize = 64 * 1024;

pub const CLAUDE_MODEL_ALIASES: [ClaudeModelAlias; 4] = [
    ClaudeModelAlias::Default,
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
    },
    ResumeSession {
        session_id: ClaudeSessionId,
        model: ClaudeModelAlias,
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

    /// Constructs only documented CLI arguments. The prompt is written to stdin and the API key is
    /// injected by the process boundary, so neither can appear here.
    pub fn args(&self, invocation: &ClaudeInvocation) -> Vec<OsString> {
        let mut args = vec![
            OsString::from("--print"),
            OsString::from("--output-format"),
            OsString::from("stream-json"),
            OsString::from("--include-partial-messages"),
            OsString::from("--verbose"),
            OsString::from("--bare"),
            OsString::from("--safe-mode"),
            OsString::from("--no-chrome"),
            OsString::from("--disable-slash-commands"),
            OsString::from("--strict-mcp-config"),
            OsString::from("--mcp-config"),
            OsString::from("{}"),
            OsString::from("--settings"),
            OsString::from("{}"),
            OsString::from("--setting-sources"),
            OsString::from(""),
            OsString::from("--prompt-suggestions"),
            OsString::from("false"),
        ];
        match invocation {
            ClaudeInvocation::NewSession { session_id, model } => {
                add_user_turn_policy(&mut args, *model);
                args.extend([
                    OsString::from("--session-id"),
                    OsString::from(session_id.as_str()),
                ]);
            }
            ClaudeInvocation::ResumeSession { session_id, model } => {
                add_user_turn_policy(&mut args, *model);
                args.extend([
                    OsString::from("--resume"),
                    OsString::from(session_id.as_str()),
                ]);
            }
        }
        args
    }
}

fn add_user_turn_policy(args: &mut Vec<OsString>, model: ClaudeModelAlias) {
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
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum ClaudeRuntimeError {
    #[error("Claude CLI was not found")]
    NotFound,
    #[error("Claude CLI version check failed")]
    VersionCheck,
    #[error("Claude CLI version is unsupported")]
    UnsupportedVersion,
    #[error("Claude credential-source probe failed")]
    CredentialProbe,
    #[error("Claude did not select the Vairë Console API key")]
    UnsupportedCredentialSource,
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
    let mut command = Command::new(executable);
    apply_claude_environment(&mut command, home, None);
    command
        .kill_on_drop(true)
        .arg("--version")
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = command
        .spawn()
        .map_err(|_| ClaudeRuntimeError::VersionCheck)?;
    let mut stdout = child
        .stdout
        .take()
        .ok_or(ClaudeRuntimeError::VersionCheck)?;
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
    match time::timeout(timeout, probe).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(error)) => {
            let _ = child.start_kill();
            let _ = child.wait().await;
            Err(error)
        }
        Err(_) => {
            let _ = child.start_kill();
            let _ = child.wait().await;
            Err(ClaudeRuntimeError::VersionCheck)
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ClaudeAuthStatusPayload {
    logged_in: bool,
    auth_method: String,
    api_provider: String,
    api_key_source: String,
}

pub async fn verify_claude_credential_source(
    executable: &Path,
    config_dir: &Path,
    cwd: &Path,
    key: &SecretValue,
    timeout: Duration,
) -> Result<(), ClaudeRuntimeError> {
    let mut command = Command::new(executable);
    apply_claude_environment(&mut command, config_dir, Some(key));
    command
        .kill_on_drop(true)
        .current_dir(cwd)
        .args(["--bare", "--safe-mode", "auth", "status", "--json"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = command
        .spawn()
        .map_err(|_| ClaudeRuntimeError::CredentialProbe)?;
    let mut stdout = child
        .stdout
        .take()
        .ok_or(ClaudeRuntimeError::CredentialProbe)?;
    let probe = async {
        let mut bytes = Vec::new();
        (&mut stdout)
            .take((MAX_VERSION_OUTPUT_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .await
            .map_err(|_| ClaudeRuntimeError::CredentialProbe)?;
        if bytes.len() > MAX_VERSION_OUTPUT_BYTES {
            return Err(ClaudeRuntimeError::CredentialProbe);
        }
        let status = child
            .wait()
            .await
            .map_err(|_| ClaudeRuntimeError::CredentialProbe)?;
        if !status.success() {
            return Err(ClaudeRuntimeError::CredentialProbe);
        }
        let payload: ClaudeAuthStatusPayload =
            serde_json::from_slice(&bytes).map_err(|_| ClaudeRuntimeError::CredentialProbe)?;
        if payload.logged_in
            && payload.auth_method == "api_key"
            && payload.api_provider == "firstParty"
            && payload.api_key_source == "ANTHROPIC_API_KEY"
        {
            Ok(())
        } else {
            Err(ClaudeRuntimeError::UnsupportedCredentialSource)
        }
    };
    match time::timeout(timeout, probe).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(error)) => {
            let _ = child.start_kill();
            let _ = child.wait().await;
            Err(error)
        }
        Err(_) => {
            let _ = child.start_kill();
            let _ = child.wait().await;
            Err(ClaudeRuntimeError::CredentialProbe)
        }
    }
}

pub(super) fn apply_claude_environment(
    command: &mut Command,
    config_dir: &Path,
    key: Option<&SecretValue>,
) {
    for (name, _) in std::env::vars_os() {
        let name_text = name.to_string_lossy();
        if name_text.starts_with("ANTHROPIC_") || name_text.starts_with("CLAUDE_") {
            command.env_remove(name);
        }
    }
    command
        .env("CLAUDE_CONFIG_DIR", config_dir)
        .env("NO_COLOR", "1");
    if let Some(key) = key {
        command.env("ANTHROPIC_API_KEY", OsStr::from_bytes(key.expose_bytes()));
    }
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
