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
    OpenRouterConversationSummary, OpenRouterConversationV1, OpenRouterConversationV2,
    OpenRouterModel, OpenRouterStoreError, OpenRouterStoreFailureCategory, OpenRouterTurnOutcome,
    MAX_ASSISTANT_BYTES, MAX_CATALOG_BODY_BYTES, MAX_CATALOG_MODELS, MAX_CATALOG_TEXT_BYTES,
};

const CATALOG_VERSION: u32 = 1;
const LEGACY_CONVERSATION_VERSION: u64 = 1;
const CONVERSATION_VERSION: u64 = 2;
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

#[derive(Deserialize)]
struct ConversationVersion {
    version: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ConversationSourceVersion {
    V1,
    V2,
}

struct DecodedConversation {
    conversation: OpenRouterConversationV2,
    source_version: ConversationSourceVersion,
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
    ) -> Result<OpenRouterConversationV2, OpenRouterStoreError>;
    fn save_conversation(
        &self,
        conversation: &OpenRouterConversationV2,
    ) -> Result<(), OpenRouterStoreError>;
    fn save_conversation_with_commit(
        &self,
        conversation: &OpenRouterConversationV2,
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
        let mut aggregate = aggregate_conversation_bytes(&self.conversations)?;
        if aggregate > MAX_AGGREGATE_CONVERSATION_BYTES {
            return Err(limit_error());
        }
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
            if metadata.len() > MAX_CONVERSATION_BYTES as u64 || validate_file(&metadata).is_err() {
                continue;
            }
            let Ok(bytes) = read_file_limited(&entry.path(), MAX_CONVERSATION_BYTES) else {
                continue;
            };
            let Ok(mut decoded) = decode_conversation(&bytes, &id) else {
                continue;
            };
            let mut changed = decoded.source_version == ConversationSourceVersion::V1;
            if (repair_in_progress || decoded.source_version == ConversationSourceVersion::V1)
                && repair_in_progress_turns(&mut decoded.conversation)
            {
                changed = true;
            }
            if changed {
                validate_conversation_v2(&decoded.conversation)?;
                let migrated = serde_json::to_vec_pretty(&decoded.conversation)
                    .map_err(|_| corrupt_error())?;
                let migrated_aggregate = aggregate
                    .saturating_sub(metadata.len())
                    .saturating_add(migrated.len() as u64);
                if migrated_aggregate > MAX_AGGREGATE_CONVERSATION_BYTES {
                    return Err(limit_error());
                }
                self.write_atomic(
                    &self.conversations,
                    &entry.path(),
                    &migrated,
                    MAX_CONVERSATION_BYTES,
                )?;
                aggregate = migrated_aggregate;
            }
            valid.push(summary(&decoded.conversation));
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
    ) -> Result<OpenRouterConversationV2, OpenRouterStoreError> {
        let _guard = self.lock.lock().map_err(|_| corrupt_error())?;
        let path = self.conversation_path(id);
        let bytes = read_file_limited(&path, MAX_CONVERSATION_BYTES)?;
        let mut decoded = decode_conversation(&bytes, id)?;
        if decoded.source_version == ConversationSourceVersion::V1 {
            repair_in_progress_turns(&mut decoded.conversation);
            validate_conversation_v2(&decoded.conversation)?;
            let migrated =
                serde_json::to_vec_pretty(&decoded.conversation).map_err(|_| corrupt_error())?;
            let metadata = fs::symlink_metadata(&path).map_err(|_| read_error())?;
            let aggregate = aggregate_conversation_bytes(&self.conversations)?;
            if aggregate
                .saturating_sub(metadata.len())
                .saturating_add(migrated.len() as u64)
                > MAX_AGGREGATE_CONVERSATION_BYTES
            {
                return Err(limit_error());
            }
            self.write_atomic(
                &self.conversations,
                &path,
                &migrated,
                MAX_CONVERSATION_BYTES,
            )?;
            let _ = self.maintain_index();
        }
        Ok(decoded.conversation)
    }

