use super::*;

fn non_codex_reasoning_state(provider: ProviderId) -> AppState {
    let selected_model = match provider {
        ProviderId::OpenRouter => ModelKey::openrouter("vendor/model").unwrap(),
        ProviderId::Claude => ModelKey::claude("sonnet").unwrap(),
        ProviderId::Codex => unreachable!("helper is only for non-Codex providers"),
    };
    AppState {
        active_provider: provider,
        selected_model: Some(selected_model),
        selected_reasoning: None,
        preferences: PreferencesV4 {
            active_provider: provider,
            ..PreferencesV4::default()
        },
        ..AppState::default()
    }
}

#[test]
fn openrouter_reasoning_remains_unsupported_without_state_change_or_effects() {
    for intent in [
        Intent::ShowReasoning,
        Intent::SelectReasoning("high".to_owned()),
    ] {
        let mut state = non_codex_reasoning_state(ProviderId::OpenRouter);
        let selected_model = state.selected_model.clone();
        let selected_reasoning = state.selected_reasoning.clone();
        let preferences = state.preferences.clone();
        let turn = state.turn.clone();

        let effects = state.reduce(Action::Intent(intent));

        assert!(effects.is_empty());
        assert_eq!(
            state.notice.as_deref(),
            Some("OpenRouter reasoning effort is unsupported")
        );
        assert!(state.popup.is_none());
        assert_eq!(state.selected_model, selected_model);
        assert_eq!(state.selected_reasoning, selected_reasoning);
        assert_eq!(state.preferences, preferences);
        assert_eq!(state.turn, turn);
    }
}

#[test]
fn claude_show_reasoning_reports_current_effort_and_all_choices() {
    let mut state = non_codex_reasoning_state(ProviderId::Claude);

    assert!(state
        .reduce(Action::Intent(Intent::ShowReasoning))
        .is_empty());
    assert_eq!(
        state.notice.as_deref(),
        Some("Claude effort: default; choices: default, low, medium, high, xhigh, max")
    );

    state.preferences.claude.selected_effort = Some(ClaudeEffort::XHigh);
    state.reduce(Action::Intent(Intent::ShowReasoning));
    assert_eq!(
        state.notice.as_deref(),
        Some("Claude effort: xhigh; choices: default, low, medium, high, xhigh, max")
    );
}

#[test]
fn claude_select_reasoning_accepts_exact_values_and_default() {
    let mut state = non_codex_reasoning_state(ProviderId::Claude);

    for (value, expected) in [
        ("low", Some(ClaudeEffort::Low)),
        ("medium", Some(ClaudeEffort::Medium)),
        ("high", Some(ClaudeEffort::High)),
        ("xhigh", Some(ClaudeEffort::XHigh)),
        ("max", Some(ClaudeEffort::Max)),
        ("default", None),
    ] {
        let effects = state.reduce(Action::Intent(Intent::SelectReasoning(value.to_owned())));
        assert_eq!(state.preferences.claude.selected_effort, expected);
        assert!(matches!(effects.as_slice(), [Effect::Persist(preferences)]
            if preferences.claude.selected_effort == expected));
        assert_eq!(
            state.notice,
            Some(format!(
                "Claude effort set to {value}; it applies to the next turn"
            ))
        );
        assert!(state.selected_reasoning.is_none());
    }

    let effects = state.reduce(Action::Intent(Intent::SelectReasoning(
        "default".to_owned(),
    )));
    assert!(effects.is_empty());
    assert_eq!(
        state.notice.as_deref(),
        Some("Claude effort is already default; it applies to the next turn")
    );
}

#[test]
fn claude_invalid_reasoning_values_stay_local_without_mutation() {
    for value in ["HIGH", "x-high", "ultracode", "unknown"] {
        let mut state = non_codex_reasoning_state(ProviderId::Claude);
        state.preferences.claude.selected_effort = Some(ClaudeEffort::Medium);
        let before = state.preferences.clone();

        let effects = state.reduce(Action::Intent(Intent::SelectReasoning(value.to_owned())));

        assert!(effects.is_empty());
        assert_eq!(state.preferences, before);
        assert_eq!(
            state.notice,
            Some(format!("unsupported Claude effort {value}; use /reasoning"))
        );
    }
}

#[test]
fn claude_effort_change_preserves_active_conversation_state() {
    let id: ClaudeSessionId = "00000000-0000-4000-8000-000000000001".parse().unwrap();
    let mut state = non_codex_reasoning_state(ProviderId::Claude);
    state.claude.conversation = ClaudeConversationState::Ready { id: id.clone() };
    state.claude.resolved_model = Some(ClaudeModelMetadata {
        id: "claude-resolved".to_owned(),
        display_name: Some("Resolved".to_owned()),
    });
    state.preferences.claude.auto_resume_session_id = Some(id.clone());
    state.preferences.claude.selected_model_alias = Some(ClaudeModelAlias::Sonnet);
    state.preferences.codex.reasoning_effort = Some("high".to_owned());
    state.context_remaining_percent = Some(42);
    state.transcript.push(TranscriptEntry {
        provider: ProviderId::Claude,
        role: TranscriptRole::Assistant,
        status: TranscriptEntryStatus::Normal,
        text: "preserved".to_owned(),
        item_id: None,
        turn_id: None,
    });
    let conversation = state.claude.conversation.clone();
    let resolved_model = state.claude.resolved_model.clone();
    let selected_model = state.selected_model.clone();
    let transcript = state.transcript.clone();

    let effects = state.reduce(Action::Intent(Intent::SelectReasoning("max".to_owned())));

    assert!(matches!(effects.as_slice(), [Effect::Persist(_)]));
    assert_eq!(
        state.preferences.claude.selected_effort,
        Some(ClaudeEffort::Max)
    );
    assert_eq!(
        state.preferences.codex.reasoning_effort.as_deref(),
        Some("high")
    );
    assert_eq!(state.preferences.claude.auto_resume_session_id, Some(id));
    assert_eq!(
        state.preferences.claude.selected_model_alias,
        Some(ClaudeModelAlias::Sonnet)
    );
    assert_eq!(state.claude.conversation, conversation);
    assert_eq!(state.claude.resolved_model, resolved_model);
    assert_eq!(state.selected_model, selected_model);
    assert_eq!(state.transcript, transcript);
    assert_eq!(state.context_remaining_percent, Some(42));
    assert!(state.selected_reasoning.is_none());
}

#[test]
fn claude_effort_change_is_blocked_during_turn_or_eager_creation() {
    let mut active = non_codex_reasoning_state(ProviderId::Claude);
    active.turn = TurnState::Starting;
    let active_preferences = active.preferences.clone();
    assert!(active
        .reduce(Action::Intent(Intent::SelectReasoning("high".to_owned())))
        .is_empty());
    assert_eq!(active.preferences, active_preferences);
    assert_eq!(
        active.notice.as_deref(),
        Some("wait for or interrupt the active turn")
    );

    let mut pending = non_codex_reasoning_state(ProviderId::Claude);
    pending.pending_new_claude_session = true;
    let pending_preferences = pending.preferences.clone();
    assert!(pending
        .reduce(Action::Intent(Intent::SelectReasoning("high".to_owned())))
        .is_empty());
    assert_eq!(pending.preferences, pending_preferences);
    assert_eq!(
        pending.notice.as_deref(),
        Some("wait for the new Claude conversation request to finish")
    );
}
