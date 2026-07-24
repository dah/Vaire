use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::{Arc, Mutex};

use crate::storage::CommitStatus;
use std::time::Duration;

use tempfile::TempDir;

use crate::credentials::{CredentialAccount, CredentialStore, FakeCredentialStore, SecretValue};
use crate::provider::{ClaudeSessionId, ClaudeTurnId};

use super::process::argv_strings;
use super::*;

fn session_id(value: &str) -> ClaudeSessionId {
    value.parse().unwrap()
}

fn turn_id(value: &str) -> ClaudeTurnId {
    value.parse().unwrap()
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
fn fixed_aliases_and_argv_keep_prompt_and_key_out_of_arguments() {
    let root = TempDir::new().unwrap();
    let policy = policy("/bin/false", root.path());
    let id = session_id("00000000-0000-4000-8000-000000000001");
    let user = argv_strings(
        &policy,
        &ClaudeInvocation::NewSession {
            session_id: id,
            model: ClaudeModelAlias::Sonnet,
        },
    );
    assert!(user.contains(&"--safe-mode".to_owned()));
    assert!(user.contains(&"--verbose".to_owned()));
    assert!(user.contains(&"--dangerously-skip-permissions".to_owned()));
    assert!(user.windows(2).any(|pair| pair == ["--model", "sonnet"]));
    let joined = user.join(" ");
    assert!(!joined.contains("user prompt"));
    assert!(!joined.contains("test-console-key"));
    assert_eq!(
        CLAUDE_MODEL_ALIASES.map(claude_model_selector),
        ["default", "opus", "sonnet", "haiku"]
    );
}

#[tokio::test]
async fn credential_probe_requires_the_injected_console_key_source() {
    let root = TempDir::new().unwrap();
    let home = root.path().join("home");
    let cwd = root.path().join("cwd");
    fs::create_dir(&home).unwrap();
    fs::create_dir(&cwd).unwrap();
    let script = root.path().join("fake-claude");
    fs::write(
        &script,
        r#"#!/bin/sh
[ "$1" = "--bare" ] || exit 2
[ "$2" = "--safe-mode" ] || exit 3
[ "$3" = "auth" ] || exit 4
[ "$4" = "status" ] || exit 5
[ "$5" = "--json" ] || exit 6
[ "$ANTHROPIC_API_KEY" = "test-console-key" ] || exit 7
[ -n "$CLAUDE_CONFIG_DIR" ] || exit 8
printf '%s\n' '{"loggedIn":true,"authMethod":"api_key","apiProvider":"firstParty","apiKeySource":"ANTHROPIC_API_KEY"}'
"#,
    )
    .unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).unwrap();
    let key = SecretValue::from_input("test-console-key").unwrap();
    verify_claude_credential_source(&script, &home, &cwd, &key, Duration::from_secs(2))
        .await
        .unwrap();
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
            "#!/bin/sh\nread prompt\n[ -n \"$ANTHROPIC_API_KEY\" ] || exit 8\n[ \"$UNRELATED_SECRET\" = \"must-inherit\" ] || exit 9\n[ -z \"$ANTHROPIC_BASE_URL\" ] || exit 10\ncase \"$CLAUDE_CONFIG_DIR\" in */home) ;; *) exit 11 ;; esac\nprintf '%s\\n' '{init}' '{delta}' '{result}'\n"
        ),
    )
    .unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).unwrap();
    std::env::set_var("UNRELATED_SECRET", "must-inherit");
    std::env::set_var("ANTHROPIC_BASE_URL", "must-not-leak");
    let key = SecretValue::from_input("test-console-key").unwrap();
    let policy = policy(&script, root.path());
    let invocation = ClaudeInvocation::NewSession {
        session_id: id.clone(),
        model: ClaudeModelAlias::Haiku,
    };
    let cancellation = tokio_util::sync::CancellationToken::new();
    let mut child = ClaudeChild::spawn(&policy, &invocation, id, "hello", key, &cancellation)
        .await
        .unwrap();
    let mut events = Vec::new();
    while let Some(event) = child.next_event().await.unwrap() {
        events.push(event);
    }
    child.finish(&cancellation).await.unwrap();
    std::env::remove_var("UNRELATED_SECRET");
    std::env::remove_var("ANTHROPIC_BASE_URL");
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
    let delta = format!(
        r#"{{"type":"stream_event","session_id":"{}","event":{{"type":"content_block_delta","delta":{{"type":"text_delta","text":"done"}}}}}}"#,
        id_text
    );
    let result = format!(
        r#"{{"type":"result","subtype":"success","is_error":false,"session_id":"{}","result":"done"}}"#,
        id_text
    );
    fs::write(
        &script,
        format!("#!/bin/sh\nread prompt\nprintf '%s\\n' '{init}' '{delta}' '{result}'\n"),
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
    let credentials = Arc::new(FakeCredentialStore::new());
    credentials
        .replace(
            CredentialAccount::AnthropicConsoleApiKey,
            SecretValue::from_input("test-console-key").unwrap(),
        )
        .unwrap();
    let mut service = ClaudeService::new(
        policy(&script, root.path()),
        credentials,
        Arc::clone(&store),
    );
    let prepared = service
        .prepare_turn(id.clone(), ClaudeModelAlias::Sonnet, "hello".to_owned(), 2)
        .await
        .unwrap();
    service.launch_prepared_turn(prepared, 3).await.unwrap();

    let mut finished = false;
    while let Some(event) = service.next_event().await {
        if matches!(
            event,
            ClaudeServiceEvent::TurnFinished {
                outcome: ClaudeTurnOutcome::Completed,
                ..
            }
        ) {
            finished = true;
            break;
        }
    }
    assert!(finished);
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
    let credentials = Arc::new(FakeCredentialStore::new());
    credentials
        .replace(
            CredentialAccount::AnthropicConsoleApiKey,
            SecretValue::from_input("test-console-key").unwrap(),
        )
        .unwrap();
    let mut service = ClaudeService::new(
        policy(root.path().join("missing-claude"), root.path()),
        credentials,
        Arc::clone(&store),
    );
    let prepared = service
        .prepare_turn(id.clone(), ClaudeModelAlias::Sonnet, "hello".to_owned(), 2)
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
    let credentials = Arc::new(FakeCredentialStore::new());
    credentials
        .replace(
            CredentialAccount::AnthropicConsoleApiKey,
            SecretValue::from_input("test-console-key").unwrap(),
        )
        .unwrap();
    let mut service = ClaudeService::new(
        policy(&script, root.path()),
        credentials,
        Arc::clone(&store),
    );
    let prepared = service
        .prepare_turn(id.clone(), ClaudeModelAlias::Sonnet, "hello".to_owned(), 2)
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
    let credentials = Arc::new(FakeCredentialStore::new());
    credentials
        .replace(
            CredentialAccount::AnthropicConsoleApiKey,
            SecretValue::from_input("test-console-key").unwrap(),
        )
        .unwrap();
    let mut service = ClaudeService::new(
        policy(&script, root.path()),
        credentials,
        Arc::clone(&store),
    );
    let prepared = service
        .prepare_turn(id.clone(), ClaudeModelAlias::Sonnet, "hello".to_owned(), 2)
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
    let credentials = Arc::new(FakeCredentialStore::new());
    credentials
        .replace(
            CredentialAccount::AnthropicConsoleApiKey,
            SecretValue::from_input("test-console-key").unwrap(),
        )
        .unwrap();
    let mut service = ClaudeService::new(
        policy(&script, root.path()),
        credentials,
        Arc::clone(&store),
    );
    let prepared = service
        .prepare_turn(id.clone(), ClaudeModelAlias::Sonnet, "hello".to_owned(), 2)
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
    let credentials = Arc::new(FakeCredentialStore::new());
    let mut service = ClaudeService::new(
        policy("/bin/false", root.path()),
        credentials.clone(),
        unverified.clone(),
    );
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
    service = ClaudeService::new(
        policy("/bin/false", root.path()),
        credentials,
        seeded.clone(),
    );
    assert!(service
        .prepare_turn(id.clone(), ClaudeModelAlias::Sonnet, "hello".to_owned(), 2)
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
    service = ClaudeService::new(
        policy("/bin/false", root.path()),
        Arc::new(FakeCredentialStore::new()),
        uncertain_store.clone(),
    );
    let prepared = service
        .prepare_turn(
            uncertain_id.clone(),
            ClaudeModelAlias::Sonnet,
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
    let credentials = Arc::new(FakeCredentialStore::new());
    credentials
        .replace(
            CredentialAccount::AnthropicConsoleApiKey,
            SecretValue::from_input("test-console-key").unwrap(),
        )
        .unwrap();
    let mut service = ClaudeService::new(
        policy(root.path().join("missing-claude"), root.path()),
        credentials,
        Arc::clone(&store),
    );

    let abandoned = service
        .prepare_turn(
            id.clone(),
            ClaudeModelAlias::Sonnet,
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
        .prepare_turn(id.clone(), ClaudeModelAlias::Sonnet, "retry".to_owned(), 4)
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
async fn uncertain_resume_credential_failure_is_correlated_and_reblocks() {
    let root = TempDir::new().unwrap();
    for directory in [root.path().join("home"), root.path().join("cwd")] {
        fs::create_dir(&directory).unwrap();
    }
    let id = session_id("00000000-0000-4000-8000-000000000026");
    let store: Arc<dyn ClaudeSessionStore> =
        Arc::new(FileClaudeSessionStore::new(root.path().join("store")).unwrap());
    let mut session = ClaudeSessionV1::new(
        id.clone(),
        ClaudeModelAlias::Sonnet,
        1,
        "uncertain credential",
    );
    session.lifecycle = ClaudeSessionLifecycle::CreationUncertain;
    store.save_session(&session).unwrap();
    let mut service = ClaudeService::new(
        policy("/bin/false", root.path()),
        Arc::new(FakeCredentialStore::new()),
        Arc::clone(&store),
    );

    let prepared = service
        .prepare_turn(id.clone(), ClaudeModelAlias::Sonnet, "retry".to_owned(), 2)
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
                stage: ClaudeFailureStage::Credential,
                category: ClaudeFailureCategory::InvalidCredential,
            }),
            ..
        })
    ));
    let stored = store.load_session(&id).unwrap();
    assert_eq!(stored.lifecycle, ClaudeSessionLifecycle::CreationUncertain);
    assert_eq!(stored.turns[0].outcome, ClaudeTurnOutcome::Interrupted);
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
    let credentials = Arc::new(FakeCredentialStore::new());
    credentials
        .replace(
            CredentialAccount::AnthropicConsoleApiKey,
            SecretValue::from_input("test-console-key").unwrap(),
        )
        .unwrap();
    let mut service =
        ClaudeService::new(policy(script, root.path()), credentials, Arc::clone(&store));
    let (id, commit) = service
        .create_session(ClaudeModelAlias::Sonnet, 1)
        .await
        .unwrap();
    assert_eq!(commit.source, CommitStatus::Verified);
    let prepared = service
        .prepare_turn(
            id.clone(),
            ClaudeModelAlias::Sonnet,
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
