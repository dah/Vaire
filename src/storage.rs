use std::fmt;
use std::io;
use std::path::Path;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CommitStatus {
    Verified,
    CommittedUnverified,
}

pub(crate) trait DirectorySync: fmt::Debug + Send + Sync {
    fn sync(&self, path: &Path) -> io::Result<()>;
}

#[derive(Debug, Default)]
pub(crate) struct RealDirectorySync;

impl DirectorySync for RealDirectorySync {
    fn sync(&self, path: &Path) -> io::Result<()> {
        std::fs::OpenOptions::new()
            .read(true)
            .open(path)?
            .sync_all()
    }
}

#[cfg(test)]
#[derive(Debug)]
pub(crate) struct ScriptedDirectorySync {
    remaining_successes: std::sync::Mutex<usize>,
}

#[cfg(test)]
impl ScriptedDirectorySync {
    pub(crate) fn fail_after(successes: usize) -> Self {
        Self {
            remaining_successes: std::sync::Mutex::new(successes),
        }
    }
}

#[cfg(test)]
impl DirectorySync for ScriptedDirectorySync {
    fn sync(&self, path: &Path) -> io::Result<()> {
        let mut remaining = self
            .remaining_successes
            .lock()
            .map_err(|_| io::Error::other("directory-sync failpoint lock poisoned"))?;
        if *remaining == 0 {
            return Err(io::Error::other("injected directory sync failure"));
        }
        *remaining -= 1;
        RealDirectorySync.sync(path)
    }
}