    fn save_conversation(
        &self,
        conversation: &OpenRouterConversationV2,
    ) -> Result<(), OpenRouterStoreError> {
        self.save_conversation_with_commit(conversation).map(|_| ())
    }

    fn save_conversation_with_commit(
        &self,
        conversation: &OpenRouterConversationV2,
    ) -> Result<OpenRouterConversationCommit, OpenRouterStoreError> {
        let _guard = self.lock.lock().map_err(|_| corrupt_error())?;
        validate_conversation_v2(conversation)?;
        let bytes = serde_json::to_vec_pretty(conversation).map_err(|_| corrupt_error())?;
        if bytes.len() > MAX_CONVERSATION_BYTES {
            return Err(limit_error());
        }
        let target = self.conversation_path(&conversation.id);
        let (existing_len, target_exists) = match fs::symlink_metadata(&target) {
            Ok(metadata) => {
                validate_file(&metadata)?;
                let existing = read_file_limited(&target, MAX_CONVERSATION_BYTES)?;
                decode_conversation(&existing, &conversation.id)?;
                (metadata.len(), true)
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => (0, false),
            Err(_) => return Err(write_error()),
        };
        let aggregate = aggregate_conversation_bytes(&self.conversations)?;
        if aggregate
            .saturating_sub(existing_len)
            .saturating_add(bytes.len() as u64)
            > MAX_AGGREGATE_CONVERSATION_BYTES
        {
            return Err(limit_error());
        }
        if !target_exists && self.scan_conversations(false)?.len() >= MAX_CONVERSATIONS {
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
                ));
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

fn repair_in_progress_turns(conversation: &mut OpenRouterConversationV2) -> bool {
    let mut changed = false;
    for turn in &mut conversation.turns {
        if turn.outcome == OpenRouterTurnOutcome::InProgress {
            turn.outcome = OpenRouterTurnOutcome::Interrupted;
            turn.assistant_text = None;
            turn.incomplete_assistant_text = None;
            changed = true;
        }
    }
    changed
}

fn decode_conversation(
    bytes: &[u8],
    expected_id: &OpenRouterConversationId,
) -> Result<DecodedConversation, OpenRouterStoreError> {
    let version: ConversationVersion =
        serde_json::from_slice(bytes).map_err(|_| corrupt_error())?;
    let decoded = match version.version {
        LEGACY_CONVERSATION_VERSION => {
            let legacy: OpenRouterConversationV1 =
                serde_json::from_slice(bytes).map_err(|_| corrupt_error())?;
            validate_conversation_v1(&legacy)?;
            DecodedConversation {
                conversation: legacy.into(),
                source_version: ConversationSourceVersion::V1,
            }
        }
        CONVERSATION_VERSION => {
            let conversation: OpenRouterConversationV2 =
                serde_json::from_slice(bytes).map_err(|_| corrupt_error())?;
            DecodedConversation {
                conversation,
                source_version: ConversationSourceVersion::V2,
            }
        }
        _ => return Err(unsupported_version_error()),
    };
    if &decoded.conversation.id != expected_id {
        return Err(corrupt_error());
    }
    validate_conversation_v2(&decoded.conversation)?;
    Ok(decoded)
}

fn validate_conversation_v1(
    conversation: &OpenRouterConversationV1,
) -> Result<(), OpenRouterStoreError> {
    if u64::from(conversation.version) != LEGACY_CONVERSATION_VERSION
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
        validate_turn_identity(
            &mut ids,
            &turn.id,
            &turn.model_id,
            &turn.user_text,
            &mut canonical_text,
        )?;
        match turn.outcome {
            OpenRouterTurnOutcome::Completed => {
                let Some(assistant) = &turn.assistant_text else {
                    return Err(corrupt_error());
                };
                add_completed_assistant(assistant, &mut canonical_text)?;
            }
            OpenRouterTurnOutcome::InProgress
            | OpenRouterTurnOutcome::Interrupted
            | OpenRouterTurnOutcome::Failed => {
                if turn.assistant_text.is_some() {
                    return Err(corrupt_error());
                }
            }
        }
        ensure_canonical_bound(canonical_text)?;
    }
    Ok(())
}

fn validate_conversation_v2(
    conversation: &OpenRouterConversationV2,
) -> Result<(), OpenRouterStoreError> {
    if u64::from(conversation.version) != CONVERSATION_VERSION
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
        validate_turn_identity(
            &mut ids,
            &turn.id,
            &turn.model_id,
            &turn.user_text,
            &mut canonical_text,
        )?;
        match turn.outcome {
            OpenRouterTurnOutcome::Completed => {
                let Some(assistant) = &turn.assistant_text else {
                    return Err(corrupt_error());
                };
                if turn.incomplete_assistant_text.is_some() {
                    return Err(corrupt_error());
                }
                add_completed_assistant(assistant, &mut canonical_text)?;
            }
            OpenRouterTurnOutcome::Failed => {
                if turn.assistant_text.is_some() {
                    return Err(corrupt_error());
                }
                if let Some(incomplete) = &turn.incomplete_assistant_text {
                    if incomplete.is_empty() {
                        return Err(corrupt_error());
                    }
                    if incomplete.len() > MAX_ASSISTANT_BYTES {
                        return Err(limit_error());
                    }
                }
            }
            OpenRouterTurnOutcome::InProgress | OpenRouterTurnOutcome::Interrupted => {
                if turn.assistant_text.is_some() || turn.incomplete_assistant_text.is_some() {
                    return Err(corrupt_error());
                }
            }
        }
        ensure_canonical_bound(canonical_text)?;
    }
    Ok(())
}

