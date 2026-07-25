use super::support::*;

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
        MemoryPreferences::new(PreferencesV4::default()),
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
IFS= read -r hold
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
        MemoryPreferences::new(PreferencesV4::default()),
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

#[tokio::test]
async fn context_meter_uses_last_usage_not_cumulative_total_and_survives_completion() {
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
printf '%s\n' '{{"id":4,"result":{{"thread":{{"id":"thr-context","turns":[]}}}}}}'
IFS= read -r turn_start
printf '%s\n' '{{"id":5,"result":{{"turn":{{"id":"turn-context","items":[],"status":"inProgress"}}}}}}'
printf '%s\n' '{{"method":"thread/tokenUsage/updated","params":{{"threadId":"thr-context","turnId":"turn-context","tokenUsage":{{"last":{{"cachedInputTokens":0,"inputTokens":20,"outputTokens":0,"reasoningOutputTokens":0,"totalTokens":20}},"total":{{"cachedInputTokens":0,"inputTokens":20,"outputTokens":0,"reasoningOutputTokens":0,"totalTokens":20}},"modelContextWindow":100}}}}}}'
printf '%s\n' '{{"method":"thread/tokenUsage/updated","params":{{"threadId":"thr-context","turnId":"turn-context","tokenUsage":{{"last":{{"cachedInputTokens":0,"inputTokens":5,"outputTokens":0,"reasoningOutputTokens":0,"totalTokens":5}},"total":{{"cachedInputTokens":0,"inputTokens":250,"outputTokens":0,"reasoningOutputTokens":0,"totalTokens":250}},"modelContextWindow":100}}}}}}'
printf '%s\n' '{{"method":"turn/completed","params":{{"threadId":"thr-context","turn":{{"id":"turn-context","items":[],"status":"completed"}}}}}}'
IFS= read -r hold
"#
    );
    let mut backend = BackendCoordinator::new(
        session(temp.path(), &body).await,
        MemoryPreferences::new(PreferencesV4::default()),
        RecordingBrowser::default(),
    );
    backend.startup().await.unwrap();
    backend
        .handle_intent(Intent::SendMessage("measure context".to_owned()))
        .await
        .unwrap();
    assert_eq!(backend.state().context_remaining_percent, None);

    assert!(backend.pump_event().await.unwrap());
    assert_eq!(backend.state().context_remaining_percent, Some(80));
    assert!(backend.pump_event().await.unwrap());
    assert_eq!(backend.state().context_remaining_percent, Some(95));
    assert!(backend.pump_event().await.unwrap());
    assert!(matches!(backend.state().turn, TurnState::Completed { .. }));
    assert_eq!(backend.state().context_remaining_percent, Some(95));

    backend.shutdown().await.unwrap();
}
