use std::collections::BTreeSet;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tempfile::tempdir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use url::Url;
use vaire::app::{
    Intent, OpenRouterConversationState, TranscriptEntryStatus, TranscriptRole, TurnState,
};
use vaire::backend::BackendCoordinator;
use vaire::codex::safety::{FullAccessPolicy, IsolationPaths};
use vaire::codex::session::SessionService;
use vaire::codex::transport::{AppServerTransport, ProcessSpec};
use vaire::credentials::{
    CredentialAccount, CredentialStore, FakeCredentialOperation, FakeCredentialStore, SecretValue,
};
use vaire::openrouter::{
    ChatRole, FileOpenRouterStore, OpenRouterAuthStatus, OpenRouterClient,
    OpenRouterConversationStore, OpenRouterConversationV2, OpenRouterService, OpenRouterTimeouts,
    OpenRouterTurnOutcome,
};
use vaire::persistence::{LoadOutcome, PersistenceError, PreferencesPort, PreferencesV4};
use vaire::platform::{BrowserError, BrowserOpener};
use vaire::provider::ProviderId;

const TEST_KEY: &str = "sk-or-v1-offline-vertical-slice-key";

#[path = "openrouter_vertical_slice/support.rs"]
mod support;
use support::*;

#[path = "openrouter_vertical_slice/auth_persistence.rs"]
mod auth_persistence;
#[path = "openrouter_vertical_slice/catalog_resume.rs"]
mod catalog_resume;
#[path = "openrouter_vertical_slice/history_restore.rs"]
mod history_restore;
#[path = "openrouter_vertical_slice/shutdown_fairness.rs"]
mod shutdown_fairness;
#[path = "openrouter_vertical_slice/turn_lifecycle.rs"]
mod turn_lifecycle;
