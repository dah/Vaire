use std::fs;
use std::os::unix::fs::{symlink, PermissionsExt};
use std::sync::Arc;

use tempfile::tempdir;

use super::*;
use crate::storage::{CommitStatus, ScriptedDirectorySync};

fn paths() -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
    let temp = tempdir().unwrap();
    let home = temp.path().join("runtime").join("openrouter-home");
    let credential = home.join("api-key");
    (temp, home, credential)
}

fn secret(value: &str) -> SecretValue {
    SecretValue::from_input(value).unwrap()
}

#[test]
fn secret_values_normalize_outer_input_and_never_debug_contents() {
    let value = secret("  sk-test-sensitive  ");
    assert_eq!(value.expose_bytes(), b"sk-test-sensitive");
    assert_eq!(format!("{value:?}"), "SecretValue([REDACTED])");
    for invalid in ["", "   ", "sk key", "sk\nkey", "\u{7f}", &"x".repeat(8193)] {
        assert!(SecretValue::from_input(invalid).is_err());
    }
}

#[test]
fn fake_store_is_deterministic_and_records_no_secret_content() {
    let store = FakeCredentialStore::new();
    store
        .replace(CredentialAccount::OpenRouterApiKey, secret("sk-fake"))
        .unwrap();
    let loaded = store
        .load(CredentialAccount::OpenRouterApiKey)
        .unwrap()
        .unwrap();
    assert_eq!(loaded.expose_bytes(), b"sk-fake");
    assert_eq!(
        store.operations(),
        vec![
            FakeCredentialOperation::Replace(CredentialAccount::OpenRouterApiKey),
            FakeCredentialOperation::Load(CredentialAccount::OpenRouterApiKey),
        ]
    );
    assert!(!format!("{store:?}").contains("sk-fake"));

    store.fail_next(CredentialFailureCategory::Delete);
    let error = store
        .delete(CredentialAccount::OpenRouterApiKey)
        .unwrap_err();
    assert_eq!(error.category(), CredentialFailureCategory::Delete);
    assert!(store.is_configured(CredentialAccount::OpenRouterApiKey));
}

#[test]
fn file_stores_are_bound_to_one_account_and_keep_provider_files_isolated() {
    let temp = tempdir().unwrap();
    let runtime = temp.path().join("runtime");
    let openrouter_home = runtime.join("openrouter-home");
    let openrouter_file = openrouter_home.join("api-key");
    let anthropic_home = runtime.join("anthropic-home");
    let anthropic_file = anthropic_home.join("api-key");
    let openrouter = FileCredentialStore::new(
        CredentialAccount::OpenRouterApiKey,
        &openrouter_home,
        &openrouter_file,
    )
    .unwrap();
    let anthropic = FileCredentialStore::new(
        CredentialAccount::AnthropicConsoleApiKey,
        &anthropic_home,
        &anthropic_file,
    )
    .unwrap();

    openrouter
        .replace(
            CredentialAccount::OpenRouterApiKey,
            secret("openrouter-test"),
        )
        .unwrap();
    anthropic
        .replace(
            CredentialAccount::AnthropicConsoleApiKey,
            secret("anthropic-test"),
        )
        .unwrap();

    for error in [
        openrouter
            .load(CredentialAccount::AnthropicConsoleApiKey)
            .unwrap_err(),
        openrouter
            .replace(
                CredentialAccount::AnthropicConsoleApiKey,
                secret("must-not-write"),
            )
            .unwrap_err(),
        anthropic
            .delete(CredentialAccount::OpenRouterApiKey)
            .unwrap_err(),
    ] {
        assert_eq!(error.category(), CredentialFailureCategory::Permissions);
    }
    assert_eq!(fs::read(&openrouter_file).unwrap(), b"openrouter-test");
    assert_eq!(fs::read(&anthropic_file).unwrap(), b"anthropic-test");
    assert_eq!(
        fs::metadata(&anthropic_home).unwrap().permissions().mode() & 0o7777,
        0o700
    );
    assert_eq!(
        fs::metadata(&anthropic_file).unwrap().permissions().mode() & 0o7777,
        0o600
    );
}

#[test]
fn file_store_creates_exact_owner_only_layout_and_round_trips_without_newline() {
    let (_temp, home, credential) = paths();
    let store =
        FileCredentialStore::new(CredentialAccount::OpenRouterApiKey, &home, &credential).unwrap();
    assert_eq!(
        fs::metadata(&home).unwrap().permissions().mode() & 0o7777,
        0o700
    );

    store
        .replace(CredentialAccount::OpenRouterApiKey, secret("sk-old"))
        .unwrap();
    assert_eq!(fs::read(&credential).unwrap(), b"sk-old");
    assert_eq!(
        fs::metadata(&credential).unwrap().permissions().mode() & 0o7777,
        0o600
    );

    store
        .replace(CredentialAccount::OpenRouterApiKey, secret("sk-new"))
        .unwrap();
    assert_eq!(
        store
            .load(CredentialAccount::OpenRouterApiKey)
            .unwrap()
            .unwrap()
            .expose_bytes(),
        b"sk-new"
    );
    assert!(fs::read_dir(&home).unwrap().all(|entry| !entry
        .unwrap()
        .file_name()
        .to_string_lossy()
        .ends_with(".tmp")));
}

