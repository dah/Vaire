use super::{
    AccountScope, ClaudePreferencesV3, CodexPreferencesV2, FilePreferences, LoadNotice,
    OpenRouterPreferencesV2, PreferencesPort, PreferencesV3, MAX_PREFERENCES_BYTES,
    PREFERENCES_VERSION,
};
use crate::provider::{ClaudeModelAlias, ClaudeSessionId, OpenRouterConversationId, ProviderId};
use crate::storage::{CommitStatus, ScriptedDirectorySync};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::os::unix::fs::symlink;
use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;
use tempfile::tempdir;

fn scope() -> AccountScope {
    AccountScope::from_chatgpt_email("user@example.com").unwrap()
}

#[test]
fn rename_commit_is_reported_when_directory_sync_fails() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("preferences.json");
    let store =
        FilePreferences::with_directory_sync(&path, Arc::new(ScriptedDirectorySync::fail_after(0)));

    assert_eq!(
        store.save_with_commit(&PreferencesV3::default()).unwrap(),
        CommitStatus::CommittedUnverified
    );
    assert_eq!(store.load().unwrap().preferences, PreferencesV3::default());
}

#[test]
fn round_trips_v3_atomically_with_owner_only_permissions() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("state").join("preferences.json");
    let store = FilePreferences::new(&path);
    let preferences = PreferencesV3 {
        version: PREFERENCES_VERSION,
        active_provider: ProviderId::Codex,
        codex: CodexPreferencesV2 {
            account_scope: AccountScope::from_chatgpt_email(" USER@Example.COM "),
            auto_resume_thread_id: Some("thr-1".to_owned()),
            model_id: Some("model-1".to_owned()),
            reasoning_effort: Some("high".to_owned()),
            thread_account_scopes: BTreeMap::from([("thr-1".to_owned(), scope())]),
        },
        openrouter: OpenRouterPreferencesV2 {
            selected_model_id: Some("anthropic/claude".to_owned()),
            enabled_model_ids: BTreeSet::from(["anthropic/claude".to_owned()]),
            ..OpenRouterPreferencesV2::default()
        },
        claude: ClaudePreferencesV3 {
            auto_resume_session_id: None,
            selected_model_alias: Some(ClaudeModelAlias::Sonnet),
        },
    };
    store.save(&preferences).unwrap();
    assert_eq!(store.load().unwrap().preferences, preferences);
    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(
        fs::metadata(path.parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
}

#[test]
fn migrates_every_v1_field_exactly_and_marks_it_for_atomic_resave() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("preferences.json");
    fs::write(
        &path,
        br#"{
          "version": 1,
          "account_scope": {"kind":"chatgpt_email","value":"user@example.com"},
          "thread_id": "thr-v1",
          "model_id": "model-v1",
          "reasoning_effort": "high",
          "thread_account_scopes": {
            "thr-v1": {"kind":"chatgpt_email","value":"user@example.com"},
            "thr-old": {"kind":"chatgpt_email","value":"user@example.com"}
          }
        }"#,
    )
    .unwrap();

    let outcome = FilePreferences::new(&path).load().unwrap();
    assert_eq!(outcome.notice, Some(LoadNotice::MigratedV1));
    assert!(outcome.may_overwrite);
    assert!(outcome.needs_save);
    assert_eq!(outcome.preferences.version, 3);
    assert_eq!(outcome.preferences.active_provider, ProviderId::Codex);
    assert_eq!(
        outcome.preferences.codex,
        CodexPreferencesV2 {
            account_scope: Some(scope()),
            auto_resume_thread_id: Some("thr-v1".to_owned()),
            model_id: Some("model-v1".to_owned()),
            reasoning_effort: Some("high".to_owned()),
            thread_account_scopes: BTreeMap::from([
                ("thr-old".to_owned(), scope()),
                ("thr-v1".to_owned(), scope()),
            ]),
        }
    );
    assert_eq!(
        outcome.preferences.openrouter,
        OpenRouterPreferencesV2::default()
    );
    assert_eq!(outcome.preferences.claude, ClaudePreferencesV3::default());
}

