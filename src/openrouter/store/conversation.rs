use super::*;

#[derive(Deserialize)]
pub(super) struct ConversationVersion {
    version: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ConversationSourceVersion {
    V1,
    V2,
}

pub(super) struct DecodedConversation {
    pub(super) conversation: OpenRouterConversationV2,
    pub(super) source_version: ConversationSourceVersion,
}

pub(super) fn repair_in_progress_turns(conversation: &mut OpenRouterConversationV2) -> bool {
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

pub(super) fn decode_conversation(
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

pub(super) fn validate_conversation_v1(
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

pub(super) fn validate_conversation_v2(
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

pub(super) fn validate_turn_identity<'a>(
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

pub(super) fn add_completed_assistant(
    assistant: &str,
    canonical_text: &mut usize,
) -> Result<(), OpenRouterStoreError> {
    if assistant.len() > MAX_ASSISTANT_BYTES {
        return Err(limit_error());
    }
    *canonical_text = canonical_text.saturating_add(assistant.len());
    Ok(())
}

pub(super) fn ensure_canonical_bound(canonical_text: usize) -> Result<(), OpenRouterStoreError> {
    if canonical_text > MAX_CANONICAL_TEXT_BYTES {
        return Err(limit_error());
    }
    Ok(())
}

pub(super) fn summary(conversation: &OpenRouterConversationV2) -> OpenRouterConversationSummary {
    OpenRouterConversationSummary {
        id: conversation.id.clone(),
        created_at_ms: conversation.created_at_ms,
        updated_at_ms: conversation.updated_at_ms,
        title: conversation.title.clone(),
        turn_count: conversation.turns.len(),
    }
}
