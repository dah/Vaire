use super::*;

#[test]
fn picker_ignores_mismatched_results_then_atomically_switches_threads() {
    let mut state = thread_ready_state();
    seed_thinking(&mut state, "active reasoning");
    state.context_remaining_percent = Some(67);
    state.reduce(Action::Intent(Intent::Resume));
    state.reduce(Action::Event(DomainEvent::ThreadListLoaded(vec![
        thread("thr-active", "Current", 30),
        thread("thr-old", "Target", 20),
        thread("thr-old-a", "Unrelated", 10),
    ])));
    state.reduce(Action::Intent(Intent::ThreadPickerMoveDown));
    assert_eq!(
        state.reduce(Action::Intent(Intent::ThreadPickerSelect)),
        vec![Effect::SwitchThread {
            id: "thr-old".to_owned(),
            model: ModelKey::codex("m1").unwrap(),
            reasoning: "high".to_owned(),
        }]
    );
    assert!(matches!(
        state.conversation_popup().map(|picker| &picker.phase),
        Some(ThreadPickerPhase::Resuming {
            provider: ProviderId::Codex,
            id,
        }) if id == "thr-old"
    ));
    let awaiting_b = state.clone();

    assert!(state
        .reduce(Action::Event(DomainEvent::ThreadSwitchSucceeded {
            id: "thr-old-a".to_owned(),
            history: vec![TranscriptEntry {
                provider: ProviderId::Codex,
                role: TranscriptRole::Assistant,
                status: TranscriptEntryStatus::Normal,
                text: "wrong history".to_owned(),
                item_id: None,
                turn_id: None,
            }],
            model: ModelKey::codex("m1").unwrap(),
            reasoning: "high".to_owned(),
        }))
        .is_empty());
    assert_eq!(state, awaiting_b);

    assert!(state
        .reduce(Action::Event(DomainEvent::ThreadSwitchFailed {
            id: "thr-old-a".to_owned(),
            message: "wrong failure".to_owned(),
        }))
        .is_empty());
    assert_eq!(state, awaiting_b);

    let history = vec![TranscriptEntry {
        provider: ProviderId::Codex,
        role: TranscriptRole::Assistant,
        status: TranscriptEntryStatus::Normal,
        text: "target history".to_owned(),
        item_id: Some("agent-b".to_owned()),
        turn_id: Some("turn-b".to_owned()),
    }];
    let effects = state.reduce(Action::Event(DomainEvent::ThreadSwitchSucceeded {
        id: "thr-old".to_owned(),
        history: history.clone(),
        model: ModelKey::codex("m1").unwrap(),
        reasoning: "high".to_owned(),
    }));
    assert!(matches!(effects.as_slice(), [Effect::Persist(_)]));
    assert!(matches!(&state.thread, ThreadState::Ready { id } if id == "thr-old"));
    assert_eq!(state.transcript, history);
    assert!(state.thinking.entries.is_empty());
    assert!(state.thinking.visible);
    assert_eq!(state.context_remaining_percent, None);
    assert!(state.popup.is_none());
}

#[test]
fn successful_picker_switch_rejects_every_stale_old_turn_event() {
    let mut state = thread_ready_state();
    seed_thinking(&mut state, "active reasoning");
    state.context_remaining_percent = Some(67);
    state.popup = conversation_popup(ThreadPickerState {
        phase: ThreadPickerPhase::Resuming {
            provider: ProviderId::Codex,
            id: "thr-b".to_owned(),
        },
        threads: vec![
            thread("thr-active", "Current", 30),
            thread("thr-b", "Target", 20),
        ],
        selected: 1,
        confirmation: None,
        message: None,
    });
    state.reduce(Action::Event(DomainEvent::ThreadSwitchSucceeded {
        id: "thr-b".to_owned(),
        history: vec![TranscriptEntry {
            provider: ProviderId::Codex,
            role: TranscriptRole::Assistant,
            status: TranscriptEntryStatus::Normal,
            text: "target history".to_owned(),
            item_id: Some("agent-b".to_owned()),
            turn_id: Some("turn-b".to_owned()),
        }],
        model: ModelKey::codex("m1").unwrap(),
        reasoning: "high".to_owned(),
    }));
    let replaced = state.clone();

    deliver_stale_old_turn_events(&mut state);

    assert_eq!(state, replaced);
    assert!(matches!(&state.thread, ThreadState::Ready { id } if id == "thr-b"));
    assert!(matches!(state.turn, TurnState::Idle));
    assert_eq!(state.transcript[0].text, "target history");
    assert!(state.thinking.entries.is_empty());
    assert_eq!(state.context_remaining_percent, None);
}

