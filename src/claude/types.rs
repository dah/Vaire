use std::fmt;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::provider::{ClaudeSessionId, ClaudeTurnId};

use super::ClaudeModelAlias;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ClaudeModelMetadata {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaudeSessionLifecycle {
    Fresh,
    CreationPending,
    Established,
    CreationUncertain,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaudeTurnOutcome {
    InProgress,
    Completed,
    Interrupted,
    Failed,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ClaudeTurnRecord {
    pub id: ClaudeTurnId,
    pub requested_model: ClaudeModelAlias,
    pub user_text: String,
    pub assistant_text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub incomplete_assistant_text: Option<String>,
    pub outcome: ClaudeTurnOutcome,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ClaudeSessionV1 {
    pub version: u32,
    pub session_id: ClaudeSessionId,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
    pub title: String,
    pub selected_model: ClaudeModelAlias,
    pub resolved_model: Option<ClaudeModelMetadata>,
    pub lifecycle: ClaudeSessionLifecycle,
    pub turns: Vec<ClaudeTurnRecord>,
}

impl ClaudeSessionV1 {
    pub fn new(
        session_id: ClaudeSessionId,
        model: ClaudeModelAlias,
        now_ms: u64,
        title: impl Into<String>,
    ) -> Self {
        Self {
            version: 1,
            session_id,
            created_at_ms: now_ms,
            updated_at_ms: now_ms,
            title: title.into(),
            selected_model: model,
            resolved_model: None,
            lifecycle: ClaudeSessionLifecycle::Fresh,
            turns: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ClaudeSessionSummary {
    pub session_id: ClaudeSessionId,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
    pub title: String,
    pub turn_count: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClaudeAuthAction {
    Login,
    Logout,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClaudeCliAuthState {
    SignedOut,
    Subscription,
    Unsupported,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClaudeAuthStatus {
    SignedOut,
    Subscription,
    Unsupported,
    Unverified,
    CliUnavailable,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClaudeFailureStage {
    Spawn,
    Stdin,
    Stdout,
    Protocol,
    Exit,
    Reap,
    Store,
    Auth,
    Shutdown,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClaudeFailureCategory {
    Unavailable,
    Io,
    Protocol,
    ResourceLimit,
    Interrupted,
    NonZeroExit,
    CorruptStore,
}

#[derive(Clone, Copy, Error, Eq, PartialEq)]
#[error("Claude operation failed ({stage:?}/{category:?})")]
pub struct ClaudeError {
    pub stage: ClaudeFailureStage,
    pub category: ClaudeFailureCategory,
}

impl ClaudeError {
    pub const fn new(stage: ClaudeFailureStage, category: ClaudeFailureCategory) -> Self {
        Self { stage, category }
    }
}

impl fmt::Debug for ClaudeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ClaudeError")
            .field("stage", &self.stage)
            .field("category", &self.category)
            .finish()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ClaudeServiceEvent {
    TurnStarted {
        session_id: ClaudeSessionId,
        turn_id: ClaudeTurnId,
    },
    Initialized {
        session_id: ClaudeSessionId,
        turn_id: ClaudeTurnId,
        model: ClaudeModelMetadata,
    },
    TextDelta {
        session_id: ClaudeSessionId,
        turn_id: ClaudeTurnId,
        delta: String,
    },
    TurnFinished {
        session_id: ClaudeSessionId,
        turn_id: ClaudeTurnId,
        outcome: ClaudeTurnOutcome,
        assistant_text: Option<String>,
        incomplete_assistant_text: Option<String>,
        creation_uncertain: bool,
        failure: Option<ClaudeError>,
    },
}
