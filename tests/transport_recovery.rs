use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use agentharness::codex::protocol::InboundEvent;
use agentharness::codex::transport::{
    AppServerTransport, ProcessSpec, RequestTimeouts, TransportError, EVENT_QUEUE_CAPACITY,
};
use agentharness::diagnostics::MemoryDiagnosticSink;
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

#[tokio::test]
async fn late_timed_out_response_is_ignored_and_connection_remains_usable() {
    let temp = tempdir().unwrap();
    let executable = script(
        temp.path(),
        r#"
IFS= read -r first
sleep 0.08
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
async fn malformed_json_and_eof_are_visible_recoverable_events() {
    for (body, category) in [
        ("printf '%s\\n' '{broken'\nsleep 1", "framing"),
        ("exit 0", "eof"),
    ] {
        let temp = tempdir().unwrap();
        let executable = script(temp.path(), body);
        let mut transport = AppServerTransport::spawn(spec(temp.path(), executable))
            .await
            .unwrap();
        let event = tokio::time::timeout(Duration::from_secs(1), transport.next_event())
            .await
            .unwrap()
            .unwrap();
        assert!(
            matches!(event.event, InboundEvent::ConnectionClosed { category: ref actual } if actual == category)
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
    let executable = script(
        temp.path(),
        &format!(
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
  while [ "$i" -lt {} ]; do
    printf '{{"method":"%s","params":{{"payload":"x"}}}}\n' "$method"
    i=$((i + 1))
  done
done
printf '%s\n' '{{"method":"item/agentMessage/delta","params":{{"threadId":"thr-tools","turnId":"turn-tools","itemId":"item-agent","delta":"done"}}}}'
printf '%s\n' '{{"method":"item/reasoning/summaryTextDelta","params":{{"threadId":"thr-tools","turnId":"turn-tools","itemId":"item-reasoning","summaryIndex":0,"delta":"summary"}}}}'
printf '%s\n' '{{"method":"thread/tokenUsage/updated","params":{{"threadId":"thr-tools","turnId":"turn-tools","tokenUsage":{{"last":{{"cachedInputTokens":0,"inputTokens":1,"outputTokens":1,"reasoningOutputTokens":0,"totalTokens":2}},"total":{{"cachedInputTokens":0,"inputTokens":1,"outputTokens":1,"reasoningOutputTokens":0,"totalTokens":2}},"modelContextWindow":100}}}}}}'
printf '%s\n' '{{"method":"turn/completed","params":{{"threadId":"thr-tools","turn":{{"id":"turn-tools","items":[],"status":"completed"}}}}}}'
printf '%s\n' '{{"method":"error","params":{{"threadId":"other-thread","turnId":"other-turn","willRetry":false,"error":{{"message":"unrelated","additionalDetails":null}}}}}}'
IFS= read -r request
printf '%s\n' '{{"id":1,"result":{{"ok":true}}}}'
IFS= read -r hold
"#,
            EVENT_QUEUE_CAPACITY + 32
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
