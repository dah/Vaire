use super::*;

#[test]
fn v2_failed_partial_round_trips_and_reopen_does_not_rewrite_source() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("openrouter");
    let store = FileOpenRouterStore::new(&root).unwrap();
    let mut conversation = completed_conversation();
    conversation.turns.push(turn(
        OpenRouterTurnOutcome::Failed,
        None,
        Some("retained partial"),
    ));
    store.save_conversation(&conversation).unwrap();
    let path = conversation_path(&root, &conversation.id);
    let before_reopen = fs::read(&path).unwrap();
    drop(store);

    let reopened = FileOpenRouterStore::new(&root).unwrap();
    let loaded = reopened.load_conversation(&conversation.id).unwrap();
    assert_eq!(loaded, conversation);
    assert_eq!(
        loaded.turns[1].incomplete_assistant_text.as_deref(),
        Some("retained partial")
    );
    assert_eq!(fs::read(&path).unwrap(), before_reopen);
    drop(reopened);

    let reopened_again = FileOpenRouterStore::new(&root).unwrap();
    assert_eq!(
        reopened_again.load_conversation(&conversation.id).unwrap(),
        conversation
    );
    assert_eq!(fs::read(path).unwrap(), before_reopen);
}

#[test]
fn committed_source_and_delete_are_not_lost_to_directory_sync_failures() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("openrouter");
    let store = FileOpenRouterStore::with_directory_sync(
        &root,
        Arc::new(ScriptedDirectorySync::fail_after(0)),
    )
    .unwrap();
    let conversation = completed_conversation();

    let saved = store.save_conversation_with_commit(&conversation).unwrap();
    assert_eq!(saved.source, CommitStatus::CommittedUnverified);
    assert_eq!(saved.index, OpenRouterIndexMaintenance::CommittedUnverified);
    assert_eq!(
        store.load_conversation(&conversation.id).unwrap(),
        conversation
    );

    let deleted = store
        .delete_conversation_with_commit(&conversation.id)
        .unwrap();
    assert_eq!(deleted.source, CommitStatus::CommittedUnverified);
    assert!(matches!(
        store.load_conversation(&conversation.id),
        Err(error) if error.category() == OpenRouterStoreFailureCategory::NotFound
    ));
    assert!(store.list_conversations().unwrap().is_empty());
}

#[test]
fn atomically_round_trips_catalog_and_history_with_owner_only_modes() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("openrouter");
    let store = FileOpenRouterStore::new(&root).unwrap();
    let catalog = vec![OpenRouterModel {
        id: "vendor/model".to_owned(),
        name: Some("Model".to_owned()),
        context_length: Some(4096),
    }];
    store.save_catalog(7, &catalog).unwrap();
    assert_eq!(store.load_catalog().unwrap(), Some((7, catalog)));
    let conversation = completed_conversation();
    store.save_conversation(&conversation).unwrap();
    assert_eq!(
        store.load_conversation(&conversation.id).unwrap(),
        conversation
    );
    assert_eq!(store.list_conversations().unwrap().len(), 1);
    assert_eq!(
        fs::metadata(&root).unwrap().permissions().mode() & 0o7777,
        0o700
    );
    assert_eq!(
        fs::metadata(root.join("catalog.json"))
            .unwrap()
            .permissions()
            .mode()
            & 0o7777,
        0o600
    );
    assert_eq!(
        fs::metadata(conversation_path(&root, &conversation.id))
            .unwrap()
            .permissions()
            .mode()
            & 0o7777,
        0o600
    );
}
