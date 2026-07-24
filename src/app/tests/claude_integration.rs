use super::*;
use crate::claude::{ClaudeModelMetadata, ClaudeSessionLifecycle, ClaudeTurnRecord};
use crate::persistence::ClaudePreferencesV3;

fn session_id(value: &str) -> ClaudeSessionId {
    value.parse().unwrap()
}

fn turn_id(value: &str) -> ClaudeTurnId {
    value.parse().unwrap()
}

fn ready_claude_state(alias: ClaudeModelAlias) -> AppState {
    let mut state = AppState {
        active_provider: ProviderId::Claude,
        claude: ClaudeState {
            availability: ClaudeAvailability::Ready,
            auth: ClaudeAuthStatus::Subscription,
            ..ClaudeState::default()
        },
        selected_model: Some(ModelKey::claude(alias.as_str()).unwrap()),
        preferences: PreferencesV3 {
            active_provider: ProviderId::Claude,
            claude: ClaudePreferencesV3 {
                selected_model_alias: Some(alias),
                ..ClaudePreferencesV3::default()
            },
            ..PreferencesV3::default()
        },
        ..AppState::default()
    };
    state.sync_active_selection_preferences();
    state
}

fn saved_session(alias: ClaudeModelAlias) -> ClaudeSessionV1 {
    ClaudeSessionV1 {
        version: 1,
        session_id: session_id("00000000-0000-4000-8000-000000000001"),
        created_at_ms: 1,
        updated_at_ms: 2,
        title: "saved".to_owned(),
        selected_model: alias,
        resolved_model: Some(ClaudeModelMetadata {
            id: "claude-resolved".to_owned(),
            display_name: Some("Resolved Claude".to_owned()),
        }),
        lifecycle: ClaudeSessionLifecycle::Established,
        turns: vec![
            ClaudeTurnRecord {
                id: turn_id("00000000-0000-4000-8000-000000000010"),
                requested_model: alias,
                user_text: "complete user".to_owned(),
                assistant_text: Some("complete assistant".to_owned()),
                incomplete_assistant_text: None,
                outcome: ClaudeTurnOutcome::Completed,
            },
            ClaudeTurnRecord {
                id: turn_id("00000000-0000-4000-8000-000000000011"),
                requested_model: alias,
                user_text: "failed user".to_owned(),
                assistant_text: None,
                incomplete_assistant_text: Some("failed partial".to_owned()),
                outcome: ClaudeTurnOutcome::Failed,
            },
            ClaudeTurnRecord {
                id: turn_id("00000000-0000-4000-8000-000000000012"),
                requested_model: alias,
                user_text: "interrupted user".to_owned(),
                assistant_text: None,
                incomplete_assistant_text: None,
                outcome: ClaudeTurnOutcome::Interrupted,
            },
        ],
    }
}

#[test]
fn changing_claude_alias_is_a_blank_boundary_and_clears_the_pointer() {
    let id = session_id("00000000-0000-4000-8000-000000000001");
    let mut state = ready_claude_state(ClaudeModelAlias::Sonnet);
    state.claude.conversation = ClaudeConversationState::Ready { id: id.clone() };
    state.preferences.set_auto_resume_claude_session(Some(id));
    state.transcript.push(TranscriptEntry {
        provider: ProviderId::Claude,
        role: TranscriptRole::Assistant,
        status: TranscriptEntryStatus::Normal,
        text: "old".to_owned(),
        item_id: None,
        turn_id: None,
    });

    let effects = state.reduce(Action::Intent(Intent::SelectProviderModel(
        ModelKey::claude("opus").unwrap(),
    )));

    assert_eq!(state.claude.conversation, ClaudeConversationState::None);
    assert!(state.preferences.claude.auto_resume_session_id.is_none());
    assert!(state.transcript.is_empty());
    assert_eq!(
        state.preferences.claude.selected_model_alias,
        Some(ClaudeModelAlias::Opus)
    );
    assert!(matches!(effects.as_slice(), [Effect::Persist(_)]));
}

