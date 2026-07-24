use std::time::Duration;

use super::*;
use crate::claude::{verify_claude_credential_source, ClaudeRuntimeError};

const CLAUDE_AUTH_TIMEOUT: Duration = Duration::from_secs(5);

pub(in crate::backend) fn claude_error(
    stage: ClaudeFailureStage,
    category: ClaudeFailureCategory,
) -> ClaudeError {
    ClaudeError::new(stage, category)
}

pub(in crate::backend) fn credential_probe_error(error: ClaudeRuntimeError) -> ClaudeError {
    let category = match error {
        ClaudeRuntimeError::UnsupportedCredentialSource => ClaudeFailureCategory::InvalidCredential,
        ClaudeRuntimeError::NotFound
        | ClaudeRuntimeError::VersionCheck
        | ClaudeRuntimeError::UnsupportedVersion
        | ClaudeRuntimeError::CredentialProbe => ClaudeFailureCategory::Unavailable,
    };
    claude_error(ClaudeFailureStage::Credential, category)
}

pub(in crate::backend) async fn validate_claude_key(
    runtime: &ClaudeBackendRuntime,
    key: &crate::credentials::SecretValue,
) -> Result<(), ClaudeError> {
    verify_claude_credential_source(
        runtime.policy.executable(),
        runtime.policy.home(),
        runtime.policy.cwd(),
        key,
        CLAUDE_AUTH_TIMEOUT,
    )
    .await
    .map_err(credential_probe_error)
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
