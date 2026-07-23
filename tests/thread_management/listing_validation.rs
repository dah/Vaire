use super::support::*;

#[tokio::test]
async fn mixed_source_listing_rejects_foreign_cwd_and_preserves_active_thread() {
    let temp = tempdir().unwrap();
    let foreign = listed_thread(
        "thr-foreign",
        Some("Foreign"),
        "outside the dedicated conversation directory",
        &temp.path().join("other-conversation"),
        50,
        "vscode",
    );
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
IFS= read -r list_threads
printf '%s\n' '{{"id":6,"result":{{"data":[{foreign}],"nextCursor":null}}}}'
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

    let picker = backend.state().conversation_popup().unwrap();
    assert!(matches!(picker.phase, ThreadPickerPhase::Failed));
    assert!(picker
        .message
        .as_deref()
        .unwrap()
        .contains("working directory"));
    assert!(matches!(&backend.state().thread, ThreadState::Ready { id } if id == "thr-active"));
    assert_eq!(
        saved.value().codex.auto_resume_thread_id.as_deref(),
        Some("thr-active")
    );
    backend.shutdown().await.unwrap();
}

#[tokio::test]
async fn historical_listing_shows_only_registered_legacy_threads_without_creating_the_cwd() {
    let temp = tempdir().unwrap();
    let current = temp.path().join("runtime/conversation");
    let historical = temp.path().join("legacy/runtime/conversation");
    let registered = listed_thread(
        "thr-old",
        Some("Historical"),
        "registered legacy thread",
        &historical,
        20,
        "vscode",
    );
    let unregistered = listed_thread(
        "thr-unregistered",
        Some("Unregistered"),
        "must remain hidden",
        &historical,
        10,
        "appServer",
    );
    let current_json = serde_json::to_string(&current).unwrap();
    let historical_json = serde_json::to_string(&historical).unwrap();
    let body = format!(
        r#"
IFS= read -r initialize
printf '%s\n' '{INITIALIZED}'
IFS= read -r initialized
IFS= read -r account
printf '%s\n' '{ACCOUNT}'
IFS= read -r models
printf '%s\n' '{MODELS}'
IFS= read -r current_list
case "$current_list" in *'"method":"thread/list"'*'"cwd":{current_json}'*'"sourceKinds":["appServer","vscode"]'*) ;; *) exit 61 ;; esac
printf '%s\n' '{{"id":4,"result":{{"data":[],"nextCursor":null}}}}'
IFS= read -r historical_list
case "$historical_list" in *'"method":"thread/list"'*'"cwd":{historical_json}'*'"sourceKinds":["appServer","vscode"]'*) ;; *) exit 62 ;; esac
printf '%s\n' '{{"id":5,"result":{{"data":[{registered},{unregistered}],"nextCursor":null}}}}'
IFS= read -r resume_historical
case "$resume_historical" in *'"method":"thread/resume"'*'"cwd":{current_json}'*'"threadId":"thr-old"'*) ;; *) exit 63 ;; esac
printf '%s\n' '{{"id":6,"result":{{"thread":{{"id":"thr-old","turns":[]}}}}}}'
IFS= read -r read_historical
case "$read_historical" in *'"method":"thread/read"'*'"threadId":"thr-old"'*) ;; *) exit 64 ;; esac
printf '%s\n' '{{"id":7,"result":{{"thread":{{"id":"thr-old","turns":[]}}}}}}'
IFS= read -r hold
"#
    );
    let saved = MemoryPreferences::new(preferences(None));
    let mut backend = BackendCoordinator::new(
        session_with_historical(temp.path(), Some(&historical), &body).await,
        saved.clone(),
        NoopBrowser,
    );
    assert!(
        !historical.exists(),
        "historical cwd metadata must not create the old directory"
    );

    backend.startup().await.unwrap();
    backend.handle_intent(Intent::Resume).await.unwrap();

    let picker = backend.state().conversation_popup().unwrap();
    assert!(matches!(picker.phase, ThreadPickerPhase::Ready));
    assert_eq!(
        picker
            .threads
            .iter()
            .map(|thread| thread.id.as_str())
            .collect::<Vec<_>>(),
        vec!["thr-old"]
    );
    assert!(
        !saved
            .value()
            .codex
            .thread_account_scopes
            .contains_key("thr-unregistered"),
        "discovery must never auto-register an unknown thread"
    );
    backend
        .handle_intent(Intent::ThreadPickerSelect)
        .await
        .unwrap();
    assert!(matches!(&backend.state().thread, ThreadState::Ready { id } if id == "thr-old"));
    assert_eq!(
        saved.value().codex.auto_resume_thread_id.as_deref(),
        Some("thr-old")
    );
    assert!(!historical.exists());
    backend.shutdown().await.unwrap();
}