#[test]
fn resume_restores_saved_alias_and_failed_partial_display_history() {
    let session = saved_session(ClaudeModelAlias::Haiku);
    let session_id = session.session_id.clone();
    let mut state = ready_claude_state(ClaudeModelAlias::Opus);
    state.popup = conversation_popup(ThreadPickerState {
        phase: ThreadPickerPhase::Resuming {
            provider: ProviderId::Claude,
            id: session_id.as_str().to_owned(),
        },
        threads: Vec::new(),
        selected: 0,
        confirmation: None,
        message: None,
    });

    let effects = state.reduce(Action::Event(DomainEvent::ClaudeSessionRestored {
        session,
        automatic: false,
    }));

    assert_eq!(
        state.selected_model,
        Some(ModelKey::claude(ClaudeModelAlias::Haiku.as_str()).unwrap())
    );
    assert_eq!(
        state.preferences.claude.selected_model_alias,
        Some(ClaudeModelAlias::Haiku)
    );
    assert_eq!(state.transcript.len(), 5);
    assert_eq!(
        state.transcript[3].status,
        TranscriptEntryStatus::FailedIncomplete
    );
    assert_eq!(state.transcript[3].text, "failed partial");
    assert!(state
        .transcript
        .iter()
        .all(|entry| entry.text != "interrupted assistant"));
    assert!(matches!(effects.as_slice(), [Effect::Persist(_)]));
}

#[test]
fn lazy_session_registration_preserves_pending_user_and_starting_state() {
    let id = session_id("00000000-0000-4000-8000-000000000001");
    let mut state = ready_claude_state(ClaudeModelAlias::Sonnet);

    let send = state.reduce(Action::Intent(Intent::SendMessage("hello".to_owned())));
    assert!(matches!(
        send.as_slice(),
        [Effect::SendClaudeMessage { text }] if text == "hello"
    ));
    assert_eq!(state.turn, TurnState::Starting);
    assert_eq!(state.transcript.len(), 1);

    let effects = state.reduce(Action::Event(DomainEvent::ClaudeSessionStarted {
        session_id: id.clone(),
    }));
    assert_eq!(state.turn, TurnState::Starting);
    assert_eq!(state.transcript.len(), 1);
    assert_eq!(state.transcript[0].text, "hello");
    assert_eq!(
        state.claude.conversation,
        ClaudeConversationState::Ready { id }
    );
    assert!(matches!(effects.as_slice(), [Effect::Persist(_)]));
}

#[test]
fn explicit_new_replaces_claude_state_only_after_success() {
    let old = session_id("00000000-0000-4000-8000-000000000001");
    let new = session_id("00000000-0000-4000-8000-000000000002");
    let mut state = ready_claude_state(ClaudeModelAlias::Sonnet);
    state.claude.conversation = ClaudeConversationState::Ready { id: old.clone() };
    state
        .preferences
        .set_auto_resume_claude_session(Some(old.clone()));
    state.transcript.push(TranscriptEntry {
        provider: ProviderId::Claude,
        role: TranscriptRole::Assistant,
        status: TranscriptEntryStatus::Normal,
        text: "old history".to_owned(),
        item_id: None,
        turn_id: None,
    });

    let effects = state.reduce(Action::Intent(Intent::NewThread));
    assert_eq!(effects, vec![Effect::StartNewClaudeSession]);
    assert!(state.pending_new_claude_session);
    assert_eq!(
        state.claude.conversation,
        ClaudeConversationState::Ready { id: old }
    );
    assert_eq!(state.transcript.len(), 1);

    let persisted = state.reduce(Action::Event(DomainEvent::ClaudeSessionStarted {
        session_id: new.clone(),
    }));
    assert!(!state.pending_new_claude_session);
    assert_eq!(
        state.claude.conversation,
        ClaudeConversationState::Ready { id: new.clone() }
    );
    assert_eq!(state.preferences.claude.auto_resume_session_id, Some(new));
    assert!(state.transcript.is_empty());
    assert!(matches!(persisted.as_slice(), [Effect::Persist(_)]));
}

