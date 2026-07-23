use std::collections::BTreeSet;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use agentharness::app::{
    Intent, OpenRouterConversationState, TranscriptEntryStatus, TranscriptRole, TurnState,
};
use agentharness::backend::BackendCoordinator;
use agentharness::codex::safety::{FullAccessPolicy, IsolationPaths};
use agentharness::codex::session::SessionService;
use agentharness::codex::transport::{AppServerTransport, ProcessSpec};
use agentharness::credentials::{
    CredentialAccount, CredentialStore, FakeCredentialOperation, FakeCredentialStore, SecretValue,
};
use agentharness::openrouter::{
    ChatRole, FileOpenRouterStore, OpenRouterAuthStatus, OpenRouterClient,
    OpenRouterConversationStore, OpenRouterConversationV2, OpenRouterService, OpenRouterTimeouts,
    OpenRouterTurnOutcome,
};
use agentharness::persistence::{LoadOutcome, PersistenceError, PreferencesPort, PreferencesV2};
use agentharness::platform::{BrowserError, BrowserOpener};
use agentharness::provider::ProviderId;
use tempfile::tempdir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use url::Url;

const TEST_KEY: &str = "sk-or-v1-offline-vertical-slice-key";

#[derive(Clone)]
struct MemoryPreferences(Arc<Mutex<PreferencesV2>>);

