use super::*;
use crate::openrouter::{OpenRouterAuthStatus, OpenRouterFailureCategory, OpenRouterModel};

fn openrouter_model(id: &str) -> OpenRouterModel {
    OpenRouterModel {
        id: id.to_owned(),
        name: Some(id.to_owned()),
        context_length: Some(4096),
    }
}

#[test]
fn cross_provider_model_selection_is_a_hard_blank_boundary() {
    let mut state = AppState {
        models: vec![model("codex", true, &["high"], "high")],
        selected_model: Some(ModelKey::codex("codex").unwrap()),
        selected_reasoning: Some("high".to_owned()),
        thread: ThreadState::Ready {
            id: "thr-preserved-remotely".to_owned(),
        },
        ..AppState::default()
    };
    state
        .preferences
        .set_auto_resume_thread(Some("thr-preserved-remotely".to_owned()));
    state.openrouter.catalog = vec![openrouter_model("vendor/model")];
    state
        .preferences
        .openrouter
        .enabled_model_ids
        .insert("vendor/model".to_owned());
    state.transcript.push(TranscriptEntry {
        provider: crate::provider::ProviderId::Codex,
        role: TranscriptRole::Assistant,
        text: "must not cross".to_owned(),
        item_id: Some("item".to_owned()),
        turn_id: Some("turn".to_owned()),
    });
    state.thinking.entries.push(ThinkingEntry {
        provider: crate::provider::ProviderId::Codex,
        turn_id: "turn".to_owned(),
        item_id: "reason".to_owned(),
        kind: ThinkingKind::Summary,
        index: 0,
        text: "must not cross".to_owned(),
        completed: true,
    });
    state.context_remaining_percent = Some(42);

    let effects = state.reduce(Action::Intent(Intent::SelectProviderModel(
        ModelKey::openrouter("vendor/model").unwrap(),
    )));

    assert_eq!(state.active_provider, ProviderId::OpenRouter);
    assert!(matches!(
        state.openrouter.conversation,
        OpenRouterConversationState::None
    ));
    assert!(state.transcript.is_empty());
    assert!(state.thinking.entries.is_empty());
    assert_eq!(state.context_remaining_percent, None);
    assert_eq!(state.preferences.codex.auto_resume_thread_id, None);
    assert_eq!(
        state.preferences.openrouter.auto_resume_conversation_id,
        None
    );
    assert_eq!(effects, vec![Effect::Persist(state.preferences.clone())]);
    assert_eq!(
        state.notice.as_deref(),
        Some("Switching provider starts a new conversation; use /resume for history.")
    );
}

#[test]
fn cross_provider_model_selection_rejects_active_turns_in_both_directions() {
    let mut codex = thread_ready_state();
    codex.openrouter.catalog = vec![openrouter_model("vendor/model")];
    codex
        .preferences
        .openrouter
        .enabled_model_ids
        .insert("vendor/model".to_owned());
    codex.turn = TurnState::Streaming {
        thread_id: "thr-active".to_owned(),
        turn_id: "turn-active".to_owned(),
    };
    let before = codex.clone();
    assert!(codex
        .reduce(Action::Intent(Intent::SelectProviderModel(
            ModelKey::openrouter("vendor/model").unwrap(),
        )))
        .is_empty());
    assert_eq!(codex.active_provider, before.active_provider);
    assert_eq!(codex.transcript, before.transcript);
    assert_eq!(codex.thread, before.thread);

    let mut openrouter = AppState {
        active_provider: ProviderId::OpenRouter,
        models: vec![model("codex", true, &["high"], "high")],
        turn: TurnState::OpenRouterStreaming {
            conversation_id: OpenRouterConversationId::default(),
            turn_id: OpenRouterTurnId::new(),
        },
        ..AppState::default()
    };
    openrouter.preferences.codex.model_id = Some("codex".to_owned());
    let before = openrouter.clone();
    assert!(openrouter
        .reduce(Action::Intent(Intent::SelectProviderModel(
            ModelKey::codex("codex").unwrap(),
        )))
        .is_empty());
    assert_eq!(openrouter.active_provider, before.active_provider);
    assert_eq!(openrouter.turn, before.turn);
}

