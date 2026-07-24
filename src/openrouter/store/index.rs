use super::*;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct ConversationIndex {
    version: u32,
    conversations: Vec<OpenRouterConversationSummary>,
}

impl FileOpenRouterStore {
    pub(super) fn rebuild_index(
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

    pub(super) fn scan_conversations(
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

    pub(super) fn write_index(
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

    pub(super) fn write_atomic(
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

    pub(super) fn maintain_index(&self) -> OpenRouterIndexMaintenance {
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
