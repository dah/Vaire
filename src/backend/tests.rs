use super::lifecycle::openrouter_history;
use super::{
    load_notice_message, BackendCoordinator, CompletedItemTracker, Effect,
    MAX_TRACKED_COMPLETED_ITEMS_PER_TURN, MAX_TRACKED_COMPLETED_ITEM_ID_BYTES,
};
use crate::persistence::{
    FilePreferences, LoadNotice, LoadOutcome, PersistenceError, PreferencesPort, PreferencesV4,
};
use crate::platform::{BrowserError, BrowserOpener};
use std::sync::{Arc, Mutex};

#[test]
fn missing_preferences_are_a_quiet_first_run() {
    assert_eq!(load_notice_message(Some(LoadNotice::Missing)), None);
    assert!(load_notice_message(Some(LoadNotice::Corrupt)).is_some());
}

#[test]
fn shared_openrouter_history_restores_failed_partials_with_explicit_status() {
    use crate::app::{TranscriptEntryStatus, TranscriptRole};
    use crate::openrouter::{
        OpenRouterConversationV2, OpenRouterTurnOutcome, OpenRouterTurnRecord,
    };
    use crate::provider::{OpenRouterConversationId, OpenRouterTurnId};

    let mut conversation =
        OpenRouterConversationV2::new(OpenRouterConversationId::new(), 1, "history");
    conversation.turns = vec![
        OpenRouterTurnRecord {
            id: OpenRouterTurnId::new(),
            model_id: "vendor/model".to_owned(),
            user_text: "completed user".to_owned(),
            assistant_text: Some("completed assistant".to_owned()),
            incomplete_assistant_text: None,
            outcome: OpenRouterTurnOutcome::Completed,
        },
        OpenRouterTurnRecord {
            id: OpenRouterTurnId::new(),
            model_id: "vendor/model".to_owned(),
            user_text: "failed user".to_owned(),
            assistant_text: None,
            incomplete_assistant_text: Some("failed partial".to_owned()),
            outcome: OpenRouterTurnOutcome::Failed,
        },
        OpenRouterTurnRecord {
            id: OpenRouterTurnId::new(),
            model_id: "vendor/model".to_owned(),
            user_text: "interrupted user".to_owned(),
            assistant_text: None,
            incomplete_assistant_text: None,
            outcome: OpenRouterTurnOutcome::Interrupted,
        },
    ];

    let history = openrouter_history(&conversation);
    assert_eq!(
        history
            .iter()
            .map(|entry| (entry.role.clone(), entry.status, entry.text.as_str()))
            .collect::<Vec<_>>(),
        vec![
            (
                TranscriptRole::User,
                TranscriptEntryStatus::Normal,
                "completed user",
            ),
            (
                TranscriptRole::Assistant,
                TranscriptEntryStatus::Normal,
                "completed assistant",
            ),
            (
                TranscriptRole::User,
                TranscriptEntryStatus::Normal,
                "failed user",
            ),
            (
                TranscriptRole::Assistant,
                TranscriptEntryStatus::FailedIncomplete,
                "failed partial",
            ),
            (
                TranscriptRole::User,
                TranscriptEntryStatus::Normal,
                "interrupted user",
            ),
        ]
    );
}

#[derive(Clone, Debug)]
struct NoopBrowser;

impl BrowserOpener for NoopBrowser {
    fn open_login_url(&self, _value: &str) -> Result<(), BrowserError> {
        Ok(())
    }
}

#[derive(Clone)]
struct UnverifiedPreferences {
    saved: Arc<Mutex<PreferencesV4>>,
}

impl UnverifiedPreferences {
    fn new() -> Self {
        Self {
            saved: Arc::new(Mutex::new(PreferencesV4::default())),
        }
    }
}

impl PreferencesPort for UnverifiedPreferences {
    fn load(&self) -> Result<LoadOutcome, PersistenceError> {
        Ok(LoadOutcome {
            preferences: self.saved.lock().unwrap().clone(),
            notice: None,
            may_overwrite: true,
            needs_save: false,
        })
    }

    fn save(&self, preferences: &PreferencesV4) -> Result<(), PersistenceError> {
        *self.saved.lock().unwrap() = preferences.clone();
        Ok(())
    }

    fn save_with_commit(
        &self,
        preferences: &PreferencesV4,
    ) -> Result<crate::storage::CommitStatus, PersistenceError> {
        self.save(preferences)?;
        Ok(crate::storage::CommitStatus::CommittedUnverified)
    }
}

#[tokio::test]
async fn future_preferences_remain_byte_for_byte_unchanged_through_all_backend_writes() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("preferences.json");
    let original = b"{\"version\":5,\"future\":{\"preserve\":true}}\n";
    std::fs::write(&path, original).unwrap();
    let preferences = FilePreferences::new(&path);
    let loaded = preferences.load().unwrap();
    assert!(!loaded.may_overwrite);

    let mut backend =
        BackendCoordinator::without_codex(preferences, NoopBrowser, "Codex unavailable".to_owned());
    backend.startup().await.unwrap();
    backend
        .execute_pending(vec![Effect::Persist(PreferencesV4::default())])
        .await
        .unwrap();
    backend.shutdown().await.unwrap();

    assert_eq!(std::fs::read(&path).unwrap(), original);
}

