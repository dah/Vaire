use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use agentharness::app::{
    AuthState, ConnectionState, Intent, ThinkingKind, ThreadState, TranscriptRole, TurnState,
};
use agentharness::backend::BackendCoordinator;
use agentharness::codex::safety::{FullAccessPolicy, IsolationPaths};
use agentharness::codex::session::SessionService;
use agentharness::codex::transport::{AppServerTransport, ProcessSpec, RequestTimeouts};
use agentharness::persistence::{
    AccountScope, LoadOutcome, PersistenceError, PreferencesPort, PreferencesV1,
};
use agentharness::platform::{validate_login_url, BrowserError, BrowserOpener};
use tempfile::tempdir;

fn script(root: &Path, body: &str) -> PathBuf {
    let path = root.join("fake-app-server");
    fs::write(&path, format!("#!/bin/sh\nset -eu\n{body}\n")).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
    path
}

async fn session(root: &Path, body: &str) -> SessionService {
    let paths = IsolationPaths::prepare(root.join("runtime")).unwrap();
    let executable = script(root, body);
    let transport = AppServerTransport::spawn(ProcessSpec {
        executable,
        args: Vec::new(),
        cwd: root.to_owned(),
        env: Vec::new(),
    })
    .await
    .unwrap();
    SessionService::new(transport, paths, FullAccessPolicy)
}

#[derive(Clone)]
struct MemoryPreferences {
    value: Arc<Mutex<PreferencesV1>>,
}

impl MemoryPreferences {
    fn new(value: PreferencesV1) -> Self {
        Self {
            value: Arc::new(Mutex::new(value)),
        }
    }
    fn value(&self) -> PreferencesV1 {
        self.value.lock().unwrap().clone()
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
        *self.value.lock().unwrap() = preferences.clone();
        Ok(())
    }
}

struct FailingPreferences;

impl PreferencesPort for FailingPreferences {
    fn load(&self) -> Result<LoadOutcome, PersistenceError> {
        Ok(LoadOutcome {
            preferences: PreferencesV1::default(),
            notice: None,
            may_overwrite: true,
        })
    }

    fn save(&self, _preferences: &PreferencesV1) -> Result<(), PersistenceError> {
        Err(std::io::Error::other("simulated persistence failure").into())
    }
}

#[derive(Clone, Default)]
struct RecordingBrowser {
    urls: Arc<Mutex<Vec<String>>>,
}

impl BrowserOpener for RecordingBrowser {
    fn open_login_url(&self, value: &str) -> Result<(), BrowserError> {
        validate_login_url(value)?;
        self.urls.lock().unwrap().push(value.to_owned());
        Ok(())
    }
}

#[derive(Clone, Debug, Default)]
struct FailingBrowser;

impl BrowserOpener for FailingBrowser {
    fn open_login_url(&self, _value: &str) -> Result<(), BrowserError> {
        Err(BrowserError::Open("browser unavailable".to_owned()))
    }
}

const INITIALIZED: &str = r#"{"id":1,"result":{"codexHome":"/private/tmp/codex","platformFamily":"unix","platformOs":"macos","userAgent":"fake/0.144.6"}}"#;
const MODEL_PAGE: &str = r#"{"id":3,"result":{"data":[{"id":"m1","displayName":"Model One","isDefault":true,"defaultReasoningEffort":"high","supportedReasoningEfforts":[{"reasoningEffort":"low","description":"fast"},{"reasoningEffort":"high","description":"deep"}],"hidden":false}],"nextCursor":null}}"#;

