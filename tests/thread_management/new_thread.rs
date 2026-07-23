use super::support::*;

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
case "$new_thread" in *'"method":"thread/start"'*'"threadSource":"appServer"'*) ;; *) exit 41 ;; esac
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
    assert_eq!(
        saved.value().codex.auto_resume_thread_id.as_deref(),
        Some("thr-new")
    );
    backend.shutdown().await.unwrap();
}

#[tokio::test]
async fn backend_rejects_an_invalid_effect_that_includes_the_active_thread() {
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
printf '%s\n' '{{"id":4,"result":{{"thread":{{"id":"thr-active","turns":[]}}}}}}'
IFS= read -r read_thread
printf '%s\n' '{{"id":5,"result":{{"thread":{{"id":"thr-active","turns":[]}}}}}}'
IFS= read -r delete_inactive
case "$delete_inactive" in
  *'"method":"thread/delete"'*'"threadId":"thr-old"'*) ;;
  *) exit 61 ;;
esac
printf '%s\n' '{{"id":6,"result":{{}}}}'
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

    backend
        .execute_pending(vec![Effect::DeleteThreads {
            ids: vec!["thr-active".to_owned(), "thr-old".to_owned()],
        }])
        .await
        .unwrap();

    assert!(matches!(
        backend.state().connection,
        ConnectionState::Ready { .. }
    ));
    assert!(matches!(&backend.state().thread, ThreadState::Ready { id } if id == "thr-active"));
    assert_eq!(
        backend
            .state()
            .preferences
            .codex
            .auto_resume_thread_id
            .as_deref(),
        Some("thr-active")
    );
    assert_eq!(
        saved.value().codex.auto_resume_thread_id.as_deref(),
        Some("thr-active")
    );
    backend.shutdown().await.unwrap();
}

#[tokio::test]
async fn approval_during_new_thread_preserves_the_active_conversation_and_fails_closed() {
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
printf '%s\n' '{{"id":4,"result":{{"thread":{{"id":"thr-active","turns":[]}}}}}}'
IFS= read -r read_thread
printf '%s\n' '{{"id":5,"result":{{"thread":{{"id":"thr-active","turns":[{{"id":"turn-history","status":"completed","items":[{{"id":"history-agent","type":"agentMessage","text":"old answer"}}]}}]}}}}}}'
IFS= read -r seed_turn
case "$seed_turn" in *'"method":"turn/start"'*'"threadId":"thr-active"'*) ;; *) exit 71 ;; esac
printf '%s\n' '{{"id":6,"result":{{"turn":{{"id":"turn-seed","items":[],"status":"inProgress"}}}}}}'
printf '%s\n' '{{"method":"item/reasoning/summaryTextDelta","params":{{"threadId":"thr-active","turnId":"turn-seed","itemId":"thinking-seed","summaryIndex":0,"delta":"reasoning to preserve"}}}}'
printf '%s\n' '{{"method":"thread/tokenUsage/updated","params":{{"threadId":"thr-active","turnId":"turn-seed","tokenUsage":{{"last":{{"cachedInputTokens":0,"inputTokens":25,"outputTokens":0,"reasoningOutputTokens":0,"totalTokens":25}},"total":{{"cachedInputTokens":0,"inputTokens":25,"outputTokens":0,"reasoningOutputTokens":0,"totalTokens":25}},"modelContextWindow":100}}}}}}'
printf '%s\n' '{{"method":"turn/completed","params":{{"threadId":"thr-active","turn":{{"id":"turn-seed","items":[],"status":"completed"}}}}}}'
IFS= read -r new_thread
case "$new_thread" in *'"method":"thread/start"'*) ;; *) exit 72 ;; esac
printf '%s\n' '{{"id":"approval-1","method":"item/commandExecution/requestApproval","params":{{}}}}'
IFS= read -r denial
case "$denial" in *'"id":"approval-1"'*'"decision":"cancel"'*) ;; *) exit 73 ;; esac
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
    backend
        .handle_intent(Intent::SendMessage("seed state".to_owned()))
        .await
        .unwrap();
    for _ in 0..3 {
        assert!(backend.pump_event().await.unwrap());
    }
    assert!(matches!(backend.state().turn, TurnState::Completed { .. }));
    assert_eq!(backend.state().thinking.entries.len(), 1);
    assert_eq!(backend.state().context_remaining_percent, Some(75));

    let preserved_thread = backend.state().thread.clone();
    let preserved_turn = backend.state().turn.clone();
    let preserved_transcript = backend.state().transcript.clone();
    let preserved_thinking = backend.state().thinking.clone();
    let preserved_context = backend.state().context_remaining_percent;
    let preserved_preferences = backend.state().preferences.clone();
    let persisted_preferences = saved.value();

    backend.handle_intent(Intent::NewThread).await.unwrap();

    assert!(matches!(
        backend.state().connection,
        ConnectionState::Failed(_)
    ));
    assert_eq!(backend.state().thread, preserved_thread);
    assert_eq!(backend.state().turn, preserved_turn);
    assert_eq!(backend.state().transcript, preserved_transcript);
    assert_eq!(backend.state().thinking, preserved_thinking);
    assert_eq!(backend.state().context_remaining_percent, preserved_context);
    assert_eq!(backend.state().preferences, preserved_preferences);
    assert_eq!(saved.value(), persisted_preferences);
    assert!(backend
        .state()
        .notice
        .as_deref()
        .unwrap()
        .contains("current thread was preserved"));

    assert!(
        backend.pump_event().await.unwrap(),
        "the queued safety event should be delivered after the failed request"
    );
    assert!(matches!(
        backend.state().connection,
        ConnectionState::Failed(_)
    ));
    assert_eq!(backend.state().thread, preserved_thread);
    assert_eq!(backend.state().turn, preserved_turn);
    assert_eq!(backend.state().transcript, preserved_transcript);
    assert_eq!(backend.state().thinking, preserved_thinking);
    assert_eq!(backend.state().context_remaining_percent, preserved_context);
    assert_eq!(backend.state().preferences, preserved_preferences);
    assert_eq!(saved.value(), persisted_preferences);
    assert!(backend
        .state()
        .notice
        .as_deref()
        .unwrap()
        .contains("unexpected server request denied"));
    backend.shutdown().await.unwrap();
}
