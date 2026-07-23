use std::collections::HashSet;
use std::fs::{self, DirBuilder, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

use crate::provider::{ModelKey, OpenRouterConversationId, ProviderId};
use crate::storage::{CommitStatus, DirectorySync, RealDirectorySync};
use crate::text::sanitize_terminal_text;

use super::types::{
    OpenRouterConversationSummary, OpenRouterConversationV1, OpenRouterModel, OpenRouterStoreError,
    OpenRouterStoreFailureCategory, OpenRouterTurnOutcome, MAX_ASSISTANT_BYTES,
    MAX_CATALOG_BODY_BYTES, MAX_CATALOG_MODELS, MAX_CATALOG_TEXT_BYTES,
};

const CATALOG_VERSION: u32 = 1;
const CONVERSATION_VERSION: u32 = 1;
const MAX_CONVERSATIONS: usize = 50;
const MAX_CONVERSATION_BYTES: usize = 1024 * 1024;
const MAX_CANONICAL_TEXT_BYTES: usize = 768 * 1024;
const MAX_TURNS: usize = 1024;
const MAX_USER_BYTES: usize = 128 * 1024;
const MAX_AGGREGATE_CONVERSATION_BYTES: u64 = 16 * 1024 * 1024;
const MAX_INDEX_BYTES: usize = 256 * 1024;
const MAX_TITLE_BYTES: usize = 256;
static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct CatalogFile {
    version: u32,
    fetched_at_ms: u64,
    models: Vec<OpenRouterModel>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct ConversationIndex {
    version: u32,
    conversations: Vec<OpenRouterConversationSummary>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OpenRouterIndexMaintenance {
    Verified,
    CommittedUnverified,
    NotUpdated(OpenRouterStoreFailureCategory),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OpenRouterConversationCommit {
    pub source: CommitStatus,
    pub index: OpenRouterIndexMaintenance,
}

pub trait OpenRouterConversationStore: Send + Sync {
    fn load_catalog(&self) -> Result<Option<(u64, Vec<OpenRouterModel>)>, OpenRouterStoreError>;
    fn save_catalog(
        &self,
        fetched_at_ms: u64,
        models: &[OpenRouterModel],
    ) -> Result<(), OpenRouterStoreError>;
    fn save_catalog_with_commit(
        &self,
        fetched_at_ms: u64,
        models: &[OpenRouterModel],
    ) -> Result<CommitStatus, OpenRouterStoreError> {
        self.save_catalog(fetched_at_ms, models)?;
        Ok(CommitStatus::Verified)
    }
    fn list_conversations(
        &self,
    ) -> Result<Vec<OpenRouterConversationSummary>, OpenRouterStoreError>;
    fn load_conversation(
        &self,
        id: &OpenRouterConversationId,
    ) -> Result<OpenRouterConversationV1, OpenRouterStoreError>;
    fn save_conversation(
        &self,
        conversation: &OpenRouterConversationV1,
    ) -> Result<(), OpenRouterStoreError>;
    fn save_conversation_with_commit(
        &self,
        conversation: &OpenRouterConversationV1,
    ) -> Result<OpenRouterConversationCommit, OpenRouterStoreError> {
        self.save_conversation(conversation)?;
        Ok(OpenRouterConversationCommit {
            source: CommitStatus::Verified,
            index: OpenRouterIndexMaintenance::Verified,
        })
    }
    fn delete_conversation(
        &self,
        id: &OpenRouterConversationId,
    ) -> Result<(), OpenRouterStoreError>;
    fn delete_conversation_with_commit(
        &self,
        id: &OpenRouterConversationId,
    ) -> Result<OpenRouterConversationCommit, OpenRouterStoreError> {
        self.delete_conversation(id)?;
        Ok(OpenRouterConversationCommit {
            source: CommitStatus::Verified,
            index: OpenRouterIndexMaintenance::Verified,
        })
    }
}

#[derive(Debug)]
pub struct FileOpenRouterStore {
    root: PathBuf,
    conversations: PathBuf,
    lock: Mutex<()>,
    directory_sync: Arc<dyn DirectorySync>,
}

impl FileOpenRouterStore {
    pub fn new(root: impl Into<PathBuf>) -> Result<Self, OpenRouterStoreError> {
        let root = root.into();
        secure_directory(&root)?;
        let conversations = root.join("conversations");
        secure_directory(&conversations)?;
        let store = Self {
            root,
            conversations,
            lock: Mutex::new(()),
            directory_sync: Arc::new(RealDirectorySync),
        };
        let _ = store.rebuild_index(true)?;
        Ok(store)
    }

    #[cfg(test)]
    fn with_directory_sync(
        root: impl Into<PathBuf>,
        directory_sync: Arc<dyn DirectorySync>,
    ) -> Result<Self, OpenRouterStoreError> {
        let mut store = Self::new(root)?;
        store.directory_sync = directory_sync;
        Ok(store)
    }

    fn catalog_path(&self) -> PathBuf {
        self.root.join("catalog.json")
    }

    fn index_path(&self) -> PathBuf {
        self.conversations.join("index.json")
    }

    fn conversation_path(&self, id: &OpenRouterConversationId) -> PathBuf {
        self.conversations.join(format!("{}.json", id.as_str()))
    }

    fn rebuild_index(
        &self,
        repair_in_progress: bool,
    ) -> Result<Vec<OpenRouterConversationSummary>, OpenRouterStoreError> {
        let mut conversations = self.scan_conversations(repair_in_progress)?;
        conversations.sort_by(|left, right| {
            right
                .updated_at_ms
                .cmp(&left.updated_at_ms)
                .then_with(|| left.id.cmp(&right.id))
        });
        self.write_index(&conversations)?;
        Ok(conversations)
    }

    fn scan_conversations(
        &self,
        repair_in_progress: bool,
    ) -> Result<Vec<OpenRouterConversationSummary>, OpenRouterStoreError> {
        validate_directory(&fs::symlink_metadata(&self.conversations).map_err(|_| read_error())?)?;
        let mut aggregate = 0u64;
        let mut valid = Vec::new();
        for entry in fs::read_dir(&self.conversations).map_err(|_| read_error())? {
            let entry = entry.map_err(|_| read_error())?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            let Some(stem) = name.strip_suffix(".json") else {
                continue;
            };
            if stem == "index" {
                continue;
            }
            let Ok(id) = stem.parse::<OpenRouterConversationId>() else {
                continue;
            };
            let metadata = fs::symlink_metadata(entry.path()).map_err(|_| read_error())?;
            if !metadata.file_type().is_file() {
                continue;
            }
            aggregate = aggregate.saturating_add(metadata.len());
            if aggregate > MAX_AGGREGATE_CONVERSATION_BYTES {
                return Err(limit_error());
            }
            if metadata.len() > MAX_CONVERSATION_BYTES as u64 || validate_file(&metadata).is_err() {
                continue;
            }
            let Ok(bytes) = read_file_limited(&entry.path(), MAX_CONVERSATION_BYTES) else {
                continue;
            };
            let Ok(mut conversation) = serde_json::from_slice::<OpenRouterConversationV1>(&bytes)
            else {
                continue;
            };
            if conversation.id != id || validate_conversation(&conversation).is_err() {
                continue;
            }
            if repair_in_progress {
                let mut changed = false;
                for turn in &mut conversation.turns {
                    if turn.outcome == OpenRouterTurnOutcome::InProgress {
                        turn.outcome = OpenRouterTurnOutcome::Interrupted;
                        turn.assistant_text = None;
                        changed = true;
                    }
                }
                if changed {
                    self.write_atomic(
                        &self.conversations,
                        &entry.path(),
                        &serde_json::to_vec_pretty(&conversation).map_err(|_| corrupt_error())?,
                        MAX_CONVERSATION_BYTES,
                    )?;
                }
            }
            valid.push(summary(&conversation));
            if valid.len() > MAX_CONVERSATIONS {
                return Err(limit_error());
            }
        }
        Ok(valid)
    }

    fn write_index(
        &self,
        conversations: &[OpenRouterConversationSummary],
    ) -> Result<CommitStatus, OpenRouterStoreError> {
        let file = ConversationIndex {
            version: 1,
            conversations: conversations.to_vec(),
        };
        let bytes = serde_json::to_vec_pretty(&file).map_err(|_| corrupt_error())?;
        self.write_atomic(
            &self.conversations,
            &self.index_path(),
            &bytes,
            MAX_INDEX_BYTES,
        )
    }

    fn write_atomic(
        &self,
        directory: &Path,
        target: &Path,
        bytes: &[u8],
        limit: usize,
    ) -> Result<CommitStatus, OpenRouterStoreError> {
        write_atomic(
            directory,
            target,
            bytes,
            limit,
            self.directory_sync.as_ref(),
        )
    }

    fn maintain_index(&self) -> OpenRouterIndexMaintenance {
        let mut conversations = match self.scan_conversations(false) {
            Ok(conversations) => conversations,
            Err(error) => return OpenRouterIndexMaintenance::NotUpdated(error.category()),
        };
        conversations.sort_by(|left, right| {
            right
                .updated_at_ms
                .cmp(&left.updated_at_ms)
                .then_with(|| left.id.cmp(&right.id))
        });
        match self.write_index(&conversations) {
            Ok(CommitStatus::Verified) => OpenRouterIndexMaintenance::Verified,
            Ok(CommitStatus::CommittedUnverified) => {
                OpenRouterIndexMaintenance::CommittedUnverified
            }
            Err(error) => OpenRouterIndexMaintenance::NotUpdated(error.category()),
        }
    }
}

impl OpenRouterConversationStore for FileOpenRouterStore {
    fn load_catalog(&self) -> Result<Option<(u64, Vec<OpenRouterModel>)>, OpenRouterStoreError> {
        let _guard = self.lock.lock().map_err(|_| corrupt_error())?;
        let path = self.catalog_path();
        match fs::symlink_metadata(&path) {
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err(read_error()),
        }
        let bytes = read_file_limited(&path, MAX_CATALOG_BODY_BYTES)?;
        let catalog: CatalogFile = serde_json::from_slice(&bytes).map_err(|_| corrupt_error())?;
        validate_catalog(&catalog.models)?;
        if catalog.version != CATALOG_VERSION {
            return Err(corrupt_error());
        }
        Ok(Some((catalog.fetched_at_ms, catalog.models)))
    }

    fn save_catalog(
        &self,
        fetched_at_ms: u64,
        models: &[OpenRouterModel],
    ) -> Result<(), OpenRouterStoreError> {
        self.save_catalog_with_commit(fetched_at_ms, models)
            .map(|_| ())
    }

    fn save_catalog_with_commit(
        &self,
        fetched_at_ms: u64,
        models: &[OpenRouterModel],
    ) -> Result<CommitStatus, OpenRouterStoreError> {
        let _guard = self.lock.lock().map_err(|_| corrupt_error())?;
        validate_catalog(models)?;
        let bytes = serde_json::to_vec_pretty(&CatalogFile {
            version: CATALOG_VERSION,
            fetched_at_ms,
            models: models.to_vec(),
        })
        .map_err(|_| corrupt_error())?;
        self.write_atomic(
            &self.root,
            &self.catalog_path(),
            &bytes,
            MAX_CATALOG_BODY_BYTES,
        )
    }

    fn list_conversations(
        &self,
    ) -> Result<Vec<OpenRouterConversationSummary>, OpenRouterStoreError> {
        let _guard = self.lock.lock().map_err(|_| corrupt_error())?;
        let mut conversations = self.scan_conversations(false)?;
        conversations.sort_by(|left, right| {
            right
                .updated_at_ms
                .cmp(&left.updated_at_ms)
                .then_with(|| left.id.cmp(&right.id))
        });
        let _ = self.write_index(&conversations);
        Ok(conversations)
    }

    fn load_conversation(
        &self,
        id: &OpenRouterConversationId,
    ) -> Result<OpenRouterConversationV1, OpenRouterStoreError> {
        let _guard = self.lock.lock().map_err(|_| corrupt_error())?;
        let path = self.conversation_path(id);
        let bytes = match read_file_limited(&path, MAX_CONVERSATION_BYTES) {
            Ok(bytes) => bytes,
            Err(error) if error.category() == OpenRouterStoreFailureCategory::NotFound => {
                return Err(error)
            }
            Err(_) => return Err(corrupt_error()),
        };
        let conversation: OpenRouterConversationV1 =
            serde_json::from_slice(&bytes).map_err(|_| corrupt_error())?;
        if &conversation.id != id {
            return Err(corrupt_error());
        }
        validate_conversation(&conversation)?;
        Ok(conversation)
    }

    fn save_conversation(
        &self,
        conversation: &OpenRouterConversationV1,
    ) -> Result<(), OpenRouterStoreError> {
        self.save_conversation_with_commit(conversation).map(|_| ())
    }

    fn save_conversation_with_commit(
        &self,
        conversation: &OpenRouterConversationV1,
    ) -> Result<OpenRouterConversationCommit, OpenRouterStoreError> {
        let _guard = self.lock.lock().map_err(|_| corrupt_error())?;
        validate_conversation(conversation)?;
        let bytes = serde_json::to_vec_pretty(conversation).map_err(|_| corrupt_error())?;
        if bytes.len() > MAX_CONVERSATION_BYTES {
            return Err(limit_error());
        }
        let target = self.conversation_path(&conversation.id);
        let existing_len = fs::symlink_metadata(&target)
            .ok()
            .filter(|metadata| metadata.file_type().is_file())
            .map_or(0, |metadata| metadata.len());
        let aggregate = aggregate_conversation_bytes(&self.conversations)?;
        if aggregate
            .saturating_sub(existing_len)
            .saturating_add(bytes.len() as u64)
            > MAX_AGGREGATE_CONVERSATION_BYTES
        {
            return Err(limit_error());
        }
        if !target.exists() && self.scan_conversations(false)?.len() >= MAX_CONVERSATIONS {
            return Err(limit_error());
        }
        let source =
            self.write_atomic(&self.conversations, &target, &bytes, MAX_CONVERSATION_BYTES)?;
        Ok(OpenRouterConversationCommit {
            source,
            index: self.maintain_index(),
        })
    }

    fn delete_conversation(
        &self,
        id: &OpenRouterConversationId,
    ) -> Result<(), OpenRouterStoreError> {
        self.delete_conversation_with_commit(id).map(|_| ())
    }

    fn delete_conversation_with_commit(
        &self,
        id: &OpenRouterConversationId,
    ) -> Result<OpenRouterConversationCommit, OpenRouterStoreError> {
        let _guard = self.lock.lock().map_err(|_| corrupt_error())?;
        let path = self.conversation_path(id);
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Err(OpenRouterStoreError::new(
                    OpenRouterStoreFailureCategory::NotFound,
                ))
            }
            Err(_) => return Err(delete_error()),
        };
        validate_file(&metadata)?;
        fs::remove_file(&path).map_err(|_| delete_error())?;
        let source = match self.directory_sync.sync(&self.conversations) {
            Ok(()) => CommitStatus::Verified,
            Err(_) if matches!(fs::symlink_metadata(&path), Err(error) if error.kind() == io::ErrorKind::NotFound) => {
                CommitStatus::CommittedUnverified
            }
            Err(_) => return Err(delete_error()),
        };
        Ok(OpenRouterConversationCommit {
            source,
            index: self.maintain_index(),
        })
    }
}

fn validate_catalog(models: &[OpenRouterModel]) -> Result<(), OpenRouterStoreError> {
    if models.len() > MAX_CATALOG_MODELS {
        return Err(limit_error());
    }
    let mut ids = HashSet::with_capacity(models.len());
    let mut text = 0usize;
    for model in models {
        if !model.validate() || !ids.insert(&model.id) {
            return Err(corrupt_error());
        }
        text = text
            .saturating_add(model.id.len())
            .saturating_add(model.name.as_ref().map_or(0, String::len));
        if text > MAX_CATALOG_TEXT_BYTES {
            return Err(limit_error());
        }
    }
    Ok(())
}

fn validate_conversation(
    conversation: &OpenRouterConversationV1,
) -> Result<(), OpenRouterStoreError> {
    if conversation.version != CONVERSATION_VERSION
        || conversation.updated_at_ms < conversation.created_at_ms
        || conversation.title.len() > MAX_TITLE_BYTES
        || sanitize_terminal_text(&conversation.title) != conversation.title
        || conversation.title.contains(['\n', '\r'])
        || conversation.turns.len() > MAX_TURNS
    {
        return Err(corrupt_error());
    }
    let mut ids = HashSet::with_capacity(conversation.turns.len());
    let mut canonical_text = 0usize;
    for turn in &conversation.turns {
        if !ids.insert(&turn.id)
            || ModelKey::new(ProviderId::OpenRouter, turn.model_id.clone()).is_err()
            || turn.user_text.is_empty()
        {
            return Err(corrupt_error());
        }
        if turn.user_text.len() > MAX_USER_BYTES {
            return Err(limit_error());
        }
        canonical_text = canonical_text.saturating_add(turn.user_text.len());
        match turn.outcome {
            OpenRouterTurnOutcome::Completed => {
                let Some(assistant) = &turn.assistant_text else {
                    return Err(corrupt_error());
                };
                if assistant.len() > MAX_ASSISTANT_BYTES {
                    return Err(limit_error());
                }
                canonical_text = canonical_text.saturating_add(assistant.len());
            }
            OpenRouterTurnOutcome::InProgress
            | OpenRouterTurnOutcome::Interrupted
            | OpenRouterTurnOutcome::Failed => {
                if turn.assistant_text.is_some() {
                    return Err(corrupt_error());
                }
            }
        }
        if canonical_text > MAX_CANONICAL_TEXT_BYTES {
            return Err(limit_error());
        }
    }
    Ok(())
}

fn summary(conversation: &OpenRouterConversationV1) -> OpenRouterConversationSummary {
    OpenRouterConversationSummary {
        id: conversation.id.clone(),
        created_at_ms: conversation.created_at_ms,
        updated_at_ms: conversation.updated_at_ms,
        title: conversation.title.clone(),
        turn_count: conversation.turns.len(),
    }
}

fn secure_directory(path: &Path) -> Result<(), OpenRouterStoreError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => return validate_directory(&metadata),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(_) => return Err(write_error()),
    }
    let parent = path.parent().ok_or_else(write_error)?;
    fs::create_dir_all(parent).map_err(|_| write_error())?;
    match DirBuilder::new().mode(0o700).create(path) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(_) => return Err(write_error()),
    }
    let metadata = fs::symlink_metadata(path).map_err(|_| write_error())?;
    validate_directory(&metadata)
}

