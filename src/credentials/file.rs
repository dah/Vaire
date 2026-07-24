use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use super::{
    CredentialAccount, CredentialFailureCategory, CredentialStore, CredentialStoreError,
    SecretValue, MAX_CREDENTIAL_BYTES,
};
use crate::storage::{CommitStatus, DirectorySync, RealDirectorySync};

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug)]
pub struct FileCredentialStore {
    account: CredentialAccount,
    home_dir: PathBuf,
    credential_file: PathBuf,
    directory_sync: Arc<dyn DirectorySync>,
}

impl FileCredentialStore {
    pub fn new(
        account: CredentialAccount,
        home_dir: impl Into<PathBuf>,
        credential_file: impl Into<PathBuf>,
    ) -> Result<Self, CredentialStoreError> {
        let store = Self {
            account,
            home_dir: home_dir.into(),
            credential_file: credential_file.into(),
            directory_sync: Arc::new(RealDirectorySync),
        };
        if store.credential_file.parent() != Some(store.home_dir.as_path()) {
            return Err(CredentialStoreError::new(
                CredentialFailureCategory::Permissions,
            ));
        }
        store.initialize_directory()?;
        store.cleanup_orphans()?;
        Ok(store)
    }

    #[cfg(test)]
    pub(crate) fn with_directory_sync(
        account: CredentialAccount,
        home_dir: impl Into<PathBuf>,
        credential_file: impl Into<PathBuf>,
        directory_sync: Arc<dyn DirectorySync>,
    ) -> Result<Self, CredentialStoreError> {
        let mut store = Self::new(account, home_dir, credential_file)?;
        store.directory_sync = directory_sync;
        Ok(store)
    }

    fn initialize_directory(&self) -> Result<(), CredentialStoreError> {
        match fs::symlink_metadata(&self.home_dir) {
            Ok(metadata) => return validate_directory(&metadata),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(_) => {
                return Err(CredentialStoreError::new(CredentialFailureCategory::Write));
            }
        }

        let parent = self
            .home_dir
            .parent()
            .ok_or_else(|| CredentialStoreError::new(CredentialFailureCategory::Write))?;
        fs::create_dir_all(parent)
            .map_err(|_| CredentialStoreError::new(CredentialFailureCategory::Write))?;
        match DirBuilder::new().mode(0o700).create(&self.home_dir) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(_) => {
                return Err(CredentialStoreError::new(CredentialFailureCategory::Write));
            }
        }
        let metadata = fs::symlink_metadata(&self.home_dir)
            .map_err(|_| CredentialStoreError::new(CredentialFailureCategory::Write))?;
        validate_directory(&metadata)
    }

    fn validate_home(&self) -> Result<(), CredentialStoreError> {
        let metadata = fs::symlink_metadata(&self.home_dir)
            .map_err(|_| CredentialStoreError::new(CredentialFailureCategory::Permissions))?;
        validate_directory(&metadata)
    }

    fn cleanup_orphans(&self) -> Result<(), CredentialStoreError> {
        self.validate_home()?;
        let file_name = self
            .credential_file
            .file_name()
            .and_then(|value| value.to_str())
            .ok_or_else(|| CredentialStoreError::new(CredentialFailureCategory::Permissions))?;
        let prefix = format!(".{file_name}.");
        let entries = fs::read_dir(&self.home_dir)
            .map_err(|_| CredentialStoreError::new(CredentialFailureCategory::Delete))?;
        for entry in entries {
            let entry =
                entry.map_err(|_| CredentialStoreError::new(CredentialFailureCategory::Delete))?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            let Some(temporary_identity) = name
                .strip_prefix(&prefix)
                .and_then(|name| name.strip_suffix(".tmp"))
            else {
                continue;
            };
            let mut parts = temporary_identity.split('.');
            let recognized = parts.next().is_some_and(|part| {
                !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit())
            }) && parts.next().is_some_and(|part| {
                !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit())
            }) && parts.next().is_none();
            if !recognized {
                continue;
            }
            let metadata = fs::symlink_metadata(entry.path())
                .map_err(|_| CredentialStoreError::new(CredentialFailureCategory::Delete))?;
            if metadata.file_type().is_dir() {
                return Err(CredentialStoreError::new(CredentialFailureCategory::Delete));
            }
            fs::remove_file(entry.path())
                .map_err(|_| CredentialStoreError::new(CredentialFailureCategory::Delete))?;
        }
        let _ = self.directory_sync.sync(&self.home_dir);
        Ok(())
    }

    fn path_for(&self, account: CredentialAccount) -> Result<&Path, CredentialStoreError> {
        if account != self.account {
            return Err(CredentialStoreError::new(
                CredentialFailureCategory::Permissions,
            ));
        }
        Ok(&self.credential_file)
    }

    fn create_temp_file(&self) -> Result<(PathBuf, File), CredentialStoreError> {
        let file_name = self
            .credential_file
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("api-key");
        for _ in 0..128 {
            let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let path = self.home_dir.join(format!(
                ".{file_name}.{}.{}.tmp",
                std::process::id(),
                sequence
            ));
            match OpenOptions::new()
                .create_new(true)
                .write(true)
                .mode(0o600)
                .open(&path)
            {
                Ok(file) => return Ok((path, file)),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(_) => {
                    return Err(CredentialStoreError::new(CredentialFailureCategory::Write));
                }
            }
        }
        Err(CredentialStoreError::new(CredentialFailureCategory::Write))
    }
}

