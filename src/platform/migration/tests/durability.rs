use super::*;

#[test]
fn moves_legacy_root_without_inspecting_nested_opaque_sentinel() {
    let (home, _, legacy, current) = roots();
    owner_only(&legacy);
    let nested = legacy.join("opaque");
    fs::create_dir(&nested).unwrap();
    let sentinel = nested.join("sentinel");
    fs::write(&sentinel, b"opaque bytes").unwrap();
    fs::set_permissions(&sentinel, fs::Permissions::from_mode(0o000)).unwrap();

    assert_eq!(run(home.path()).unwrap(), MigrationOutcome::Migrated);
    assert!(!legacy.exists());
    assert_eq!(
        fs::symlink_metadata(current.join("opaque/sentinel"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0
    );
}

#[test]
fn reclassifies_a_concurrent_winner_as_current_after_synchronizing_the_parent() {
    let (home, parent, legacy, current) = roots();
    owner_only(&legacy);
    let file_system = RenameFailure {
        winner_moves_first: true,
        calls: Mutex::new(0),
    };
    let directory_sync = RecordingDirectorySync::new(false);
    assert_eq!(
        migrate_support_root_with(home.path(), &file_system, &directory_sync, unsafe {
            libc::geteuid()
        },)
        .unwrap(),
        MigrationOutcome::Current
    );
    assert_eq!(*directory_sync.calls.lock().unwrap(), vec![parent]);
    assert!(!legacy.exists());
    assert!(current.exists());
}

#[test]
fn concurrent_winner_sync_failure_reports_unverified_durability_without_rollback() {
    let (home, parent, legacy, current) = roots();
    owner_only(&legacy);
    let file_system = RenameFailure {
        winner_moves_first: true,
        calls: Mutex::new(0),
    };
    let directory_sync = RecordingDirectorySync::new(true);

    assert!(matches!(
        migrate_support_root_with(home.path(), &file_system, &directory_sync, unsafe {
            libc::geteuid()
        },),
        Err(SupportRootMigrationError::Durability { .. })
    ));
    assert_eq!(*directory_sync.calls.lock().unwrap(), vec![parent]);
    assert!(!legacy.exists());
    assert!(current.exists());
}

#[test]
fn reports_committed_but_unverified_durability_without_rollback() {
    let (home, _, legacy, current) = roots();
    owner_only(&legacy);
    let result = migrate_support_root_with(
        home.path(),
        &RealMigrationFileSystem,
        &ScriptedDirectorySync::fail_after(0),
        unsafe { libc::geteuid() },
    );
    assert!(matches!(
        result,
        Err(SupportRootMigrationError::Durability { .. })
    ));
    assert!(!legacy.exists());
    assert!(current.exists());
}