#[test]
fn cross_provider_blank_boundary_survives_restart_until_lazy_first_send() {
    let mut state = AppState {
        models: vec![model("codex", true, &["high"], "high")],
        selected_model: Some(ModelKey::codex("codex").unwrap()),
        selected_reasoning: Some("high".to_owned()),
        thread: ThreadState::Ready {
            id: "old-codex".to_owned(),
        },
        transcript: vec![TranscriptEntry {
            provider: ProviderId::Codex,
            role: TranscriptRole::Assistant,
            text: "must stay behind".to_owned(),
            item_id: None,
            turn_id: None,
        }],
        ..AppState::default()
    };
    state.openrouter.catalog = vec![openrouter_model("vendor/model")];
    state
        .preferences
        .openrouter
        .enabled_model_ids
        .insert("vendor/model".to_owned());
    state.reduce(Action::Intent(Intent::SelectProviderModel(
        ModelKey::openrouter("vendor/model").unwrap(),
    )));
    let persisted = state.preferences.clone();
    assert!(state.transcript.is_empty());
    assert!(matches!(
        state.openrouter.conversation,
        OpenRouterConversationState::None
    ));

    let mut restarted = AppState::default();
    restarted.reduce(Action::Event(DomainEvent::PreferencesLoaded(persisted)));
    restarted.reduce(Action::Event(DomainEvent::OpenRouterStartup {
        auth: OpenRouterAuthStatus::Valid,
        catalog: vec![openrouter_model("vendor/model")],
    }));
    assert!(restarted.transcript.is_empty());
    assert!(matches!(
        restarted.openrouter.conversation,
        OpenRouterConversationState::None
    ));
    assert_eq!(
        restarted.reduce(Action::Intent(Intent::SendMessage(
            "first destination message".to_owned(),
        ))),
        vec![Effect::SendOpenRouterMessage {
            text: "first destination message".to_owned(),
        }]
    );
    assert!(matches!(restarted.turn, TurnState::Starting));
    assert_eq!(restarted.transcript.len(), 1);
    assert_eq!(restarted.transcript[0].text, "first destination message");
}

#[test]
fn openrouter_catalog_arriving_around_codex_catalog_never_overwrites_codex_selection() {
    let mut state = AppState {
        models: vec![model("codex", true, &["high"], "high")],
        selected_model: Some(ModelKey::codex("codex").unwrap()),
        selected_reasoning: Some("high".to_owned()),
        ..AppState::default()
    };
    state.preferences.codex.model_id = Some("codex".to_owned());
    state.preferences.codex.reasoning_effort = Some("high".to_owned());

    // The background OpenRouter result may arrive before the active Codex refresh.
    state.reduce(Action::Event(DomainEvent::OpenRouterCatalogLoaded(vec![
        openrouter_model("vendor/model"),
    ])));
    assert_eq!(
        state.selected_model,
        Some(ModelKey::codex("codex").unwrap())
    );
    assert_eq!(state.selected_reasoning.as_deref(), Some("high"));
    state.reduce(Action::Event(DomainEvent::CatalogLoaded(vec![model(
        "codex",
        true,
        &["high"],
        "high",
    )])));
    assert_eq!(
        state.selected_model,
        Some(ModelKey::codex("codex").unwrap())
    );
    assert_eq!(state.selected_reasoning.as_deref(), Some("high"));

    // The same background result arriving after the active catalog is equally inert.
    state.reduce(Action::Event(DomainEvent::OpenRouterCatalogLoaded(vec![
        openrouter_model("vendor/other"),
    ])));
    assert_eq!(
        state.selected_model,
        Some(ModelKey::codex("codex").unwrap())
    );
    assert_eq!(state.selected_reasoning.as_deref(), Some("high"));
}

