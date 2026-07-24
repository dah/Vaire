use super::*;

#[tokio::test]
async fn failed_stream_persists_and_reopens_only_the_nonempty_partial() {
    let body = concat!(
        "data: {\"id\":\"chat-1\",\"model\":\"vendor/model\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"partial\"},\"finish_reason\":null}]}\n\n",
        "data: {\"error\":{\"code\":429,\"message\":\"private detail\",\"metadata\":{\"error_type\":\"rate_limit_exceeded\"}},\"choices\":null,\"usage\":{\"total_tokens\":\"bad\"}}\n\n",
    );
    let (mut service, store, server, directory) = sse_service(body, false).await;
    let (conversation_id, _) = service
        .start_turn(None, "vendor/model".to_owned(), "hello".to_owned())
        .await
        .unwrap();
    let mut saw_partial = false;
    loop {
        let event = tokio::time::timeout(Duration::from_secs(1), service.next_event())
            .await
            .unwrap()
            .unwrap();
        match event {
            OpenRouterServiceEvent::TextDelta { delta, .. } => {
                saw_partial |= delta == "partial";
            }
            OpenRouterServiceEvent::TurnFinished {
                outcome,
                assistant_text,
                incomplete_assistant_text,
                failure,
                failure_stage,
                ..
            } => {
                assert_eq!(outcome, OpenRouterTurnOutcome::Failed);
                assert_eq!(assistant_text, None);
                assert_eq!(incomplete_assistant_text.as_deref(), Some("partial"));
                assert_eq!(failure, Some(OpenRouterFailureCategory::RateLimited));
                assert_eq!(failure_stage, None);
                break;
            }
            _ => {}
        }
    }
    assert!(saw_partial);
    server.await.unwrap();

    let record = &store.load_conversation(&conversation_id).unwrap().turns[0];
    assert_eq!(record.outcome, OpenRouterTurnOutcome::Failed);
    assert_eq!(record.assistant_text, None);
    assert_eq!(record.incomplete_assistant_text.as_deref(), Some("partial"));
    drop(service);
    drop(store);

    let reopened = FileOpenRouterStore::new(directory.path().join("openrouter")).unwrap();
    let conversation = reopened.load_conversation(&conversation_id).unwrap();
    assert_eq!(
        conversation.turns[0].incomplete_assistant_text.as_deref(),
        Some("partial")
    );
    assert!(conversation
        .canonical_messages()
        .iter()
        .all(|message| message.content != "partial"));
}

#[tokio::test]
async fn staged_parser_failure_reaches_service_terminal_but_not_conversation_schema() {
    let body = concat!(
        "data: {\"model\":\"vendor/model\",\"choices\":[{\"delta\":{\"content\":\"partial\"}}]}\n\n",
        "data: {\"model\":\"vendor/model\",\"choices\":[{\"delta\":null}]}\n\n",
    );
    let (mut service, store, server, _directory) = sse_service(body, false).await;
    let (conversation_id, _) = service
        .start_turn(None, "vendor/model".to_owned(), "hello".to_owned())
        .await
        .unwrap();
    loop {
        let event = tokio::time::timeout(Duration::from_secs(1), service.next_event())
            .await
            .unwrap()
            .unwrap();
        if let OpenRouterServiceEvent::TurnFinished {
            outcome,
            incomplete_assistant_text,
            failure,
            failure_stage,
            ..
        } = event
        {
            assert_eq!(outcome, OpenRouterTurnOutcome::Failed);
            assert_eq!(incomplete_assistant_text.as_deref(), Some("partial"));
            assert_eq!(failure, Some(OpenRouterFailureCategory::InvalidResponse));
            assert_eq!(failure_stage, Some(OpenRouterStreamStage::CompletionShape));
            break;
        }
    }
    server.await.unwrap();

    let conversation = store.load_conversation(&conversation_id).unwrap();
    assert_eq!(conversation.turns[0].outcome, OpenRouterTurnOutcome::Failed);
    assert_eq!(
        conversation.turns[0].incomplete_assistant_text.as_deref(),
        Some("partial")
    );
    let persisted = serde_json::to_string(&conversation).unwrap();
    assert!(!persisted.contains("failure_stage"));
    assert!(!persisted.contains("CompletionShape"));
}

