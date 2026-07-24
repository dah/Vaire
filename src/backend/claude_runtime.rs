use std::time::Duration;

use super::*;
use crate::claude::{inspect_claude_auth, ClaudeCliAuthState, ClaudeRuntimeError};

const CLAUDE_AUTH_TIMEOUT: Duration = Duration::from_secs(5);

pub(in crate::backend) fn claude_error(
    stage: ClaudeFailureStage,
    category: ClaudeFailureCategory,
) -> ClaudeError {
    ClaudeError::new(stage, category)
}

pub(in crate::backend) fn auth_operation_error(_error: ClaudeRuntimeError) -> ClaudeError {
    claude_error(ClaudeFailureStage::Auth, ClaudeFailureCategory::Unavailable)
}

pub(in crate::backend) async fn inspect_runtime_auth(
    runtime: &ClaudeBackendRuntime,
) -> Result<ClaudeAuthStatus, ClaudeError> {
    let status = inspect_claude_auth(
        runtime.policy.executable(),
        runtime.policy.home(),
        runtime.policy.cwd(),
        CLAUDE_AUTH_TIMEOUT,
    )
    .await
    .map_err(auth_operation_error)?;
    Ok(match status {
        ClaudeCliAuthState::SignedOut => ClaudeAuthStatus::SignedOut,
        ClaudeCliAuthState::Subscription => ClaudeAuthStatus::Subscription,
        ClaudeCliAuthState::Unsupported => ClaudeAuthStatus::Unsupported,
    })
}

pub(in crate::backend) fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

pub(in crate::backend) fn selected_claude_alias(
    state: &AppState,
) -> crate::provider::ClaudeModelAlias {
    state
        .preferences
        .claude
        .selected_model_alias
        .unwrap_or(crate::provider::ClaudeModelAlias::Default)
}

pub(in crate::backend) fn claude_store_error() -> ClaudeError {
    claude_error(
        ClaudeFailureStage::Store,
        ClaudeFailureCategory::CorruptStore,
    )
}
