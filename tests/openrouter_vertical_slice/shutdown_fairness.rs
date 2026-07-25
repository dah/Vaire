use super::*;

async fn flooding_codex_session(root: &std::path::Path) -> SessionService {
    let executable = root.join("fake-codex-flood");
    let script = r#"#!/bin/sh
set -eu
IFS= read -r initialize
printf '%s\n' '{"id":1,"result":{"codexHome":"/private/tmp/codex","platformFamily":"unix","platformOs":"macos","userAgent":"fake/0.144.6"}}'
IFS= read -r initialized
IFS= read -r account
printf '%s\n' '{"id":2,"result":{"account":null,"requiresOpenaiAuth":true}}'
IFS= read -r models
printf '%s\n' '{"id":3,"result":{"data":[{"id":"codex-model","displayName":"Codex","isDefault":true,"defaultReasoningEffort":"high","supportedReasoningEfforts":[{"reasoningEffort":"high","description":"deep"}],"hidden":false}],"nextCursor":null}}'
i=0
while [ "$i" -lt 48 ]; do
  printf '%s\n' '{"method":"future/noisyNotification","params":{"ignored":true}}'
  i=$((i + 1))
done
IFS= read -r hold
"#;
    fs::write(&executable, script).unwrap();
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
    let isolation = IsolationPaths::prepare(root.join("runtime")).unwrap();
    let transport = AppServerTransport::spawn(ProcessSpec {
        executable,
        args: Vec::new(),
        cwd: root.to_owned(),
        env: Vec::new(),
    })
    .await
    .unwrap();
    SessionService::new(transport, isolation, FullAccessPolicy)
}

#[tokio::test]
async fn completion_queued_before_logout_remains_completed_with_final_text() {
    let (base, _requests, server) = fake_openrouter().await;
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
    let openrouter = OpenRouterService::new(client, credentials, store);
    let mut preferences = PreferencesV4 {
        active_provider: ProviderId::OpenRouter,
        ..PreferencesV4::default()
    };
    preferences.openrouter.selected_model_id = Some("vendor/model".to_owned());
    preferences.openrouter.enabled_model_ids = BTreeSet::from(["vendor/model".to_owned()]);
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
    backend
        .handle_intent(Intent::SendMessage("finish before logout".to_owned()))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    backend
        .handle_intent(Intent::LogoutOpenRouter)
        .await
        .unwrap();

    assert!(matches!(backend.state().turn, TurnState::Completed { .. }));
    assert!(backend.state().transcript.iter().any(|entry| {
        entry.role == TranscriptRole::Assistant
            && entry.provider == ProviderId::OpenRouter
            && entry.text == "hello"
    }));
    assert_eq!(
        backend.state().openrouter.auth,
        OpenRouterAuthStatus::Missing
    );
    backend.shutdown().await.unwrap();
    server.await.unwrap();
}

#[tokio::test]
async fn codex_event_flood_does_not_starve_openrouter_or_deadlock_dual_provider_shutdown() {
    let (base, _requests, server) = fake_openrouter().await;
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
    let openrouter = OpenRouterService::new(client, credentials, store);
    let mut preferences = PreferencesV4 {
        active_provider: ProviderId::OpenRouter,
        ..PreferencesV4::default()
    };
    preferences.openrouter.selected_model_id = Some("vendor/model".to_owned());
    preferences.openrouter.enabled_model_ids = BTreeSet::from(["vendor/model".to_owned()]);
    let preferences = MemoryPreferences(Arc::new(Mutex::new(preferences)));
    let session = flooding_codex_session(root.path()).await;
    let mut backend =
        BackendCoordinator::new(session, preferences, NoopBrowser).with_openrouter(openrouter);

    backend.startup().await.unwrap();
    for _ in 0..140 {
        if backend.state().openrouter.auth == OpenRouterAuthStatus::Valid
            && !backend.state().openrouter.catalog.is_empty()
        {
            break;
        }
        tokio::time::timeout(Duration::from_secs(2), backend.pump_event())
            .await
            .unwrap()
            .unwrap();
    }
    assert_eq!(backend.state().openrouter.auth, OpenRouterAuthStatus::Valid);
    assert!(!backend.state().openrouter.catalog.is_empty());
    backend
        .handle_intent(Intent::SendMessage("survives flood".to_owned()))
        .await
        .unwrap();
    for _ in 0..140 {
        if !backend.state().turn.is_active() {
            break;
        }
        tokio::time::timeout(Duration::from_secs(2), backend.pump_event())
            .await
            .unwrap()
            .unwrap();
    }
    assert!(
        matches!(backend.state().turn, TurnState::Completed { .. }),
        "turn after dual-provider flood: {:?}; notice: {:?}",
        backend.state().turn,
        backend.state().notice
    );
    tokio::time::timeout(Duration::from_secs(2), backend.shutdown())
        .await
        .expect("both provider tasks and the Codex child must settle")
        .unwrap();
    server.await.unwrap();
}
