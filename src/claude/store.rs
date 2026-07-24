use std::collections::HashSet;
use std::fs::{self, DirBuilder, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::provider::{ClaudeSessionId, ClaudeTurnId};
use crate::storage::CommitStatus;
use crate::text::sanitize_terminal_text;

use super::{ClaudeSessionLifecycle, ClaudeSessionSummary, ClaudeSessionV1, ClaudeTurnOutcome};

const SESSION_VERSION: u32 = 1;
const MAX_SESSIONS: usize = 50;
const MAX_SESSION_BYTES: usize = 1024 * 1024;
const MAX_AGGREGATE_SESSION_BYTES: u64 = 16 * 1024 * 1024;
const MAX_INDEX_BYTES: usize = 256 * 1024;
const MAX_TURNS: usize = 1024;
const MAX_USER_BYTES: usize = 128 * 1024;
const MAX_ASSISTANT_BYTES: usize = 256 * 1024;
const MAX_DISPLAY_TEXT_BYTES: usize = 768 * 1024;
const MAX_TITLE_BYTES: usize = 256;
const MAX_MODEL_ID_BYTES: usize = 512;
const MAX_MODEL_NAME_BYTES: usize = 1024;
static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum ClaudeStoreError {
    #[error("Claude session store read failed")]
    Read,
    #[error("Claude session store write failed")]
    Write,
    #[error("Claude session store delete failed")]
    Delete,
    #[error("Claude session store permissions are invalid")]
    Permissions,
    #[error("Claude session store data is corrupt")]
    Corrupt,
    #[error("Claude session store version is unsupported")]
    UnsupportedVersion,
    #[error("Claude session store resource limit exceeded")]
    ResourceLimit,
    #[error("Claude session was not found")]
    NotFound,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ClaudeSessionCommit {
    pub source: CommitStatus,
    pub index: Option<CommitStatus>,
}

pub trait ClaudeSessionStore: Send + Sync {
    fn list_sessions(&self) -> Result<Vec<ClaudeSessionSummary>, ClaudeStoreError>;
    /// Loads a session at a recovery boundary, repairing work left live by a prior process.
    fn load_session(&self, id: &ClaudeSessionId) -> Result<ClaudeSessionV1, ClaudeStoreError>;
    /// Loads a session for an in-process update without applying crash recovery.
    ///
    /// Existing store implementations retain their prior behavior by default. Stores that can
    /// distinguish recovery reads from live mutations should override this method.
    fn load_session_for_update(
        &self,
        id: &ClaudeSessionId,
    ) -> Result<ClaudeSessionV1, ClaudeStoreError> {
        self.load_session(id)
    }
    fn save_session(&self, session: &ClaudeSessionV1) -> Result<(), ClaudeStoreError>;
    fn save_session_with_commit(
        &self,
        session: &ClaudeSessionV1,
    ) -> Result<ClaudeSessionCommit, ClaudeStoreError> {
        self.save_session(session)?;
        Ok(ClaudeSessionCommit {
            source: CommitStatus::Verified,
            index: Some(CommitStatus::Verified),
        })
    }
    fn delete_session(&self, id: &ClaudeSessionId) -> Result<(), ClaudeStoreError>;
}

#[derive(Debug)]
pub struct FileClaudeSessionStore {
    root: PathBuf,
    sessions: PathBuf,
    lock: Mutex<()>,
}

impl FileClaudeSessionStore {
    pub fn new(root: impl Into<PathBuf>) -> Result<Self, ClaudeStoreError> {
        let root = root.into();
        secure_directory(&root)?;
        let sessions = root.join("sessions");
        secure_directory(&sessions)?;
        let store = Self {
            root,
            sessions,
            lock: Mutex::new(()),
        };
        let _ = store.rebuild_index()?;
        Ok(store)
    }

    fn session_path(&self, id: &ClaudeSessionId) -> PathBuf {
        self.sessions.join(format!("{}.json", id.as_str()))
    }

    fn index_path(&self) -> PathBuf {
        self.root.join("index.json")
    }

    fn scan_sessions(&self) -> Result<Vec<ClaudeSessionSummary>, ClaudeStoreError> {
        let mut summaries = Vec::new();
        for entry in fs::read_dir(&self.sessions).map_err(|_| ClaudeStoreError::Read)? {
            let entry = entry.map_err(|_| ClaudeStoreError::Read)?;
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            let Some(stem) = name.strip_suffix(".json") else {
                continue;
            };
            let Ok(id) = stem.parse::<ClaudeSessionId>() else {
                continue;
            };
            let bytes = read_file_limited(&entry.path(), MAX_SESSION_BYTES)?;
            let session = decode_session(&bytes, &id)?;
            summaries.push(summary(&session));
            if summaries.len() > MAX_SESSIONS {
                return Err(ClaudeStoreError::ResourceLimit);
            }
        }
        summaries.sort_by(|left, right| {
            right
                .updated_at_ms
                .cmp(&left.updated_at_ms)
                .then_with(|| left.session_id.cmp(&right.session_id))
        });
        Ok(summaries)
    }

    fn rebuild_index(&self) -> Result<Vec<ClaudeSessionSummary>, ClaudeStoreError> {
        let summaries = self.scan_sessions()?;
        let index = SessionIndex {
            version: 1,
            sessions: summaries.clone(),
        };
        let bytes = serde_json::to_vec_pretty(&index).map_err(|_| ClaudeStoreError::Corrupt)?;
        write_atomic(&self.root, &self.index_path(), &bytes, MAX_INDEX_BYTES)?;
        Ok(summaries)
    }

    fn read_session(&self, id: &ClaudeSessionId) -> Result<ClaudeSessionV1, ClaudeStoreError> {
        let path = self.session_path(id);
        let bytes = read_file_limited(&path, MAX_SESSION_BYTES)?;
        decode_session(&bytes, id)
    }

    fn write_session(
        &self,
        session: &ClaudeSessionV1,
    ) -> Result<ClaudeSessionCommit, ClaudeStoreError> {
        validate_session(session)?;
        let bytes = serde_json::to_vec_pretty(session).map_err(|_| ClaudeStoreError::Corrupt)?;
        if bytes.len() > MAX_SESSION_BYTES {
            return Err(ClaudeStoreError::ResourceLimit);
        }
        let target = self.session_path(&session.session_id);
        let (old_len, exists) = match fs::symlink_metadata(&target) {
            Ok(metadata) => {
                validate_file(&metadata)?;
                let old = read_file_limited(&target, MAX_SESSION_BYTES)?;
                decode_session(&old, &session.session_id)?;
                (metadata.len(), true)
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => (0, false),
            Err(_) => return Err(ClaudeStoreError::Write),
        };
        if !exists && self.scan_sessions()?.len() >= MAX_SESSIONS {
            return Err(ClaudeStoreError::ResourceLimit);
        }
        let aggregate = aggregate_bytes(&self.sessions)?;
        if aggregate
            .saturating_sub(old_len)
            .saturating_add(bytes.len() as u64)
            > MAX_AGGREGATE_SESSION_BYTES
        {
            return Err(ClaudeStoreError::ResourceLimit);
        }
        let source = write_atomic(&self.sessions, &target, &bytes, MAX_SESSION_BYTES)?;
        let index = self.rebuild_index().ok().map(|_| CommitStatus::Verified);
        Ok(ClaudeSessionCommit { source, index })
    }
}

impl ClaudeSessionStore for FileClaudeSessionStore {
    fn list_sessions(&self) -> Result<Vec<ClaudeSessionSummary>, ClaudeStoreError> {
        let _guard = self.lock.lock().map_err(|_| ClaudeStoreError::Corrupt)?;
        let summaries = self.scan_sessions()?;
        let _ = self.rebuild_index();
        Ok(summaries)
    }

    fn load_session(&self, id: &ClaudeSessionId) -> Result<ClaudeSessionV1, ClaudeStoreError> {
        let _guard = self.lock.lock().map_err(|_| ClaudeStoreError::Corrupt)?;
        let mut session = self.read_session(id)?;
        let mut repaired = false;
        if session.lifecycle == ClaudeSessionLifecycle::CreationPending {
            session.lifecycle = ClaudeSessionLifecycle::CreationUncertain;
            repaired = true;
        }
        for turn in &mut session.turns {
            if turn.outcome == ClaudeTurnOutcome::InProgress {
                turn.outcome = ClaudeTurnOutcome::Interrupted;
                turn.assistant_text = None;
                turn.incomplete_assistant_text = None;
                repaired = true;
            }
        }
        if repaired {
            self.write_session(&session)?;
        }
        Ok(session)
    }

    fn load_session_for_update(
        &self,
        id: &ClaudeSessionId,
    ) -> Result<ClaudeSessionV1, ClaudeStoreError> {
        let _guard = self.lock.lock().map_err(|_| ClaudeStoreError::Corrupt)?;
        self.read_session(id)
    }

    fn save_session(&self, session: &ClaudeSessionV1) -> Result<(), ClaudeStoreError> {
        self.save_session_with_commit(session).map(|_| ())
    }

    fn save_session_with_commit(
        &self,
        session: &ClaudeSessionV1,
    ) -> Result<ClaudeSessionCommit, ClaudeStoreError> {
        let _guard = self.lock.lock().map_err(|_| ClaudeStoreError::Corrupt)?;
        self.write_session(session)
    }

    fn delete_session(&self, id: &ClaudeSessionId) -> Result<(), ClaudeStoreError> {
        let _guard = self.lock.lock().map_err(|_| ClaudeStoreError::Corrupt)?;
        let path = self.session_path(id);
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Err(ClaudeStoreError::NotFound)
            }
            Err(_) => return Err(ClaudeStoreError::Delete),
        };
        validate_file(&metadata)?;
        fs::remove_file(&path).map_err(|_| ClaudeStoreError::Delete)?;
        sync_directory(&self.sessions).map_err(|_| ClaudeStoreError::Delete)?;
        let _ = self.rebuild_index();
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct SessionIndex {
    version: u32,
    sessions: Vec<ClaudeSessionSummary>,
}

fn decode_session(
    bytes: &[u8],
    expected_id: &ClaudeSessionId,
) -> Result<ClaudeSessionV1, ClaudeStoreError> {
    #[derive(Deserialize)]
    struct Version {
        version: u32,
    }
    let version: Version = serde_json::from_slice(bytes).map_err(|_| ClaudeStoreError::Corrupt)?;
    if version.version != SESSION_VERSION {
        return Err(ClaudeStoreError::UnsupportedVersion);
    }
    let session: ClaudeSessionV1 =
        serde_json::from_slice(bytes).map_err(|_| ClaudeStoreError::Corrupt)?;
    if &session.session_id != expected_id {
        return Err(ClaudeStoreError::Corrupt);
    }
    validate_session(&session)?;
    Ok(session)
}

fn validate_session(session: &ClaudeSessionV1) -> Result<(), ClaudeStoreError> {
    if session.version != SESSION_VERSION
        || session.updated_at_ms < session.created_at_ms
        || session.title.len() > MAX_TITLE_BYTES
        || session.title.contains(['\n', '\r'])
        || sanitize_terminal_text(&session.title) != session.title
        || session.turns.len() > MAX_TURNS
    {
        return Err(ClaudeStoreError::Corrupt);
    }
    if let Some(model) = &session.resolved_model {
        validate_plain_text(&model.id, MAX_MODEL_ID_BYTES, false)?;
        if let Some(name) = &model.display_name {
            validate_plain_text(name, MAX_MODEL_NAME_BYTES, false)?;
        }
    }
    let mut turn_ids: HashSet<&ClaudeTurnId> = HashSet::with_capacity(session.turns.len());
    let mut display_bytes = 0usize;
    for turn in &session.turns {
        if !turn_ids.insert(&turn.id) || turn.user_text.is_empty() {
            return Err(ClaudeStoreError::Corrupt);
        }
        if turn.user_text.len() > MAX_USER_BYTES {
            return Err(ClaudeStoreError::ResourceLimit);
        }
        display_bytes = display_bytes.saturating_add(turn.user_text.len());
        match turn.outcome {
            ClaudeTurnOutcome::Completed => {
                let assistant = turn
                    .assistant_text
                    .as_deref()
                    .ok_or(ClaudeStoreError::Corrupt)?;
                if assistant.len() > MAX_ASSISTANT_BYTES || turn.incomplete_assistant_text.is_some()
                {
                    return Err(ClaudeStoreError::ResourceLimit);
                }
                display_bytes = display_bytes.saturating_add(assistant.len());
            }
            ClaudeTurnOutcome::Failed => {
                if turn.assistant_text.is_some() {
                    return Err(ClaudeStoreError::Corrupt);
                }
                if let Some(incomplete) = &turn.incomplete_assistant_text {
                    if incomplete.is_empty() || incomplete.len() > MAX_ASSISTANT_BYTES {
                        return Err(ClaudeStoreError::ResourceLimit);
                    }
                    display_bytes = display_bytes.saturating_add(incomplete.len());
                }
            }
            ClaudeTurnOutcome::InProgress | ClaudeTurnOutcome::Interrupted => {
                if turn.assistant_text.is_some() || turn.incomplete_assistant_text.is_some() {
                    return Err(ClaudeStoreError::Corrupt);
                }
            }
        }
        if display_bytes > MAX_DISPLAY_TEXT_BYTES {
            return Err(ClaudeStoreError::ResourceLimit);
        }
    }
    Ok(())
}

fn validate_plain_text(value: &str, max: usize, allow_empty: bool) -> Result<(), ClaudeStoreError> {
    if (!allow_empty && value.is_empty())
        || value.len() > max
        || value.chars().any(char::is_control)
    {
        Err(ClaudeStoreError::Corrupt)
    } else {
        Ok(())
    }
}

fn summary(session: &ClaudeSessionV1) -> ClaudeSessionSummary {
    ClaudeSessionSummary {
        session_id: session.session_id.clone(),
        created_at_ms: session.created_at_ms,
        updated_at_ms: session.updated_at_ms,
        title: session.title.clone(),
        turn_count: session.turns.len(),
    }
}

fn secure_directory(path: &Path) -> Result<(), ClaudeStoreError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => return validate_directory(&metadata),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(_) => return Err(ClaudeStoreError::Write),
    }
    let parent = path.parent().ok_or(ClaudeStoreError::Write)?;
    fs::create_dir_all(parent).map_err(|_| ClaudeStoreError::Write)?;
    match DirBuilder::new().mode(0o700).create(path) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(_) => return Err(ClaudeStoreError::Write),
    }
    validate_directory(&fs::symlink_metadata(path).map_err(|_| ClaudeStoreError::Write)?)
}