#[tokio::test]
async fn malformed_terminal_usage_persists_completed_answer_and_reopens_canonically() {
    let body = concat!(
        "data: {\"id\":\"chat-1\",\"model\":\"vendor/resolved-model\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"answer\"}}]}\n\n",
        "data: {\"id\":\"chat-1\",\"model\":\"vendor/resolved-model\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
        "data: {\"id\":\"metadata-only\",\"model\":\"vendor/resolved-usage\",\"choices\":[],\"usage\":{\"total_tokens\":null}}\n\n",
        "data: [DONE]\n\n",
    );
    let (mut service, store, server, directory) = sse_service(body, false).await;
    let (conversation_id, _) = service
        .start_turn(
            None,
            "vendor/requested-alias".to_owned(),
            "hello".to_owned(),
        )
        .await
        .unwrap();
    loop {
        let event = tokio::time::timeout(Duration::from_secs(1), service.next_event())
            .await
            .unwrap()
            .unwrap();
        if let OpenRouterServiceEvent::TurnFinished {
            outcome,
            assistant_text,
            incomplete_assistant_text,
            usage,
            failure,
            ..
        } = event
        {
            assert_eq!(outcome, OpenRouterTurnOutcome::Completed);
            assert_eq!(assistant_text.as_deref(), Some("answer"));
            assert_eq!(incomplete_assistant_text, None);
            assert_eq!(usage, None);
            assert_eq!(failure, None);
            break;
        }
    }
    server.await.unwrap();
    let record = &store.load_conversation(&conversation_id).unwrap().turns[0];
    assert_eq!(record.outcome, OpenRouterTurnOutcome::Completed);
    assert_eq!(record.model_id, "vendor/requested-alias");
    assert_eq!(record.assistant_text.as_deref(), Some("answer"));
    assert_eq!(record.incomplete_assistant_text, None);
    drop(service);
    drop(store);

    let reopened = FileOpenRouterStore::new(directory.path().join("openrouter")).unwrap();
    let conversation = reopened.load_conversation(&conversation_id).unwrap();
    assert_eq!(
        conversation.turns[0].outcome,
        OpenRouterTurnOutcome::Completed
    );
    assert_eq!(
        conversation.turns[0].assistant_text.as_deref(),
        Some("answer")
    );
    assert_eq!(conversation.turns[0].incomplete_assistant_text, None);
    let canonical = conversation.canonical_messages();
    assert_eq!(canonical.len(), 2);
    assert_eq!(canonical[0].content, "hello");
    assert_eq!(canonical[1].content, "answer");
}

#[tokio::test]
async fn failed_stream_without_a_delta_persists_no_incomplete_text() {
    let body = "data: {\"error\":{\"code\":429,\"message\":\"private detail\",\"metadata\":{\"error_type\":\"rate_limit_exceeded\"}},\"choices\":[]}\n\n";
    let (mut service, store, server, _directory) = sse_service(body, false).await;
    let (conversation_id, _) = service
        .start_turn(None, "vendor/model".to_owned(), "hello".to_owned())
        .await
        .unwrap();
    loop {
        let event = tokio::time::timeout(Duration::from_secs(1), service.next_event())
            .await
            .unwrap()
            .unwrap();
        if let OpenRouterServiceEvent::TurnFinished {
            outcome,
            incomplete_assistant_text,
            ..
        } = event
        {
            assert_eq!(outcome, OpenRouterTurnOutcome::Failed);
            assert_eq!(incomplete_assistant_text, None);
            break;
        }
    }
    server.await.unwrap();
    assert_eq!(
        store.load_conversation(&conversation_id).unwrap().turns[0].incomplete_assistant_text,
        None
    );
}

#[tokio::test]
async fn interruption_after_a_delta_discards_incomplete_text() {
    let body = "data: {\"id\":\"chat-1\",\"model\":\"vendor/model\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"partial\"},\"finish_reason\":null}]}\n\n";
    let (mut service, store, server, _directory) = sse_service(body, true).await;
    let (conversation_id, _) = service
        .start_turn(None, "vendor/model".to_owned(), "hello".to_owned())
        .await
        .unwrap();
    loop {
        let event = tokio::time::timeout(Duration::from_secs(1), service.next_event())
            .await
            .unwrap()
            .unwrap();
        if matches!(event, OpenRouterServiceEvent::TextDelta { .. }) {
            service.interrupt_turn();
            break;
        }
    }
    loop {
        let event = tokio::time::timeout(Duration::from_secs(1), service.next_event())
            .await
            .unwrap()
            .unwrap();
        if let OpenRouterServiceEvent::TurnFinished {
            outcome,
            assistant_text,
            incomplete_assistant_text,
            ..
        } = event
        {
            assert_eq!(outcome, OpenRouterTurnOutcome::Interrupted);
            assert_eq!(assistant_text, None);
            assert_eq!(incomplete_assistant_text, None);
            break;
        }
    }
    server.abort();
    let _ = server.await;
    let record = &store.load_conversation(&conversation_id).unwrap().turns[0];
    assert_eq!(record.outcome, OpenRouterTurnOutcome::Interrupted);
    assert_eq!(record.incomplete_assistant_text, None);
}