#[tokio::test]
async fn first_run_creates_one_thread_and_reconciles_streaming_final_text() {
    let temp = tempdir().unwrap();
    let body = format!(
        r#"
IFS= read -r initialize
printf '%s\n' '{INITIALIZED}'
IFS= read -r initialized
IFS= read -r account
printf '%s\n' '{{"id":2,"result":{{"account":{{"type":"chatgpt","email":"user@example.com","planType":"plus"}},"requiresOpenaiAuth":true}}}}'
IFS= read -r model_page_one
printf '%s\n' '{{"id":3,"result":{{"data":[{{"id":"m1","displayName":"Model One","isDefault":true,"defaultReasoningEffort":"high","supportedReasoningEfforts":[{{"reasoningEffort":"high","description":"deep"}}],"hidden":false}}],"nextCursor":"page-2"}}}}'
IFS= read -r model_page_two
printf '%s\n' '{{"id":4,"result":{{"data":[{{"id":"m1","displayName":"duplicate","isDefault":false,"defaultReasoningEffort":"high","supportedReasoningEfforts":[{{"reasoningEffort":"high","description":"deep"}}],"hidden":false}},{{"id":"m2","displayName":"Model Two","isDefault":false,"defaultReasoningEffort":"low","supportedReasoningEfforts":[{{"reasoningEffort":"low","description":"fast"}}],"hidden":false}}],"nextCursor":null}}}}'
IFS= read -r thread_start
printf '%s\n' '{{"id":5,"result":{{"thread":{{"id":"thr-new","turns":[]}}}}}}'
IFS= read -r turn_start
case "$turn_start" in
  *'"summary":"auto"'*) ;;
  *) exit 89 ;;
esac
printf '%s\n' '{{"id":6,"result":{{"turn":{{"id":"turn-new","items":[],"status":"inProgress"}}}}}}'
printf '%s\n' '{{"method":"future/notification","params":{{"ignored":true}}}}'
printf '%s\n' '{{"method":"item/agentMessage/delta","params":{{"threadId":"stale","turnId":"turn-new","itemId":"item-a","delta":"wrong"}}}}'
printf '%s\n' '{{"method":"item/reasoning/summaryTextDelta","params":{{"threadId":"stale","turnId":"turn-new","itemId":"why","summaryIndex":0,"delta":"wrong"}}}}'
printf '%s\n' '{{"method":"item/reasoning/summaryPartAdded","params":{{"threadId":"thr-new","turnId":"turn-new","itemId":"why","summaryIndex":0}}}}'
printf '%s\n' '{{"method":"item/reasoning/summaryTextDelta","params":{{"threadId":"thr-new","turnId":"turn-new","itemId":"why","summaryIndex":0,"delta":"checking"}}}}'
printf '%s\n' '{{"method":"item/reasoning/textDelta","params":{{"threadId":"thr-new","turnId":"turn-new","itemId":"why","contentIndex":0,"delta":"emitted"}}}}'
printf '%s\n' '{{"method":"item/completed","params":{{"threadId":"thr-new","turnId":"turn-new","completedAtMs":1,"item":{{"id":"why","type":"reasoning","summary":["checking facts"],"content":["emitted detail"]}}}}}}'
printf '%s\n' '{{"method":"item/agentMessage/delta","params":{{"threadId":"thr-new","turnId":"turn-new","itemId":"item-a","delta":"hé"}}}}'
printf '%s\n' '{{"method":"item/completed","params":{{"threadId":"thr-new","turnId":"turn-new","completedAtMs":1,"item":{{"id":"item-a","type":"agentMessage","text":"héllo"}}}}}}'
printf '%s\n' '{{"method":"turn/completed","params":{{"threadId":"thr-new","turn":{{"id":"turn-new","items":[],"status":"completed"}}}}}}'
IFS= read -r hold
"#
    );
    let session = session(temp.path(), &body).await;
    let preferences = MemoryPreferences::new(PreferencesV1::default());
    let saved = preferences.clone();
    let mut backend = BackendCoordinator::new(session, preferences, RecordingBrowser::default());
    backend.startup().await.unwrap();
    assert_eq!(
        backend.state().models.len(),
        2,
        "pagination should deduplicate m1"
    );
    backend
        .handle_intent(Intent::SendMessage("hello".to_owned()))
        .await
        .unwrap();
    for _ in 0..10 {
        assert!(backend.pump_event().await.unwrap());
    }
    assert!(matches!(backend.state().thread, ThreadState::Ready { ref id } if id == "thr-new"));
    assert!(matches!(backend.state().turn, TurnState::Completed { .. }));
    let assistant = backend
        .state()
        .transcript
        .iter()
        .find(|entry| entry.role == TranscriptRole::Assistant)
        .unwrap();
    assert_eq!(assistant.text, "héllo");
    assert_eq!(backend.state().thinking.entries.len(), 2);
    assert_eq!(
        backend.state().thinking.entries[0].kind,
        ThinkingKind::Summary
    );
    assert_eq!(backend.state().thinking.entries[0].text, "checking facts");
    assert_eq!(
        backend.state().thinking.entries[1].kind,
        ThinkingKind::EmittedText
    );
    assert_eq!(backend.state().thinking.entries[1].text, "emitted detail");
    assert_eq!(saved.value().thread_id.as_deref(), Some("thr-new"));
    backend.shutdown().await.unwrap();
}

