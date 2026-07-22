use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use agentharness::codex::safety::{FullAccessPolicy, IsolationPaths};
use agentharness::codex::session::{SessionError, SessionService};
use agentharness::codex::transport::{AppServerTransport, ProcessSpec};
use serde_json::json;
use tempfile::tempdir;

fn script(root: &Path, body: &str) -> PathBuf {
    let path = root.join("fake-app-server");
    fs::write(&path, format!("#!/bin/sh\nset -eu\n{body}\n")).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
    path
}

async fn session(root: &Path, body: &str) -> SessionService {
    let paths = IsolationPaths::prepare(root.join("runtime")).unwrap();
    let transport = AppServerTransport::spawn(ProcessSpec {
        executable: script(root, body),
        args: Vec::new(),
        cwd: root.to_owned(),
        env: Vec::new(),
    })
    .await
    .unwrap();
    SessionService::new(transport, paths, FullAccessPolicy)
}

#[tokio::test]
async fn malformed_thread_and_turn_start_snapshots_are_rejected_without_poisoning_transport() {
    let temp = tempdir().unwrap();
    let mut session = session(
        temp.path(),
        r#"
IFS= read -r missing_thread_turns
printf '%s\n' '{"id":1,"result":{"thread":{"id":"thr-missing"}}}'
IFS= read -r empty_thread_id
printf '%s\n' '{"id":2,"result":{"thread":{"id":"","turns":[]}}}'
IFS= read -r missing_turn_items
printf '%s\n' '{"id":3,"result":{"turn":{"id":"turn-missing","status":"inProgress"}}}'
IFS= read -r empty_turn_id
printf '%s\n' '{"id":4,"result":{"turn":{"id":"","items":[],"status":"inProgress"}}}'
IFS= read -r incomplete_agent_item
printf '%s\n' '{"id":5,"result":{"turn":{"id":"turn-agent","items":[{"id":"agent","type":"agentMessage"}],"status":"inProgress"}}}'
IFS= read -r terminal_turn
printf '%s\n' '{"id":6,"result":{"turn":{"id":"turn-terminal","items":[],"status":"completed"}}}'
IFS= read -r hold
"#,
    )
    .await;

    assert!(matches!(
        session.start_thread("m1").await,
        Err(SessionError::Decode {
            method: "thread/start"
        })
    ));
    assert!(matches!(
        session.start_thread("m1").await,
        Err(SessionError::Protocol(_))
    ));
    assert!(matches!(
        session.start_turn("thr", "hello", "m1", "high").await,
        Err(SessionError::Decode {
            method: "turn/start"
        })
    ));
    assert!(matches!(
        session.start_turn("thr", "hello", "m1", "high").await,
        Err(SessionError::Protocol(_))
    ));
    assert!(matches!(
        session.start_turn("thr", "hello", "m1", "high").await,
        Err(SessionError::Protocol(_))
    ));
    assert!(matches!(
        session.start_turn("thr", "hello", "m1", "high").await,
        Err(SessionError::Protocol(message)) if message.contains("turn status")
    ));

    session.shutdown().await.unwrap();
}

#[tokio::test]
async fn model_cursor_cycles_are_rejected() {
    let temp = tempdir().unwrap();
    let mut session = session(
        temp.path(),
        r#"
IFS= read -r cycle_page_one
printf '%s\n' '{"id":1,"result":{"data":[],"nextCursor":"repeat"}}'
IFS= read -r cycle_page_two
printf '%s\n' '{"id":2,"result":{"data":[],"nextCursor":"repeat"}}'
IFS= read -r empty_cursor
printf '%s\n' '{"id":3,"result":{"data":[],"nextCursor":""}}'
IFS= read -r hold
"#,
    )
    .await;

    assert!(matches!(
        session.list_models().await,
        Err(SessionError::Protocol(message)) if message.contains("cursor cycle")
    ));
    assert!(matches!(
        session.list_models().await,
        Err(SessionError::Protocol(message)) if message.contains("empty pagination cursor")
    ));
    session.shutdown().await.unwrap();
}

#[tokio::test]
async fn invalid_login_challenge_fields_are_rejected() {
    let temp = tempdir().unwrap();
    let mut session = session(
        temp.path(),
        r#"
IFS= read -r browser_login
printf '%s\n' '{"id":1,"result":{"type":"chatgpt","loginId":" login-with-padding ","authUrl":"https://example.invalid/login"}}'
IFS= read -r device_login
printf '%s\n' '{"id":2,"result":{"type":"chatgptDeviceCode","loginId":"login","verificationUrl":"https://example.invalid/device","userCode":"  "}}'
IFS= read -r hold
"#,
    )
    .await;

    assert!(matches!(
        session.start_login().await,
        Err(SessionError::Protocol(message)) if message.contains("loginId")
    ));
    assert!(matches!(
        session.start_device_login().await,
        Err(SessionError::Protocol(message)) if message.contains("userCode")
    ));
    session.shutdown().await.unwrap();
}

#[tokio::test]
async fn command_responses_that_should_be_objects_reject_null_and_arrays() {
    let temp = tempdir().unwrap();
    let mut session = session(
        temp.path(),
        r#"
IFS= read -r logout
printf '%s\n' '{"id":1,"result":null}'
IFS= read -r interrupt
printf '%s\n' '{"id":2,"result":[]}'
IFS= read -r hold
"#,
    )
    .await;

    assert!(matches!(
        session.logout().await,
        Err(SessionError::Decode {
            method: "account/logout"
        })
    ));
    assert!(matches!(
        session.interrupt_turn("thr", "turn").await,
        Err(SessionError::Decode {
            method: "turn/interrupt"
        })
    ));
    session.shutdown().await.unwrap();
}

#[tokio::test]
async fn unique_cursor_stream_is_bounded() {
    let temp = tempdir().unwrap();
    let mut session = session(
        temp.path(),
        r#"
i=1
while [ "$i" -le 256 ]; do
  IFS= read -r request
  printf '{"id":%s,"result":{"data":[],"nextCursor":"page-%s"}}\n' "$i" "$i"
  i=$((i + 1))
done
IFS= read -r hold
"#,
    )
    .await;

    assert!(matches!(
        session.list_models().await,
        Err(SessionError::Protocol(message)) if message.contains("pagination limit")
    ));
    session.shutdown().await.unwrap();
}

#[tokio::test]
async fn thread_listing_rejects_a_source_outside_the_requested_resume_set() {
    let temp = tempdir().unwrap();
    let cwd = temp.path().join("runtime/conversation");
    let response = json!({
        "id": 1,
        "result": {
            "data": [{
                "id": "thr-cli",
                "name": "CLI thread",
                "preview": "must not be offered",
                "createdAt": 1,
                "updatedAt": 2,
                "cwd": cwd,
                "ephemeral": false,
                "source": "cli"
            }],
            "nextCursor": null
        }
    });
    let body = format!(
        "IFS= read -r list\nprintf '%s\\n' '{}'\nIFS= read -r hold",
        response
    );
    let mut session = session(temp.path(), &body).await;

    assert!(matches!(
        session.list_threads().await,
        Err(SessionError::Protocol(message)) if message.contains("unsupported source")
    ));
    session.shutdown().await.unwrap();
}
