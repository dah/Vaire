use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use agentharness::app::{
    Intent, ThreadDeleteConfirmation, ThreadPickerPhase, ThreadState, TranscriptRole,
};
use agentharness::backend::BackendCoordinator;
use agentharness::codex::safety::{ConversationSafetyPolicy, IsolationPaths};
use agentharness::codex::session::SessionService;
use agentharness::codex::transport::{AppServerTransport, ProcessSpec};
use agentharness::persistence::{
    AccountScope, LoadOutcome, PersistenceError, PreferencesPort, PreferencesV1,
};
use agentharness::platform::{BrowserError, BrowserOpener};
use serde_json::json;
use tempfile::tempdir;

const INITIALIZED: &str = r#"{"id":1,"result":{"codexHome":"/private/tmp/codex","platformFamily":"unix","platformOs":"macos","userAgent":"fake/0.144.6"}}"#;
const ACCOUNT: &str = r#"{"id":2,"result":{"account":{"type":"chatgpt","email":"user@example.com","planType":"plus"},"requiresOpenaiAuth":true}}"#;
const MODELS: &str = r#"{"id":3,"result":{"data":[{"id":"m1","displayName":"Model One","isDefault":true,"defaultReasoningEffort":"high","supportedReasoningEfforts":[{"reasoningEffort":"high","description":"deep"}],"hidden":false}],"nextCursor":null}}"#;

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
    SessionService::new(transport, paths, ConversationSafetyPolicy)
}

#[derive(Clone)]
struct MemoryPreferences(Arc<Mutex<PreferencesV1>>);

impl MemoryPreferences {
    fn new(value: PreferencesV1) -> Self {
        Self(Arc::new(Mutex::new(value)))
    }

    fn value(&self) -> PreferencesV1 {
        self.0.lock().unwrap().clone()
    }
}

impl PreferencesPort for MemoryPreferences {
    fn load(&self) -> Result<LoadOutcome, PersistenceError> {
        Ok(LoadOutcome {
            preferences: self.value(),
            notice: None,
            may_overwrite: true,
        })
    }

    fn save(&self, preferences: &PreferencesV1) -> Result<(), PersistenceError> {
        *self.0.lock().unwrap() = preferences.clone();
        Ok(())
    }
}

#[derive(Clone, Debug, Default)]
struct NoopBrowser;

impl BrowserOpener for NoopBrowser {
    fn open_login_url(&self, _value: &str) -> Result<(), BrowserError> {
        Ok(())
    }
}

fn preferences(thread_id: Option<&str>) -> PreferencesV1 {
    let scope = AccountScope::from_chatgpt_email("user@example.com").unwrap();
    PreferencesV1 {
        account_scope: Some(scope.clone()),
        thread_id: thread_id.map(str::to_owned),
        model_id: Some("m1".to_owned()),
        reasoning_effort: Some("high".to_owned()),
        thread_account_scopes: [
            "thr-old",
            "thr-active",
            "thr-old-a",
            "thr-old-b",
            "thr-old-c",
        ]
        .into_iter()
        .map(|id| (id.to_owned(), scope.clone()))
        .collect(),
        ..PreferencesV1::default()
    }
}

fn listed_thread(
    id: &str,
    name: Option<&str>,
    preview: &str,
    cwd: &Path,
    updated_at: i64,
) -> String {
    json!({
        "id": id,
        "name": name,
        "preview": preview,
        "createdAt": updated_at.saturating_sub(10),
        "updatedAt": updated_at,
        "cwd": cwd,
        "ephemeral": false,
        "cliVersion": "0.144.6",
        "modelProvider": "openai",
        "sessionId": id,
        "source": "appServer",
        "status": {"type": "idle"},
        "turns": []
    })
    .to_string()
}

#[tokio::test]
async fn new_eagerly_creates_and_persists_without_deleting_the_previous_thread() {
    let temp = tempdir().unwrap();
    let body = format!(
        r#"
IFS= read -r initialize
printf '%s\n' '{INITIALIZED}'
IFS= read -r initialized
IFS= read -r account
printf '%s\n' '{ACCOUNT}'
IFS= read -r models
printf '%s\n' '{MODELS}'
IFS= read -r resume
printf '%s\n' '{{"id":4,"result":{{"thread":{{"id":"thr-old","turns":[]}}}}}}'
IFS= read -r read_thread
printf '%s\n' '{{"id":5,"result":{{"thread":{{"id":"thr-old","turns":[{{"id":"old-turn","status":"completed","items":[{{"id":"u","type":"userMessage","content":[{{"type":"text","text":"old question"}}]}},{{"id":"a","type":"agentMessage","text":"old answer"}}]}}]}}}}}}'
IFS= read -r new_thread
case "$new_thread" in *'"method":"thread/start"'*) ;; *) exit 41 ;; esac
printf '%s\n' '{{"id":6,"result":{{"thread":{{"id":"thr-new","turns":[]}}}}}}'
IFS= read -r hold
"#
    );
    let saved = MemoryPreferences::new(preferences(Some("thr-old")));
    let mut backend = BackendCoordinator::new(
        session(temp.path(), &body).await,
        saved.clone(),
        NoopBrowser,
    );
    backend.startup().await.unwrap();
    assert_eq!(backend.state().transcript.len(), 2);

    backend.handle_intent(Intent::NewThread).await.unwrap();
    assert!(matches!(&backend.state().thread, ThreadState::Ready { id } if id == "thr-new"));
    assert!(backend.state().transcript.is_empty());
    assert_eq!(saved.value().thread_id.as_deref(), Some("thr-new"));
    backend.shutdown().await.unwrap();
}

