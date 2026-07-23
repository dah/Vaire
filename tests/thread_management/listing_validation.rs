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