#[tokio::test]
async fn same_account_resume_restores_history_and_stale_resume_never_replaces_id() {
    let temp = tempdir().unwrap();
    let body = format!(
        r#"
IFS= read -r initialize
printf '%s\n' '{INITIALIZED}'
IFS= read -r initialized
IFS= read -r account
printf '%s\n' '{{"id":2,"result":{{"account":{{"type":"chatgpt","email":"user@example.com","planType":"plus"}},"requiresOpenaiAuth":true}}}}'
IFS= read -r models
printf '%s\n' '{MODEL_PAGE}'
IFS= read -r resume
printf '%s\n' '{{"id":4,"result":{{"thread":{{"id":"thr-saved","turns":[]}}}}}}'
IFS= read -r read_thread
printf '%s\n' '{{"id":5,"result":{{"thread":{{"id":"thr-saved","turns":[{{"id":"old-turn","status":"completed","items":[{{"id":"u","type":"userMessage","content":[{{"type":"text","text":"old question"}}]}},{{"id":"a","type":"agentMessage","text":"old answer"}}]}}]}}}}}}'
IFS= read -r hold
"#
    );
    let preferences = MemoryPreferences::new(PreferencesV1 {
        account_scope: AccountScope::from_chatgpt_email("user@example.com"),
        thread_id: Some("thr-saved".to_owned()),
        model_id: Some("m1".to_owned()),
        reasoning_effort: Some("high".to_owned()),
        ..PreferencesV1::default()
    });
    let mut backend = BackendCoordinator::new(
        session(temp.path(), &body).await,
        preferences,
        RecordingBrowser::default(),
    );
    backend.startup().await.unwrap();
    assert_eq!(backend.state().transcript.len(), 2);
    assert_eq!(backend.state().transcript[1].text, "old answer");
    backend.shutdown().await.unwrap();

    let stale_temp = tempdir().unwrap();
    let stale_body = format!(
        r#"
IFS= read -r initialize
printf '%s\n' '{INITIALIZED}'
IFS= read -r initialized
IFS= read -r account
printf '%s\n' '{{"id":2,"result":{{"account":{{"type":"chatgpt","email":"user@example.com","planType":"plus"}},"requiresOpenaiAuth":true}}}}'
IFS= read -r models
printf '%s\n' '{MODEL_PAGE}'
IFS= read -r resume
printf '%s\n' '{{"id":4,"error":{{"code":-32001,"message":"stale thread details must not leak"}}}}'
IFS= read -r hold
"#
    );
    let stale_preferences = MemoryPreferences::new(PreferencesV1 {
        account_scope: AccountScope::from_chatgpt_email("user@example.com"),
        thread_id: Some("thr-stale".to_owned()),
        model_id: Some("m1".to_owned()),
        reasoning_effort: Some("high".to_owned()),
        ..PreferencesV1::default()
    });
    let saved = stale_preferences.clone();
    let mut stale = BackendCoordinator::new(
        session(stale_temp.path(), &stale_body).await,
        stale_preferences,
        RecordingBrowser::default(),
    );
    stale.startup().await.unwrap();
    assert!(
        matches!(stale.state().thread, ThreadState::ResumeFailed { ref id, .. } if id == "thr-stale")
    );
    stale
        .handle_intent(Intent::SendMessage("must stay local".to_owned()))
        .await
        .unwrap();
    assert_eq!(saved.value().thread_id.as_deref(), Some("thr-stale"));
    stale.shutdown().await.unwrap();

    let mismatch_temp = tempdir().unwrap();
    let mismatch_body = format!(
        r#"
IFS= read -r initialize
printf '%s\n' '{INITIALIZED}'
IFS= read -r initialized
IFS= read -r account
printf '%s\n' '{{"id":2,"result":{{"account":{{"type":"chatgpt","email":"new@example.com","planType":"plus"}},"requiresOpenaiAuth":true}}}}'
IFS= read -r models
printf '%s\n' '{MODEL_PAGE}'
IFS= read -r hold
"#
    );
    let mismatch_preferences = MemoryPreferences::new(PreferencesV1 {
        account_scope: AccountScope::from_chatgpt_email("old@example.com"),
        thread_id: Some("thr-other-account".to_owned()),
        model_id: Some("m1".to_owned()),
        reasoning_effort: Some("high".to_owned()),
        ..PreferencesV1::default()
    });
    let mut mismatch = BackendCoordinator::new(
        session(mismatch_temp.path(), &mismatch_body).await,
        mismatch_preferences,
        RecordingBrowser::default(),
    );
    mismatch.startup().await.unwrap();
    assert!(
        matches!(mismatch.state().thread, ThreadState::AccountMismatch { ref id } if id == "thr-other-account")
    );
    mismatch.shutdown().await.unwrap();
}