#[tokio::test]
async fn v1_preferences_are_resaved_as_v4_through_the_load_derived_gate() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("preferences.json");
    std::fs::write(
        &path,
        br#"{
          "version": 1,
          "account_scope": {"kind":"chatgpt_email","value":"user@example.com"},
          "thread_id": "thr-v1",
          "model_id": "model-v1",
          "reasoning_effort": "high",
          "thread_account_scopes": {
            "thr-v1": {"kind":"chatgpt_email","value":"user@example.com"}
          }
        }"#,
    )
    .unwrap();
    let mut backend = BackendCoordinator::without_codex(
        FilePreferences::new(&path),
        NoopBrowser,
        "Codex unavailable".to_owned(),
    );

    backend.startup().await.unwrap();

    let migrated: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(migrated["version"], 4);
    assert_eq!(migrated["codex"]["auto_resume_thread_id"], "thr-v1");
}

#[test]
fn active_openrouter_turn_blocks_credential_replacement_and_stale_401_is_inert() {
    let temp = tempfile::tempdir().unwrap();
    let mut backend = BackendCoordinator::without_codex(
        FilePreferences::new(temp.path().join("preferences.json")),
        NoopBrowser,
        "Codex unavailable".to_owned(),
    );
    let conversation_id = crate::provider::OpenRouterConversationId::new();
    let turn_id = crate::provider::OpenRouterTurnId::new();
    backend.state.openrouter.auth = crate::openrouter::OpenRouterAuthStatus::Valid;
    backend.state.turn = crate::app::TurnState::OpenRouterStreaming {
        conversation_id: conversation_id.clone(),
        turn_id: turn_id.clone(),
    };

    let candidate = crate::credentials::SecretValue::from_input("candidate-secret-value").unwrap();
    assert!(backend.accept_openrouter_credential(candidate).is_err());
    assert!(backend
        .state
        .notice
        .as_deref()
        .unwrap()
        .contains("active OpenRouter turn"));

    let stale_conversation = crate::provider::OpenRouterConversationId::new();
    let _ = backend.reduce_openrouter_service_event(
        crate::openrouter::OpenRouterServiceEvent::TurnFinished {
            conversation_id: stale_conversation,
            turn_id,
            outcome: crate::openrouter::OpenRouterTurnOutcome::Failed,
            assistant_text: None,
            incomplete_assistant_text: None,
            usage: None,
            failure: Some(crate::openrouter::OpenRouterFailureCategory::Unauthorized),
            failure_stage: None,
        },
    );
    assert_eq!(
        backend.state.openrouter.auth,
        crate::openrouter::OpenRouterAuthStatus::Valid
    );
}

fn fake_claude_policy(
    root: &std::path::Path,
    executable: std::path::PathBuf,
) -> crate::claude::ClaudeCliPolicy {
    crate::claude::ClaudeCliPolicy::new(executable, root.join("claude-home"), root.to_owned())
}

fn fake_claude_auth_executable(
    root: &std::path::Path,
    name: &str,
    payload: &str,
) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;

    let executable = root.join(name);
    std::fs::write(
        &executable,
        format!("#!/bin/sh\nprintf '%s\\n' '{payload}'\n"),
    )
    .unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755)).unwrap();
    executable
}

fn fake_claude_subscription_then_fail(root: &std::path::Path, name: &str) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;

    let executable = root.join(name);
    std::fs::write(
        &executable,
        r#"#!/bin/sh
case " $* " in
  *" auth status --json "*)
    printf '%s\n' '{"loggedIn":true,"authMethod":"claude.ai","apiProvider":"firstParty"}'
    exit 0
    ;;
esac
exit 1
"#,
    )
    .unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755)).unwrap();
    executable
}

fn fake_claude_subscription_records_effort_then_fail(
    root: &std::path::Path,
    name: &str,
    marker: &std::path::Path,
) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;

    let executable = root.join(name);
    std::fs::write(
        &executable,
        format!(
            r#"#!/bin/sh
case " $* " in
  *" auth status --json "*)
    printf '%s\n' '{{"loggedIn":true,"authMethod":"claude.ai","apiProvider":"firstParty"}}'
    exit 0
    ;;
esac
effort_count=0
effort_value=
while [ "$#" -gt 0 ]; do
  if [ "$1" = "--effort" ]; then
    effort_count=$((effort_count + 1))
    shift
    effort_value=$1
  fi
  shift
done
printf '%s:%s\n' "$effort_count" "$effort_value" > '{}'
exit 1
"#,
            marker.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755)).unwrap();
    executable
}

