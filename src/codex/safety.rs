use std::ffi::OsString;
use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use super::protocol::RequestId;

pub const POLICY_ID: &str = "conversation-safety/codex-0.144.6-v1";

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

const DISABLED_FEATURES: [&str; 22] = [
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
    "shell_snapshot",
    "shell_tool",
    "skill_mcp_dependency_install",
    "tool_call_mcp_elicitation",
    "unified_exec",
    "workspace_dependencies",
];

const CONFIG_OVERRIDES: [&str; 9] = [
    "approval_policy=\"never\"",
    "sandbox_mode=\"read-only\"",
    "web_search=\"disabled\"",
    "mcp_servers={}",
    "apps._default.enabled=false",
    "history.persistence=\"none\"",
    "forced_login_method=\"chatgpt\"",
    "allow_login_shell=false",
    "shell_environment_policy.inherit=\"none\"",
];

#[derive(Clone, Debug)]
pub struct IsolationPaths {
    pub root: PathBuf,
    pub codex_home: PathBuf,
    pub conversation: PathBuf,
}

impl IsolationPaths {
    /// Creates owner-only runtime directories and requires a fresh, empty conversation cwd.
    pub fn prepare(root: impl AsRef<Path>) -> io::Result<Self> {
        let root = root.as_ref().to_path_buf();
        let codex_home = root.join("codex-home");
        let conversation = root.join("conversation");

        create_owner_only(&root)?;
        create_owner_only(&codex_home)?;
        create_owner_only(&conversation)?;

        if fs::read_dir(&conversation)?.next().transpose()?.is_some() {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "conversation directory must be empty",
            ));
        }

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
pub struct ConversationSafetyPolicy;

impl ConversationSafetyPolicy {
    /// Direct argv for codex app-server; no shell interpolation is required.
    pub fn app_server_args(&self) -> Vec<OsString> {
        let mut args = vec![
            OsString::from("app-server"),
            OsString::from("--stdio"),
            OsString::from("--strict-config"),
        ];

        for feature in DISABLED_FEATURES {
            args.push(OsString::from("--disable"));
            args.push(OsString::from(feature));
        }
        for config in CONFIG_OVERRIDES {
            args.push(OsString::from("-c"));
            args.push(OsString::from(config));
        }
        args
    }

    pub fn thread_start_overrides(&self, cwd: &Path) -> Value {
        let feature_values = DISABLED_FEATURES
            .into_iter()
            .map(|feature| (feature.to_owned(), Value::Bool(false)))
            .collect::<serde_json::Map<_, _>>();

        json!({
            "approvalPolicy": "never",
            "cwd": cwd,
            "sandbox": "read-only",
            "config": {
                "approval_policy": "never",
                "sandbox_mode": "read-only",
                "web_search": "disabled",
                "mcp_servers": {},
                "apps": {"_default": {"enabled": false}},
                "features": feature_values,
                "history": {"persistence": "none"},
                "allow_login_shell": false,
                "shell_environment_policy": {"inherit": "none"}
            }
        })
    }

    pub fn turn_start_overrides(&self, cwd: &Path) -> Value {
        json!({
            "approvalPolicy": "never",
            "cwd": cwd,
            "sandboxPolicy": {
                "type": "readOnly",
                "networkAccess": false
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
                "message": "AgentHarness conversation policy denied the server request"
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

    use super::{
        denial_response, ConversationSafetyPolicy, IsolationPaths, KNOWN_SERVER_REQUEST_METHODS,
    };
    use crate::codex::protocol::RequestId;

    #[test]
    fn prepares_owner_only_empty_isolation() {
        let temp = tempdir().unwrap();
        let paths = IsolationPaths::prepare(temp.path().join("runtime")).unwrap();
        assert_eq!(
            std::fs::metadata(paths.codex_home)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert!(std::fs::read_dir(paths.conversation)
            .unwrap()
            .next()
            .is_none());
    }

    #[test]
    fn supplies_defense_in_depth_at_thread_and_turn_boundaries() {
        let policy = ConversationSafetyPolicy;
        let cwd = Path::new("/private/tmp/agentharness-conversation");
        let thread = policy.thread_start_overrides(cwd);
        let turn = policy.turn_start_overrides(cwd);

        assert_eq!(thread["approvalPolicy"], "never");
        assert_eq!(thread["sandbox"], "read-only");
        assert_eq!(thread["config"]["features"]["shell_tool"], false);
        assert_eq!(thread["config"]["web_search"], "disabled");
        assert_eq!(turn["approvalPolicy"], "never");
        assert_eq!(
            turn["sandboxPolicy"],
            json!({"type": "readOnly", "networkAccess": false})
        );
    }

    #[test]
    fn direct_args_use_strict_config_and_disable_known_tool_features() {
        let args = ConversationSafetyPolicy.app_server_args();
        assert!(args.iter().any(|arg| arg == OsStr::new("--strict-config")));
        assert!(args.iter().any(|arg| arg == OsStr::new("shell_tool")));
        assert!(args.iter().any(|arg| arg == OsStr::new("multi_agent")));
        assert!(args
            .iter()
            .any(|arg| arg == OsStr::new("approval_policy=\"never\"")));
    }

    #[test]
    fn every_generated_request_has_a_negative_or_error_response() {
        for method in KNOWN_SERVER_REQUEST_METHODS {
            let response = denial_response(&RequestId::String("server-1".to_owned()), method);
            assert!(response.get("result").is_some() || response.get("error").is_some());
            assert_ne!(response.pointer("/result/decision"), Some(&json!("accept")));
            assert_ne!(
                response.pointer("/result/decision"),
                Some(&json!("approved"))
            );
            assert_ne!(response.pointer("/result/action"), Some(&json!("accept")));
        }
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
