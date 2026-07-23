use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;
use url::Url;

use crate::credentials::{FakeCredentialOperation, FakeCredentialStore, SecretValue};

use super::{
    ChatMessage, ChatRequest, ChatRole, ChatStreamEvent, OpenRouterClient,
    OpenRouterFailureCategory, OpenRouterStreamStage, OpenRouterTimeouts, TokenUsage,
};

const TEST_KEY: &str = "sk-or-v1-recognizable-offline-test-key";

#[derive(Clone)]
struct Script {
    status: u16,
    content_type: &'static str,
    headers: Vec<(&'static str, &'static str)>,
    parts: Vec<Vec<u8>>,
    delay: Duration,
}

impl Script {
    fn json(status: u16, body: &str) -> Self {
        Self {
            status,
            content_type: "application/json",
            headers: Vec::new(),
            parts: vec![body.as_bytes().to_vec()],
            delay: Duration::ZERO,
        }
    }

    fn sse(parts: &[&str]) -> Self {
        Self {
            status: 200,
            content_type: "text/event-stream",
            headers: Vec::new(),
            parts: parts.iter().map(|part| part.as_bytes().to_vec()).collect(),
            delay: Duration::from_millis(5),
        }
    }
}

async fn fake_server(
    scripts: Vec<Script>,
) -> (Url, Arc<Mutex<Vec<Vec<u8>>>>, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let captured = requests.clone();
    let task = tokio::spawn(async move {
        for script in scripts {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut buffer = [0u8; 4096];
            let header_end = loop {
                let count = socket.read(&mut buffer).await.unwrap();
                if count == 0 {
                    return;
                }
                request.extend_from_slice(&buffer[..count]);
                if let Some(position) = request.windows(4).position(|value| value == b"\r\n\r\n") {
                    break position + 4;
                }
            };
            let headers = String::from_utf8_lossy(&request[..header_end]);
            let content_length = headers
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().ok())
                        .flatten()
                })
                .unwrap_or(0);
            while request.len() < header_end + content_length {
                let count = socket.read(&mut buffer).await.unwrap();
                if count == 0 {
                    break;
                }
                request.extend_from_slice(&buffer[..count]);
            }
            captured.lock().unwrap().push(request);
            let reason = if script.status == 200 {
                "OK"
            } else if script.status == 503 {
                "Service Unavailable"
            } else if script.status == 401 {
                "Unauthorized"
            } else {
                "Error"
            };
            let total: usize = script.parts.iter().map(Vec::len).sum();
            let mut response = format!(
                "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n",
                script.status, reason, script.content_type, total
            );
            for (name, value) in script.headers {
                response.push_str(name);
                response.push_str(": ");
                response.push_str(value);
                response.push_str("\r\n");
            }
            response.push_str("\r\n");
            if socket.write_all(response.as_bytes()).await.is_err() {
                continue;
            }
            for part in script.parts {
                if socket.write_all(&part).await.is_err() {
                    break;
                }
                if !script.delay.is_zero() {
                    tokio::time::sleep(script.delay).await;
                }
            }
        }
    });
    (
        Url::parse(&format!("http://{address}")).unwrap(),
        requests,
        task,
    )
}

fn test_client(base: Url, store: Arc<FakeCredentialStore>) -> OpenRouterClient {
    OpenRouterClient::with_loopback_base_url(
        base,
        store,
        OpenRouterTimeouts {
            connect: Duration::from_secs(1),
            get_attempt: Duration::from_secs(1),
            chat_headers: Duration::from_secs(1),
            sse_idle: Duration::from_millis(200),
            chat_total: Duration::from_secs(2),
            retry_delay: Duration::from_millis(1),
        },
    )
    .unwrap()
}

fn credentials() -> Arc<FakeCredentialStore> {
    Arc::new(FakeCredentialStore::with_openrouter_key(
        SecretValue::from_input(TEST_KEY).unwrap(),
    ))
}

fn request_text(request: &[u8]) -> String {
    String::from_utf8_lossy(request).into_owned()
}

#[tokio::test]
async fn key_and_catalog_use_exact_authenticated_paths_and_load_per_operation() {
    let scripts = vec![
        Script::json(200, r#"{"data":{"label":"test"}}"#),
        Script::json(
            200,
            r#"{"data":[{"id":"vendor/model","name":"First","context_length":4096},{"id":"vendor/model","name":"Duplicate","context_length":8}]}"#,
        ),
    ];
    let (base, requests, server) = fake_server(scripts).await;
    let store = credentials();
    let client = test_client(base, store.clone());

    client
        .validate_stored_key(CancellationToken::new())
        .await
        .unwrap();
    let catalog = client
        .fetch_catalog(CancellationToken::new())
        .await
        .unwrap();
    server.await.unwrap();

    assert_eq!(catalog.len(), 1);
    assert_eq!(catalog[0].name.as_deref(), Some("First"));
    let requests = requests.lock().unwrap();
    let key = request_text(&requests[0]);
    let catalog_request = request_text(&requests[1]);
    assert!(key.starts_with("GET /api/v1/key HTTP/1.1\r\n"));
    assert!(catalog_request.starts_with("GET /api/v1/models/user HTTP/1.1\r\n"));
    assert_eq!("Vairë".as_bytes(), &[0x56, 0x61, 0x69, 0x72, 0xC3, 0xAB]);
    let title_header = [
        b"x-title: ".as_slice(),
        &[0x56, 0x61, 0x69, 0x72, 0xC3, 0xAB],
        b"\r\n".as_slice(),
    ]
    .concat();
    for request in [&requests[0], &requests[1]] {
        assert!(request_text(request)
            .to_ascii_lowercase()
            .contains(&format!("authorization: bearer {}", TEST_KEY).to_ascii_lowercase()));
        assert!(request
            .windows(title_header.len())
            .any(|window| window == title_header));
        assert!(request
            .windows(b"user-agent: vaire/".len())
            .any(|window| window == b"user-agent: vaire/"));
    }
    assert_eq!(
        store.operations(),
        vec![
            FakeCredentialOperation::Load(crate::credentials::CredentialAccount::OpenRouterApiKey),
            FakeCredentialOperation::Load(crate::credentials::CredentialAccount::OpenRouterApiKey),
        ]
    );
    assert!(!format!("{client:?}").contains(TEST_KEY));
}

#[tokio::test]
async fn catalog_retries_only_retryable_get_and_remote_bodies_are_redacted() {
    let mut unavailable = Script::json(503, TEST_KEY);
    unavailable.headers.push(("Retry-After", "0"));
    let scripts = vec![
        unavailable,
        Script::json(200, r#"{"data":[{"id":"vendor/model"}]}"#),
    ];
    let (base, requests, server) = fake_server(scripts).await;
    let client = test_client(base, credentials());
    assert_eq!(
        client
            .fetch_catalog(CancellationToken::new())
            .await
            .unwrap()
            .len(),
        1
    );
    server.await.unwrap();
    assert_eq!(requests.lock().unwrap().len(), 2);

    let (base, _, server) = fake_server(vec![Script::json(
        401,
        &format!(r#"{{"error":"{TEST_KEY}"}}"#),
    )])
    .await;
    let error = test_client(base, credentials())
        .fetch_catalog(CancellationToken::new())
        .await
        .unwrap_err();
    server.await.unwrap();
    assert_eq!(error.category(), OpenRouterFailureCategory::Unauthorized);
    assert!(!format!("{error:?}").contains(TEST_KEY));
    assert!(!error.to_string().contains(TEST_KEY));
}

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
