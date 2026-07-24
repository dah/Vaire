use super::*;

#[test]
fn eager_v1_migration_is_lossless_repairs_in_progress_and_preserves_modes() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("openrouter");
    drop(FileOpenRouterStore::new(&root).unwrap());

    let id = OpenRouterConversationId::new();
    let path = conversation_path(&root, &id);
    let legacy = legacy_fixture(&id);
    let legacy_value: Value = serde_json::from_slice(&legacy).unwrap();
    write_owner_only(&path, &legacy);

    let store = FileOpenRouterStore::new(&root).unwrap();
    let migrated = store.load_conversation(&id).unwrap();
    assert_eq!(migrated.version, 2);
    assert_eq!(migrated.created_at_ms, 11);
    assert_eq!(migrated.updated_at_ms, 22);
    assert_eq!(migrated.title, "Legacy");
    assert_eq!(migrated.turns.len(), 4);
    for (migrated_turn, legacy_turn) in migrated
        .turns
        .iter()
        .zip(legacy_value["turns"].as_array().unwrap())
    {
        assert_eq!(
            migrated_turn.id.as_str(),
            legacy_turn["id"].as_str().unwrap()
        );
        assert_eq!(
            migrated_turn.model_id,
            legacy_turn["model_id"].as_str().unwrap()
        );
        assert_eq!(
            migrated_turn.user_text,
            legacy_turn["user_text"].as_str().unwrap()
        );
        assert_eq!(
            migrated_turn.assistant_text.as_deref(),
            legacy_turn["assistant_text"].as_str()
        );
        assert_eq!(migrated_turn.incomplete_assistant_text, None);
    }
    assert_eq!(migrated.turns[0].user_text, "completed user");
    assert_eq!(
        migrated.turns[0].assistant_text.as_deref(),
        Some("completed assistant")
    );
    assert_eq!(migrated.turns[1].outcome, OpenRouterTurnOutcome::Failed);
    assert_eq!(migrated.turns[1].assistant_text, None);
    assert_eq!(migrated.turns[1].incomplete_assistant_text, None);
    assert_eq!(
        migrated.turns[2].outcome,
        OpenRouterTurnOutcome::Interrupted
    );
    assert_eq!(
        migrated.turns[3].outcome,
        OpenRouterTurnOutcome::Interrupted
    );
    assert!(migrated
        .turns
        .iter()
        .all(|turn| turn.incomplete_assistant_text.is_none()));
    assert_eq!(
        migrated
            .canonical_messages()
            .iter()
            .map(|message| message.content.as_str())
            .collect::<Vec<_>>(),
        vec![
            "completed user",
            "completed assistant",
            "failed user",
            "interrupted user",
            "in progress user",
        ]
    );
    assert_eq!(store.list_conversations().unwrap().len(), 1);
    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o7777,
        0o600
    );
    let written: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert_eq!(written["version"], 2);
}

#[test]
fn load_fallback_migrates_v1_introduced_after_startup() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("openrouter");
    let store = FileOpenRouterStore::new(&root).unwrap();
    let id = OpenRouterConversationId::new();
    let path = conversation_path(&root, &id);
    write_owner_only(&path, &legacy_fixture(&id));

    let migrated = store.load_conversation(&id).unwrap();
    assert_eq!(migrated.version, 2);
    assert_eq!(migrated.turns[1].outcome, OpenRouterTurnOutcome::Failed);
    assert_eq!(migrated.turns[1].incomplete_assistant_text, None);
    assert_eq!(
        migrated.turns[3].outcome,
        OpenRouterTurnOutcome::Interrupted
    );
    let written: Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
    assert_eq!(written["version"], 2);
    assert_eq!(written["turns"][3]["outcome"], "interrupted");
}

#[test]
fn list_fallback_migrates_v1_and_repairs_stale_in_progress() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("openrouter");
    let store = FileOpenRouterStore::new(&root).unwrap();
    let id = OpenRouterConversationId::new();
    let path = conversation_path(&root, &id);
    write_owner_only(&path, &legacy_fixture(&id));

    let listed = store.list_conversations().unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, id);
    let written: Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
    assert_eq!(written["version"], 2);
    assert_eq!(written["turns"][3]["outcome"], "interrupted");
}

#[test]
fn migration_resource_failure_is_order_independent_and_preserves_v1_source() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("openrouter");
    drop(FileOpenRouterStore::new(&root).unwrap());

    let id = OpenRouterConversationId::new();
    let path = conversation_path(&root, &id);
    let legacy_value: Value = serde_json::from_slice(&legacy_fixture(&id)).unwrap();
    let compact_legacy = serde_json::to_vec(&legacy_value).unwrap();
    write_owner_only(&path, &compact_legacy);

    let filler_path = root.join("conversations/or_padding.json");
    let filler_len = MAX_AGGREGATE_CONVERSATION_BYTES as usize - compact_legacy.len() - 1;
    write_owner_only(&filler_path, &vec![b'x'; filler_len]);

    assert_eq!(
        FileOpenRouterStore::new(&root).unwrap_err().category(),
        OpenRouterStoreFailureCategory::ResourceLimit
    );
    assert_eq!(fs::read(path).unwrap(), compact_legacy);
}

#[test]
fn startup_accepts_explicit_null_and_omitted_incomplete_text_then_repairs_v2() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("openrouter");
    drop(FileOpenRouterStore::new(&root).unwrap());

    let id = OpenRouterConversationId::new();
    let path = conversation_path(&root, &id);
    let source = serde_json::to_vec_pretty(&serde_json::json!({
        "version": 2,
        "id": id,
        "created_at_ms": 1,
        "updated_at_ms": 1,
        "title": "Repairable V2",
        "turns": [{
            "id": OpenRouterTurnId::new(),
            "model_id": "vendor/model",
            "user_text": "hello",
            "assistant_text": null,
            "outcome": "in_progress"
        }]
    }))
    .unwrap();
    write_owner_only(&path, &source);

    let reopened = FileOpenRouterStore::new(&root).unwrap();
    let repaired = reopened.load_conversation(&id).unwrap();
    assert_eq!(
        repaired.turns[0].outcome,
        OpenRouterTurnOutcome::Interrupted
    );
    assert_eq!(repaired.turns[0].assistant_text, None);
    assert_eq!(repaired.turns[0].incomplete_assistant_text, None);
    let written: Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
    assert_eq!(written["turns"][0]["assistant_text"], Value::Null);
    assert!(written["turns"][0]
        .get("incomplete_assistant_text")
        .is_none());
}