#[tokio::test]
async fn chatgpt_browser_login_completion_and_logout_are_protocol_driven() {
    let temp = tempdir().unwrap();
    let body = format!(
        r#"
IFS= read -r initialize
printf '%s\n' '{INITIALIZED}'
IFS= read -r initialized
IFS= read -r account
printf '%s\n' '{{"id":2,"result":{{"account":null,"requiresOpenaiAuth":true}}}}'
IFS= read -r models
printf '%s\n' '{MODEL_PAGE}'
IFS= read -r login
printf '%s\n' '{{"id":4,"result":{{"type":"chatgpt","loginId":"login-1","authUrl":"https://auth.openai.com/oauth?state=opaque"}}}}'
printf '%s\n' '{{"method":"account/login/completed","params":{{"loginId":"login-other","success":true,"error":null}}}}'
printf '%s\n' '{{"method":"account/login/completed","params":{{"loginId":"login-1","success":true,"error":null}}}}'
IFS= read -r refreshed_account
printf '%s\n' '{{"id":5,"result":{{"account":{{"type":"chatgpt","email":"user@example.com","planType":"plus"}},"requiresOpenaiAuth":true}}}}'
printf '%s\n' '{{"method":"account/login/completed","params":{{"loginId":"login-1","success":true,"error":null}}}}'
IFS= read -r logout
printf '%s\n' '{{"id":6,"result":{{}}}}'
IFS= read -r hold
"#
    );
    let browser = RecordingBrowser::default();
    let opened = browser.urls.clone();
    let mut backend = BackendCoordinator::new(
        session(temp.path(), &body).await,
        MemoryPreferences::new(PreferencesV1::default()),
        browser,
    );
    backend.startup().await.unwrap();
    assert!(matches!(backend.state().auth, AuthState::SignedOut));
    backend.handle_intent(Intent::Login).await.unwrap();
    assert_eq!(opened.lock().unwrap().len(), 1);
    assert!(backend
        .state()
        .notice
        .as_deref()
        .is_some_and(|notice| notice.contains("/login device")));
    backend.pump_event().await.unwrap();
    assert!(matches!(backend.state().auth, AuthState::SigningIn { .. }));
    backend.pump_event().await.unwrap();
    assert_eq!(
        backend.state().auth,
        AuthState::SignedIn {
            scope: AccountScope::from_chatgpt_email("user@example.com"),
        }
    );
    assert_eq!(
        backend.state().notice.as_deref(),
        Some("Signed in to ChatGPT")
    );
    backend.pump_event().await.unwrap();
    assert!(matches!(backend.state().auth, AuthState::SignedIn { .. }));
    assert_eq!(
        backend.state().notice.as_deref(),
        Some("Signed in to ChatGPT")
    );
    backend.handle_intent(Intent::Logout).await.unwrap();
    assert!(matches!(backend.state().auth, AuthState::SignedOut));
    backend.shutdown().await.unwrap();
}

