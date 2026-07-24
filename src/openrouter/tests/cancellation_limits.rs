use super::*;

#[tokio::test]
async fn malformed_sse_and_cancellation_fail_once_without_post_retry() {
    let (base, requests, server) = fake_server(vec![Script::sse(&["data: not-json\n\n"])]).await;
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
    assert_eq!(error.category(), OpenRouterFailureCategory::InvalidResponse);
    assert_eq!(error.stage(), Some(OpenRouterStreamStage::ChunkJson));
    assert_eq!(requests.lock().unwrap().len(), 1);

    let mut hanging = Script::sse(&["data: {\"choices\":[]}\n\n", "data: [DONE]\n\n"]);
    hanging.delay = Duration::from_secs(1);
    let (base, requests, server) = fake_server(vec![hanging]).await;
    let client = test_client(base, credentials());
    let cancellation = CancellationToken::new();
    let canceller = cancellation.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(20)).await;
        canceller.cancel();
    });
    let error = client
        .chat(&request, cancellation, |_| {})
        .await
        .unwrap_err();
    assert_eq!(error.category(), OpenRouterFailureCategory::Cancelled);
    assert_eq!(requests.lock().unwrap().len(), 1);
    server.abort();
}

#[tokio::test]
async fn chat_stages_content_type_and_premature_eof_failures() {
    let mut wrong_content_type = Script::json(200, "{}");
    wrong_content_type.content_type = "text/event-streaming";
    let (base, _, server) = fake_server(vec![wrong_content_type]).await;
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
    assert_eq!(error.stage(), Some(OpenRouterStreamStage::ContentType));

    let (base, _, server) = fake_server(vec![Script::sse(&[
        "data: {\"choices\":[{\"delta\":{\"content\":\"partial\"}}]}\n\n",
    ])])
    .await;
    let error = test_client(base, credentials())
        .chat(&request, CancellationToken::new(), |_| {})
        .await
        .unwrap_err();
    server.await.unwrap();
    assert_eq!(error.stage(), Some(OpenRouterStreamStage::PrematureEof));
}

#[tokio::test]
async fn stalled_sse_hits_idle_timeout_without_post_retry() {
    let mut stalled = Script::sse(&["data: {\"choices\":[]}\n\n", "data: [DONE]\n\n"]);
    stalled.delay = Duration::from_secs(1);
    let (base, requests, server) = fake_server(vec![stalled]).await;
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
    assert_eq!(error.category(), OpenRouterFailureCategory::Timeout);
    assert_eq!(requests.lock().unwrap().len(), 1);
    server.abort();
}

#[test]
fn outbound_request_is_bounded_before_network() {
    let oversized = "x".repeat(1024 * 1024);
    assert_eq!(
        ChatRequest::new(
            "vendor/model",
            vec![ChatMessage {
                role: ChatRole::User,
                content: oversized,
            }],
        )
        .unwrap_err()
        .category(),
        OpenRouterFailureCategory::ResourceLimit
    );
}