#[test]
fn explicit_new_pointer_uncertainty_binds_the_new_uuid() {
    let old = session_id("00000000-0000-4000-8000-000000000001");
    let new = session_id("00000000-0000-4000-8000-000000000002");
    let mut state = ready_claude_state(ClaudeModelAlias::Sonnet);
    state.claude.conversation = ClaudeConversationState::Ready { id: old.clone() };
    state.claude.resolved_model = Some(ClaudeModelMetadata {
        id: "old-resolved-model".to_owned(),
        display_name: None,
    });
    state.preferences.set_auto_resume_claude_session(Some(old));
    state.transcript.push(TranscriptEntry {
        provider: ProviderId::Claude,
        role: TranscriptRole::Assistant,
        status: TranscriptEntryStatus::Normal,
        text: "old history".to_owned(),
        item_id: None,
        turn_id: None,
    });

    assert_eq!(
        state.reduce(Action::Intent(Intent::NewThread)),
        vec![Effect::StartNewClaudeSession]
    );
    let effects = state.reduce(Action::Event(DomainEvent::ClaudeSessionCreationUncertain {
        session_id: new.clone(),
        message: "pointer durability is uncertain".to_owned(),
    }));

    assert!(!state.pending_new_claude_session);
    assert!(matches!(
        state.claude.conversation,
        ClaudeConversationState::CreationUncertain { ref id, .. } if id == &new
    ));
    assert_eq!(state.preferences.claude.auto_resume_session_id, Some(new));
    assert!(state.transcript.is_empty());
    assert!(state.claude.resolved_model.is_none());
    assert_eq!(state.turn, TurnState::Idle);
    assert!(state
        .notice
        .as_deref()
        .is_some_and(|notice| notice.contains("session creation is uncertain")));
    assert!(matches!(effects.as_slice(), [Effect::Persist(_)]));
}

#[test]
fn automatic_resume_failure_preserves_the_saved_claude_pointer() {
    let id = session_id("00000000-0000-4000-8000-000000000001");
    let mut state = ready_claude_state(ClaudeModelAlias::Sonnet);
    state
        .preferences
        .set_auto_resume_claude_session(Some(id.clone()));

    state.reduce(Action::Event(DomainEvent::ClaudeResumeFailed {
        session_id: id.clone(),
        message: "unavailable".to_owned(),
    }));

    assert_eq!(
        state.preferences.claude.auto_resume_session_id,
        Some(id.clone())
    );
    assert!(matches!(
        state.claude.conversation,
        ClaudeConversationState::ResumeFailed { id: ref failed, .. } if failed == &id
    ));
}

#[test]
fn picker_resumes_claude_by_registered_id_without_a_provisional_alias() {
    let id = session_id("00000000-0000-4000-8000-000000000001");
    let mut state = ready_claude_state(ClaudeModelAlias::Opus);
    state.popup = conversation_popup(ThreadPickerState {
        phase: ThreadPickerPhase::Ready,
        threads: vec![ThreadChoice {
            provider: ProviderId::Claude,
            id: id.as_str().to_owned(),
            title: "saved Claude".to_owned(),
            updated_at: 1,
        }],
        selected: 0,
        confirmation: None,
        message: None,
    });

    let effects = state.reduce(Action::Intent(Intent::ThreadPickerSelect));
    assert_eq!(
        effects,
        vec![Effect::SwitchClaudeSession { id: id.clone() }]
    );
    assert!(matches!(
        state.conversation_popup().map(|picker| &picker.phase),
        Some(ThreadPickerPhase::Resuming {
            provider: ProviderId::Claude,
            id: selected,
        }) if selected == id.as_str()
    ));
}

#[test]
fn picker_deletes_only_the_inactive_claude_registration() {
    let active = session_id("00000000-0000-4000-8000-000000000001");
    let inactive = session_id("00000000-0000-4000-8000-000000000002");
    let mut state = ready_claude_state(ClaudeModelAlias::Sonnet);
    state.claude.conversation = ClaudeConversationState::Ready { id: active.clone() };
    state
        .preferences
        .set_auto_resume_claude_session(Some(active.clone()));
    state.popup = conversation_popup(ThreadPickerState {
        phase: ThreadPickerPhase::Ready,
        threads: vec![
            ThreadChoice {
                provider: ProviderId::Claude,
                id: active.as_str().to_owned(),
                title: "active".to_owned(),
                updated_at: 2,
            },
            ThreadChoice {
                provider: ProviderId::Claude,
                id: inactive.as_str().to_owned(),
                title: "inactive".to_owned(),
                updated_at: 1,
            },
        ],
        selected: 1,
        confirmation: None,
        message: None,
    });

    state.reduce(Action::Intent(Intent::ThreadPickerRequestDelete));
    let effects = state.reduce(Action::Intent(Intent::ThreadPickerConfirmDelete));
    assert_eq!(
        effects,
        vec![Effect::DeleteClaudeSessions {
            ids: vec![inactive]
        }]
    );
}

