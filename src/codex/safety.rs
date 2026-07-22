use std::ffi::OsString;
use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use super::protocol::RequestId;

pub const POLICY_ID: &str = "full-access-tools/codex-0.144.6-v1";

pub const KNOWN_SERVER_REQUEST_METHODS: [&str; 10] = [
    "item/commandExecution/requestApproval",
    "item/fileChange/requestApproval",
    "item/tool/requestUserInput",
    "mcpServer/elicitation/request",
    "item/permissions/requestApproval",
    "item/tool/call",
    "account/chatgptAuthTokens/refresh",
    "attestation/generate",
    "applyPatchApproval",
    "execCommandApproval",
];

const DISABLED_OPTIONAL_FEATURES: [&str; 19] = [
    "apps",
    "auth_elicitation",
    "browser_use",
    "browser_use_external",
    "browser_use_full_cdp_access",
    "code_mode_host",
    "computer_use",
    "goals",
    "guardian_approval",
    "hooks",
    "image_generation",
    "in_app_browser",
    "multi_agent",
    "plugin_sharing",
    "plugins",
    "remote_plugin",
    "skill_mcp_dependency_install",
    "tool_call_mcp_elicitation",
    "workspace_dependencies",
];

const ENABLED_COMMAND_FEATURES: [&str; 3] = ["shell_snapshot", "shell_tool", "unified_exec"];

const CONFIG_OVERRIDES: [&str; 9] = [
    "approval_policy=\"never\"",
    "sandbox_mode=\"danger-full-access\"",
    "web_search=\"disabled\"",
    "mcp_servers={}",
    "apps._default.enabled=false",
    "history.persistence=\"none\"",
    "show_raw_agent_reasoning=true",
    "forced_login_method=\"chatgpt\"",
    "shell_environment_policy.inherit=\"all\"",
];

#[derive(Clone, Debug)]
pub struct IsolationPaths {
    pub root: PathBuf,
    pub codex_home: PathBuf,
    pub conversation: PathBuf,
}

impl IsolationPaths {
    /// Creates owner-only runtime directories while preserving the conversation workspace.
    ///
    /// Built-in tools run from this dedicated non-project directory. Files created there are
    /// intentionally retained so they remain available after AgentHarness restarts.
    pub fn prepare(root: impl AsRef<Path>) -> io::Result<Self> {
        let root = root.as_ref().to_path_buf();
        let codex_home = root.join("codex-home");
        let conversation = root.join("conversation");

        create_owner_only(&root)?;
        create_owner_only(&codex_home)?;
        create_owner_only(&conversation)?;

        Ok(Self {
            root,
            codex_home,
            conversation,
        })
    }
}

