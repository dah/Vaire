use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::provider::{OpenRouterConversationId, ProviderId};
use crate::storage::{CommitStatus, DirectorySync, RealDirectorySync};
use crate::text::is_terminal_unsafe;

pub const PREFERENCES_VERSION: u32 = 2;
const LEGACY_PREFERENCES_VERSION: u32 = 1;
const MAX_PREFERENCES_BYTES: usize = 1024 * 1024;
const MAX_PREFERENCE_STRING_BYTES: usize = 16 * 1024;
const MAX_EMAIL_BYTES: usize = 320;
static TEMP_FILE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

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
struct PreferencesV1 {
    version: u32,
    account_scope: Option<AccountScope>,
    thread_id: Option<String>,
    model_id: Option<String>,
    reasoning_effort: Option<String>,
    #[serde(default)]
    thread_account_scopes: BTreeMap<String, AccountScope>,
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

#[derive(Clone, Debug)]
pub struct FilePreferences {
    path: PathBuf,
    directory_sync: Arc<dyn DirectorySync>,
}

impl FilePreferences {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            directory_sync: Arc::new(RealDirectorySync),
        }
    }

    #[cfg(test)]
    fn with_directory_sync(
        path: impl Into<PathBuf>,
        directory_sync: Arc<dyn DirectorySync>,
    ) -> Self {
        Self {
            path: path.into(),
            directory_sync,
        }
    }

    fn parent(&self) -> Result<&Path, io::Error> {
        self.path.parent().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "preferences path has no parent",
            )
        })
    }

    fn create_temp_file(&self, parent: &Path) -> Result<(PathBuf, File), io::Error> {
        let name = self
            .path
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("preferences");
        for _ in 0..128 {
            let sequence = TEMP_FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let path = parent.join(format!(".{name}.{}.{}.tmp", std::process::id(), sequence));
            match OpenOptions::new()
                .create_new(true)
                .write(true)
                .mode(0o600)
                .open(&path)
            {
                Ok(file) => return Ok((path, file)),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            }
        }
        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "could not allocate a unique preferences temporary file",
        ))
    }
}

fn valid_preference_string(value: &str) -> bool {
    !value.trim().is_empty()
        && value == value.trim()
        && value.len() <= MAX_PREFERENCE_STRING_BYTES
        && !value.chars().any(is_terminal_unsafe)
}

fn valid_scope(scope: &AccountScope) -> bool {
    match scope {
        AccountScope::ChatgptEmail(email) => {
            AccountScope::from_chatgpt_email(email).as_ref() == Some(scope)
        }
    }
}

fn valid_codex(preferences: &CodexPreferencesV2) -> bool {
    if preferences
        .account_scope
        .as_ref()
        .is_some_and(|scope| !valid_scope(scope))
        || preferences
            .auto_resume_thread_id
            .as_deref()
            .is_some_and(|value| !valid_preference_string(value))
        || preferences
            .model_id
            .as_deref()
            .is_some_and(|value| !valid_preference_string(value))
        || preferences
            .reasoning_effort
            .as_deref()
            .is_some_and(|value| !valid_preference_string(value))
        || preferences
            .thread_account_scopes
            .iter()
            .any(|(id, scope)| !valid_preference_string(id) || !valid_scope(scope))
    {
        return false;
    }

    match (
        preferences.auto_resume_thread_id.as_ref(),
        preferences.account_scope.as_ref(),
    ) {
        (Some(thread_id), Some(account_scope)) => preferences
            .thread_account_scopes
            .get(thread_id)
            .is_none_or(|registered_scope| registered_scope == account_scope),
        _ => true,
    }
}

fn valid_legacy(preferences: &PreferencesV1) -> bool {
    preferences.version == LEGACY_PREFERENCES_VERSION
        && valid_codex(&CodexPreferencesV2 {
            account_scope: preferences.account_scope.clone(),
            auto_resume_thread_id: preferences.thread_id.clone(),
            model_id: preferences.model_id.clone(),
            reasoning_effort: preferences.reasoning_effort.clone(),
            thread_account_scopes: preferences.thread_account_scopes.clone(),
        })
}