fn validate_directory(metadata: &fs::Metadata) -> Result<(), ClaudeStoreError> {
    if metadata.file_type().is_dir()
        && metadata.uid() == current_uid()
        && metadata.mode() & 0o7777 == 0o700
    {
        Ok(())
    } else {
        Err(ClaudeStoreError::Permissions)
    }
}

fn validate_file(metadata: &fs::Metadata) -> Result<(), ClaudeStoreError> {
    if metadata.file_type().is_file()
        && metadata.uid() == current_uid()
        && metadata.mode() & 0o7777 == 0o600
    {
        Ok(())
    } else {
        Err(ClaudeStoreError::Permissions)
    }
}

fn read_file_limited(path: &Path, limit: usize) -> Result<Vec<u8>, ClaudeStoreError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Err(ClaudeStoreError::NotFound)
        }
        Err(_) => return Err(ClaudeStoreError::Read),
    };
    validate_file(&metadata)?;
    if metadata.len() > limit as u64 {
        return Err(ClaudeStoreError::ResourceLimit);
    }
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .map_err(|_| ClaudeStoreError::Read)?;
    validate_file(&file.metadata().map_err(|_| ClaudeStoreError::Read)?)?;
    let mut bytes = Vec::new();
    file.take((limit + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| ClaudeStoreError::Read)?;
    if bytes.len() > limit {
        return Err(ClaudeStoreError::ResourceLimit);
    }
    Ok(bytes)
}

