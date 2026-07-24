use super::*;

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