impl CredentialStore for FileCredentialStore {
    fn load(
        &self,
        account: CredentialAccount,
    ) -> Result<Option<SecretValue>, CredentialStoreError> {
        self.validate_home()?;
        let path = self.path_for(account)?;
        let metadata = match fs::symlink_metadata(path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(_) => {
                return Err(CredentialStoreError::new(CredentialFailureCategory::Read));
            }
        };
        validate_credential_file(&metadata)?;

        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)
            .map_err(|_| CredentialStoreError::new(CredentialFailureCategory::Read))?;
        let opened_metadata = file
            .metadata()
            .map_err(|_| CredentialStoreError::new(CredentialFailureCategory::Read))?;
        validate_credential_file(&opened_metadata)?;

        let mut bytes = Vec::new();
        file.take((MAX_CREDENTIAL_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|_| CredentialStoreError::new(CredentialFailureCategory::Read))?;
        if bytes.len() > MAX_CREDENTIAL_BYTES {
            return Err(CredentialStoreError::new(
                CredentialFailureCategory::Corrupt,
            ));
        }
        SecretValue::from_stored_bytes(bytes).map(Some)
    }

    fn replace(
        &self,
        account: CredentialAccount,
        value: SecretValue,
    ) -> Result<(), CredentialStoreError> {
        self.replace_with_commit(account, value).map(|_| ())
    }

    fn replace_with_commit(
        &self,
        account: CredentialAccount,
        value: SecretValue,
    ) -> Result<CommitStatus, CredentialStoreError> {
        self.validate_home()?;
        let target = self.path_for(account)?;
        match fs::symlink_metadata(target) {
            Ok(metadata) => validate_credential_file(&metadata)?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(_) => {
                return Err(CredentialStoreError::new(CredentialFailureCategory::Write));
            }
        }

        let (temporary_path, mut file) = self.create_temp_file()?;
        let precommit = (|| -> Result<(), CredentialStoreError> {
            file.set_permissions(fs::Permissions::from_mode(0o600))
                .map_err(|_| CredentialStoreError::new(CredentialFailureCategory::Write))?;
            file.write_all(value.expose_bytes())
                .map_err(|_| CredentialStoreError::new(CredentialFailureCategory::Write))?;
            file.sync_all()
                .map_err(|_| CredentialStoreError::new(CredentialFailureCategory::Write))?;
            fs::rename(&temporary_path, target)
                .map_err(|_| CredentialStoreError::new(CredentialFailureCategory::Write))?;
            Ok(())
        })();
        if let Err(error) = precommit {
            let _ = fs::remove_file(&temporary_path);
            return Err(error);
        }
        Ok(match self.directory_sync.sync(&self.home_dir) {
            Ok(()) => CommitStatus::Verified,
            Err(_) if fs::read(target).is_ok_and(|written| written == value.expose_bytes()) => {
                CommitStatus::CommittedUnverified
            }
            Err(_) => return Err(CredentialStoreError::new(CredentialFailureCategory::Write)),
        })
    }

    fn delete(&self, account: CredentialAccount) -> Result<(), CredentialStoreError> {
        self.delete_with_commit(account).map(|_| ())
    }

    fn delete_with_commit(
        &self,
        account: CredentialAccount,
    ) -> Result<CommitStatus, CredentialStoreError> {
        self.validate_home()?;
        let path = self.path_for(account)?;
        let metadata = match fs::symlink_metadata(path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(CommitStatus::Verified)
            }
            Err(_) => {
                return Err(CredentialStoreError::new(CredentialFailureCategory::Delete));
            }
        };
        validate_credential_file(&metadata)?;
        fs::remove_file(path)
            .map_err(|_| CredentialStoreError::new(CredentialFailureCategory::Delete))?;
        Ok(match self.directory_sync.sync(&self.home_dir) {
            Ok(()) => CommitStatus::Verified,
            Err(_) if matches!(fs::symlink_metadata(path), Err(error) if error.kind() == io::ErrorKind::NotFound) => {
                CommitStatus::CommittedUnverified
            }
            Err(_) => return Err(CredentialStoreError::new(CredentialFailureCategory::Delete)),
        })
    }
}

fn current_uid() -> u32 {
    // SAFETY: geteuid has no preconditions and does not retain pointers.
    unsafe { libc::geteuid() }
}

fn validate_directory(metadata: &fs::Metadata) -> Result<(), CredentialStoreError> {
    if !metadata.file_type().is_dir()
        || metadata.uid() != current_uid()
        || metadata.mode() & 0o7777 != 0o700
    {
        return Err(CredentialStoreError::new(
            CredentialFailureCategory::Permissions,
        ));
    }
    Ok(())
}

fn validate_credential_file(metadata: &fs::Metadata) -> Result<(), CredentialStoreError> {
    if !metadata.file_type().is_file()
        || metadata.uid() != current_uid()
        || metadata.mode() & 0o7777 != 0o600
    {
        return Err(CredentialStoreError::new(
            CredentialFailureCategory::Permissions,
        ));
    }
    Ok(())
}
