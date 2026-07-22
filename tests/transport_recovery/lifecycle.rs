use super::support::*;

#[tokio::test]
async fn restart_increments_generation_and_stderr_flood_does_not_deadlock() {
    let temp = tempdir().unwrap();
    let executable = script(
        temp.path(),
        r#"
dd if=/dev/zero bs=65536 count=32 1>&2 2>/dev/null
IFS= read -r request
printf '%s\n' '{"id":1,"result":{"ok":true}}'
IFS= read -r hold
"#,
    );
    let diagnostics = MemoryDiagnosticSink::default();
    let mut first = AppServerTransport::spawn_with_diagnostics(
        spec(temp.path(), executable.clone()),
        Arc::new(diagnostics.clone()),
    )
    .await
    .unwrap();
    let first_generation = first.generation();
    let pid = first.child_pid();
    let response = first
        .request("ping", json!({}), Duration::from_secs(2))
        .await
        .unwrap();
    assert_eq!(response["ok"], true);
    first.shutdown().await.unwrap();
    assert!(!std::process::Command::new("/bin/kill")
        .args(["-0", &pid.to_string()])
        .output()
        .unwrap()
        .status
        .success());
    assert!(diagnostics
        .events()
        .iter()
        .any(|event| event.category == "stderr_bytes" && event.byte_count.is_some()));

    let mut second = AppServerTransport::spawn(spec(temp.path(), executable))
        .await
        .unwrap();
    assert!(second.generation() > first_generation);
    second.shutdown().await.unwrap();
}

#[tokio::test]
async fn unsolicited_unknown_response_id_closes_as_protocol_error() {
    let temp = tempdir().unwrap();
    let executable = script(
        temp.path(),
        r#"
printf '%s\n' '{"id":999,"result":{}}'
sleep 1
"#,
    );
    let mut transport = AppServerTransport::spawn(spec(temp.path(), executable))
        .await
        .unwrap();
    let event = tokio::time::timeout(Duration::from_secs(1), transport.next_event())
        .await
        .unwrap()
        .unwrap();
    assert!(
        matches!(event.event, InboundEvent::ConnectionClosed { category } if category == "protocol")
    );
    transport.shutdown().await.unwrap();
}

#[tokio::test]
async fn in_flight_requests_fail_on_uncorrelatable_response_ids_and_eof() {
    for (body, expected_category) in [
        (
            r#"
IFS= read -r request
printf '%s\n' '{"id":"1","result":{}}'
sleep 1
"#,
            "protocol",
        ),
        (
            r#"
IFS= read -r request
exit 0
"#,
            "eof",
        ),
    ] {
        let temp = tempdir().unwrap();
        let executable = script(temp.path(), body);
        let mut transport = AppServerTransport::spawn(spec(temp.path(), executable))
            .await
            .unwrap();

        let result = transport
            .request("pending", json!({}), Duration::from_secs(1))
            .await;
        match expected_category {
            "protocol" => assert!(matches!(result, Err(TransportError::Protocol(_)))),
            "eof" => assert_eq!(result, Err(TransportError::Closed)),
            _ => unreachable!(),
        }
        let event = tokio::time::timeout(Duration::from_secs(1), transport.next_event())
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(
            event.event,
            InboundEvent::ConnectionClosed { category } if category == expected_category
        ));
        transport.shutdown().await.unwrap();
    }
}
