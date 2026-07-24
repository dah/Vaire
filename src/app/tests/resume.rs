use super::*;

#[test]
fn successful_automatic_resume_settles_turn_state() {
    let mut state = thread_ready_state();
    state.thread = ThreadState::Resuming {
        id: "thr-active".to_owned(),
    };
    state.turn = TurnState::Streaming {
        thread_id: "thr-active".to_owned(),
        turn_id: "stale-turn".to_owned(),
    };

    state.reduce(Action::Event(DomainEvent::ResumeSucceeded {
        id: "thr-active".to_owned(),
        history: Vec::new(),
    }));

    assert!(matches!(state.turn, TurnState::Idle));
}

#[test]
fn resume_results_are_correlated_and_cannot_cross_account_boundaries() {
    let mut state = thread_ready_state();
    let snapshot = state.clone();

    assert!(state
        .reduce(Action::Event(DomainEvent::ResumeStarted {
            id: "thr-stale".to_owned(),
        }))
        .is_empty());
    assert_eq!(state, snapshot);
    assert!(state
        .reduce(Action::Event(DomainEvent::ResumeSucceeded {
            id: "thr-stale".to_owned(),
            history: Vec::new(),
        }))
        .is_empty());
    assert_eq!(state, snapshot);
    assert!(state
        .reduce(Action::Event(DomainEvent::ResumeFailed {
            id: "thr-stale".to_owned(),
            message: "late failure".to_owned(),
        }))
        .is_empty());
    assert_eq!(state, snapshot);

    state.thread = ThreadState::Resuming {
        id: "thr-active".to_owned(),
    };
    state.reduce(Action::Event(DomainEvent::AccountLoaded(
        AccountScope::from_chatgpt_email("other@example.com"),
    )));
    let account_switched = state.clone();

    assert!(state
        .reduce(Action::Event(DomainEvent::ResumeSucceeded {
            id: "thr-active".to_owned(),
            history: vec![TranscriptEntry {
                provider: crate::provider::ProviderId::Codex,
                role: TranscriptRole::Assistant,
                status: TranscriptEntryStatus::Normal,
                text: "wrong account history".to_owned(),
                item_id: None,
                turn_id: None,
            }],
        }))
        .is_empty());
    assert_eq!(state, account_switched);
    assert!(state
        .reduce(Action::Event(DomainEvent::ResumeFailed {
            id: "thr-active".to_owned(),
            message: "late failure".to_owned(),
        }))
        .is_empty());
    assert_eq!(state, account_switched);
}

#[test]
fn account_change_and_resume_failure_preserve_saved_id() {
    let mut state = AppState::default();
    state.preferences.codex.auto_resume_thread_id = Some("thr-old".to_owned());
    state.preferences.codex.account_scope = AccountScope::from_chatgpt_email("old@example.com");
    state.reduce(Action::Event(DomainEvent::AccountLoaded(
        AccountScope::from_chatgpt_email("new@example.com"),
    )));
    assert!(matches!(state.thread, ThreadState::AccountMismatch { .. }));
    state.reduce(Action::Event(DomainEvent::ResumeFailed {
        id: "thr-old".to_owned(),
        message: "stale".to_owned(),
    }));
    assert_eq!(
        state.preferences.codex.auto_resume_thread_id.as_deref(),
        Some("thr-old")
    );
}

#[test]
fn thread_picker_requires_account_identity_and_can_safely_replace_a_mismatched_active_thread() {
    let saved_scope = AccountScope::from_chatgpt_email("saved@example.com");
    let mut state = AppState {
        connection: ConnectionState::Ready { generation: 1 },
        auth: AuthState::SignedIn {
            scope: AccountScope::from_chatgpt_email("other@example.com"),
        },
        thread: ThreadState::AccountMismatch {
            id: "thr-saved".to_owned(),
        },
        preferences: PreferencesV3 {
            codex: CodexPreferencesV2 {
                account_scope: saved_scope.clone(),
                auto_resume_thread_id: Some("thr-saved".to_owned()),
                ..CodexPreferencesV2::default()
            },
            ..PreferencesV3::default()
        },
        models: vec![model("m1", true, &["high"], "high")],
        selected_model: Some(ModelKey::codex("m1").unwrap()),
        ..AppState::default()
    };

    assert_eq!(
        state.reduce(Action::Intent(Intent::Resume)),
        vec![Effect::ListThreads]
    );
    assert_eq!(
        state.preferences.codex.auto_resume_thread_id.as_deref(),
        Some("thr-saved")
    );
    assert!(matches!(
        state.conversation_popup().map(|picker| &picker.phase),
        Some(ThreadPickerPhase::Loading)
    ));
    state.reduce(Action::Intent(Intent::ThreadPickerClose));

    state.auth = AuthState::SignedIn { scope: None };
    assert_eq!(
        state.reduce(Action::Intent(Intent::Resume)),
        vec![Effect::ListThreads]
    );
    assert!(matches!(
        state.conversation_popup().map(|picker| &picker.phase),
        Some(ThreadPickerPhase::Loading)
    ));
    state.reduce(Action::Intent(Intent::ThreadPickerClose));

    state.auth = AuthState::SignedIn { scope: saved_scope };
    state.thread = ThreadState::ResumeFailed {
        id: "thr-saved".to_owned(),
        message: "temporary failure".to_owned(),
    };
    assert_eq!(
        state.reduce(Action::Intent(Intent::Resume)),
        vec![Effect::ListThreads]
    );
    assert!(matches!(
        state.conversation_popup().map(|picker| &picker.phase),
        Some(ThreadPickerPhase::Loading)
    ));
}

#[test]
fn automatic_resume_preserves_thinking_on_failure_and_clears_it_on_success() {
    let mut state = thread_ready_state();
    seed_thinking(&mut state, "current thread reasoning");
    state.context_remaining_percent = Some(66);

    state.thread = ThreadState::Resuming {
        id: "thr-old".to_owned(),
    };
    state.reduce(Action::Event(DomainEvent::ResumeFailed {
        id: "thr-old".to_owned(),
        message: "temporary failure".to_owned(),
    }));
    assert_eq!(state.thinking.entries[0].text, "current thread reasoning");
    assert_eq!(state.context_remaining_percent, Some(66));

    state.thread = ThreadState::Resuming {
        id: "thr-old".to_owned(),
    };
    state.reduce(Action::Event(DomainEvent::ResumeSucceeded {
        id: "thr-old".to_owned(),
        history: Vec::new(),
    }));
    assert!(state.thinking.entries.is_empty());
    assert!(state.thinking.visible);
    assert_eq!(state.context_remaining_percent, None);
}