#[test]
fn claude_operation_failure_settles_only_an_active_claude_send() {
    let session = session_id("00000000-0000-4000-8000-000000000001");
    let turn = turn_id("00000000-0000-4000-8000-000000000010");
    let mut state = ready_claude_state(ClaudeModelAlias::Sonnet);
    state.turn = TurnState::ClaudeStreaming {
        session_id: session,
        turn_id: turn.clone(),
    };
    state.reduce(Action::Event(DomainEvent::ClaudeOperationFailed(
        ClaudeError::new(
            crate::claude::ClaudeFailureStage::Protocol,
            crate::claude::ClaudeFailureCategory::Protocol,
        ),
    )));
    assert!(matches!(
        state.turn,
        TurnState::Failed {
            turn_id: Some(ref id),
            ..
        } if id == turn.as_str()
    ));

    state.turn = TurnState::ClaudeStreaming {
        session_id: session_id("00000000-0000-4000-8000-000000000001"),
        turn_id: turn.clone(),
    };
    state.claude.auth_operation = ClaudeAuthOperation::Checking { operation_id: 1 };
    state.reduce(Action::Event(DomainEvent::ClaudeOperationFailed(
        ClaudeError::new(
            crate::claude::ClaudeFailureStage::Auth,
            crate::claude::ClaudeFailureCategory::Unavailable,
        ),
    )));
    assert!(matches!(state.turn, TurnState::ClaudeStreaming { .. }));
    assert_eq!(state.claude.auth_operation, ClaudeAuthOperation::Idle);
    assert!(state
        .notice
        .as_deref()
        .is_some_and(|notice| notice.contains("subscription status check failed")));

    state.turn = TurnState::Completed {
        turn_id: "settled".to_owned(),
    };
    state.reduce(Action::Event(DomainEvent::ClaudeOperationFailed(
        ClaudeError::new(
            crate::claude::ClaudeFailureStage::Auth,
            crate::claude::ClaudeFailureCategory::Unavailable,
        ),
    )));
    assert_eq!(
        state.turn,
        TurnState::Completed {
            turn_id: "settled".to_owned()
        }
    );
}

#[test]
fn claude_login_uses_native_auth_and_correlates_its_terminal_status() {
    let mut state = ready_claude_state(ClaudeModelAlias::Sonnet);
    state.popup = Some(PopupState::Auth {
        mode: AuthPopupMode::Login,
        selected: ProviderId::Claude,
    });
    let effects = state.reduce(Action::Intent(Intent::PopupSelect));
    assert_eq!(effects, vec![Effect::LoginClaude]);
    assert!(state.popup.is_none());

    let request = ClaudeAuthRequest {
        operation_id: 7,
        action: crate::claude::ClaudeAuthAction::Login,
    };
    state.reduce(Action::Event(DomainEvent::ClaudeAuthRequested(request)));
    assert_eq!(state.pending_claude_auth_request(), Some(&request));
    assert_eq!(state.claude.auth, ClaudeAuthStatus::Unverified);
    assert!(state
        .notice
        .as_deref()
        .is_some_and(|notice| notice.contains("browser")));
    assert!(state
        .reduce(Action::Intent(Intent::SendMessage("queued".to_owned())))
        .is_empty());
    assert!(state
        .notice
        .as_deref()
        .is_some_and(|notice| notice.contains("authentication operation")));

    state.reduce(Action::Event(DomainEvent::ClaudeAuthChanged(
        ClaudeAuthStatus::Subscription,
    )));
    assert_eq!(state.pending_claude_auth_request(), None);
    assert_eq!(state.claude.auth_operation, ClaudeAuthOperation::Idle);
    assert_eq!(
        state.notice.as_deref(),
        Some("Claude subscription is connected")
    );
}

#[test]
fn claude_auth_terminal_states_clear_control_work_and_use_subscription_wording() {
    let mut state = ready_claude_state(ClaudeModelAlias::Sonnet);
    state.claude.auth_operation = ClaudeAuthOperation::Checking { operation_id: 3 };
    state.reduce(Action::Event(DomainEvent::ClaudeAuthChanged(
        ClaudeAuthStatus::Unsupported,
    )));
    assert_eq!(state.claude.auth_operation, ClaudeAuthOperation::Idle);
    assert!(state.notice.as_deref().is_some_and(|notice| notice
        .contains("unsupported authentication source")
        && notice.contains("subscription")));

    state.claude.auth_operation = ClaudeAuthOperation::AwaitingTerminal {
        request: ClaudeAuthRequest {
            operation_id: 4,
            action: crate::claude::ClaudeAuthAction::Logout,
        },
    };
    state.pending_new_claude_session = true;
    state.reduce(Action::Event(DomainEvent::ClaudeAuthChanged(
        ClaudeAuthStatus::SignedOut,
    )));
    assert_eq!(state.claude.auth_operation, ClaudeAuthOperation::Idle);
    assert!(!state.pending_new_claude_session);
    assert_eq!(state.notice.as_deref(), Some("Claude is signed out"));
}

