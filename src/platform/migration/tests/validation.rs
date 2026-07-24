use super::*;

#[test]
fn first_run_creates_nothing() {
    let home = tempfile::tempdir().unwrap();
    let legacy = home
        .path()
        .join("Library/Application Support")
        .join(LEGACY_NAME);
    let current = home
        .path()
        .join("Library/Application Support")
        .join(CURRENT_NAME);
    assert_eq!(run(home.path()).unwrap(), MigrationOutcome::FirstRun);
    assert!(!legacy.exists());
    assert!(!current.exists());
    assert!(!home.path().join("Library").exists());
}

#[test]
fn accepts_current_root_and_is_idempotent() {
    let (home, _, _, current) = roots();
    owner_only(&current);
    assert_eq!(run(home.path()).unwrap(), MigrationOutcome::Current);
    assert_eq!(run(home.path()).unwrap(), MigrationOutcome::Current);
}

#[test]
fn rejects_both_roots_without_mutation() {
    let (home, _, legacy, current) = roots();
    owner_only(&legacy);
    owner_only(&current);
    assert!(matches!(
        run(home.path()),
        Err(SupportRootMigrationError::Collision)
    ));
    assert!(legacy.exists());
    assert!(current.exists());
}

#[test]
fn rejects_symlink_file_and_wrong_mode_without_following() {
    let (home, _, legacy, current) = roots();
    let target = home.path().join("outside-target");
    owner_only(&target);
    symlink(&target, &legacy).unwrap();
    assert!(matches!(
        run(home.path()),
        Err(SupportRootMigrationError::UnsafeRoot { .. })
    ));
    fs::remove_file(&legacy).unwrap();
    fs::write(&legacy, b"file").unwrap();
    assert!(matches!(
        run(home.path()),
        Err(SupportRootMigrationError::UnsafeRoot { .. })
    ));
    fs::remove_file(&legacy).unwrap();
    owner_only(&legacy);
    fs::set_permissions(&legacy, fs::Permissions::from_mode(0o750)).unwrap();
    assert!(matches!(
        run(home.path()),
        Err(SupportRootMigrationError::UnsafeRoot { .. })
    ));
    assert!(!current.exists());
}

#[test]
fn reports_injected_no_follow_metadata_failure_without_mutation() {
    let home = tempfile::tempdir().unwrap();
    assert!(matches!(
        migrate_support_root_with(home.path(), &MetadataFailure, &RealDirectorySync, unsafe {
            libc::geteuid()
        },),
        Err(SupportRootMigrationError::Inspect { .. })
    ));
    assert!(!home.path().join("Library").exists());
}

#[test]
fn rejects_wrong_owner_from_injected_metadata() {
    let (home, _, legacy, _) = roots();
    owner_only(&legacy);
    let uid = unsafe { libc::geteuid() };
    let file_system = MetadataOverride {
        inner: RealMigrationFileSystem,
        uid: uid.wrapping_add(1),
    };
    assert!(matches!(
        migrate_support_root_with(home.path(), &file_system, &RealDirectorySync, uid),
        Err(SupportRootMigrationError::UnsafeRoot { .. })
    ));
}