#[test]
fn initialization_cleans_only_recognized_orphan_files() {
    let (_temp, home, credential) = paths();
    fs::create_dir_all(&home).unwrap();
    fs::set_permissions(&home, fs::Permissions::from_mode(0o700)).unwrap();
    let orphan = home.join(".api-key.123.456.tmp");
    let unrelated = home.join(".catalog.123.tmp");
    let similar_but_unrecognized = home.join(".api-key.notes.tmp");
    fs::write(&orphan, b"never read this").unwrap();
    fs::write(&unrelated, b"keep").unwrap();
    fs::write(&similar_but_unrecognized, b"also keep").unwrap();

    FileCredentialStore::new(CredentialAccount::OpenRouterApiKey, &home, &credential).unwrap();
    assert!(!orphan.exists());
    assert_eq!(fs::read(unrelated).unwrap(), b"keep");
    assert_eq!(fs::read(similar_but_unrecognized).unwrap(), b"also keep");
}

#[test]
fn file_store_rejects_unsafe_directory_file_and_content_states() {
    let (_temp, home, credential) = paths();
    fs::create_dir_all(&home).unwrap();
    fs::set_permissions(&home, fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(
        FileCredentialStore::new(CredentialAccount::OpenRouterApiKey, &home, &credential)
            .unwrap_err()
            .category(),
        CredentialFailureCategory::Permissions
    );
    fs::set_permissions(&home, fs::Permissions::from_mode(0o700)).unwrap();
    let store =
        FileCredentialStore::new(CredentialAccount::OpenRouterApiKey, &home, &credential).unwrap();

    for bytes in [
        Vec::new(),
        b"sk key".to_vec(),
        b"sk\nkey".to_vec(),
        vec![0xff],
        vec![b'x'; MAX_CREDENTIAL_BYTES + 1],
    ] {
        fs::write(&credential, bytes).unwrap();
        fs::set_permissions(&credential, fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(
            store
                .load(CredentialAccount::OpenRouterApiKey)
                .unwrap_err()
                .category(),
            CredentialFailureCategory::Corrupt
        );
    }

    fs::write(&credential, b"sk-permissions").unwrap();
    fs::set_permissions(&credential, fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(
        store
            .load(CredentialAccount::OpenRouterApiKey)
            .unwrap_err()
            .category(),
        CredentialFailureCategory::Permissions
    );
    assert_eq!(fs::read(&credential).unwrap(), b"sk-permissions");
}

#[test]
fn file_store_rejects_symlinks_and_non_regular_targets_without_following_them() {
    let (_temp, home, credential) = paths();
    let store =
        FileCredentialStore::new(CredentialAccount::OpenRouterApiKey, &home, &credential).unwrap();
    let victim = home.parent().unwrap().join("victim");
    fs::write(&victim, b"sk-victim").unwrap();
    fs::set_permissions(&victim, fs::Permissions::from_mode(0o600)).unwrap();
    symlink(&victim, &credential).unwrap();

    assert_eq!(
        store
            .load(CredentialAccount::OpenRouterApiKey)
            .unwrap_err()
            .category(),
        CredentialFailureCategory::Permissions
    );
    assert_eq!(
        store
            .replace(CredentialAccount::OpenRouterApiKey, secret("sk-new"))
            .unwrap_err()
            .category(),
        CredentialFailureCategory::Permissions
    );
    assert_eq!(fs::read(&victim).unwrap(), b"sk-victim");

    fs::remove_file(&credential).unwrap();
    fs::create_dir(&credential).unwrap();
    assert_eq!(
        store
            .delete(CredentialAccount::OpenRouterApiKey)
            .unwrap_err()
            .category(),
        CredentialFailureCategory::Permissions
    );
}

#[test]
fn committed_replace_and_delete_survive_directory_sync_failure() {
    let (_temp, home, credential) = paths();
    let store = FileCredentialStore::with_directory_sync(
        CredentialAccount::OpenRouterApiKey,
        &home,
        &credential,
        Arc::new(ScriptedDirectorySync::fail_after(0)),
    )
    .unwrap();

    assert_eq!(
        store
            .replace_with_commit(CredentialAccount::OpenRouterApiKey, secret("sk-committed"))
            .unwrap(),
        CommitStatus::CommittedUnverified
    );
    assert_eq!(fs::read(&credential).unwrap(), b"sk-committed");
    assert_eq!(
        store
            .delete_with_commit(CredentialAccount::OpenRouterApiKey)
            .unwrap(),
        CommitStatus::CommittedUnverified
    );
    assert!(!credential.exists());
}

#[test]
fn deletion_unlinks_the_valid_file_and_is_idempotent() {
    let (_temp, home, credential) = paths();
    let store =
        FileCredentialStore::new(CredentialAccount::OpenRouterApiKey, &home, &credential).unwrap();
    store
        .replace(CredentialAccount::OpenRouterApiKey, secret("sk-delete"))
        .unwrap();
    store.delete(CredentialAccount::OpenRouterApiKey).unwrap();
    assert!(!credential.exists());
    store.delete(CredentialAccount::OpenRouterApiKey).unwrap();
}

#[test]
fn invalid_existing_target_preserves_prior_bytes_on_replace_failure() {
    let (_temp, home, credential) = paths();
    let store =
        FileCredentialStore::new(CredentialAccount::OpenRouterApiKey, &home, &credential).unwrap();
    fs::write(&credential, b"sk-preserve").unwrap();
    fs::set_permissions(&credential, fs::Permissions::from_mode(0o644)).unwrap();

    assert!(store
        .replace(
            CredentialAccount::OpenRouterApiKey,
            secret("sk-replacement")
        )
        .is_err());
    assert_eq!(fs::read(&credential).unwrap(), b"sk-preserve");
}