fn create_owner_only(path: &Path) -> io::Result<()> {
    fs::create_dir_all(path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
}

#[derive(Clone, Debug, Default)]
pub struct FullAccessPolicy;

impl FullAccessPolicy {
    /// Direct argv for codex app-server; no shell interpolation is required.
    pub fn app_server_args(&self) -> Vec<OsString> {
        let mut args = vec![
            OsString::from("app-server"),
            OsString::from("--stdio"),
            OsString::from("--strict-config"),
        ];

        for feature in DISABLED_OPTIONAL_FEATURES {
            args.push(OsString::from("--disable"));
            args.push(OsString::from(feature));
        }
        for feature in ENABLED_COMMAND_FEATURES {
            args.push(OsString::from("--enable"));
            args.push(OsString::from(feature));
        }
        for config in CONFIG_OVERRIDES {
            args.push(OsString::from("-c"));
            args.push(OsString::from(config));
        }
        args
    }

    pub fn thread_start_overrides(&self, cwd: &Path) -> Value {
        let mut feature_values = DISABLED_OPTIONAL_FEATURES
            .into_iter()
            .map(|feature| (feature.to_owned(), Value::Bool(false)))
            .collect::<serde_json::Map<_, _>>();
        feature_values.extend(
            ENABLED_COMMAND_FEATURES
                .into_iter()
                .map(|feature| (feature.to_owned(), Value::Bool(true))),
        );

        json!({
            "approvalPolicy": "never",
            "cwd": cwd,
            "sandbox": "danger-full-access",
            "config": {
                "approval_policy": "never",
                "sandbox_mode": "danger-full-access",
                "web_search": "disabled",
                "mcp_servers": {},
                "apps": {"_default": {"enabled": false}},
                "features": feature_values,
                "history": {"persistence": "none"},
                "show_raw_agent_reasoning": true,
                "shell_environment_policy": {"inherit": "all"}
            }
        })
    }

    pub fn turn_start_overrides(&self, cwd: &Path) -> Value {
        json!({
            "approvalPolicy": "never",
            "cwd": cwd,
            "sandboxPolicy": {
                "type": "dangerFullAccess"
            }
        })
    }
}

pub fn denial_response(id: &RequestId, method: &str) -> Value {
    let id = serde_json::to_value(id).expect("request id is serializable");
    match method {
        "item/commandExecution/requestApproval" | "item/fileChange/requestApproval" => {
            json!({"id": id, "result": {"decision": "cancel"}})
        }
        "mcpServer/elicitation/request" => {
            json!({"id": id, "result": {"action": "cancel"}})
        }
        "applyPatchApproval" | "execCommandApproval" => {
            json!({"id": id, "result": {"decision": "abort"}})
        }
        method if KNOWN_SERVER_REQUEST_METHODS.contains(&method) => json!({
            "id": id,
            "error": {
                "code": -32080,
                "message": "AgentHarness runtime policy denied the server request"
            }
        }),
        _ => json!({
            "id": id,
            "error": {
                "code": -32601,
                "message": "AgentHarness does not support server-initiated requests"
            }
        }),
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;

    use serde_json::json;
    use tempfile::tempdir;

    use super::{denial_response, FullAccessPolicy, IsolationPaths};
    use crate::codex::protocol::RequestId;

    #[test]
    fn prepares_owner_only_persistent_isolation() {
        let temp = tempdir().unwrap();
        let root = temp.path().join("runtime");
        let paths = IsolationPaths::prepare(&root).unwrap();
        assert_eq!(
            std::fs::metadata(paths.codex_home)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert!(std::fs::read_dir(&paths.conversation)
            .unwrap()
            .next()
            .is_none());

        let artifact = paths.conversation.join("created-by-agent.txt");
        std::fs::write(&artifact, "keep me").unwrap();
        let relaunched = IsolationPaths::prepare(&root).unwrap();
        assert_eq!(
            std::fs::read_to_string(relaunched.conversation.join("created-by-agent.txt")).unwrap(),
            "keep me"
        );
    }

    #[test]
    fn supplies_full_access_tools_at_thread_and_turn_boundaries() {
        let policy = FullAccessPolicy;
        let cwd = Path::new("/private/tmp/agentharness-conversation");
        let thread = policy.thread_start_overrides(cwd);
        let turn = policy.turn_start_overrides(cwd);

        assert_eq!(thread["approvalPolicy"], "never");
        assert_eq!(thread["sandbox"], "danger-full-access");
        assert_eq!(thread["config"]["sandbox_mode"], "danger-full-access");
        assert_eq!(thread["config"]["features"]["shell_snapshot"], true);
        assert_eq!(thread["config"]["features"]["shell_tool"], true);
        assert_eq!(thread["config"]["features"]["unified_exec"], true);
        assert_eq!(thread["config"]["features"]["multi_agent"], false);
        assert_eq!(thread["config"]["web_search"], "disabled");
        assert_eq!(thread["config"]["show_raw_agent_reasoning"], true);
        assert_eq!(
            thread["config"]["shell_environment_policy"]["inherit"],
            "all"
        );
        assert!(thread["config"].get("allow_login_shell").is_none());
        assert_eq!(turn["approvalPolicy"], "never");
        assert_eq!(turn["sandboxPolicy"], json!({"type": "dangerFullAccess"}));
    }

    #[test]
    fn direct_args_enable_command_tools_and_full_access_without_approvals() {
        let args = FullAccessPolicy.app_server_args();
        assert!(args.iter().any(|arg| arg == OsStr::new("--strict-config")));
        for feature in ["shell_snapshot", "shell_tool", "unified_exec"] {
            assert!(args
                .windows(2)
                .any(|pair| pair[0] == OsStr::new("--enable") && pair[1] == OsStr::new(feature)));
            assert!(!args
                .windows(2)
                .any(|pair| pair[0] == OsStr::new("--disable") && pair[1] == OsStr::new(feature)));
        }
        assert!(
            args.windows(2)
                .any(|pair| pair[0] == OsStr::new("--disable")
                    && pair[1] == OsStr::new("multi_agent"))
        );
        assert!(args
            .iter()
            .any(|arg| arg == OsStr::new("approval_policy=\"never\"")));
        assert!(args
            .iter()
            .any(|arg| arg == OsStr::new("sandbox_mode=\"danger-full-access\"")));
        assert!(args
            .iter()
            .any(|arg| arg == OsStr::new("show_raw_agent_reasoning=true")));
        assert!(args
            .iter()
            .any(|arg| arg == OsStr::new("shell_environment_policy.inherit=\"all\"")));
        assert!(!args
            .iter()
            .any(|arg| arg == OsStr::new("allow_login_shell=false")));
    }

    #[test]
    fn denial_preserves_numeric_request_ids() {
        let response = denial_response(
            &RequestId::Number(42),
            "item/commandExecution/requestApproval",
        );
        assert_eq!(response["id"], 42);
        assert_eq!(response["result"]["decision"], "cancel");
    }
}