#[tokio::test]
async fn idless_login_failure_allows_retry_and_pending_login_can_be_cancelled() {
    let temp = tempdir().unwrap();
    let body = format!(
        r#"
IFS= read -r initialize
printf '%s\n' '{INITIALIZED}'
IFS= read -r initialized
IFS= read -r account
printf '%s\n' '{{"id":2,"result":{{"account":null,"requiresOpenaiAuth":true}}}}'
IFS= read -r models
printf '%s\n' '{MODEL_PAGE}'
IFS= read -r first_login
printf '%s\n' '{{"id":4,"result":{{"type":"chatgpt","loginId":"login-failed","authUrl":"https://auth.openai.com/oauth?state=failed"}}}}'
printf '%s\n' '{{"method":"account/login/completed","params":{{"success":false,"error":"browser sign-in failed"}}}}'
IFS= read -r second_login
printf '%s\n' '{{"id":5,"result":{{"type":"chatgpt","loginId":"login-cancel","authUrl":"https://auth.openai.com/oauth?state=cancel"}}}}'
IFS= read -r cancel_login
case "$cancel_login" in
  *'"method":"account/login/cancel"'*'"loginId":"login-cancel"'*) ;;
  *) exit 90 ;;
esac
printf '%s\n' '{{"id":6,"result":{{"status":"canceled"}}}}'
IFS= read -r refreshed_account
printf '%s\n' '{{"id":7,"result":{{"account":null,"requiresOpenaiAuth":true}}}}'
IFS= read -r third_login
printf '%s\n' '{{"id":8,"result":{{"type":"chatgpt","loginId":"login-retry","authUrl":"https://auth.openai.com/oauth?state=retry"}}}}'
IFS= read -r hold
"#
    );
    let browser = RecordingBrowser::default();
    let opened = browser.urls.clone();
    let mut backend = BackendCoordinator::new(
        session(temp.path(), &body).await,
        MemoryPreferences::new(PreferencesV1::default()),
        browser,
    );

    backend.startup().await.unwrap();
    backend.handle_intent(Intent::Login).await.unwrap();
    assert!(matches!(
        backend.state().auth,
        AuthState::SigningIn { ref login_id } if login_id == "login-failed"
    ));

    backend.pump_event().await.unwrap();
    assert!(matches!(backend.state().auth, AuthState::SignedOut));
    assert_eq!(
        backend.state().notice.as_deref(),
        Some("browser sign-in failed")
    );

    backend.handle_intent(Intent::Login).await.unwrap();
    assert!(matches!(
        backend.state().auth,
        AuthState::SigningIn { ref login_id } if login_id == "login-cancel"
    ));
    backend.handle_intent(Intent::Logout).await.unwrap();
    assert!(matches!(backend.state().auth, AuthState::SignedOut));

    backend.handle_intent(Intent::Login).await.unwrap();
    assert!(matches!(
        backend.state().auth,
        AuthState::SigningIn { ref login_id } if login_id == "login-retry"
    ));
    assert_eq!(opened.lock().unwrap().len(), 3);
    backend.shutdown().await.unwrap();
}

