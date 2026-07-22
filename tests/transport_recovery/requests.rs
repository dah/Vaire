use super::support::*;

#[tokio::test]
async fn late_timed_out_response_is_ignored_and_connection_remains_usable() {
    let temp = tempdir().unwrap();
    let executable = script(
        temp.path(),
        r#"
IFS= read -r first
IFS= read -r release
printf '%s\n' '{"id":1,"result":{"late":true}}'
IFS= read -r second
printf '%s\n' '{"id":2,"result":{"ok":true}}'
IFS= read -r hold
"#,
    );
    let diagnostics = MemoryDiagnosticSink::default();
    let mut transport = AppServerTransport::spawn_with_diagnostics(
        spec(temp.path(), executable),
        Arc::new(diagnostics.clone()),
    )
    .await
    .unwrap();
    let timeouts = RequestTimeouts {
        fallback: Duration::from_millis(20),
        ..RequestTimeouts::default()
    };
    transport.set_timeouts(timeouts);
    assert_eq!(
        transport.request_default("slow", json!({})).await,
        Err(TransportError::Timeout)
    );
    transport.notify("release", json!({})).await.unwrap();
    let response = transport
        .request("next", json!({}), Duration::from_secs(1))
        .await
        .unwrap();
    assert_eq!(response["ok"], true);
    let events = diagnostics.events();
    assert!(events
        .iter()
        .any(|event| event.category == "request_timeout"));
    assert!(events
        .iter()
        .any(|event| event.category == "stale_response"));
    transport.shutdown().await.unwrap();
}

#[tokio::test]
async fn oversized_outbound_request_is_rejected_without_closing_the_connection() {
    let temp = tempdir().unwrap();
    let executable = script(
        temp.path(),
        r#"
IFS= read -r request
printf '%s\n' '{"id":1,"result":{"ok":true}}'
IFS= read -r hold
"#,
    );
    let mut transport = AppServerTransport::spawn(spec(temp.path(), executable))
        .await
        .unwrap();

    assert!(matches!(
        transport
            .request("invalid-timeout", json!({}), Duration::MAX)
            .await,
        Err(TransportError::Protocol(message)) if message.contains("timeout range")
    ));
    let oversized = "x".repeat(agentharness::codex::transport::MAX_FRAME_BYTES);
    assert!(matches!(
        transport
            .notify("oversized-notify", json!({"value": oversized.clone()}))
            .await,
        Err(TransportError::Protocol(message)) if message.contains("size limit")
    ));
    assert!(matches!(
        transport
            .request("oversized", json!({"value": oversized}), Duration::from_secs(1))
            .await,
        Err(TransportError::Protocol(message)) if message.contains("size limit")
    ));
    let response = transport
        .request("ping", json!({}), Duration::from_secs(1))
        .await
        .unwrap();
    assert_eq!(response["ok"], true);

    transport.shutdown().await.unwrap();
}

#[tokio::test]
async fn concurrent_request_admission_and_cancellation_are_bounded_without_closing_the_connection()
{
    let temp = tempdir().unwrap();
    let executable = script(
        temp.path(),
        r#"
held=0
while IFS= read -r request; do
  case "$request" in
    *'"method":"held"'*) held=$((held + 1)) ;;
    *'"method":"ping"'*) printf '{"id":%s,"result":{"ok":true}}\n' "$((held + 1))" ;;
  esac
done
"#,
    );
    let mut transport = AppServerTransport::spawn(spec(temp.path(), executable))
        .await
        .unwrap();
    let mut requests = FuturesUnordered::new();
    for sequence in 0..=MAX_PENDING_REQUESTS {
        requests.push(transport.request(
            "held",
            json!({"sequence": sequence}),
            Duration::from_secs(5),
        ));
    }

    let overload = tokio::time::timeout(Duration::from_secs(2), async {
        while let Some(result) = requests.next().await {
            if matches!(
                &result,
                Err(TransportError::Protocol(message)) if message.contains("concurrent")
            ) {
                return result;
            }
        }
        panic!("all requests settled without an admission error");
    })
    .await
    .expect("unbounded in-flight requests never applied backpressure");
    assert!(matches!(overload, Err(TransportError::Protocol(_))));

    // Dropping the remaining callers must immediately retire their 128 pending IDs. A useful
    // request should not have to wait for their five-second deadlines before it can be admitted.
    drop(requests);

    let response = transport
        .request("ping", json!({}), Duration::from_secs(1))
        .await
        .unwrap();
    assert_eq!(response["ok"], true);
    transport.shutdown().await.unwrap();
}
