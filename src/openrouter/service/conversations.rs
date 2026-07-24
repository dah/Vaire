use super::*;

impl OpenRouterService {
    pub async fn list_conversations(
        &self,
    ) -> Result<Vec<OpenRouterConversationSummary>, OpenRouterStoreError> {
        let store = self.store.clone();
        tokio::task::spawn_blocking(move || store.list_conversations())
            .await
            .map_err(|_| {
                super::OpenRouterStoreError::new(super::OpenRouterStoreFailureCategory::Read)
            })?
    }

    pub async fn load_conversation(
        &self,
        id: OpenRouterConversationId,
    ) -> Result<OpenRouterConversationV2, OpenRouterStoreError> {
        let store = self.store.clone();
        tokio::task::spawn_blocking(move || store.load_conversation(&id))
            .await
            .map_err(|_| {
                super::OpenRouterStoreError::new(super::OpenRouterStoreFailureCategory::Read)
            })?
    }

    pub async fn create_conversation(
        &self,
    ) -> Result<OpenRouterConversationId, OpenRouterStoreError> {
        let store = self.store.clone();
        let id = OpenRouterConversationId::new();
        let saved_id = id.clone();
        tokio::task::spawn_blocking(move || {
            store.save_conversation_with_commit(&OpenRouterConversationV2::new(
                saved_id,
                now_ms(),
                "New conversation",
            ))
        })
        .await
        .map_err(|_| {
            super::OpenRouterStoreError::new(super::OpenRouterStoreFailureCategory::Write)
        })??;
        Ok(id)
    }

    pub async fn delete_conversation(
        &self,
        id: OpenRouterConversationId,
    ) -> Result<(), OpenRouterStoreError> {
        let store = self.store.clone();
        tokio::task::spawn_blocking(move || store.delete_conversation_with_commit(&id))
            .await
            .map_err(|_| {
                super::OpenRouterStoreError::new(super::OpenRouterStoreFailureCategory::Delete)
            })?
            .map(|_| ())
    }

    pub async fn prepare_turn(
        &mut self,
        conversation_id: Option<OpenRouterConversationId>,
        model_id: String,
        user_text: String,
    ) -> Result<PreparedOpenRouterTurn, OpenRouterFailure> {
        if self
            .chat_task
            .as_ref()
            .is_some_and(|task| !task.is_finished())
        {
            return Err(OpenRouterFailure::new(
                OpenRouterFailureCategory::InvalidRequest,
            ));
        }
        self.reap_chat();
        let conversation_id = conversation_id.unwrap_or_default();
        let turn_id = OpenRouterTurnId::new();
        let store = self.store.clone();
        let saved_id = conversation_id.clone();
        let saved_turn = turn_id.clone();
        let saved_model = model_id.clone();
        let saved_text = user_text.clone();
        let (conversation, request) = tokio::task::spawn_blocking(move || {
            let mut conversation = match store.load_conversation(&saved_id) {
                Ok(value) => value,
                Err(error)
                    if error.category() == super::OpenRouterStoreFailureCategory::NotFound =>
                {
                    OpenRouterConversationV2::new(
                        saved_id.clone(),
                        now_ms(),
                        title_for(&saved_text),
                    )
                }
                Err(error) => return Err(error),
            };
            conversation.updated_at_ms = now_ms();
            conversation.turns.push(OpenRouterTurnRecord {
                id: saved_turn,
                model_id: saved_model,
                user_text: saved_text,
                assistant_text: None,
                incomplete_assistant_text: None,
                outcome: OpenRouterTurnOutcome::InProgress,
            });
            let request = ChatRequest::new(
                saved_model_for_request(&conversation),
                conversation.canonical_messages(),
            )
            .map_err(|_| {
                super::OpenRouterStoreError::new(
                    super::OpenRouterStoreFailureCategory::ResourceLimit,
                )
            })?;
            store.save_conversation_with_commit(&conversation)?;
            Ok((conversation, request))
        })
        .await
        .map_err(|_| OpenRouterFailure::new(OpenRouterFailureCategory::CredentialStore))?
        .map_err(|_| OpenRouterFailure::new(OpenRouterFailureCategory::CredentialStore))?;
        Ok(PreparedOpenRouterTurn {
            conversation_id,
            turn_id,
            conversation,
            request,
        })
    }

    pub async fn abandon_prepared_turn(
        &self,
        mut prepared: PreparedOpenRouterTurn,
    ) -> Result<(), OpenRouterFailure> {
        if let Some(record) = prepared.conversation.turns.last_mut() {
            record.outcome = OpenRouterTurnOutcome::Failed;
            record.assistant_text = None;
            record.incomplete_assistant_text = None;
        }
        prepared.conversation.updated_at_ms = now_ms();
        let store = self.store.clone();
        tokio::task::spawn_blocking(move || {
            store.save_conversation_with_commit(&prepared.conversation)
        })
        .await
        .map_err(|_| OpenRouterFailure::new(OpenRouterFailureCategory::CredentialStore))?
        .map_err(|_| OpenRouterFailure::new(OpenRouterFailureCategory::CredentialStore))
        .map(|_| ())
    }
}

pub(super) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

pub(super) fn title_for(text: &str) -> String {
    const MAX_TITLE_BYTES: usize = 80;
    let sanitized = crate::text::sanitize_terminal_text(text);
    let mut title = String::new();
    for character in sanitized.chars() {
        let character = if matches!(character, '\n' | '\r' | '\t') {
            ' '
        } else {
            character
        };
        if title.len().saturating_add(character.len_utf8()) > MAX_TITLE_BYTES {
            break;
        }
        title.push(character);
    }
    let title = title.trim().to_owned();
    if title.is_empty() {
        "New conversation".to_owned()
    } else {
        title
    }
}

pub(super) fn saved_model_for_request(conversation: &OpenRouterConversationV2) -> String {
    conversation
        .turns
        .last()
        .map(|turn| turn.model_id.clone())
        .unwrap_or_default()
}