#[tokio::test]
async fn device_code_login_opens_verification_page_displays_code_and_can_be_cancelled() {
    let temp = tempdir().unwrap();
    let body = format!(
        r#"
IFS= read -r initialize
printf '%s\n' '{INITIALIZED}'
IFS= read -r initialized
IFS= read -r account
printf '%s\n' '{{"id":2,"result":{{"account":null,"requiresOpenaiAuth":true}}}}'
IFS= read -r models
printf '%s\n' '{MODEL_PAGE}'
IFS= read -r device_login
case "$device_login" in
  *'"method":"account/login/start"'*'"type":"chatgptDeviceCode"'*) ;;
  *) exit 91 ;;
esac
printf '%s\n' '{{"id":4,"result":{{"type":"chatgptDeviceCode","loginId":"login-device","userCode":"ABCD-EFGH","verificationUrl":"https://auth.openai.com/codex/device"}}}}'
IFS= read -r cancel_login
case "$cancel_login" in
  *'"method":"account/login/cancel"'*'"loginId":"login-device"'*) ;;
  *) exit 92 ;;
esac
printf '%s\n' '{{"id":5,"result":{{"status":"canceled"}}}}'
IFS= read -r refreshed_account
printf '%s\n' '{{"id":6,"result":{{"account":null,"requiresOpenaiAuth":true}}}}'
printf '%s\n' '{{"method":"account/login/completed","params":{{"loginId":"login-device","success":false,"error":"cancelled"}}}}'
IFS= read -r hold
"#
    );
    let browser = RecordingBrowser::default();
    let opened = browser.urls.clone();
    let mut backend = BackendCoordinator::new(
        session(temp.path(), &body).await,
        MemoryPreferences::new(PreferencesV1::default()),
        browser,
    );

    backend.startup().await.unwrap();
    backend.handle_intent(Intent::LoginDevice).await.unwrap();
    assert!(matches!(
        backend.state().auth,
        AuthState::SigningIn { ref login_id } if login_id == "login-device"
    ));
    assert_eq!(
        opened.lock().unwrap().as_slice(),
        ["https://auth.openai.com/codex/device"]
    );
    assert!(backend
        .state()
        .notice
        .as_deref()
        .is_some_and(|notice| notice.contains("ABCD-EFGH")));

    backend.handle_intent(Intent::Logout).await.unwrap();
    assert!(matches!(backend.state().auth, AuthState::SignedOut));
    assert!(backend
        .state()
        .notice
        .as_deref()
        .is_some_and(|notice| notice.contains("cancelled")));
    backend.pump_event().await.unwrap();
    assert!(matches!(backend.state().auth, AuthState::SignedOut));
    assert!(backend
        .state()
        .notice
        .as_deref()
        .is_some_and(|notice| notice.contains("cancelled")));
    backend.shutdown().await.unwrap();
}

#[tokio::test]
async fn browser_open_failure_retains_login_id_until_explicit_cancellation() {
    let temp = tempdir().unwrap();
    let body = format!(
        r#"
IFS= read -r initialize
printf '%s\n' '{INITIALIZED}'
IFS= read -r initialized
IFS= read -r account
printf '%s\n' '{{"id":2,"result":{{"account":null,"requiresOpenaiAuth":true}}}}'
IFS= read -r models
printf '%s\n' '{MODEL_PAGE}'
IFS= read -r login
printf '%s\n' '{{"id":4,"result":{{"type":"chatgpt","loginId":"login-browser-failed","authUrl":"https://auth.openai.com/oauth?state=opaque"}}}}'
IFS= read -r cancel_login
case "$cancel_login" in
  *'"method":"account/login/cancel"'*'"loginId":"login-browser-failed"'*) ;;
  *) exit 93 ;;
esac
printf '%s\n' '{{"id":5,"result":{{"status":"canceled"}}}}'
IFS= read -r refreshed_account
printf '%s\n' '{{"id":6,"result":{{"account":null,"requiresOpenaiAuth":true}}}}'
IFS= read -r hold
"#
    );
    let mut backend = BackendCoordinator::new(
        session(temp.path(), &body).await,
        MemoryPreferences::new(PreferencesV1::default()),
        FailingBrowser,
    );

    backend.startup().await.unwrap();
    backend.handle_intent(Intent::Login).await.unwrap();
    assert!(matches!(
        backend.state().auth,
        AuthState::SigningIn { ref login_id } if login_id == "login-browser-failed"
    ));
    assert!(backend
        .state()
        .notice
        .as_deref()
        .is_some_and(|notice| notice.contains("/logout")));

    backend.handle_intent(Intent::Logout).await.unwrap();
    assert!(matches!(backend.state().auth, AuthState::SignedOut));
    backend.shutdown().await.unwrap();
}