impl PreferencesPort for MemoryPreferences {
    fn load(&self) -> Result<LoadOutcome, PersistenceError> {
        Ok(LoadOutcome {
            preferences: self.0.lock().unwrap().clone(),
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

#[derive(Clone)]
struct ReadOnlyPreferences {
    value: PreferencesV2,
    saves: Arc<Mutex<usize>>,
}

impl PreferencesPort for ReadOnlyPreferences {
    fn load(&self) -> Result<LoadOutcome, PersistenceError> {
        Ok(LoadOutcome {
            preferences: self.value.clone(),
            notice: None,
            may_overwrite: false,
            needs_save: false,
        })
    }

    fn save(&self, _preferences: &PreferencesV2) -> Result<(), PersistenceError> {
        *self.saves.lock().unwrap() += 1;
        Ok(())
    }
}

#[derive(Clone)]
struct NoopBrowser;

impl BrowserOpener for NoopBrowser {
    fn open_login_url(&self, _value: &str) -> Result<(), BrowserError> {
        Ok(())
    }
}

async fn read_request(socket: &mut tokio::net::TcpStream) -> Vec<u8> {
    let mut request = Vec::new();
    let mut buffer = [0_u8; 4096];
    let header_end = loop {
        let count = socket.read(&mut buffer).await.unwrap();
        assert_ne!(count, 0, "request ended before its headers");
        request.extend_from_slice(&buffer[..count]);
        if let Some(position) = request.windows(4).position(|value| value == b"\r\n\r\n") {
            break position + 4;
        }
    };
    let headers = String::from_utf8_lossy(&request[..header_end]);
    let content_length = headers
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().ok())
                .flatten()
        })
        .unwrap_or(0);
    while request.len() < header_end + content_length {
        let count = socket.read(&mut buffer).await.unwrap();
        if count == 0 {
            break;
        }
        request.extend_from_slice(&buffer[..count]);
    }
    request
}

async fn scripted_openrouter(
    responses: Vec<(&'static str, String)>,
) -> (Url, Arc<Mutex<Vec<Vec<u8>>>>, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let captured = requests.clone();
    let task = tokio::spawn(async move {
        for (content_type, body) in responses {
            let (mut socket, _) = listener.accept().await.unwrap();
            let request = read_request(&mut socket).await;
            captured.lock().unwrap().push(request);
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            socket.write_all(response.as_bytes()).await.unwrap();
        }
    });
    (
        Url::parse(&format!("http://{address}")).unwrap(),
        requests,
        task,
    )
}

fn openrouter_service(
    base: Url,
    credentials: Arc<FakeCredentialStore>,
    store_root: &Path,
) -> (OpenRouterService, Arc<FileOpenRouterStore>) {
    let store = Arc::new(FileOpenRouterStore::new(store_root).unwrap());
    let client = OpenRouterClient::with_loopback_base_url(
        base,
        credentials.clone(),
        OpenRouterTimeouts {
            connect: Duration::from_secs(1),
            get_attempt: Duration::from_secs(1),
            chat_headers: Duration::from_secs(1),
            sse_idle: Duration::from_secs(1),
            chat_total: Duration::from_secs(2),
            retry_delay: Duration::ZERO,
        },
    )
    .unwrap();
    (
        OpenRouterService::new(client, credentials, store.clone()),
        store,
    )
}

fn openrouter_preferences(model_id: &str) -> PreferencesV2 {
    let mut preferences = PreferencesV2 {
        active_provider: ProviderId::OpenRouter,
        ..PreferencesV2::default()
    };
    preferences.openrouter.selected_model_id = Some(model_id.to_owned());
    preferences.openrouter.enabled_model_ids = BTreeSet::from([model_id.to_owned()]);
    preferences
}

fn catalog_responses(model_id: &str) -> Vec<(&'static str, String)> {
    vec![
        (
            "application/json",
            r#"{"data":{"label":"offline"}}"#.to_owned(),
        ),
        (
            "application/json",
            format!(
                r#"{{"data":[{{"id":"{model_id}","name":"Offline Model","context_length":1000}}]}}"#
            ),
        ),
    ]
}

fn failed_partial_responses(model_id: &str, partial: &str) -> Vec<(&'static str, String)> {
    let mut responses = catalog_responses(model_id);
    responses.push((
        "text/event-stream",
        format!(
            concat!(
                "data: {{\"id\":\"chat-failed\",\"model\":\"{model_id}\",\"choices\":[{{\"index\":0,\"delta\":{{\"content\":\"{partial}\"}},\"finish_reason\":null}}]}}\n\n",
                "data: {{\"id\":\"chat-failed\",\"model\":\"{model_id}\",\"error\":{{\"code\":429,\"message\":\"redacted by client\",\"metadata\":{{\"error_type\":\"rate_limit_exceeded\"}}}},\"choices\":[{{\"index\":0,\"delta\":{{\"content\":\"\"}},\"finish_reason\":\"error\"}}]}}\n\n"
            ),
            model_id = model_id,
            partial = partial,
        ),
    ));
    responses
}

async fn pump_until_turn_settles<P: PreferencesPort, B: BrowserOpener>(
    backend: &mut BackendCoordinator<P, B>,
) {
    for _ in 0..8 {
        if !backend.state().turn.is_active() {
            return;
        }
        tokio::time::timeout(Duration::from_secs(2), backend.pump_event())
            .await
            .unwrap()
            .unwrap();
    }
    panic!("OpenRouter turn did not settle: {:?}", backend.state().turn);
}

async fn persist_failed_partial(
    store_root: &Path,
    model_id: &str,
    user_text: &str,
    partial: &str,
) -> PreferencesV2 {
    let (base, _requests, server) =
        scripted_openrouter(failed_partial_responses(model_id, partial)).await;
    let credentials = Arc::new(FakeCredentialStore::with_openrouter_key(
        SecretValue::from_input(TEST_KEY).unwrap(),
    ));
    let (openrouter, store) = openrouter_service(base, credentials, store_root);
    let preferences = MemoryPreferences(Arc::new(Mutex::new(openrouter_preferences(model_id))));
    let mut backend = BackendCoordinator::without_codex(
        preferences,
        NoopBrowser,
        "offline Codex unavailable".to_owned(),
    )
    .with_openrouter(openrouter);

    backend.startup().await.unwrap();
    for _ in 0..2 {
        backend.pump_event().await.unwrap();
    }
    backend
        .handle_intent(Intent::SendMessage(user_text.to_owned()))
        .await
        .unwrap();
    pump_until_turn_settles(&mut backend).await;
    assert!(matches!(backend.state().turn, TurnState::Failed { .. }));

    let preferences = backend.state().preferences.clone();
    let conversation_id = preferences
        .openrouter
        .auto_resume_conversation_id
        .as_ref()
        .unwrap();
    let conversation = store.load_conversation(conversation_id).unwrap();
    assert_eq!(conversation.turns.len(), 1);
    assert_eq!(conversation.turns[0].outcome, OpenRouterTurnOutcome::Failed);
    assert_eq!(
        conversation.turns[0].incomplete_assistant_text.as_deref(),
        Some(partial)
    );
    assert_eq!(conversation.turns[0].assistant_text, None);

    backend.shutdown().await.unwrap();
    server.await.unwrap();
    preferences
}

async fn fake_openrouter() -> (Url, Arc<Mutex<Vec<Vec<u8>>>>, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let captured = requests.clone();
    let task = tokio::spawn(async move {
        let responses = [
            ("application/json", r#"{"data":{"label":"offline"}}"#),
            (
                "application/json",
                r#"{"data":[{"id":"vendor/model","name":"Offline Model","context_length":1000}]}"#,
            ),
            (
                "text/event-stream",
                concat!(
                    "data: {\"id\":\"chat-1\",\"model\":\"vendor/model\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hello\"},\"finish_reason\":null}]}\n\n",
                    "data: {\"id\":\"chat-1\",\"model\":\"vendor/model\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
                    "data: {\"id\":\"chat-1\",\"model\":\"vendor/model\",\"choices\":[],\"usage\":{\"prompt_tokens\":2,\"completion_tokens\":1,\"total_tokens\":3}}\n\n",
                    "data: [DONE]\n\n"
                ),
            ),
        ];
        for (content_type, body) in responses {
            let (mut socket, _) = listener.accept().await.unwrap();
            let request = read_request(&mut socket).await;
            captured.lock().unwrap().push(request);
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            socket.write_all(response.as_bytes()).await.unwrap();
        }
    });
    (
        Url::parse(&format!("http://{address}")).unwrap(),
        requests,
        task,
    )
}

async fn fake_candidate_catalog_failure(
    status: u16,
) -> (Url, Arc<Mutex<Vec<Vec<u8>>>>, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let captured = requests.clone();
    let task = tokio::spawn(async move {
        for (index, (response_status, body)) in [
            (200, r#"{"data":{"label":"candidate"}}"#),
            (status, r#"{"error":"sanitized remote rejection"}"#),
        ]
        .into_iter()
        .enumerate()
        {
            let (mut socket, _) = listener.accept().await.unwrap();
            let request = read_request(&mut socket).await;
            captured.lock().unwrap().push(request);
            let reason = if response_status == 401 {
                "Unauthorized"
            } else if response_status == 403 {
                "Forbidden"
            } else {
                "OK"
            };
            let response = format!(
                "HTTP/1.1 {response_status} {reason}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            socket.write_all(response.as_bytes()).await.unwrap();
            assert!(index < 2);
        }
    });
    (
        Url::parse(&format!("http://{address}")).unwrap(),
        requests,
        task,
    )
}

#[tokio::test]
async fn stored_candidate_rejected_by_catalog_401_or_403_becomes_invalid_but_is_retained() {
    const CANDIDATE: &str = "candidate-offline-key";
    for status in [401, 403] {
        let (base, requests, server) = fake_candidate_catalog_failure(status).await;
        let root = tempdir().unwrap();
        let credentials = Arc::new(FakeCredentialStore::with_openrouter_key(
            SecretValue::from_input("old-offline-key").unwrap(),
        ));
        let store = Arc::new(FileOpenRouterStore::new(root.path().join("openrouter")).unwrap());
        let client = OpenRouterClient::with_loopback_base_url(
            base,
            credentials.clone(),
            OpenRouterTimeouts {
                connect: Duration::from_secs(1),
                get_attempt: Duration::from_secs(1),
                chat_headers: Duration::from_secs(1),
                sse_idle: Duration::from_secs(1),
                chat_total: Duration::from_secs(2),
                retry_delay: Duration::ZERO,
            },
        )
        .unwrap();
        let openrouter = OpenRouterService::new(client, credentials.clone(), store);
        let mut backend = BackendCoordinator::without_codex(
            MemoryPreferences(Arc::new(Mutex::new(PreferencesV2::default()))),
            NoopBrowser,
            "offline Codex unavailable".to_owned(),
        )
        .with_openrouter(openrouter);

        backend
            .accept_openrouter_credential(SecretValue::from_input(CANDIDATE).unwrap())
            .unwrap();
        for _ in 0..2 {
            backend.pump_event().await.unwrap();
        }

        assert_eq!(
            backend.state().openrouter.auth,
            OpenRouterAuthStatus::Invalid
        );
        assert_eq!(
            credentials
                .load(CredentialAccount::OpenRouterApiKey)
                .unwrap()
                .unwrap()
                .expose_bytes(),
            CANDIDATE.as_bytes()
        );
        assert!(!credentials.operations().iter().any(|operation| matches!(
            operation,
            FakeCredentialOperation::Delete(CredentialAccount::OpenRouterApiKey)
        )));
        let notice = backend.state().notice.as_deref().unwrap();
        assert!(notice.contains("provider rejected it"));
        assert!(!notice.contains(CANDIDATE));
        backend.shutdown().await.unwrap();
        server.await.unwrap();
        assert_eq!(requests.lock().unwrap().len(), 2);
    }
}

#[tokio::test]
async fn first_openrouter_send_never_bypasses_a_read_only_preferences_gate() {
    let (base, server) = fake_openrouter_catalog_only("vendor/model").await;
    let root = tempdir().unwrap();
    let credentials = Arc::new(FakeCredentialStore::with_openrouter_key(
        SecretValue::from_input(TEST_KEY).unwrap(),
    ));
    let store = Arc::new(FileOpenRouterStore::new(root.path().join("openrouter")).unwrap());
    let client = OpenRouterClient::with_loopback_base_url(
        base,
        credentials.clone(),
        OpenRouterTimeouts {
            connect: Duration::from_secs(1),
            get_attempt: Duration::from_secs(1),
            chat_headers: Duration::from_secs(1),
            sse_idle: Duration::from_secs(1),
            chat_total: Duration::from_secs(2),
            retry_delay: Duration::ZERO,
        },
    )
    .unwrap();
    let openrouter = OpenRouterService::new(client, credentials, store.clone());
    let mut value = PreferencesV2 {
        active_provider: ProviderId::OpenRouter,
        ..PreferencesV2::default()
    };
    value.openrouter.selected_model_id = Some("vendor/model".to_owned());
    value.openrouter.enabled_model_ids = BTreeSet::from(["vendor/model".to_owned()]);
    let saves = Arc::new(Mutex::new(0));
    let preferences = ReadOnlyPreferences {
        value,
        saves: saves.clone(),
    };
    let mut backend = BackendCoordinator::without_codex(
        preferences,
        NoopBrowser,
        "offline Codex unavailable".to_owned(),
    )
    .with_openrouter(openrouter);

    backend.startup().await.unwrap();
    for _ in 0..2 {
        backend.pump_event().await.unwrap();
    }
    backend
        .handle_intent(Intent::SendMessage("must not post".to_owned()))
        .await
        .unwrap();

    assert!(!backend.state().turn.is_active());
    assert_eq!(*saves.lock().unwrap(), 0);
    let summaries = store.list_conversations().unwrap();
    assert_eq!(summaries.len(), 1);
    let stored = store.load_conversation(&summaries[0].id).unwrap();
    assert_eq!(stored.turns[0].outcome, OpenRouterTurnOutcome::Failed);
    backend.shutdown().await.unwrap();
    assert_eq!(*saves.lock().unwrap(), 0);
    server.await.unwrap();
}

async fn fake_openrouter_catalog_only(
    model_id: &'static str,
) -> (Url, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        let bodies = [
            r#"{"data":{"label":"offline"}}"#.to_owned(),
            format!(
                r#"{{"data":[{{"id":"{model_id}","name":"Offline Model","context_length":1000}}]}}"#
            ),
        ];
        for body in bodies {
            let (mut socket, _) = listener.accept().await.unwrap();
            let _ = read_request(&mut socket).await;
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            socket.write_all(response.as_bytes()).await.unwrap();
        }
    });
    (Url::parse(&format!("http://{address}")).unwrap(), task)
}

#[tokio::test]
async fn missing_or_corrupt_cached_catalog_waits_for_exact_live_model_before_auto_resume() {
    for corrupt_catalog in [false, true] {
        let (base, server) = fake_openrouter_catalog_only("vendor/model").await;
        let root = tempdir().unwrap();
        let credentials = Arc::new(FakeCredentialStore::with_openrouter_key(
            SecretValue::from_input(TEST_KEY).unwrap(),
        ));
        let store_root = root.path().join("openrouter");
        let store = Arc::new(FileOpenRouterStore::new(&store_root).unwrap());
        let conversation =
            OpenRouterConversationV2::new(Default::default(), 1, "Saved conversation");
        let conversation_id = conversation.id.clone();
        store.save_conversation(&conversation).unwrap();
        if corrupt_catalog {
            fs::write(store_root.join("catalog.json"), b"not-json").unwrap();
            fs::set_permissions(
                store_root.join("catalog.json"),
                fs::Permissions::from_mode(0o600),
            )
            .unwrap();
        }
        let client = OpenRouterClient::with_loopback_base_url(
            base,
            credentials.clone(),
            OpenRouterTimeouts {
                connect: Duration::from_secs(1),
                get_attempt: Duration::from_secs(1),
                chat_headers: Duration::from_secs(1),
                sse_idle: Duration::from_secs(1),
                chat_total: Duration::from_secs(2),
                retry_delay: Duration::ZERO,
            },
        )
        .unwrap();
        let openrouter = OpenRouterService::new(client, credentials, store);
        let mut preferences = PreferencesV2 {
            active_provider: ProviderId::OpenRouter,
            ..PreferencesV2::default()
        };
        preferences.openrouter.selected_model_id = Some("vendor/model".to_owned());
        preferences.openrouter.enabled_model_ids = BTreeSet::from(["vendor/model".to_owned()]);
        preferences.set_auto_resume_conversation(Some(conversation_id.clone()));
        let mut backend = BackendCoordinator::without_codex(
            MemoryPreferences(Arc::new(Mutex::new(preferences))),
            NoopBrowser,
            "offline Codex unavailable".to_owned(),
        )
        .with_openrouter(openrouter);

        backend.startup().await.unwrap();
        assert_eq!(
            backend.state().openrouter.conversation,
            OpenRouterConversationState::None
        );
        for _ in 0..2 {
            tokio::time::timeout(Duration::from_secs(2), backend.pump_event())
                .await
                .unwrap()
                .unwrap();
        }
        assert_eq!(
            backend.state().openrouter.conversation,
            OpenRouterConversationState::Ready {
                id: conversation_id
            }
        );
        assert_eq!(
            backend.state().selected_model.as_ref().unwrap().id,
            "vendor/model"
        );
        backend.shutdown().await.unwrap();
        server.await.unwrap();
    }
}

#[tokio::test]
async fn refreshed_catalog_without_exact_saved_model_blocks_auto_resume_without_fallback() {
    let (base, server) = fake_openrouter_catalog_only("vendor/other").await;
    let root = tempdir().unwrap();
    let credentials = Arc::new(FakeCredentialStore::with_openrouter_key(
        SecretValue::from_input(TEST_KEY).unwrap(),
    ));
    let store = Arc::new(FileOpenRouterStore::new(root.path().join("openrouter")).unwrap());
    let conversation = OpenRouterConversationV2::new(Default::default(), 1, "Saved conversation");
    let conversation_id = conversation.id.clone();
    store.save_conversation(&conversation).unwrap();
    let client = OpenRouterClient::with_loopback_base_url(
        base,
        credentials.clone(),
        OpenRouterTimeouts {
            connect: Duration::from_secs(1),
            get_attempt: Duration::from_secs(1),
            chat_headers: Duration::from_secs(1),
            sse_idle: Duration::from_secs(1),
            chat_total: Duration::from_secs(2),
            retry_delay: Duration::ZERO,
        },
    )
    .unwrap();
    let openrouter = OpenRouterService::new(client, credentials, store);
    let mut preferences = PreferencesV2 {
        active_provider: ProviderId::OpenRouter,
        ..PreferencesV2::default()
    };
    preferences.openrouter.selected_model_id = Some("vendor/model".to_owned());
    preferences.openrouter.enabled_model_ids =
        BTreeSet::from(["vendor/model".to_owned(), "vendor/other".to_owned()]);
    preferences.set_auto_resume_conversation(Some(conversation_id.clone()));
    let mut backend = BackendCoordinator::without_codex(
        MemoryPreferences(Arc::new(Mutex::new(preferences))),
        NoopBrowser,
        "offline Codex unavailable".to_owned(),
    )
    .with_openrouter(openrouter);

    backend.startup().await.unwrap();
    for _ in 0..2 {
        backend.pump_event().await.unwrap();
    }

    assert!(matches!(
        &backend.state().openrouter.conversation,
        OpenRouterConversationState::ResumeFailed { id, .. } if id == &conversation_id
    ));
    assert_eq!(
        backend
            .state()
            .preferences
            .openrouter
            .auto_resume_conversation_id
            .as_ref(),
        Some(&conversation_id)
    );
    assert_eq!(
        backend
            .state()
            .preferences
            .openrouter
            .selected_model_id
            .as_deref(),
        Some("vendor/model")
    );
    assert_eq!(
        backend.state().selected_model.as_ref().unwrap().id,
        "vendor/model"
    );
    backend.shutdown().await.unwrap();
    server.await.unwrap();
}

#[tokio::test]
async fn failed_partial_survives_reopen_auto_resume_and_is_excluded_from_later_post() {
    const MODEL: &str = "moonshotai/kimi-k3";
    const FIRST_USER: &str = "first question";
    const FAILED_PARTIAL: &str = "DISPLAY-ONLY-FAILED-PARTIAL";
    const LATER_USER: &str = "later question";

    let root = tempdir().unwrap();
    let store_root = root.path().join("openrouter");
    let preferences = persist_failed_partial(&store_root, MODEL, FIRST_USER, FAILED_PARTIAL).await;
    let conversation_id = preferences
        .openrouter
        .auto_resume_conversation_id
        .clone()
        .unwrap();

    let mut responses = catalog_responses(MODEL);
    responses.push((
        "text/event-stream",
        concat!(
            "data: {\"id\":\"chat-later\",\"model\":\"moonshotai/kimi-k3\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"later answer\"},\"finish_reason\":null}]}\n\n",
            "data: {\"id\":\"chat-later\",\"model\":\"moonshotai/kimi-k3\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
            "data: {\"id\":\"chat-later\",\"model\":\"moonshotai/kimi-k3\",\"choices\":[],\"usage\":{\"prompt_tokens\":4,\"completion_tokens\":2,\"total_tokens\":6}}\n\n",
            "data: [DONE]\n\n"
        )
        .to_owned(),
    ));
    let (base, requests, server) = scripted_openrouter(responses).await;
    let credentials = Arc::new(FakeCredentialStore::with_openrouter_key(
        SecretValue::from_input(TEST_KEY).unwrap(),
    ));
    let (openrouter, reopened_store) = openrouter_service(base, credentials, &store_root);
    let mut backend = BackendCoordinator::without_codex(
        MemoryPreferences(Arc::new(Mutex::new(preferences))),
        NoopBrowser,
        "offline Codex unavailable".to_owned(),
    )
    .with_openrouter(openrouter);

    backend.startup().await.unwrap();
    for _ in 0..2 {
        backend.pump_event().await.unwrap();
    }

    assert_eq!(
        backend.state().openrouter.conversation,
        OpenRouterConversationState::Ready {
            id: conversation_id.clone()
        }
    );
    let incomplete = backend
        .state()
        .transcript
        .iter()
        .filter(|entry| {
            entry.role == TranscriptRole::Assistant
                && entry.status == TranscriptEntryStatus::FailedIncomplete
        })
        .collect::<Vec<_>>();
    assert_eq!(incomplete.len(), 1);
    assert_eq!(incomplete[0].text, FAILED_PARTIAL);
    assert_eq!(
        backend
            .state()
            .transcript
            .iter()
            .filter(|entry| entry.role == TranscriptRole::Assistant)
            .count(),
        1
    );

    backend
        .handle_intent(Intent::SendMessage(LATER_USER.to_owned()))
        .await
        .unwrap();
    pump_until_turn_settles(&mut backend).await;
    assert!(matches!(backend.state().turn, TurnState::Completed { .. }));

    let reopened = reopened_store.load_conversation(&conversation_id).unwrap();
    assert_eq!(reopened.turns.len(), 2);
    assert_eq!(reopened.turns[0].outcome, OpenRouterTurnOutcome::Failed);
    assert_eq!(
        reopened.turns[0].incomplete_assistant_text.as_deref(),
        Some(FAILED_PARTIAL)
    );
    assert_eq!(reopened.turns[1].outcome, OpenRouterTurnOutcome::Completed);
    assert_eq!(
        reopened.turns[1].assistant_text.as_deref(),
        Some("later answer")
    );

    backend.shutdown().await.unwrap();
    server.await.unwrap();
    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 3);
    let post = String::from_utf8_lossy(&requests[2]);
    assert!(post.starts_with("POST /api/v1/chat/completions HTTP/1.1"));
    let body = post.split_once("\r\n\r\n").unwrap().1;
    assert!(!body.contains(FAILED_PARTIAL));
    let body: serde_json::Value = serde_json::from_str(body).unwrap();
    let messages = body["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0]["role"], "user");
    assert_eq!(messages[0]["content"], FIRST_USER);
    assert_eq!(messages[1]["role"], "user");
    assert_eq!(messages[1]["content"], LATER_USER);
}

#[tokio::test]
async fn unified_picker_effect_restores_failed_partial_from_reopened_real_store() {
    const MODEL: &str = "moonshotai/kimi-k3";
    const FAILED_PARTIAL: &str = "PICKER-RESTORED-FAILED-PARTIAL";

    let root = tempdir().unwrap();
    let store_root = root.path().join("openrouter");
    let mut preferences =
        persist_failed_partial(&store_root, MODEL, "picker question", FAILED_PARTIAL).await;
    let conversation_id = preferences
        .openrouter
        .auto_resume_conversation_id
        .clone()
        .unwrap();
    preferences.active_provider = ProviderId::Codex;

    let (base, _requests, server) = scripted_openrouter(catalog_responses(MODEL)).await;
    let credentials = Arc::new(FakeCredentialStore::with_openrouter_key(
        SecretValue::from_input(TEST_KEY).unwrap(),
    ));
    let (openrouter, _reopened_store) = openrouter_service(base, credentials, &store_root);
    let mut backend = BackendCoordinator::without_codex(
        MemoryPreferences(Arc::new(Mutex::new(preferences))),
        NoopBrowser,
        "offline Codex unavailable".to_owned(),
    )
    .with_openrouter(openrouter);

    backend.startup().await.unwrap();
    for _ in 0..2 {
        backend.pump_event().await.unwrap();
    }
    assert_eq!(backend.state().active_provider, ProviderId::Codex);

    backend.handle_intent(Intent::Resume).await.unwrap();
    backend
        .handle_intent(Intent::ThreadPickerSelect)
        .await
        .unwrap();

    assert_eq!(backend.state().active_provider, ProviderId::OpenRouter);
    assert_eq!(
        backend.state().openrouter.conversation,
        OpenRouterConversationState::Ready {
            id: conversation_id
        }
    );
    let incomplete = backend
        .state()
        .transcript
        .iter()
        .filter(|entry| {
            entry.role == TranscriptRole::Assistant
                && entry.status == TranscriptEntryStatus::FailedIncomplete
        })
        .collect::<Vec<_>>();
    assert_eq!(incomplete.len(), 1);
    assert_eq!(incomplete[0].text, FAILED_PARTIAL);
    assert_eq!(
        backend
            .state()
            .transcript
            .iter()
            .filter(|entry| entry.role == TranscriptRole::Assistant)
            .count(),
        1
    );

    backend.shutdown().await.unwrap();
    server.await.unwrap();
}

#[tokio::test]
async fn resolved_kimi_completion_persists_through_service_and_store_reopen() {
    const MODEL: &str = "moonshotai/kimi-k3";

    let root = tempdir().unwrap();
    let store_root = root.path().join("openrouter");
    let mut responses = catalog_responses(MODEL);
    responses.push((
        "text/event-stream",
        concat!(
            "data: {\"id\":\"chat-kimi\",\"model\":\"moonshotai/kimi-k3\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hello\"},\"finish_reason\":null}]}\n\n",
            "data: {\"id\":\"chat-kimi\",\"model\":\"moonshotai/kimi-k3\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
            "data: {\"id\":\"chat-kimi\",\"model\":\"moonshotai/kimi-k3-20260715\",\"choices\":[],\"usage\":{\"prompt_tokens\":2,\"completion_tokens\":1,\"total_tokens\":3}}\n\n",
            "data: [DONE]\n\n"
        )
        .to_owned(),
    ));
    let (base, _requests, server) = scripted_openrouter(responses).await;
    let credentials = Arc::new(FakeCredentialStore::with_openrouter_key(
        SecretValue::from_input(TEST_KEY).unwrap(),
    ));
    let (openrouter, store) = openrouter_service(base, credentials, &store_root);
    let preferences = MemoryPreferences(Arc::new(Mutex::new(openrouter_preferences(MODEL))));
    let mut backend = BackendCoordinator::without_codex(
        preferences,
        NoopBrowser,
        "offline Codex unavailable".to_owned(),
    )
    .with_openrouter(openrouter);

    backend.startup().await.unwrap();
    for _ in 0..2 {
        backend.pump_event().await.unwrap();
    }
    backend
        .handle_intent(Intent::SendMessage("resolved model question".to_owned()))
        .await
        .unwrap();
    pump_until_turn_settles(&mut backend).await;
    assert!(matches!(backend.state().turn, TurnState::Completed { .. }));

    let preferences = backend.state().preferences.clone();
    let conversation_id = preferences
        .openrouter
        .auto_resume_conversation_id
        .clone()
        .unwrap();
    let stored = store.load_conversation(&conversation_id).unwrap();
    assert_eq!(stored.turns.len(), 1);
    assert_eq!(stored.turns[0].outcome, OpenRouterTurnOutcome::Completed);
    assert_eq!(stored.turns[0].model_id, MODEL);
    assert_eq!(stored.turns[0].assistant_text.as_deref(), Some("hello"));
    assert_eq!(stored.turns[0].incomplete_assistant_text, None);

    backend.shutdown().await.unwrap();
    server.await.unwrap();
    drop(store);

    let (base, _requests, server) = scripted_openrouter(catalog_responses(MODEL)).await;
    let credentials = Arc::new(FakeCredentialStore::with_openrouter_key(
        SecretValue::from_input(TEST_KEY).unwrap(),
    ));
    let (openrouter, reopened_store) = openrouter_service(base, credentials, &store_root);
    let mut reopened_backend = BackendCoordinator::without_codex(
        MemoryPreferences(Arc::new(Mutex::new(preferences))),
        NoopBrowser,
        "offline Codex unavailable".to_owned(),
    )
    .with_openrouter(openrouter);

    reopened_backend.startup().await.unwrap();
    for _ in 0..2 {
        reopened_backend.pump_event().await.unwrap();
    }
    let restored = reopened_backend
        .state()
        .transcript
        .iter()
        .filter(|entry| entry.role == TranscriptRole::Assistant)
        .collect::<Vec<_>>();
    assert_eq!(restored.len(), 1);
    assert_eq!(restored[0].text, "hello");
    assert_eq!(restored[0].status, TranscriptEntryStatus::Normal);
    let reopened = reopened_store.load_conversation(&conversation_id).unwrap();
    assert_eq!(reopened.turns[0].outcome, OpenRouterTurnOutcome::Completed);
    assert_eq!(reopened.turns[0].model_id, MODEL);
    assert_eq!(reopened.turns[0].assistant_text.as_deref(), Some("hello"));
    assert_eq!(reopened.turns[0].incomplete_assistant_text, None);
    let canonical = reopened.canonical_messages();
    assert_eq!(canonical.len(), 2);
    assert_eq!(canonical[0].role, ChatRole::User);
    assert_eq!(canonical[0].content, "resolved model question");
    assert_eq!(canonical[1].role, ChatRole::Assistant);
    assert_eq!(canonical[1].content, "hello");

    reopened_backend.shutdown().await.unwrap();
    server.await.unwrap();
}

async fn flooding_codex_session(root: &std::path::Path) -> SessionService {
    let executable = root.join("fake-codex-flood");
    let script = r#"#!/bin/sh
set -eu
IFS= read -r initialize
printf '%s\n' '{"id":1,"result":{"codexHome":"/private/tmp/codex","platformFamily":"unix","platformOs":"macos","userAgent":"fake/0.144.6"}}'
IFS= read -r initialized
IFS= read -r account
printf '%s\n' '{"id":2,"result":{"account":null,"requiresOpenaiAuth":true}}'
IFS= read -r models
printf '%s\n' '{"id":3,"result":{"data":[{"id":"codex-model","displayName":"Codex","isDefault":true,"defaultReasoningEffort":"high","supportedReasoningEfforts":[{"reasoningEffort":"high","description":"deep"}],"hidden":false}],"nextCursor":null}}'
i=0
while [ "$i" -lt 48 ]; do
  printf '%s\n' '{"method":"future/noisyNotification","params":{"ignored":true}}'
  i=$((i + 1))
done
IFS= read -r hold
"#;
    fs::write(&executable, script).unwrap();
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
    let isolation = IsolationPaths::prepare(root.join("runtime")).unwrap();
    let transport = AppServerTransport::spawn(ProcessSpec {
        executable,
        args: Vec::new(),
        cwd: root.to_owned(),
        env: Vec::new(),
    })
    .await
    .unwrap();
    SessionService::new(transport, isolation, FullAccessPolicy)
}

#[tokio::test]
async fn offline_backend_runs_openrouter_startup_catalog_chat_persistence_and_shutdown() {
    let (base, requests, server) = fake_openrouter().await;
    let root = tempdir().unwrap();
    let credentials = Arc::new(FakeCredentialStore::with_openrouter_key(
        SecretValue::from_input(TEST_KEY).unwrap(),
    ));
    let store = Arc::new(FileOpenRouterStore::new(root.path().join("openrouter")).unwrap());
    let client = OpenRouterClient::with_loopback_base_url(
        base,
        credentials.clone(),
        OpenRouterTimeouts {
            connect: Duration::from_secs(1),
            get_attempt: Duration::from_secs(1),
            chat_headers: Duration::from_secs(1),
            sse_idle: Duration::from_secs(1),
            chat_total: Duration::from_secs(2),
            retry_delay: Duration::ZERO,
        },
    )
    .unwrap();
    let openrouter = OpenRouterService::new(client, credentials, store.clone());
    let mut preferences = PreferencesV2 {
        active_provider: ProviderId::OpenRouter,
        ..PreferencesV2::default()
    };
    preferences.openrouter.selected_model_id = Some("vendor/model".to_owned());
    preferences.openrouter.enabled_model_ids = BTreeSet::from(["vendor/model".to_owned()]);
    let preferences = MemoryPreferences(Arc::new(Mutex::new(preferences)));
    let mut backend = BackendCoordinator::without_codex(
        preferences,
        NoopBrowser,
        "offline Codex unavailable".to_owned(),
    )
    .with_openrouter(openrouter);

    backend.startup().await.unwrap();
    for _ in 0..2 {
        tokio::time::timeout(Duration::from_secs(2), backend.pump_event())
            .await
            .unwrap()
            .unwrap();
    }
    assert_eq!(backend.state().openrouter.auth, OpenRouterAuthStatus::Valid);
    assert_eq!(backend.state().active_provider, ProviderId::OpenRouter);

    backend
        .handle_intent(Intent::SendMessage("offline hello".to_owned()))
        .await
        .unwrap();
    for _ in 0..8 {
        if !backend.state().turn.is_active() {
            break;
        }
        tokio::time::timeout(Duration::from_secs(2), backend.pump_event())
            .await
            .unwrap()
            .unwrap();
    }
    assert!(matches!(backend.state().turn, TurnState::Completed { .. }));
    assert!(backend.state().transcript.iter().any(|entry| {
        entry.role == TranscriptRole::Assistant
            && entry.provider == ProviderId::OpenRouter
            && entry.text == "hello"
    }));
    let conversation_id = backend
        .state()
        .preferences
        .openrouter
        .auto_resume_conversation_id
        .clone()
        .unwrap();
    let stored = store.load_conversation(&conversation_id).unwrap();
    assert_eq!(stored.turns[0].outcome, OpenRouterTurnOutcome::Completed);
    assert_eq!(stored.turns[0].assistant_text.as_deref(), Some("hello"));

    tokio::time::timeout(Duration::from_secs(2), backend.shutdown())
        .await
        .expect("dual-provider coordinator shutdown must remain bounded")
        .unwrap();
    server.await.unwrap();
    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 3);
    let text = requests
        .iter()
        .map(|request| String::from_utf8_lossy(request).into_owned())
        .collect::<Vec<_>>();
    assert!(text[0].starts_with("GET /api/v1/key HTTP/1.1"));
    assert!(text[1].starts_with("GET /api/v1/models/user HTTP/1.1"));
    assert!(text[2].starts_with("POST /api/v1/chat/completions HTTP/1.1"));
    assert!(text.iter().all(|request| request
        .to_ascii_lowercase()
        .contains(&format!("authorization: bearer {TEST_KEY}").to_ascii_lowercase())));
    let body = text[2].split_once("\r\n\r\n").unwrap().1;
    assert!(!body.contains("Codex"));
    let body: serde_json::Value = serde_json::from_str(body).unwrap();
    let messages = body["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0]["role"], "user");
    assert_eq!(messages[0]["content"], "offline hello");
}

#[tokio::test]
async fn completion_queued_before_logout_remains_completed_with_final_text() {
    let (base, _requests, server) = fake_openrouter().await;
    let root = tempdir().unwrap();
    let credentials = Arc::new(FakeCredentialStore::with_openrouter_key(
        SecretValue::from_input(TEST_KEY).unwrap(),
    ));
    let store = Arc::new(FileOpenRouterStore::new(root.path().join("openrouter")).unwrap());
    let client = OpenRouterClient::with_loopback_base_url(
        base,
        credentials.clone(),
        OpenRouterTimeouts {
            connect: Duration::from_secs(1),
            get_attempt: Duration::from_secs(1),
            chat_headers: Duration::from_secs(1),
            sse_idle: Duration::from_secs(1),
            chat_total: Duration::from_secs(2),
            retry_delay: Duration::ZERO,
        },
    )
    .unwrap();
    let openrouter = OpenRouterService::new(client, credentials, store);
    let mut preferences = PreferencesV2 {
        active_provider: ProviderId::OpenRouter,
        ..PreferencesV2::default()
    };
    preferences.openrouter.selected_model_id = Some("vendor/model".to_owned());
    preferences.openrouter.enabled_model_ids = BTreeSet::from(["vendor/model".to_owned()]);
    let mut backend = BackendCoordinator::without_codex(
        MemoryPreferences(Arc::new(Mutex::new(preferences))),
        NoopBrowser,
        "offline Codex unavailable".to_owned(),
    )
    .with_openrouter(openrouter);

    backend.startup().await.unwrap();
    for _ in 0..2 {
        backend.pump_event().await.unwrap();
    }
    backend
        .handle_intent(Intent::SendMessage("finish before logout".to_owned()))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    backend
        .handle_intent(Intent::LogoutOpenRouter)
        .await
        .unwrap();

    assert!(matches!(backend.state().turn, TurnState::Completed { .. }));
    assert!(backend.state().transcript.iter().any(|entry| {
        entry.role == TranscriptRole::Assistant
            && entry.provider == ProviderId::OpenRouter
            && entry.text == "hello"
    }));
    assert_eq!(
        backend.state().openrouter.auth,
        OpenRouterAuthStatus::Missing
    );
    backend.shutdown().await.unwrap();
    server.await.unwrap();
}

#[tokio::test]
async fn codex_event_flood_does_not_starve_openrouter_or_deadlock_dual_provider_shutdown() {
    let (base, _requests, server) = fake_openrouter().await;
    let root = tempdir().unwrap();
    let credentials = Arc::new(FakeCredentialStore::with_openrouter_key(
        SecretValue::from_input(TEST_KEY).unwrap(),
    ));
    let store = Arc::new(FileOpenRouterStore::new(root.path().join("openrouter")).unwrap());
    let client = OpenRouterClient::with_loopback_base_url(
        base,
        credentials.clone(),
        OpenRouterTimeouts {
            connect: Duration::from_secs(1),
            get_attempt: Duration::from_secs(1),
            chat_headers: Duration::from_secs(1),
            sse_idle: Duration::from_secs(1),
            chat_total: Duration::from_secs(2),
            retry_delay: Duration::ZERO,
        },
    )
    .unwrap();
    let openrouter = OpenRouterService::new(client, credentials, store);
    let mut preferences = PreferencesV2 {
        active_provider: ProviderId::OpenRouter,
        ..PreferencesV2::default()
    };
    preferences.openrouter.selected_model_id = Some("vendor/model".to_owned());
    preferences.openrouter.enabled_model_ids = BTreeSet::from(["vendor/model".to_owned()]);
    let preferences = MemoryPreferences(Arc::new(Mutex::new(preferences)));
    let session = flooding_codex_session(root.path()).await;
    let mut backend =
        BackendCoordinator::new(session, preferences, NoopBrowser).with_openrouter(openrouter);

    backend.startup().await.unwrap();
    for _ in 0..140 {
        if backend.state().openrouter.auth == OpenRouterAuthStatus::Valid
            && !backend.state().openrouter.catalog.is_empty()
        {
            break;
        }
        tokio::time::timeout(Duration::from_secs(2), backend.pump_event())
            .await
            .unwrap()
            .unwrap();
    }
    assert_eq!(backend.state().openrouter.auth, OpenRouterAuthStatus::Valid);
    assert!(!backend.state().openrouter.catalog.is_empty());
    backend
        .handle_intent(Intent::SendMessage("survives flood".to_owned()))
        .await
        .unwrap();
    for _ in 0..140 {
        if !backend.state().turn.is_active() {
            break;
        }
        tokio::time::timeout(Duration::from_secs(2), backend.pump_event())
            .await
            .unwrap()
            .unwrap();
    }
    assert!(
        matches!(backend.state().turn, TurnState::Completed { .. }),
        "turn after dual-provider flood: {:?}; notice: {:?}",
        backend.state().turn,
        backend.state().notice
    );
    tokio::time::timeout(Duration::from_secs(2), backend.shutdown())
        .await
        .expect("both provider tasks and the Codex child must settle")
        .unwrap();
    server.await.unwrap();
}
