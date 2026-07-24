use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use crate::storage::{CommitStatus, DirectorySync, RealDirectorySync};

use super::domain::{
    LoadNotice, LoadOutcome, PersistenceError, PreferencesPort, PreferencesV3,
    LEGACY_PREFERENCES_VERSION, MAX_PREFERENCES_BYTES, PREFERENCES_VERSION, V2_PREFERENCES_VERSION,
};
use super::validation::{corrupt_load_outcome, valid_legacy, valid_preferences, valid_v2};

static TEMP_FILE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

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
    pub(super) fn with_directory_sync(
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
impl PreferencesPort for FilePreferences {
    fn load(&self) -> Result<LoadOutcome, PersistenceError> {
        let file = match File::open(&self.path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(LoadOutcome {
                    preferences: PreferencesV3::default(),
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
                    preferences: PreferencesV3::from(preferences),
                    notice: Some(LoadNotice::MigratedV1),
                    may_overwrite: true,
                    needs_save: true,
                }),
                Ok(_) | Err(_) => Ok(corrupt_load_outcome()),
            },
            V2_PREFERENCES_VERSION => match serde_json::from_value(raw) {
                Ok(preferences) if valid_v2(&preferences) => Ok(LoadOutcome {
                    preferences: PreferencesV3::from(preferences),
                    notice: Some(LoadNotice::MigratedV2),
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
                preferences: PreferencesV3::default(),
                notice: Some(LoadNotice::UnsupportedVersion(version)),
                may_overwrite: false,
                needs_save: false,
            }),
        }
    }

    fn save(&self, preferences: &PreferencesV3) -> Result<(), PersistenceError> {
        self.save_with_commit(preferences).map(|_| ())
    }

    fn save_with_commit(
        &self,
        preferences: &PreferencesV3,
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
