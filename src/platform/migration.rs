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
mod tests {
    use std::os::unix::fs::{symlink, PermissionsExt};
    use std::path::PathBuf;
    use std::sync::Mutex;

    use tempfile::TempDir;

    use super::*;
    use crate::storage::ScriptedDirectorySync;

    fn roots() -> (TempDir, PathBuf, PathBuf, PathBuf) {
        let home = tempfile::tempdir().unwrap();
        let parent = home.path().join("Library/Application Support");
        fs::create_dir_all(&parent).unwrap();
        let legacy = parent.join(LEGACY_NAME);
        let current = parent.join(CURRENT_NAME);
        (home, parent, legacy, current)
    }

    fn owner_only(path: &Path) {
        fs::create_dir(path).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    }

    fn run(home: &Path) -> Result<MigrationOutcome, SupportRootMigrationError> {
        migrate_support_root_with(home, &RealMigrationFileSystem, &RealDirectorySync, unsafe {
            libc::geteuid()
        })
    }

    #[test]
    fn first_run_creates_nothing() {
        let home = tempfile::tempdir().unwrap();
        let legacy = home
            .path()
            .join("Library/Application Support")
            .join(LEGACY_NAME);
        let current = home
            .path()
            .join("Library/Application Support")
            .join(CURRENT_NAME);
        assert_eq!(run(home.path()).unwrap(), MigrationOutcome::FirstRun);
        assert!(!legacy.exists());
        assert!(!current.exists());
        assert!(!home.path().join("Library").exists());
    }

    #[test]
    fn accepts_current_root_and_is_idempotent() {
        let (home, _, _, current) = roots();
        owner_only(&current);
        assert_eq!(run(home.path()).unwrap(), MigrationOutcome::Current);
        assert_eq!(run(home.path()).unwrap(), MigrationOutcome::Current);
    }

