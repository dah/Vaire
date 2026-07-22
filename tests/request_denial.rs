use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::Duration;

use agentharness::codex::protocol::{InboundEvent, InitializeParams};
use agentharness::codex::safety::KNOWN_SERVER_REQUEST_METHODS;
use agentharness::codex::transport::{AppServerTransport, ProcessSpec, TransportError};
use serde_json::{json, Value};
use tempfile::tempdir;

const FAKE_SERVER: &str = r#"#!/bin/sh
IFS= read -r request
if [ "$FAKE_REQUEST_FIRST" = "1" ]; then
  printf '{"id":"server-1","method":"%s","params":{}}\n' "$FAKE_METHOD"
  IFS= read -r denial
  printf '%s\n' "$denial" > "$FAKE_CAPTURE"
  IFS= read -r hold
  exit 0
fi
printf '%s\n' '{"id":1,"result":{"initialized":true}}'
printf '{"id":"server-1","method":"%s","params":{}}\n' "$FAKE_METHOD"
IFS= read -r denial
printf '%s\n' "$denial" > "$FAKE_CAPTURE"
IFS= read -r hold
"#;

fn fake_spec(root: &Path, method: &str, capture: &Path, request_first: bool) -> ProcessSpec {
    let executable = root.join("fake-app-server");
    fs::write(&executable, FAKE_SERVER).unwrap();
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();

    ProcessSpec {
        executable,
        args: Vec::new(),
        cwd: root.to_path_buf(),
        env: vec![
            (
                OsString::from("FAKE_METHOD"),
                OsString::from(method.to_owned()),
            ),
            (
                OsString::from("FAKE_CAPTURE"),
                capture.as_os_str().to_owned(),
            ),
            (
                OsString::from("FAKE_REQUEST_FIRST"),
                OsString::from(if request_first { "1" } else { "0" }),
            ),
        ],
    }
}

async fn captured_response(method: &str) -> Value {
    let temp = tempdir().unwrap();
    let capture = temp.path().join("denial.json");
    let spec = fake_spec(temp.path(), method, &capture, false);
    let mut transport = AppServerTransport::spawn(spec).await.unwrap();

    let initialized = transport
        .request(
            "initialize",
            InitializeParams::agentharness(),
            Duration::from_secs(1),
        )
        .await
        .unwrap();
    assert_eq!(initialized["initialized"], true);

    let event = tokio::time::timeout(Duration::from_secs(1), transport.next_event())
        .await
        .unwrap()
        .unwrap();
    match event.event {
        InboundEvent::SafetyViolation {
            method: actual_method,
            ..
        } => assert_eq!(actual_method, method),
        other => panic!("expected safety violation, got {other:?}"),
    }

    let error = transport
        .request("account/read", json!({}), Duration::from_secs(1))
        .await
        .unwrap_err();
    assert_eq!(
        error,
        TransportError::SafetyViolation(method.to_owned()),
        "connection must be unusable after a server request"
    );

    let mut response = None;
    for _ in 0..100 {
        if let Ok(bytes) = fs::read(&capture) {
            if let Ok(value) = serde_json::from_slice(&bytes) {
                response = Some(value);
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let response = response.expect("fake app-server did not capture a complete denial response");
    transport.shutdown().await.unwrap();
    response
}

#[tokio::test]
async fn full_access_still_denies_every_unimplemented_server_request() {
    for method in KNOWN_SERVER_REQUEST_METHODS {
        let response = captured_response(method).await;
        assert_eq!(response["id"], "server-1");
        assert_ne!(response.pointer("/result/decision"), Some(&json!("accept")));
        assert_ne!(
            response.pointer("/result/decision"),
            Some(&json!("approved"))
        );
        assert_ne!(response.pointer("/result/action"), Some(&json!("accept")));

        match method {
            "item/commandExecution/requestApproval" | "item/fileChange/requestApproval" => {
                assert_eq!(response["result"]["decision"], "cancel");
            }
            "mcpServer/elicitation/request" => {
                assert_eq!(response["result"]["action"], "cancel");
            }
            "applyPatchApproval" | "execCommandApproval" => {
                assert_eq!(response["result"]["decision"], "abort");
            }
            _ => assert_eq!(response["error"]["code"], -32080),
        }
    }
}

#[tokio::test]
async fn denies_unknown_server_requests_and_closes_the_safety_boundary() {
    let response = captured_response("future/tool/requestApproval").await;
    assert_eq!(response["error"]["code"], -32601);
    assert!(response["error"]["message"]
        .as_str()
        .unwrap()
        .contains("does not support"));
}

#[tokio::test]
async fn server_request_fails_in_flight_work_before_responding() {
    let temp = tempdir().unwrap();
    let capture = temp.path().join("denial.json");
    let method = "item/tool/call";
    let spec = fake_spec(temp.path(), method, &capture, true);
    let mut transport = AppServerTransport::spawn(spec).await.unwrap();

    let error = transport
        .request(
            "initialize",
            InitializeParams::agentharness(),
            Duration::from_secs(1),
        )
        .await
        .unwrap_err();
    assert_eq!(error, TransportError::SafetyViolation(method.to_owned()));

    assert!(matches!(
        transport.next_event().await,
        Some(agentharness::codex::transport::TransportEvent {
            event: InboundEvent::SafetyViolation { method: actual_method, .. },
            ..
        }) if actual_method == method
    ));

    transport.shutdown().await.unwrap();
}
