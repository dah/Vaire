use super::*;

#[tokio::test]
async fn chat_classifies_a_bare_error_finish_after_terminal_as_remote() {
    let scripts = vec![Script::sse(&[
        "data: {\"id\":\"chat-1\",\"model\":\"vendor/model\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"partial\"},\"finish_reason\":null}]}\n\n",
        "data: {\"id\":\"chat-1\",\"model\":\"vendor/model\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
        "data: {\"id\":\"chat-1\",\"model\":\"vendor/model\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"error\"}]}\n\n",
    ])];
    let (base, _, server) = fake_server(scripts).await;
    let client = test_client(base, credentials());
    let request = ChatRequest::new(
        "vendor/model",
        vec![ChatMessage {
            role: ChatRole::User,
            content: "hi".to_owned(),
        }],
    )
    .unwrap();
    let mut events = Vec::new();

    let error = client
        .chat(&request, CancellationToken::new(), |event| {
            events.push(event)
        })
        .await
        .unwrap_err();
    server.await.unwrap();

    assert_eq!(error.category(), OpenRouterFailureCategory::Remote);
    assert_eq!(error.stage(), None);
    assert_eq!(
        events,
        vec![ChatStreamEvent::TextDelta("partial".to_owned())]
    );
    assert!(!events
        .iter()
        .any(|event| matches!(event, ChatStreamEvent::Finished { .. })));
}

#[tokio::test]
async fn chat_rejects_a_conflicting_later_semantic_model() {
    let scripts = vec![Script::sse(&[
        "data: {\"model\":\"vendor/resolved-a\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"partial\"}}]}\n\n",
        "data: {\"model\":\"vendor/resolved-b\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
    ])];
    let (base, _, server) = fake_server(scripts).await;
    let client = test_client(base, credentials());
    let request = ChatRequest::new(
        "vendor/requested-alias",
        vec![ChatMessage {
            role: ChatRole::User,
            content: "hi".to_owned(),
        }],
    )
    .unwrap();
    let mut events = Vec::new();

    let error = client
        .chat(&request, CancellationToken::new(), |event| {
            events.push(event)
        })
        .await
        .unwrap_err();
    server.await.unwrap();

    assert_eq!(error.category(), OpenRouterFailureCategory::InvalidResponse);
    assert_eq!(error.stage(), Some(OpenRouterStreamStage::Model));
    assert_eq!(
        events,
        vec![ChatStreamEvent::TextDelta("partial".to_owned())]
    );
}

#[tokio::test]
async fn chat_classifies_a_documented_midstream_error_after_partial_text() {
    let scripts = vec![Script::sse(&[
        "data: {\"id\":\"chat-1\",\"model\":\"moonshotai/kimi-k3\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"partial\"},\"finish_reason\":null}]}\n\n",
        "data: {\"id\":\"chat-1\",\"model\":\"moonshotai/kimi-k3\",\"error\":{\"code\":429,\"message\":\"sensitive upstream detail\",\"metadata\":{\"error_type\":\"rate_limit_exceeded\"}},\"choices\":[{\"index\":0,\"delta\":{\"content\":\"\"},\"finish_reason\":\"error\"}]}\n\n",
    ])];
    let (base, _, server) = fake_server(scripts).await;
    let client = test_client(base, credentials());
    let request = ChatRequest::new(
        "moonshotai/kimi-k3",
        vec![ChatMessage {
            role: ChatRole::User,
            content: "hi".to_owned(),
        }],
    )
    .unwrap();
    let mut events = Vec::new();

    let error = client
        .chat(&request, CancellationToken::new(), |event| {
            events.push(event)
        })
        .await
        .unwrap_err();
    server.await.unwrap();

    assert_eq!(error.category(), OpenRouterFailureCategory::RateLimited);
    assert_eq!(error.status(), Some(429));
    assert_eq!(error.stage(), None);
    assert!(events.contains(&ChatStreamEvent::TextDelta("partial".to_owned())));
    assert!(!events
        .iter()
        .any(|event| matches!(event, ChatStreamEvent::Finished { .. })));
    assert!(!format!("{error:?}").contains("sensitive upstream detail"));
}

#[tokio::test]
async fn chat_provider_error_precedes_malformed_siblings() {
    for error_event in [
        "data: {\"error\":{\"code\":429,\"message\":\"SENSITIVE\"},\"choices\":null,\"usage\":{\"total_tokens\":\"bad\"}}\n\n",
        "data: {\"error\":{\"code\":\"429\"},\"choices\":[{\"index\":0,\"delta\":null}]}\n\n",
        "data: {\"error\":{\"code\":\"429\",\"message\":\"SENSITIVE\",\"metadata\":{\"error_type\":\"authentication\"}},\"choices\":null}\n\n",
    ] {
        let (base, _, server) = fake_server(vec![Script::sse(&[error_event])]).await;
        let client = test_client(base, credentials());
        let request = ChatRequest::new(
            "vendor/model",
            vec![ChatMessage {
                role: ChatRole::User,
                content: "hi".to_owned(),
            }],
        )
        .unwrap();
        let error = client
            .chat(&request, CancellationToken::new(), |_| {})
            .await
            .unwrap_err();
        server.await.unwrap();
        assert_eq!(error.category(), OpenRouterFailureCategory::RateLimited);
        assert_eq!(error.status(), Some(429));
        assert_eq!(error.stage(), None);
        assert!(!format!("{error:?}").contains("SENSITIVE"));
        assert!(!error.to_string().contains("SENSITIVE"));
    }
}

#[tokio::test]
async fn chat_completes_after_malformed_terminal_usage_and_preserves_valid_usage() {
    for (usage_events, expected_total) in [
        (
            vec!["data: {\"choices\":[],\"usage\":{\"total_tokens\":\"bad\"}}\n\n"],
            None,
        ),
        (
            vec![
                "data: {\"choices\":[],\"usage\":{\"total_tokens\":3}}\n\n",
                "data: {\"choices\":[],\"usage\":{\"total_tokens\":null}}\n\n",
            ],
            Some(3),
        ),
    ] {
        let mut parts = vec![
            "data: {\"model\":\"resolved\",\"choices\":[{\"delta\":{\"content\":\"answer\"}}]}\n\n",
            "data: {\"model\":\"resolved\",\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
        ];
        parts.extend(usage_events);
        parts.push("data: [DONE]\n\n");
        let (base, _, server) = fake_server(vec![Script::sse(&parts)]).await;
        let client = test_client(base, credentials());
        let request = ChatRequest::new(
            "requested-alias",
            vec![ChatMessage {
                role: ChatRole::User,
                content: "hi".to_owned(),
            }],
        )
        .unwrap();
        let mut events = Vec::new();
        client
            .chat(&request, CancellationToken::new(), |event| {
                events.push(event)
            })
            .await
            .unwrap();
        server.await.unwrap();
        let Some(ChatStreamEvent::Finished {
            assistant_text,
            usage,
        }) = events.last()
        else {
            panic!("missing terminal event");
        };
        assert_eq!(assistant_text, "answer");
        assert_eq!(usage.map(|usage| usage.total_tokens), expected_total);
    }
}
