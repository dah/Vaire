use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use agentharness::codex::safety::{FullAccessPolicy, IsolationPaths};
use agentharness::codex::session::SessionService;
use agentharness::codex::transport::{AppServerTransport, ProcessSpec};
use serde_json::Value;
use tempfile::tempdir;

struct ScopedEnv {
    key: &'static str,
    previous: Option<OsString>,
}

impl ScopedEnv {
    fn set(key: &'static str, value: &str) -> Self {
        let previous = std::env::var_os(key);
        std::env::set_var(key, value);
        Self { key, previous }
    }
}

impl Drop for ScopedEnv {
    fn drop(&mut self) {
        if let Some(previous) = &self.previous {
            std::env::set_var(self.key, previous);
        } else {
            std::env::remove_var(self.key);
        }
    }
}

fn script(root: &Path, name: &str, body: &str) -> PathBuf {
    let path = root.join(name);
    fs::write(&path, format!("#!/bin/sh\nset -eu\n{body}\n")).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
    path
}

async fn wait_for(path: &Path) {
    for _ in 0..100 {
        if path.exists() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("timed out waiting for {}", path.display());
}

#[tokio::test]
async fn codex_process_receives_path_and_dedicated_runtime_directories() {
    const INHERITED_KEY: &str = "AGENTHARNESS_ENV_INHERIT_TEST";
    const SCRUBBED_KEY: &str = "CODEX_AGENTHARNESS_ENV_SCRUB_TEST";
    let inherited = ScopedEnv::set(INHERITED_KEY, "ambient-value");
    let scrubbed = ScopedEnv::set(SCRUBBED_KEY, "must-not-reach-child");
    let temp = tempdir().unwrap();
    let captures = temp.path().join("captures");
    fs::create_dir(&captures).unwrap();
    let executable = script(
        temp.path(),
        "capture-environment",
        r#"
printf '%s' "$PATH" > "$CAPTURE_DIR/path"
printf '%s' "$CODEX_HOME" > "$CAPTURE_DIR/codex-home"
printf '%s' "$PWD" > "$CAPTURE_DIR/cwd"
printf '%s' "$AGENTHARNESS_ENV_INHERIT_TEST" > "$CAPTURE_DIR/inherited"
printf '%s' "${CODEX_AGENTHARNESS_ENV_SCRUB_TEST-unset}" > "$CAPTURE_DIR/scrubbed"
IFS= read -r hold || true
"#,
    );
    let paths = IsolationPaths::prepare(temp.path().join("runtime")).unwrap();
    let mut spec = ProcessSpec::codex(executable, &paths, &FullAccessPolicy);
    spec.env.push((
        OsString::from("CAPTURE_DIR"),
        captures.as_os_str().to_owned(),
    ));

    let mut transport = AppServerTransport::spawn(spec).await.unwrap();
    drop(inherited);
    drop(scrubbed);
    wait_for(&captures.join("cwd")).await;

    assert_eq!(
        fs::read_to_string(captures.join("path")).unwrap(),
        std::env::var("PATH").unwrap()
    );
    assert_eq!(
        fs::read_to_string(captures.join("codex-home")).unwrap(),
        paths.codex_home.to_string_lossy()
    );
    assert_eq!(
        fs::read_to_string(captures.join("cwd")).unwrap(),
        fs::canonicalize(&paths.conversation)
            .unwrap()
            .to_string_lossy()
    );
    assert_eq!(
        fs::read_to_string(captures.join("inherited")).unwrap(),
        "ambient-value"
    );
    assert_eq!(
        fs::read_to_string(captures.join("scrubbed")).unwrap(),
        "unset"
    );

    transport.shutdown().await.unwrap();
}

fn assert_thread_policy(request: &Value, method: &str, cwd: &Path) {
    assert_eq!(request["method"], method);
    assert_eq!(request["params"]["approvalPolicy"], "never");
    assert_eq!(request["params"]["cwd"], cwd.to_string_lossy().as_ref());
    assert_eq!(request["params"]["sandbox"], "danger-full-access");
    assert_eq!(request["params"]["config"]["approval_policy"], "never");
    assert_eq!(
        request["params"]["config"]["sandbox_mode"],
        "danger-full-access"
    );
    assert_eq!(
        request["params"]["config"]["shell_environment_policy"]["inherit"],
        "all"
    );
    assert_eq!(
        request["params"]["config"]["show_raw_agent_reasoning"],
        true
    );
    assert_eq!(
        request["params"]["config"]["features"]["shell_snapshot"],
        true
    );
    assert_eq!(request["params"]["config"]["features"]["shell_tool"], true);
    assert_eq!(
        request["params"]["config"]["features"]["unified_exec"],
        true
    );
    assert_eq!(
        request["params"]["config"]["features"]["multi_agent"],
        false
    );
}

#[tokio::test]
async fn fake_server_receives_full_access_on_thread_start_resume_and_turn_start() {
    let temp = tempdir().unwrap();
    let captures = temp.path().join("captures");
    fs::create_dir(&captures).unwrap();
    let executable = script(
        temp.path(),
        "fake-app-server",
        r#"
IFS= read -r initialize
printf '%s\n' '{"id":1,"result":{"codexHome":"/private/tmp/codex","platformFamily":"unix","platformOs":"macos","userAgent":"fake/0.144.6"}}'
IFS= read -r initialized
IFS= read -r thread_start
printf '%s\n' "$thread_start" > "$CAPTURE_DIR/thread-start.json"
printf '%s\n' '{"id":2,"result":{"thread":{"id":"thr-policy","turns":[]}}}'
IFS= read -r turn_start
printf '%s\n' "$turn_start" > "$CAPTURE_DIR/turn-start.json"
printf '%s\n' '{"id":3,"result":{"turn":{"id":"turn-policy","items":[],"status":"inProgress"}}}'
IFS= read -r thread_resume
printf '%s\n' "$thread_resume" > "$CAPTURE_DIR/thread-resume.json"
printf '%s\n' '{"id":4,"result":{"thread":{"id":"thr-policy","turns":[]}}}'
IFS= read -r thread_read
printf '%s\n' '{"id":5,"result":{"thread":{"id":"thr-policy","turns":[]}}}'
IFS= read -r hold || true
"#,
    );
    let paths = IsolationPaths::prepare(temp.path().join("runtime")).unwrap();
    let transport = AppServerTransport::spawn(ProcessSpec {
        executable,
        args: Vec::new(),
        cwd: temp.path().to_owned(),
        env: vec![(
            OsString::from("CAPTURE_DIR"),
            captures.as_os_str().to_owned(),
        )],
    })
    .await
    .unwrap();
    let mut session = SessionService::new(transport, paths.clone(), FullAccessPolicy);

    session.initialize().await.unwrap();
    session.start_thread("m1").await.unwrap();
    session
        .start_turn("thr-policy", "use the command line", "m1", "high")
        .await
        .unwrap();
    session.resume_thread("thr-policy", "m1").await.unwrap();

    let thread_start: Value =
        serde_json::from_slice(&fs::read(captures.join("thread-start.json")).unwrap()).unwrap();
    let turn_start: Value =
        serde_json::from_slice(&fs::read(captures.join("turn-start.json")).unwrap()).unwrap();
    let thread_resume: Value =
        serde_json::from_slice(&fs::read(captures.join("thread-resume.json")).unwrap()).unwrap();

    assert_thread_policy(&thread_start, "thread/start", &paths.conversation);
    assert_eq!(thread_start["params"]["threadSource"], "appServer");
    assert_thread_policy(&thread_resume, "thread/resume", &paths.conversation);
    assert_eq!(thread_resume["params"]["threadId"], "thr-policy");
    assert!(thread_resume["params"].get("threadSource").is_none());
    assert_eq!(turn_start["method"], "turn/start");
    assert_eq!(turn_start["params"]["approvalPolicy"], "never");
    assert_eq!(turn_start["params"]["summary"], "detailed");
    assert_eq!(
        turn_start["params"]["cwd"],
        paths.conversation.to_string_lossy().as_ref()
    );
    assert_eq!(
        turn_start["params"]["sandboxPolicy"],
        serde_json::json!({"type": "dangerFullAccess"})
    );

    session.shutdown().await.unwrap();
}