fn validate_turn_identity<'a>(
    ids: &mut HashSet<&'a crate::provider::OpenRouterTurnId>,
    id: &'a crate::provider::OpenRouterTurnId,
    model_id: &str,
    user_text: &str,
    canonical_text: &mut usize,
) -> Result<(), OpenRouterStoreError> {
    if !ids.insert(id)
        || ModelKey::new(ProviderId::OpenRouter, model_id.to_owned()).is_err()
        || user_text.is_empty()
    {
        return Err(corrupt_error());
    }
    if user_text.len() > MAX_USER_BYTES {
        return Err(limit_error());
    }
    *canonical_text = canonical_text.saturating_add(user_text.len());
    Ok(())
}

fn add_completed_assistant(
    assistant: &str,
    canonical_text: &mut usize,
) -> Result<(), OpenRouterStoreError> {
    if assistant.len() > MAX_ASSISTANT_BYTES {
        return Err(limit_error());
    }
    *canonical_text = canonical_text.saturating_add(assistant.len());
    Ok(())
}

fn ensure_canonical_bound(canonical_text: usize) -> Result<(), OpenRouterStoreError> {
    if canonical_text > MAX_CANONICAL_TEXT_BYTES {
        return Err(limit_error());
    }
    Ok(())
}

fn summary(conversation: &OpenRouterConversationV2) -> OpenRouterConversationSummary {
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
            ));
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

fn unsupported_version_error() -> OpenRouterStoreError {
    OpenRouterStoreError::new(OpenRouterStoreFailureCategory::UnsupportedVersion)
}