#[tokio::test]
async fn cancellation_failure_keeps_pending_login_retryable() {
    let temp = tempdir().unwrap();
    let body = format!(
        r#"
IFS= read -r initialize
printf '%s\n' '{INITIALIZED}'
IFS= read -r initialized
IFS= read -r account
printf '%s\n' '{{"id":2,"result":{{"account":null,"requiresOpenaiAuth":true}}}}'
IFS= read -r models
printf '%s\n' '{MODEL_PAGE}'
IFS= read -r login
printf '%s\n' '{{"id":4,"result":{{"type":"chatgpt","loginId":"login-cancel-retry","authUrl":"https://auth.openai.com/oauth?state=opaque"}}}}'
IFS= read -r cancel_login
printf '%s\n' '{{"id":5,"error":{{"code":-32000,"message":"temporary cancel failure"}}}}'
IFS= read -r hold
"#
    );
    let mut backend = BackendCoordinator::new(
        session(temp.path(), &body).await,
        MemoryPreferences::new(PreferencesV1::default()),
        RecordingBrowser::default(),
    );

    backend.startup().await.unwrap();
    backend.handle_intent(Intent::Login).await.unwrap();
    backend.handle_intent(Intent::Logout).await.unwrap();
    assert!(matches!(
        backend.state().auth,
        AuthState::SigningIn { ref login_id } if login_id == "login-cancel-retry"
    ));
    assert!(backend
        .state()
        .notice
        .as_deref()
        .is_some_and(|notice| notice.contains("/logout to retry")));
    backend.shutdown().await.unwrap();
}

#[tokio::test]
async fn interrupt_and_terminal_error_keep_one_turn_active_at_a_time() {
    let temp = tempdir().unwrap();
    let body = format!(
        r#"
IFS= read -r initialize
printf '%s\n' '{INITIALIZED}'
IFS= read -r initialized
IFS= read -r account
printf '%s\n' '{{"id":2,"result":{{"account":{{"type":"chatgpt","email":"user@example.com","planType":"plus"}},"requiresOpenaiAuth":true}}}}'
IFS= read -r models
printf '%s\n' '{MODEL_PAGE}'
IFS= read -r thread_start
printf '%s\n' '{{"id":4,"result":{{"thread":{{"id":"thr-one","turns":[]}}}}}}'
IFS= read -r first_turn
printf '%s\n' '{{"id":5,"result":{{"turn":{{"id":"turn-interrupt","items":[],"status":"inProgress"}}}}}}'
IFS= read -r interrupt
printf '%s\n' '{{"id":6,"result":{{}}}}'
printf '%s\n' '{{"method":"turn/completed","params":{{"threadId":"thr-one","turn":{{"id":"turn-interrupt","items":[],"status":"interrupted"}}}}}}'
IFS= read -r second_turn
printf '%s\n' '{{"id":7,"result":{{"turn":{{"id":"turn-fail","items":[],"status":"inProgress"}}}}}}'
printf '%s\n' '{{"method":"error","params":{{"threadId":"thr-one","turnId":"turn-fail","willRetry":false,"error":{{"message":"service failed"}}}}}}'
IFS= read -r hold
"#
    );
    let mut backend = BackendCoordinator::new(
        session(temp.path(), &body).await,
        MemoryPreferences::new(PreferencesV1::default()),
        RecordingBrowser::default(),
    );
    backend.startup().await.unwrap();
    backend
        .handle_intent(Intent::SendMessage("first".to_owned()))
        .await
        .unwrap();
    backend
        .handle_intent(Intent::SendMessage("blocked".to_owned()))
        .await
        .unwrap();
    assert_eq!(
        backend
            .state()
            .transcript
            .iter()
            .filter(|entry| entry.role == TranscriptRole::User)
            .count(),
        1
    );
    backend.handle_intent(Intent::Interrupt).await.unwrap();
    backend.pump_event().await.unwrap();
    assert!(matches!(
        backend.state().turn,
        TurnState::Interrupted { .. }
    ));

    backend
        .handle_intent(Intent::SendMessage("second".to_owned()))
        .await
        .unwrap();
    backend.pump_event().await.unwrap();
    assert!(matches!(backend.state().turn, TurnState::Failed { .. }));
    backend.shutdown().await.unwrap();
}