#[test]
fn migrates_every_v2_field_and_defaults_claude_preferences() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("preferences.json");
    fs::write(
        &path,
        br#"{
          "version": 2,
          "active_provider": "open_router",
          "codex": {
            "account_scope": null,
            "auto_resume_thread_id": null,
            "model_id": "gpt-5",
            "reasoning_effort": "high",
            "thread_account_scopes": {}
          },
          "openrouter": {
            "auto_resume_conversation_id": "or_00000000000000000000000000000000",
            "selected_model_id": "anthropic/claude",
            "enabled_model_ids": ["anthropic/claude"]
          }
        }"#,
    )
    .unwrap();

    let before = fs::read(&path).unwrap();
    let outcome = FilePreferences::new(&path).load().unwrap();
    assert_eq!(fs::read(&path).unwrap(), before);
    assert_eq!(outcome.notice, Some(LoadNotice::MigratedV2));
    assert!(outcome.may_overwrite);
    assert!(outcome.needs_save);
    assert_eq!(outcome.preferences.version, 3);
    assert_eq!(outcome.preferences.active_provider, ProviderId::OpenRouter);
    assert_eq!(outcome.preferences.codex.model_id.as_deref(), Some("gpt-5"));
    assert_eq!(
        outcome.preferences.openrouter.selected_model_id.as_deref(),
        Some("anthropic/claude")
    );
    assert_eq!(outcome.preferences.claude, ClaudePreferencesV3::default());
}

#[test]
fn missing_corrupt_and_unknown_versions_are_safe() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("preferences.json");
    let store = FilePreferences::new(&path);
    assert_eq!(store.load().unwrap().notice, Some(LoadNotice::Missing));

    fs::write(&path, b"{not-json").unwrap();
    let corrupt = store.load().unwrap();
    assert_eq!(corrupt.notice, Some(LoadNotice::Corrupt));
    assert!(!corrupt.may_overwrite);

    fs::write(&path, br#"{"version":99}"#).unwrap();
    let future = store.load().unwrap();
    assert_eq!(future.notice, Some(LoadNotice::UnsupportedVersion(99)));
    assert!(!future.may_overwrite);
    assert!(!future.needs_save);
}

#[test]
fn enforces_the_single_active_provider_resume_pointer_invariant() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("preferences.json");
    let store = FilePreferences::new(&path);
    let conversation: OpenRouterConversationId =
        "or_00000000000000000000000000000000".parse().unwrap();

    let mut codex = PreferencesV3::default();
    codex.codex.auto_resume_thread_id = Some("thr".to_owned());
    codex.openrouter.auto_resume_conversation_id = Some(conversation.clone());
    assert!(store.save(&codex).is_err());

    let mut openrouter = PreferencesV3 {
        active_provider: ProviderId::OpenRouter,
        ..PreferencesV3::default()
    };
    openrouter.codex.auto_resume_thread_id = Some("thr".to_owned());
    assert!(store.save(&openrouter).is_err());

    openrouter.set_auto_resume_conversation(Some(conversation));
    assert_eq!(openrouter.active_provider, ProviderId::OpenRouter);
    assert!(openrouter.codex.auto_resume_thread_id.is_none());
    store.save(&openrouter).unwrap();

    openrouter.set_auto_resume_thread(Some("thr-new".to_owned()));
    assert_eq!(openrouter.active_provider, ProviderId::Codex);
    assert!(openrouter.openrouter.auto_resume_conversation_id.is_none());
    store.save(&openrouter).unwrap();

    let session: ClaudeSessionId = "00000000-0000-4000-8000-000000000000".parse().unwrap();
    openrouter.set_auto_resume_claude_session(Some(session.clone()));
    assert_eq!(openrouter.active_provider, ProviderId::Claude);
    assert!(openrouter.codex.auto_resume_thread_id.is_none());
    assert!(openrouter.openrouter.auto_resume_conversation_id.is_none());
    assert_eq!(
        openrouter.claude.auto_resume_session_id.as_ref(),
        Some(&session)
    );
    store.save(&openrouter).unwrap();

    openrouter.codex.auto_resume_thread_id = Some("cross-provider".to_owned());
    assert!(store.save(&openrouter).is_err());
    openrouter.codex.auto_resume_thread_id = None;

    let mut independent_clear = PreferencesV3::default();
    independent_clear.codex.auto_resume_thread_id = Some("thr-keep".to_owned());
    independent_clear.set_auto_resume_conversation(None);
    assert_eq!(
        independent_clear.codex.auto_resume_thread_id.as_deref(),
        Some("thr-keep")
    );
    independent_clear.set_auto_resume_thread(None);
    assert!(independent_clear.codex.auto_resume_thread_id.is_none());
}