#[test]
fn picker_navigation_and_failed_switch_preserve_the_active_thread() {
    let mut state = thread_ready_state();
    seed_thinking(&mut state, "active thread reasoning");
    state.context_remaining_percent = Some(67);
    assert_eq!(
        state.reduce(Action::Intent(Intent::Resume)),
        vec![Effect::ListThreads]
    );
    assert!(matches!(state.popup, Some(PopupState::Conversation(_))));
    state.reduce(Action::Event(DomainEvent::ThreadListLoaded(vec![
        thread("thr-active", "Current", 30),
        thread("thr-old", "Older", 20),
    ])));
    state.reduce(Action::Intent(Intent::ThreadPickerMoveDown));
    assert_eq!(state.conversation_popup().unwrap().selected, 1);
    assert_eq!(
        state.reduce(Action::Intent(Intent::ThreadPickerSelect)),
        vec![Effect::SwitchThread {
            id: "thr-old".to_owned(),
            model: ModelKey::codex("m1").unwrap(),
            reasoning: "high".to_owned(),
        }]
    );
    assert!(matches!(&state.thread, ThreadState::Ready { id } if id == "thr-active"));
    assert_eq!(state.context_remaining_percent, Some(67));

    state.reduce(Action::Event(DomainEvent::ThreadSwitchFailed {
        id: "thr-old".to_owned(),
        message: "malformed history".to_owned(),
    }));
    assert_eq!(
        state.preferences.codex.auto_resume_thread_id.as_deref(),
        Some("thr-active")
    );
    assert_eq!(state.transcript[0].text, "old conversation");
    assert_eq!(state.thinking.entries[0].text, "active thread reasoning");
    assert_eq!(state.context_remaining_percent, Some(67));
    assert!(matches!(
        state.conversation_popup().map(|picker| &picker.phase),
        Some(ThreadPickerPhase::Ready)
    ));

    assert_eq!(
        state.reduce(Action::Intent(Intent::ThreadPickerSelect)),
        vec![Effect::SwitchThread {
            id: "thr-old".to_owned(),
            model: ModelKey::codex("m1").unwrap(),
            reasoning: "high".to_owned(),
        }]
    );
    let history = vec![TranscriptEntry {
        provider: ProviderId::Codex,
        role: TranscriptRole::User,
        status: TranscriptEntryStatus::Normal,
        text: "restored".to_owned(),
        item_id: None,
        turn_id: None,
    }];
    let effects = state.reduce(Action::Event(DomainEvent::ThreadSwitchSucceeded {
        id: "thr-old".to_owned(),
        history: history.clone(),
        model: ModelKey::codex("m1").unwrap(),
        reasoning: "high".to_owned(),
    }));
    assert_eq!(
        state.preferences.codex.auto_resume_thread_id.as_deref(),
        Some("thr-old")
    );
    assert_eq!(state.transcript, history);
    assert!(state.thinking.entries.is_empty());
    assert!(state.thinking.visible);
    assert_eq!(state.context_remaining_percent, None);
    assert!(state.conversation_popup().is_none());
    assert!(matches!(effects.as_slice(), [Effect::Persist(_)]));

    state.reduce(Action::Intent(Intent::SendMessage("continue".to_owned())));
    state.reduce(Action::Event(DomainEvent::TurnStarted {
        thread_id: "thr-old".to_owned(),
        turn_id: "turn-new".to_owned(),
    }));
    state.reduce(Action::Event(DomainEvent::ThinkingDelta {
        thread_id: "thr-active".to_owned(),
        turn_id: "turn-old".to_owned(),
        item_id: "thinking-stale".to_owned(),
        kind: ThinkingKind::Summary,
        index: 0,
        delta: "stale reasoning".to_owned(),
    }));
    state.reduce(Action::Event(DomainEvent::TokenUsageUpdated {
        thread_id: "thr-active".to_owned(),
        turn_id: "turn-old".to_owned(),
        context_tokens: 99,
        model_context_window: Some(100),
    }));
    assert!(state.thinking.entries.is_empty());
    assert_eq!(state.context_remaining_percent, None);
}
#[test]
fn picker_only_exposes_threads_registered_to_the_current_account() {
    let mut state = thread_ready_state();
    state.preferences.codex.thread_account_scopes.insert(
        "thr-foreign".to_owned(),
        AccountScope::from_chatgpt_email("other@example.com").unwrap(),
    );
    state.popup = conversation_popup(ThreadPickerState::loading());
    state.reduce(Action::Event(DomainEvent::ThreadListLoaded(vec![
        thread("thr-active", "Current", 30),
        thread("thr-old", "Same account", 20),
        thread("thr-foreign", "Other account", 10),
        thread("thr-unknown", "Unknown account", 5),
    ])));
    assert_eq!(
        state
            .conversation_popup()
            .unwrap()
            .threads
            .iter()
            .map(|thread| thread.id.as_str())
            .collect::<Vec<_>>(),
        vec!["thr-active", "thr-old"]
    );

    let scope = AccountScope::from_chatgpt_email("legacy@example.com").unwrap();
    let mut legacy = AppState::default();
    legacy.reduce(Action::Event(DomainEvent::PreferencesLoaded(
        PreferencesV2 {
            codex: CodexPreferencesV2 {
                account_scope: Some(scope.clone()),
                auto_resume_thread_id: Some("thr-legacy".to_owned()),
                ..CodexPreferencesV2::default()
            },
            ..PreferencesV2::default()
        },
    )));
    assert_eq!(
        legacy
            .preferences
            .codex
            .thread_account_scopes
            .get("thr-legacy"),
        Some(&scope)
    );
}
