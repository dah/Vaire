use std::path::PathBuf;
use std::time::Duration;

use agentharness::codex::protocol::{
    CancelLoginAccountParams, CancelLoginAccountResponse, CancelLoginAccountStatus,
    InitializeParams, LoginAccountParams, LoginAccountResponse,
};
use agentharness::codex::safety::{FullAccessPolicy, IsolationPaths};
use agentharness::codex::transport::{AppServerTransport, ProcessSpec};
use agentharness::platform::validate_login_url;
use serde_json::json;
use tempfile::tempdir;

#[tokio::test]
#[ignore = "requires an installed Codex CLI; run explicitly during protocol upgrades"]
async fn installed_cli_initializes_with_full_access_policy() {
    let executable = std::env::var_os("AGENTHARNESS_CODEX_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("codex"));
    let temp = tempdir().unwrap();
    let paths = IsolationPaths::prepare(temp.path().join("runtime")).unwrap();
    let policy = FullAccessPolicy;
    let spec = ProcessSpec::codex(executable, &paths, &policy);
    let mut transport = AppServerTransport::spawn(spec).await.unwrap();

    transport
        .request(
            "initialize",
            InitializeParams::agentharness(),
            Duration::from_secs(10),
        )
        .await
        .unwrap();
    transport.notify("initialized", json!({})).await.unwrap();

    let config = transport
        .request(
            "config/read",
            json!({
                "cwd": paths.conversation,
                "includeLayers": true
            }),
            Duration::from_secs(10),
        )
        .await
        .unwrap();
    assert_eq!(config["config"]["approval_policy"], "never");
    assert_eq!(config["config"]["sandbox_mode"], "danger-full-access");
    assert_eq!(config["config"]["show_raw_agent_reasoning"], true);
    assert_eq!(config["config"]["web_search"], "disabled");
    assert_eq!(
        config["config"]["shell_environment_policy"]["inherit"],
        "all"
    );

    transport.shutdown().await.unwrap();
}

#[tokio::test]
#[ignore = "requires an installed Codex CLI and network access; starts and immediately cancels a device login"]
async fn installed_cli_starts_and_cancels_device_login() {
    let executable = std::env::var_os("AGENTHARNESS_CODEX_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("codex"));
    let temp = tempdir().unwrap();
    let paths = IsolationPaths::prepare(temp.path().join("runtime")).unwrap();
    let policy = FullAccessPolicy;
    let spec = ProcessSpec::codex(executable, &paths, &policy);
    let mut transport = AppServerTransport::spawn(spec).await.unwrap();

    transport
        .request(
            "initialize",
            InitializeParams::agentharness(),
            Duration::from_secs(10),
        )
        .await
        .unwrap();
    transport.notify("initialized", json!({})).await.unwrap();

    let response: LoginAccountResponse = serde_json::from_value(
        transport
            .request(
                "account/login/start",
                LoginAccountParams::chatgpt_device_code(),
                Duration::from_secs(30),
            )
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(response.kind, "chatgptDeviceCode");
    assert!(response
        .user_code
        .as_deref()
        .is_some_and(|code| !code.is_empty()));
    let verification_url = response.verification_url.as_deref().unwrap();
    validate_login_url(verification_url).unwrap();

    let login_id = response.login_id.unwrap();
    let cancel: CancelLoginAccountResponse = serde_json::from_value(
        transport
            .request(
                "account/login/cancel",
                CancelLoginAccountParams::new(login_id),
                Duration::from_secs(10),
            )
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(cancel.status, CancelLoginAccountStatus::Canceled);

    transport.shutdown().await.unwrap();
}