#[test]
fn uncertain_creation_preserves_pointer_and_blocks_sends() {
    let session = session_id("00000000-0000-4000-8000-000000000001");
    let turn = turn_id("00000000-0000-4000-8000-000000000010");
    let mut state = ready_claude_state(ClaudeModelAlias::Sonnet);
    state.claude.conversation = ClaudeConversationState::Ready {
        id: session.clone(),
    };
    state.turn = TurnState::ClaudeStreaming {
        session_id: session.clone(),
        turn_id: turn.clone(),
    };

    let effects = state.reduce(Action::Event(DomainEvent::ClaudeTurnFinished {
        session_id: session.clone(),
        turn_id: turn,
        outcome: ClaudeTurnOutcome::Interrupted,
        assistant_text: None,
        incomplete_assistant_text: None,
        creation_uncertain: true,
        failure: None,
    }));

    assert!(matches!(
        state.claude.conversation,
        ClaudeConversationState::CreationUncertain { ref id, .. } if id == &session
    ));
    assert_eq!(
        state.preferences.claude.auto_resume_session_id,
        Some(session)
    );
    assert!(matches!(state.turn, TurnState::Interrupted { .. }));
    assert!(matches!(effects.as_slice(), [Effect::Persist(_)]));
    let blocked = state.reduce(Action::Intent(Intent::SendMessage("again".to_owned())));
    assert!(blocked.is_empty());
    assert!(state
        .notice
        .as_deref()
        .is_some_and(|message| message.contains("/resume or /new")));
}

#[test]
fn completed_snapshot_appends_suffix_and_failed_partial_is_display_only() {
    let session = session_id("00000000-0000-4000-8000-000000000001");
    let completed = turn_id("00000000-0000-4000-8000-000000000010");
    let mut state = ready_claude_state(ClaudeModelAlias::Sonnet);
    state.claude.conversation = ClaudeConversationState::Ready {
        id: session.clone(),
    };
    state.turn = TurnState::ClaudeStreaming {
        session_id: session.clone(),
        turn_id: completed.clone(),
    };
    state.reduce(Action::Event(DomainEvent::ClaudeDelta {
        session_id: session.clone(),
        turn_id: completed.clone(),
        delta: "hello".to_owned(),
    }));
    state.reduce(Action::Event(DomainEvent::ClaudeTurnFinished {
        session_id: session.clone(),
        turn_id: completed.clone(),
        outcome: ClaudeTurnOutcome::Completed,
        assistant_text: Some("hello world".to_owned()),
        incomplete_assistant_text: None,
        creation_uncertain: false,
        failure: None,
    }));
    assert_eq!(state.transcript.last().unwrap().text, "hello world");
    assert!(matches!(state.turn, TurnState::Completed { .. }));

    let failed = turn_id("00000000-0000-4000-8000-000000000011");
    state.turn = TurnState::ClaudeStreaming {
        session_id: session.clone(),
        turn_id: failed.clone(),
    };
    state.reduce(Action::Event(DomainEvent::ClaudeDelta {
        session_id: session.clone(),
        turn_id: failed.clone(),
        delta: "partial".to_owned(),
    }));
    state.reduce(Action::Event(DomainEvent::ClaudeTurnFinished {
        session_id: session,
        turn_id: failed,
        outcome: ClaudeTurnOutcome::Failed,
        assistant_text: None,
        incomplete_assistant_text: Some("partial result".to_owned()),
        creation_uncertain: false,
        failure: None,
    }));
    let entry = state.transcript.last().unwrap();
    assert_eq!(entry.text, "partial result");
    assert_eq!(entry.status, TranscriptEntryStatus::FailedIncomplete);
    assert!(matches!(state.turn, TurnState::Failed { .. }));
}

