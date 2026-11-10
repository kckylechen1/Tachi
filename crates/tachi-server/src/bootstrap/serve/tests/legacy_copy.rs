use super::*;

#[cfg(unix)]
struct CopyFaultGuard;

#[cfg(unix)]
impl CopyFaultGuard {
    fn arm(dest: &Path, fault: LegacyCopyTestFault) -> Self {
        LEGACY_COPY_TEST_FAULT.with(|slot| *slot.borrow_mut() = Some((dest.to_path_buf(), fault)));
        Self
    }
}

#[cfg(unix)]
impl Drop for CopyFaultGuard {
    fn drop(&mut self) {
        LEGACY_COPY_TEST_FAULT.with(|slot| *slot.borrow_mut() = None);
    }
}

#[cfg(unix)]
#[tokio::test]
async fn partial_main_failure_cleans_its_files_and_allows_a_complete_retry() {
    let root = tempfile::tempdir().unwrap();
    let src = root.path().join("legacy.db");
    let dest = root.path().join("target").join(memcore::MEMORY_DB_FILENAME);
    std::fs::write(&src, b"complete legacy main").unwrap();
    std::fs::write(sidecar_path(&src, "-wal"), b"committed legacy WAL").unwrap();
    let _ownership = ClearOwnershipInject;
    crate::db_ownership::set_ownership_inject_for_test(Some(
        crate::db_ownership::DbOwnership::NotOwned,
    ));
    let fault = CopyFaultGuard::arm(&dest, LegacyCopyTestFault::PartialMain);
    let error = copy_legacy_db_guarded(&src, &dest).await.unwrap_err();
    assert!(
        error
            .to_string()
            .contains("injected failure after partial main copy"),
        "{error}"
    );
    assert!(
        !dest.exists(),
        "the partially written main belongs to this attempt and must be removed"
    );
    assert!(!memcore::legacy_copy_marker_path(&dest).exists());
    assert_eq!(std::fs::read(&src).unwrap(), b"complete legacy main");
    assert_eq!(
        std::fs::read(sidecar_path(&src, "-wal")).unwrap(),
        b"committed legacy WAL"
    );
    drop(fault);

    copy_legacy_db_guarded(&src, &dest).await.unwrap();
    assert_eq!(std::fs::read(&dest).unwrap(), b"complete legacy main");
    assert_eq!(
        std::fs::read(sidecar_path(&dest, "-wal")).unwrap(),
        b"committed legacy WAL"
    );
    assert!(!memcore::legacy_copy_marker_path(&dest).exists());
}

