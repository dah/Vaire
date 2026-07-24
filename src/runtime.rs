use std::ffi::{OsStr, OsString};
use std::fs;
use std::future::Future;
use std::os::unix::fs::MetadataExt;
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
use crate::backend::{BackendCoordinator, ClaudeBackendRuntime};
use crate::claude::{
    resolve_claude, verify_claude_version, ClaudeCliPolicy, ClaudeService, ClaudeSessionStore,
    FileClaudeSessionStore,
};
use crate::codex::safety::{FullAccessPolicy, IsolationPaths};
use crate::codex::session::SessionService;
use crate::codex::transport::{AppServerTransport, ProcessSpec};
use crate::credentials::{CredentialAccount, CredentialStore, FileCredentialStore};
use crate::diagnostics::{DiagnosticSink, FileDiagnosticSink};
use crate::openrouter::{
    FileOpenRouterStore, OpenRouterClient, OpenRouterConversationStore, OpenRouterService,
};
use crate::persistence::FilePreferences;
use crate::platform::{AppPaths, MacOsBrowser};

const SHUTDOWN_BOUND: Duration = Duration::from_secs(15);

mod build;
mod scheduler;
mod types;
mod version;

pub(in crate::runtime) use build::{build_backend, publish};
pub(in crate::runtime) use scheduler::*;
pub use types::{RuntimeCommand, RuntimeConfig, RuntimeError, RuntimeHandle};
pub(in crate::runtime) use version::verify_codex_version;
#[cfg(test)]
pub(in crate::runtime) use version::{
    collect_version_output, find_version, verify_codex_version_with_timeout,
};
pub use version::{resolve_codex, TESTED_CODEX_VERSION};

#[cfg(test)]
mod tests;