#[tokio::test]
async fn paginated_picker_switches_threads_and_reports_partial_bulk_deletion() {
    let temp = tempdir().unwrap();
    let cwd = temp.path().join("runtime/conversation");
    let active = listed_thread("thr-active", Some("Current"), "current", &cwd, 40);
    let old_a = listed_thread("thr-old-a", None, "Question A", &cwd, 30);
    let old_b = listed_thread("thr-old-b", Some("Old B"), "question b", &cwd, 20);
    let old_c = listed_thread("thr-old-c", Some("Old C"), "question c", &cwd, 10);
    let body = format!(
        r#"
IFS= read -r initialize
printf '%s\n' '{INITIALIZED}'
IFS= read -r initialized
IFS= read -r account
printf '%s\n' '{ACCOUNT}'
IFS= read -r models
printf '%s\n' '{MODELS}'
IFS= read -r resume_active
printf '%s\n' '{{"id":4,"result":{{"thread":{{"id":"thr-active","turns":[]}}}}}}'
IFS= read -r read_active
printf '%s\n' '{{"id":5,"result":{{"thread":{{"id":"thr-active","turns":[]}}}}}}'
IFS= read -r list_page_one
case "$list_page_one" in *'"method":"thread/list"'*'"cursor":null'*'"sortKey":"updated_at"'*'"sourceKinds":["appServer"]'*) ;; *) exit 51 ;; esac
printf '%s\n' '{{"id":6,"result":{{"data":[{active},{old_a}],"nextCursor":"page-2"}}}}'
IFS= read -r list_page_two
case "$list_page_two" in *'"cursor":"page-2"'*) ;; *) exit 52 ;; esac
printf '%s\n' '{{"id":7,"result":{{"data":[{old_a},{old_b},{old_c}],"nextCursor":null}}}}'
IFS= read -r resume_old_a
case "$resume_old_a" in *'"method":"thread/resume"'*'"threadId":"thr-old-a"'*) ;; *) exit 53 ;; esac
printf '%s\n' '{{"id":8,"result":{{"thread":{{"id":"thr-old-a","turns":[]}}}}}}'
IFS= read -r read_old_a
printf '%s\n' '{{"id":9,"result":{{"thread":{{"id":"thr-old-a","turns":[{{"id":"restored-turn","status":"completed","items":[{{"id":"a","type":"agentMessage","text":"restored A"}}]}}]}}}}}}'
IFS= read -r list_again
printf '%s\n' '{{"id":10,"result":{{"data":[{old_a},{old_b},{old_c},{active}],"nextCursor":null}}}}'
IFS= read -r delete_old_b
case "$delete_old_b" in *'"method":"thread/delete"'*'"threadId":"thr-old-b"'*) ;; *) exit 54 ;; esac
printf '%s\n' '{{"id":11,"result":{{}}}}'
IFS= read -r delete_old_c
case "$delete_old_c" in *'"threadId":"thr-old-c"'*) ;; *) exit 55 ;; esac
printf '%s\n' '{{"id":12,"result":{{}}}}'
IFS= read -r delete_prior_active
case "$delete_prior_active" in *'"threadId":"thr-active"'*) ;; *) exit 56 ;; esac
printf '%s\n' '{{"id":13,"error":{{"code":-32010,"message":"simulated delete failure"}}}}'
IFS= read -r hold
"#
    );
    let saved = MemoryPreferences::new(preferences(Some("thr-active")));
    let mut backend = BackendCoordinator::new(
        session(temp.path(), &body).await,
        saved.clone(),
        NoopBrowser,
    );
    backend.startup().await.unwrap();

    backend.handle_intent(Intent::Resume).await.unwrap();
    let picker = backend.state().thread_picker.as_ref().unwrap();
    assert!(
        matches!(picker.phase, ThreadPickerPhase::Ready),
        "unexpected picker state: {picker:?}"
    );
    assert_eq!(
        picker.threads.len(),
        4,
        "pagination should deduplicate thr-old-a"
    );
    assert_eq!(picker.selected, 0, "the active thread should be selected");

    backend
        .handle_intent(Intent::ThreadPickerMoveDown)
        .await
        .unwrap();
    backend
        .handle_intent(Intent::ThreadPickerSelect)
        .await
        .unwrap();
    assert!(matches!(&backend.state().thread, ThreadState::Ready { id } if id == "thr-old-a"));
    assert_eq!(saved.value().thread_id.as_deref(), Some("thr-old-a"));
    assert_eq!(backend.state().transcript.len(), 1);
    assert_eq!(
        backend.state().transcript[0].role,
        TranscriptRole::Assistant
    );
    assert_eq!(backend.state().transcript[0].text, "restored A");

    backend.handle_intent(Intent::Resume).await.unwrap();
    backend
        .handle_intent(Intent::ThreadPickerMoveDown)
        .await
        .unwrap();
    backend
        .handle_intent(Intent::ThreadPickerRequestDelete)
        .await
        .unwrap();
    assert!(matches!(
        backend
            .state()
            .thread_picker
            .as_ref()
            .and_then(|picker| picker.confirmation.as_ref()),
        Some(ThreadDeleteConfirmation::Selected { target }) if target.id == "thr-old-b"
    ));
    backend
        .handle_intent(Intent::ThreadPickerCancelDelete)
        .await
        .unwrap();
    assert!(backend
        .state()
        .thread_picker
        .as_ref()
        .unwrap()
        .confirmation
        .is_none());
    assert!(backend
        .state()
        .thread_picker
        .as_ref()
        .unwrap()
        .threads
        .iter()
        .any(|thread| thread.id == "thr-old-b"));
    backend
        .handle_intent(Intent::ThreadPickerRequestDelete)
        .await
        .unwrap();
    backend
        .handle_intent(Intent::ThreadPickerConfirmDelete)
        .await
        .unwrap();
    assert!(!backend
        .state()
        .thread_picker
        .as_ref()
        .unwrap()
        .threads
        .iter()
        .any(|thread| thread.id == "thr-old-b"));

    backend
        .handle_intent(Intent::ThreadPickerRequestClearInactive)
        .await
        .unwrap();
    let targets = backend
        .state()
        .thread_picker
        .as_ref()
        .unwrap()
        .confirmation
        .as_ref()
        .unwrap()
        .targets();
    assert_eq!(
        targets
            .iter()
            .map(|target| target.id.as_str())
            .collect::<Vec<_>>(),
        vec!["thr-old-c", "thr-active"]
    );
    backend
        .handle_intent(Intent::ThreadPickerConfirmDelete)
        .await
        .unwrap();
    let picker = backend.state().thread_picker.as_ref().unwrap();
    assert_eq!(
        picker
            .threads
            .iter()
            .map(|thread| thread.id.as_str())
            .collect::<Vec<_>>(),
        vec!["thr-old-a", "thr-active"]
    );
    assert!(picker
        .message
        .as_deref()
        .unwrap()
        .contains("Deleted 1 of 2"));
    assert!(picker
        .message
        .as_deref()
        .unwrap()
        .contains("app-server returned error -32010"));
    assert_eq!(saved.value().thread_id.as_deref(), Some("thr-old-a"));
    backend.shutdown().await.unwrap();
}

