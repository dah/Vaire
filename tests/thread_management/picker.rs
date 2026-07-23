use super::support::*;

#[tokio::test]
async fn paginated_picker_switches_threads_and_reports_partial_bulk_deletion() {
    let temp = tempdir().unwrap();
    let cwd = temp.path().join("runtime/conversation");
    let active = listed_thread(
        "thr-active",
        Some("Current"),
        "current",
        &cwd,
        40,
        "appServer",
    );
    let old_a = listed_thread("thr-old-a", None, "Question A", &cwd, 30, "vscode");
    let old_b = listed_thread(
        "thr-old-b",
        Some("Old B"),
        "question b",
        &cwd,
        20,
        "appServer",
    );
    let old_c = listed_thread("thr-old-c", Some("Old C"), "question c", &cwd, 10, "vscode");
    let unregistered = listed_thread(
        "thr-unregistered",
        Some("Unregistered"),
        "must stay hidden",
        &cwd,
        35,
        "vscode",
    );
    let cwd_json = serde_json::to_string(&cwd).unwrap();
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
IFS= read -r list_page_one
case "$list_page_one" in *'"method":"thread/list"'*'"cursor":null'*'"cwd":{cwd_json}'*'"sortKey":"updated_at"'*'"sourceKinds":["appServer","vscode"]'*) ;; *) exit 51 ;; esac
printf '%s\n' '{{"id":6,"result":{{"data":[{active},{unregistered},{old_a}],"nextCursor":"page-2"}}}}'
IFS= read -r list_page_two
case "$list_page_two" in *'"cursor":"page-2"'*) ;; *) exit 52 ;; esac
printf '%s\n' '{{"id":7,"result":{{"data":[{old_a},{old_b},{old_c}],"nextCursor":null}}}}'
IFS= read -r resume_old_a
case "$resume_old_a" in *'"method":"thread/resume"'*'"threadId":"thr-old-a"'*) ;; *) exit 53 ;; esac
printf '%s\n' '{{"id":8,"result":{{"thread":{{"id":"thr-old-a","turns":[]}}}}}}'
IFS= read -r read_old_a
printf '%s\n' '{{"id":9,"result":{{"thread":{{"id":"thr-old-a","turns":[{{"id":"restored-turn","status":"completed","items":[{{"id":"a","type":"agentMessage","text":"restored A"}}]}}]}}}}}}'
IFS= read -r list_again
printf '%s\n' '{{"id":10,"result":{{"data":[{old_a},{old_b},{old_c},{active}],"nextCursor":null}}}}'
IFS= read -r delete_old_b
case "$delete_old_b" in *'"method":"thread/delete"'*'"threadId":"thr-old-b"'*) ;; *) exit 54 ;; esac
printf '%s\n' '{{"id":11,"result":{{}}}}'
IFS= read -r delete_old_c
case "$delete_old_c" in *'"threadId":"thr-old-c"'*) ;; *) exit 55 ;; esac
printf '%s\n' '{{"id":12,"result":{{}}}}'
IFS= read -r delete_prior_active
case "$delete_prior_active" in *'"threadId":"thr-active"'*) ;; *) exit 56 ;; esac
printf '%s\n' '{{"id":13,"error":{{"code":-32010,"message":"simulated delete failure"}}}}'
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
    assert!(
        matches!(picker.phase, ThreadPickerPhase::Ready),
        "unexpected picker state: {picker:?}"
    );
    assert_eq!(
        picker.threads.len(),
        4,
        "pagination should deduplicate thr-old-a"
    );
    assert_eq!(picker.selected, 0, "the active thread should be selected");
    assert!(
        picker.threads.iter().any(|thread| thread.id == "thr-old-a"),
        "a registered legacy vscode thread should be discoverable"
    );
    assert!(
        picker
            .threads
            .iter()
            .all(|thread| thread.id != "thr-unregistered"),
        "mixed-source discovery must retain account-scope filtering"
    );

    backend
        .handle_intent(Intent::ThreadPickerMoveDown)
        .await
        .unwrap();
    backend
        .handle_intent(Intent::ThreadPickerSelect)
        .await
        .unwrap();
    assert!(matches!(&backend.state().thread, ThreadState::Ready { id } if id == "thr-old-a"));
    assert_eq!(
        saved.value().codex.auto_resume_thread_id.as_deref(),
        Some("thr-old-a")
    );
    assert_eq!(backend.state().transcript.len(), 1);
    assert_eq!(
        backend.state().transcript[0].role,
        TranscriptRole::Assistant
    );
    assert_eq!(backend.state().transcript[0].text, "restored A");

    backend.handle_intent(Intent::Resume).await.unwrap();
    backend
        .handle_intent(Intent::ThreadPickerMoveDown)
        .await
        .unwrap();
    backend
        .handle_intent(Intent::ThreadPickerRequestDelete)
        .await
        .unwrap();
    assert!(matches!(
        backend
            .state()
            .conversation_popup()
            .and_then(|picker| picker.confirmation.as_ref()),
        Some(ThreadDeleteConfirmation::Selected { target }) if target.id == "thr-old-b"
    ));
    backend
        .handle_intent(Intent::ThreadPickerCancelDelete)
        .await
        .unwrap();
    assert!(backend
        .state()
        .conversation_popup()
        .unwrap()
        .confirmation
        .is_none());
    assert!(backend
        .state()
        .conversation_popup()
        .unwrap()
        .threads
        .iter()
        .any(|thread| thread.id == "thr-old-b"));
    backend
        .handle_intent(Intent::ThreadPickerRequestDelete)
        .await
        .unwrap();
    backend
        .handle_intent(Intent::ThreadPickerConfirmDelete)
        .await
        .unwrap();
    assert!(!backend
        .state()
        .conversation_popup()
        .unwrap()
        .threads
        .iter()
        .any(|thread| thread.id == "thr-old-b"));

    backend
        .handle_intent(Intent::ThreadPickerRequestClearInactive)
        .await
        .unwrap();
    let targets = backend
        .state()
        .conversation_popup()
        .unwrap()
        .confirmation
        .as_ref()
        .unwrap()
        .targets();
    assert_eq!(
        targets
            .iter()
            .map(|target| target.id.as_str())
            .collect::<Vec<_>>(),
        vec!["thr-old-c", "thr-active"]
    );
    backend
        .handle_intent(Intent::ThreadPickerConfirmDelete)
        .await
        .unwrap();
    let picker = backend.state().conversation_popup().unwrap();
    assert_eq!(
        picker
            .threads
            .iter()
            .map(|thread| thread.id.as_str())
            .collect::<Vec<_>>(),
        vec!["thr-old-a", "thr-active"]
    );
    assert!(picker
        .message
        .as_deref()
        .unwrap()
        .contains("Deleted 1 of 2"));
    assert!(picker
        .message
        .as_deref()
        .unwrap()
        .contains("app-server returned error -32010"));
    assert_eq!(
        saved.value().codex.auto_resume_thread_id.as_deref(),
        Some("thr-old-a")
    );
    backend.shutdown().await.unwrap();
}