fn validate_directory(metadata: &fs::Metadata) -> Result<(), OpenRouterStoreError> {
    if !metadata.file_type().is_dir()
        || metadata.uid() != current_uid()
        || metadata.mode() & 0o7777 != 0o700
    {
        return Err(permission_error());
    }
    Ok(())
}

fn validate_file(metadata: &fs::Metadata) -> Result<(), OpenRouterStoreError> {
    if !metadata.file_type().is_file()
        || metadata.uid() != current_uid()
        || metadata.mode() & 0o7777 != 0o600
    {
        return Err(permission_error());
    }
    Ok(())
}

fn read_file_limited(path: &Path, limit: usize) -> Result<Vec<u8>, OpenRouterStoreError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Err(OpenRouterStoreError::new(
                OpenRouterStoreFailureCategory::NotFound,
            ))
        }
        Err(_) => return Err(read_error()),
    };
    validate_file(&metadata)?;
    if metadata.len() > limit as u64 {
        return Err(limit_error());
    }
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .map_err(|_| read_error())?;
    validate_file(&file.metadata().map_err(|_| read_error())?)?;
    let mut bytes = Vec::new();
    file.take((limit + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| read_error())?;
    if bytes.len() > limit {
        return Err(limit_error());
    }
    Ok(bytes)
}