    #[test]
    fn moves_legacy_root_without_inspecting_nested_opaque_sentinel() {
        let (home, _, legacy, current) = roots();
        owner_only(&legacy);
        let nested = legacy.join("opaque");
        fs::create_dir(&nested).unwrap();
        let sentinel = nested.join("sentinel");
        fs::write(&sentinel, b"opaque bytes").unwrap();
        fs::set_permissions(&sentinel, fs::Permissions::from_mode(0o000)).unwrap();

        assert_eq!(run(home.path()).unwrap(), MigrationOutcome::Migrated);
        assert!(!legacy.exists());
        assert_eq!(
            fs::symlink_metadata(current.join("opaque/sentinel"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0
        );
    }

    #[test]
    fn rejects_both_roots_without_mutation() {
        let (home, _, legacy, current) = roots();
        owner_only(&legacy);
        owner_only(&current);
        assert!(matches!(
            run(home.path()),
            Err(SupportRootMigrationError::Collision)
        ));
        assert!(legacy.exists());
        assert!(current.exists());
    }

    #[test]
    fn rejects_symlink_file_and_wrong_mode_without_following() {
        let (home, _, legacy, current) = roots();
        let target = home.path().join("outside-target");
        owner_only(&target);
        symlink(&target, &legacy).unwrap();
        assert!(matches!(
            run(home.path()),
            Err(SupportRootMigrationError::UnsafeRoot { .. })
        ));
        fs::remove_file(&legacy).unwrap();
        fs::write(&legacy, b"file").unwrap();
        assert!(matches!(
            run(home.path()),
            Err(SupportRootMigrationError::UnsafeRoot { .. })
        ));
        fs::remove_file(&legacy).unwrap();
        owner_only(&legacy);
        fs::set_permissions(&legacy, fs::Permissions::from_mode(0o750)).unwrap();
        assert!(matches!(
            run(home.path()),
            Err(SupportRootMigrationError::UnsafeRoot { .. })
        ));
        assert!(!current.exists());
    }

    #[derive(Debug)]
    struct MetadataOverride {
        inner: RealMigrationFileSystem,
        uid: u32,
    }

    #[derive(Debug)]
    struct MetadataFailure;

    impl MigrationFileSystem for MetadataFailure {
        fn metadata_no_follow(&self, _path: &Path) -> io::Result<Option<RootMetadata>> {
            Err(io::Error::other("injected metadata failure"))
        }

        fn rename_exclusive(&self, _from: &Path, _to: &Path) -> io::Result<()> {
            unreachable!("metadata failure must prevent mutation")
        }
    }

    #[test]
    fn reports_injected_no_follow_metadata_failure_without_mutation() {
        let home = tempfile::tempdir().unwrap();
        assert!(matches!(
            migrate_support_root_with(home.path(), &MetadataFailure, &RealDirectorySync, unsafe {
                libc::geteuid()
            },),
            Err(SupportRootMigrationError::Inspect { .. })
        ));
        assert!(!home.path().join("Library").exists());
    }

    impl MigrationFileSystem for MetadataOverride {
        fn metadata_no_follow(&self, path: &Path) -> io::Result<Option<RootMetadata>> {
            let mut metadata = self.inner.metadata_no_follow(path)?;
            if let Some(metadata) = &mut metadata {
                metadata.uid = self.uid;
            }
            Ok(metadata)
        }

        fn rename_exclusive(&self, from: &Path, to: &Path) -> io::Result<()> {
            self.inner.rename_exclusive(from, to)
        }
    }

    #[test]
    fn rejects_wrong_owner_from_injected_metadata() {
        let (home, _, legacy, _) = roots();
        owner_only(&legacy);
        let uid = unsafe { libc::geteuid() };
        let file_system = MetadataOverride {
            inner: RealMigrationFileSystem,
            uid: uid.wrapping_add(1),
        };
        assert!(matches!(
            migrate_support_root_with(home.path(), &file_system, &RealDirectorySync, uid),
            Err(SupportRootMigrationError::UnsafeRoot { .. })
        ));
    }

    #[derive(Debug)]
    struct RenameFailure {
        winner_moves_first: bool,
        calls: Mutex<usize>,
    }

    #[derive(Debug)]
    struct RecordingDirectorySync {
        calls: Mutex<Vec<PathBuf>>,
        fail: bool,
    }

    impl RecordingDirectorySync {
        fn new(fail: bool) -> Self {
            Self {
                calls: Mutex::new(Vec::new()),
                fail,
            }
        }
    }

    impl DirectorySync for RecordingDirectorySync {
        fn sync(&self, path: &Path) -> io::Result<()> {
            self.calls.lock().unwrap().push(path.to_path_buf());
            if self.fail {
                Err(io::Error::other("injected directory sync failure"))
            } else {
                Ok(())
            }
        }
    }

    impl MigrationFileSystem for RenameFailure {
        fn metadata_no_follow(&self, path: &Path) -> io::Result<Option<RootMetadata>> {
            RealMigrationFileSystem.metadata_no_follow(path)
        }

        fn rename_exclusive(&self, from: &Path, to: &Path) -> io::Result<()> {
            *self.calls.lock().unwrap() += 1;
            if self.winner_moves_first {
                rename_exclusive(from, to)?;
            }
            Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "injected rename failure",
            ))
        }
    }

    #[test]
    fn reports_rename_failure_when_no_concurrent_winner() {
        let (home, _, legacy, _) = roots();
        owner_only(&legacy);
        let file_system = RenameFailure {
            winner_moves_first: false,
            calls: Mutex::new(0),
        };
        assert!(matches!(
            migrate_support_root_with(home.path(), &file_system, &RealDirectorySync, unsafe {
                libc::geteuid()
            },),
            Err(SupportRootMigrationError::Rename { .. })
        ));
    }

    #[derive(Debug)]
    struct CollisionAfterRenameFailure;

    impl MigrationFileSystem for CollisionAfterRenameFailure {
        fn metadata_no_follow(&self, path: &Path) -> io::Result<Option<RootMetadata>> {
            RealMigrationFileSystem.metadata_no_follow(path)
        }

