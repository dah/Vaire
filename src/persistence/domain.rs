use std::collections::{BTreeMap, BTreeSet};
use std::io;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::provider::{OpenRouterConversationId, ProviderId};
use crate::storage::CommitStatus;
use crate::text::is_terminal_unsafe;

pub const PREFERENCES_VERSION: u32 = 2;
pub(super) const LEGACY_PREFERENCES_VERSION: u32 = 1;
pub(super) const MAX_PREFERENCES_BYTES: usize = 1024 * 1024;
pub(super) const MAX_PREFERENCE_STRING_BYTES: usize = 16 * 1024;
const MAX_EMAIL_BYTES: usize = 320;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum AccountScope {
    ChatgptEmail(String),
}

impl AccountScope {
    pub fn from_chatgpt_email(email: &str) -> Option<Self> {
        let normalized = email.trim().to_ascii_lowercase();
        (!normalized.is_empty()
            && normalized.len() <= MAX_EMAIL_BYTES
            && !normalized
                .chars()
                .any(|value| is_terminal_unsafe(value) || value.is_whitespace()))
        .then_some(Self::ChatgptEmail(normalized))
    }
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct CodexPreferencesV2 {
    pub account_scope: Option<AccountScope>,
    pub auto_resume_thread_id: Option<String>,
    pub model_id: Option<String>,
    pub reasoning_effort: Option<String>,
    #[serde(default)]
    pub thread_account_scopes: BTreeMap<String, AccountScope>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct OpenRouterPreferencesV2 {
    pub auto_resume_conversation_id: Option<OpenRouterConversationId>,
    pub selected_model_id: Option<String>,
    #[serde(default)]
    pub enabled_model_ids: BTreeSet<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PreferencesV2 {
    pub version: u32,
    pub active_provider: ProviderId,
    pub codex: CodexPreferencesV2,
    pub openrouter: OpenRouterPreferencesV2,
}

impl Default for PreferencesV2 {
    fn default() -> Self {
        Self {
            version: PREFERENCES_VERSION,
            active_provider: ProviderId::Codex,
            codex: CodexPreferencesV2::default(),
            openrouter: OpenRouterPreferencesV2::default(),
        }
    }
}

impl PreferencesV2 {
    pub fn set_auto_resume_conversation(
        &mut self,
        conversation_id: Option<OpenRouterConversationId>,
    ) {
        if conversation_id.is_some() {
            self.active_provider = ProviderId::OpenRouter;
            self.codex.auto_resume_thread_id = None;
        }
        self.openrouter.auto_resume_conversation_id = conversation_id;
    }

    pub fn set_auto_resume_thread(&mut self, thread_id: Option<String>) {
        if thread_id.is_some() {
            self.active_provider = ProviderId::Codex;
            self.openrouter.auto_resume_conversation_id = None;
        }
        self.codex.auto_resume_thread_id = thread_id;
    }

    pub fn clear_auto_resume(&mut self) {
        self.codex.auto_resume_thread_id = None;
        self.openrouter.auto_resume_conversation_id = None;
    }
}

#[derive(Clone, Debug, Deserialize)]
pub(super) struct PreferencesV1 {
    pub(super) version: u32,
    pub(super) account_scope: Option<AccountScope>,
    pub(super) thread_id: Option<String>,
    pub(super) model_id: Option<String>,
    pub(super) reasoning_effort: Option<String>,
    #[serde(default)]
    pub(super) thread_account_scopes: BTreeMap<String, AccountScope>,
}

impl From<PreferencesV1> for PreferencesV2 {
    fn from(value: PreferencesV1) -> Self {
        Self {
            version: PREFERENCES_VERSION,
            active_provider: ProviderId::Codex,
            codex: CodexPreferencesV2 {
                account_scope: value.account_scope,
                auto_resume_thread_id: value.thread_id,
                model_id: value.model_id,
                reasoning_effort: value.reasoning_effort,
                thread_account_scopes: value.thread_account_scopes,
            },
            openrouter: OpenRouterPreferencesV2::default(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LoadNotice {
    Missing,
    MigratedV1,
    Corrupt,
    UnsupportedVersion(u32),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LoadOutcome {
    pub preferences: PreferencesV2,
    pub notice: Option<LoadNotice>,
    pub may_overwrite: bool,
    pub needs_save: bool,
}

#[derive(Debug, Error)]
pub enum PersistenceError {
    #[error("preferences I/O failed: {0}")]
    Io(#[from] io::Error),
    #[error("preferences encoding failed: {0}")]
    Encode(#[from] serde_json::Error),
}

pub trait PreferencesPort {
    fn load(&self) -> Result<LoadOutcome, PersistenceError>;
    fn save(&self, preferences: &PreferencesV2) -> Result<(), PersistenceError>;

    fn save_with_commit(
        &self,
        preferences: &PreferencesV2,
    ) -> Result<CommitStatus, PersistenceError> {
        self.save(preferences)?;
        Ok(CommitStatus::Verified)
    }
}
