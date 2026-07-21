use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const PREFERENCES_VERSION: u32 = 1;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum AccountScope {
    ChatgptEmail(String),
}

impl AccountScope {
    pub fn from_chatgpt_email(email: &str) -> Option<Self> {
        let normalized = email.trim().to_ascii_lowercase();
        (!normalized.is_empty()).then_some(Self::ChatgptEmail(normalized))
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PreferencesV1 {
    pub version: u32,
    pub account_scope: Option<AccountScope>,
    pub thread_id: Option<String>,
    pub model_id: Option<String>,
    pub reasoning_effort: Option<String>,
}

impl Default for PreferencesV1 {
    fn default() -> Self {
        Self {
            version: PREFERENCES_VERSION,
            account_scope: None,
            thread_id: None,
            model_id: None,
            reasoning_effort: None,
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
}

impl PreferencesPort for FilePreferences {
    fn load(&self) -> Result<LoadOutcome, PersistenceError> {
        let bytes = match fs::read(&self.path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(LoadOutcome {
                    preferences: PreferencesV1::default(),
                    notice: Some(LoadNotice::Missing),
                    may_overwrite: true,
                });
            }
            Err(error) => return Err(error.into()),
        };

        let raw: serde_json::Value = match serde_json::from_slice(&bytes) {
            Ok(value) => value,
            Err(_) => {
                return Ok(LoadOutcome {
                    preferences: PreferencesV1::default(),
                    notice: Some(LoadNotice::Corrupt),
                    may_overwrite: false,
                })
            }
        };
        let version = raw
            .get("version")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0) as u32;
        if version != PREFERENCES_VERSION {
            return Ok(LoadOutcome {
                preferences: PreferencesV1::default(),
                notice: Some(LoadNotice::UnsupportedVersion(version)),
                may_overwrite: false,
            });
        }

        match serde_json::from_value(raw) {
            Ok(preferences) => Ok(LoadOutcome {
                preferences,
                notice: None,
                may_overwrite: true,
            }),
            Err(_) => Ok(LoadOutcome {
                preferences: PreferencesV1::default(),
                notice: Some(LoadNotice::Corrupt),
                may_overwrite: false,
            }),
        }
    }

    fn save(&self, preferences: &PreferencesV1) -> Result<(), PersistenceError> {
        if preferences.version != PREFERENCES_VERSION {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "cannot write an unsupported preferences version",
            )
            .into());
        }
        let parent = self.parent()?;
        fs::create_dir_all(parent)?;
        fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
        let temp_path = parent.join(format!(
            ".{}.{}.tmp",
            self.path
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or("preferences"),
            std::process::id()
        ));
        let bytes = serde_json::to_vec_pretty(preferences)?;
        let write_result = (|| -> io::Result<()> {
            let mut file = OpenOptions::new()
                .create(true)
                .truncate(true)
                .write(true)
                .mode(0o600)
                .open(&temp_path)?;
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
        PREFERENCES_VERSION,
    };
    use std::fs;
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
        fs::write(&path, b"{not-json").unwrap();
        let corrupt = store.load().unwrap();
        assert_eq!(corrupt.notice, Some(LoadNotice::Corrupt));
        assert!(!corrupt.may_overwrite);
        fs::write(&path, br#"{"version":99}"#).unwrap();
        let future = store.load().unwrap();
        assert_eq!(future.notice, Some(LoadNotice::UnsupportedVersion(99)));
        assert!(!future.may_overwrite);
    }
}
