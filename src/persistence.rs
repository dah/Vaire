use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::text::is_terminal_unsafe;

pub const PREFERENCES_VERSION: u32 = 1;
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

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PreferencesV1 {
    pub version: u32,
    pub account_scope: Option<AccountScope>,
    pub thread_id: Option<String>,
    pub model_id: Option<String>,
    pub reasoning_effort: Option<String>,
    #[serde(default)]
    pub thread_account_scopes: BTreeMap<String, AccountScope>,
}

impl Default for PreferencesV1 {
    fn default() -> Self {
        Self {
            version: PREFERENCES_VERSION,
            account_scope: None,
            thread_id: None,
            model_id: None,
            reasoning_effort: None,
            thread_account_scopes: BTreeMap::new(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LoadNotice {
    Missing,
    Corrupt,
    UnsupportedVersion(u32),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LoadOutcome {
    pub preferences: PreferencesV1,
    pub notice: Option<LoadNotice>,
    pub may_overwrite: bool,
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
    fn save(&self, preferences: &PreferencesV1) -> Result<(), PersistenceError>;
}

#[derive(Clone, Debug)]
pub struct FilePreferences {
    path: PathBuf,
}

impl FilePreferences {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
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

fn valid_preferences(preferences: &PreferencesV1) -> bool {
    if preferences.version != PREFERENCES_VERSION
        || preferences
            .account_scope
            .as_ref()
            .is_some_and(|scope| !valid_scope(scope))
        || preferences
            .thread_id
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
        preferences.thread_id.as_ref(),
        preferences.account_scope.as_ref(),
    ) {
        (Some(thread_id), Some(account_scope)) => preferences
            .thread_account_scopes
            .get(thread_id)
            .is_none_or(|registered_scope| registered_scope == account_scope),
        _ => true,
    }
}

fn corrupt_load_outcome() -> LoadOutcome {
    LoadOutcome {
        preferences: PreferencesV1::default(),
        notice: Some(LoadNotice::Corrupt),
        may_overwrite: false,
    }
}

impl PreferencesPort for FilePreferences {
    fn load(&self) -> Result<LoadOutcome, PersistenceError> {
        let file = match File::open(&self.path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(LoadOutcome {
                    preferences: PreferencesV1::default(),
                    notice: Some(LoadNotice::Missing),
                    may_overwrite: true,
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
        if version != PREFERENCES_VERSION {
            return Ok(LoadOutcome {
                preferences: PreferencesV1::default(),
                notice: Some(LoadNotice::UnsupportedVersion(version)),
                may_overwrite: false,
            });
        }

        match serde_json::from_value(raw) {
            Ok(preferences) if valid_preferences(&preferences) => Ok(LoadOutcome {
                preferences,
                notice: None,
                may_overwrite: true,
            }),
            Ok(_) | Err(_) => Ok(corrupt_load_outcome()),
        }
    }

    fn save(&self, preferences: &PreferencesV1) -> Result<(), PersistenceError> {
        if !valid_preferences(preferences) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "cannot write invalid preferences",
            )
            .into());
        }
        let bytes = serde_json::to_vec_pretty(preferences)?;
        // Include the trailing newline without arithmetic that could wrap at an adversarial
        // boundary on a narrower target.
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
        let write_result = (|| -> io::Result<()> {
            file.set_permissions(fs::Permissions::from_mode(0o600))?;
            file.write_all(&bytes)?;
            file.write_all(b"\n")?;
            file.sync_all()?;
            fs::rename(&temp_path, &self.path)?;
            OpenOptions::new().read(true).open(parent)?.sync_all()?;
            Ok(())
        })();
        if write_result.is_err() {
            let _ = fs::remove_file(&temp_path);
        }
        write_result.map_err(PersistenceError::Io)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        AccountScope, FilePreferences, LoadNotice, PreferencesPort, PreferencesV1,
        MAX_PREFERENCES_BYTES, PREFERENCES_VERSION,
    };
    use std::collections::BTreeMap;
    use std::fs;
    use std::os::unix::fs::symlink;
    use std::os::unix::fs::PermissionsExt;
    use tempfile::tempdir;

    #[test]
    fn round_trips_atomically_with_owner_only_permissions() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("state").join("preferences.json");
        let store = FilePreferences::new(&path);
        let preferences = PreferencesV1 {
            version: PREFERENCES_VERSION,
            account_scope: AccountScope::from_chatgpt_email(" USER@Example.COM "),
            thread_id: Some("thr-1".to_owned()),
            model_id: Some("model-1".to_owned()),
            reasoning_effort: Some("high".to_owned()),
            thread_account_scopes: BTreeMap::from([(
                "thr-1".to_owned(),
                AccountScope::from_chatgpt_email("user@example.com").unwrap(),
            )]),
        };
        store.save(&preferences).unwrap();
        assert_eq!(store.load().unwrap().preferences, preferences);
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            fs::metadata(path.parent().unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
    }

    #[test]
    fn missing_corrupt_and_unknown_versions_are_clean_first_runs() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("preferences.json");
        let store = FilePreferences::new(&path);
        assert_eq!(store.load().unwrap().notice, Some(LoadNotice::Missing));
        fs::write(
            &path,
            br#"{"version":1,"account_scope":null,"thread_id":null,"model_id":null,"reasoning_effort":null}"#,
        )
        .unwrap();
        let prior_v1 = store.load().unwrap();
        assert_eq!(prior_v1.notice, None);
        assert!(prior_v1.preferences.thread_account_scopes.is_empty());
        fs::write(&path, b"{not-json").unwrap();
        let corrupt = store.load().unwrap();
        assert_eq!(corrupt.notice, Some(LoadNotice::Corrupt));
        assert!(!corrupt.may_overwrite);
        fs::write(&path, br#"{"version":99}"#).unwrap();
        let future = store.load().unwrap();
        assert_eq!(future.notice, Some(LoadNotice::UnsupportedVersion(99)));
        assert!(!future.may_overwrite);
    }

    #[test]
    fn rejects_semantically_corrupt_and_oversized_preferences_without_overwriting_them() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("preferences.json");
        let store = FilePreferences::new(&path);
        let malformed = [
            br#"{"version":1,"account_scope":{"kind":"chatgpt_email","value":" USER@example.com "},"thread_id":null,"model_id":null,"reasoning_effort":null}"#.as_slice(),
            br#"{"version":1,"account_scope":{"kind":"chatgpt_email","value":"a@example.com"},"thread_id":"thr","model_id":null,"reasoning_effort":null,"thread_account_scopes":{"thr":{"kind":"chatgpt_email","value":"b@example.com"}}}"#.as_slice(),
            br#"{"version":1,"account_scope":null,"thread_id":" ","model_id":null,"reasoning_effort":null}"#.as_slice(),
            br#"{"version":4294967297}"#.as_slice(),
        ];
        for bytes in malformed {
            fs::write(&path, bytes).unwrap();
            let outcome = store.load().unwrap();
            assert_eq!(outcome.notice, Some(LoadNotice::Corrupt));
            assert!(!outcome.may_overwrite);
        }

        fs::write(&path, vec![b' '; MAX_PREFERENCES_BYTES + 1]).unwrap();
        let oversized = store.load().unwrap();
        assert_eq!(oversized.notice, Some(LoadNotice::Corrupt));
        assert!(!oversized.may_overwrite);

        let valid = PreferencesV1::default();
        store.save(&valid).unwrap();
        let before = fs::read(&path).unwrap();
        let mut too_large = valid;
        too_large.model_id = Some("m".repeat(MAX_PREFERENCES_BYTES));
        assert!(store.save(&too_large).is_err());
        assert_eq!(fs::read(&path).unwrap(), before);
    }

    #[test]
    fn atomic_save_never_follows_a_predictable_legacy_temp_symlink() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("preferences.json");
        let victim = temp.path().join("victim.txt");
        fs::write(&victim, b"must remain unchanged").unwrap();
        let legacy_temp = temp
            .path()
            .join(format!(".preferences.json.{}.tmp", std::process::id()));
        symlink(&victim, &legacy_temp).unwrap();

        let store = FilePreferences::new(&path);
        store.save(&PreferencesV1::default()).unwrap();

        assert_eq!(fs::read(&victim).unwrap(), b"must remain unchanged");
        assert!(!fs::symlink_metadata(&path)
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(store.load().unwrap().preferences, PreferencesV1::default());
    }

    #[test]
    fn failed_atomic_replace_preserves_the_target_and_cleans_its_temp_file() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("preferences.json");
        fs::create_dir(&path).unwrap();
        fs::write(path.join("sentinel"), b"preserve me").unwrap();
        let store = FilePreferences::new(&path);

        assert!(store.save(&PreferencesV1::default()).is_err());

        assert_eq!(fs::read(path.join("sentinel")).unwrap(), b"preserve me");
        let temp_prefix = format!(".preferences.json.{}.", std::process::id());
        assert!(fs::read_dir(temp.path()).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(&temp_prefix)
        }));
    }

    #[test]
    fn account_scope_constructor_rejects_non_identity_and_normalizes_valid_email() {
        assert_eq!(
            AccountScope::from_chatgpt_email(" USER@Example.COM "),
            Some(AccountScope::ChatgptEmail("user@example.com".to_owned()))
        );
        for invalid in [
            "",
            "   ",
            "a b@example.com",
            "a\nb@example.com",
            "spoof\u{202e}@example.com",
        ] {
            assert_eq!(AccountScope::from_chatgpt_email(invalid), None);
        }
    }
}