        fn rename_exclusive(&self, _from: &Path, to: &Path) -> io::Result<()> {
            owner_only(to);
            Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "injected collision",
            ))
        }
    }

    #[test]
    fn failed_rename_with_both_roots_is_reclassified_as_collision() {
        let (home, _, legacy, current) = roots();
        owner_only(&legacy);
        assert!(matches!(
            migrate_support_root_with(
                home.path(),
                &CollisionAfterRenameFailure,
                &RealDirectorySync,
                unsafe { libc::geteuid() },
            ),
            Err(SupportRootMigrationError::Collision)
        ));
        assert!(legacy.exists());
        assert!(current.exists());
    }

    #[derive(Debug)]
    struct IdentityMismatchAfterRenameFailure;

    impl MigrationFileSystem for IdentityMismatchAfterRenameFailure {
        fn metadata_no_follow(&self, path: &Path) -> io::Result<Option<RootMetadata>> {
            RealMigrationFileSystem.metadata_no_follow(path)
        }

        fn rename_exclusive(&self, from: &Path, to: &Path) -> io::Result<()> {
            let replacement = to.with_file_name("replacement");
            owner_only(&replacement);
            fs::remove_dir(from)?;
            rename_exclusive(&replacement, to)?;
            Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "injected mismatched winner",
            ))
        }
    }

    #[test]
    fn concurrent_winner_requires_the_original_source_identity() {
        let (home, _, legacy, current) = roots();
        owner_only(&legacy);
        let source_identity = RealMigrationFileSystem
            .metadata_no_follow(&legacy)
            .unwrap()
            .unwrap()
            .identity();
        assert!(matches!(
            migrate_support_root_with(
                home.path(),
                &IdentityMismatchAfterRenameFailure,
                &RealDirectorySync,
                unsafe { libc::geteuid() },
            ),
            Err(SupportRootMigrationError::Rename { .. })
        ));
        let current_identity = RealMigrationFileSystem
            .metadata_no_follow(&current)
            .unwrap()
            .unwrap()
            .identity();
        assert_ne!(current_identity, source_identity);
    }

    #[test]
    fn reclassifies_a_concurrent_winner_as_current_after_synchronizing_the_parent() {
        let (home, parent, legacy, current) = roots();
        owner_only(&legacy);
        let file_system = RenameFailure {
            winner_moves_first: true,
            calls: Mutex::new(0),
        };
        let directory_sync = RecordingDirectorySync::new(false);
        assert_eq!(
            migrate_support_root_with(home.path(), &file_system, &directory_sync, unsafe {
                libc::geteuid()
            },)
            .unwrap(),
            MigrationOutcome::Current
        );
        assert_eq!(*directory_sync.calls.lock().unwrap(), vec![parent]);
        assert!(!legacy.exists());
        assert!(current.exists());
    }

    #[test]
    fn concurrent_winner_sync_failure_reports_unverified_durability_without_rollback() {
        let (home, parent, legacy, current) = roots();
        owner_only(&legacy);
        let file_system = RenameFailure {
            winner_moves_first: true,
            calls: Mutex::new(0),
        };
        let directory_sync = RecordingDirectorySync::new(true);

        assert!(matches!(
            migrate_support_root_with(home.path(), &file_system, &directory_sync, unsafe {
                libc::geteuid()
            },),
            Err(SupportRootMigrationError::Durability { .. })
        ));
        assert_eq!(*directory_sync.calls.lock().unwrap(), vec![parent]);
        assert!(!legacy.exists());
        assert!(current.exists());
    }

    #[test]
    fn reports_committed_but_unverified_durability_without_rollback() {
        let (home, _, legacy, current) = roots();
        owner_only(&legacy);
        let result = migrate_support_root_with(
            home.path(),
            &RealMigrationFileSystem,
            &ScriptedDirectorySync::fail_after(0),
            unsafe { libc::geteuid() },
        );
        assert!(matches!(
            result,
            Err(SupportRootMigrationError::Durability { .. })
        ));
        assert!(!legacy.exists());
        assert!(current.exists());
    }
}
