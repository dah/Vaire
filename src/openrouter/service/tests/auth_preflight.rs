use super::*;

#[test]
fn persisted_title_is_control_free_and_unicode_byte_bounded() {
    let title = title_for(
        "  hello\n\u{1b}[31m界界界界界界界界界界界界界界界界界界界界界界界界界界界界界界  ",
    );
    assert!(!title.chars().any(char::is_control));
    assert!(!title.contains('\n'));
    assert!(title.len() <= 80);
    assert!(std::str::from_utf8(title.as_bytes()).is_ok());
}

#[tokio::test]
async fn request_preflight_failure_leaves_no_in_progress_conversation() {
    let directory = tempdir().unwrap();
    let store = Arc::new(FileOpenRouterStore::new(directory.path().join("openrouter")).unwrap());
    let credentials = Arc::new(FakeCredentialStore::with_openrouter_key(
        SecretValue::from_input("offline-test-key").unwrap(),
    ));
    let mut service = service(credentials, store.clone());
    let result = service
        .start_turn(None, "vendor/model".to_owned(), "x".repeat(1024 * 1024))
        .await;
    assert!(result.is_err());
    assert!(store.list_conversations().unwrap().is_empty());
}

#[tokio::test]
async fn prepared_turn_does_not_post_until_the_pointer_can_be_persisted() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = Url::parse(&format!("http://{}", listener.local_addr().unwrap())).unwrap();
    let directory = tempdir().unwrap();
    let store = Arc::new(FileOpenRouterStore::new(directory.path().join("openrouter")).unwrap());
    let credentials = Arc::new(FakeCredentialStore::with_openrouter_key(
        SecretValue::from_input("offline-test-key").unwrap(),
    ));
    let client = OpenRouterClient::with_loopback_base_url(
        base,
        credentials.clone(),
        OpenRouterTimeouts {
            connect: Duration::from_millis(20),
            get_attempt: Duration::from_millis(50),
            chat_headers: Duration::from_millis(50),
            sse_idle: Duration::from_millis(50),
            chat_total: Duration::from_millis(100),
            retry_delay: Duration::ZERO,
        },
    )
    .unwrap();
    let mut service = OpenRouterService::new(client, credentials, store.clone());

    let prepared = service
        .prepare_turn(None, "vendor/model".to_owned(), "hello".to_owned())
        .await
        .unwrap();
    let id = prepared.conversation_id().clone();
    assert!(
        tokio::time::timeout(Duration::from_millis(30), listener.accept())
            .await
            .is_err()
    );
    assert_eq!(
        store.load_conversation(&id).unwrap().turns[0].outcome,
        OpenRouterTurnOutcome::InProgress
    );

    service.abandon_prepared_turn(prepared).await.unwrap();
    assert_eq!(
        store.load_conversation(&id).unwrap().turns[0].outcome,
        OpenRouterTurnOutcome::Failed
    );
}

#[tokio::test]
async fn logout_joins_candidate_validation_before_deleting_credential() {
    let directory = tempdir().unwrap();
    let store = Arc::new(FileOpenRouterStore::new(directory.path().join("openrouter")).unwrap());
    let credentials = Arc::new(FakeCredentialStore::with_openrouter_key(
        SecretValue::from_input("old-offline-test-key").unwrap(),
    ));
    let mut service = service(credentials.clone(), store);
    service
        .replace_candidate(SecretValue::from_input("candidate-offline-test-key").unwrap())
        .unwrap();
    service.logout().await.1.unwrap();
    assert!(!credentials.is_configured(CredentialAccount::OpenRouterApiKey));
}