fn write_atomic(
    directory: &Path,
    target: &Path,
    bytes: &[u8],
    limit: usize,
    directory_sync: &dyn DirectorySync,
) -> Result<CommitStatus, OpenRouterStoreError> {
    if bytes.len() > limit {
        return Err(limit_error());
    }
    validate_directory(&fs::symlink_metadata(directory).map_err(|_| write_error())?)?;
    match fs::symlink_metadata(target) {
        Ok(metadata) => validate_file(&metadata)?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(_) => return Err(write_error()),
    }
    let name = target
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(write_error)?;
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
            Err(_) => return Err(write_error()),
        }
    }
    let (temporary_path, mut file) = temporary.ok_or_else(write_error)?;
    let precommit = (|| {
        file.set_permissions(fs::Permissions::from_mode(0o600))
            .map_err(|_| write_error())?;
        file.write_all(bytes).map_err(|_| write_error())?;
        file.sync_all().map_err(|_| write_error())?;
        fs::rename(&temporary_path, target).map_err(|_| write_error())?;
        Ok(())
    })();
    if let Err(error) = precommit {
        let _ = fs::remove_file(&temporary_path);
        return Err(error);
    }
    Ok(match directory_sync.sync(directory) {
        Ok(()) => CommitStatus::Verified,
        Err(_) if fs::read(target).is_ok_and(|written| written == bytes) => {
            CommitStatus::CommittedUnverified
        }
        Err(_) => return Err(write_error()),
    })
}

