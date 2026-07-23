pub(super) use std::path::Path;
pub(super) use std::sync::{Arc, Mutex};

pub(super) use agentharness::app::{
    ConnectionState, Effect, Intent, ThreadDeleteConfirmation, ThreadPickerPhase, ThreadState,
    TranscriptRole, TurnState,
};
pub(super) use agentharness::backend::BackendCoordinator;
pub(super) use agentharness::codex::safety::{FullAccessPolicy, IsolationPaths};
pub(super) use agentharness::codex::session::SessionService;
pub(super) use agentharness::codex::transport::{AppServerTransport, ProcessSpec};
pub(super) use agentharness::persistence::{
    AccountScope, CodexPreferencesV2, LoadOutcome, PersistenceError, PreferencesPort, PreferencesV2,
};
pub(super) use agentharness::platform::{BrowserError, BrowserOpener};
pub(super) use serde_json::json;
pub(super) use tempfile::tempdir;

pub(super) const INITIALIZED: &str = r#"{"id":1,"result":{"codexHome":"/private/tmp/codex","platformFamily":"unix","platformOs":"macos","userAgent":"fake/0.144.6"}}"#;
pub(super) const ACCOUNT: &str = r#"{"id":2,"result":{"account":{"type":"chatgpt","email":"user@example.com","planType":"plus"},"requiresOpenaiAuth":true}}"#;
pub(super) const MODELS: &str = r#"{"id":3,"result":{"data":[{"id":"m1","displayName":"Model One","isDefault":true,"defaultReasoningEffort":"high","supportedReasoningEfforts":[{"reasoningEffort":"high","description":"deep"}],"hidden":false}],"nextCursor":null}}"#;

pub(super) use crate::shared_support::script;

pub(super) async fn session(root: &Path, body: &str) -> SessionService {
    let paths = IsolationPaths::prepare(root.join("runtime")).unwrap();
    let transport = AppServerTransport::spawn(ProcessSpec {
        executable: script(root, body),
        args: Vec::new(),
        cwd: root.to_owned(),
        env: Vec::new(),
    })
    .await
    .unwrap();
    SessionService::new(transport, paths, FullAccessPolicy)
}

#[derive(Clone)]
pub(super) struct MemoryPreferences(Arc<Mutex<PreferencesV2>>);

impl MemoryPreferences {
    pub(super) fn new(value: PreferencesV2) -> Self {
        Self(Arc::new(Mutex::new(value)))
    }

    pub(super) fn value(&self) -> PreferencesV2 {
        self.0.lock().unwrap().clone()
    }
}

impl PreferencesPort for MemoryPreferences {
    fn load(&self) -> Result<LoadOutcome, PersistenceError> {
        Ok(LoadOutcome {
            preferences: self.value(),
            notice: None,
            may_overwrite: true,
            needs_save: false,
        })
    }

    fn save(&self, preferences: &PreferencesV2) -> Result<(), PersistenceError> {
        *self.0.lock().unwrap() = preferences.clone();
        Ok(())
    }
}

#[derive(Clone, Debug, Default)]
pub(super) struct NoopBrowser;

impl BrowserOpener for NoopBrowser {
    fn open_login_url(&self, _value: &str) -> Result<(), BrowserError> {
        Ok(())
    }
}

pub(super) fn preferences(thread_id: Option<&str>) -> PreferencesV2 {
    let scope = AccountScope::from_chatgpt_email("user@example.com").unwrap();
    PreferencesV2 {
        codex: CodexPreferencesV2 {
            account_scope: Some(scope.clone()),
            auto_resume_thread_id: thread_id.map(str::to_owned),
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
        },
        ..PreferencesV2::default()
    }
}

pub(super) fn listed_thread(
    id: &str,
    name: Option<&str>,
    preview: &str,
    cwd: &Path,
    updated_at: i64,
    source: &str,
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
        "source": source,
        "status": {"type": "idle"},
        "turns": []
    })
    .to_string()
}
