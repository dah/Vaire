use super::*;

#[test]
fn v2_validation_enforces_the_complete_outcome_matrix_and_partial_bound() {
    let mut conversation = completed_conversation();
    assert!(validate_conversation_v2(&conversation).is_ok());

    conversation.turns[0].assistant_text = Some(String::new());
    assert!(validate_conversation_v2(&conversation).is_ok());
    conversation.turns[0].assistant_text = None;
    assert_eq!(
        validate_conversation_v2(&conversation)
            .unwrap_err()
            .category(),
        OpenRouterStoreFailureCategory::Corrupt
    );

    conversation.turns[0] = turn(
        OpenRouterTurnOutcome::Completed,
        Some("done"),
        Some("partial"),
    );
    assert_eq!(
        validate_conversation_v2(&conversation)
            .unwrap_err()
            .category(),
        OpenRouterStoreFailureCategory::Corrupt
    );

    conversation.turns[0] = turn(OpenRouterTurnOutcome::Failed, None, None);
    assert!(validate_conversation_v2(&conversation).is_ok());
    conversation.turns[0].incomplete_assistant_text = Some("partial".to_owned());
    assert!(validate_conversation_v2(&conversation).is_ok());
    conversation.turns[0].incomplete_assistant_text = Some(String::new());
    assert_eq!(
        validate_conversation_v2(&conversation)
            .unwrap_err()
            .category(),
        OpenRouterStoreFailureCategory::Corrupt
    );
    conversation.turns[0] = turn(OpenRouterTurnOutcome::Failed, Some("done"), None);
    assert_eq!(
        validate_conversation_v2(&conversation)
            .unwrap_err()
            .category(),
        OpenRouterStoreFailureCategory::Corrupt
    );

    for outcome in [
        OpenRouterTurnOutcome::Interrupted,
        OpenRouterTurnOutcome::InProgress,
    ] {
        conversation.turns[0] = turn(outcome, None, Some("partial"));
        assert_eq!(
            validate_conversation_v2(&conversation)
                .unwrap_err()
                .category(),
            OpenRouterStoreFailureCategory::Corrupt
        );
    }

    conversation.turns[0] = turn(OpenRouterTurnOutcome::Failed, None, None);
    conversation.turns[0].incomplete_assistant_text = Some("x".repeat(MAX_ASSISTANT_BYTES + 1));
    assert_eq!(
        validate_conversation_v2(&conversation)
            .unwrap_err()
            .category(),
        OpenRouterStoreFailureCategory::ResourceLimit
    );
}

#[test]
fn canonical_history_keeps_completed_semantics_and_excludes_failed_partial() {
    let mut conversation = completed_conversation();
    let mut failed = turn(OpenRouterTurnOutcome::Failed, None, Some("display only"));
    failed.user_text = "again".to_owned();
    conversation.turns.push(failed);

    let messages = conversation.canonical_messages();
    assert_eq!(
        messages
            .iter()
            .map(|message| (&message.role, message.content.as_str()))
            .collect::<Vec<_>>(),
        vec![
            (&ChatRole::User, "hello"),
            (&ChatRole::Assistant, "world"),
            (&ChatRole::User, "again"),
        ]
    );
}

#[test]
fn unsupported_and_corrupt_sources_remain_byte_identical_and_unlisted() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("openrouter");
    drop(FileOpenRouterStore::new(&root).unwrap());

    let fixtures = [
        (
            0,
            br#"{"version":3,"id":"or_00000000000000000000000000000000"}"#.as_slice(),
            OpenRouterStoreFailureCategory::UnsupportedVersion,
        ),
        (
            1,
            br#"{"version":0,"id":"or_00000000000000000000000000000001"}"#.as_slice(),
            OpenRouterStoreFailureCategory::UnsupportedVersion,
        ),
        (
            2,
            br#"{"version":"2","id":"or_00000000000000000000000000000002"}"#.as_slice(),
            OpenRouterStoreFailureCategory::Corrupt,
        ),
        (
            3,
            br#"not json"#.as_slice(),
            OpenRouterStoreFailureCategory::Corrupt,
        ),
        (
            4,
            br#"{"version":1,"id":"or_00000000000000000000000000000004","created_at_ms":1,"updated_at_ms":1,"title":"Legacy","turns":[],"unknown":true}"#.as_slice(),
            OpenRouterStoreFailureCategory::Corrupt,
        ),
        (
            5,
            br#"{"id":"or_00000000000000000000000000000005"}"#.as_slice(),
            OpenRouterStoreFailureCategory::Corrupt,
        ),
        (
            6,
            br#"{"version":2,"id":"or_00000000000000000000000000000006","created_at_ms":1,"updated_at_ms":1,"title":"V2","turns":[],"unknown":true}"#.as_slice(),
            OpenRouterStoreFailureCategory::Corrupt,
        ),
        (
            7,
            br#"{"version":1,"id":"or_00000000000000000000000000000007","created_at_ms":1,"updated_at_ms":1,"title":"Legacy","turns":[{"id":"ort_00000000000000000000000000000007","model_id":"vendor/model","user_text":"failed","outcome":"failed"}]}"#.as_slice(),
            OpenRouterStoreFailureCategory::Corrupt,
        ),
        (
            8,
            br#"{"version":4294967296,"id":"or_00000000000000000000000000000008"}"#.as_slice(),
            OpenRouterStoreFailureCategory::UnsupportedVersion,
        ),
        (
            9,
            br#"{"version":-1,"id":"or_00000000000000000000000000000009"}"#.as_slice(),
            OpenRouterStoreFailureCategory::Corrupt,
        ),
    ];
    let mut saved = Vec::new();
    for (suffix, bytes, category) in fixtures {
        let id: OpenRouterConversationId = format!("or_{suffix:032x}").parse().unwrap();
        let path = conversation_path(&root, &id);
        write_owner_only(&path, bytes);
        saved.push((id, path, bytes.to_vec(), category));
    }

    let store = FileOpenRouterStore::new(&root).unwrap();
    assert!(store.list_conversations().unwrap().is_empty());
    for (id, path, original, category) in &saved {
        assert_eq!(fs::read(path).unwrap(), *original);
        assert_eq!(
            store.load_conversation(id).unwrap_err().category(),
            *category
        );
        let replacement = OpenRouterConversationV2::new(id.clone(), 1, "Replacement");
        assert_eq!(
            store
                .save_conversation(&replacement)
                .unwrap_err()
                .category(),
            *category
        );
        assert_eq!(fs::read(path).unwrap(), *original);
    }
    drop(store);

    let reopened = FileOpenRouterStore::new(&root).unwrap();
    assert!(reopened.list_conversations().unwrap().is_empty());
    for (_, path, original, _) in saved {
        assert_eq!(fs::read(path).unwrap(), original);
    }
}