fn write_atomic(
    directory: &Path,
    target: &Path,
    bytes: &[u8],
    limit: usize,
) -> Result<CommitStatus, ClaudeStoreError> {
    if bytes.len() > limit {
        return Err(ClaudeStoreError::ResourceLimit);
    }
    validate_directory(&fs::symlink_metadata(directory).map_err(|_| ClaudeStoreError::Write)?)?;
    match fs::symlink_metadata(target) {
        Ok(metadata) => validate_file(&metadata)?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(_) => return Err(ClaudeStoreError::Write),
    }
    let name = target
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or(ClaudeStoreError::Write)?;
    let mut temporary = None;
    for _ in 0..128 {
        let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let path = directory.join(format!(".{name}.{}.{}.tmp", std::process::id(), sequence));
        match OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(&path)
        {
            Ok(file) => {
                temporary = Some((path, file));
                break;
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(_) => return Err(ClaudeStoreError::Write),
        }
    }
    let (temporary_path, mut file) = temporary.ok_or(ClaudeStoreError::Write)?;
    let result = (|| {
        file.set_permissions(fs::Permissions::from_mode(0o600))
            .map_err(|_| ClaudeStoreError::Write)?;
        file.write_all(bytes).map_err(|_| ClaudeStoreError::Write)?;
        file.sync_all().map_err(|_| ClaudeStoreError::Write)?;
        fs::rename(&temporary_path, target).map_err(|_| ClaudeStoreError::Write)?;
        Ok(())
    })();
    if let Err(error) = result {
        let _ = fs::remove_file(&temporary_path);
        return Err(error);
    }
    match sync_directory(directory) {
        Ok(()) => Ok(CommitStatus::Verified),
        Err(_) if fs::read(target).is_ok_and(|written| written == bytes) => {
            Ok(CommitStatus::CommittedUnverified)
        }
        Err(_) => Err(ClaudeStoreError::Write),
    }
}

fn aggregate_bytes(directory: &Path) -> Result<u64, ClaudeStoreError> {
    let mut total = 0u64;
    for entry in fs::read_dir(directory).map_err(|_| ClaudeStoreError::Read)? {
        let entry = entry.map_err(|_| ClaudeStoreError::Read)?;
        let path = entry.path();
        if path.extension().and_then(|extension| extension.to_str()) != Some("json") {
            continue;
        }
        let metadata = fs::symlink_metadata(path).map_err(|_| ClaudeStoreError::Read)?;
        if metadata.file_type().is_file() {
            total = total.saturating_add(metadata.len());
        }
    }
    Ok(total)
}

fn sync_directory(path: &Path) -> io::Result<()> {
    OpenOptions::new().read(true).open(path)?.sync_all()
}

fn current_uid() -> u32 {
    // SAFETY: geteuid has no preconditions and retains no pointers.
    unsafe { libc::geteuid() }
}
