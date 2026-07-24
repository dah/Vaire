use super::*;

#[tokio::test]
async fn resolved_kimi_completion_persists_through_service_and_store_reopen() {
    const MODEL: &str = "moonshotai/kimi-k3";

    let root = tempdir().unwrap();
    let store_root = root.path().join("openrouter");
    let mut responses = catalog_responses(MODEL);
    responses.push((
        "text/event-stream",
        concat!(
            "data: {\"id\":\"chat-kimi\",\"model\":\"moonshotai/kimi-k3\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hello\"},\"finish_reason\":null}]}\n\n",
            "data: {\"id\":\"chat-kimi\",\"model\":\"moonshotai/kimi-k3\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
            "data: {\"id\":\"chat-kimi\",\"model\":\"moonshotai/kimi-k3-20260715\",\"choices\":[],\"usage\":{\"prompt_tokens\":2,\"completion_tokens\":1,\"total_tokens\":3}}\n\n",
            "data: [DONE]\n\n"
        )
        .to_owned(),
    ));
    let (base, _requests, server) = scripted_openrouter(responses).await;
    let credentials = Arc::new(FakeCredentialStore::with_openrouter_key(
        SecretValue::from_input(TEST_KEY).unwrap(),
    ));
    let (openrouter, store) = openrouter_service(base, credentials, &store_root);
    let preferences = MemoryPreferences(Arc::new(Mutex::new(openrouter_preferences(MODEL))));
    let mut backend = BackendCoordinator::without_codex(
        preferences,
        NoopBrowser,
        "offline Codex unavailable".to_owned(),
    )
    .with_openrouter(openrouter);

    backend.startup().await.unwrap();
    for _ in 0..2 {
        backend.pump_event().await.unwrap();
    }
    backend
        .handle_intent(Intent::SendMessage("resolved model question".to_owned()))
        .await
        .unwrap();
    pump_until_turn_settles(&mut backend).await;
    assert!(matches!(backend.state().turn, TurnState::Completed { .. }));

    let preferences = backend.state().preferences.clone();
    let conversation_id = preferences
        .openrouter
        .auto_resume_conversation_id
        .clone()
        .unwrap();
    let stored = store.load_conversation(&conversation_id).unwrap();
    assert_eq!(stored.turns.len(), 1);
    assert_eq!(stored.turns[0].outcome, OpenRouterTurnOutcome::Completed);
    assert_eq!(stored.turns[0].model_id, MODEL);
    assert_eq!(stored.turns[0].assistant_text.as_deref(), Some("hello"));
    assert_eq!(stored.turns[0].incomplete_assistant_text, None);

    backend.shutdown().await.unwrap();
    server.await.unwrap();
    drop(store);

    let (base, _requests, server) = scripted_openrouter(catalog_responses(MODEL)).await;
    let credentials = Arc::new(FakeCredentialStore::with_openrouter_key(
        SecretValue::from_input(TEST_KEY).unwrap(),
    ));
    let (openrouter, reopened_store) = openrouter_service(base, credentials, &store_root);
    let mut reopened_backend = BackendCoordinator::without_codex(
        MemoryPreferences(Arc::new(Mutex::new(preferences))),
        NoopBrowser,
        "offline Codex unavailable".to_owned(),
    )
    .with_openrouter(openrouter);

    reopened_backend.startup().await.unwrap();
    for _ in 0..2 {
        reopened_backend.pump_event().await.unwrap();
    }
    let restored = reopened_backend
        .state()
        .transcript
        .iter()
        .filter(|entry| entry.role == TranscriptRole::Assistant)
        .collect::<Vec<_>>();
    assert_eq!(restored.len(), 1);
    assert_eq!(restored[0].text, "hello");
    assert_eq!(restored[0].status, TranscriptEntryStatus::Normal);
    let reopened = reopened_store.load_conversation(&conversation_id).unwrap();
    assert_eq!(reopened.turns[0].outcome, OpenRouterTurnOutcome::Completed);
    assert_eq!(reopened.turns[0].model_id, MODEL);
    assert_eq!(reopened.turns[0].assistant_text.as_deref(), Some("hello"));
    assert_eq!(reopened.turns[0].incomplete_assistant_text, None);
    let canonical = reopened.canonical_messages();
    assert_eq!(canonical.len(), 2);
    assert_eq!(canonical[0].role, ChatRole::User);
    assert_eq!(canonical[0].content, "resolved model question");
    assert_eq!(canonical[1].role, ChatRole::Assistant);
    assert_eq!(canonical[1].content, "hello");

    reopened_backend.shutdown().await.unwrap();
    server.await.unwrap();
}