#[test]
fn automatic_uncertain_restore_stays_blocked_and_failed_explicit_recovery_reblocks() {
    let mut session = saved_session(ClaudeModelAlias::Sonnet);
    session.lifecycle = ClaudeSessionLifecycle::CreationUncertain;
    let id = session.session_id.clone();
    let mut automatic = ready_claude_state(ClaudeModelAlias::Sonnet);
    automatic
        .preferences
        .set_auto_resume_claude_session(Some(id.clone()));
    automatic.reduce(Action::Event(DomainEvent::ClaudeSessionRestored {
        session: session.clone(),
        automatic: true,
    }));
    assert!(matches!(
        automatic.claude.conversation,
        ClaudeConversationState::CreationUncertain { ref id, .. } if id == &session.session_id
    ));
    assert!(automatic
        .reduce(Action::Intent(Intent::SendMessage("blocked".to_owned())))
        .is_empty());

    let mut explicit = ready_claude_state(ClaudeModelAlias::Sonnet);
    explicit.popup = conversation_popup(ThreadPickerState {
        phase: ThreadPickerPhase::Ready,
        threads: vec![ThreadChoice {
            provider: ProviderId::Claude,
            id: id.as_str().to_owned(),
            title: "uncertain".to_owned(),
            updated_at: 1,
        }],
        selected: 0,
        confirmation: None,
        message: None,
    });
    explicit.reduce(Action::Intent(Intent::ThreadPickerSelect));
    let expected_id = id.clone();
    explicit.reduce(Action::Event(DomainEvent::ClaudeSessionRestored {
        session,
        automatic: false,
    }));
    assert!(matches!(
        explicit.claude.conversation,
        ClaudeConversationState::Ready { ref id } if id == &expected_id
    ));

    let send = explicit.reduce(Action::Intent(Intent::SendMessage("retry".to_owned())));
    assert!(matches!(
        send.as_slice(),
        [Effect::SendClaudeMessage { text }] if text == "retry"
    ));
    let recovery_turn = turn_id("00000000-0000-4000-8000-000000000099");
    explicit.reduce(Action::Event(DomainEvent::ClaudeTurnStarted {
        session_id: expected_id.clone(),
        turn_id: recovery_turn.clone(),
    }));
    explicit.reduce(Action::Event(DomainEvent::ClaudeTurnFinished {
        session_id: expected_id.clone(),
        turn_id: recovery_turn,
        outcome: ClaudeTurnOutcome::Failed,
        assistant_text: None,
        incomplete_assistant_text: None,
        creation_uncertain: true,
        failure: Some(ClaudeError::new(
            crate::claude::ClaudeFailureStage::Spawn,
            crate::claude::ClaudeFailureCategory::Unavailable,
        )),
    }));
    assert!(matches!(
        explicit.claude.conversation,
        ClaudeConversationState::CreationUncertain { ref id, .. } if id == &expected_id
    ));
    assert!(explicit
        .reduce(Action::Intent(Intent::SendMessage(
            "blocked again".to_owned()
        )))
        .is_empty());
    assert!(explicit
        .notice
        .as_deref()
        .is_some_and(|message| message.contains("/resume or /new")));
}

#[test]
fn completed_event_with_store_failure_is_displayed_only_as_failed_incomplete() {
    let session = session_id("00000000-0000-4000-8000-000000000001");
    let turn = turn_id("00000000-0000-4000-8000-000000000099");
    let mut state = ready_claude_state(ClaudeModelAlias::Sonnet);
    state.claude.conversation = ClaudeConversationState::Ready {
        id: session.clone(),
    };
    state.turn = TurnState::ClaudeStreaming {
        session_id: session.clone(),
        turn_id: turn.clone(),
    };
    state.reduce(Action::Event(DomainEvent::ClaudeDelta {
        session_id: session.clone(),
        turn_id: turn.clone(),
        delta: "answer".to_owned(),
    }));
    state.reduce(Action::Event(DomainEvent::ClaudeTurnFinished {
        session_id: session,
        turn_id: turn,
        outcome: ClaudeTurnOutcome::Completed,
        assistant_text: Some("answer".to_owned()),
        incomplete_assistant_text: None,
        creation_uncertain: false,
        failure: Some(ClaudeError::new(
            crate::claude::ClaudeFailureStage::Store,
            crate::claude::ClaudeFailureCategory::CorruptStore,
        )),
    }));

    assert!(matches!(state.turn, TurnState::Failed { .. }));
    assert_eq!(
        state.transcript.last().map(|entry| entry.status),
        Some(TranscriptEntryStatus::FailedIncomplete)
    );
}
