use super::*;

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