#[cfg(unix)]
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn failed_cleanup_fences_existing_global_and_project_targets_on_restart() {
    let _environment = crate::utils::global_test_lock().lock().unwrap();
    let _memory_db = EnvRestore::remove("MEMORY_DB_PATH");
    let root = tempfile::tempdir().unwrap();
    let app_home = root.path().join("home");
    let src = app_home.join(memcore::LEGACY_MEMORY_DB_FILENAME);
    let dest = app_home.join("global").join(memcore::MEMORY_DB_FILENAME);
    std::fs::create_dir_all(&app_home).unwrap();
    std::fs::write(&src, b"complete legacy main").unwrap();
    let _ownership = ClearOwnershipInject;
    crate::db_ownership::set_ownership_inject_for_test(Some(
        crate::db_ownership::DbOwnership::NotOwned,
    ));
    let fault = CopyFaultGuard::arm(&dest, LegacyCopyTestFault::PartialMainAndCleanup);
    let error = copy_legacy_db_guarded(&src, &dest).await.unwrap_err();
    assert!(
        error.to_string().contains("injected cleanup failure"),
        "{error}"
    );
    assert_eq!(std::fs::read(&dest).unwrap(), b"incomplete legacy copy");
    assert!(memcore::legacy_copy_marker_path(&dest).exists());
    drop(fault);

    let ctx = StartupContext {
        home: root.path().to_path_buf(),
        app_home: app_home.clone(),
        command: Commands::Serve,
        defer_manifest_startup: true,
        git_root: None,
        schema_migration: memcore::MigrationAuthority::Deny,
    };
    let mut cli = startup_test_cli();
    let error = migrate_legacy_global_db_if_needed(&cli, &ctx, &dest, None)
        .await
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("incomplete legacy database copy"),
        "{error}"
    );
    cli.global_db = Some(dest.clone());
    let error = migrate_legacy_global_db_if_needed(&cli, &ctx, &dest, None)
        .await
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("incomplete legacy database copy"),
        "{error}"
    );
    cli.project_db = Some(dest.clone());
    let other_global = root.path().join("unrelated-global.db");
    let error = match run_startup_hygiene(&cli, &ctx, &other_global).await {
        Err(error) => error,
        Ok(_) => panic!("explicit project DB must not bypass incomplete-copy admission"),
    };
    assert!(
        error
            .to_string()
            .contains("incomplete legacy database copy"),
        "{error}"
    );
    assert_eq!(std::fs::read(&dest).unwrap(), b"incomplete legacy copy");
    assert_eq!(std::fs::read(&src).unwrap(), b"complete legacy main");

    // An offline operator quarantines the incomplete set before clearing its
    // marker. Startup may then retry from the preserved legacy source.
    std::fs::rename(&dest, root.path().join("quarantined-incomplete.db")).unwrap();
    std::fs::rename(
        memcore::legacy_copy_marker_path(&dest),
        root.path().join("quarantined-marker"),
    )
    .unwrap();
    cli.global_db = None;
    migrate_legacy_global_db_if_needed(&cli, &ctx, &dest, None)
        .await
        .unwrap();
    assert_eq!(std::fs::read(&dest).unwrap(), b"complete legacy main");
    assert!(!memcore::legacy_copy_marker_path(&dest).exists());
}

#[cfg(unix)]
#[tokio::test]
async fn legacy_copy_preserves_existing_main_or_sidecar_and_never_marks_it() {
    let root = tempfile::tempdir().unwrap();
    let src = root.path().join("legacy.db");
    std::fs::write(&src, b"legacy main").unwrap();
    std::fs::write(sidecar_path(&src, "-wal"), b"legacy WAL").unwrap();
    let _ownership = ClearOwnershipInject;
    crate::db_ownership::set_ownership_inject_for_test(Some(
        crate::db_ownership::DbOwnership::NotOwned,
    ));
    for (name, suffix) in [("main", ""), ("wal", "-wal"), ("source-alias", "")] {
        let dest = root.path().join(name).join(memcore::MEMORY_DB_FILENAME);
        std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
        let existing = sidecar_path(&dest, suffix);
        let retained: &[u8] = if name == "source-alias" {
            std::os::unix::fs::symlink(&src, &existing).unwrap();
            b"legacy main"
        } else {
            std::fs::write(&existing, b"another owner's data").unwrap();
            b"another owner's data"
        };
        let error = copy_legacy_db_guarded(&src, &dest).await.unwrap_err();
        assert!(
            error.to_string().contains("refuses existing target"),
            "{error}"
        );
        assert_eq!(std::fs::read(&existing).unwrap(), retained);
        assert_eq!(std::fs::read(&src).unwrap(), b"legacy main");
        if name == "source-alias" {
            assert!(std::fs::symlink_metadata(existing)
                .unwrap()
                .file_type()
                .is_symlink());
        }
        assert!(!memcore::legacy_copy_marker_path(&dest).exists());
        if !suffix.is_empty() {
            assert!(!dest.exists());
        }
    }
}

#[cfg(unix)]
#[tokio::test]
async fn exclusive_copy_collision_does_not_grant_cleanup_ownership() {
    let root = tempfile::tempdir().unwrap();
    let src = root.path().join("legacy.db");
    let dest = root.path().join(memcore::MEMORY_DB_FILENAME);
    std::fs::write(&src, b"legacy main").unwrap();
    std::fs::write(memcore::legacy_copy_marker_path(&dest), b"copy in progress").unwrap();
    // Publication by another owner after the initial absence check.
    std::fs::write(&dest, b"another owner's data").unwrap();
    let (owned, result) = copy_legacy_file_exclusive(&src, &dest).await;
    assert!(!owned);
    let error = result.unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
    let receipt = discard_failed_copy(&src, &dest, &dest, &error, &[]).await;
    assert!(receipt.contains("target remains"), "{receipt}");
    assert_eq!(std::fs::read(&dest).unwrap(), b"another owner's data");
    assert!(memcore::legacy_copy_marker_path(&dest).exists());
}

