use super::*;

#[tokio::test]
async fn saturated_control_queue_cannot_deadlock_logout_or_recreate_credential() {
    let directory = tempdir().unwrap();
    let store = Arc::new(FileOpenRouterStore::new(directory.path().join("openrouter")).unwrap());
    let credentials = Arc::new(FakeCredentialStore::with_openrouter_key(
        SecretValue::from_input("old-offline-test-key").unwrap(),
    ));
    let mut service = service(credentials.clone(), store);
    for _ in 0..EVENT_QUEUE {
        service
            .control_events_tx
            .try_send(OpenRouterServiceEvent::AuthValidated { operation_id: 0 })
            .unwrap();
    }
    service
        .replace_candidate(SecretValue::from_input("candidate-offline-test-key").unwrap())
        .unwrap();

    tokio::time::timeout(Duration::from_secs(1), service.logout())
        .await
        .expect("logout must drain a saturated control queue")
        .1
        .unwrap();
    assert!(!credentials.is_configured(CredentialAccount::OpenRouterApiKey));
}

#[tokio::test]
async fn saturated_chat_queue_cannot_deadlock_interrupting_shutdown() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = Url::parse(&format!("http://{}", listener.local_addr().unwrap())).unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = vec![0_u8; 16 * 1024];
        let _ = stream.read(&mut request).await.unwrap();
        let body = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"hello\"}}]}\n\n",
            "data: [DONE]\n\n"
        );
        let response = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        stream.write_all(response.as_bytes()).await.unwrap();
    });
    let directory = tempdir().unwrap();
    let store = Arc::new(FileOpenRouterStore::new(directory.path().join("openrouter")).unwrap());
    let credentials = Arc::new(FakeCredentialStore::with_openrouter_key(
        SecretValue::from_input("offline-test-key").unwrap(),
    ));
    let client = OpenRouterClient::with_loopback_base_url(
        base,
        credentials,
        OpenRouterTimeouts {
            connect: Duration::from_secs(1),
            get_attempt: Duration::from_secs(1),
            chat_headers: Duration::from_secs(1),
            sse_idle: Duration::from_secs(1),
            chat_total: Duration::from_secs(1),
            retry_delay: Duration::ZERO,
        },
    )
    .unwrap();
    let mut service = OpenRouterService::new(
        client,
        Arc::new(FakeCredentialStore::default()),
        store.clone(),
    );
    for _ in 0..EVENT_QUEUE {
        service
            .chat_events_tx
            .try_send(OpenRouterServiceEvent::TextDelta {
                conversation_id: OpenRouterConversationId::default(),
                turn_id: OpenRouterTurnId::new(),
                delta: "queued".to_owned(),
            })
            .unwrap();
    }
    let prepared = service
        .prepare_turn(None, "vendor/model".to_owned(), "hello".to_owned())
        .await
        .unwrap();
    let conversation_id = prepared.conversation_id().clone();
    service.launch_prepared_turn(prepared);
    tokio::time::sleep(Duration::from_millis(30)).await;

    tokio::time::timeout(Duration::from_secs(1), service.shutdown())
        .await
        .expect("shutdown must drain a saturated chat queue");
    let record = &store.load_conversation(&conversation_id).unwrap().turns[0];
    assert_eq!(record.outcome, OpenRouterTurnOutcome::Interrupted);
    assert_eq!(record.assistant_text, None);
    assert_eq!(record.incomplete_assistant_text, None);
    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn logout_preserves_a_real_completed_terminal_event_and_final_text() {
    let directory = tempdir().unwrap();
    let store = Arc::new(FileOpenRouterStore::new(directory.path().join("openrouter")).unwrap());
    let credentials = Arc::new(FakeCredentialStore::with_openrouter_key(
        SecretValue::from_input("offline-test-key").unwrap(),
    ));
    let mut service = service(credentials, store);
    let conversation_id = OpenRouterConversationId::default();
    let turn_id = OpenRouterTurnId::new();
    let events = service.chat_events_tx.clone();
    let expected_conversation = conversation_id.clone();
    let expected_turn = turn_id.clone();
    service.chat_task = Some(tokio::spawn(async move {
        events
            .send(OpenRouterServiceEvent::TurnFinished {
                conversation_id: expected_conversation,
                turn_id: expected_turn,
                outcome: OpenRouterTurnOutcome::Completed,
                assistant_text: Some("final text".to_owned()),
                incomplete_assistant_text: None,
                usage: None,
                failure: None,
                failure_stage: None,
            })
            .await
            .unwrap();
    }));

    let (drained, result) = service.logout().await;
    result.unwrap();
    assert!(drained.iter().any(|event| matches!(
        event,
        OpenRouterServiceEvent::TurnFinished {
            conversation_id: id,
            turn_id: turn,
            outcome: OpenRouterTurnOutcome::Completed,
            assistant_text: Some(text),
            ..
        } if id == &conversation_id && turn == &turn_id && text == "final text"
    )));
}

#[tokio::test]
async fn logout_interrupts_a_hanging_chat_and_preserves_its_terminal_event() {
    let directory = tempdir().unwrap();
    let store = Arc::new(FileOpenRouterStore::new(directory.path().join("openrouter")).unwrap());
    let credentials = Arc::new(FakeCredentialStore::with_openrouter_key(
        SecretValue::from_input("offline-test-key").unwrap(),
    ));
    let mut service = service(credentials, store);
    let conversation_id = OpenRouterConversationId::default();
    let turn_id = OpenRouterTurnId::new();
    let cancel = CancellationToken::new();
    let task_cancel = cancel.clone();
    let events = service.chat_events_tx.clone();
    let expected_conversation = conversation_id.clone();
    let expected_turn = turn_id.clone();
    service.chat_cancel = Some(cancel);
    service.chat_task = Some(tokio::spawn(async move {
        task_cancel.cancelled().await;
        events
            .send(OpenRouterServiceEvent::TurnFinished {
                conversation_id: expected_conversation,
                turn_id: expected_turn,
                outcome: OpenRouterTurnOutcome::Interrupted,
                assistant_text: None,
                incomplete_assistant_text: None,
                usage: None,
                failure: None,
                failure_stage: None,
            })
            .await
            .unwrap();
    }));

    let (drained, result) = service.logout().await;
    result.unwrap();
    assert!(drained.iter().any(|event| matches!(
        event,
        OpenRouterServiceEvent::TurnFinished {
            conversation_id: id,
            turn_id: turn,
            outcome: OpenRouterTurnOutcome::Interrupted,
            ..
        } if id == &conversation_id && turn == &turn_id
    )));
}