#[test]
fn missing_v2_assistant_text_is_rejected_without_rewriting_or_listing() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("openrouter");
    drop(FileOpenRouterStore::new(&root).unwrap());

    let id = OpenRouterConversationId::new();
    let path = conversation_path(&root, &id);
    let malformed = serde_json::to_vec_pretty(&serde_json::json!({
        "version": 2,
        "id": id,
        "created_at_ms": 1,
        "updated_at_ms": 1,
        "title": "Malformed V2",
        "turns": [{
            "id": OpenRouterTurnId::new(),
            "model_id": "vendor/model",
            "user_text": "hello",
            "outcome": "in_progress"
        }]
    }))
    .unwrap();
    write_owner_only(&path, &malformed);

    let store = FileOpenRouterStore::new(&root).unwrap();
    assert_eq!(fs::read(&path).unwrap(), malformed);
    assert!(store.list_conversations().unwrap().is_empty());
    assert_eq!(fs::read(&path).unwrap(), malformed);
    assert_eq!(
        store.load_conversation(&id).unwrap_err().category(),
        OpenRouterStoreFailureCategory::Corrupt
    );
    assert_eq!(fs::read(&path).unwrap(), malformed);

    let replacement = OpenRouterConversationV2::new(id.clone(), 2, "Replacement");
    assert_eq!(
        store
            .save_conversation(&replacement)
            .unwrap_err()
            .category(),
        OpenRouterStoreFailureCategory::Corrupt
    );
    assert_eq!(fs::read(&path).unwrap(), malformed);
    assert_eq!(
        store
            .save_conversation_with_commit(&replacement)
            .unwrap_err()
            .category(),
        OpenRouterStoreFailureCategory::Corrupt
    );
    assert_eq!(fs::read(&path).unwrap(), malformed);
    drop(store);

    let reopened = FileOpenRouterStore::new(&root).unwrap();
    assert!(reopened.list_conversations().unwrap().is_empty());
    assert_eq!(fs::read(path).unwrap(), malformed);
}

#[test]
fn corrupt_active_file_fails_without_exposing_contents() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("openrouter");
    let store = FileOpenRouterStore::new(&root).unwrap();
    let corrupt_id: OpenRouterConversationId =
        "or_00000000000000000000000000000000".parse().unwrap();
    let path = conversation_path(&root, &corrupt_id);
    write_owner_only(&path, b"secret-looking-corrupt-body");

    assert_eq!(
        store.load_conversation(&corrupt_id).unwrap_err().category(),
        OpenRouterStoreFailureCategory::Corrupt
    );
    assert!(
        !format!("{:?}", store.load_conversation(&corrupt_id).unwrap_err())
            .contains("secret-looking")
    );
}

#[test]
fn history_limit_failure_preserves_the_last_valid_atomic_file() {
    let temp = tempfile::tempdir().unwrap();
    let store = FileOpenRouterStore::new(temp.path().join("openrouter")).unwrap();
    let mut conversation = completed_conversation();
    store.save_conversation(&conversation).unwrap();
    let original = store.load_conversation(&conversation.id).unwrap();
    conversation.turns[0].user_text = "x".repeat(MAX_USER_BYTES + 1);
    assert_eq!(
        store
            .save_conversation(&conversation)
            .unwrap_err()
            .category(),
        OpenRouterStoreFailureCategory::ResourceLimit
    );
    assert_eq!(store.load_conversation(&conversation.id).unwrap(), original);
}