#[tokio::test]
#[cfg(unix)]
async fn copy_legacy_db_guarded_refuses_when_owned() {
    let dir = tempfile::tempdir().expect("tempdir");
    let src = dir.path().join("legacy.db");
    let dest = dir.path().join("global").join("memory.db");
    std::fs::write(&src, b"legacy-bytes").expect("seed legacy db");

    let _clear = ClearOwnershipInject;
    crate::db_ownership::set_ownership_inject_for_test(Some(
        crate::db_ownership::DbOwnership::Owned,
    ));

    let result = copy_legacy_db_guarded(&src, &dest).await;

    assert!(
        result.is_err(),
        "must refuse to copy a DB a live daemon holds open"
    );
    assert!(
        !dest.exists(),
        "refused copy must leave zero bytes at the destination"
    );
}

#[tokio::test]
#[cfg(unix)]
async fn copy_legacy_db_guarded_refuses_when_unknown() {
    let dir = tempfile::tempdir().expect("tempdir");
    let src = dir.path().join("legacy.db");
    let dest = dir.path().join("global").join("memory.db");
    std::fs::write(&src, b"legacy-bytes").expect("seed legacy db");

    let _clear = ClearOwnershipInject;
    crate::db_ownership::set_ownership_inject_for_test(Some(
        crate::db_ownership::DbOwnership::Unknown("lsof unavailable: test".to_string()),
    ));

    let result = copy_legacy_db_guarded(&src, &dest).await;

    let err = result.expect_err("undetermined ownership must refuse");
    assert!(
        err.to_string().contains("cannot determine"),
        "error must say ownership was undetermined, got: {err}"
    );
    assert!(
        !dest.exists(),
        "refused copy must leave zero bytes at the destination"
    );
}

#[tokio::test]
#[cfg(unix)]
async fn copy_legacy_db_guarded_proceeds_when_not_owned() {
    let dir = tempfile::tempdir().expect("tempdir");
    let src = dir.path().join("legacy.db");
    let dest = dir.path().join("global").join("memory.db");
    std::fs::write(&src, b"legacy-bytes").expect("seed legacy db");

    let _clear = ClearOwnershipInject;
    crate::db_ownership::set_ownership_inject_for_test(Some(
        crate::db_ownership::DbOwnership::NotOwned,
    ));

    let result = copy_legacy_db_guarded(&src, &dest).await;

    assert!(result.is_ok(), "not-owned must proceed: {result:?}");
    assert_eq!(
        std::fs::read(&dest).expect("dest must exist"),
        b"legacy-bytes"
    );
}

#[tokio::test]
#[cfg(unix)]
async fn copy_legacy_db_guarded_copies_wal_and_shm_sidecars_when_present() {
    // Crashed-WAL fixture: committed rows can live only in `-wal` until
    // checkpointed. Copying just the main file would silently drop them.
    let dir = tempfile::tempdir().expect("tempdir");
    let src = dir.path().join("legacy.db");
    let dest = dir.path().join("global").join("memory.db");
    std::fs::write(&src, b"legacy-main").expect("seed legacy main");
    std::fs::write(sidecar_path(&src, "-wal"), b"legacy-wal-rows").expect("seed legacy wal");
    std::fs::write(sidecar_path(&src, "-shm"), b"legacy-shm").expect("seed legacy shm");

    let _clear = ClearOwnershipInject;
    crate::db_ownership::set_ownership_inject_for_test(Some(
        crate::db_ownership::DbOwnership::NotOwned,
    ));

    let result = copy_legacy_db_guarded(&src, &dest).await;

    assert!(result.is_ok(), "not-owned must proceed: {result:?}");
    assert_eq!(std::fs::read(&dest).expect("dest main"), b"legacy-main");
    assert_eq!(
        std::fs::read(sidecar_path(&dest, "-wal")).expect("dest wal must exist"),
        b"legacy-wal-rows",
        "committed-only-in-WAL rows must not be silently dropped"
    );
    assert_eq!(
        std::fs::read(sidecar_path(&dest, "-shm")).expect("dest shm must exist"),
        b"legacy-shm"
    );
}

