use super::*;

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
        status: TranscriptEntryStatus::Normal,
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
            status: TranscriptEntryStatus::Normal,
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
        status: TranscriptEntryStatus::Normal,
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