fn limit_error() -> OpenRouterStoreError {
    OpenRouterStoreError::new(OpenRouterStoreFailureCategory::ResourceLimit)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::openrouter::{ChatRole, OpenRouterTurnRecord};
    use crate::provider::OpenRouterTurnId;
    use crate::storage::ScriptedDirectorySync;
    use serde_json::{json, Value};
    use std::os::unix::fs::PermissionsExt;
    use std::sync::Arc;

    fn turn(
        outcome: OpenRouterTurnOutcome,
        assistant_text: Option<&str>,
        incomplete_assistant_text: Option<&str>,
    ) -> OpenRouterTurnRecord {
        OpenRouterTurnRecord {
            id: OpenRouterTurnId::new(),
            model_id: "vendor/model".to_owned(),
            user_text: "hello".to_owned(),
            assistant_text: assistant_text.map(str::to_owned),
            incomplete_assistant_text: incomplete_assistant_text.map(str::to_owned),
            outcome,
        }
    }

    fn completed_conversation() -> OpenRouterConversationV2 {
        let mut conversation =
            OpenRouterConversationV2::new(OpenRouterConversationId::new(), 1, "Test");
        conversation.updated_at_ms = 2;
        conversation
            .turns
            .push(turn(OpenRouterTurnOutcome::Completed, Some("world"), None));
        conversation
    }

    fn conversation_path(root: &Path, id: &OpenRouterConversationId) -> PathBuf {
        root.join("conversations")
            .join(format!("{}.json", id.as_str()))
    }

    fn write_owner_only(path: &Path, bytes: &[u8]) {
        fs::write(path, bytes).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
    }

    fn legacy_fixture(id: &OpenRouterConversationId) -> Vec<u8> {
        serde_json::to_vec_pretty(&json!({
            "version": 1,
            "id": id,
            "created_at_ms": 11,
            "updated_at_ms": 22,
            "title": "Legacy",
            "turns": [
                {
                    "id": OpenRouterTurnId::new(),
                    "model_id": "vendor/model",
                    "user_text": "completed user",
                    "assistant_text": "completed assistant",
                    "outcome": "completed"
                },
                {
                    "id": OpenRouterTurnId::new(),
                    "model_id": "vendor/model",
                    "user_text": "failed user",
                    "assistant_text": null,
                    "outcome": "failed"
                },
                {
                    "id": OpenRouterTurnId::new(),
                    "model_id": "vendor/model",
                    "user_text": "interrupted user",
                    "assistant_text": null,
                    "outcome": "interrupted"
                },
                {
                    "id": OpenRouterTurnId::new(),
                    "model_id": "vendor/model",
                    "user_text": "in progress user",
                    "assistant_text": null,
                    "outcome": "in_progress"
                }
            ]
        }))
        .unwrap()
    }

    #[test]
    fn v2_validation_enforces_the_complete_outcome_matrix_and_partial_bound() {
        let mut conversation = completed_conversation();
        assert!(validate_conversation_v2(&conversation).is_ok());

        conversation.turns[0].assistant_text = Some(String::new());
        assert!(validate_conversation_v2(&conversation).is_ok());
        conversation.turns[0].assistant_text = None;
        assert_eq!(
            validate_conversation_v2(&conversation)
                .unwrap_err()
                .category(),
            OpenRouterStoreFailureCategory::Corrupt
        );

        conversation.turns[0] = turn(
            OpenRouterTurnOutcome::Completed,
            Some("done"),
            Some("partial"),
        );
        assert_eq!(
            validate_conversation_v2(&conversation)
                .unwrap_err()
                .category(),
            OpenRouterStoreFailureCategory::Corrupt
        );

        conversation.turns[0] = turn(OpenRouterTurnOutcome::Failed, None, None);
        assert!(validate_conversation_v2(&conversation).is_ok());
        conversation.turns[0].incomplete_assistant_text = Some("partial".to_owned());
        assert!(validate_conversation_v2(&conversation).is_ok());
        conversation.turns[0].incomplete_assistant_text = Some(String::new());
        assert_eq!(
            validate_conversation_v2(&conversation)
                .unwrap_err()
                .category(),
            OpenRouterStoreFailureCategory::Corrupt
        );
        conversation.turns[0] = turn(OpenRouterTurnOutcome::Failed, Some("done"), None);
        assert_eq!(
            validate_conversation_v2(&conversation)
                .unwrap_err()
                .category(),
            OpenRouterStoreFailureCategory::Corrupt
        );

        for outcome in [
            OpenRouterTurnOutcome::Interrupted,
            OpenRouterTurnOutcome::InProgress,
        ] {
            conversation.turns[0] = turn(outcome, None, Some("partial"));
            assert_eq!(
                validate_conversation_v2(&conversation)
                    .unwrap_err()
                    .category(),
                OpenRouterStoreFailureCategory::Corrupt
            );
        }

        conversation.turns[0] = turn(OpenRouterTurnOutcome::Failed, None, None);
        conversation.turns[0].incomplete_assistant_text = Some("x".repeat(MAX_ASSISTANT_BYTES + 1));
        assert_eq!(
            validate_conversation_v2(&conversation)
                .unwrap_err()
                .category(),
            OpenRouterStoreFailureCategory::ResourceLimit
        );
    }

    #[test]
    fn canonical_history_keeps_completed_semantics_and_excludes_failed_partial() {
        let mut conversation = completed_conversation();
        let mut failed = turn(OpenRouterTurnOutcome::Failed, None, Some("display only"));
        failed.user_text = "again".to_owned();
        conversation.turns.push(failed);

        let messages = conversation.canonical_messages();
        assert_eq!(
            messages
                .iter()
                .map(|message| (&message.role, message.content.as_str()))
                .collect::<Vec<_>>(),
            vec![
                (&ChatRole::User, "hello"),
                (&ChatRole::Assistant, "world"),
                (&ChatRole::User, "again"),
            ]
        );
    }

    #[test]
    fn eager_v1_migration_is_lossless_repairs_in_progress_and_preserves_modes() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("openrouter");
        drop(FileOpenRouterStore::new(&root).unwrap());

        let id = OpenRouterConversationId::new();
        let path = conversation_path(&root, &id);
        let legacy = legacy_fixture(&id);
        let legacy_value: Value = serde_json::from_slice(&legacy).unwrap();
        write_owner_only(&path, &legacy);

        let store = FileOpenRouterStore::new(&root).unwrap();
        let migrated = store.load_conversation(&id).unwrap();
        assert_eq!(migrated.version, 2);
        assert_eq!(migrated.created_at_ms, 11);
        assert_eq!(migrated.updated_at_ms, 22);
        assert_eq!(migrated.title, "Legacy");
        assert_eq!(migrated.turns.len(), 4);
        for (migrated_turn, legacy_turn) in migrated
            .turns
            .iter()
            .zip(legacy_value["turns"].as_array().unwrap())
        {
            assert_eq!(
                migrated_turn.id.as_str(),
                legacy_turn["id"].as_str().unwrap()
            );
            assert_eq!(
                migrated_turn.model_id,
                legacy_turn["model_id"].as_str().unwrap()
            );
            assert_eq!(
                migrated_turn.user_text,
                legacy_turn["user_text"].as_str().unwrap()
            );
            assert_eq!(
                migrated_turn.assistant_text.as_deref(),
                legacy_turn["assistant_text"].as_str()
            );
            assert_eq!(migrated_turn.incomplete_assistant_text, None);
        }
        assert_eq!(migrated.turns[0].user_text, "completed user");
        assert_eq!(
            migrated.turns[0].assistant_text.as_deref(),
            Some("completed assistant")
        );
        assert_eq!(migrated.turns[1].outcome, OpenRouterTurnOutcome::Failed);
        assert_eq!(migrated.turns[1].assistant_text, None);
        assert_eq!(migrated.turns[1].incomplete_assistant_text, None);
        assert_eq!(
            migrated.turns[2].outcome,
            OpenRouterTurnOutcome::Interrupted
        );
        assert_eq!(
            migrated.turns[3].outcome,
            OpenRouterTurnOutcome::Interrupted
        );
        assert!(migrated
            .turns
            .iter()
            .all(|turn| turn.incomplete_assistant_text.is_none()));
        assert_eq!(
            migrated
                .canonical_messages()
                .iter()
                .map(|message| message.content.as_str())
                .collect::<Vec<_>>(),
            vec![
                "completed user",
                "completed assistant",
                "failed user",
                "interrupted user",
                "in progress user",
            ]
        );
        assert_eq!(store.list_conversations().unwrap().len(), 1);
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o7777,
            0o600
        );
        let written: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(written["version"], 2);
    }

    #[test]
    fn load_fallback_migrates_v1_introduced_after_startup() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("openrouter");
        let store = FileOpenRouterStore::new(&root).unwrap();
        let id = OpenRouterConversationId::new();
        let path = conversation_path(&root, &id);
        write_owner_only(&path, &legacy_fixture(&id));

        let migrated = store.load_conversation(&id).unwrap();
        assert_eq!(migrated.version, 2);
        assert_eq!(migrated.turns[1].outcome, OpenRouterTurnOutcome::Failed);
        assert_eq!(migrated.turns[1].incomplete_assistant_text, None);
        assert_eq!(
            migrated.turns[3].outcome,
            OpenRouterTurnOutcome::Interrupted
        );
        let written: Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
        assert_eq!(written["version"], 2);
        assert_eq!(written["turns"][3]["outcome"], "interrupted");
    }

    #[test]
    fn list_fallback_migrates_v1_and_repairs_stale_in_progress() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("openrouter");
        let store = FileOpenRouterStore::new(&root).unwrap();
        let id = OpenRouterConversationId::new();
        let path = conversation_path(&root, &id);
        write_owner_only(&path, &legacy_fixture(&id));

        let listed = store.list_conversations().unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, id);
        let written: Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
        assert_eq!(written["version"], 2);
        assert_eq!(written["turns"][3]["outcome"], "interrupted");
    }

    #[test]
    fn migration_resource_failure_is_order_independent_and_preserves_v1_source() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("openrouter");
        drop(FileOpenRouterStore::new(&root).unwrap());

        let id = OpenRouterConversationId::new();
        let path = conversation_path(&root, &id);
        let legacy_value: Value = serde_json::from_slice(&legacy_fixture(&id)).unwrap();
        let compact_legacy = serde_json::to_vec(&legacy_value).unwrap();
        write_owner_only(&path, &compact_legacy);

        let filler_path = root.join("conversations/or_padding.json");
        let filler_len = MAX_AGGREGATE_CONVERSATION_BYTES as usize - compact_legacy.len() - 1;
        write_owner_only(&filler_path, &vec![b'x'; filler_len]);

        assert_eq!(
            FileOpenRouterStore::new(&root).unwrap_err().category(),
            OpenRouterStoreFailureCategory::ResourceLimit
        );
        assert_eq!(fs::read(path).unwrap(), compact_legacy);
    }

    #[test]
    fn unsupported_and_corrupt_sources_remain_byte_identical_and_unlisted() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("openrouter");
        drop(FileOpenRouterStore::new(&root).unwrap());

        let fixtures = [
            (
                0,
                br#"{"version":3,"id":"or_00000000000000000000000000000000"}"#.as_slice(),
                OpenRouterStoreFailureCategory::UnsupportedVersion,
            ),
            (
                1,
                br#"{"version":0,"id":"or_00000000000000000000000000000001"}"#.as_slice(),
                OpenRouterStoreFailureCategory::UnsupportedVersion,
            ),
            (
                2,
                br#"{"version":"2","id":"or_00000000000000000000000000000002"}"#.as_slice(),
                OpenRouterStoreFailureCategory::Corrupt,
            ),
            (
                3,
                br#"not json"#.as_slice(),
                OpenRouterStoreFailureCategory::Corrupt,
            ),
            (
                4,
                br#"{"version":1,"id":"or_00000000000000000000000000000004","created_at_ms":1,"updated_at_ms":1,"title":"Legacy","turns":[],"unknown":true}"#.as_slice(),
                OpenRouterStoreFailureCategory::Corrupt,
            ),
            (
                5,
                br#"{"id":"or_00000000000000000000000000000005"}"#.as_slice(),
                OpenRouterStoreFailureCategory::Corrupt,
            ),
            (
                6,
                br#"{"version":2,"id":"or_00000000000000000000000000000006","created_at_ms":1,"updated_at_ms":1,"title":"V2","turns":[],"unknown":true}"#.as_slice(),
                OpenRouterStoreFailureCategory::Corrupt,
            ),
            (
                7,
                br#"{"version":1,"id":"or_00000000000000000000000000000007","created_at_ms":1,"updated_at_ms":1,"title":"Legacy","turns":[{"id":"ort_00000000000000000000000000000007","model_id":"vendor/model","user_text":"failed","outcome":"failed"}]}"#.as_slice(),
                OpenRouterStoreFailureCategory::Corrupt,
            ),
            (
                8,
                br#"{"version":4294967296,"id":"or_00000000000000000000000000000008"}"#.as_slice(),
                OpenRouterStoreFailureCategory::UnsupportedVersion,
            ),
            (
                9,
                br#"{"version":-1,"id":"or_00000000000000000000000000000009"}"#.as_slice(),
                OpenRouterStoreFailureCategory::Corrupt,
            ),
        ];
        let mut saved = Vec::new();
        for (suffix, bytes, category) in fixtures {
            let id: OpenRouterConversationId = format!("or_{suffix:032x}").parse().unwrap();
            let path = conversation_path(&root, &id);
            write_owner_only(&path, bytes);
            saved.push((id, path, bytes.to_vec(), category));
        }

        let store = FileOpenRouterStore::new(&root).unwrap();
        assert!(store.list_conversations().unwrap().is_empty());
        for (id, path, original, category) in &saved {
            assert_eq!(fs::read(path).unwrap(), *original);
            assert_eq!(
                store.load_conversation(id).unwrap_err().category(),
                *category
            );
            let replacement = OpenRouterConversationV2::new(id.clone(), 1, "Replacement");
            assert_eq!(
                store
                    .save_conversation(&replacement)
                    .unwrap_err()
                    .category(),
                *category
            );
            assert_eq!(fs::read(path).unwrap(), *original);
        }
        drop(store);

        let reopened = FileOpenRouterStore::new(&root).unwrap();
        assert!(reopened.list_conversations().unwrap().is_empty());
        for (_, path, original, _) in saved {
            assert_eq!(fs::read(path).unwrap(), original);
        }
    }

    #[test]
    fn missing_v2_assistant_text_is_rejected_without_rewriting_or_listing() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("openrouter");
        drop(FileOpenRouterStore::new(&root).unwrap());

        let id = OpenRouterConversationId::new();
        let path = conversation_path(&root, &id);
        let malformed = serde_json::to_vec_pretty(&serde_json::json!({
            "version": 2,
            "id": id,
            "created_at_ms": 1,
            "updated_at_ms": 1,
            "title": "Malformed V2",
            "turns": [{
                "id": OpenRouterTurnId::new(),
                "model_id": "vendor/model",
                "user_text": "hello",
                "outcome": "in_progress"
            }]
        }))
        .unwrap();
        write_owner_only(&path, &malformed);

        let store = FileOpenRouterStore::new(&root).unwrap();
        assert_eq!(fs::read(&path).unwrap(), malformed);
        assert!(store.list_conversations().unwrap().is_empty());
        assert_eq!(fs::read(&path).unwrap(), malformed);
        assert_eq!(
            store.load_conversation(&id).unwrap_err().category(),
            OpenRouterStoreFailureCategory::Corrupt
        );
        assert_eq!(fs::read(&path).unwrap(), malformed);

        let replacement = OpenRouterConversationV2::new(id.clone(), 2, "Replacement");
        assert_eq!(
            store
                .save_conversation(&replacement)
                .unwrap_err()
                .category(),
            OpenRouterStoreFailureCategory::Corrupt
        );
        assert_eq!(fs::read(&path).unwrap(), malformed);
        assert_eq!(
            store
                .save_conversation_with_commit(&replacement)
                .unwrap_err()
                .category(),
            OpenRouterStoreFailureCategory::Corrupt
        );
        assert_eq!(fs::read(&path).unwrap(), malformed);
        drop(store);

        let reopened = FileOpenRouterStore::new(&root).unwrap();
        assert!(reopened.list_conversations().unwrap().is_empty());
        assert_eq!(fs::read(path).unwrap(), malformed);
    }

    #[test]
    fn v2_failed_partial_round_trips_and_reopen_does_not_rewrite_source() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("openrouter");
        let store = FileOpenRouterStore::new(&root).unwrap();
        let mut conversation = completed_conversation();
        conversation.turns.push(turn(
            OpenRouterTurnOutcome::Failed,
            None,
            Some("retained partial"),
        ));
        store.save_conversation(&conversation).unwrap();
        let path = conversation_path(&root, &conversation.id);
        let before_reopen = fs::read(&path).unwrap();
        drop(store);

        let reopened = FileOpenRouterStore::new(&root).unwrap();
        let loaded = reopened.load_conversation(&conversation.id).unwrap();
        assert_eq!(loaded, conversation);
        assert_eq!(
            loaded.turns[1].incomplete_assistant_text.as_deref(),
            Some("retained partial")
        );
        assert_eq!(fs::read(&path).unwrap(), before_reopen);
        drop(reopened);

        let reopened_again = FileOpenRouterStore::new(&root).unwrap();
        assert_eq!(
            reopened_again.load_conversation(&conversation.id).unwrap(),
            conversation
        );
        assert_eq!(fs::read(path).unwrap(), before_reopen);
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
        assert_eq!(
            fs::metadata(conversation_path(&root, &conversation.id))
                .unwrap()
                .permissions()
                .mode()
                & 0o7777,
            0o600
        );
    }

    #[test]
    fn startup_accepts_explicit_null_and_omitted_incomplete_text_then_repairs_v2() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("openrouter");
        drop(FileOpenRouterStore::new(&root).unwrap());

        let id = OpenRouterConversationId::new();
        let path = conversation_path(&root, &id);
        let source = serde_json::to_vec_pretty(&serde_json::json!({
            "version": 2,
            "id": id,
            "created_at_ms": 1,
            "updated_at_ms": 1,
            "title": "Repairable V2",
            "turns": [{
                "id": OpenRouterTurnId::new(),
                "model_id": "vendor/model",
                "user_text": "hello",
                "assistant_text": null,
                "outcome": "in_progress"
            }]
        }))
        .unwrap();
        write_owner_only(&path, &source);

        let reopened = FileOpenRouterStore::new(&root).unwrap();
        let repaired = reopened.load_conversation(&id).unwrap();
        assert_eq!(
            repaired.turns[0].outcome,
            OpenRouterTurnOutcome::Interrupted
        );
        assert_eq!(repaired.turns[0].assistant_text, None);
        assert_eq!(repaired.turns[0].incomplete_assistant_text, None);
        let written: Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
        assert_eq!(written["turns"][0]["assistant_text"], Value::Null);
        assert!(written["turns"][0]
            .get("incomplete_assistant_text")
            .is_none());
    }

    #[test]
    fn corrupt_active_file_fails_without_exposing_contents() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("openrouter");
        let store = FileOpenRouterStore::new(&root).unwrap();
        let corrupt_id: OpenRouterConversationId =
            "or_00000000000000000000000000000000".parse().unwrap();
        let path = conversation_path(&root, &corrupt_id);
        write_owner_only(&path, b"secret-looking-corrupt-body");

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
}