#[tokio::test]
async fn historical_listing_rejects_a_result_from_the_wrong_originating_cwd() {
    let temp = tempdir().unwrap();
    let historical = temp.path().join("legacy/runtime/conversation");
    let wrong = listed_thread(
        "thr-old",
        Some("Wrong cwd"),
        "must be rejected",
        &temp.path().join("somewhere-else"),
        10,
        "vscode",
    );
    let body = format!(
        r#"
IFS= read -r current_list
printf '%s\n' '{{"id":1,"result":{{"data":[],"nextCursor":null}}}}'
IFS= read -r historical_list
printf '%s\n' '{{"id":2,"result":{{"data":[{wrong}],"nextCursor":null}}}}'
IFS= read -r hold
"#
    );
    let mut service = session_with_historical(temp.path(), Some(&historical), &body).await;

    assert!(matches!(
        service.list_threads().await,
        Err(SessionError::Protocol(message)) if message.contains("requested Vairë working directory")
    ));
    service.shutdown().await.unwrap();
}

#[tokio::test]
async fn thread_listing_rejects_the_same_id_under_current_and_historical_cwds() {
    let temp = tempdir().unwrap();
    let current = temp.path().join("runtime/conversation");
    let historical = temp.path().join("legacy/runtime/conversation");
    let current_thread = listed_thread(
        "thr-conflict",
        Some("Current"),
        "current copy",
        &current,
        20,
        "appServer",
    );
    let historical_thread = listed_thread(
        "thr-conflict",
        Some("Historical"),
        "historical copy",
        &historical,
        10,
        "vscode",
    );
    let body = format!(
        r#"
IFS= read -r current_list
printf '%s\n' '{{"id":1,"result":{{"data":[{current_thread}],"nextCursor":null}}}}'
IFS= read -r historical_list
printf '%s\n' '{{"id":2,"result":{{"data":[{historical_thread}],"nextCursor":null}}}}'
IFS= read -r hold
"#
    );
    let mut service = session_with_historical(temp.path(), Some(&historical), &body).await;

    assert!(matches!(
        service.list_threads().await,
        Err(SessionError::Protocol(message)) if message.contains("conflicting working directories")
    ));
    service.shutdown().await.unwrap();
}

#[tokio::test]
async fn thread_cursor_cycle_tracking_is_scoped_to_each_cwd_query() {
    let temp = tempdir().unwrap();
    let historical = temp.path().join("legacy/runtime/conversation");
    let body = r#"
IFS= read -r current_one
printf '%s\n' '{"id":1,"result":{"data":[],"nextCursor":"repeat"}}'
IFS= read -r current_two
case "$current_two" in *'"cursor":"repeat"'*) ;; *) exit 71 ;; esac
printf '%s\n' '{"id":2,"result":{"data":[],"nextCursor":null}}'
IFS= read -r historical_one
case "$historical_one" in *'"cursor":null'*) ;; *) exit 72 ;; esac
printf '%s\n' '{"id":3,"result":{"data":[],"nextCursor":"repeat"}}'
IFS= read -r historical_two
case "$historical_two" in *'"cursor":"repeat"'*) ;; *) exit 73 ;; esac
printf '%s\n' '{"id":4,"result":{"data":[],"nextCursor":null}}'
IFS= read -r hold
"#;
    let mut service = session_with_historical(temp.path(), Some(&historical), body).await;

    assert!(service.list_threads().await.unwrap().is_empty());
    service.shutdown().await.unwrap();
}

#[tokio::test]
async fn historical_metadata_equal_to_the_current_cwd_is_not_queried_twice() {
    let temp = tempdir().unwrap();
    let current = temp.path().join("runtime/conversation");
    let body = r#"
IFS= read -r current_list
printf '%s\n' '{"id":1,"result":{"data":[],"nextCursor":null}}'
IFS= read -r hold
"#;
    let mut service = session_with_historical(temp.path(), Some(&current), body).await;

    assert!(service.list_threads().await.unwrap().is_empty());
    service.shutdown().await.unwrap();
}

#[tokio::test]
async fn current_and_historical_queries_share_the_global_page_ceiling() {
    let temp = tempdir().unwrap();
    let historical = temp.path().join("legacy/runtime/conversation");
    let historical_json = serde_json::to_string(&historical).unwrap();
    let body = format!(
        r#"
i=1
while [ "$i" -le 255 ]; do
  IFS= read -r request
  if [ "$i" -lt 255 ]; then
    printf '{{"id":%s,"result":{{"data":[],"nextCursor":"page-%s"}}}}\n' "$i" "$i"
  else
    printf '{{"id":%s,"result":{{"data":[],"nextCursor":null}}}}\n' "$i"
  fi
  i=$((i + 1))
done
IFS= read -r historical_list
case "$historical_list" in *'"cursor":null'*'"cwd":{historical_json}'*) ;; *) exit 81 ;; esac
printf '%s\n' '{{"id":256,"result":{{"data":[],"nextCursor":"historical-page-2"}}}}'
IFS= read -r hold
"#
    );
    let mut service = session_with_historical(temp.path(), Some(&historical), &body).await;

    assert!(matches!(
        service.list_threads().await,
        Err(SessionError::Protocol(message)) if message.contains("pagination limit")
    ));
    service.shutdown().await.unwrap();
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
    let picker = backend.state().conversation_popup().unwrap();
    assert!(matches!(picker.phase, ThreadPickerPhase::Failed));
    assert!(picker
        .message
        .as_deref()
        .unwrap()
        .contains("tested protocol"));
    assert_eq!(saved.value().codex.auto_resume_thread_id, None);
    assert!(matches!(backend.state().thread, ThreadState::None));
    backend.shutdown().await.unwrap();
}
