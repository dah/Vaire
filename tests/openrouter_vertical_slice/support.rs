use super::*;

#[derive(Clone)]
pub(super) struct MemoryPreferences(pub(super) Arc<Mutex<PreferencesV3>>);

impl PreferencesPort for MemoryPreferences {
    fn load(&self) -> Result<LoadOutcome, PersistenceError> {
        Ok(LoadOutcome {
            preferences: self.0.lock().unwrap().clone(),
            notice: None,
            may_overwrite: true,
            needs_save: false,
        })
    }

    fn save(&self, preferences: &PreferencesV3) -> Result<(), PersistenceError> {
        *self.0.lock().unwrap() = preferences.clone();
        Ok(())
    }
}

#[derive(Clone)]
pub(super) struct ReadOnlyPreferences {
    pub(super) value: PreferencesV3,
    pub(super) saves: Arc<Mutex<usize>>,
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

    fn save(&self, _preferences: &PreferencesV3) -> Result<(), PersistenceError> {
        *self.saves.lock().unwrap() += 1;
        Ok(())
    }
}

#[derive(Clone)]
pub(super) struct NoopBrowser;

impl BrowserOpener for NoopBrowser {
    fn open_login_url(&self, _value: &str) -> Result<(), BrowserError> {
        Ok(())
    }
}

pub(super) async fn read_request(socket: &mut tokio::net::TcpStream) -> Vec<u8> {
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

pub(super) async fn scripted_openrouter(
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

pub(super) fn openrouter_service(
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

pub(super) fn openrouter_preferences(model_id: &str) -> PreferencesV3 {
    let mut preferences = PreferencesV3 {
        active_provider: ProviderId::OpenRouter,
        ..PreferencesV3::default()
    };
    preferences.openrouter.selected_model_id = Some(model_id.to_owned());
    preferences.openrouter.enabled_model_ids = BTreeSet::from([model_id.to_owned()]);
    preferences
}

pub(super) fn catalog_responses(model_id: &str) -> Vec<(&'static str, String)> {
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

pub(super) fn failed_partial_responses(
    model_id: &str,
    partial: &str,
) -> Vec<(&'static str, String)> {
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

pub(super) async fn pump_until_turn_settles<P: PreferencesPort, B: BrowserOpener>(
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

pub(super) async fn persist_failed_partial(
    store_root: &Path,
    model_id: &str,
    user_text: &str,
    partial: &str,
) -> PreferencesV3 {
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

pub(super) async fn fake_openrouter() -> (Url, Arc<Mutex<Vec<Vec<u8>>>>, tokio::task::JoinHandle<()>)
{
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

pub(super) async fn fake_candidate_catalog_failure(
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

pub(super) async fn fake_openrouter_catalog_only(
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
