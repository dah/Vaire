use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::time::Duration;

use serde_json::json;
use tempfile::tempdir;
use vaire::codex::protocol::{
    CancelLoginAccountParams, CancelLoginAccountResponse, CancelLoginAccountStatus,
    InitializeParams, LoginAccountParams, LoginAccountResponse,
};
use vaire::codex::safety::{FullAccessPolicy, IsolationPaths};
use vaire::codex::transport::{AppServerTransport, ProcessSpec};
use vaire::platform::validate_login_url;

#[tokio::test]
#[ignore = "requires an installed Claude Code CLI; checks version and auth help only, never login state"]
async fn installed_claude_cli_exposes_supported_subscription_auth_commands() {
    let executable = std::env::var_os("VAIRE_CLAUDE_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("claude"));
    let temp = tempdir().unwrap();
    let config_dir = temp.path().join("claude-home");
    std::fs::create_dir(&config_dir).unwrap();
    std::fs::set_permissions(&config_dir, std::fs::Permissions::from_mode(0o700)).unwrap();

    vaire::claude::verify_claude_version(&executable, &config_dir, Duration::from_secs(10))
        .await
        .unwrap();

    for (args, expected) in [
        (["auth", "login", "--help"], "--claudeai"),
        (["auth", "logout", "--help"], "Log out"),
    ] {
        let mut command = tokio::process::Command::new(&executable);
        for (name, _) in std::env::vars_os() {
            let name_text = name.to_string_lossy();
            if name_text.starts_with("ANTHROPIC_") || name_text.starts_with("CLAUDE") {
                command.env_remove(name);
            }
        }
        let output = tokio::time::timeout(
            Duration::from_secs(10),
            command
                .args(args)
                .current_dir(temp.path())
                .env("CLAUDE_CONFIG_DIR", &config_dir)
                .env("NO_COLOR", "1")
                .output(),
        )
        .await
        .expect("Claude help command timed out")
        .expect("Claude help command could not start");
        assert!(output.status.success());
        let help = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(help.contains(expected));
    }
}

#[tokio::test]
#[ignore = "requires an installed Codex CLI; run explicitly during protocol upgrades"]
async fn installed_cli_initializes_with_full_access_policy() {
    let executable = std::env::var_os("VAIRE_CODEX_BIN")
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
            InitializeParams::vaire(),
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
    let executable = std::env::var_os("VAIRE_CODEX_BIN")
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
            InitializeParams::vaire(),
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
