use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::{Arc, Mutex};

use crate::storage::CommitStatus;
use std::time::Duration;

use tempfile::TempDir;

use crate::provider::{ClaudeSessionId, ClaudeTurnId};

use super::process::argv_strings;
use super::*;

fn session_id(value: &str) -> ClaudeSessionId {
    value.parse().unwrap()
}

fn turn_id(value: &str) -> ClaudeTurnId {
    value.parse().unwrap()
}

async fn wait_for_test_pid_gone(pid: i32) -> bool {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            // SAFETY: signal zero only probes the exact descendant PID written by a fake CLI.
            if unsafe { libc::kill(pid, 0) } == -1
                && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .is_ok()
}

fn kill_test_pid(pid: i32) {
    // SAFETY: test-only fallback targets the exact PID written by the fake CLI.
    let _ = unsafe { libc::kill(pid, libc::SIGKILL) };
}

fn policy(executable: impl Into<std::path::PathBuf>, root: &Path) -> ClaudeCliPolicy {
    ClaudeCliPolicy::new(executable.into(), root.join("home"), root.join("cwd"))
}

struct ScriptedCommitStore {
    session: Mutex<Option<ClaudeSessionV1>>,
    status: CommitStatus,
}

impl ScriptedCommitStore {
    fn new(session: Option<ClaudeSessionV1>, status: CommitStatus) -> Self {
        Self {
            session: Mutex::new(session),
            status,
        }
    }
}

impl ClaudeSessionStore for ScriptedCommitStore {
    fn list_sessions(&self) -> Result<Vec<ClaudeSessionSummary>, ClaudeStoreError> {
        Ok(Vec::new())
    }

    fn load_session(&self, id: &ClaudeSessionId) -> Result<ClaudeSessionV1, ClaudeStoreError> {
        self.load_session_for_update(id)
    }

    fn load_session_for_update(
        &self,
        id: &ClaudeSessionId,
    ) -> Result<ClaudeSessionV1, ClaudeStoreError> {
        self.session
            .lock()
            .map_err(|_| ClaudeStoreError::Corrupt)?
            .as_ref()
            .filter(|session| &session.session_id == id)
            .cloned()
            .ok_or(ClaudeStoreError::NotFound)
    }

    fn save_session(&self, session: &ClaudeSessionV1) -> Result<(), ClaudeStoreError> {
        *self.session.lock().map_err(|_| ClaudeStoreError::Corrupt)? = Some(session.clone());
        Ok(())
    }

    fn save_session_with_commit(
        &self,
        session: &ClaudeSessionV1,
    ) -> Result<ClaudeSessionCommit, ClaudeStoreError> {
        self.save_session(session)?;
        Ok(ClaudeSessionCommit {
            source: self.status,
            index: Some(self.status),
        })
    }

    fn delete_session(&self, id: &ClaudeSessionId) -> Result<(), ClaudeStoreError> {
        let mut session = self.session.lock().map_err(|_| ClaudeStoreError::Corrupt)?;
        if session
            .as_ref()
            .is_some_and(|session| &session.session_id == id)
        {
            *session = None;
            Ok(())
        } else {
            Err(ClaudeStoreError::NotFound)
        }
    }
}

