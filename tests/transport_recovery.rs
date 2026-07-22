use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use agentharness::codex::protocol::InboundEvent;
use agentharness::codex::transport::{
    AppServerTransport, ProcessSpec, RequestTimeouts, TransportError, EVENT_QUEUE_CAPACITY,
    MAX_FRAME_BYTES, MAX_PENDING_REQUESTS,
};
use agentharness::diagnostics::MemoryDiagnosticSink;
use futures_util::{stream::FuturesUnordered, StreamExt};
use serde_json::json;
use tempfile::tempdir;

fn script(root: &Path, body: &str) -> PathBuf {
    let path = root.join("fake-app-server");
    fs::write(&path, format!("#!/bin/sh\nset -eu\n{body}\n")).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
    path
}

fn spec(root: &Path, executable: PathBuf) -> ProcessSpec {
    ProcessSpec {
        executable,
        args: Vec::new(),
        cwd: root.to_owned(),
        env: Vec::new(),
    }
}

fn unrendered_tool_flood(count: usize) -> String {
    format!(
        r#"
for method in \
  command/exec/outputDelta \
  process/outputDelta \
  turn/diff/updated \
  item/commandExecution/outputDelta \
  item/commandExecution/terminalInteraction \
  item/fileChange/outputDelta \
  item/fileChange/patchUpdated
do
  i=0
  while [ "$i" -lt {count} ]; do
    printf '{{"method":"%s","params":{{"payload":"x"}}}}\n' "$method"
    i=$((i + 1))
  done
done
for lifecycle in item/started item/completed
do
  i=0
  while [ "$i" -lt {count} ]; do
    printf '{{"method":"%s","params":{{"threadId":"thr-tools","turnId":"turn-tools","item":{{"id":"command-%s","type":"commandExecution"}}}}}}\n' "$lifecycle" "$i"
    printf '{{"method":"%s","params":{{"threadId":"thr-tools","turnId":"turn-tools","item":{{"id":"file-%s","kind":"fileChange"}}}}}}\n' "$lifecycle" "$i"
    i=$((i + 1))
  done
done
"#
    )
}

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

#[tokio::test]
async fn malformed_json_is_reported_as_a_framing_failure() {
    let temp = tempdir().unwrap();
    let executable = script(temp.path(), "printf '%s\\n' '{broken'\nsleep 1");
    let mut transport = AppServerTransport::spawn(spec(temp.path(), executable))
        .await
        .unwrap();
    let event = tokio::time::timeout(Duration::from_secs(1), transport.next_event())
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        event.event,
        InboundEvent::ConnectionClosed { category } if category == "framing"
    ));
    transport.shutdown().await.unwrap();
}

#[tokio::test]
async fn codec_rejects_oversized_and_invalid_utf8_lines_without_poisoning_later_connections() {
    let cases = [
        (
            "oversized-line",
            format!(
                "dd if=/dev/zero bs={} count=1 2>/dev/null | tr '\\000' x\nprintf '\\n'\nIFS= read -r hold || true",
                MAX_FRAME_BYTES + 1
            ),
        ),
        (
            "invalid-utf8",
            "printf '\\377\\n'\nIFS= read -r hold || true".to_owned(),
        ),
    ];

    for (name, body) in cases {
        let temp = tempdir().unwrap();
        let executable = script(temp.path(), &body);
        let mut transport = AppServerTransport::spawn(spec(temp.path(), executable))
            .await
            .unwrap();

        let event = tokio::time::timeout(Duration::from_secs(2), transport.next_event())
            .await
            .unwrap_or_else(|_| panic!("{name} did not close the connection"))
            .expect("framing failure event was missing");
        assert!(
            matches!(event.event, InboundEvent::ConnectionClosed { category } if category == "framing"),
            "{name} was not classified as a codec framing failure"
        );
        assert!(
            tokio::time::timeout(Duration::from_secs(2), transport.next_event())
                .await
                .expect("connection worker did not finish")
                .is_none(),
            "{name} emitted data after its terminal framing event"
        );
        assert_eq!(
            transport
                .request("after-framing-failure", json!({}), Duration::from_secs(1))
                .await,
            Err(TransportError::Closed),
            "{name} left a partially usable transport behind"
        );
        transport.shutdown().await.unwrap();
    }
}

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