#[test]
fn codex_catalog_arriving_around_openrouter_catalog_never_overwrites_openrouter_selection() {
    let mut state = AppState {
        active_provider: ProviderId::OpenRouter,
        selected_model: Some(ModelKey::openrouter("vendor/model").unwrap()),
        selected_reasoning: None,
        ..AppState::default()
    };
    state.preferences.active_provider = ProviderId::OpenRouter;
    state.preferences.openrouter.selected_model_id = Some("vendor/model".to_owned());
    state
        .preferences
        .openrouter
        .enabled_model_ids
        .insert("vendor/model".to_owned());

    // The background Codex result may arrive before the active OpenRouter refresh.
    state.reduce(Action::Event(DomainEvent::CatalogLoaded(vec![model(
        "codex-new",
        true,
        &["medium"],
        "medium",
    )])));
    assert_eq!(
        state.selected_model,
        Some(ModelKey::openrouter("vendor/model").unwrap())
    );
    assert_eq!(state.selected_reasoning, None);
    state.reduce(Action::Event(DomainEvent::OpenRouterCatalogLoaded(vec![
        openrouter_model("vendor/model"),
    ])));
    assert_eq!(
        state.selected_model,
        Some(ModelKey::openrouter("vendor/model").unwrap())
    );
    assert_eq!(state.selected_reasoning, None);

    // The same background result arriving after the active catalog is equally inert.
    state.reduce(Action::Event(DomainEvent::CatalogLoaded(vec![model(
        "codex-late",
        true,
        &["low"],
        "low",
    )])));
    assert_eq!(
        state.selected_model,
        Some(ModelKey::openrouter("vendor/model").unwrap())
    );
    assert_eq!(state.selected_reasoning, None);
}

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
fn same_provider_selection_and_catalog_draft_preserve_history_and_stale_ids() {
    let conversation_id = OpenRouterConversationId::new();
    let mut state = AppState {
        active_provider: ProviderId::OpenRouter,
        ..AppState::default()
    };
    state.preferences.active_provider = ProviderId::OpenRouter;
    state.openrouter.auth = OpenRouterAuthStatus::Valid;
    state.openrouter.catalog = vec![openrouter_model("vendor/a"), openrouter_model("vendor/b")];
    state.preferences.openrouter.enabled_model_ids.extend([
        "vendor/a".to_owned(),
        "vendor/b".to_owned(),
        "stale/model".to_owned(),
    ]);
    state.selected_model = Some(ModelKey::openrouter("vendor/a").unwrap());
    state.openrouter.conversation = OpenRouterConversationState::Ready {
        id: conversation_id.clone(),
    };
    state
        .preferences
        .set_auto_resume_conversation(Some(conversation_id.clone()));
    state.transcript.push(TranscriptEntry {
        provider: crate::provider::ProviderId::OpenRouter,
        role: TranscriptRole::User,
        text: "retained".to_owned(),
        item_id: None,
        turn_id: None,
    });

    state.reduce(Action::Intent(Intent::SelectProviderModel(
        ModelKey::openrouter("vendor/b").unwrap(),
    )));
    assert!(matches!(
        state.openrouter.conversation,
        OpenRouterConversationState::Ready { ref id } if id == &conversation_id
    ));
    assert_eq!(state.transcript[0].text, "retained");
    assert_eq!(
        state.preferences.openrouter.auto_resume_conversation_id,
        Some(conversation_id)
    );

    state.popup = Some(PopupState::OpenRouterCatalog {
        models: state.openrouter.catalog.clone(),
        draft_enabled: state.preferences.openrouter.enabled_model_ids.clone(),
        selected: 0,
        search: "vendor/a".to_owned(),
    });
    state.reduce(Action::Intent(Intent::PopupCatalogToggle));
    state.reduce(Action::Intent(Intent::PopupSelect));
    assert!(!state
        .preferences
        .openrouter
        .enabled_model_ids
        .contains("vendor/a"));
    assert!(state
        .preferences
        .openrouter
        .enabled_model_ids
        .contains("stale/model"));
}

#[test]
fn catalog_subset_commit_preserves_unrelated_codex_context() {
    let mut state = thread_ready_state();
    state.context_remaining_percent = Some(71);
    state.openrouter.catalog = vec![openrouter_model("vendor/model")];
    state.popup = Some(PopupState::OpenRouterCatalog {
        models: state.openrouter.catalog.clone(),
        draft_enabled: ["vendor/model".to_owned()].into_iter().collect(),
        selected: 0,
        search: String::new(),
    });

    let effects = state.reduce(Action::Intent(Intent::PopupSelect));
    assert_eq!(state.active_provider, ProviderId::Codex);
    assert_eq!(state.context_remaining_percent, Some(71));
    assert!(matches!(effects.as_slice(), [Effect::Persist(_)]));
}

