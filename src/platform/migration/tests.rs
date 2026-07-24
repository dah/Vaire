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

mod durability;
mod rename_races;
mod validation;
