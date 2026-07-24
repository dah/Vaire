use std::ffi::CString;
use std::fs;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::storage::{DirectorySync, RealDirectorySync};

const LEGACY_NAME: &str = "AgentHarness";
const CURRENT_NAME: &str = "vaire";

#[derive(Debug, Error)]
pub(crate) enum SupportRootMigrationError {
    #[error("both legacy and current application-data roots exist; stop all Vairë and legacy processes, move one complete root aside, then restart")]
    Collision,
    #[error("the {root} application-data root is unsafe ({reason}); restore it as a current-user-owned directory with permissions 0700, then restart")]
    UnsafeRoot {
        root: &'static str,
        reason: &'static str,
    },
    #[error("could not inspect the {root} application-data root: {source}")]
    Inspect {
        root: &'static str,
        #[source]
        source: io::Error,
    },
    #[error("could not move the legacy application-data root to Vairë without replacing existing data: {source}")]
    Rename {
        #[source]
        source: io::Error,
    },
    #[error("the application-data root move committed but its result could not be verified; stop and inspect both support roots before restarting")]
    Verification,
    #[error("the application-data root move committed, but directory durability could not be verified: {source}; do not move it back automatically")]
    Durability {
        #[source]
        source: io::Error,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MigrationOutcome {
    FirstRun,
    Current,
    Migrated,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum EntryKind {
    Directory,
    Symlink,
    Other,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct RootMetadata {
    kind: EntryKind,
    uid: u32,
    mode: u32,
    device: u64,
    inode: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct RootIdentity {
    device: u64,
    inode: u64,
}

impl RootMetadata {
    fn identity(self) -> RootIdentity {
        RootIdentity {
            device: self.device,
            inode: self.inode,
        }
    }
}

trait MigrationFileSystem {
    fn metadata_no_follow(&self, path: &Path) -> io::Result<Option<RootMetadata>>;
    fn rename_exclusive(&self, from: &Path, to: &Path) -> io::Result<()>;
}

#[derive(Debug)]
struct RealMigrationFileSystem;

impl MigrationFileSystem for RealMigrationFileSystem {
    fn metadata_no_follow(&self, path: &Path) -> io::Result<Option<RootMetadata>> {
        match fs::symlink_metadata(path) {
            Ok(metadata) => {
                let file_type = metadata.file_type();
                let kind = if file_type.is_dir() {
                    EntryKind::Directory
                } else if file_type.is_symlink() {
                    EntryKind::Symlink
                } else {
                    EntryKind::Other
                };
                Ok(Some(RootMetadata {
                    kind,
                    uid: metadata.uid(),
                    mode: metadata.mode() & 0o7777,
                    device: metadata.dev(),
                    inode: metadata.ino(),
                }))
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }

    fn rename_exclusive(&self, from: &Path, to: &Path) -> io::Result<()> {
        rename_exclusive(from, to)
    }
}

pub(crate) fn migrate_support_root(home: &Path) -> Result<(), SupportRootMigrationError> {
    migrate_support_root_with(home, &RealMigrationFileSystem, &RealDirectorySync, unsafe {
        libc::geteuid()
    })
    .map(|_| ())
}

pub(crate) fn legacy_support_dir(home: &Path) -> PathBuf {
    support_parent(home).join(LEGACY_NAME)
}

fn migrate_support_root_with(
    home: &Path,
    file_system: &dyn MigrationFileSystem,
    directory_sync: &dyn DirectorySync,
    current_uid: u32,
) -> Result<MigrationOutcome, SupportRootMigrationError> {
    let parent = support_parent(home);
    let legacy = legacy_support_dir(home);
    let current = parent.join(CURRENT_NAME);
    let legacy_metadata = inspect(file_system, &legacy, "legacy")?;
    let current_metadata = inspect(file_system, &current, "current")?;

    match (legacy_metadata, current_metadata) {
        (None, None) => Ok(MigrationOutcome::FirstRun),
        (Some(_), Some(_)) => Err(SupportRootMigrationError::Collision),
        (None, Some(metadata)) => {
            validate(metadata, current_uid, "current")?;
            Ok(MigrationOutcome::Current)
        }
        (Some(metadata), None) => {
            validate(metadata, current_uid, "legacy")?;
            let source_identity = metadata.identity();
            if let Err(source) = file_system.rename_exclusive(&legacy, &current) {
                let legacy_after = inspect(file_system, &legacy, "legacy")?;
                let current_after = inspect(file_system, &current, "current")?;
                match (legacy_after, current_after) {
                    (Some(_), Some(_)) => return Err(SupportRootMigrationError::Collision),
                    (None, Some(current_metadata))
                        if validate(current_metadata, current_uid, "current").is_ok()
                            && current_metadata.identity() == source_identity =>
                    {
                        directory_sync
                            .sync(&parent)
                            .map_err(|source| SupportRootMigrationError::Durability { source })?;
                        return Ok(MigrationOutcome::Current);
                    }
                    _ => return Err(SupportRootMigrationError::Rename { source }),
                }
            }
            if !moved_source_is_current(
                file_system,
                &legacy,
                &current,
                current_uid,
                source_identity,
            )
            .unwrap_or(false)
            {
                return Err(SupportRootMigrationError::Verification);
            }
            directory_sync
                .sync(&parent)
                .map_err(|source| SupportRootMigrationError::Durability { source })?;
            Ok(MigrationOutcome::Migrated)
        }
    }
}

fn support_parent(home: &Path) -> PathBuf {
    home.join("Library").join("Application Support")
}

fn moved_source_is_current(
    file_system: &dyn MigrationFileSystem,
    legacy: &Path,
    current: &Path,
    current_uid: u32,
    source_identity: RootIdentity,
) -> Result<bool, SupportRootMigrationError> {
    let legacy = inspect(file_system, legacy, "legacy")?;
    let current = inspect(file_system, current, "current")?;
    match (legacy, current) {
        (None, Some(metadata)) => {
            validate(metadata, current_uid, "current")?;
            Ok(metadata.identity() == source_identity)
        }
        _ => Ok(false),
    }
}

fn inspect(
    file_system: &dyn MigrationFileSystem,
    path: &Path,
    root: &'static str,
) -> Result<Option<RootMetadata>, SupportRootMigrationError> {
    file_system
        .metadata_no_follow(path)
        .map_err(|source| SupportRootMigrationError::Inspect { root, source })
}

fn validate(
    metadata: RootMetadata,
    current_uid: u32,
    root: &'static str,
) -> Result<(), SupportRootMigrationError> {
    let reason = if metadata.kind == EntryKind::Symlink {
        Some("it is a symlink")
    } else if metadata.kind != EntryKind::Directory {
        Some("it is not a directory")
    } else if metadata.uid != current_uid {
        Some("it is not owned by the current user")
    } else if metadata.mode != 0o700 {
        Some("its permissions are not exactly 0700")
    } else {
        None
    };
    match reason {
        Some(reason) => Err(SupportRootMigrationError::UnsafeRoot { root, reason }),
        None => Ok(()),
    }
}

#[cfg(target_os = "macos")]
fn rename_exclusive(from: &Path, to: &Path) -> io::Result<()> {
    let from = path_c_string(from)?;
    let to = path_c_string(to)?;
    let result = unsafe {
        libc::renameatx_np(
            libc::AT_FDCWD,
            from.as_ptr(),
            libc::AT_FDCWD,
            to.as_ptr(),
            libc::RENAME_EXCL,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(not(target_os = "macos"))]
fn rename_exclusive(_from: &Path, _to: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "support-root migration is supported only on macOS",
    ))
}

fn path_c_string(path: &Path) -> io::Result<CString> {
    CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains a NUL byte"))
}

#[cfg(all(test, target_os = "macos"))]
mod tests;
