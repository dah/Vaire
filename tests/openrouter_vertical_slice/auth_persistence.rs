use super::*;

#[tokio::test]
async fn stored_candidate_rejected_by_catalog_401_or_403_becomes_invalid_but_is_retained() {
    const CANDIDATE: &str = "candidate-offline-key";
    for status in [401, 403] {
        let (base, requests, server) = fake_candidate_catalog_failure(status).await;
        let root = tempdir().unwrap();
        let credentials = Arc::new(FakeCredentialStore::with_openrouter_key(
            SecretValue::from_input("old-offline-key").unwrap(),
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
        let openrouter = OpenRouterService::new(client, credentials.clone(), store);
        let mut backend = BackendCoordinator::without_codex(
            MemoryPreferences(Arc::new(Mutex::new(PreferencesV4::default()))),
            NoopBrowser,
            "offline Codex unavailable".to_owned(),
        )
        .with_openrouter(openrouter);

        backend
            .accept_openrouter_credential(SecretValue::from_input(CANDIDATE).unwrap())
            .unwrap();
        for _ in 0..2 {
            backend.pump_event().await.unwrap();
        }

        assert_eq!(
            backend.state().openrouter.auth,
            OpenRouterAuthStatus::Invalid
        );
        assert_eq!(
            credentials
                .load(CredentialAccount::OpenRouterApiKey)
                .unwrap()
                .unwrap()
                .expose_bytes(),
            CANDIDATE.as_bytes()
        );
        assert!(!credentials.operations().iter().any(|operation| matches!(
            operation,
            FakeCredentialOperation::Delete(CredentialAccount::OpenRouterApiKey)
        )));
        let notice = backend.state().notice.as_deref().unwrap();
        assert!(notice.contains("provider rejected it"));
        assert!(!notice.contains(CANDIDATE));
        backend.shutdown().await.unwrap();
        server.await.unwrap();
        assert_eq!(requests.lock().unwrap().len(), 2);
    }
}

#[tokio::test]
async fn first_openrouter_send_never_bypasses_a_read_only_preferences_gate() {
    let (base, server) = fake_openrouter_catalog_only("vendor/model").await;
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
    let mut value = PreferencesV4 {
        active_provider: ProviderId::OpenRouter,
        ..PreferencesV4::default()
    };
    value.openrouter.selected_model_id = Some("vendor/model".to_owned());
    value.openrouter.enabled_model_ids = BTreeSet::from(["vendor/model".to_owned()]);
    let saves = Arc::new(Mutex::new(0));
    let preferences = ReadOnlyPreferences {
        value,
        saves: saves.clone(),
    };
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
        .handle_intent(Intent::SendMessage("must not post".to_owned()))
        .await
        .unwrap();

    assert!(!backend.state().turn.is_active());
    assert_eq!(*saves.lock().unwrap(), 0);
    let summaries = store.list_conversations().unwrap();
    assert_eq!(summaries.len(), 1);
    let stored = store.load_conversation(&summaries[0].id).unwrap();
    assert_eq!(stored.turns[0].outcome, OpenRouterTurnOutcome::Failed);
    backend.shutdown().await.unwrap();
    assert_eq!(*saves.lock().unwrap(), 0);
    server.await.unwrap();
}
