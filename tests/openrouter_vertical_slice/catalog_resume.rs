use super::*;

#[tokio::test]
async fn missing_or_corrupt_cached_catalog_waits_for_exact_live_model_before_auto_resume() {
    for corrupt_catalog in [false, true] {
        let (base, server) = fake_openrouter_catalog_only("vendor/model").await;
        let root = tempdir().unwrap();
        let credentials = Arc::new(FakeCredentialStore::with_openrouter_key(
            SecretValue::from_input(TEST_KEY).unwrap(),
        ));
        let store_root = root.path().join("openrouter");
        let store = Arc::new(FileOpenRouterStore::new(&store_root).unwrap());
        let conversation =
            OpenRouterConversationV2::new(Default::default(), 1, "Saved conversation");
        let conversation_id = conversation.id.clone();
        store.save_conversation(&conversation).unwrap();
        if corrupt_catalog {
            fs::write(store_root.join("catalog.json"), b"not-json").unwrap();
            fs::set_permissions(
                store_root.join("catalog.json"),
                fs::Permissions::from_mode(0o600),
            )
            .unwrap();
        }
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
        let openrouter = OpenRouterService::new(client, credentials, store);
        let mut preferences = PreferencesV4 {
            active_provider: ProviderId::OpenRouter,
            ..PreferencesV4::default()
        };
        preferences.openrouter.selected_model_id = Some("vendor/model".to_owned());
        preferences.openrouter.enabled_model_ids = BTreeSet::from(["vendor/model".to_owned()]);
        preferences.set_auto_resume_conversation(Some(conversation_id.clone()));
        let mut backend = BackendCoordinator::without_codex(
            MemoryPreferences(Arc::new(Mutex::new(preferences))),
            NoopBrowser,
            "offline Codex unavailable".to_owned(),
        )
        .with_openrouter(openrouter);

        backend.startup().await.unwrap();
        assert_eq!(
            backend.state().openrouter.conversation,
            OpenRouterConversationState::None
        );
        for _ in 0..2 {
            tokio::time::timeout(Duration::from_secs(2), backend.pump_event())
                .await
                .unwrap()
                .unwrap();
        }
        assert_eq!(
            backend.state().openrouter.conversation,
            OpenRouterConversationState::Ready {
                id: conversation_id
            }
        );
        assert_eq!(
            backend.state().selected_model.as_ref().unwrap().id,
            "vendor/model"
        );
        backend.shutdown().await.unwrap();
        server.await.unwrap();
    }
}

#[tokio::test]
async fn refreshed_catalog_without_exact_saved_model_blocks_auto_resume_without_fallback() {
    let (base, server) = fake_openrouter_catalog_only("vendor/other").await;
    let root = tempdir().unwrap();
    let credentials = Arc::new(FakeCredentialStore::with_openrouter_key(
        SecretValue::from_input(TEST_KEY).unwrap(),
    ));
    let store = Arc::new(FileOpenRouterStore::new(root.path().join("openrouter")).unwrap());
    let conversation = OpenRouterConversationV2::new(Default::default(), 1, "Saved conversation");
    let conversation_id = conversation.id.clone();
    store.save_conversation(&conversation).unwrap();
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
    let openrouter = OpenRouterService::new(client, credentials, store);
    let mut preferences = PreferencesV4 {
        active_provider: ProviderId::OpenRouter,
        ..PreferencesV4::default()
    };
    preferences.openrouter.selected_model_id = Some("vendor/model".to_owned());
    preferences.openrouter.enabled_model_ids =
        BTreeSet::from(["vendor/model".to_owned(), "vendor/other".to_owned()]);
    preferences.set_auto_resume_conversation(Some(conversation_id.clone()));
    let mut backend = BackendCoordinator::without_codex(
        MemoryPreferences(Arc::new(Mutex::new(preferences))),
        NoopBrowser,
        "offline Codex unavailable".to_owned(),
    )
    .with_openrouter(openrouter);

    backend.startup().await.unwrap();
    for _ in 0..2 {
        backend.pump_event().await.unwrap();
    }

    assert!(matches!(
        &backend.state().openrouter.conversation,
        OpenRouterConversationState::ResumeFailed { id, .. } if id == &conversation_id
    ));
    assert_eq!(
        backend
            .state()
            .preferences
            .openrouter
            .auto_resume_conversation_id
            .as_ref(),
        Some(&conversation_id)
    );
    assert_eq!(
        backend
            .state()
            .preferences
            .openrouter
            .selected_model_id
            .as_deref(),
        Some("vendor/model")
    );
    assert_eq!(
        backend.state().selected_model.as_ref().unwrap().id,
        "vendor/model"
    );
    backend.shutdown().await.unwrap();
    server.await.unwrap();
}