#[tokio::test]
async fn offline_backend_runs_openrouter_startup_catalog_chat_persistence_and_shutdown() {
    let (base, requests, server) = fake_openrouter().await;
    let root = tempdir().unwrap();
    let credentials = Arc::new(FakeCredentialStore::with_openrouter_key(
        SecretValue::from_input(TEST_KEY).unwrap(),
    ));
    let store = Arc::new(FileOpenRouterStore::new(root.path().join("openrouter")).unwrap());
    let client = OpenRouterClient::with_loopback_base_url(
        base,
        credentials.clone(),
        OpenRouterTimeouts {
            connect: Duration::from_secs(1),
            get_attempt: Duration::from_secs(1),
            chat_headers: Duration::from_secs(1),
            sse_idle: Duration::from_secs(1),
            chat_total: Duration::from_secs(2),
            retry_delay: Duration::ZERO,
        },
    )
    .unwrap();
    let openrouter = OpenRouterService::new(client, credentials, store.clone());
    let mut preferences = PreferencesV2 {
        active_provider: ProviderId::OpenRouter,
        ..PreferencesV2::default()
    };
    preferences.openrouter.selected_model_id = Some("vendor/model".to_owned());
    preferences.openrouter.enabled_model_ids = BTreeSet::from(["vendor/model".to_owned()]);
    let preferences = MemoryPreferences(Arc::new(Mutex::new(preferences)));
    let mut backend = BackendCoordinator::without_codex(
        preferences,
        NoopBrowser,
        "offline Codex unavailable".to_owned(),
    )
    .with_openrouter(openrouter);

    backend.startup().await.unwrap();
    for _ in 0..2 {
        tokio::time::timeout(Duration::from_secs(2), backend.pump_event())
            .await
            .unwrap()
            .unwrap();
    }
    assert_eq!(backend.state().openrouter.auth, OpenRouterAuthStatus::Valid);
    assert_eq!(backend.state().active_provider, ProviderId::OpenRouter);

    backend
        .handle_intent(Intent::SendMessage("offline hello".to_owned()))
        .await
        .unwrap();
    for _ in 0..8 {
        if !backend.state().turn.is_active() {
            break;
        }
        tokio::time::timeout(Duration::from_secs(2), backend.pump_event())
            .await
            .unwrap()
            .unwrap();
    }
    assert!(matches!(backend.state().turn, TurnState::Completed { .. }));
    assert!(backend.state().transcript.iter().any(|entry| {
        entry.role == TranscriptRole::Assistant
            && entry.provider == ProviderId::OpenRouter
            && entry.text == "hello"
    }));
    let conversation_id = backend
        .state()
        .preferences
        .openrouter
        .auto_resume_conversation_id
        .clone()
        .unwrap();
    let stored = store.load_conversation(&conversation_id).unwrap();
    assert_eq!(stored.turns[0].outcome, OpenRouterTurnOutcome::Completed);
    assert_eq!(stored.turns[0].assistant_text.as_deref(), Some("hello"));

    tokio::time::timeout(Duration::from_secs(2), backend.shutdown())
        .await
        .expect("dual-provider coordinator shutdown must remain bounded")
        .unwrap();
    server.await.unwrap();
    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 3);
    let text = requests
        .iter()
        .map(|request| String::from_utf8_lossy(request).into_owned())
        .collect::<Vec<_>>();
    assert!(text[0].starts_with("GET /api/v1/key HTTP/1.1"));
    assert!(text[1].starts_with("GET /api/v1/models/user HTTP/1.1"));
    assert!(text[2].starts_with("POST /api/v1/chat/completions HTTP/1.1"));
    assert!(text.iter().all(|request| request
        .to_ascii_lowercase()
        .contains(&format!("authorization: bearer {TEST_KEY}").to_ascii_lowercase())));
    let body = text[2].split_once("\r\n\r\n").unwrap().1;
    assert!(!body.contains("Codex"));
    let body: serde_json::Value = serde_json::from_str(body).unwrap();
    let messages = body["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0]["role"], "user");
    assert_eq!(messages[0]["content"], "offline hello");
}
