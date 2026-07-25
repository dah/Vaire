use super::*;

#[test]
fn signed_out_send_is_local_and_same_account_auto_resumes() {
    let mut state = AppState::default();
    state.reduce(Action::Event(DomainEvent::Connected { generation: 1 }));
    let effects = state.reduce(Action::Intent(Intent::SendMessage("hello".to_owned())));
    assert!(effects.is_empty());
    assert!(state.notice.as_deref().unwrap().contains("sign in"));
    let scope = AccountScope::from_chatgpt_email("a@example.com");
    state.reduce(Action::Event(DomainEvent::PreferencesLoaded(
        PreferencesV4 {
            codex: CodexPreferencesV2 {
                account_scope: scope.clone(),
                auto_resume_thread_id: Some("thr-old".to_owned()),
                ..CodexPreferencesV2::default()
            },
            ..PreferencesV4::default()
        },
    )));
    assert_eq!(
        state.reduce(Action::Event(DomainEvent::AccountLoaded(scope))),
        vec![Effect::ResumeThread {
            id: "thr-old".to_owned()
        }]
    );
}

#[test]
fn invalid_or_oversized_messages_remain_local_and_preserve_conversation_state() {
    for message in [
        "\u{1b}\u{0007}".to_owned(),
        "x".repeat(MAX_MESSAGE_BYTES + 1),
    ] {
        let mut state = thread_ready_state();
        let transcript = state.transcript.clone();
        assert!(state
            .reduce(Action::Intent(Intent::SendMessage(message)))
            .is_empty());
        assert_eq!(state.transcript, transcript);
        assert!(matches!(state.turn, TurnState::Completed { .. }));
        assert!(state.notice.is_some());
    }
}

#[test]
fn same_account_refresh_preserves_ready_and_in_flight_resume_state() {
    let mut state = thread_ready_state();
    let scope = AccountScope::from_chatgpt_email("user@example.com");
    state.turn = TurnState::Streaming {
        thread_id: "thr-active".to_owned(),
        turn_id: "turn-old".to_owned(),
    };
    state.context_remaining_percent = Some(61);
    seed_thinking(&mut state, "live reasoning");
    let ready_snapshot = state.clone();

    assert!(state
        .reduce(Action::Event(DomainEvent::AccountLoaded(scope.clone())))
        .is_empty());
    assert_eq!(state, ready_snapshot);

    state.thread = ThreadState::Resuming {
        id: "thr-active".to_owned(),
    };
    state.turn = TurnState::Idle;
    let resuming_snapshot = state.clone();
    assert!(state
        .reduce(Action::Event(DomainEvent::AccountLoaded(scope)))
        .is_empty());
    assert_eq!(state, resuming_snapshot);
}

#[test]
fn account_switch_settles_old_turn_closes_picker_and_rejects_late_events() {
    let mut state = thread_ready_state();
    state.turn = TurnState::Streaming {
        thread_id: "thr-active".to_owned(),
        turn_id: "turn-old".to_owned(),
    };
    state.context_remaining_percent = Some(61);
    seed_thinking(&mut state, "old account reasoning");
    state.popup = conversation_popup(ThreadPickerState {
        phase: ThreadPickerPhase::Ready,
        threads: vec![thread("thr-old", "Old account thread", 1)],
        selected: 0,
        confirmation: None,
        message: None,
    });
    let transcript = state.transcript.clone();

    assert!(state
        .reduce(Action::Event(DomainEvent::AccountLoaded(
            AccountScope::from_chatgpt_email("other@example.com"),
        )))
        .is_empty());
    assert!(matches!(state.turn, TurnState::Idle));
    assert!(matches!(
        state.thread,
        ThreadState::AccountMismatch { ref id } if id == "thr-active"
    ));
    assert!(state.conversation_popup().is_none());
    assert!(state.thinking.entries.is_empty());
    assert_eq!(state.context_remaining_percent, None);

    deliver_stale_old_turn_events(&mut state);
    assert!(matches!(state.turn, TurnState::Idle));
    assert_eq!(state.transcript, transcript);
    assert!(state.thinking.entries.is_empty());
    assert_eq!(state.context_remaining_percent, None);
}

