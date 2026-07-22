use super::support::*;

#[tokio::test]
async fn notification_flood_is_bounded_and_closes_the_connection() {
    let temp = tempdir().unwrap();
    let executable = script(
        temp.path(),
        &format!(
            r#"
i=0
while [ "$i" -lt {} ]; do
  printf '%s\n' '{{"method":"future/event","params":{{"payload":"x"}}}}'
  i=$((i + 1))
done
sleep 1
"#,
            EVENT_QUEUE_CAPACITY + 1_000
        ),
    );
    let diagnostics = MemoryDiagnosticSink::default();
    let mut transport = AppServerTransport::spawn_with_diagnostics(
        spec(temp.path(), executable),
        Arc::new(diagnostics.clone()),
    )
    .await
    .unwrap();

    for _ in 0..200 {
        if diagnostics
            .events()
            .iter()
            .any(|event| event.category == "event_backlog")
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(diagnostics
        .events()
        .iter()
        .any(|event| event.category == "event_backlog"));
    let mut received = 0;
    while tokio::time::timeout(Duration::from_secs(2), transport.next_event())
        .await
        .unwrap()
        .is_some()
    {
        received += 1;
    }
    assert_eq!(received, EVENT_QUEUE_CAPACITY);
    assert_eq!(
        transport
            .request("ping", json!({}), Duration::from_millis(100))
            .await,
        Err(TransportError::Closed)
    );
    transport.shutdown().await.unwrap();
}

#[tokio::test]
async fn unrendered_tool_progress_flood_is_dropped_before_the_event_queue() {
    let temp = tempdir().unwrap();
    let flood = unrendered_tool_flood(EVENT_QUEUE_CAPACITY + 32);
    let executable = script(
        temp.path(),
        &format!(
            r#"
{flood}
printf '%s\n' '{{"method":"item/agentMessage/delta","params":{{"threadId":"thr-tools","turnId":"turn-tools","itemId":"item-agent","delta":"done"}}}}'
printf '%s\n' '{{"method":"item/reasoning/summaryTextDelta","params":{{"threadId":"thr-tools","turnId":"turn-tools","itemId":"item-reasoning","summaryIndex":0,"delta":"summary"}}}}'
printf '%s\n' '{{"method":"item/completed","params":{{"threadId":"thr-tools","turnId":"turn-tools","item":{{"id":"item-reasoning","type":"reasoning","summary":["summary"],"content":[]}}}}}}'
printf '%s\n' '{{"method":"item/completed","params":{{"threadId":"thr-tools","turnId":"turn-tools","item":{{"id":"item-agent","type":"agentMessage","text":"done"}}}}}}'
printf '%s\n' '{{"method":"thread/tokenUsage/updated","params":{{"threadId":"thr-tools","turnId":"turn-tools","tokenUsage":{{"last":{{"cachedInputTokens":0,"inputTokens":1,"outputTokens":1,"reasoningOutputTokens":0,"totalTokens":2}},"total":{{"cachedInputTokens":0,"inputTokens":1,"outputTokens":1,"reasoningOutputTokens":0,"totalTokens":2}},"modelContextWindow":100}}}}}}'
printf '%s\n' '{{"method":"turn/completed","params":{{"threadId":"thr-tools","turn":{{"id":"turn-tools","items":[],"status":"completed"}}}}}}'
printf '%s\n' '{{"method":"error","params":{{"threadId":"other-thread","turnId":"other-turn","willRetry":false,"error":{{"message":"unrelated","additionalDetails":null}}}}}}'
IFS= read -r request
printf '%s\n' '{{"id":1,"result":{{"ok":true}}}}'
IFS= read -r hold
"#
        ),
    );
    let diagnostics = MemoryDiagnosticSink::default();
    let mut transport = AppServerTransport::spawn_with_diagnostics(
        spec(temp.path(), executable),
        Arc::new(diagnostics.clone()),
    )
    .await
    .unwrap();

    for expected in [
        "item/agentMessage/delta",
        "item/reasoning/summaryTextDelta",
        "item/completed",
        "item/completed",
        "thread/tokenUsage/updated",
        "turn/completed",
        "error",
    ] {
        let event = tokio::time::timeout(Duration::from_secs(2), transport.next_event())
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(
            event.event,
            InboundEvent::Notification { ref method, .. } if method == expected
        ));
    }

    let response = transport
        .request("ping", json!({}), Duration::from_secs(1))
        .await
        .unwrap();
    assert_eq!(response["ok"], true);
    assert!(!diagnostics
        .events()
        .iter()
        .any(|event| event.category == "event_backlog"));

    transport.shutdown().await.unwrap();
}

#[tokio::test]
async fn filtered_tool_flood_does_not_starve_approval_denial() {
    let temp = tempdir().unwrap();
    let capture = temp.path().join("denial.json");
    let flood = unrendered_tool_flood(EVENT_QUEUE_CAPACITY + 32);
    let executable = script(
        temp.path(),
        &format!(
            r#"
{flood}
printf '%s\n' '{{"id":"approval-1","method":"item/commandExecution/requestApproval","params":{{}}}}'
IFS= read -r denial
printf '%s\n' "$denial" > "$FAKE_CAPTURE"
IFS= read -r hold
"#
        ),
    );
    let diagnostics = MemoryDiagnosticSink::default();
    let mut process_spec = spec(temp.path(), executable);
    process_spec.env.push((
        OsString::from("FAKE_CAPTURE"),
        capture.as_os_str().to_owned(),
    ));
    let mut transport =
        AppServerTransport::spawn_with_diagnostics(process_spec, Arc::new(diagnostics.clone()))
            .await
            .unwrap();

    let event = tokio::time::timeout(Duration::from_secs(2), transport.next_event())
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        event.event,
        InboundEvent::SafetyViolation { ref method, .. }
            if method == "item/commandExecution/requestApproval"
    ));
    assert_eq!(
        transport
            .request("ping", json!({}), Duration::from_millis(100))
            .await,
        Err(TransportError::SafetyViolation(
            "item/commandExecution/requestApproval".to_owned()
        ))
    );

    let mut denial: Option<serde_json::Value> = None;
    for _ in 0..100 {
        if let Ok(bytes) = fs::read(&capture) {
            if let Ok(value) = serde_json::from_slice(&bytes) {
                denial = Some(value);
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let denial = denial.expect("fake app-server did not capture the approval denial");
    assert_eq!(denial["id"], "approval-1");
    assert_eq!(denial["result"]["decision"], "cancel");
    assert!(!diagnostics
        .events()
        .iter()
        .any(|event| event.category == "event_backlog"));

    transport.shutdown().await.unwrap();
}
