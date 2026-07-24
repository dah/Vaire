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
}

mod catalog;
mod conversation;
mod filesystem;
mod index;
mod operations;

use catalog::*;
use conversation::*;
use filesystem::*;

#[cfg(test)]
mod tests;