#[test]
fn account_switch_closes_picker_even_when_new_scope_matches_saved_thread() {
    let mut state = thread_ready_state();
    let saved_scope = AccountScope::from_chatgpt_email("saved@example.com");
    state.thread = ThreadState::AccountMismatch {
        id: "thr-saved".to_owned(),
    };
    state.turn = TurnState::Starting;
    state.preferences.codex.account_scope = saved_scope.clone();
    state.preferences.codex.auto_resume_thread_id = Some("thr-saved".to_owned());
    state.popup = conversation_popup(ThreadPickerState {
        phase: ThreadPickerPhase::Ready,
        threads: vec![thread("thr-old", "Previous account thread", 1)],
        selected: 0,
        confirmation: None,
        message: None,
    });

    assert!(state
        .reduce(Action::Event(DomainEvent::AccountLoaded(saved_scope)))
        .is_empty());
    assert!(state.conversation_popup().is_none());
    assert!(matches!(state.turn, TurnState::Idle));
    assert!(matches!(
        state.thread,
        ThreadState::AccountMismatch { ref id } if id == "thr-saved"
    ));
}

#[test]
fn unsupported_account_detaches_saved_thread_and_rejects_late_events() {
    let mut state = thread_ready_state();
    state.turn = TurnState::Streaming {
        thread_id: "thr-active".to_owned(),
        turn_id: "turn-old".to_owned(),
    };
    state.context_remaining_percent = Some(61);
    seed_thinking(&mut state, "old account reasoning");
    state.popup = conversation_popup(ThreadPickerState {
        phase: ThreadPickerPhase::Ready,
        threads: vec![thread("thr-old", "Old account thread", 1)],
        selected: 0,
        confirmation: None,
        message: None,
    });
    let transcript = state.transcript.clone();

    state.reduce(Action::Event(DomainEvent::UnsupportedAccount(
        "unsupported account type apiKey; use ChatGPT login".to_owned(),
    )));
    assert!(matches!(state.auth, AuthState::Unsupported(_)));
    assert!(matches!(state.turn, TurnState::Idle));
    assert!(matches!(
        state.thread,
        ThreadState::AccountMismatch { ref id } if id == "thr-active"
    ));
    assert!(state.conversation_popup().is_none());
    assert!(state.thinking.entries.is_empty());
    assert_eq!(state.context_remaining_percent, None);

    deliver_stale_old_turn_events(&mut state);
    assert!(matches!(state.turn, TurnState::Idle));
    assert_eq!(state.transcript, transcript);
    assert!(state.thinking.entries.is_empty());
    assert_eq!(state.context_remaining_percent, None);
}

#[test]
fn signing_in_logout_cancels_the_active_login() {
    let mut state = AppState {
        connection: ConnectionState::Ready { generation: 1 },
        auth: AuthState::SigningIn {
            login_id: "login-active".to_owned(),
        },
        ..AppState::default()
    };

    assert_eq!(
        state.reduce(Action::Intent(Intent::Logout)),
        vec![Effect::CancelLogin {
            login_id: "login-active".to_owned(),
        }]
    );
    assert!(matches!(state.auth, AuthState::SigningIn { .. }));
}

#[test]
fn completed_login_replaces_the_pending_browser_notice() {
    let mut state = AppState {
        auth: AuthState::SigningIn {
            login_id: "login-active".to_owned(),
        },
        notice: Some(
            "Complete sign-in in the browser; if it fails, use /logout then /login device"
                .to_owned(),
        ),
        ..AppState::default()
    };

    state.reduce(Action::Event(DomainEvent::AccountLoaded(
        AccountScope::from_chatgpt_email("user@example.com"),
    )));

    assert_eq!(
        state.auth,
        AuthState::SignedIn {
            scope: AccountScope::from_chatgpt_email("user@example.com"),
        }
    );
    assert_eq!(state.notice.as_deref(), Some("Signed in to ChatGPT"));
}

#[test]
fn account_switch_and_logout_replace_and_remove_the_runtime_identity() {
    let mut state = AppState::default();

    state.reduce(Action::Event(DomainEvent::AccountLoaded(
        AccountScope::from_chatgpt_email("first@example.com"),
    )));
    assert_eq!(
        state.auth,
        AuthState::SignedIn {
            scope: AccountScope::from_chatgpt_email("first@example.com"),
        }
    );

    state.reduce(Action::Event(DomainEvent::AccountLoaded(
        AccountScope::from_chatgpt_email("second@example.com"),
    )));
    assert_eq!(
        state.auth,
        AuthState::SignedIn {
            scope: AccountScope::from_chatgpt_email("second@example.com"),
        }
    );

    state.reduce(Action::Event(DomainEvent::LoggedOut));
    assert_eq!(state.auth, AuthState::SignedOut);
}
