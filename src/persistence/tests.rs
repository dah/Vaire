use super::{
    AccountScope, FilePreferences, LoadNotice, PreferencesPort, PreferencesV1,
    MAX_PREFERENCES_BYTES, PREFERENCES_VERSION,
};
use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::symlink;
use std::os::unix::fs::PermissionsExt;
use tempfile::tempdir;

#[test]
fn round_trips_atomically_with_owner_only_permissions() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("state").join("preferences.json");
    let store = FilePreferences::new(&path);
    let preferences = PreferencesV1 {
        version: PREFERENCES_VERSION,
        account_scope: AccountScope::from_chatgpt_email(" USER@Example.COM "),
        thread_id: Some("thr-1".to_owned()),
        model_id: Some("model-1".to_owned()),
        reasoning_effort: Some("high".to_owned()),
        thread_account_scopes: BTreeMap::from([(
            "thr-1".to_owned(),
            AccountScope::from_chatgpt_email("user@example.com").unwrap(),
        )]),
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
fn missing_corrupt_and_unknown_versions_are_clean_first_runs() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("preferences.json");
    let store = FilePreferences::new(&path);
    assert_eq!(store.load().unwrap().notice, Some(LoadNotice::Missing));
    fs::write(
            &path,
            br#"{"version":1,"account_scope":null,"thread_id":null,"model_id":null,"reasoning_effort":null}"#,
        )
        .unwrap();
    let prior_v1 = store.load().unwrap();
    assert_eq!(prior_v1.notice, None);
    assert!(prior_v1.preferences.thread_account_scopes.is_empty());
    fs::write(&path, b"{not-json").unwrap();
    let corrupt = store.load().unwrap();
    assert_eq!(corrupt.notice, Some(LoadNotice::Corrupt));
    assert!(!corrupt.may_overwrite);
    fs::write(&path, br#"{"version":99}"#).unwrap();
    let future = store.load().unwrap();
    assert_eq!(future.notice, Some(LoadNotice::UnsupportedVersion(99)));
    assert!(!future.may_overwrite);
}

#[test]
fn rejects_semantically_corrupt_and_oversized_preferences_without_overwriting_them() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("preferences.json");
    let store = FilePreferences::new(&path);
    let malformed = [
            br#"{"version":1,"account_scope":{"kind":"chatgpt_email","value":" USER@example.com "},"thread_id":null,"model_id":null,"reasoning_effort":null}"#.as_slice(),
            br#"{"version":1,"account_scope":{"kind":"chatgpt_email","value":"a@example.com"},"thread_id":"thr","model_id":null,"reasoning_effort":null,"thread_account_scopes":{"thr":{"kind":"chatgpt_email","value":"b@example.com"}}}"#.as_slice(),
            br#"{"version":1,"account_scope":null,"thread_id":" ","model_id":null,"reasoning_effort":null}"#.as_slice(),
            br#"{"version":4294967297}"#.as_slice(),
        ];
    for bytes in malformed {
        fs::write(&path, bytes).unwrap();
        let outcome = store.load().unwrap();
        assert_eq!(outcome.notice, Some(LoadNotice::Corrupt));
        assert!(!outcome.may_overwrite);
    }

    fs::write(&path, vec![b' '; MAX_PREFERENCES_BYTES + 1]).unwrap();
    let oversized = store.load().unwrap();
    assert_eq!(oversized.notice, Some(LoadNotice::Corrupt));
    assert!(!oversized.may_overwrite);

    let valid = PreferencesV1::default();
    store.save(&valid).unwrap();
    let before = fs::read(&path).unwrap();
    let mut too_large = valid;
    too_large.model_id = Some("m".repeat(MAX_PREFERENCES_BYTES));
    assert!(store.save(&too_large).is_err());
    assert_eq!(fs::read(&path).unwrap(), before);
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
    store.save(&PreferencesV1::default()).unwrap();

    assert_eq!(fs::read(&victim).unwrap(), b"must remain unchanged");
    assert!(!fs::symlink_metadata(&path)
        .unwrap()
        .file_type()
        .is_symlink());
    assert_eq!(store.load().unwrap().preferences, PreferencesV1::default());
}

#[test]
fn failed_atomic_replace_preserves_the_target_and_cleans_its_temp_file() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("preferences.json");
    fs::create_dir(&path).unwrap();
    fs::write(path.join("sentinel"), b"preserve me").unwrap();
    let store = FilePreferences::new(&path);

    assert!(store.save(&PreferencesV1::default()).is_err());

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