#[tokio::test]
#[cfg(unix)]
async fn copy_legacy_db_guarded_skips_absent_sidecars() {
    // No -wal/-shm on disk (already checkpointed / non-WAL source): the
    // copy must still succeed and must not fabricate sidecar files.
    let dir = tempfile::tempdir().expect("tempdir");
    let src = dir.path().join("legacy.db");
    let dest = dir.path().join("global").join("memory.db");
    std::fs::write(&src, b"legacy-main").expect("seed legacy main");

    let _clear = ClearOwnershipInject;
    crate::db_ownership::set_ownership_inject_for_test(Some(
        crate::db_ownership::DbOwnership::NotOwned,
    ));

    let result = copy_legacy_db_guarded(&src, &dest).await;

    assert!(result.is_ok(), "not-owned must proceed: {result:?}");
    assert!(!sidecar_path(&dest, "-wal").exists());
    assert!(!sidecar_path(&dest, "-shm").exists());
}

#[tokio::test]
#[cfg(unix)]
async fn copy_legacy_db_guarded_discards_partial_copy_when_sidecar_copy_fails() {
    // Force the -wal sidecar copy to fail (source is a directory, not a
    // regular file) after the main file has already been copied. The
    // whole destination — main + any sidecar already copied — must be
    // discarded; a main-file-only leftover would silently drop the rows
    // that only exist in -wal.
    let dir = tempfile::tempdir().expect("tempdir");
    let src = dir.path().join("legacy.db");
    let dest = dir.path().join("global").join("memory.db");
    std::fs::write(&src, b"legacy-main").expect("seed legacy main");
    std::fs::create_dir(sidecar_path(&src, "-wal")).expect("wal as directory");

    let _clear = ClearOwnershipInject;
    crate::db_ownership::set_ownership_inject_for_test(Some(
        crate::db_ownership::DbOwnership::NotOwned,
    ));

    let result = copy_legacy_db_guarded(&src, &dest).await;

    assert!(
        result.is_err(),
        "sidecar copy failure must fail the whole operation"
    );
    assert!(
        !dest.exists(),
        "partial main-file-only copy must be discarded on sidecar failure"
    );
    assert!(!sidecar_path(&dest, "-wal").exists());
    assert!(!sidecar_path(&dest, "-shm").exists());
}

#[tokio::test]
async fn verify_copy_rejects_same_length_zeroed_destination() {
    // #2270: a copy reported success but left a destination of the right
    // length containing only zero bytes. Length-only checks pass this.
    let dir = tempfile::tempdir().expect("tempdir");
    let src = dir.path().join("legacy.db");
    let dest = dir.path().join("memory.db");
    let content = vec![0xABu8; (1 << 20) + 17];
    std::fs::write(&src, &content).expect("seed src");
    std::fs::write(&dest, &content).expect("seed dest");
    verify_copy(&src, &dest)
        .await
        .expect("identical files must verify");

    std::fs::write(&dest, vec![0u8; content.len()]).expect("zero dest");
    let err = verify_copy(&src, &dest)
        .await
        .expect_err("zeroed same-length destination must fail verification");
    assert!(err.to_string().contains("verification failed"), "{err}");

    // A difference only in the final partial chunk must also be caught.
    let mut tail_diff = content.clone();
    *tail_diff.last_mut().unwrap() ^= 1;
    std::fs::write(&dest, &tail_diff).expect("tail-diff dest");
    verify_copy(&src, &dest)
        .await
        .expect_err("tail-only difference must fail verification");
}
