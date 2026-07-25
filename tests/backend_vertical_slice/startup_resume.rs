use super::support::*;

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
case "$thread_start" in
  *'"threadSource":"appServer"'*) ;;
  *) exit 88 ;;
esac
case "$thread_start" in
  *'"show_raw_agent_reasoning":true'*) ;;
  *) exit 87 ;;
esac
printf '%s\n' '{{"id":5,"result":{{"thread":{{"id":"thr-new","turns":[]}}}}}}'
IFS= read -r turn_start
case "$turn_start" in
  *'"summary":"detailed"'*) ;;
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
printf '%s\n' '{{"method":"item/reasoning/summaryTextDelta","params":{{"threadId":"thr-new","turnId":"turn-new","itemId":"why","summaryIndex":0,"delta":" stale"}}}}'
printf '%s\n' '{{"method":"item/agentMessage/delta","params":{{"threadId":"thr-new","turnId":"turn-new","itemId":"item-a","delta":"h\u001b\u202eé"}}}}'
printf '%s\n' '{{"method":"item/completed","params":{{"threadId":"thr-new","turnId":"turn-new","completedAtMs":1,"item":{{"id":"item-a","type":"agentMessage","text":"h\u001b\u202eéllo"}}}}}}'
printf '%s\n' '{{"method":"item/agentMessage/delta","params":{{"threadId":"thr-new","turnId":"turn-new","itemId":"item-a","delta":" stale"}}}}'
printf '%s\n' '{{"method":"turn/completed","params":{{"threadId":"thr-new","turn":{{"id":"turn-new","items":[],"status":"completed"}}}}}}'
IFS= read -r hold
"#
    );
    let session = session(temp.path(), &body).await;
    let preferences = MemoryPreferences::new(PreferencesV4::default());
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
    for _ in 0..12 {
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
    assert_eq!(
        saved.value().codex.auto_resume_thread_id.as_deref(),
        Some("thr-new")
    );
    backend.shutdown().await.unwrap();
}

#[tokio::test]
async fn same_account_update_does_not_resume_or_disturb_the_active_turn() {
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
printf '%s\n' '{{"id":4,"result":{{"thread":{{"id":"thr-refresh","turns":[]}}}}}}'
IFS= read -r turn_start
printf '%s\n' '{{"id":5,"result":{{"turn":{{"id":"turn-refresh","items":[],"status":"inProgress"}}}}}}'
printf '%s\n' '{{"method":"item/reasoning/summaryTextDelta","params":{{"threadId":"thr-refresh","turnId":"turn-refresh","itemId":"why","summaryIndex":0,"delta":"still thinking"}}}}'
printf '%s\n' '{{"method":"thread/tokenUsage/updated","params":{{"threadId":"thr-refresh","turnId":"turn-refresh","tokenUsage":{{"last":{{"cachedInputTokens":0,"inputTokens":20,"outputTokens":0,"reasoningOutputTokens":0,"totalTokens":20}},"total":{{"cachedInputTokens":0,"inputTokens":20,"outputTokens":0,"reasoningOutputTokens":0,"totalTokens":20}},"modelContextWindow":100}}}}}}'
printf '%s\n' '{{"method":"account/updated","params":{{"authMode":"chatgpt"}}}}'
IFS= read -r refreshed_account
case "$refreshed_account" in *'"method":"account/read"'*) ;; *) exit 89 ;; esac
printf '%s\n' '{{"id":6,"result":{{"account":{{"type":"chatgpt","email":"user@example.com"}},"requiresOpenaiAuth":true}}}}'
printf '%s\n' '{{"method":"item/agentMessage/delta","params":{{"threadId":"thr-refresh","turnId":"turn-refresh","itemId":"answer","delta":"done"}}}}'
printf '%s\n' '{{"method":"turn/completed","params":{{"threadId":"thr-refresh","turn":{{"id":"turn-refresh","items":[],"status":"completed"}}}}}}'
if IFS= read -r unexpected; then
  case "$unexpected" in *'"method":"thread/resume"'*) exit 90 ;; *) exit 91 ;; esac
fi
"#
    );
    let mut backend = BackendCoordinator::new(
        session(temp.path(), &body).await,
        MemoryPreferences::new(PreferencesV4::default()),
        RecordingBrowser::default(),
    );
    backend.startup().await.unwrap();
    backend
        .handle_intent(Intent::SendMessage("keep working".to_owned()))
        .await
        .unwrap();

    assert!(backend.pump_event().await.unwrap());
    assert!(backend.pump_event().await.unwrap());
    assert_eq!(backend.state().thinking.entries[0].text, "still thinking");
    assert_eq!(backend.state().context_remaining_percent, Some(80));
    let transcript_before_refresh = backend.state().transcript.clone();

    assert!(backend.pump_event().await.unwrap());
    assert!(matches!(
        backend.state().thread,
        ThreadState::Ready { ref id } if id == "thr-refresh"
    ));
    assert!(matches!(
        backend.state().turn,
        TurnState::Streaming { ref turn_id, .. } if turn_id == "turn-refresh"
    ));
    assert_eq!(backend.state().transcript, transcript_before_refresh);
    assert_eq!(backend.state().thinking.entries[0].text, "still thinking");
    assert_eq!(backend.state().context_remaining_percent, Some(80));

    assert!(backend.pump_event().await.unwrap());
    assert!(backend.pump_event().await.unwrap());
    assert!(matches!(backend.state().turn, TurnState::Completed { .. }));
    assert!(backend
        .state()
        .transcript
        .iter()
        .any(|entry| entry.role == TranscriptRole::Assistant && entry.text == "done"));

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
    let preferences = MemoryPreferences::new(PreferencesV4 {
        codex: CodexPreferencesV2 {
            account_scope: AccountScope::from_chatgpt_email("user@example.com"),
            auto_resume_thread_id: Some("thr-saved".to_owned()),
            model_id: Some("m1".to_owned()),
            reasoning_effort: Some("high".to_owned()),
            ..CodexPreferencesV2::default()
        },
        ..PreferencesV4::default()
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
    let stale_preferences = MemoryPreferences::new(PreferencesV4 {
        codex: CodexPreferencesV2 {
            account_scope: AccountScope::from_chatgpt_email("user@example.com"),
            auto_resume_thread_id: Some("thr-stale".to_owned()),
            model_id: Some("m1".to_owned()),
            reasoning_effort: Some("high".to_owned()),
            ..CodexPreferencesV2::default()
        },
        ..PreferencesV4::default()
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
    assert_eq!(
        saved.value().codex.auto_resume_thread_id.as_deref(),
        Some("thr-stale")
    );
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
    let mismatch_preferences = MemoryPreferences::new(PreferencesV4 {
        codex: CodexPreferencesV2 {
            account_scope: AccountScope::from_chatgpt_email("old@example.com"),
            auto_resume_thread_id: Some("thr-other-account".to_owned()),
            model_id: Some("m1".to_owned()),
            reasoning_effort: Some("high".to_owned()),
            ..CodexPreferencesV2::default()
        },
        ..PreferencesV4::default()
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