#[test]
fn rejects_semantically_corrupt_and_oversized_preferences_without_overwriting_them() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("preferences.json");
    let store = FilePreferences::new(&path);
    let malformed = [
        br#"{"version":1,"account_scope":{"kind":"chatgpt_email","value":" USER@example.com "},"thread_id":null,"model_id":null,"reasoning_effort":null}"#.as_slice(),
        br#"{"version":1,"account_scope":{"kind":"chatgpt_email","value":"a@example.com"},"thread_id":"thr","model_id":null,"reasoning_effort":null,"thread_account_scopes":{"thr":{"kind":"chatgpt_email","value":"b@example.com"}}}"#.as_slice(),
        br#"{"version":2,"active_provider":"codex","codex":{"account_scope":null,"auto_resume_thread_id":null,"model_id":" ","reasoning_effort":null,"thread_account_scopes":{}},"openrouter":{"auto_resume_conversation_id":null,"selected_model_id":null,"enabled_model_ids":[]}}"#.as_slice(),
        br#"{"version":3,"active_provider":"claude","codex":{"account_scope":null,"auto_resume_thread_id":null,"model_id":null,"reasoning_effort":null,"thread_account_scopes":{}},"openrouter":{"auto_resume_conversation_id":null,"selected_model_id":null,"enabled_model_ids":[]},"claude":{"auto_resume_session_id":null,"selected_model_alias":"unknown"}}"#.as_slice(),
        br#"{"version":3,"active_provider":"codex","codex":{"account_scope":null,"auto_resume_thread_id":null,"model_id":null,"reasoning_effort":null,"thread_account_scopes":{}},"openrouter":{"auto_resume_conversation_id":null,"selected_model_id":null,"enabled_model_ids":[]},"claude":{"auto_resume_session_id":"00000000-0000-4000-8000-000000000000","selected_model_alias":"default"}}"#.as_slice(),
        br#"{"version":4294967297}"#.as_slice(),
    ];
    for bytes in malformed {
        fs::write(&path, bytes).unwrap();
        let outcome = store.load().unwrap();
        assert_eq!(outcome.notice, Some(LoadNotice::Corrupt));
        assert!(!outcome.may_overwrite);
    }

    fs::write(&path, vec![b' '; MAX_PREFERENCES_BYTES + 1]).unwrap();
    assert_eq!(store.load().unwrap().notice, Some(LoadNotice::Corrupt));

    let valid = PreferencesV3::default();
    store.save(&valid).unwrap();
    let before = fs::read(&path).unwrap();
    let mut too_large = valid;
    too_large.codex.model_id = Some("m".repeat(MAX_PREFERENCES_BYTES));
    assert!(store.save(&too_large).is_err());
    assert_eq!(fs::read(&path).unwrap(), before);
}

#[test]
fn serialized_v3_has_no_credential_or_runtime_secret_fields() {
    let json = serde_json::to_string(&PreferencesV3::default()).unwrap();
    for forbidden in [
        "api_key",
        "credential",
        "authorization",
        "transcript",
        "context_remaining",
    ] {
        assert!(!json.contains(forbidden));
    }
}

#[test]
fn atomic_save_never_follows_a_predictable_legacy_temp_symlink() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("preferences.json");
    let victim = temp.path().join("victim.txt");
    fs::write(&victim, b"must remain unchanged").unwrap();
    let legacy_temp = temp
        .path()
        .join(format!(".preferences.json.{}.tmp", std::process::id()));
    symlink(&victim, &legacy_temp).unwrap();

    let store = FilePreferences::new(&path);
    store.save(&PreferencesV3::default()).unwrap();

    assert_eq!(fs::read(&victim).unwrap(), b"must remain unchanged");
    assert!(!fs::symlink_metadata(&path)
        .unwrap()
        .file_type()
        .is_symlink());
    assert_eq!(store.load().unwrap().preferences, PreferencesV3::default());
}

#[test]
fn failed_atomic_replace_preserves_the_target_and_cleans_its_temp_file() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("preferences.json");
    fs::create_dir(&path).unwrap();
    fs::write(path.join("sentinel"), b"preserve me").unwrap();
    let store = FilePreferences::new(&path);

    assert!(store.save(&PreferencesV3::default()).is_err());

    assert_eq!(fs::read(path.join("sentinel")).unwrap(), b"preserve me");
    let temp_prefix = format!(".preferences.json.{}.", std::process::id());
    assert!(fs::read_dir(temp.path()).unwrap().all(|entry| {
        !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(&temp_prefix)
    }));
}

#[test]
fn account_scope_constructor_rejects_non_identity_and_normalizes_valid_email() {
    assert_eq!(
        AccountScope::from_chatgpt_email(" USER@Example.COM "),
        Some(AccountScope::ChatgptEmail("user@example.com".to_owned()))
    );
    for invalid in [
        "",
        "   ",
        "a b@example.com",
        "a\nb@example.com",
        "spoof\u{202e}@example.com",
    ] {
        assert_eq!(AccountScope::from_chatgpt_email(invalid), None);
    }
}
