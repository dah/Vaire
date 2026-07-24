//! Claude Code provider boundary.
//!
//! This module uses only the installed CLI's documented non-interactive interface. Claude-owned
//! state is opaque; the store below contains only Vairë registrations and bounded display history.

mod config;
mod process;
mod protocol;
mod service;
mod store;
mod types;

pub use config::{
    claude_model_selector, resolve_claude, verify_claude_credential_source, verify_claude_version,
    ClaudeCliPolicy, ClaudeInvocation, ClaudeModelAlias, ClaudeRuntimeError, CLAUDE_MODEL_ALIASES,
    TESTED_CLAUDE_VERSION,
};
pub use process::{ClaudeChild, ClaudeProcessError};
pub use protocol::{ClaudeProtocolError, ClaudeStreamEvent, ClaudeStreamParser};
pub use service::{ClaudeService, PreparedClaudeTurn};
pub use store::{
    ClaudeSessionCommit, ClaudeSessionStore, ClaudeStoreError, FileClaudeSessionStore,
};
pub use types::*;

#[cfg(test)]
mod tests;
