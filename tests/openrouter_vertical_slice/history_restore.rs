use super::*;

#[tokio::test]
async fn failed_partial_survives_reopen_auto_resume_and_is_excluded_from_later_post() {
    const MODEL: &str = "moonshotai/kimi-k3";
    const FIRST_USER: &str = "first question";
    const FAILED_PARTIAL: &str = "DISPLAY-ONLY-FAILED-PARTIAL";
    const LATER_USER: &str = "later question";

    let root = tempdir().unwrap();
    let store_root = root.path().join("openrouter");
    let preferences = persist_failed_partial(&store_root, MODEL, FIRST_USER, FAILED_PARTIAL).await;
    let conversation_id = preferences
        .openrouter
        .auto_resume_conversation_id
        .clone()
        .unwrap();

    let mut responses = catalog_responses(MODEL);
    responses.push((
        "text/event-stream",
        concat!(
            "data: {\"id\":\"chat-later\",\"model\":\"moonshotai/kimi-k3\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"later answer\"},\"finish_reason\":null}]}\n\n",
            "data: {\"id\":\"chat-later\",\"model\":\"moonshotai/kimi-k3\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
            "data: {\"id\":\"chat-later\",\"model\":\"moonshotai/kimi-k3\",\"choices\":[],\"usage\":{\"prompt_tokens\":4,\"completion_tokens\":2,\"total_tokens\":6}}\n\n",
            "data: [DONE]\n\n"
        )
        .to_owned(),
    ));
    let (base, requests, server) = scripted_openrouter(responses).await;
    let credentials = Arc::new(FakeCredentialStore::with_openrouter_key(
        SecretValue::from_input(TEST_KEY).unwrap(),
    ));
    let (openrouter, reopened_store) = openrouter_service(base, credentials, &store_root);
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

    assert_eq!(
        backend.state().openrouter.conversation,
        OpenRouterConversationState::Ready {
            id: conversation_id.clone()
        }
    );
    let incomplete = backend
        .state()
        .transcript
        .iter()
        .filter(|entry| {
            entry.role == TranscriptRole::Assistant
                && entry.status == TranscriptEntryStatus::FailedIncomplete
        })
        .collect::<Vec<_>>();
    assert_eq!(incomplete.len(), 1);
    assert_eq!(incomplete[0].text, FAILED_PARTIAL);
    assert_eq!(
        backend
            .state()
            .transcript
            .iter()
            .filter(|entry| entry.role == TranscriptRole::Assistant)
            .count(),
        1
    );

    backend
        .handle_intent(Intent::SendMessage(LATER_USER.to_owned()))
        .await
        .unwrap();
    pump_until_turn_settles(&mut backend).await;
    assert!(matches!(backend.state().turn, TurnState::Completed { .. }));

    let reopened = reopened_store.load_conversation(&conversation_id).unwrap();
    assert_eq!(reopened.turns.len(), 2);
    assert_eq!(reopened.turns[0].outcome, OpenRouterTurnOutcome::Failed);
    assert_eq!(
        reopened.turns[0].incomplete_assistant_text.as_deref(),
        Some(FAILED_PARTIAL)
    );
    assert_eq!(reopened.turns[1].outcome, OpenRouterTurnOutcome::Completed);
    assert_eq!(
        reopened.turns[1].assistant_text.as_deref(),
        Some("later answer")
    );

    backend.shutdown().await.unwrap();
    server.await.unwrap();
    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 3);
    let post = String::from_utf8_lossy(&requests[2]);
    assert!(post.starts_with("POST /api/v1/chat/completions HTTP/1.1"));
    let body = post.split_once("\r\n\r\n").unwrap().1;
    assert!(!body.contains(FAILED_PARTIAL));
    let body: serde_json::Value = serde_json::from_str(body).unwrap();
    let messages = body["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0]["role"], "user");
    assert_eq!(messages[0]["content"], FIRST_USER);
    assert_eq!(messages[1]["role"], "user");
    assert_eq!(messages[1]["content"], LATER_USER);
}

#[tokio::test]
async fn unified_picker_effect_restores_failed_partial_from_reopened_real_store() {
    const MODEL: &str = "moonshotai/kimi-k3";
    const FAILED_PARTIAL: &str = "PICKER-RESTORED-FAILED-PARTIAL";

    let root = tempdir().unwrap();
    let store_root = root.path().join("openrouter");
    let mut preferences =
        persist_failed_partial(&store_root, MODEL, "picker question", FAILED_PARTIAL).await;
    let conversation_id = preferences
        .openrouter
        .auto_resume_conversation_id
        .clone()
        .unwrap();
    preferences.active_provider = ProviderId::Codex;

    let (base, _requests, server) = scripted_openrouter(catalog_responses(MODEL)).await;
    let credentials = Arc::new(FakeCredentialStore::with_openrouter_key(
        SecretValue::from_input(TEST_KEY).unwrap(),
    ));
    let (openrouter, _reopened_store) = openrouter_service(base, credentials, &store_root);
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
    assert_eq!(backend.state().active_provider, ProviderId::Codex);

    backend.handle_intent(Intent::Resume).await.unwrap();
    backend
        .handle_intent(Intent::ThreadPickerSelect)
        .await
        .unwrap();

    assert_eq!(backend.state().active_provider, ProviderId::OpenRouter);
    assert_eq!(
        backend.state().openrouter.conversation,
        OpenRouterConversationState::Ready {
            id: conversation_id
        }
    );
    let incomplete = backend
        .state()
        .transcript
        .iter()
        .filter(|entry| {
            entry.role == TranscriptRole::Assistant
                && entry.status == TranscriptEntryStatus::FailedIncomplete
        })
        .collect::<Vec<_>>();
    assert_eq!(incomplete.len(), 1);
    assert_eq!(incomplete[0].text, FAILED_PARTIAL);
    assert_eq!(
        backend
            .state()
            .transcript
            .iter()
            .filter(|entry| entry.role == TranscriptRole::Assistant)
            .count(),
        1
    );

    backend.shutdown().await.unwrap();
    server.await.unwrap();
}
