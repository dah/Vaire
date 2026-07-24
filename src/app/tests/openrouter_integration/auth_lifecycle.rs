use super::*;

#[test]
fn unauthorized_chat_invalidates_auth_and_failed_resume_blocks_lazy_replacement() {
    let mut state = AppState {
        active_provider: ProviderId::OpenRouter,
        ..AppState::default()
    };
    state.preferences.active_provider = ProviderId::OpenRouter;
    state.openrouter.auth = OpenRouterAuthStatus::Valid;
    state.openrouter.catalog = vec![openrouter_model("vendor/model")];
    state.selected_model = Some(ModelKey::openrouter("vendor/model").unwrap());
    state
        .preferences
        .openrouter
        .enabled_model_ids
        .insert("vendor/model".to_owned());
    state.reduce(Action::Event(DomainEvent::OpenRouterOperationFailed(
        OpenRouterFailureCategory::Unauthorized,
    )));
    assert_eq!(state.openrouter.auth, OpenRouterAuthStatus::Invalid);
    assert!(state
        .reduce(Action::Intent(Intent::SendMessage("blocked".to_owned())))
        .is_empty());

    let id = OpenRouterConversationId::new();
    state
        .preferences
        .set_auto_resume_conversation(Some(id.clone()));
    state.reduce(Action::Event(DomainEvent::OpenRouterResumeFailed {
        conversation_id: id.clone(),
    }));
    assert!(matches!(
        state.openrouter.conversation,
        OpenRouterConversationState::ResumeFailed { id: failed, .. } if failed == id
    ));
    assert_eq!(
        state.preferences.openrouter.auto_resume_conversation_id,
        Some(id)
    );
}

#[test]
fn rejected_candidate_preserves_existing_valid_auth() {
    let mut state = AppState::default();
    state.openrouter.auth = OpenRouterAuthStatus::Valid;
    state.reduce(Action::Event(DomainEvent::OpenRouterCandidateRejected(
        OpenRouterFailureCategory::Unauthorized,
    )));
    assert_eq!(state.openrouter.auth, OpenRouterAuthStatus::Valid);
    assert!(state.notice.as_deref().unwrap().contains("preserved"));
}

#[test]
fn post_store_unauthorized_catalog_failure_marks_candidate_invalid_without_losing_history() {
    let conversation_id = OpenRouterConversationId::new();
    let mut state = AppState {
        active_provider: ProviderId::OpenRouter,
        selected_model: Some(ModelKey::openrouter("vendor/model").unwrap()),
        ..AppState::default()
    };
    state.openrouter.auth = OpenRouterAuthStatus::Valid;
    state.openrouter.catalog = vec![openrouter_model("vendor/model")];
    state.openrouter.conversation = OpenRouterConversationState::Ready {
        id: conversation_id.clone(),
    };
    state.openrouter.credential_validation = OpenRouterCredentialValidation::Validating {
        operation_id: 7,
        candidate_saved: true,
    };
    state
        .preferences
        .set_auto_resume_conversation(Some(conversation_id.clone()));
    state.transcript.push(TranscriptEntry {
        provider: ProviderId::OpenRouter,
        role: TranscriptRole::Assistant,
        status: TranscriptEntryStatus::Normal,
        text: "preserve history".to_owned(),
        item_id: None,
        turn_id: None,
    });
    let original_preferences = state.preferences.clone();

    state.reduce(Action::Event(DomainEvent::OpenRouterOperationFailed(
        OpenRouterFailureCategory::Unauthorized,
    )));

    assert_eq!(state.openrouter.auth, OpenRouterAuthStatus::Invalid);
    assert_eq!(
        state.openrouter.conversation,
        OpenRouterConversationState::Ready {
            id: conversation_id
        }
    );
    assert_eq!(state.transcript[0].text, "preserve history");
    assert_eq!(state.preferences, original_preferences);
    assert_eq!(
        state.openrouter.catalog,
        vec![openrouter_model("vendor/model")]
    );
    let notice = state.notice.as_deref().unwrap();
    assert!(notice.contains("saved the credential"));
    assert!(notice.contains("provider rejected it"));
}

