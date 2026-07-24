use super::*;

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
        status: TranscriptEntryStatus::FailedIncomplete,
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
        status: TranscriptEntryStatus::Normal,
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
    let mut preferences = PreferencesV3 {
        active_provider: ProviderId::OpenRouter,
        ..PreferencesV3::default()
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
        status: TranscriptEntryStatus::Normal,
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