fn aggregate_conversation_bytes(directory: &Path) -> Result<u64, OpenRouterStoreError> {
    let mut total = 0u64;
    for entry in fs::read_dir(directory).map_err(|_| read_error())? {
        let entry = entry.map_err(|_| read_error())?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if name == "index.json" || !name.starts_with("or_") || !name.ends_with(".json") {
            continue;
        }
        let metadata = fs::symlink_metadata(entry.path()).map_err(|_| read_error())?;
        if metadata.file_type().is_file() {
            total = total.saturating_add(metadata.len());
        }
    }
    Ok(total)
}

fn current_uid() -> u32 {
    // SAFETY: geteuid has no preconditions and retains no pointers.
    unsafe { libc::geteuid() }
}

fn read_error() -> OpenRouterStoreError {
    OpenRouterStoreError::new(OpenRouterStoreFailureCategory::Read)
}

fn write_error() -> OpenRouterStoreError {
    OpenRouterStoreError::new(OpenRouterStoreFailureCategory::Write)
}

fn delete_error() -> OpenRouterStoreError {
    OpenRouterStoreError::new(OpenRouterStoreFailureCategory::Delete)
}

fn permission_error() -> OpenRouterStoreError {
    OpenRouterStoreError::new(OpenRouterStoreFailureCategory::Permissions)
}