#[test]
fn transient_catalog_failure_does_not_invalidate_a_valid_credential() {
    let mut state = AppState::default();
    state.openrouter.auth = OpenRouterAuthStatus::Valid;
    state.reduce(Action::Event(DomainEvent::OpenRouterOperationFailed(
        OpenRouterFailureCategory::Network,
    )));
    assert_eq!(state.openrouter.auth, OpenRouterAuthStatus::Valid);
}

#[test]
fn codex_lifecycle_events_preserve_active_openrouter_state() {
    let conversation_id = OpenRouterConversationId::default();
    let turn_id = OpenRouterTurnId::new();
    let mut base = AppState {
        active_provider: ProviderId::OpenRouter,
        connection: ConnectionState::Ready { generation: 1 },
        auth: AuthState::SignedIn {
            scope: AccountScope::from_chatgpt_email("old@example.com"),
        },
        openrouter: OpenRouterState {
            auth: OpenRouterAuthStatus::Valid,
            catalog: vec![openrouter_model("vendor/model")],
            conversation: OpenRouterConversationState::Ready {
                id: conversation_id.clone(),
            },
            ..OpenRouterState::default()
        },
        turn: TurnState::OpenRouterStreaming {
            conversation_id,
            turn_id: turn_id.clone(),
        },
        selected_model: Some(ModelKey::openrouter("vendor/model").unwrap()),
        context_remaining_percent: Some(63),
        popup: Some(PopupState::Model {
            choices: vec![ModelKey::openrouter("vendor/model").unwrap()],
            selected: 0,
            search: String::new(),
        }),
        ..AppState::default()
    };
    base.preferences.codex.auto_resume_thread_id = Some("codex-saved".to_owned());
    base.preferences.codex.account_scope = AccountScope::from_chatgpt_email("new@example.com");
    base.preferences
        .openrouter
        .enabled_model_ids
        .insert("vendor/model".to_owned());
    base.transcript.push(TranscriptEntry {
        provider: ProviderId::OpenRouter,
        role: TranscriptRole::Assistant,
        status: TranscriptEntryStatus::Normal,
        text: "keep".to_owned(),
        item_id: Some("openrouter-assistant".to_owned()),
        turn_id: Some(turn_id.as_str().to_owned()),
    });
    base.thinking.entries.push(ThinkingEntry {
        provider: ProviderId::OpenRouter,
        turn_id: turn_id.as_str().to_owned(),
        item_id: "reason".to_owned(),
        kind: ThinkingKind::Summary,
        index: 0,
        text: "keep reasoning".to_owned(),
        completed: true,
    });

    for event in [
        DomainEvent::Connecting,
        DomainEvent::Connected { generation: 2 },
        DomainEvent::CatalogLoaded(vec![model("codex-new", true, &["high"], "high")]),
        DomainEvent::ConnectionFailed("codex failed".to_owned()),
        DomainEvent::ProcessExited("codex exited".to_owned()),
        DomainEvent::UnsupportedAccount("unsupported".to_owned()),
        DomainEvent::LoggedOut,
        DomainEvent::AccountLoaded(AccountScope::from_chatgpt_email("new@example.com")),
    ] {
        let mut state = base.clone();
        assert!(state.reduce(Action::Event(event)).is_empty());
        assert_eq!(state.active_provider, ProviderId::OpenRouter);
        assert_eq!(state.openrouter.conversation, base.openrouter.conversation);
        assert_eq!(state.turn, base.turn);
        assert_eq!(state.transcript, base.transcript);
        assert_eq!(state.thinking, base.thinking);
        assert_eq!(state.context_remaining_percent, Some(63));
        assert_eq!(state.popup, base.popup);
    }

    let mut account = base;
    assert!(account
        .reduce(Action::Event(DomainEvent::AccountLoaded(
            AccountScope::from_chatgpt_email("new@example.com"),
        )))
        .is_empty());
    assert!(!matches!(account.thread, ThreadState::Resuming { .. }));
}