#[test]
fn refreshed_openrouter_catalog_fallback_clears_context_and_is_persisted() {
    let mut state = AppState {
        active_provider: ProviderId::OpenRouter,
        selected_model: Some(ModelKey::openrouter("vendor/old").unwrap()),
        context_remaining_percent: Some(44),
        ..AppState::default()
    };
    state.openrouter.auth = OpenRouterAuthStatus::Valid;
    state.openrouter.catalog = vec![
        openrouter_model("vendor/old"),
        openrouter_model("vendor/fallback"),
    ];
    state.preferences.openrouter.selected_model_id = Some("vendor/old".to_owned());
    state.preferences.openrouter.enabled_model_ids =
        ["vendor/old".to_owned(), "vendor/fallback".to_owned()]
            .into_iter()
            .collect();

    let effects = state.reduce(Action::Event(DomainEvent::OpenRouterCatalogLoaded(vec![
        openrouter_model("vendor/fallback"),
    ])));
    assert_eq!(
        state.selected_model,
        Some(ModelKey::openrouter("vendor/fallback").unwrap())
    );
    assert_eq!(state.context_remaining_percent, None);
    assert_eq!(
        state.preferences.openrouter.selected_model_id.as_deref(),
        Some("vendor/fallback")
    );
    assert!(matches!(effects.as_slice(), [Effect::Persist(_)]));
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

#[test]
fn unified_resume_switches_codex_to_openrouter_with_destination_model() {
    let mut state = thread_ready_state();
    let conversation_id = OpenRouterConversationId::default();
    state.openrouter.auth = OpenRouterAuthStatus::Valid;
    state.openrouter.catalog = vec![openrouter_model("vendor/model")];
    state
        .preferences
        .openrouter
        .enabled_model_ids
        .insert("vendor/model".to_owned());
    state.preferences.openrouter.selected_model_id = Some("vendor/model".to_owned());

    assert_eq!(
        state.reduce(Action::Intent(Intent::Resume)),
        vec![Effect::ListThreads]
    );
    state.reduce(Action::Event(DomainEvent::ThreadListLoaded(vec![
        ThreadChoice {
            provider: ProviderId::OpenRouter,
            id: conversation_id.as_str().to_owned(),
            title: "OpenRouter saved".to_owned(),
            updated_at: 1,
        },
    ])));
    assert_eq!(
        state.reduce(Action::Intent(Intent::ThreadPickerSelect)),
        vec![Effect::SwitchOpenRouterConversation {
            id: conversation_id.clone(),
            model: ModelKey::openrouter("vendor/model").unwrap(),
        }]
    );

    let history = vec![TranscriptEntry {
        provider: ProviderId::OpenRouter,
        role: TranscriptRole::Assistant,
        text: "restored OpenRouter".to_owned(),
        item_id: None,
        turn_id: None,
    }];
    let effects = state.reduce(Action::Event(DomainEvent::OpenRouterConversationRestored {
        conversation_id: conversation_id.clone(),
        history: history.clone(),
        model: ModelKey::openrouter("vendor/model").unwrap(),
        automatic: false,
    }));
    assert!(matches!(effects.as_slice(), [Effect::Persist(_)]));
    assert_eq!(state.active_provider, ProviderId::OpenRouter);
    assert_eq!(
        state.selected_model,
        Some(ModelKey::openrouter("vendor/model").unwrap())
    );
    assert_eq!(state.selected_reasoning, None);
    assert_eq!(state.transcript, history);
    assert!(matches!(
        state.openrouter.conversation,
        OpenRouterConversationState::Ready { ref id } if id == &conversation_id
    ));

    let late_history = vec![TranscriptEntry {
        provider: ProviderId::OpenRouter,
        role: TranscriptRole::Assistant,
        text: "late duplicate".to_owned(),
        item_id: None,
        turn_id: None,
    }];
    assert!(state
        .reduce(Action::Event(DomainEvent::OpenRouterConversationRestored {
            conversation_id: conversation_id.clone(),
            history: late_history,
            model: ModelKey::openrouter("vendor/model").unwrap(),
            automatic: false,
        }))
        .is_empty());
    assert_eq!(state.transcript, history);
}

#[test]
fn openrouter_startup_preserves_exact_saved_model_for_automatic_resume() {
    let conversation_id = OpenRouterConversationId::default();
    let mut preferences = PreferencesV2 {
        active_provider: ProviderId::OpenRouter,
        ..PreferencesV2::default()
    };
    preferences.openrouter.selected_model_id = Some("vendor/saved".to_owned());
    preferences
        .openrouter
        .enabled_model_ids
        .extend(["vendor/saved".to_owned(), "vendor/fallback".to_owned()]);
    preferences.openrouter.auto_resume_conversation_id = Some(conversation_id.clone());

    let mut state = AppState::default();
    state.reduce(Action::Event(DomainEvent::PreferencesLoaded(preferences)));
    state.reduce(Action::Event(DomainEvent::OpenRouterStartup {
        auth: OpenRouterAuthStatus::Valid,
        catalog: vec![openrouter_model("vendor/fallback")],
    }));

    assert_eq!(
        state.selected_model,
        Some(ModelKey::openrouter("vendor/saved").unwrap())
    );
    assert_eq!(
        state.preferences.openrouter.selected_model_id.as_deref(),
        Some("vendor/saved")
    );
    state.reduce(Action::Event(DomainEvent::OpenRouterResumeFailed {
        conversation_id: conversation_id.clone(),
    }));
    assert!(matches!(
        state.openrouter.conversation,
        OpenRouterConversationState::ResumeFailed { id, .. } if id == conversation_id
    ));
    assert_eq!(
        state.preferences.openrouter.auto_resume_conversation_id,
        Some(conversation_id)
    );
}

#[test]
fn unavailable_openrouter_runtime_rejects_resume_before_dispatch() {
    let mut state = thread_ready_state();
    let conversation_id = OpenRouterConversationId::default();
    state.openrouter.auth = OpenRouterAuthStatus::CredentialUnavailable;
    state.openrouter.catalog = vec![openrouter_model("vendor/model")];
    state
        .preferences
        .openrouter
        .enabled_model_ids
        .insert("vendor/model".to_owned());
    state.preferences.openrouter.selected_model_id = Some("vendor/model".to_owned());

    assert_eq!(
        state.reduce(Action::Intent(Intent::Resume)),
        vec![Effect::ListThreads]
    );
    state.reduce(Action::Event(DomainEvent::ThreadListLoaded(vec![
        ThreadChoice {
            provider: ProviderId::OpenRouter,
            id: conversation_id.as_str().to_owned(),
            title: "Unavailable OpenRouter".to_owned(),
            updated_at: 1,
        },
    ])));

    assert!(state
        .reduce(Action::Intent(Intent::ThreadPickerSelect))
        .is_empty());
    let picker = state.conversation_popup().expect("picker remains open");
    assert!(matches!(picker.phase, ThreadPickerPhase::Ready));
    assert!(picker
        .message
        .as_deref()
        .is_some_and(|message| message.contains("unavailable")));
}

#[test]
fn unified_resume_switches_openrouter_to_codex_with_saved_model_and_reasoning() {
    let mut state = thread_ready_state();
    let conversation_id = OpenRouterConversationId::default();
    state.active_provider = ProviderId::OpenRouter;
    state.preferences.active_provider = ProviderId::OpenRouter;
    state.openrouter.auth = OpenRouterAuthStatus::Valid;
    state.openrouter.catalog = vec![openrouter_model("vendor/model")];
    state
        .preferences
        .openrouter
        .enabled_model_ids
        .insert("vendor/model".to_owned());
    state.selected_model = Some(ModelKey::openrouter("vendor/model").unwrap());
    state.selected_reasoning = None;
    state.openrouter.conversation = OpenRouterConversationState::Ready {
        id: conversation_id,
    };
    state.transcript[0].provider = ProviderId::OpenRouter;

    assert_eq!(
        state.reduce(Action::Intent(Intent::Resume)),
        vec![Effect::ListThreads]
    );
    state.reduce(Action::Event(DomainEvent::ThreadListLoaded(vec![thread(
        "thr-old",
        "Codex saved",
        1,
    )])));
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
        role: TranscriptRole::Assistant,
        text: "restored Codex".to_owned(),
        item_id: None,
        turn_id: None,
    }];
    let effects = state.reduce(Action::Event(DomainEvent::ThreadSwitchSucceeded {
        id: "thr-old".to_owned(),
        history: history.clone(),
        model: ModelKey::codex("m1").unwrap(),
        reasoning: "high".to_owned(),
    }));
    assert!(matches!(effects.as_slice(), [Effect::Persist(_)]));
    assert_eq!(state.active_provider, ProviderId::Codex);
    assert_eq!(state.selected_model, Some(ModelKey::codex("m1").unwrap()));
    assert_eq!(state.selected_reasoning.as_deref(), Some("high"));
    assert_eq!(state.transcript, history);
}