#[tokio::test]
async fn malformed_thread_list_is_recoverable_and_preserves_saved_state() {
    let temp = tempdir().unwrap();
    let body = format!(
        r#"
IFS= read -r initialize
printf '%s\n' '{INITIALIZED}'
IFS= read -r initialized
IFS= read -r account
printf '%s\n' '{ACCOUNT}'
IFS= read -r models
printf '%s\n' '{MODELS}'
IFS= read -r list_threads
printf '%s\n' '{{"id":4,"result":{{"data":[{{"id":"broken","preview":"missing required fields"}}],"nextCursor":null}}}}'
IFS= read -r hold
"#
    );
    let saved = MemoryPreferences::new(preferences(None));
    let mut backend = BackendCoordinator::new(
        session(temp.path(), &body).await,
        saved.clone(),
        NoopBrowser,
    );
    backend.startup().await.unwrap();
    backend.handle_intent(Intent::Resume).await.unwrap();
    let picker = backend.state().thread_picker.as_ref().unwrap();
    assert!(matches!(picker.phase, ThreadPickerPhase::Failed));
    assert!(picker
        .message
        .as_deref()
        .unwrap()
        .contains("tested protocol"));
    assert_eq!(saved.value().thread_id, None);
    assert!(matches!(backend.state().thread, ThreadState::None));
    backend.shutdown().await.unwrap();
}