fn valid_preferences(preferences: &PreferencesV2) -> bool {
    if preferences.version != PREFERENCES_VERSION
        || !valid_codex(&preferences.codex)
        || preferences
            .openrouter
            .selected_model_id
            .as_deref()
            .is_some_and(|value| !valid_preference_string(value))
        || preferences
            .openrouter
            .enabled_model_ids
            .iter()
            .any(|value| !valid_preference_string(value))
    {
        return false;
    }

    match preferences.active_provider {
        ProviderId::Codex => preferences.openrouter.auto_resume_conversation_id.is_none(),
        ProviderId::OpenRouter => preferences.codex.auto_resume_thread_id.is_none(),
    }
}

fn corrupt_load_outcome() -> LoadOutcome {
    LoadOutcome {
        preferences: PreferencesV2::default(),
        notice: Some(LoadNotice::Corrupt),
        may_overwrite: false,
        needs_save: false,
    }
}

impl PreferencesPort for FilePreferences {
    fn load(&self) -> Result<LoadOutcome, PersistenceError> {
        let file = match File::open(&self.path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(LoadOutcome {
                    preferences: PreferencesV2::default(),
                    notice: Some(LoadNotice::Missing),
                    may_overwrite: true,
                    needs_save: false,
                });
            }
            Err(error) => return Err(error.into()),
        };
        let mut bytes = Vec::new();
        file.take((MAX_PREFERENCES_BYTES + 1) as u64)
            .read_to_end(&mut bytes)?;
        if bytes.len() > MAX_PREFERENCES_BYTES {
            return Ok(corrupt_load_outcome());
        }

        let raw: serde_json::Value = match serde_json::from_slice(&bytes) {
            Ok(value) => value,
            Err(_) => return Ok(corrupt_load_outcome()),
        };
        let version = match raw.get("version") {
            None => 0,
            Some(value) => {
                let Some(version) = value
                    .as_u64()
                    .and_then(|version| u32::try_from(version).ok())
                else {
                    return Ok(corrupt_load_outcome());
                };
                version
            }
        };

        match version {
            LEGACY_PREFERENCES_VERSION => match serde_json::from_value(raw) {
                Ok(preferences) if valid_legacy(&preferences) => Ok(LoadOutcome {
                    preferences: PreferencesV2::from(preferences),
                    notice: Some(LoadNotice::MigratedV1),
                    may_overwrite: true,
                    needs_save: true,
                }),
                Ok(_) | Err(_) => Ok(corrupt_load_outcome()),
            },
            PREFERENCES_VERSION => match serde_json::from_value(raw) {
                Ok(preferences) if valid_preferences(&preferences) => Ok(LoadOutcome {
                    preferences,
                    notice: None,
                    may_overwrite: true,
                    needs_save: false,
                }),
                Ok(_) | Err(_) => Ok(corrupt_load_outcome()),
            },
            _ => Ok(LoadOutcome {
                preferences: PreferencesV2::default(),
                notice: Some(LoadNotice::UnsupportedVersion(version)),
                may_overwrite: false,
                needs_save: false,
            }),
        }
    }

    fn save(&self, preferences: &PreferencesV2) -> Result<(), PersistenceError> {
        self.save_with_commit(preferences).map(|_| ())
    }

    fn save_with_commit(
        &self,
        preferences: &PreferencesV2,
    ) -> Result<CommitStatus, PersistenceError> {
        if !valid_preferences(preferences) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "cannot write invalid preferences",
            )
            .into());
        }
        let bytes = serde_json::to_vec_pretty(preferences)?;
        if bytes.len() >= MAX_PREFERENCES_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "preferences exceeded the size limit",
            )
            .into());
        }
        let parent = self.parent()?;
        fs::create_dir_all(parent)?;
        fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
        let (temp_path, mut file) = self.create_temp_file(parent)?;
        let precommit = (|| -> io::Result<()> {
            file.set_permissions(fs::Permissions::from_mode(0o600))?;
            file.write_all(&bytes)?;
            file.write_all(b"\n")?;
            file.sync_all()?;
            fs::rename(&temp_path, &self.path)?;
            Ok(())
        })();
        if let Err(error) = precommit {
            let _ = fs::remove_file(&temp_path);
            return Err(PersistenceError::Io(error));
        }
        Ok(match self.directory_sync.sync(parent) {
            Ok(()) => CommitStatus::Verified,
            Err(_)
                if fs::read(&self.path).is_ok_and(|written| {
                    written.strip_suffix(b"\n").unwrap_or(&written) == bytes
                }) =>
            {
                CommitStatus::CommittedUnverified
            }
            Err(error) => return Err(PersistenceError::Io(error)),
        })
    }
}

#[cfg(test)]
mod tests;
