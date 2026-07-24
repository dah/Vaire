use super::*;

#[test]
fn reports_rename_failure_when_no_concurrent_winner() {
    let (home, _, legacy, _) = roots();
    owner_only(&legacy);
    let file_system = RenameFailure {
        winner_moves_first: false,
        calls: Mutex::new(0),
    };
    assert!(matches!(
        migrate_support_root_with(home.path(), &file_system, &RealDirectorySync, unsafe {
            libc::geteuid()
        },),
        Err(SupportRootMigrationError::Rename { .. })
    ));
}

#[test]
fn failed_rename_with_both_roots_is_reclassified_as_collision() {
    let (home, _, legacy, current) = roots();
    owner_only(&legacy);
    assert!(matches!(
        migrate_support_root_with(
            home.path(),
            &CollisionAfterRenameFailure,
            &RealDirectorySync,
            unsafe { libc::geteuid() },
        ),
        Err(SupportRootMigrationError::Collision)
    ));
    assert!(legacy.exists());
    assert!(current.exists());
}

#[test]
fn concurrent_winner_requires_the_original_source_identity() {
    let (home, _, legacy, current) = roots();
    owner_only(&legacy);
    let source_identity = RealMigrationFileSystem
        .metadata_no_follow(&legacy)
        .unwrap()
        .unwrap()
        .identity();
    assert!(matches!(
        migrate_support_root_with(
            home.path(),
            &IdentityMismatchAfterRenameFailure,
            &RealDirectorySync,
            unsafe { libc::geteuid() },
        ),
        Err(SupportRootMigrationError::Rename { .. })
    ));
    let current_identity = RealMigrationFileSystem
        .metadata_no_follow(&current)
        .unwrap()
        .unwrap()
        .identity();
    assert_ne!(current_identity, source_identity);
}