#[test]
fn fixed_aliases_and_argv_keep_prompt_and_authentication_out_of_arguments() {
    let root = TempDir::new().unwrap();
    let policy = policy("/bin/false", root.path());
    let id = session_id("00000000-0000-4000-8000-000000000001");
    let user = argv_strings(
        &policy,
        &ClaudeInvocation::NewSession {
            session_id: id,
            model: ClaudeModelAlias::Fable,
            effort: Some(ClaudeEffort::XHigh),
        },
    );
    assert!(user.contains(&"--safe-mode".to_owned()));
    assert!(!user.contains(&"--bare".to_owned()));
    assert!(user.contains(&"--verbose".to_owned()));
    assert!(user.contains(&"--dangerously-skip-permissions".to_owned()));
    assert!(user.windows(2).any(|pair| pair == ["--model", "fable"]));
    assert_eq!(
        user.windows(2)
            .filter(|pair| pair == &["--effort", "xhigh"])
            .count(),
        1
    );
    assert!(!user.contains(&"x_high".to_owned()));
    assert_eq!(
        user.iter()
            .filter(|argument| argument.as_str() == "--strict-mcp-config")
            .count(),
        1
    );
    assert_eq!(
        user.windows(2)
            .filter(|pair| pair == &["--mcp-config", r#"{"mcpServers":{}}"#])
            .count(),
        1
    );
    assert!(user
        .windows(2)
        .any(|pair| { pair == ["--settings", r#"{"forceLoginMethod":"claudeai"}"#] }));
    let joined = user.join(" ");
    assert!(!joined.contains("user prompt"));
    assert_eq!(
        CLAUDE_MODEL_ALIASES.map(claude_model_selector),
        ["default", "fable", "opus", "sonnet", "haiku"]
    );
}

#[test]
fn effort_argv_is_exact_for_new_resume_and_provider_default() {
    let root = TempDir::new().unwrap();
    let policy = policy("/bin/false", root.path());
    let id = session_id("00000000-0000-4000-8000-000000000031");
    let cases = [
        (
            ClaudeInvocation::NewSession {
                session_id: id.clone(),
                model: ClaudeModelAlias::Sonnet,
                effort: Some(ClaudeEffort::XHigh),
            },
            Some("xhigh"),
            "--session-id",
        ),
        (
            ClaudeInvocation::ResumeSession {
                session_id: id.clone(),
                model: ClaudeModelAlias::Opus,
                effort: Some(ClaudeEffort::Max),
            },
            Some("max"),
            "--resume",
        ),
        (
            ClaudeInvocation::NewSession {
                session_id: id.clone(),
                model: ClaudeModelAlias::Default,
                effort: None,
            },
            None,
            "--session-id",
        ),
        (
            ClaudeInvocation::ResumeSession {
                session_id: id,
                model: ClaudeModelAlias::Haiku,
                effort: None,
            },
            None,
            "--resume",
        ),
    ];

    for (invocation, expected_effort, session_flag) in cases {
        let args = argv_strings(&policy, &invocation);
        assert_eq!(
            args.iter()
                .filter(|argument| argument.as_str() == session_flag)
                .count(),
            1
        );
        assert_eq!(
            args.iter()
                .filter(|argument| argument.as_str() == "--effort")
                .count(),
            usize::from(expected_effort.is_some())
        );
        if let Some(expected_effort) = expected_effort {
            assert_eq!(
                args.windows(2)
                    .filter(|pair| pair == &["--effort", expected_effort])
                    .count(),
                1
            );
        }
    }
}

#[tokio::test]
async fn service_preparation_carries_effort_through_fresh_established_and_uncertain_paths() {
    let root = TempDir::new().unwrap();
    let store: Arc<dyn ClaudeSessionStore> =
        Arc::new(FileClaudeSessionStore::new(root.path().join("store")).unwrap());
    let fresh_id = session_id("00000000-0000-4000-8000-000000000032");
    let established_id = session_id("00000000-0000-4000-8000-000000000033");
    let uncertain_id = session_id("00000000-0000-4000-8000-000000000034");
    let mut established = ClaudeSessionV1::new(
        established_id.clone(),
        ClaudeModelAlias::Opus,
        1,
        "established",
    );
    established.lifecycle = ClaudeSessionLifecycle::Established;
    let mut uncertain = ClaudeSessionV1::new(
        uncertain_id.clone(),
        ClaudeModelAlias::Haiku,
        1,
        "uncertain",
    );
    uncertain.lifecycle = ClaudeSessionLifecycle::CreationUncertain;
    for session in [
        ClaudeSessionV1::new(fresh_id.clone(), ClaudeModelAlias::Sonnet, 1, "fresh"),
        established,
        uncertain,
    ] {
        store.save_session(&session).unwrap();
    }
    let mut service = ClaudeService::new(policy("/bin/false", root.path()), Arc::clone(&store));
    let cases = [
        (
            fresh_id.clone(),
            ClaudeModelAlias::Sonnet,
            Some(ClaudeEffort::Low),
            ClaudeInvocation::NewSession {
                session_id: fresh_id,
                model: ClaudeModelAlias::Sonnet,
                effort: Some(ClaudeEffort::Low),
            },
            false,
        ),
        (
            established_id.clone(),
            ClaudeModelAlias::Opus,
            Some(ClaudeEffort::XHigh),
            ClaudeInvocation::ResumeSession {
                session_id: established_id,
                model: ClaudeModelAlias::Opus,
                effort: Some(ClaudeEffort::XHigh),
            },
            false,
        ),
        (
            uncertain_id.clone(),
            ClaudeModelAlias::Haiku,
            Some(ClaudeEffort::Max),
            ClaudeInvocation::ResumeSession {
                session_id: uncertain_id,
                model: ClaudeModelAlias::Haiku,
                effort: Some(ClaudeEffort::Max),
            },
            true,
        ),
    ];

    for (id, alias, effort, expected, expected_uncertain) in cases {
        let prepared = service
            .prepare_turn(id, alias, effort, "prompt".to_owned(), 2)
            .await
            .unwrap();
        assert_eq!(prepared.invocation(), &expected);
        assert_eq!(
            service.abandon_prepared_turn(prepared, 3).await.unwrap(),
            expected_uncertain
        );
    }
}

#[tokio::test]
async fn auth_status_accepts_only_native_first_party_subscription() {
    let root = TempDir::new().unwrap();
    let home = root.path().join("home");
    let cwd = root.path().join("cwd");
    fs::create_dir(&home).unwrap();
    fs::create_dir(&cwd).unwrap();
    let write_status_cli = |name: &str, result: &str| {
        let script = root.path().join(name);
        fs::write(
            &script,
            format!(
                r#"#!/bin/sh
[ "$1" = "--safe-mode" ] || exit 2
[ "$2" = "--settings" ] || exit 3
[ "$3" = '{{"forceLoginMethod":"claudeai"}}' ] || exit 4
[ "$4" = "--setting-sources" ] || exit 5
[ -z "$5" ] || exit 6
[ "$6" = "auth" ] || exit 7
[ "$7" = "status" ] || exit 8
[ "$8" = "--json" ] || exit 9
[ -z "$ANTHROPIC_VAIRE_TEST_SHOULD_SCRUB" ] || exit 10
[ -z "$CLAUDE_VAIRE_TEST_SHOULD_SCRUB" ] || exit 11
[ "$VAIRE_CLAUDE_TEST_UNRELATED" = "must-inherit" ] || exit 12
case "$CLAUDE_CONFIG_DIR" in */home) ;; *) exit 13 ;; esac
{result}
"#
            ),
        )
        .unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).unwrap();
        script
    };
    let subscription = write_status_cli(
        "subscription",
        r#"printf '%s\n' '{"loggedIn":true,"authMethod":"claude.ai","apiProvider":"firstParty"}'"#,
    );
    let signed_out = write_status_cli(
        "signed-out",
        r#"printf '%s\n' '{"loggedIn":false,"authMethod":"none","apiProvider":"firstParty"}'; exit 1"#,
    );
    let failed = write_status_cli(
        "failed",
        r#"printf '%s\n' '{"loggedIn":true,"authMethod":"claude.ai","apiProvider":"firstParty"}'; exit 1"#,
    );
    let unsupported = write_status_cli(
        "unsupported",
        r#"printf '%s\n' '{"loggedIn":true,"authMethod":"oauth_token","apiProvider":"firstParty","subscriptionType":"max"}'"#,
    );
    std::env::set_var("ANTHROPIC_VAIRE_TEST_SHOULD_SCRUB", "must-not-inherit");
    std::env::set_var("CLAUDE_VAIRE_TEST_SHOULD_SCRUB", "must-not-inherit");
    std::env::set_var("VAIRE_CLAUDE_TEST_UNRELATED", "must-inherit");
    assert_eq!(
        inspect_claude_auth(&subscription, &home, &cwd, Duration::from_secs(2))
            .await
            .unwrap(),
        ClaudeCliAuthState::Subscription
    );
    assert_eq!(
        inspect_claude_auth(&signed_out, &home, &cwd, Duration::from_secs(2))
            .await
            .unwrap(),
        ClaudeCliAuthState::SignedOut
    );
    assert_eq!(
        inspect_claude_auth(&unsupported, &home, &cwd, Duration::from_secs(2))
            .await
            .unwrap(),
        ClaudeCliAuthState::Unsupported
    );
    assert_eq!(
        inspect_claude_auth(&failed, &home, &cwd, Duration::from_secs(2)).await,
        Err(ClaudeRuntimeError::AuthStatus)
    );
    std::env::remove_var("ANTHROPIC_VAIRE_TEST_SHOULD_SCRUB");
    std::env::remove_var("CLAUDE_VAIRE_TEST_SHOULD_SCRUB");
    std::env::remove_var("VAIRE_CLAUDE_TEST_UNRELATED");
}

#[tokio::test]
async fn auth_status_output_and_runtime_are_bounded() {
    let root = TempDir::new().unwrap();
    let home = root.path().join("home");
    let cwd = root.path().join("cwd");
    fs::create_dir(&home).unwrap();
    fs::create_dir(&cwd).unwrap();

    let oversized = root.path().join("oversized-auth-status");
    fs::write(
        &oversized,
        "#!/bin/sh\ndd if=/dev/zero bs=65537 count=1 2>/dev/null\n",
    )
    .unwrap();
    fs::set_permissions(&oversized, fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(
        inspect_claude_auth(&oversized, &home, &cwd, Duration::from_secs(2)).await,
        Err(ClaudeRuntimeError::AuthStatus)
    );

    let blocked = root.path().join("blocked-auth-status");
    fs::write(
        &blocked,
        "#!/bin/sh\nsleep 30 &\nprintf '%s\\n' \"$!\" > \"$0.descendant-pid\"\nwait\n",
    )
    .unwrap();
    fs::set_permissions(&blocked, fs::Permissions::from_mode(0o700)).unwrap();
    let result = tokio::time::timeout(
        Duration::from_secs(3),
        inspect_claude_auth(&blocked, &home, &cwd, Duration::from_secs(1)),
    )
    .await
    .expect("status timeout cleanup must be bounded");
    assert_eq!(result, Err(ClaudeRuntimeError::AuthStatus));
    let descendant_pid: i32 =
        fs::read_to_string(root.path().join("blocked-auth-status.descendant-pid"))
            .unwrap()
            .trim()
            .parse()
            .unwrap();
    let descendant_gone = wait_for_test_pid_gone(descendant_pid).await;
    if !descendant_gone {
        kill_test_pid(descendant_pid);
    }
    assert!(descendant_gone, "status timeout must kill CLI descendants");
}

#[tokio::test]
async fn version_probe_cancellation_is_bounded_and_kills_descendants() {
    let root = TempDir::new().unwrap();
    let home = root.path().join("home");
    fs::create_dir(&home).unwrap();
    let script = root.path().join("blocked-version-claude");
    fs::write(
        &script,
        "#!/bin/sh\nsleep 30 &\nprintf '%s\\n' \"$!\" > \"$0.descendant-pid\"\n: > \"$0.started\"\nwait\n",
    )
    .unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).unwrap();
    let started = root.path().join("blocked-version-claude.started");
    let cancellation = tokio_util::sync::CancellationToken::new();
    let driver_cancellation = cancellation.clone();
    let driver = tokio::spawn(async move {
        tokio::time::timeout(Duration::from_secs(2), async {
            while !started.exists() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("version child must start before cancellation");
        driver_cancellation.cancel();
    });

    let result = tokio::time::timeout(
        Duration::from_secs(3),
        verify_claude_version_cancellable(&script, &home, Duration::from_secs(30), &cancellation),
    )
    .await
    .expect("version cancellation and reap must be bounded");
    driver.await.unwrap();

    assert_eq!(result, Err(ClaudeRuntimeError::AuthCancelled));
    let descendant_pid: i32 =
        fs::read_to_string(root.path().join("blocked-version-claude.descendant-pid"))
            .unwrap()
            .trim()
            .parse()
            .unwrap();
    let descendant_gone = wait_for_test_pid_gone(descendant_pid).await;
    if !descendant_gone {
        kill_test_pid(descendant_pid);
    }
    assert!(
        descendant_gone,
        "version cancellation must kill descendants"
    );
}

#[tokio::test]
async fn auth_actions_use_fixed_arguments_and_cli_owned_environment() {
    let root = TempDir::new().unwrap();
    let home = root.path().join("home");
    let cwd = root.path().join("cwd");
    fs::create_dir(&home).unwrap();
    fs::create_dir(&cwd).unwrap();
    let script = root.path().join("fake-auth-claude");
    fs::write(
        &script,
        r#"#!/bin/sh
[ -z "$ANTHROPIC_VAIRE_AUTH_ACTION_TEST" ] || exit 2
[ -z "$CLAUDE_VAIRE_AUTH_ACTION_TEST" ] || exit 3
[ "$VAIRE_AUTH_ACTION_TEST_UNRELATED" = "must-inherit" ] || exit 4
case "$CLAUDE_CONFIG_DIR" in */home) ;; *) exit 5 ;; esac
printf '<%s>\n' "$@" > "$0.args"
ps -o pgid= -p $$ > "$0.pgid"
"#,
    )
    .unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).unwrap();
    std::env::set_var("ANTHROPIC_VAIRE_AUTH_ACTION_TEST", "must-not-inherit");
    std::env::set_var("CLAUDE_VAIRE_AUTH_ACTION_TEST", "must-not-inherit");
    std::env::set_var("VAIRE_AUTH_ACTION_TEST_UNRELATED", "must-inherit");

    run_claude_auth_action(
        &script,
        &home,
        &cwd,
        ClaudeAuthAction::Login,
        tokio_util::sync::CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(
        fs::read_to_string(root.path().join("fake-auth-claude.args")).unwrap(),
        concat!(
            "<--safe-mode>\n",
            "<--settings>\n",
            "<{\"forceLoginMethod\":\"claudeai\"}>\n",
            "<--setting-sources>\n",
            "<>\n",
            "<auth>\n",
            "<login>\n",
            "<--claudeai>\n",
        )
    );
    let child_process_group: i32 = fs::read_to_string(root.path().join("fake-auth-claude.pgid"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    // SAFETY: getpgrp has no preconditions and retains no pointers.
    assert_eq!(child_process_group, unsafe { libc::getpgrp() });

    run_claude_auth_action(
        &script,
        &home,
        &cwd,
        ClaudeAuthAction::Logout,
        tokio_util::sync::CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(
        fs::read_to_string(root.path().join("fake-auth-claude.args")).unwrap(),
        concat!(
            "<--safe-mode>\n",
            "<--settings>\n",
            "<{\"forceLoginMethod\":\"claudeai\"}>\n",
            "<--setting-sources>\n",
            "<>\n",
            "<auth>\n",
            "<logout>\n",
        )
    );
    std::env::remove_var("ANTHROPIC_VAIRE_AUTH_ACTION_TEST");
    std::env::remove_var("CLAUDE_VAIRE_AUTH_ACTION_TEST");
    std::env::remove_var("VAIRE_AUTH_ACTION_TEST_UNRELATED");
}

#[tokio::test]
async fn auth_action_cancellation_is_bounded_and_reaps_the_child() {
    let root = TempDir::new().unwrap();
    let home = root.path().join("home");
    let cwd = root.path().join("cwd");
    fs::create_dir(&home).unwrap();
    fs::create_dir(&cwd).unwrap();
    let script = root.path().join("blocked-auth-claude");
    fs::write(
        &script,
        "#!/bin/sh\nsleep 30 &\nprintf '%s\\n' \"$!\" > \"$0.descendant-pid\"\n: > \"$0.started\"\nwait\n",
    )
    .unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).unwrap();
    let started = root.path().join("blocked-auth-claude.started");
    let cancellation = tokio_util::sync::CancellationToken::new();
    let driver_cancellation = cancellation.clone();
    let driver = tokio::spawn(async move {
        tokio::time::timeout(Duration::from_secs(2), async {
            while !started.exists() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("auth child must start before cancellation");
        driver_cancellation.cancel();
    });
    let result = tokio::time::timeout(
        Duration::from_secs(3),
        run_claude_auth_action(&script, &home, &cwd, ClaudeAuthAction::Login, cancellation),
    )
    .await
    .expect("auth cancellation and reap must be bounded");
    driver.await.unwrap();
    assert_eq!(result, Err(ClaudeRuntimeError::AuthCancelled));
    let descendant_pid: i32 =
        fs::read_to_string(root.path().join("blocked-auth-claude.descendant-pid"))
            .unwrap()
            .trim()
            .parse()
            .unwrap();
    let descendant_gone = wait_for_test_pid_gone(descendant_pid).await;
    if !descendant_gone {
        kill_test_pid(descendant_pid);
    }
    assert!(
        descendant_gone,
        "auth cancellation must kill CLI descendants"
    );
}

#[test]
fn parser_requires_correlated_init_and_reconciles_terminal_snapshot() {
    let id = session_id("00000000-0000-4000-8000-000000000002");
    let mut parser = ClaudeStreamParser::new(id.clone());
    let init = format!(
        r#"{{"type":"system","subtype":"init","session_id":"{}","model":"claude-sonnet-test"}}"#,
        id.as_str()
    );
    assert!(matches!(
        parser.parse_line(init.as_bytes()).unwrap(),
        Some(ClaudeStreamEvent::Initialized { .. })
    ));
    let delta = format!(
        r#"{{"type":"stream_event","session_id":"{}","event":{{"type":"content_block_delta","delta":{{"type":"text_delta","text":"hel"}}}}}}"#,
        id.as_str()
    );
    assert_eq!(
        parser.parse_line(delta.as_bytes()).unwrap(),
        Some(ClaudeStreamEvent::TextDelta {
            delta: "hel".to_owned()
        })
    );
    let result = format!(
        r#"{{"type":"result","subtype":"success","is_error":false,"session_id":"{}","result":"hello"}}"#,
        id.as_str()
    );
    assert_eq!(
        parser.parse_line(result.as_bytes()).unwrap(),
        Some(ClaudeStreamEvent::Terminal {
            success: true,
            final_text: Some("hello".to_owned())
        })
    );
    assert_eq!(parser.assistant_text(), "hello");
    assert!(parser.finish_eof().is_ok());
}

#[test]
fn parser_rejects_semantic_output_before_init_and_contradictory_final_text() {
    let id = session_id("00000000-0000-4000-8000-000000000003");
    let delta = format!(
        r#"{{"type":"stream_event","session_id":"{}","event":{{"type":"content_block_delta","delta":{{"type":"text_delta","text":"x"}}}}}}"#,
        id.as_str()
    );
    let mut parser = ClaudeStreamParser::new(id.clone());
    assert_eq!(
        parser.parse_line(delta.as_bytes()),
        Err(ClaudeProtocolError::Ordering)
    );

    let mut parser = ClaudeStreamParser::new(id.clone());
    let init = format!(
        r#"{{"type":"system","subtype":"init","session_id":"{}","model":"m"}}"#,
        id.as_str()
    );
    parser.parse_line(init.as_bytes()).unwrap();
    parser.parse_line(delta.as_bytes()).unwrap();
    let result = format!(
        r#"{{"type":"result","subtype":"success","is_error":false,"session_id":"{}","result":"different"}}"#,
        id.as_str()
    );
    assert_eq!(
        parser.parse_line(result.as_bytes()),
        Err(ClaudeProtocolError::ContradictoryFinal)
    );
}

#[test]
fn parser_requires_live_exact_correlation_for_unknown_top_level_events() {
    let id = session_id("00000000-0000-4000-8000-000000000004");
    let other = session_id("00000000-0000-4000-8000-000000000005");
    let mut parser = ClaudeStreamParser::new(id.clone());

    assert_eq!(
        parser.parse_line(br#"{"type":"future_event"}"#),
        Err(ClaudeProtocolError::Ordering)
    );

    let init = format!(
        r#"{{"type":"system","subtype":"init","session_id":"{}","model":"m"}}"#,
        id.as_str()
    );
    parser.parse_line(init.as_bytes()).unwrap();

    assert_eq!(
        parser.parse_line(br#"{"type":"future_event"}"#),
        Err(ClaudeProtocolError::Malformed)
    );
    let mismatched = format!(
        r#"{{"type":"future_event","session_id":"{}"}}"#,
        other.as_str()
    );
    assert_eq!(
        parser.parse_line(mismatched.as_bytes()),
        Err(ClaudeProtocolError::Ordering)
    );
    let correlated = format!(
        r#"{{"type":"future_event","session_id":"{}"}}"#,
        id.as_str()
    );
    assert_eq!(parser.parse_line(correlated.as_bytes()).unwrap(), None);

    let result = format!(
        r#"{{"type":"result","subtype":"success","is_error":false,"session_id":"{}","result":""}}"#,
        id.as_str()
    );
    parser.parse_line(result.as_bytes()).unwrap();
    assert_eq!(
        parser.parse_line(correlated.as_bytes()),
        Err(ClaudeProtocolError::Ordering)
    );
}

#[test]
fn parser_correlates_non_init_system_subtypes() {
    let id = session_id("00000000-0000-4000-8000-000000000006");
    let other = session_id("00000000-0000-4000-8000-000000000007");
    let retry = |session: &ClaudeSessionId| {
        format!(
            r#"{{"type":"system","subtype":"api_retry","session_id":"{}"}}"#,
            session.as_str()
        )
    };

    let mut parser = ClaudeStreamParser::new(id.clone());
    assert_eq!(
        parser.parse_line(retry(&id).as_bytes()),
        Err(ClaudeProtocolError::Ordering)
    );
    let init = format!(
        r#"{{"type":"system","subtype":"init","session_id":"{}","model":"m"}}"#,
        id.as_str()
    );
    parser.parse_line(init.as_bytes()).unwrap();
    assert_eq!(
        parser.parse_line(br#"{"type":"system","subtype":"api_retry"}"#),
        Err(ClaudeProtocolError::Malformed)
    );
    assert_eq!(
        parser.parse_line(retry(&other).as_bytes()),
        Err(ClaudeProtocolError::Ordering)
    );
    assert_eq!(parser.parse_line(retry(&id).as_bytes()).unwrap(), None);
}

#[test]
fn file_store_round_trips_and_repairs_pending_work() {
    let root = TempDir::new().unwrap();
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let store = FileClaudeSessionStore::new(root.path().join("claude")).unwrap();
    let id = session_id("00000000-0000-4000-8000-000000000004");
    let mut session = ClaudeSessionV1::new(id.clone(), ClaudeModelAlias::Opus, 1, "title");
    session.lifecycle = ClaudeSessionLifecycle::CreationPending;
    session.turns.push(ClaudeTurnRecord {
        id: turn_id("00000000-0000-4000-8000-000000000005"),
        requested_model: ClaudeModelAlias::Opus,
        user_text: "hello".to_owned(),
        assistant_text: None,
        incomplete_assistant_text: None,
        outcome: ClaudeTurnOutcome::InProgress,
    });
    store.save_session(&session).unwrap();

    let live = store.load_session_for_update(&id).unwrap();
    assert_eq!(live.lifecycle, ClaudeSessionLifecycle::CreationPending);
    assert_eq!(live.turns[0].outcome, ClaudeTurnOutcome::InProgress);

    let mut repaired = store.load_session(&id).unwrap();
    assert_eq!(
        repaired.lifecycle,
        ClaudeSessionLifecycle::CreationUncertain
    );
    assert_eq!(repaired.turns[0].outcome, ClaudeTurnOutcome::Interrupted);
    assert!(repaired.turns[0].incomplete_assistant_text.is_none());
    repaired.turns.push(ClaudeTurnRecord {
        id: turn_id("00000000-0000-4000-8000-000000000006"),
        requested_model: ClaudeModelAlias::Opus,
        user_text: "fail".to_owned(),
        assistant_text: None,
        incomplete_assistant_text: Some("partial".to_owned()),
        outcome: ClaudeTurnOutcome::Failed,
    });
    store.save_session(&repaired).unwrap();
    let restored = store.load_session(&id).unwrap();
    assert_eq!(
        restored.turns[1].incomplete_assistant_text.as_deref(),
        Some("partial")
    );
    assert_eq!(store.list_sessions().unwrap().len(), 1);
    store.delete_session(&id).unwrap();
    assert!(matches!(
        store.load_session(&id),
        Err(ClaudeStoreError::NotFound)
    ));
}

#[test]
fn file_store_rejects_non_owner_only_root_modes() {
    let root = TempDir::new().unwrap();
    let store_root = root.path().join("claude");
    fs::create_dir(&store_root).unwrap();
    fs::set_permissions(&store_root, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(matches!(
        FileClaudeSessionStore::new(store_root),
        Err(ClaudeStoreError::Permissions)
    ));
}

#[tokio::test]
async fn fake_process_streams_and_reaps_with_sanitized_inherited_environment() {
    let root = TempDir::new().unwrap();
    for directory in [root.path().join("home"), root.path().join("cwd")] {
        fs::create_dir(&directory).unwrap();
    }
    let id = session_id("00000000-0000-4000-8000-000000000006");
    let script = root.path().join("fake-claude");
    let init = format!(
        r#"{{"type":"system","subtype":"init","session_id":"{}","model":"fake"}}"#,
        id.as_str()
    );
    let delta = format!(
        r#"{{"type":"stream_event","session_id":"{}","event":{{"type":"content_block_delta","delta":{{"type":"text_delta","text":"ok"}}}}}}"#,
        id.as_str()
    );
    let result = format!(
        r#"{{"type":"result","subtype":"success","is_error":false,"session_id":"{}","result":"ok"}}"#,
        id.as_str()
    );
    fs::write(
        &script,
        format!(
            r#"#!/bin/sh
effort_count=0
effort_value=
while [ "$#" -gt 0 ]; do
  [ "$1" != "hello" ] || exit 13
  if [ "$1" = "--effort" ]; then
    effort_count=$((effort_count + 1))
    shift
    [ "$#" -gt 0 ] || exit 14
    effort_value=$1
  fi
  shift
done
[ "$effort_count" -eq 1 ] || exit 15
[ "$effort_value" = "high" ] || exit 16
IFS= read -r prompt
[ "$prompt" = "hello" ] || exit 17
[ -z "$ANTHROPIC_TEST_SHOULD_SCRUB" ] || exit 8
[ "$UNRELATED_SECRET" = "must-inherit" ] || exit 9
[ -z "$ANTHROPIC_BASE_URL" ] || exit 10
[ -z "$CLAUDE_TEST_SHOULD_SCRUB" ] || exit 11
case "$CLAUDE_CONFIG_DIR" in */home) ;; *) exit 12 ;; esac
printf '%s\n' '{init}' '{delta}' '{result}'
"#
        ),
    )
    .unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).unwrap();
    std::env::set_var("UNRELATED_SECRET", "must-inherit");
    std::env::set_var("ANTHROPIC_TEST_SHOULD_SCRUB", "must-not-leak");
    std::env::set_var("ANTHROPIC_BASE_URL", "must-not-leak");
    std::env::set_var("CLAUDE_TEST_SHOULD_SCRUB", "must-not-leak");
    let policy = policy(&script, root.path());
    let invocation = ClaudeInvocation::NewSession {
        session_id: id.clone(),
        model: ClaudeModelAlias::Haiku,
        effort: Some(ClaudeEffort::High),
    };
    let cancellation = tokio_util::sync::CancellationToken::new();
    let mut child = ClaudeChild::spawn(&policy, &invocation, id, "hello", &cancellation)
        .await
        .unwrap();
    let mut events = Vec::new();
    while let Some(event) = child.next_event().await.unwrap() {
        events.push(event);
    }
    child.finish(&cancellation).await.unwrap();
    std::env::remove_var("UNRELATED_SECRET");
    std::env::remove_var("ANTHROPIC_TEST_SHOULD_SCRUB");
    std::env::remove_var("ANTHROPIC_BASE_URL");
    std::env::remove_var("CLAUDE_TEST_SHOULD_SCRUB");
    assert_eq!(events.len(), 3);
}

#[tokio::test]
async fn service_completes_and_persists_only_successful_assistant_text() {
    let root = TempDir::new().unwrap();
    for directory in [root.path().join("home"), root.path().join("cwd")] {
        fs::create_dir(&directory).unwrap();
    }
    let id_text = "00000000-0000-4000-8000-000000000007";
    let script = root.path().join("fake-claude");
    let init = format!(
        r#"{{"type":"system","subtype":"init","session_id":"{}","model":"fake"}}"#,
        id_text
    );
    let status = format!(
        r#"{{"type":"system","subtype":"status","session_id":"{}"}}"#,
        id_text
    );
    let rate_limit = format!(
        r#"{{"type":"rate_limit_event","session_id":"{}"}}"#,
        id_text
    );
    let delta = format!(
        r#"{{"type":"stream_event","session_id":"{}","event":{{"type":"content_block_delta","delta":{{"type":"text_delta","text":"done"}}}}}}"#,
        id_text
    );
    let assistant = format!(
        r#"{{"type":"assistant","session_id":"{}","message":{{}}}}"#,
        id_text
    );
    let stop = format!(
        r#"{{"type":"stream_event","session_id":"{}","event":{{"type":"message_stop"}}}}"#,
        id_text
    );
    let result = format!(
        r#"{{"type":"result","subtype":"success","is_error":false,"session_id":"{}","result":"done"}}"#,
        id_text
    );
    fs::write(
        &script,
        format!(
            "#!/bin/sh\nmcp_config=\nstrict_mcp_count=0\nwhile [ \"$#\" -gt 0 ]; do\n  case \"$1\" in\n    --strict-mcp-config)\n      strict_mcp_count=$((strict_mcp_count + 1))\n      ;;\n    --mcp-config)\n      shift\n      [ \"$#\" -gt 0 ] || exit 31\n      mcp_config=$1\n      ;;\n  esac\n  shift\ndone\n[ \"$strict_mcp_count\" -eq 1 ] || exit 32\n[ \"$mcp_config\" = '{{\"mcpServers\":{{}}}}' ] || exit 33\nread prompt\nprintf '%s\\n' '{init}' '{status}' '{rate_limit}' '{delta}' '{assistant}' '{stop}' '{result}'\n"
        ),
    )
    .unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).unwrap();

    let store_root = root.path().join("store");
    let store: Arc<dyn ClaudeSessionStore> =
        Arc::new(FileClaudeSessionStore::new(&store_root).unwrap());
    let id = session_id(id_text);
    store
        .save_session(&ClaudeSessionV1::new(
            id.clone(),
            ClaudeModelAlias::Sonnet,
            1,
            "title",
        ))
        .unwrap();
    let mut service = ClaudeService::new(policy(&script, root.path()), Arc::clone(&store));
    let prepared = service
        .prepare_turn(
            id.clone(),
            ClaudeModelAlias::Sonnet,
            None,
            "hello".to_owned(),
            2,
        )
        .await
        .unwrap();
    service.launch_prepared_turn(prepared, 3).await.unwrap();

    tokio::time::timeout(Duration::from_secs(2), async {
        while let Some(event) = service.next_event().await {
            if let ClaudeServiceEvent::TurnFinished { outcome, .. } = event {
                assert_eq!(outcome, ClaudeTurnOutcome::Completed);
                return;
            }
        }
        panic!("service event stream ended before turn completion");
    })
    .await
    .expect("fake Claude turn must finish promptly");
    let stored = store.load_session(&id).unwrap();
    assert_eq!(stored.turns[0].assistant_text.as_deref(), Some("done"));
    assert_eq!(stored.lifecycle, ClaudeSessionLifecycle::Established);
    service.shutdown().await;
}

#[tokio::test]
async fn service_correlates_spawn_failure_and_restores_fresh_lifecycle() {
    let root = TempDir::new().unwrap();
    for directory in [root.path().join("home"), root.path().join("cwd")] {
        fs::create_dir(&directory).unwrap();
    }
    let id = session_id("00000000-0000-4000-8000-000000000020");
    let store: Arc<dyn ClaudeSessionStore> =
        Arc::new(FileClaudeSessionStore::new(root.path().join("store")).unwrap());
    store
        .save_session(&ClaudeSessionV1::new(
            id.clone(),
            ClaudeModelAlias::Sonnet,
            1,
            "spawn failure",
        ))
        .unwrap();
    let mut service = ClaudeService::new(
        policy(root.path().join("missing-claude"), root.path()),
        Arc::clone(&store),
    );
    let prepared = service
        .prepare_turn(
            id.clone(),
            ClaudeModelAlias::Sonnet,
            None,
            "hello".to_owned(),
            2,
        )
        .await
        .unwrap();
    service.launch_prepared_turn(prepared, 3).await.unwrap();

    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(1), service.next_event())
            .await
            .unwrap(),
        Some(ClaudeServiceEvent::TurnStarted { .. })
    ));
    let finished = tokio::time::timeout(Duration::from_secs(1), service.next_event())
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        finished,
        ClaudeServiceEvent::TurnFinished {
            outcome: ClaudeTurnOutcome::Failed,
            creation_uncertain: false,
            failure: Some(ClaudeError {
                stage: ClaudeFailureStage::Spawn,
                ..
            }),
            ..
        }
    ));
    let stored = store.load_session(&id).unwrap();
    assert_eq!(stored.lifecycle, ClaudeSessionLifecycle::Fresh);
    assert_eq!(stored.turns[0].outcome, ClaudeTurnOutcome::Failed);
}

#[tokio::test]
async fn shutdown_drains_a_saturated_event_queue_before_awaiting_the_turn() {
    let root = TempDir::new().unwrap();
    for directory in [root.path().join("home"), root.path().join("cwd")] {
        fs::create_dir(&directory).unwrap();
    }
    let id_text = "00000000-0000-4000-8000-000000000021";
    let id = session_id(id_text);
    let script = root.path().join("flood-claude");
    let init =
        format!(r#"{{"type":"system","subtype":"init","session_id":"{id_text}","model":"fake"}}"#);
    let delta = format!(
        r#"{{"type":"stream_event","session_id":"{id_text}","event":{{"type":"content_block_delta","delta":{{"type":"text_delta","text":"x"}}}}}}"#
    );
    fs::write(
        &script,
        format!(
            "#!/bin/sh\nread prompt\nprintf \x27%s\\n\x27 \x27{init}\x27\ni=0\nwhile [ \"$i\" -lt 80 ]; do printf \x27%s\\n\x27 \x27{delta}\x27; i=$((i + 1)); done\nsleep 30\n"
        ),
    )
    .unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).unwrap();
    let store: Arc<dyn ClaudeSessionStore> =
        Arc::new(FileClaudeSessionStore::new(root.path().join("store")).unwrap());
    store
        .save_session(&ClaudeSessionV1::new(
            id.clone(),
            ClaudeModelAlias::Sonnet,
            1,
            "queue flood",
        ))
        .unwrap();
    let mut service = ClaudeService::new(policy(&script, root.path()), Arc::clone(&store));
    let prepared = service
        .prepare_turn(
            id.clone(),
            ClaudeModelAlias::Sonnet,
            None,
            "hello".to_owned(),
            2,
        )
        .await
        .unwrap();
    service.launch_prepared_turn(prepared, 3).await.unwrap();
    tokio::time::sleep(Duration::from_millis(150)).await;

    let drained = tokio::time::timeout(Duration::from_secs(4), service.shutdown())
        .await
        .expect("saturated shutdown must not deadlock");
    assert!(
        drained.iter().any(|event| matches!(
            event,
            ClaudeServiceEvent::TurnFinished {
                outcome: ClaudeTurnOutcome::Interrupted,
                ..
            }
        )),
        "drained events: {drained:?}"
    );
    let stored = store.load_session(&id).unwrap();
    assert_eq!(stored.turns[0].outcome, ClaudeTurnOutcome::Interrupted);
}

#[tokio::test]
async fn final_store_failure_never_emits_a_completed_authoritative_answer() {
    let root = TempDir::new().unwrap();
    for directory in [root.path().join("home"), root.path().join("cwd")] {
        fs::create_dir(&directory).unwrap();
    }
    let id_text = "00000000-0000-4000-8000-000000000022";
    let id = session_id(id_text);
    let marker = root.path().join("continue");
    let script = root.path().join("store-failure-claude");
    let init =
        format!(r#"{{"type":"system","subtype":"init","session_id":"{id_text}","model":"fake"}}"#);
    let delta = format!(
        r#"{{"type":"stream_event","session_id":"{id_text}","event":{{"type":"content_block_delta","delta":{{"type":"text_delta","text":"done"}}}}}}"#
    );
    let result = format!(
        r#"{{"type":"result","subtype":"success","is_error":false,"session_id":"{id_text}","result":"done"}}"#
    );
    fs::write(
        &script,
        format!(
            "#!/bin/sh\nread prompt\nprintf \x27%s\\n\x27 \x27{init}\x27\nwhile [ ! -f \"{}\" ]; do sleep 0.01; done\nprintf \x27%s\\n\x27 \x27{delta}\x27\nprintf \x27%s\\n\x27 \x27{result}\x27\n",
            marker.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).unwrap();
    let store_root = root.path().join("store");
    let store: Arc<dyn ClaudeSessionStore> =
        Arc::new(FileClaudeSessionStore::new(&store_root).unwrap());
    store
        .save_session(&ClaudeSessionV1::new(
            id.clone(),
            ClaudeModelAlias::Sonnet,
            1,
            "store failure",
        ))
        .unwrap();
    let mut service = ClaudeService::new(policy(&script, root.path()), Arc::clone(&store));
    let prepared = service
        .prepare_turn(
            id.clone(),
            ClaudeModelAlias::Sonnet,
            None,
            "hello".to_owned(),
            2,
        )
        .await
        .unwrap();
    service.launch_prepared_turn(prepared, 3).await.unwrap();
    assert!(matches!(
        service.next_event().await,
        Some(ClaudeServiceEvent::TurnStarted { .. })
    ));
    let initialized_event = service.next_event().await;
    assert!(
        matches!(
            initialized_event,
            Some(ClaudeServiceEvent::Initialized { .. })
        ),
        "second event: {initialized_event:?}"
    );

    let sessions_dir = store_root.join("sessions");
    fs::set_permissions(&sessions_dir, fs::Permissions::from_mode(0o500)).unwrap();
    fs::write(&marker, b"continue").unwrap();
    let finished = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let event = service.next_event().await.unwrap();
            if matches!(event, ClaudeServiceEvent::TurnFinished { .. }) {
                break event;
            }
        }
    })
    .await
    .unwrap();
    fs::set_permissions(&sessions_dir, fs::Permissions::from_mode(0o700)).unwrap();
    assert!(matches!(
        finished,
        ClaudeServiceEvent::TurnFinished {
            outcome: ClaudeTurnOutcome::Failed,
            assistant_text: None,
            incomplete_assistant_text: Some(ref text),
            creation_uncertain: false,
            failure: Some(ClaudeError {
                stage: ClaudeFailureStage::Store,
                ..
            }),
            ..
        } if text == "done"
    ));
    let stored = store.load_session(&id).unwrap();
    assert_ne!(stored.turns[0].outcome, ClaudeTurnOutcome::Completed);
}

#[tokio::test]
async fn interrupt_cancels_the_final_wait_after_terminal_stdout_closes() {
    let root = TempDir::new().unwrap();
    for directory in [root.path().join("home"), root.path().join("cwd")] {
        fs::create_dir(&directory).unwrap();
    }
    let id_text = "00000000-0000-4000-8000-000000000023";
    let id = session_id(id_text);
    let script = root.path().join("final-wait-claude");
    let init =
        format!(r#"{{"type":"system","subtype":"init","session_id":"{id_text}","model":"fake"}}"#);
    let result = format!(
        r#"{{"type":"result","subtype":"success","is_error":false,"session_id":"{id_text}","result":"done"}}"#
    );
    fs::write(
        &script,
        format!(
            "#!/bin/sh\nread prompt\nprintf \x27%s\\n\x27 \x27{init}\x27\nprintf \x27%s\\n\x27 \x27{result}\x27\nexec 1>&-\nsleep 30\n"
        ),
    )
    .unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).unwrap();
    let store: Arc<dyn ClaudeSessionStore> =
        Arc::new(FileClaudeSessionStore::new(root.path().join("store")).unwrap());
    store
        .save_session(&ClaudeSessionV1::new(
            id.clone(),
            ClaudeModelAlias::Sonnet,
            1,
            "final wait",
        ))
        .unwrap();
    let mut service = ClaudeService::new(policy(&script, root.path()), Arc::clone(&store));
    let prepared = service
        .prepare_turn(
            id.clone(),
            ClaudeModelAlias::Sonnet,
            None,
            "hello".to_owned(),
            2,
        )
        .await
        .unwrap();
    service.launch_prepared_turn(prepared, 3).await.unwrap();
    tokio::time::sleep(Duration::from_millis(150)).await;

    let drained = tokio::time::timeout(Duration::from_secs(4), service.interrupt_and_drain())
        .await
        .expect("final wait interruption must be bounded");
    assert!(
        drained.iter().any(|event| matches!(
            event,
            ClaudeServiceEvent::TurnFinished {
                outcome: ClaudeTurnOutcome::Interrupted,
                ..
            }
        )),
        "drained events: {drained:?}"
    );
    let stored = store.load_session(&id).unwrap();
    assert_eq!(stored.turns[0].outcome, ClaudeTurnOutcome::Interrupted);
}

#[tokio::test]
async fn unverified_session_and_prepared_turn_commits_never_reach_process_launch() {
    let root = TempDir::new().unwrap();
    let unverified = Arc::new(ScriptedCommitStore::new(
        None,
        CommitStatus::CommittedUnverified,
    ));
    let mut service = ClaudeService::new(policy("/bin/false", root.path()), unverified.clone());
    let (unverified_id, commit) = service
        .create_session(ClaudeModelAlias::Sonnet, 1)
        .await
        .unwrap();
    assert_eq!(commit.source, CommitStatus::CommittedUnverified);
    assert_eq!(
        unverified
            .load_session_for_update(&unverified_id)
            .unwrap()
            .session_id,
        unverified_id
    );

    let id = session_id("00000000-0000-4000-8000-000000000024");
    let seeded = Arc::new(ScriptedCommitStore::new(
        Some(ClaudeSessionV1::new(
            id.clone(),
            ClaudeModelAlias::Sonnet,
            1,
            "unverified prepare",
        )),
        CommitStatus::CommittedUnverified,
    ));
    service = ClaudeService::new(policy("/bin/false", root.path()), seeded.clone());
    assert!(service
        .prepare_turn(
            id.clone(),
            ClaudeModelAlias::Sonnet,
            None,
            "hello".to_owned(),
            2,
        )
        .await
        .is_err());
    let stored = seeded.load_session_for_update(&id).unwrap();
    assert_eq!(stored.lifecycle, ClaudeSessionLifecycle::Fresh);
    assert_eq!(stored.turns[0].outcome, ClaudeTurnOutcome::Interrupted);

    let uncertain_id = session_id("00000000-0000-4000-8000-000000000027");
    let mut uncertain_session = ClaudeSessionV1::new(
        uncertain_id.clone(),
        ClaudeModelAlias::Sonnet,
        3,
        "unverified uncertain prepare",
    );
    uncertain_session.lifecycle = ClaudeSessionLifecycle::CreationUncertain;
    let uncertain_store = Arc::new(ScriptedCommitStore::new(
        Some(uncertain_session),
        CommitStatus::CommittedUnverified,
    ));
    service = ClaudeService::new(policy("/bin/false", root.path()), uncertain_store.clone());
    let prepared = service
        .prepare_turn(
            uncertain_id.clone(),
            ClaudeModelAlias::Sonnet,
            None,
            "retry".to_owned(),
            4,
        )
        .await
        .unwrap();
    service.launch_prepared_turn(prepared, 5).await.unwrap();
    assert!(matches!(
        service.next_event().await,
        Some(ClaudeServiceEvent::TurnStarted { .. })
    ));
    assert!(matches!(
        service.next_event().await,
        Some(ClaudeServiceEvent::TurnFinished {
            outcome: ClaudeTurnOutcome::Failed,
            creation_uncertain: true,
            failure: Some(ClaudeError {
                stage: ClaudeFailureStage::Store,
                ..
            }),
            ..
        })
    ));
    assert_eq!(
        uncertain_store
            .load_session_for_update(&uncertain_id)
            .unwrap()
            .lifecycle,
        ClaudeSessionLifecycle::CreationUncertain
    );
}

#[tokio::test]
async fn uncertain_resume_abandonment_and_preinit_failure_stay_uncertain() {
    let root = TempDir::new().unwrap();
    for directory in [root.path().join("home"), root.path().join("cwd")] {
        fs::create_dir(&directory).unwrap();
    }
    let id = session_id("00000000-0000-4000-8000-000000000025");
    let store: Arc<dyn ClaudeSessionStore> =
        Arc::new(FileClaudeSessionStore::new(root.path().join("store")).unwrap());
    let mut session =
        ClaudeSessionV1::new(id.clone(), ClaudeModelAlias::Sonnet, 1, "uncertain resume");
    session.lifecycle = ClaudeSessionLifecycle::CreationUncertain;
    store.save_session(&session).unwrap();
    let mut service = ClaudeService::new(
        policy(root.path().join("missing-claude"), root.path()),
        Arc::clone(&store),
    );

    let abandoned = service
        .prepare_turn(
            id.clone(),
            ClaudeModelAlias::Sonnet,
            None,
            "abandon".to_owned(),
            2,
        )
        .await
        .unwrap();
    assert!(service.abandon_prepared_turn(abandoned, 3).await.unwrap());
    let stored = store.load_session(&id).unwrap();
    assert_eq!(stored.lifecycle, ClaudeSessionLifecycle::CreationUncertain);
    assert_eq!(stored.turns[0].outcome, ClaudeTurnOutcome::Interrupted);

    let prepared = service
        .prepare_turn(
            id.clone(),
            ClaudeModelAlias::Sonnet,
            None,
            "retry".to_owned(),
            4,
        )
        .await
        .unwrap();
    service.launch_prepared_turn(prepared, 5).await.unwrap();
    assert!(matches!(
        service.next_event().await,
        Some(ClaudeServiceEvent::TurnStarted { .. })
    ));
    assert!(matches!(
        service.next_event().await,
        Some(ClaudeServiceEvent::TurnFinished {
            outcome: ClaudeTurnOutcome::Failed,
            creation_uncertain: true,
            failure: Some(ClaudeError {
                stage: ClaudeFailureStage::Spawn,
                ..
            }),
            ..
        })
    ));
    let stored = store.load_session(&id).unwrap();
    assert_eq!(stored.lifecycle, ClaudeSessionLifecycle::CreationUncertain);
    assert_eq!(stored.turns[1].outcome, ClaudeTurnOutcome::Failed);
}

#[tokio::test]
async fn uncertain_resume_spawn_failure_is_correlated_and_reblocks() {
    let root = TempDir::new().unwrap();
    for directory in [root.path().join("home"), root.path().join("cwd")] {
        fs::create_dir(&directory).unwrap();
    }
    let id = session_id("00000000-0000-4000-8000-000000000026");
    let store: Arc<dyn ClaudeSessionStore> =
        Arc::new(FileClaudeSessionStore::new(root.path().join("store")).unwrap());
    let mut session =
        ClaudeSessionV1::new(id.clone(), ClaudeModelAlias::Sonnet, 1, "uncertain spawn");
    session.lifecycle = ClaudeSessionLifecycle::CreationUncertain;
    store.save_session(&session).unwrap();
    let mut service = ClaudeService::new(
        policy(root.path().join("missing-claude"), root.path()),
        Arc::clone(&store),
    );

    let prepared = service
        .prepare_turn(
            id.clone(),
            ClaudeModelAlias::Sonnet,
            None,
            "retry".to_owned(),
            2,
        )
        .await
        .unwrap();
    service.launch_prepared_turn(prepared, 3).await.unwrap();
    assert!(matches!(
        service.next_event().await,
        Some(ClaudeServiceEvent::TurnStarted { .. })
    ));
    assert!(matches!(
        service.next_event().await,
        Some(ClaudeServiceEvent::TurnFinished {
            outcome: ClaudeTurnOutcome::Failed,
            creation_uncertain: true,
            failure: Some(ClaudeError {
                stage: ClaudeFailureStage::Spawn,
                category: ClaudeFailureCategory::Unavailable,
            }),
            ..
        })
    ));
    let stored = store.load_session(&id).unwrap();
    assert_eq!(stored.lifecycle, ClaudeSessionLifecycle::CreationUncertain);
    assert_eq!(stored.turns[0].outcome, ClaudeTurnOutcome::Failed);
}

#[tokio::test]
async fn post_spawn_stdin_cancellation_marks_new_session_creation_uncertain() {
    let root = TempDir::new().unwrap();
    for directory in [root.path().join("home"), root.path().join("cwd")] {
        fs::create_dir(&directory).unwrap();
    }
    let script = root.path().join("fake-claude-blocked-stdin");
    fs::write(&script, "#!/bin/sh\n: > \"$0.started\"\nsleep 30\n").unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).unwrap();
    let started = root.path().join("fake-claude-blocked-stdin.started");
    let store: Arc<dyn ClaudeSessionStore> =
        Arc::new(FileClaudeSessionStore::new(root.path().join("store")).unwrap());
    let mut service = ClaudeService::new(policy(script, root.path()), Arc::clone(&store));
    let (id, commit) = service
        .create_session(ClaudeModelAlias::Sonnet, 1)
        .await
        .unwrap();
    assert_eq!(commit.source, CommitStatus::Verified);
    let prepared = service
        .prepare_turn(
            id.clone(),
            ClaudeModelAlias::Sonnet,
            None,
            "x".repeat(128 * 1024),
            2,
        )
        .await
        .unwrap();
    service.launch_prepared_turn(prepared, 3).await.unwrap();
    assert!(matches!(
        service.next_event().await,
        Some(ClaudeServiceEvent::TurnStarted { .. })
    ));
    tokio::time::timeout(Duration::from_secs(2), async {
        while !started.exists() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("fake CLI must be running before cancellation");

    let drained = tokio::time::timeout(Duration::from_secs(4), service.interrupt_and_drain())
        .await
        .expect("post-spawn cancellation must settle promptly");
    assert!(
        drained.iter().any(|event| matches!(
            event,
            ClaudeServiceEvent::TurnFinished {
                outcome: ClaudeTurnOutcome::Interrupted,
                creation_uncertain: true,
                ..
            }
        )),
        "drained events: {drained:?}"
    );
    let stored = store.load_session(&id).unwrap();
    assert_eq!(stored.lifecycle, ClaudeSessionLifecycle::CreationUncertain);
    assert_eq!(stored.turns[0].outcome, ClaudeTurnOutcome::Interrupted);
}
