use std::fs;
use std::os::unix::fs::PermissionsExt;

use tempfile::tempdir;

use super::{
    collect_version_output, find_version, finish_event_or_shutdown, next_open_work, resolve_codex,
    verify_codex_version, verify_codex_version_with_timeout, EventCompletion, RuntimeError,
    RuntimeWork,
};
use crate::app::Intent;
use crate::codex::session::SessionEvent;

mod scheduler;
mod version;

#[tokio::test]
async fn missing_codex_is_provider_scoped_and_openrouter_runtime_still_starts() {
    let directory = tempdir().unwrap();
    let paths = crate::platform::AppPaths::from_home(directory.path());
    let config = super::RuntimeConfig {
        paths,
        codex_override: Some(directory.path().join("missing-codex").into_os_string()),
        claude_override: Some(directory.path().join("missing-claude").into_os_string()),
    };
    let mut backend = super::build_backend(config)
        .await
        .expect("core and OpenRouter paths remain usable without Codex");
    backend.startup().await.unwrap();
    assert!(matches!(
        backend.state().connection,
        crate::app::ConnectionState::Failed(_)
    ));
    assert_eq!(
        backend.state().openrouter.auth,
        crate::openrouter::OpenRouterAuthStatus::Missing
    );
}
