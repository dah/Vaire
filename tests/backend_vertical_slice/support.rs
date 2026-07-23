pub(super) use std::path::Path;
pub(super) use std::sync::{Arc, Mutex};
pub(super) use std::time::Duration;

pub(super) use tempfile::tempdir;
pub(super) use vaire::app::{
    AuthState, ConnectionState, Intent, ThinkingKind, ThreadState, TranscriptRole, TurnState,
};
pub(super) use vaire::backend::BackendCoordinator;
pub(super) use vaire::codex::safety::{FullAccessPolicy, IsolationPaths};
pub(super) use vaire::codex::session::SessionService;
pub(super) use vaire::codex::transport::{AppServerTransport, ProcessSpec, RequestTimeouts};
pub(super) use vaire::persistence::{
    AccountScope, CodexPreferencesV2, LoadOutcome, PersistenceError, PreferencesPort, PreferencesV2,
};
pub(super) use vaire::platform::{validate_login_url, BrowserError, BrowserOpener};

pub(super) use crate::shared_support::script;

pub(super) async fn session(root: &Path, body: &str) -> SessionService {
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
pub(super) struct MemoryPreferences {
    value: Arc<Mutex<PreferencesV2>>,
}

impl MemoryPreferences {
    pub(super) fn new(value: PreferencesV2) -> Self {
        Self {
            value: Arc::new(Mutex::new(value)),
        }
    }
    pub(super) fn value(&self) -> PreferencesV2 {
        self.value.lock().unwrap().clone()
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
        *self.value.lock().unwrap() = preferences.clone();
        Ok(())
    }
}

pub(super) struct FailingPreferences;

impl PreferencesPort for FailingPreferences {
    fn load(&self) -> Result<LoadOutcome, PersistenceError> {
        Ok(LoadOutcome {
            preferences: PreferencesV2::default(),
            notice: None,
            may_overwrite: true,
            needs_save: false,
        })
    }

    fn save(&self, _preferences: &PreferencesV2) -> Result<(), PersistenceError> {
        Err(std::io::Error::other("simulated persistence failure").into())
    }
}

#[derive(Clone, Default)]
pub(super) struct RecordingBrowser {
    pub(super) urls: Arc<Mutex<Vec<String>>>,
}

impl BrowserOpener for RecordingBrowser {
    fn open_login_url(&self, value: &str) -> Result<(), BrowserError> {
        validate_login_url(value)?;
        self.urls.lock().unwrap().push(value.to_owned());
        Ok(())
    }
}

#[derive(Clone, Debug, Default)]
pub(super) struct FailingBrowser;

impl BrowserOpener for FailingBrowser {
    fn open_login_url(&self, _value: &str) -> Result<(), BrowserError> {
        Err(BrowserError::Open("browser unavailable".to_owned()))
    }
}

pub(super) const INITIALIZED: &str = r#"{"id":1,"result":{"codexHome":"/private/tmp/codex","platformFamily":"unix","platformOs":"macos","userAgent":"fake/0.144.6"}}"#;
pub(super) const MODEL_PAGE: &str = r#"{"id":3,"result":{"data":[{"id":"m1","displayName":"Model One","isDefault":true,"defaultReasoningEffort":"high","supportedReasoningEfforts":[{"reasoningEffort":"low","description":"fast"},{"reasoningEffort":"high","description":"deep"}],"hidden":false}],"nextCursor":null}}"#;
