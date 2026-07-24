use super::*;

#[tokio::test]
async fn chat_posts_canonical_body_and_decodes_fragmented_sse_without_retry() {
    let scripts = vec![Script::sse(&[
        ": keepalive\nevent: completion\ndata: {\"id\":\"chat-1\",\"model\":\"vendor/model\",\"choices\":[{\"index\":0,\"delta\":{\"reasoning\":\"not collected\"},\"finish_reason\":null}]}\n\n",
        "data: {\"id\":\"chat-1\",\"model\":\"vendor/model\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hel",
        "lo\"},\"finish_reason\":null}]}\n\n",
        "data: {\"id\":\"chat-1\",\"model\":\"vendor/model\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
        "data: {\"id\":\"chat-1\",\"model\":\"vendor/model\",\"choices\":[],\"usage\":{\"prompt_tokens\":2,\"completion_tokens\":1,\"total_tokens\":3}}\n\n",
        "data: [DONE]\n\n",
    ])];
    let (base, requests, server) = fake_server(scripts).await;
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
    client
        .chat(&request, CancellationToken::new(), |event| {
            events.push(event)
        })
        .await
        .unwrap();
    server.await.unwrap();

    assert!(events.contains(&ChatStreamEvent::TextDelta("hello".to_owned())));
    assert!(matches!(
        events.last(),
        Some(ChatStreamEvent::Finished {
            assistant_text,
            usage: Some(_)
        }) if assistant_text == "hello"
    ));
    let captured = requests.lock().unwrap();
    assert_eq!(captured.len(), 1);
    let request = request_text(&captured[0]);
    assert!(request.starts_with("POST /api/v1/chat/completions HTTP/1.1\r\n"));
    let lowercase_request = request.to_ascii_lowercase();
    assert!(lowercase_request
        .contains(&format!("authorization: bearer {TEST_KEY}").to_ascii_lowercase()));
    assert!(lowercase_request.contains("accept: text/event-stream"));
    assert!(lowercase_request.contains("content-type: application/json"));
    let body = request.split_once("\r\n\r\n").unwrap().1;
    let json: serde_json::Value = serde_json::from_str(body).unwrap();
    assert_eq!(json["model"], "vendor/model");
    assert_eq!(json["stream"], true);
    assert_eq!(json["stream_options"]["include_usage"], true);
    assert!(json.get("tools").is_none());
    assert!(json.get("reasoning").is_none());
    assert!(!body.contains(TEST_KEY));
}

#[tokio::test]
async fn chat_accepts_repeated_empty_non_error_finish_markers() {
    let scripts = vec![Script::sse(&[
        "data: {\"id\":\"chat-1\",\"model\":\"vendor/model\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hello\"},\"finish_reason\":null}]}\n\n",
        "data: {\"id\":\"chat-1\",\"model\":\"vendor/model\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
        "data: {\"id\":\"chat-1\",\"model\":\"vendor/model\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"length\"}],\"usage\":{\"total_tokens\":3}}\n\n",
        "data: [DONE]\n\n",
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

    client
        .chat(&request, CancellationToken::new(), |event| {
            events.push(event)
        })
        .await
        .unwrap();
    server.await.unwrap();

    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, ChatStreamEvent::TextDelta(_)))
            .count(),
        1
    );
    assert_eq!(
        events
            .iter()
            .filter_map(|event| match event {
                ChatStreamEvent::TextDelta(text) => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>(),
        vec!["hello"]
    );
    assert!(events.contains(&ChatStreamEvent::Usage(TokenUsage {
        total_tokens: 3,
        ..TokenUsage::default()
    })));
    assert!(matches!(
        events.last(),
        Some(ChatStreamEvent::Finished {
            assistant_text,
            usage: Some(TokenUsage { total_tokens: 3, .. })
        }) if assistant_text == "hello"
    ));
}

#[tokio::test]
async fn chat_accepts_resolved_model_metadata_in_the_terminal_usage_chunk() {
    let scripts = vec![Script::sse(&[
        "data: {\"id\":\"chat-1\",\"model\":\"moonshotai/kimi-k3\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hello\"},\"finish_reason\":null}]}\n\n",
        "data: {\"id\":\"chat-1\",\"model\":\"moonshotai/kimi-k3\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
        "data: {\"id\":\"chat-1\",\"model\":\"moonshotai/kimi-k3-20260715\",\"choices\":[],\"usage\":{\"prompt_tokens\":2,\"completion_tokens\":1,\"total_tokens\":3}}\n\n",
        "data: [DONE]\n\n",
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

    client
        .chat(&request, CancellationToken::new(), |event| {
            events.push(event)
        })
        .await
        .unwrap();
    server.await.unwrap();

    assert!(matches!(
        events.last(),
        Some(ChatStreamEvent::Finished {
            assistant_text,
            usage: Some(_)
        }) if assistant_text == "hello"
    ));
}

#[tokio::test]
async fn chat_accepts_a_semantic_resolved_alias_and_ignores_metadata_identity() {
    let scripts = vec![Script::sse(&[
        "data: {\"id\":\"chat-resolved\",\"model\":\"vendor/resolved-20260723\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"resolved\"}}]}\n\n",
        "data: {\"id\":\"chat-resolved\",\"model\":\"vendor/resolved-20260723\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
        "data: {\"id\":\"metadata-other\",\"model\":\"vendor/resolved-usage\",\"choices\":[],\"usage\":{\"total_tokens\":4}}\n\n",
        "data: [DONE]\n\n",
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

    client
        .chat(&request, CancellationToken::new(), |event| {
            events.push(event)
        })
        .await
        .unwrap();
    server.await.unwrap();

    assert!(matches!(
        events.last(),
        Some(ChatStreamEvent::Finished {
            assistant_text,
            usage: Some(TokenUsage { total_tokens: 4, .. })
        }) if assistant_text == "resolved"
    ));
}