#[tokio::test]
async fn lazy_claude_send_preserves_effort_through_auth_and_registration_then_settles_terminally() {
    use crate::app::{ClaudeAvailability, ClaudeConversationState, TurnState};
    use crate::claude::{
        ClaudeEffort, ClaudeService, ClaudeSessionStore, ClaudeTurnOutcome, FileClaudeSessionStore,
    };
    use crate::provider::{ClaudeModelAlias, ProviderId};

    let temp = tempfile::tempdir().unwrap();
    let file_store = Arc::new(FileClaudeSessionStore::new(temp.path().join("sessions")).unwrap());
    let store: Arc<dyn ClaudeSessionStore> = file_store.clone();
    let effort_marker = temp.path().join("turn-effort");
    let executable = fake_claude_subscription_records_effort_then_fail(
        temp.path(),
        "subscription-then-fail",
        &effort_marker,
    );
    let policy = fake_claude_policy(temp.path(), executable);
    let service = ClaudeService::new(policy.clone(), store);
    let mut backend = BackendCoordinator::without_codex(
        FilePreferences::new(temp.path().join("preferences.json")),
        NoopBrowser,
        "Codex unavailable".to_owned(),
    )
    .with_claude(crate::backend::ClaudeBackendRuntime::new(service, policy));
    backend.may_persist = true;
    backend.state.active_provider = ProviderId::Claude;
    backend.state.preferences.active_provider = ProviderId::Claude;
    backend.state.preferences.claude.selected_model_alias = Some(ClaudeModelAlias::Default);
    backend.state.preferences.claude.selected_effort = Some(ClaudeEffort::Max);
    backend.state.claude.availability = ClaudeAvailability::Ready;
    backend.state.claude.auth = crate::claude::ClaudeAuthStatus::Subscription;
    backend.state.claude.conversation = ClaudeConversationState::None;
    backend.state.turn = TurnState::Starting;

    backend
        .execute_pending(vec![Effect::SendClaudeMessage {
            text: "hello".to_owned(),
            effort: Some(ClaudeEffort::XHigh),
        }])
        .await
        .unwrap();

    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while !effort_marker.exists() {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("lazy Claude child must record its snapshotted effort");
    assert_eq!(
        std::fs::read_to_string(&effort_marker).unwrap(),
        "1:xhigh\n"
    );

    let session_id = backend
        .state
        .preferences
        .claude
        .auto_resume_session_id
        .clone()
        .expect("lazy send registers and persists a UUID");
    let persisted = FilePreferences::new(temp.path().join("preferences.json"))
        .load()
        .unwrap()
        .preferences;
    assert_eq!(
        persisted.claude.auto_resume_session_id.as_ref(),
        Some(&session_id)
    );
    let session = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            let session = file_store.load_session(&session_id).unwrap();
            if session
                .turns
                .first()
                .is_some_and(|turn| !matches!(turn.outcome, ClaudeTurnOutcome::InProgress))
            {
                break session;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("failed lazy Claude child must settle its turn record");
    assert_eq!(session.turns.len(), 1);
    assert_eq!(
        session.lifecycle,
        crate::claude::ClaudeSessionLifecycle::CreationUncertain
    );
    assert!(matches!(
        session.turns[0].outcome,
        ClaudeTurnOutcome::Interrupted | ClaudeTurnOutcome::Failed
    ));

    backend.state.claude.conversation = ClaudeConversationState::ResumeFailed {
        id: session_id.clone(),
        message: "blocked".to_owned(),
    };
    let _ = backend
        .delete_all_conversations(Vec::new(), Vec::new(), vec![session_id.clone()])
        .await;
    assert!(file_store.load_session(&session_id).is_ok());
}

#[tokio::test]
async fn unverified_claude_pointer_commit_blocks_first_child_launch() {
    use crate::app::{ClaudeAvailability, ClaudeConversationState, TurnState};
    use crate::claude::{ClaudeService, ClaudeSessionStore, FileClaudeSessionStore};
    use crate::provider::{ClaudeModelAlias, ProviderId};

    let temp = tempfile::tempdir().unwrap();
    let file_store = Arc::new(FileClaudeSessionStore::new(temp.path().join("sessions")).unwrap());
    let store: Arc<dyn ClaudeSessionStore> = file_store.clone();
    let executable =
        fake_claude_subscription_then_fail(temp.path(), "subscription-unverified-pointer");
    let policy = fake_claude_policy(temp.path(), executable);
    let service = ClaudeService::new(policy.clone(), store);
    let mut backend = BackendCoordinator::without_codex(
        UnverifiedPreferences::new(),
        NoopBrowser,
        "Codex unavailable".to_owned(),
    )
    .with_claude(crate::backend::ClaudeBackendRuntime::new(service, policy));
    backend.may_persist = true;
    backend.state.active_provider = ProviderId::Claude;
    backend.state.preferences.active_provider = ProviderId::Claude;
    backend.state.preferences.claude.selected_model_alias = Some(ClaudeModelAlias::Default);
    backend.state.claude.availability = ClaudeAvailability::Ready;
    backend.state.claude.auth = crate::claude::ClaudeAuthStatus::Subscription;
    backend.state.claude.conversation = ClaudeConversationState::None;
    backend.state.turn = TurnState::Starting;

    backend
        .execute_pending(vec![Effect::SendClaudeMessage {
            text: "hello".to_owned(),
            effort: None,
        }])
        .await
        .unwrap();

    let sessions = file_store.list_sessions().unwrap();
    assert_eq!(sessions.len(), 1);
    let session = file_store.load_session(&sessions[0].session_id).unwrap();
    assert!(session.turns.is_empty(), "the child was never prepared");
    assert!(matches!(backend.state.turn, TurnState::Failed { .. }));
    assert!(matches!(
        backend.state.claude.conversation,
        ClaudeConversationState::CreationUncertain { .. }
    ));
}

#[tokio::test]
async fn explicit_new_with_unverified_pointer_commit_tracks_the_new_uuid_as_uncertain() {
    use crate::app::{
        ClaudeAvailability, ClaudeConversationState, Intent, TranscriptEntry,
        TranscriptEntryStatus, TranscriptRole, TurnState,
    };
    use crate::claude::{ClaudeService, ClaudeSessionStore, FileClaudeSessionStore};
    use crate::provider::{ClaudeModelAlias, ModelKey, ProviderId};

    let temp = tempfile::tempdir().unwrap();
    let preferences = UnverifiedPreferences::new();
    let file_store = Arc::new(FileClaudeSessionStore::new(temp.path().join("sessions")).unwrap());
    let store: Arc<dyn ClaudeSessionStore> = file_store.clone();
    let policy = fake_claude_policy(temp.path(), std::path::PathBuf::from("/usr/bin/false"));
    let service = ClaudeService::new(policy.clone(), store);
    let mut backend = BackendCoordinator::without_codex(
        preferences.clone(),
        NoopBrowser,
        "Codex unavailable".to_owned(),
    )
    .with_claude(crate::backend::ClaudeBackendRuntime::new(service, policy));
    let old_session: crate::provider::ClaudeSessionId =
        "00000000-0000-4000-8000-000000000001".parse().unwrap();
    backend.may_persist = true;
    backend.state.active_provider = ProviderId::Claude;
    backend.state.preferences.active_provider = ProviderId::Claude;
    backend.state.preferences.claude.selected_model_alias = Some(ClaudeModelAlias::Default);
    backend.state.preferences.claude.auto_resume_session_id = Some(old_session.clone());
    backend.state.selected_model =
        Some(ModelKey::claude(ClaudeModelAlias::Default.as_str()).unwrap());
    backend.state.claude.availability = ClaudeAvailability::Ready;
    backend.state.claude.auth = crate::claude::ClaudeAuthStatus::Subscription;
    backend.state.claude.conversation = ClaudeConversationState::Ready { id: old_session };
    backend.state.turn = TurnState::Idle;
    backend.state.transcript.push(TranscriptEntry {
        provider: ProviderId::Claude,
        role: TranscriptRole::Assistant,
        status: TranscriptEntryStatus::Normal,
        text: "old history".to_owned(),
        item_id: None,
        turn_id: None,
    });

    backend.handle_intent(Intent::NewThread).await.unwrap();

    let sessions = file_store.list_sessions().unwrap();
    assert_eq!(sessions.len(), 1);
    let new_session = sessions[0].session_id.clone();
    assert!(matches!(
        backend.state.claude.conversation,
        ClaudeConversationState::CreationUncertain { ref id, .. } if id == &new_session
    ));
    assert_eq!(
        backend
            .state
            .preferences
            .claude
            .auto_resume_session_id
            .as_ref(),
        Some(&new_session)
    );
    assert_eq!(
        preferences
            .load()
            .unwrap()
            .preferences
            .claude
            .auto_resume_session_id
            .as_ref(),
        Some(&new_session)
    );
    assert!(backend.state.transcript.is_empty());
    assert!(!backend.state.pending_new_claude_session);
    assert!(backend
        .state
        .notice
        .as_deref()
        .is_some_and(|notice| notice.contains("session creation is uncertain")
            && !notice.contains("current conversation was preserved")));
}

#[tokio::test]
async fn claude_startup_uses_native_cli_subscription_state_without_a_credential_store() {
    use crate::claude::{ClaudeAuthStatus, ClaudeService, FileClaudeSessionStore};

    let temp = tempfile::tempdir().unwrap();
    let executable = fake_claude_auth_executable(
        temp.path(),
        "fake-claude-subscription",
        r#"{"loggedIn":true,"authMethod":"claude.ai","apiProvider":"firstParty","subscriptionType":"max"}"#,
    );
    let store = Arc::new(FileClaudeSessionStore::new(temp.path().join("store")).unwrap());
    let policy = fake_claude_policy(temp.path(), executable);
    let service = ClaudeService::new(policy.clone(), store);
    let mut backend = BackendCoordinator::without_codex(
        FilePreferences::new(temp.path().join("preferences.json")),
        NoopBrowser,
        "Codex unavailable".to_owned(),
    )
    .with_claude(crate::backend::ClaudeBackendRuntime::new(service, policy));

    backend.startup().await.unwrap();

    assert_eq!(backend.state.claude.auth, ClaudeAuthStatus::Subscription);
    assert_eq!(
        backend.state.claude.auth_operation,
        crate::app::ClaudeAuthOperation::Idle
    );
}

#[tokio::test]
async fn claude_startup_requires_explicit_resume_when_account_scope_is_unavailable() {
    use crate::app::ClaudeConversationState;
    use crate::claude::{ClaudeAuthStatus, ClaudeService, FileClaudeSessionStore};
    use crate::provider::{ClaudeModelAlias, ClaudeSessionId, ProviderId};

    let temp = tempfile::tempdir().unwrap();
    let executable = fake_claude_auth_executable(
        temp.path(),
        "fake-claude-unscoped-subscription",
        r#"{"loggedIn":true,"authMethod":"claude.ai","apiProvider":"firstParty"}"#,
    );
    let session_id: ClaudeSessionId = "00000000-0000-4000-8000-000000000077".parse().unwrap();
    let preferences_path = temp.path().join("preferences.json");
    let preferences = FilePreferences::new(&preferences_path);
    let mut saved = PreferencesV4 {
        active_provider: ProviderId::Claude,
        ..PreferencesV4::default()
    };
    saved.claude.selected_model_alias = Some(ClaudeModelAlias::Default);
    saved.claude.auto_resume_session_id = Some(session_id.clone());
    preferences.save(&saved).unwrap();
    let store = Arc::new(FileClaudeSessionStore::new(temp.path().join("store")).unwrap());
    let policy = fake_claude_policy(temp.path(), executable);
    let service = ClaudeService::new(policy.clone(), store);
    let mut backend = BackendCoordinator::without_codex(
        FilePreferences::new(preferences_path),
        NoopBrowser,
        "Codex unavailable".to_owned(),
    )
    .with_claude(crate::backend::ClaudeBackendRuntime::new(service, policy));

    backend.startup().await.unwrap();

    assert_eq!(backend.state.claude.auth, ClaudeAuthStatus::Subscription);
    assert!(matches!(
        &backend.state.claude.conversation,
        ClaudeConversationState::ResumeFailed { id, message }
            if id == &session_id
                && message.contains("stable account identity")
                && message.contains("/resume")
                && message.contains("/new")
    ));
    assert_eq!(
        backend
            .state
            .preferences
            .claude
            .auto_resume_session_id
            .as_ref(),
        Some(&session_id)
    );
}

#[tokio::test]
async fn claude_refresh_demotes_a_ready_unscoped_session_after_auth_may_have_changed() {
    use crate::app::{ClaudeAvailability, ClaudeConversationState};
    use crate::claude::{ClaudeAuthStatus, ClaudeService, FileClaudeSessionStore};
    use crate::provider::{ClaudeModelAlias, ClaudeSessionId, ProviderId};

    let temp = tempfile::tempdir().unwrap();
    let executable = fake_claude_auth_executable(
        temp.path(),
        "fake-claude-refreshed-subscription",
        r#"{"loggedIn":true,"authMethod":"claude.ai","apiProvider":"firstParty"}"#,
    );
    let store = Arc::new(FileClaudeSessionStore::new(temp.path().join("store")).unwrap());
    let policy = fake_claude_policy(temp.path(), executable);
    let service = ClaudeService::new(policy.clone(), store);
    let mut backend = BackendCoordinator::without_codex(
        FilePreferences::new(temp.path().join("preferences.json")),
        NoopBrowser,
        "Codex unavailable".to_owned(),
    )
    .with_claude(crate::backend::ClaudeBackendRuntime::new(service, policy));
    let session_id: ClaudeSessionId = "00000000-0000-4000-8000-000000000078".parse().unwrap();
    backend.state.active_provider = ProviderId::Claude;
    backend.state.preferences.active_provider = ProviderId::Claude;
    backend.state.preferences.claude.selected_model_alias = Some(ClaudeModelAlias::Default);
    backend.state.preferences.claude.auto_resume_session_id = Some(session_id.clone());
    backend.state.claude.availability = ClaudeAvailability::Ready;
    backend.state.claude.auth = ClaudeAuthStatus::Subscription;
    backend.state.claude.conversation = ClaudeConversationState::Ready {
        id: session_id.clone(),
    };

    backend
        .execute_pending(vec![Effect::RefreshClaude])
        .await
        .unwrap();

    assert_eq!(backend.state.claude.auth, ClaudeAuthStatus::Subscription);
    assert!(matches!(
        &backend.state.claude.conversation,
        ClaudeConversationState::ResumeFailed { id, message }
            if id == &session_id
                && message.contains("stable account identity")
                && message.contains("/resume")
    ));
}

#[tokio::test]
async fn claude_startup_rejects_a_native_cli_api_key_session_as_unsupported() {
    use crate::claude::{ClaudeAuthStatus, ClaudeService, FileClaudeSessionStore};

    let temp = tempfile::tempdir().unwrap();
    let executable = fake_claude_auth_executable(
        temp.path(),
        "fake-claude-api-key",
        r#"{"loggedIn":true,"authMethod":"api_key","apiProvider":"firstParty"}"#,
    );
    let store = Arc::new(FileClaudeSessionStore::new(temp.path().join("store")).unwrap());
    let policy = fake_claude_policy(temp.path(), executable);
    let service = ClaudeService::new(policy.clone(), store);
    let mut backend = BackendCoordinator::without_codex(
        FilePreferences::new(temp.path().join("preferences.json")),
        NoopBrowser,
        "Codex unavailable".to_owned(),
    )
    .with_claude(crate::backend::ClaudeBackendRuntime::new(service, policy));

    backend.startup().await.unwrap();

    assert_eq!(backend.state.claude.auth, ClaudeAuthStatus::Unsupported);
}

#[tokio::test]
async fn claude_login_is_pending_until_terminal_completion_and_subscription_recheck() {
    use crate::app::ClaudeAuthOperation;
    use crate::claude::{
        ClaudeAuthAction, ClaudeAuthStatus, ClaudeService, FileClaudeSessionStore,
    };

    let temp = tempfile::tempdir().unwrap();
    let executable = fake_claude_auth_executable(
        temp.path(),
        "fake-claude-login",
        r#"{"loggedIn":true,"authMethod":"claude.ai","apiProvider":"firstParty","subscriptionType":"pro"}"#,
    );
    let store = Arc::new(FileClaudeSessionStore::new(temp.path().join("store")).unwrap());
    let policy = fake_claude_policy(temp.path(), executable);
    let service = ClaudeService::new(policy.clone(), store);
    let mut backend = BackendCoordinator::without_codex(
        FilePreferences::new(temp.path().join("preferences.json")),
        NoopBrowser,
        "Codex unavailable".to_owned(),
    )
    .with_claude(crate::backend::ClaudeBackendRuntime::new(service, policy));
    backend.state.claude.auth = ClaudeAuthStatus::SignedOut;

    backend
        .execute_pending(vec![Effect::LoginClaude])
        .await
        .unwrap();
    let request = backend
        .state
        .pending_claude_auth_request()
        .copied()
        .expect("login is handed to the foreground terminal owner");
    assert_eq!(request.action, ClaudeAuthAction::Login);
    assert!(matches!(
        backend.state.claude.auth_operation,
        ClaudeAuthOperation::AwaitingTerminal { request: pending } if pending == request
    ));
    assert_eq!(backend.state.claude.auth, ClaudeAuthStatus::Unverified);

    let effects = backend.complete_claude_auth(request, Ok(())).await;

    assert!(effects.is_empty());
    assert_eq!(backend.state.claude.auth, ClaudeAuthStatus::Subscription);
    assert_eq!(
        backend.state.claude.auth_operation,
        ClaudeAuthOperation::Idle
    );
}

#[tokio::test]
async fn claude_logout_is_pending_until_terminal_completion_and_signed_out_recheck() {
    use crate::app::ClaudeAuthOperation;
    use crate::claude::{
        ClaudeAuthAction, ClaudeAuthStatus, ClaudeService, FileClaudeSessionStore,
    };

    let temp = tempfile::tempdir().unwrap();
    let executable = fake_claude_auth_executable(
        temp.path(),
        "fake-claude-logout",
        r#"{"loggedIn":false,"authMethod":"none","apiProvider":"firstParty"}"#,
    );
    let store = Arc::new(FileClaudeSessionStore::new(temp.path().join("store")).unwrap());
    let policy = fake_claude_policy(temp.path(), executable);
    let service = ClaudeService::new(policy.clone(), store);
    let mut backend = BackendCoordinator::without_codex(
        FilePreferences::new(temp.path().join("preferences.json")),
        NoopBrowser,
        "Codex unavailable".to_owned(),
    )
    .with_claude(crate::backend::ClaudeBackendRuntime::new(service, policy));
    backend.state.claude.auth = ClaudeAuthStatus::Subscription;

    backend
        .execute_pending(vec![Effect::LogoutClaude])
        .await
        .unwrap();
    let request = backend
        .state
        .pending_claude_auth_request()
        .copied()
        .expect("logout is handed to the foreground terminal owner");
    assert_eq!(request.action, ClaudeAuthAction::Logout);
    assert_eq!(backend.state.claude.auth, ClaudeAuthStatus::Unverified);

    let effects = backend.complete_claude_auth(request, Ok(())).await;

    assert!(effects.is_empty());
    assert_eq!(backend.state.claude.auth, ClaudeAuthStatus::SignedOut);
    assert_eq!(
        backend.state.claude.auth_operation,
        ClaudeAuthOperation::Idle
    );
}

#[tokio::test]
async fn claude_logout_status_failure_keeps_auth_unverified() {
    use crate::app::ClaudeAuthOperation;
    use crate::claude::{ClaudeAuthStatus, ClaudeService, FileClaudeSessionStore};

    let temp = tempfile::tempdir().unwrap();
    let executable =
        fake_claude_auth_executable(temp.path(), "fake-claude-uncertain-logout", "not-json");
    let store = Arc::new(FileClaudeSessionStore::new(temp.path().join("store")).unwrap());
    let policy = fake_claude_policy(temp.path(), executable);
    let service = ClaudeService::new(policy.clone(), store);
    let mut backend = BackendCoordinator::without_codex(
        FilePreferences::new(temp.path().join("preferences.json")),
        NoopBrowser,
        "Codex unavailable".to_owned(),
    )
    .with_claude(crate::backend::ClaudeBackendRuntime::new(service, policy));
    backend.state.claude.auth = ClaudeAuthStatus::Subscription;

    backend
        .execute_pending(vec![Effect::LogoutClaude])
        .await
        .unwrap();
    let request = backend
        .state
        .pending_claude_auth_request()
        .copied()
        .expect("logout is handed to the foreground terminal owner");

    let effects = backend.complete_claude_auth(request, Ok(())).await;

    assert!(effects.is_empty());
    assert_eq!(backend.state.claude.auth, ClaudeAuthStatus::Unverified);
    assert_eq!(
        backend.state.claude.auth_operation,
        ClaudeAuthOperation::Idle
    );
    assert!(backend
        .state
        .notice
        .as_deref()
        .is_some_and(|notice| notice.contains("status check failed")));
}

#[tokio::test]
async fn failed_claude_refresh_invalidates_stale_subscription_authority() {
    use crate::app::ClaudeAuthOperation;
    use crate::claude::{ClaudeAuthStatus, ClaudeService, FileClaudeSessionStore};

    let temp = tempfile::tempdir().unwrap();
    let executable =
        fake_claude_auth_executable(temp.path(), "fake-claude-failed-refresh", "not-json");
    let store = Arc::new(FileClaudeSessionStore::new(temp.path().join("store")).unwrap());
    let policy = fake_claude_policy(temp.path(), executable);
    let service = ClaudeService::new(policy.clone(), store);
    let mut backend = BackendCoordinator::without_codex(
        FilePreferences::new(temp.path().join("preferences.json")),
        NoopBrowser,
        "Codex unavailable".to_owned(),
    )
    .with_claude(crate::backend::ClaudeBackendRuntime::new(service, policy));
    backend.state.claude.auth = ClaudeAuthStatus::Subscription;

    backend
        .execute_pending(vec![Effect::RefreshClaude])
        .await
        .unwrap();

    assert_eq!(backend.state.claude.auth, ClaudeAuthStatus::Unverified);
    assert_eq!(
        backend.state.claude.auth_operation,
        ClaudeAuthOperation::Idle
    );
    assert!(backend
        .state
        .notice
        .as_deref()
        .is_some_and(|notice| notice.contains("status check failed")));
}

#[tokio::test]
async fn claude_send_revalidates_system_subscription_before_launch() {
    use crate::app::{ClaudeAvailability, ClaudeConversationState, TurnState};
    use crate::claude::{ClaudeAuthStatus, ClaudeService, FileClaudeSessionStore};
    use crate::provider::{ClaudeModelAlias, ProviderId};

    let temp = tempfile::tempdir().unwrap();
    let executable = fake_claude_auth_executable(
        temp.path(),
        "fake-claude-externally-signed-out",
        r#"{"loggedIn":false,"authMethod":"none","apiProvider":"firstParty"}"#,
    );
    let store = Arc::new(FileClaudeSessionStore::new(temp.path().join("store")).unwrap());
    let policy = fake_claude_policy(temp.path(), executable);
    let service = ClaudeService::new(policy.clone(), store);
    let mut backend = BackendCoordinator::without_codex(
        FilePreferences::new(temp.path().join("preferences.json")),
        NoopBrowser,
        "Codex unavailable".to_owned(),
    )
    .with_claude(crate::backend::ClaudeBackendRuntime::new(service, policy));
    let session_id = "00000000-0000-4000-8000-000000000088".parse().unwrap();
    backend.state.active_provider = ProviderId::Claude;
    backend.state.preferences.active_provider = ProviderId::Claude;
    backend.state.preferences.claude.selected_model_alias = Some(ClaudeModelAlias::Default);
    backend.state.claude.availability = ClaudeAvailability::Ready;
    backend.state.claude.auth = ClaudeAuthStatus::Subscription;
    backend.state.claude.conversation = ClaudeConversationState::Ready { id: session_id };
    backend.state.turn = TurnState::Starting;

    backend
        .execute_pending(vec![Effect::SendClaudeMessage {
            text: "must not launch".to_owned(),
            effort: Some(crate::claude::ClaudeEffort::Low),
        }])
        .await
        .unwrap();

    assert_eq!(backend.state.claude.auth, ClaudeAuthStatus::SignedOut);
    assert!(matches!(backend.state.turn, TurnState::Failed { .. }));
    assert!(backend
        .state
        .notice
        .as_deref()
        .is_some_and(|notice| notice.contains("Auth") || notice.contains("auth")));
}

#[test]
fn service_failed_partial_reaches_the_app_with_failed_incomplete_status() {
    let temp = tempfile::tempdir().unwrap();
    let mut backend = BackendCoordinator::without_codex(
        FilePreferences::new(temp.path().join("preferences.json")),
        NoopBrowser,
        "Codex unavailable".to_owned(),
    );
    let conversation_id = crate::provider::OpenRouterConversationId::new();
    let turn_id = crate::provider::OpenRouterTurnId::new();
    backend.state.active_provider = crate::provider::ProviderId::OpenRouter;
    backend.state.turn = crate::app::TurnState::OpenRouterStreaming {
        conversation_id: conversation_id.clone(),
        turn_id: turn_id.clone(),
    };

    let _ = backend.reduce_openrouter_service_event(
        crate::openrouter::OpenRouterServiceEvent::TurnFinished {
            conversation_id,
            turn_id,
            outcome: crate::openrouter::OpenRouterTurnOutcome::Failed,
            assistant_text: None,
            incomplete_assistant_text: Some("durable partial".to_owned()),
            usage: None,
            failure: Some(crate::openrouter::OpenRouterFailureCategory::InvalidResponse),
            failure_stage: Some(crate::openrouter::OpenRouterStreamStage::CompletionShape),
        },
    );

    assert_eq!(backend.state.transcript.len(), 1);
    assert_eq!(backend.state.transcript[0].text, "durable partial");
    assert_eq!(
        backend.state.transcript[0].status,
        crate::app::TranscriptEntryStatus::FailedIncomplete
    );
    assert!(matches!(
        &backend.state.turn,
        crate::app::TurnState::Failed { message, .. }
            if message
                == "OpenRouter turn failed (InvalidResponse); stream stage CompletionShape"
    ));
}

#[tokio::test]
async fn stale_catalog_result_cannot_restore_a_pending_openrouter_conversation() {
    let temp = tempfile::tempdir().unwrap();
    let mut backend = BackendCoordinator::without_codex(
        FilePreferences::new(temp.path().join("preferences.json")),
        NoopBrowser,
        "Codex unavailable".to_owned(),
    );
    let conversation_id = crate::provider::OpenRouterConversationId::new();
    backend.state.openrouter.credential_validation =
        crate::app::OpenRouterCredentialValidation::Refreshing { operation_id: 9 };
    backend.pending_openrouter_auto_resume = Some(super::PendingOpenRouterAutoResume {
        operation_id: 9,
        conversation_id,
        model_id: Some("vendor/model".to_owned()),
    });

    let effects = backend
        .process_openrouter_service_event(
            crate::openrouter::OpenRouterServiceEvent::CatalogLoaded {
                operation_id: 9,
                catalog: vec![crate::openrouter::OpenRouterModel {
                    id: "vendor/model".to_owned(),
                    name: None,
                    context_length: None,
                }],
            },
        )
        .await;

    assert!(effects.is_empty());
    assert!(backend.pending_openrouter_auto_resume.is_none());
    assert_eq!(
        backend.state.openrouter.conversation,
        crate::app::OpenRouterConversationState::None
    );
}

#[test]
fn completed_items_reset_for_every_local_turn_even_when_server_ids_repeat() {
    let mut tracker = CompletedItemTracker::default();
    tracker.begin_turn("thread", "turn-one");
    tracker.record("thread", "turn-one", "reused-item");
    tracker.observe_turn("thread", "turn-one");
    assert!(tracker.should_ignore("thread", "turn-one", "reused-item"));

    tracker.begin_turn("thread", "turn-one");
    assert!(!tracker.should_ignore("thread", "turn-one", "reused-item"));

    tracker.record("thread", "turn-one", "reused-item");
    tracker.begin_turn("thread", "turn-two");
    assert!(!tracker.should_ignore("thread", "turn-two", "reused-item"));
    assert!(!tracker.should_ignore("thread", "turn-one", "reused-item"));
}

#[test]
fn completed_item_tracking_saturates_closed_at_count_and_byte_bounds() {
    let mut tracker = CompletedItemTracker::default();
    tracker.begin_turn("thread", "count-bound");
    for index in 0..=MAX_TRACKED_COMPLETED_ITEMS_PER_TURN {
        tracker.record("thread", "count-bound", &format!("item-{index}"));
    }
    assert_eq!(tracker.ids.len(), MAX_TRACKED_COMPLETED_ITEMS_PER_TURN);
    assert!(tracker.should_ignore("thread", "count-bound", "untracked-late-item"));
    assert!(!tracker.should_ignore("other-thread", "count-bound", "untracked-late-item"));

    tracker.begin_turn("thread", "byte-bound");
    let at_limit = "x".repeat(MAX_TRACKED_COMPLETED_ITEM_ID_BYTES);
    tracker.record("thread", "byte-bound", &at_limit);
    tracker.record("thread", "byte-bound", "over-limit");
    assert_eq!(tracker.ids.len(), 1);
    assert_eq!(tracker.id_bytes, MAX_TRACKED_COMPLETED_ITEM_ID_BYTES);
    assert!(tracker.should_ignore("thread", "byte-bound", "another-untracked-item"));

    tracker.begin_turn("thread", "fresh-turn");
    assert!(!tracker.should_ignore("thread", "fresh-turn", "new-item"));
}