#[tokio::test]
async fn shutdown_reaps_the_child_even_when_persistence_fails() {
    let temp = tempdir().unwrap();
    let body = format!(
        r#"
IFS= read -r initialize
printf '%s\n' '{INITIALIZED}'
IFS= read -r initialized
IFS= read -r account
printf '%s\n' '{{"id":2,"result":{{"account":null,"requiresOpenaiAuth":true}}}}'
IFS= read -r models
printf '%s\n' '{MODEL_PAGE}'
IFS= read -r hold
"#
    );
    let paths = IsolationPaths::prepare(temp.path().join("runtime")).unwrap();
    let executable = script(temp.path(), &body);
    let transport = AppServerTransport::spawn(ProcessSpec {
        executable,
        args: Vec::new(),
        cwd: temp.path().to_owned(),
        env: Vec::new(),
    })
    .await
    .unwrap();
    let pid = transport.child_pid();
    let session = SessionService::new(transport, paths, FullAccessPolicy);
    let mut backend =
        BackendCoordinator::new(session, FailingPreferences, RecordingBrowser::default());
    backend.startup().await.unwrap();

    assert!(backend.shutdown().await.is_err());
    assert!(
        !std::process::Command::new("/bin/kill")
            .args(["-0", &pid.to_string()])
            .output()
            .unwrap()
            .status
            .success(),
        "app-server child was not reaped after a persistence failure"
    );
}

#[tokio::test]
async fn ambiguous_thread_start_timeout_blocks_a_replacement_prompt() {
    let temp = tempdir().unwrap();
    let body = format!(
        r#"
IFS= read -r initialize
printf '%s\n' '{INITIALIZED}'
IFS= read -r initialized
IFS= read -r account
printf '%s\n' '{{"id":2,"result":{{"account":{{"type":"chatgpt","email":"user@example.com"}},"requiresOpenaiAuth":true}}}}'
IFS= read -r models
printf '%s\n' '{MODEL_PAGE}'
IFS= read -r thread_start
sleep 1
"#
    );
    let paths = IsolationPaths::prepare(temp.path().join("runtime")).unwrap();
    let executable = script(temp.path(), &body);
    let mut transport = AppServerTransport::spawn(ProcessSpec {
        executable,
        args: Vec::new(),
        cwd: temp.path().to_owned(),
        env: Vec::new(),
    })
    .await
    .unwrap();
    transport.set_timeouts(RequestTimeouts {
        thread: Duration::from_millis(20),
        ..RequestTimeouts::default()
    });
    let session = SessionService::new(transport, paths, FullAccessPolicy);
    let mut backend = BackendCoordinator::new(
        session,
        MemoryPreferences::new(PreferencesV1::default()),
        RecordingBrowser::default(),
    );
    backend.startup().await.unwrap();

    backend
        .handle_intent(Intent::SendMessage("first".to_owned()))
        .await
        .unwrap();
    assert!(matches!(
        backend.state().connection,
        ConnectionState::Failed(_)
    ));
    backend
        .handle_intent(Intent::SendMessage("replacement".to_owned()))
        .await
        .unwrap();
    assert_eq!(
        backend
            .state()
            .transcript
            .iter()
            .filter(|entry| entry.role == TranscriptRole::User)
            .count(),
        1
    );
    backend.shutdown().await.unwrap();
}