fn corrupt_error() -> OpenRouterStoreError {
    OpenRouterStoreError::new(OpenRouterStoreFailureCategory::Corrupt)
}

fn limit_error() -> OpenRouterStoreError {
    OpenRouterStoreError::new(OpenRouterStoreFailureCategory::ResourceLimit)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::OpenRouterTurnId;
    use crate::storage::ScriptedDirectorySync;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::Arc;

    fn completed_conversation() -> OpenRouterConversationV1 {
        let mut conversation =
            OpenRouterConversationV1::new(OpenRouterConversationId::new(), 1, "Test");
        conversation.updated_at_ms = 2;
        conversation
            .turns
            .push(super::super::types::OpenRouterTurnRecord {
                id: OpenRouterTurnId::new(),
                model_id: "vendor/model".to_owned(),
                user_text: "hello".to_owned(),
                assistant_text: Some("world".to_owned()),
                outcome: OpenRouterTurnOutcome::Completed,
            });
        conversation
    }

    #[test]
    fn committed_source_and_delete_are_not_lost_to_directory_sync_failures() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("openrouter");
        let store = FileOpenRouterStore::with_directory_sync(
            &root,
            Arc::new(ScriptedDirectorySync::fail_after(0)),
        )
        .unwrap();
        let conversation = completed_conversation();

        let saved = store.save_conversation_with_commit(&conversation).unwrap();
        assert_eq!(saved.source, CommitStatus::CommittedUnverified);
        assert_eq!(saved.index, OpenRouterIndexMaintenance::CommittedUnverified);
        assert_eq!(
            store.load_conversation(&conversation.id).unwrap(),
            conversation
        );

        let deleted = store
            .delete_conversation_with_commit(&conversation.id)
            .unwrap();
        assert_eq!(deleted.source, CommitStatus::CommittedUnverified);
        assert!(matches!(
            store.load_conversation(&conversation.id),
            Err(error) if error.category() == OpenRouterStoreFailureCategory::NotFound
        ));
        assert!(store.list_conversations().unwrap().is_empty());
    }

    #[test]
    fn atomically_round_trips_catalog_and_history_with_owner_only_modes() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("openrouter");
        let store = FileOpenRouterStore::new(&root).unwrap();
        let catalog = vec![OpenRouterModel {
            id: "vendor/model".to_owned(),
            name: Some("Model".to_owned()),
            context_length: Some(4096),
        }];
        store.save_catalog(7, &catalog).unwrap();
        assert_eq!(store.load_catalog().unwrap(), Some((7, catalog)));
        let conversation = completed_conversation();
        store.save_conversation(&conversation).unwrap();
        assert_eq!(
            store.load_conversation(&conversation.id).unwrap(),
            conversation
        );
        assert_eq!(store.list_conversations().unwrap().len(), 1);
        assert_eq!(
            fs::metadata(&root).unwrap().permissions().mode() & 0o7777,
            0o700
        );
        assert_eq!(
            fs::metadata(root.join("catalog.json"))
                .unwrap()
                .permissions()
                .mode()
                & 0o7777,
            0o600
        );
    }

    #[test]
    fn startup_repairs_in_progress_and_rebuilds_index_without_deleting_corrupt_files() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("openrouter");
        let store = FileOpenRouterStore::new(&root).unwrap();
        let mut conversation = completed_conversation();
        conversation.turns[0].outcome = OpenRouterTurnOutcome::InProgress;
        conversation.turns[0].assistant_text = None;
        store.save_conversation(&conversation).unwrap();
        assert_eq!(
            store.load_conversation(&conversation.id).unwrap().turns[0].outcome,
            OpenRouterTurnOutcome::InProgress
        );
        let corrupt = root.join("conversations/or_00000000000000000000000000000000.json");
        fs::write(&corrupt, b"not json").unwrap();
        fs::set_permissions(&corrupt, fs::Permissions::from_mode(0o600)).unwrap();
        drop(store);

        let reopened = FileOpenRouterStore::new(&root).unwrap();
        assert_eq!(
            reopened.load_conversation(&conversation.id).unwrap().turns[0].outcome,
            OpenRouterTurnOutcome::Interrupted
        );
        assert!(corrupt.exists());
        assert_eq!(reopened.list_conversations().unwrap().len(), 1);
    }

    #[test]
    fn corrupt_active_file_fails_and_validated_deletion_updates_index() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("openrouter");
        let store = FileOpenRouterStore::new(&root).unwrap();
        let conversation = completed_conversation();
        store.save_conversation(&conversation).unwrap();
        store.delete_conversation(&conversation.id).unwrap();
        assert!(store.list_conversations().unwrap().is_empty());

        let corrupt_id: OpenRouterConversationId =
            "or_00000000000000000000000000000000".parse().unwrap();
        let path = root
            .join("conversations")
            .join(format!("{}.json", corrupt_id.as_str()));
        fs::write(&path, b"secret-looking-corrupt-body").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(
            store.load_conversation(&corrupt_id).unwrap_err().category(),
            OpenRouterStoreFailureCategory::Corrupt
        );
        assert!(
            !format!("{:?}", store.load_conversation(&corrupt_id).unwrap_err())
                .contains("secret-looking")
        );
    }

    #[test]
    fn history_limit_failure_preserves_the_last_valid_atomic_file() {
        let temp = tempfile::tempdir().unwrap();
        let store = FileOpenRouterStore::new(temp.path().join("openrouter")).unwrap();
        let mut conversation = completed_conversation();
        store.save_conversation(&conversation).unwrap();
        let original = store.load_conversation(&conversation.id).unwrap();
        conversation.turns[0].user_text = "x".repeat(MAX_USER_BYTES + 1);
        assert_eq!(
            store
                .save_conversation(&conversation)
                .unwrap_err()
                .category(),
            OpenRouterStoreFailureCategory::ResourceLimit
        );
        assert_eq!(store.load_conversation(&conversation.id).unwrap(), original);
    }

    #[test]
    fn canonical_history_excludes_partial_and_failed_assistant_text() {
        let mut conversation = completed_conversation();
        conversation
            .turns
            .push(super::super::types::OpenRouterTurnRecord {
                id: OpenRouterTurnId::new(),
                model_id: "vendor/model".to_owned(),
                user_text: "again".to_owned(),
                assistant_text: None,
                outcome: OpenRouterTurnOutcome::Failed,
            });
        let messages = conversation.canonical_messages();
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[2].content, "again");
    }
}
