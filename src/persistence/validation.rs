use crate::provider::ProviderId;
use crate::text::is_terminal_unsafe;

use super::domain::{
    AccountScope, CodexPreferencesV2, LoadNotice, LoadOutcome, PreferencesV1, PreferencesV2,
    LEGACY_PREFERENCES_VERSION, MAX_PREFERENCE_STRING_BYTES, PREFERENCES_VERSION,
};

fn valid_preference_string(value: &str) -> bool {
    !value.trim().is_empty()
        && value == value.trim()
        && value.len() <= MAX_PREFERENCE_STRING_BYTES
        && !value.chars().any(is_terminal_unsafe)
}

fn valid_scope(scope: &AccountScope) -> bool {
    match scope {
        AccountScope::ChatgptEmail(email) => {
            AccountScope::from_chatgpt_email(email).as_ref() == Some(scope)
        }
    }
}

fn valid_codex(preferences: &CodexPreferencesV2) -> bool {
    if preferences
        .account_scope
        .as_ref()
        .is_some_and(|scope| !valid_scope(scope))
        || preferences
            .auto_resume_thread_id
            .as_deref()
            .is_some_and(|value| !valid_preference_string(value))
        || preferences
            .model_id
            .as_deref()
            .is_some_and(|value| !valid_preference_string(value))
        || preferences
            .reasoning_effort
            .as_deref()
            .is_some_and(|value| !valid_preference_string(value))
        || preferences
            .thread_account_scopes
            .iter()
            .any(|(id, scope)| !valid_preference_string(id) || !valid_scope(scope))
    {
        return false;
    }

    match (
        preferences.auto_resume_thread_id.as_ref(),
        preferences.account_scope.as_ref(),
    ) {
        (Some(thread_id), Some(account_scope)) => preferences
            .thread_account_scopes
            .get(thread_id)
            .is_none_or(|registered_scope| registered_scope == account_scope),
        _ => true,
    }
}

pub(super) fn valid_legacy(preferences: &PreferencesV1) -> bool {
    preferences.version == LEGACY_PREFERENCES_VERSION
        && valid_codex(&CodexPreferencesV2 {
            account_scope: preferences.account_scope.clone(),
            auto_resume_thread_id: preferences.thread_id.clone(),
            model_id: preferences.model_id.clone(),
            reasoning_effort: preferences.reasoning_effort.clone(),
            thread_account_scopes: preferences.thread_account_scopes.clone(),
        })
}

pub(super) fn valid_preferences(preferences: &PreferencesV2) -> bool {
    if preferences.version != PREFERENCES_VERSION
        || !valid_codex(&preferences.codex)
        || preferences
            .openrouter
            .selected_model_id
            .as_deref()
            .is_some_and(|value| !valid_preference_string(value))
        || preferences
            .openrouter
            .enabled_model_ids
            .iter()
            .any(|value| !valid_preference_string(value))
    {
        return false;
    }

    match preferences.active_provider {
        ProviderId::Codex => preferences.openrouter.auto_resume_conversation_id.is_none(),
        ProviderId::OpenRouter => preferences.codex.auto_resume_thread_id.is_none(),
    }
}

pub(super) fn corrupt_load_outcome() -> LoadOutcome {
    LoadOutcome {
        preferences: PreferencesV2::default(),
        notice: Some(LoadNotice::Corrupt),
        may_overwrite: false,
        needs_save: false,
    }
}
