use super::*;

#[test]
fn incomplete_legacy_copy_is_not_admitted_by_any_store_open_mode() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join(MEMORY_DB_FILENAME);
    drop(crate::MemoryStore::open(target.to_str().unwrap()).unwrap());
    let before = std::fs::read(&target).unwrap();
    let marker = legacy_copy_marker_path(&target);
    std::fs::write(&marker, b"copy interrupted before WAL publication").unwrap();
    for open in [
        crate::MemoryStore::open,
        crate::MemoryStore::open_read_only,
        crate::MemoryStore::open_read_only_immutable,
        crate::MemoryStore::open_existing_read_write,
    ] {
        let error = open(target.to_str().unwrap())
            .err()
            .expect("marker fences store admission");
        assert!(
            error
                .to_string()
                .contains("incomplete legacy database copy"),
            "{error}"
        );
    }
    let error =
        convert_legacy_filename_offline(&target, &OfflineFilenameAuthority::operator_attestation())
            .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("incomplete legacy database copy"),
        "{error}"
    );
    assert_eq!(std::fs::read(&target).unwrap(), before);
    assert!(marker.exists());

    // This fixture represents a verified complete copy. Removing its marker
    // restores the ordinary existing-store admission path.
    std::fs::remove_file(marker).unwrap();
    drop(crate::MemoryStore::open(target.to_str().unwrap()).unwrap());
}

#[cfg(unix)]
#[test]
fn incomplete_copy_marker_follows_existing_and_dangling_plan_c_aliases() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("target").join(MEMORY_DB_FILENAME);
    std::fs::create_dir_all(target.parent().unwrap()).unwrap();
    drop(crate::MemoryStore::open(target.to_str().unwrap()).unwrap());
    let marker = legacy_copy_marker_path(&target);
    std::fs::write(&marker, b"copy interrupted").unwrap();
    let alias = dir.path().join("plan-c.db");
    std::os::unix::fs::symlink(Path::new("target").join(MEMORY_DB_FILENAME), &alias).unwrap();
    let error = crate::MemoryStore::open(alias.to_str().unwrap())
        .err()
        .expect("alias must not bypass marker");
    assert!(
        error
            .to_string()
            .contains("incomplete legacy database copy"),
        "{error}"
    );
    std::fs::remove_file(&target).unwrap();
    let error = crate::MemoryStore::open(alias.to_str().unwrap())
        .err()
        .expect("dangling alias must not initialize an incomplete target");
    assert!(
        error
            .to_string()
            .contains("incomplete legacy database copy"),
        "{error}"
    );
    assert!(!target.exists());
    assert!(marker.exists());
}

#[cfg(unix)]
#[test]
fn offline_conversion_child_process() {
    let Ok(canonical) = std::env::var("TACHI_TEST_CONVERSION_TARGET") else {
        return;
    };
    convert_legacy_filename_offline(
        Path::new(&canonical),
        &OfflineFilenameAuthority::operator_attestation(),
    )
    .expect("uninterrupted child conversion");
}

#[cfg(unix)]
#[test]
fn hot_journal_child_process() {
    let Ok(path) = std::env::var("TACHI_TEST_HOT_JOURNAL_SOURCE") else {
        return;
    };
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch("PRAGMA journal_mode=DELETE; BEGIN IMMEDIATE;")
        .unwrap();
    conn.execute(
        "UPDATE memories SET text = 'uncommitted change' WHERE id = 'offline-preservation-row'",
        [],
    )
    .unwrap();
    let marker = std::env::var("TACHI_TEST_HOT_JOURNAL_MARKER").unwrap();
    std::fs::write(marker, b"journal transaction is open").unwrap();
    loop {
        std::thread::park();
    }
}

#[cfg(unix)]
#[test]
fn sqlite_recovers_real_hot_rollback_journal_before_filename_conversion() {
    use std::process::{Command, Stdio};
    let dir = tempfile::tempdir().unwrap();
    let legacy = dir.path().join(LEGACY_MEMORY_DB_FILENAME);
    let canonical = dir.path().join(MEMORY_DB_FILENAME);
    let marker = dir.path().join("hot-journal-marker");
    let entry = fixture_entry();
    {
        let mut store = crate::MemoryStore::open(legacy.to_str().unwrap()).unwrap();
        store.upsert(&entry).unwrap();
    }
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "db::filename::tests::hot_journal_child_process",
            "--nocapture",
        ])
        .env("TACHI_TEST_HOT_JOURNAL_SOURCE", &legacy)
        .env("TACHI_TEST_HOT_JOURNAL_MARKER", &marker)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    for _ in 0..250 {
        if marker.exists() {
            break;
        }
        if let Some(status) = child.try_wait().unwrap() {
            panic!("hot journal child exited early: {status}");
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(marker.exists(), "hot journal transaction never began");
    let journal = sqlite_sidecar_path(&legacy, "-journal");
    assert!(std::fs::metadata(&journal).unwrap().len() > 0);
    child.kill().unwrap();
    child.wait().unwrap();

    // The converter invokes SQLite recovery on the source name. If the
    // journal cannot be proven clean, it may refuse; success must recover
    // and preserve the last committed bytes, not the interrupted update.
    match convert_legacy_filename_offline(
        &canonical,
        &OfflineFilenameAuthority::operator_attestation(),
    ) {
        Ok(OfflineFilenameOutcome::Converted) => {
            let actual: String = Connection::open(&canonical)
                .unwrap()
                .query_row(
                    "SELECT text FROM memories WHERE id = ?1",
                    [entry.id.as_str()],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(actual.as_bytes(), entry.text.as_bytes());
            assert_eq!(
                std::fs::read_link(&legacy).unwrap(),
                Path::new(MEMORY_DB_FILENAME)
            );
        }
        Ok(outcome) => panic!("unexpected hot journal outcome: {outcome:?}"),
        Err(err) => {
            assert!(
                !canonical.exists(),
                "refusal cannot leave a partial target: {err}"
            );
            let actual: String = Connection::open(&legacy)
                .unwrap()
                .query_row(
                    "SELECT text FROM memories WHERE id = ?1",
                    [entry.id.as_str()],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(actual.as_bytes(), entry.text.as_bytes());
        }
    }
}

#[cfg(unix)]
#[test]
fn interrupted_offline_conversion_recovers_at_each_filesystem_phase() {
    use std::process::{Command, Stdio};
    for phase in [
        "before_checkpoint",
        "after_checkpoint",
        "before_rename",
        "after_rename",
        "after_link",
    ] {
        let dir = tempfile::tempdir().expect("temp dir");
        let legacy = dir.path().join(LEGACY_MEMORY_DB_FILENAME);
        let canonical = dir.path().join(MEMORY_DB_FILENAME);
        let marker = dir.path().join("test-pause-marker");
        let entry = fixture_entry();
        {
            let mut store = crate::MemoryStore::open(legacy.to_str().unwrap()).unwrap();
            store.upsert(&entry).unwrap();
        }
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "db::filename::tests::offline_conversion_child_process",
                "--nocapture",
            ])
            .env("TACHI_TEST_CONVERSION_TARGET", &canonical)
            .env("TACHI_TEST_CONVERSION_PAUSE", phase)
            .env("TACHI_TEST_CONVERSION_MARKER", &marker)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn converter test child");
        for _ in 0..250 {
            if marker.exists() {
                break;
            }
            if let Some(exit) = child.try_wait().unwrap() {
                panic!("converter child exited before {phase}: {exit}");
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(marker.exists(), "child never reached {phase}");
        child.kill().expect("interrupt child at controlled phase");
        child.wait().expect("reap interrupted child");
        let outcome = convert_legacy_filename_offline(
            &canonical,
            &OfflineFilenameAuthority::operator_attestation(),
        )
        .expect("explicit retry after interrupted conversion");
        assert!(matches!(
            outcome,
            OfflineFilenameOutcome::Converted
                | OfflineFilenameOutcome::RecoveredLink
                | OfflineFilenameOutcome::AlreadyConverted
        ));
        let actual: String = Connection::open(&canonical)
            .unwrap()
            .query_row(
                "SELECT text FROM memories WHERE id = ?1",
                [entry.id.as_str()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            actual.as_bytes(),
            entry.text.as_bytes(),
            "committed row lost at {phase}"
        );
        assert_eq!(
            std::fs::read_link(&legacy).unwrap(),
            Path::new(MEMORY_DB_FILENAME)
        );
    }
}

fn fixture_entry() -> crate::MemoryEntry {
    crate::MemoryEntry {
        id: "offline-preservation-row".to_string(),
        path: "/facts/offline".to_string(),
        summary: "preserve committed row".to_string(),
        text: "exact committed bytes: µ".to_string(),
        importance: 0.8,
        timestamp: chrono::Utc::now().to_rfc3339(),
        valid_from: String::new(),
        valid_until: None,
        category: "fact".to_string(),
        topic: "filename".to_string(),
        keywords: vec![],
        persons: vec![],
        entities: vec![],
        location: String::new(),
        source: "test".to_string(),
        scope: "project".to_string(),
        archived: false,
        access_count: 0,
        scored_count: 0,
        last_access: None,
        last_use_at: None,
        revision: 1,
        metadata: serde_json::json!({}),
        vector: None,
        retention_policy: Some("durable".to_string()),
        domain: Some("coding".to_string()),
        recall_count: 0,
        query_diversity: 0,
        tier: "raw".to_string(),
    }
}

#[cfg(unix)]
#[test]
fn ordinary_open_refuses_legacy_with_committed_row_and_offline_converter_preserves_it() {
    let dir = tempfile::tempdir().unwrap();
    let old = dir.path().join(LEGACY_MEMORY_DB_FILENAME);
    let new = dir.path().join(MEMORY_DB_FILENAME);
    let entry = fixture_entry();
    let old_version;
    {
        let mut store = crate::MemoryStore::open(old.to_str().unwrap()).unwrap();
        store.upsert(&entry).unwrap();
        old_version = super::super::migrations::read_schema_version(store.connection()).unwrap();
    }
    // Reopen via SQLite itself, not an immutable main-only probe: pending
    // WAL rows are part of committed state even after the old handle exits.
    let expected: String = Connection::open(&old)
        .unwrap()
        .query_row(
            "SELECT text FROM memories WHERE id = ?1",
            [entry.id.as_str()],
            |row| row.get(0),
        )
        .unwrap();
    for authority in [
        crate::DbOpenContext::open_existing_deny(),
        crate::DbOpenContext::open_existing_allow("test explicit schema authority"),
    ] {
        match crate::MemoryStore::open_with_label_and_context(
            new.to_str().unwrap(),
            "unknown",
            &authority,
        ) {
            Err(err) => assert!(
                err.to_string()
                    .contains("offline filename conversion required"),
                "{err}"
            ),
            Ok(_) => panic!("ordinary write open must not rename legacy DB"),
        }
    }
    match crate::MemoryStore::open_read_only(new.to_str().unwrap()) {
        Err(err) => assert!(err
            .to_string()
            .contains("offline filename conversion required")),
        Ok(_) => panic!("ordinary read open must not hide legacy DB"),
    }
    assert!(!new.exists());
    assert!(old.is_file());
    assert_eq!(
        convert_legacy_filename_offline(&new, &OfflineFilenameAuthority::operator_attestation())
            .unwrap(),
        OfflineFilenameOutcome::Converted
    );
    assert_eq!(
        super::super::migrations::read_schema_version(&Connection::open(&new).unwrap()).unwrap(),
        old_version
    );
    let actual: String = Connection::open(&new)
        .unwrap()
        .query_row(
            "SELECT text FROM memories WHERE id = ?1",
            [entry.id.as_str()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(actual.as_bytes(), expected.as_bytes());
    assert_eq!(
        convert_legacy_filename_offline(&new, &OfflineFilenameAuthority::operator_attestation())
            .unwrap(),
        OfflineFilenameOutcome::AlreadyConverted
    );
}

#[cfg(unix)]
#[test]
fn explicit_retry_repairs_only_verified_canonical_without_a_link() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join(MEMORY_DB_FILENAME);
    crate::MemoryStore::open(target.to_str().unwrap()).unwrap();
    let legacy = dir.path().join(LEGACY_MEMORY_DB_FILENAME);
    assert!(!legacy.exists());
    migrate_legacy_filename_if_present(&target).expect("ordinary open accepts canonical only");
    assert!(!legacy.exists());
    assert_eq!(
        convert_legacy_filename_offline(&target, &OfflineFilenameAuthority::operator_attestation())
            .unwrap(),
        OfflineFilenameOutcome::RecoveredLink
    );
    assert_eq!(
        std::fs::read_link(&legacy).unwrap(),
        Path::new(MEMORY_DB_FILENAME)
    );
}

#[cfg(unix)]
#[test]
fn orphaned_legacy_wal_blocks_canonical_even_with_valid_link() {
    let dir = tempfile::tempdir().unwrap();
    let canonical = dir.path().join(MEMORY_DB_FILENAME);
    crate::MemoryStore::open(canonical.to_str().unwrap()).unwrap();
    let legacy = dir.path().join(LEGACY_MEMORY_DB_FILENAME);
    std::os::unix::fs::symlink(MEMORY_DB_FILENAME, &legacy).unwrap();
    let wal = dir.path().join("memory.db-wal");
    std::fs::write(&wal, b"unrecovered WAL frames").unwrap();
    match crate::MemoryStore::open(canonical.to_str().unwrap()) {
        Err(err) => assert!(err.to_string().contains("legacy sidecar")),
        Ok(_) => panic!("canonical open must not ignore an orphaned WAL"),
    }
    assert_eq!(std::fs::read(&wal).unwrap(), b"unrecovered WAL frames");
    assert!(convert_legacy_filename_offline(
        &canonical,
        &OfflineFilenameAuthority::operator_attestation(),
    )
    .is_err());
    assert_eq!(std::fs::read(&wal).unwrap(), b"unrecovered WAL frames");
}

#[cfg(unix)]
#[test]
fn pending_wal_under_non_utf8_directory_is_not_hidden_by_display_loss() {
    use std::os::unix::ffi::{OsStrExt, OsStringExt};
    let dir = tempfile::tempdir().unwrap();
    let odd = dir
        .path()
        .join(std::ffi::OsString::from_vec(b"store-\xff".to_vec()));
    let db = odd.join(LEGACY_MEMORY_DB_FILENAME);
    let wal = sqlite_sidecar_path(&db, "-wal");
    assert!(wal
        .as_os_str()
        .as_bytes()
        .ends_with(b"store-\xff/memory.db-wal"));
    assert_ne!(wal, PathBuf::from(format!("{}-wal", db.display())));
    if let Err(err) = std::fs::create_dir(&odd) {
        // Some macOS filesystems reject invalid UTF-8 path components
        // altogether. The raw-byte invariant is still checked above.
        assert_eq!(err.raw_os_error(), Some(libc::EILSEQ));
        return;
    }
    std::fs::write(&wal, b"committed frames under non-UTF8 parent").unwrap();
    assert_eq!(pending_legacy_sidecar(&db).unwrap(), Some(wal));
}

#[cfg(unix)]
#[test]
fn active_writer_and_both_real_files_refuse_offline_conversion() {
    let dir = tempfile::tempdir().unwrap();
    let legacy = dir.path().join(LEGACY_MEMORY_DB_FILENAME);
    let canonical = dir.path().join(MEMORY_DB_FILENAME);
    {
        let mut store = crate::MemoryStore::open(legacy.to_str().unwrap()).unwrap();
        store.upsert(&fixture_entry()).unwrap();
    }
    let holder = Connection::open(&legacy).unwrap();
    holder.execute_batch("BEGIN EXCLUSIVE").unwrap();
    assert!(
        convert_legacy_filename_offline(
            &canonical,
            &OfflineFilenameAuthority::operator_attestation(),
        )
        .is_err(),
        "an active transaction cannot be treated as offline"
    );
    assert!(!canonical.exists());
    drop(holder);
    std::fs::write(&canonical, b"independent canonical data").unwrap();
    let legacy_before = std::fs::read(&legacy).unwrap();
    let canonical_before = std::fs::read(&canonical).unwrap();
    assert!(
        convert_legacy_filename_offline(
            &canonical,
            &OfflineFilenameAuthority::operator_attestation(),
        )
        .is_err(),
        "two real files cannot be reconciled automatically"
    );
    assert_eq!(std::fs::read(&legacy).unwrap(), legacy_before);
    assert_eq!(std::fs::read(&canonical).unwrap(), canonical_before);
}

#[cfg(unix)]
#[test]
fn offline_conversion_refuses_private_partition_without_moving_its_file() {
    let dir = tempfile::tempdir().unwrap();
    let legacy = dir.path().join(LEGACY_MEMORY_DB_FILENAME);
    let canonical = dir.path().join(MEMORY_DB_FILENAME);
    crate::MemoryStore::open(legacy.to_str().unwrap()).unwrap();
    let conn = Connection::open(&legacy).unwrap();
    crate::db::ensure_reserved_reference_write_guard(&conn).unwrap();
    conn.execute(
        "INSERT INTO hard_state (namespace, key, value_json, version, created_at, updated_at)
         VALUES ('store_identity', 'private_partition', '{\"value\":\"sealed\"}', 1, 'test', 'test')",
        [],
    ).unwrap();
    drop(conn);
    let before = std::fs::read(&legacy).unwrap();
    let err = convert_legacy_filename_offline(
        &canonical,
        &OfflineFilenameAuthority::operator_attestation(),
    )
    .expect_err("sealed private partition must never use generic converter");
    assert!(err.to_string().contains("private partition"), "{err}");
    assert_eq!(std::fs::read(&legacy).unwrap(), before);
    assert!(!canonical.exists());
}

#[cfg(unix)]
#[test]
fn offline_conversion_refuses_unknown_unstamped_file_without_adopting_it() {
    let dir = tempfile::tempdir().unwrap();
    let legacy = dir.path().join(LEGACY_MEMORY_DB_FILENAME);
    let canonical = dir.path().join(MEMORY_DB_FILENAME);
    Connection::open(&legacy)
        .unwrap()
        .execute_batch("CREATE TABLE foreign_data(x TEXT);")
        .unwrap();
    let before = std::fs::read(&legacy).unwrap();
    let err = convert_legacy_filename_offline(
        &canonical,
        &OfflineFilenameAuthority::operator_attestation(),
    )
    .expect_err("unknown database must not be adopted");
    assert!(err.to_string().contains("unknown/unstamped"), "{err}");
    assert_eq!(std::fs::read(&legacy).unwrap(), before);
    assert!(!canonical.exists());
}

#[test]
fn is_memory_db_filename_accepts_both_generations() {
    assert!(is_memory_db_filename(MEMORY_DB_FILENAME));
    assert!(is_memory_db_filename(LEGACY_MEMORY_DB_FILENAME));
    assert!(!is_memory_db_filename("other.db"));
    assert!(!is_memory_db_filename("tachi-memory.db.bak.20260101"));
}

#[test]
fn read_path_uses_legacy_only_without_mutating_it() {
    let dir = tempfile::tempdir().expect("tempdir");
    let canonical = dir.path().join(MEMORY_DB_FILENAME);
    let legacy = dir.path().join(LEGACY_MEMORY_DB_FILENAME);
    std::fs::write(&legacy, b"legacy data").expect("write legacy");

    assert_eq!(
        resolve_memory_db_read_path(&canonical).expect("resolve legacy-only read path"),
        legacy
    );
    assert!(!canonical.exists());
    assert_eq!(
        std::fs::read(&legacy).expect("legacy unchanged"),
        b"legacy data"
    );
}

#[cfg(unix)]
#[test]
fn read_path_accepts_only_the_exact_compat_link_and_rejects_two_real_files() {
    let dir = tempfile::tempdir().expect("tempdir");
    let canonical = dir.path().join(MEMORY_DB_FILENAME);
    let legacy = dir.path().join(LEGACY_MEMORY_DB_FILENAME);
    std::fs::write(&canonical, b"canonical data").expect("write canonical");
    std::os::unix::fs::symlink(MEMORY_DB_FILENAME, &legacy).expect("compat link");
    assert_eq!(
        resolve_memory_db_read_path(&canonical).expect("resolve compat link"),
        canonical
    );

    std::fs::remove_file(&legacy).expect("remove compat link");
    std::fs::write(&legacy, b"other real data").expect("write real legacy");
    let error =
        resolve_memory_db_read_path(&canonical).expect_err("two real files must fail closed");
    assert!(error.to_string().contains("both a canonical"), "{error}");
}

#[test]
fn migrate_is_a_no_op_for_a_non_canonical_target_name() {
    let dir = tempfile::tempdir().expect("tempdir");
    // A caller opening some other, non-standard filename — the seam must
    // not touch it, even if a `memory.db` sibling happens to exist.
    let other_target = dir.path().join("scratch.db");
    let legacy = dir.path().join(LEGACY_MEMORY_DB_FILENAME);
    std::fs::write(&legacy, b"legacy bytes").expect("write legacy sibling");

    migrate_legacy_filename_if_present(&other_target).expect("no-op must not error");

    assert!(
        !other_target.exists(),
        "non-canonical target must not be created"
    );
    assert!(
        legacy.exists(),
        "unrelated legacy sibling must be left alone"
    );
    assert!(
        std::fs::symlink_metadata(&legacy)
            .expect("legacy metadata")
            .file_type()
            .is_file(),
        "unrelated legacy sibling must not be touched/converted to a symlink"
    );
}

// #1226 round-4: the atomic no-clobber rename primitive must refuse to
// overwrite a canonical file a concurrent process already created, reporting
// EEXIST (DestinationExists) and leaving BOTH files byte-for-byte untouched.
// Deterministic: we pre-create the canonical file ourselves to stand in for
// the concurrent winner, then call the primitive directly (the outer state
// machine never reaches the rename when the canonical already exists at stat
// time, so this exercises the no-clobber guard in isolation).
#[cfg(all(unix, any(target_os = "macos", target_os = "ios", target_os = "linux")))]
#[test]
fn atomic_rename_refuses_to_clobber_an_existing_canonical() {
    let dir = tempfile::tempdir().expect("tempdir");
    let legacy = dir.path().join(LEGACY_MEMORY_DB_FILENAME);
    let canonical = dir.path().join(MEMORY_DB_FILENAME);
    // A concurrent process already created + populated the canonical file.
    std::fs::write(&canonical, b"winner's canonical data").expect("write canonical");
    std::fs::write(&legacy, b"our legacy data").expect("write legacy");

    let outcome = atomic_rename_no_clobber(&legacy, &canonical)
        .expect("a pre-existing destination must be a no-clobber refusal, not an error");
    assert!(
        matches!(outcome, AtomicRename::DestinationExists),
        "a pre-existing canonical must yield DestinationExists (EEXIST), never a clobber"
    );
    // The concurrent winner's bytes must be intact — NOT clobbered.
    assert_eq!(
        std::fs::read(&canonical).expect("read canonical"),
        b"winner's canonical data",
        "the concurrently-created canonical file must not be clobbered"
    );
    // Our legacy file must be left exactly where it was.
    assert_eq!(
        std::fs::read(&legacy).expect("read legacy"),
        b"our legacy data",
        "the legacy file must be untouched when the atomic rename is refused"
    );
    assert!(
        std::fs::symlink_metadata(&legacy)
            .expect("legacy metadata")
            .file_type()
            .is_file(),
        "the refused legacy file must not have been converted to a symlink"
    );
}

// The success path of the same primitive: when the canonical name is free,
// the atomic rename moves the legacy file onto it and vacates the old name.
#[cfg(all(unix, any(target_os = "macos", target_os = "ios", target_os = "linux")))]
#[test]
fn atomic_rename_moves_the_legacy_file_when_canonical_is_absent() {
    let dir = tempfile::tempdir().expect("tempdir");
    let legacy = dir.path().join(LEGACY_MEMORY_DB_FILENAME);
    let canonical = dir.path().join(MEMORY_DB_FILENAME);
    std::fs::write(&legacy, b"pre-#1132 data").expect("write legacy");

    let outcome = atomic_rename_no_clobber(&legacy, &canonical)
        .expect("rename onto a free name must succeed");
    assert!(
        matches!(outcome, AtomicRename::Renamed),
        "renaming onto a free canonical name must report Renamed"
    );
    assert_eq!(
        std::fs::read(&canonical).expect("read canonical"),
        b"pre-#1132 data",
        "the atomic move must preserve the legacy bytes exactly"
    );
    assert!(
        !legacy.exists(),
        "the legacy name must be free after a successful atomic move"
    );
}

// #1226 round-5: when the legacy source is gone, the primitive must report
// SourceVanished (ENOENT), not a generic error — so the caller can converge
// on a concurrent migrator instead of failing spuriously. Deterministic:
// with BOTH names absent the only possible errno is ENOENT (missing source),
// with no EEXIST ambiguity.
#[cfg(all(unix, any(target_os = "macos", target_os = "ios", target_os = "linux")))]
#[test]
fn atomic_rename_reports_source_vanished_when_legacy_is_absent() {
    let dir = tempfile::tempdir().expect("tempdir");
    let legacy = dir.path().join(LEGACY_MEMORY_DB_FILENAME); // never created
    let canonical = dir.path().join(MEMORY_DB_FILENAME); // also absent

    let outcome = atomic_rename_no_clobber(&legacy, &canonical)
        .expect("a missing source must be a SourceVanished signal, not an error");
    assert!(
        matches!(outcome, AtomicRename::SourceVanished),
        "a vanished legacy source (ENOENT) must report SourceVanished"
    );
}

// #1226 round-5 (codex secondary): a concurrent migrator moved the legacy
// file onto the canonical name before our rename. Our ENOENT must CONVERGE
// on the peer's canonical file (not hard-error), leaving the compat symlink
// and never touching the peer's bytes.
#[cfg(unix)]
#[test]
fn source_vanished_converges_when_canonical_now_exists() {
    let dir = tempfile::tempdir().expect("tempdir");
    let legacy = dir.path().join(LEGACY_MEMORY_DB_FILENAME); // peer already moved it away
    let canonical = dir.path().join(MEMORY_DB_FILENAME);
    std::fs::write(&canonical, b"migrated by a concurrent peer").expect("write canonical");

    converge_after_source_vanished(&legacy, &canonical)
        .expect("a peer-migrated canonical must converge, not fail");

    assert_eq!(
        std::fs::read(&canonical).expect("read canonical"),
        b"migrated by a concurrent peer",
        "the concurrent peer's canonical data must not be touched"
    );
    let legacy_meta = std::fs::symlink_metadata(&legacy)
        .expect("convergence must leave a compat symlink where the legacy file was");
    assert!(
        legacy_meta.file_type().is_symlink(),
        "converging on a peer's migration must (re-)leave the compat symlink"
    );
    assert_eq!(
        std::fs::read_link(&legacy).expect("read compat symlink target"),
        std::path::PathBuf::from(MEMORY_DB_FILENAME),
        "the compat symlink must point at the canonical filename"
    );
}

// The other side of ENOENT: the legacy file genuinely vanished (a delete /
// external move, NOT a migration) and no canonical exists to converge on.
// There is nothing to bring forward — fail loud, never open an empty DB.
#[cfg(unix)]
#[test]
fn source_vanished_fails_loud_when_canonical_also_absent() {
    let dir = tempfile::tempdir().expect("tempdir");
    let legacy = dir.path().join(LEGACY_MEMORY_DB_FILENAME); // gone
    let canonical = dir.path().join(MEMORY_DB_FILENAME); // also gone

    let err = converge_after_source_vanished(&legacy, &canonical)
        .expect_err("a genuinely-vanished legacy with no canonical must fail loud");
    assert!(
        err.to_string().contains("vanished during migration"),
        "error must name the vanished-data condition: {err}"
    );
    assert!(
        !canonical.exists(),
        "a failed convergence must NOT have created an empty canonical DB"
    );
}

#[cfg(unix)]
#[test]
fn migrate_converges_the_compat_symlink_when_canonical_exists_without_it() {
    // A crash between the atomic rename and the symlink step (or a fresh
    // canonical-only store) leaves the canonical file present but no compat
    // symlink. A subsequent open must CONVERGE — leave the symlink — without
    // clobbering the canonical data, and be a pure no-op thereafter.
    let dir = tempfile::tempdir().expect("tempdir");
    let target = dir.path().join(MEMORY_DB_FILENAME);
    let legacy = dir.path().join(LEGACY_MEMORY_DB_FILENAME);
    std::fs::write(&target, b"already migrated").expect("write target");

    migrate_legacy_filename_if_present(&target).expect("ordinary open must not error");

    assert!(
        !legacy.exists(),
        "ordinary open does not create a compatibility link"
    );
    // The direct converter verifies actual SQLite content, so this
    // byte-only state-machine fixture tests the link primitive itself.
    leave_compat_symlink(&legacy).expect("explicit converter leaves link");

    assert_eq!(
        std::fs::read(&target).expect("read target"),
        b"already migrated",
        "an existing canonical-named file must not be clobbered"
    );
    let legacy_meta = std::fs::symlink_metadata(&legacy)
        .expect("convergence must leave a compat symlink where none existed");
    assert!(
        legacy_meta.file_type().is_symlink(),
        "the half-done migration must converge by leaving the compat symlink"
    );
    assert_eq!(
        std::fs::read_link(&legacy).expect("read compat symlink target"),
        std::path::PathBuf::from(MEMORY_DB_FILENAME),
        "the converged compat symlink must point at the canonical filename"
    );

    // Second run: canonical file + compat symlink both in place -> pure
    // no-op. Nothing may change.
    migrate_legacy_filename_if_present(&target).expect("idempotent second open");
    assert_eq!(
        std::fs::read(&target).expect("read target"),
        b"already migrated"
    );
    assert!(std::fs::symlink_metadata(&legacy)
        .expect("legacy metadata")
        .file_type()
        .is_symlink());
}

#[cfg(unix)]
#[test]
fn migrate_fails_loud_when_both_canonical_and_legacy_real_files_exist() {
    // BUG #1132-1: a partially-failed/ambiguous migration where BOTH a real
    // canonical file AND a real legacy file exist. The seam must NOT silently
    // pick one (that could shadow/discard real rows) — it must fail loud and
    // touch nothing.
    let dir = tempfile::tempdir().expect("tempdir");
    let target = dir.path().join(MEMORY_DB_FILENAME);
    let legacy = dir.path().join(LEGACY_MEMORY_DB_FILENAME);
    std::fs::write(&target, b"canonical real data").expect("write canonical");
    std::fs::write(&legacy, b"legacy real data").expect("write legacy");

    let err = migrate_legacy_filename_if_present(&target)
        .expect_err("both-files-present is ambiguous and must fail loud, not silently pick one");
    let message = err.to_string();
    assert!(
        message.contains("both a canonical"),
        "error must name the both-files ambiguity: {message}"
    );
    assert!(
        message.contains("No file was touched"),
        "error must make the touch-nothing guarantee explicit: {message}"
    );

    // Neither file may have been renamed, clobbered, or turned into a link.
    assert_eq!(
        std::fs::read(&target).expect("read canonical"),
        b"canonical real data",
        "canonical file must be left exactly as it was"
    );
    assert_eq!(
        std::fs::read(&legacy).expect("read legacy"),
        b"legacy real data",
        "legacy file must be left exactly as it was"
    );
    assert!(
        std::fs::symlink_metadata(&legacy)
            .expect("legacy metadata")
            .file_type()
            .is_file(),
        "legacy real file must not have been converted to a symlink"
    );
}

#[cfg(unix)]
#[test]
fn migrate_fails_loud_when_a_db_path_is_present_but_un_stat_able() {
    // BUG #1132-2: a metadata error (permission denied, transient I/O) on a
    // memory-database path must FAIL LOUD, never be read as "absent" and fall
    // through to creating a fresh empty canonical DB that shadows the real,
    // still-present data. We strip search (x) permission on the store dir so
    // that lstat() of the paths inside returns EACCES rather than NotFound —
    // the "present but un-stat-able" class.
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().expect("tempdir");
    let store_dir = dir.path().join("store");
    std::fs::create_dir(&store_dir).expect("mk store dir");
    // Real pre-#1132 data that the buggy path would have shadowed.
    let legacy = store_dir.join(LEGACY_MEMORY_DB_FILENAME);
    std::fs::write(&legacy, b"real pre-#1132 data").expect("write legacy");
    let target = store_dir.join(MEMORY_DB_FILENAME);

    let original_mode = std::fs::metadata(&store_dir)
        .expect("store dir metadata")
        .permissions()
        .mode();
    std::fs::set_permissions(&store_dir, std::fs::Permissions::from_mode(0o000))
        .expect("make store dir unsearchable");

    let result = migrate_legacy_filename_if_present(&target);

    // Restore permission BEFORE any assertion can panic and skip cleanup.
    std::fs::set_permissions(&store_dir, std::fs::Permissions::from_mode(original_mode))
        .expect("restore store dir permissions");

    let err = result.expect_err("an un-stat-able DB path must fail loud, not be treated as absent");
    let message = err.to_string();
    assert!(
        message.contains("could not stat"),
        "error must name the stat failure: {message}"
    );
    assert!(
        message.contains("could create an empty DB that shadows real data"),
        "error must make the shadow-avoidance intent explicit: {message}"
    );
    assert!(
        !target.exists(),
        "a failed stat must NOT have created an empty canonical DB"
    );
    assert!(
        legacy.is_file(),
        "the real legacy data must be left exactly where it was"
    );
}

#[test]
fn migrate_is_a_no_op_when_genuinely_fresh() {
    let dir = tempfile::tempdir().expect("tempdir");
    let target = dir.path().join(MEMORY_DB_FILENAME);

    migrate_legacy_filename_if_present(&target).expect("no-op must not error");

    assert!(
        !target.exists(),
        "migrate() only renames — it must not create the target itself"
    );
}

#[cfg(unix)]
#[test]
fn orphaned_compat_symlink_refuses_without_creating_a_replacement() {
    let dir = tempfile::tempdir().expect("tempdir");
    let target = dir.path().join(MEMORY_DB_FILENAME);
    let legacy = dir.path().join(LEGACY_MEMORY_DB_FILENAME);
    // Simulate a directory already migrated by a prior run, where the
    // real file has since been deleted (or never landed) but the compat
    // symlink is still sitting there pointing at a name that doesn't
    // exist. An ordinary open must refuse rather than create an empty DB.
    std::os::unix::fs::symlink(MEMORY_DB_FILENAME, &legacy).expect("plant compat symlink");

    let err = migrate_legacy_filename_if_present(&target)
        .expect_err("orphaned link must refuse an ordinary open");
    assert!(err.to_string().contains("orphaned compatibility link"));

    assert!(
        !target.exists(),
        "an orphaned compat symlink must not be treated as data to rename"
    );
    assert!(
        std::fs::symlink_metadata(&legacy)
            .expect("legacy metadata")
            .file_type()
            .is_symlink(),
        "the pre-existing symlink must be left exactly as it was"
    );
}

#[cfg(unix)]
#[test]
fn migrate_fails_loud_on_a_legacy_symlink_pointing_at_the_wrong_target() {
    // RESIDUAL-2: a stale/corrupted `memory.db` symlink pointing at some
    // OTHER path (not the canonical sibling) must NOT be silently accepted —
    // following it would resolve old-name readers to the wrong data. Fail
    // loud in BOTH the canonical-absent and canonical-present cases.
    for canonical_exists in [false, true] {
        let dir = tempfile::tempdir().expect("tempdir");
        let target = dir.path().join(MEMORY_DB_FILENAME);
        let legacy = dir.path().join(LEGACY_MEMORY_DB_FILENAME);
        // A link to a plausible-but-wrong old location.
        std::os::unix::fs::symlink("../old-place/memory.db", &legacy)
            .expect("plant wrong-target symlink");
        if canonical_exists {
            std::fs::write(&target, b"canonical data").expect("write canonical");
        }

        let err = migrate_legacy_filename_if_present(&target).expect_err(
            "a legacy symlink pointing at the wrong target must fail loud, not be accepted",
        );
        let message = err.to_string();
        assert!(
            message.contains("stale/corrupted compat link"),
            "error must name the corrupted-link condition (canonical_exists={canonical_exists}): {message}"
        );
        assert!(
            message.contains("wrong data"),
            "error must make the wrong-data risk explicit: {message}"
        );
        // The seam must not have touched anything.
        assert_eq!(
            std::fs::read_link(&legacy).expect("legacy link"),
            std::path::PathBuf::from("../old-place/memory.db"),
            "the suspect symlink must be left exactly as it was"
        );
        if canonical_exists {
            assert_eq!(
                std::fs::read(&target).expect("read canonical"),
                b"canonical data",
                "canonical data must be untouched"
            );
        } else {
            assert!(!target.exists(), "no canonical file may be created");
        }
    }
}

#[cfg(unix)]
#[test]
fn migrate_renames_a_legacy_regular_file_and_leaves_a_compat_symlink() {
    let dir = tempfile::tempdir().expect("tempdir");
    let target = dir.path().join(MEMORY_DB_FILENAME);
    let legacy = dir.path().join(LEGACY_MEMORY_DB_FILENAME);
    crate::MemoryStore::open(legacy.to_str().unwrap()).expect("seed genuine SQLite store");
    let old_version =
        crate::db::migrations::read_schema_version(&Connection::open(&legacy).unwrap()).unwrap();

    let err = migrate_legacy_filename_if_present(&target)
        .expect_err("ordinary open must refuse old filename");
    assert!(err
        .to_string()
        .contains("offline filename conversion required"));
    assert!(!target.exists());
    assert!(legacy.is_file());

    assert_eq!(
        convert_legacy_filename_offline(&target, &OfflineFilenameAuthority::operator_attestation())
            .expect("explicit offline conversion"),
        OfflineFilenameOutcome::Converted
    );

    assert!(
        target.is_file(),
        "the legacy file must now live at the canonical name"
    );
    assert_eq!(
        crate::db::migrations::read_schema_version(&Connection::open(&target).unwrap()).unwrap(),
        old_version,
        "filename conversion must not upgrade the schema"
    );
    let legacy_meta = std::fs::symlink_metadata(&legacy).expect("legacy metadata");
    assert!(
        legacy_meta.file_type().is_symlink(),
        "the old name must become a compat symlink, not vanish or stay a regular file"
    );
    assert_eq!(
        std::fs::read_link(&legacy).expect("read compat symlink target"),
        std::path::PathBuf::from(MEMORY_DB_FILENAME),
        "the compat symlink must point at the canonical filename"
    );
    // Old-name readers still resolving through the symlink must see the
    // same bytes as the canonical path — that's the entire point of the
    // one-release-window compat shim.
    assert_eq!(
        std::fs::read(&legacy).expect("read through compat symlink"),
        std::fs::read(&target).unwrap(),
    );
}

#[cfg(unix)]
#[test]
fn migrate_rename_failure_is_loud_not_silent() {
    let dir = tempfile::tempdir().expect("tempdir");
    let target = dir.path().join(MEMORY_DB_FILENAME);
    let legacy = dir.path().join(LEGACY_MEMORY_DB_FILENAME);
    crate::MemoryStore::open(legacy.to_str().unwrap()).expect("seed genuine SQLite store");

    // Make the containing directory non-writable so the rename() syscall
    // itself fails with a permission error, simulating a read-only-fs /
    // permissions class of failure without needing an actual read-only
    // mount.
    use std::os::unix::fs::PermissionsExt;
    let original_mode = std::fs::metadata(dir.path())
        .expect("dir metadata")
        .permissions()
        .mode();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o555))
        .expect("make dir read-only");

    let result =
        convert_legacy_filename_offline(&target, &OfflineFilenameAuthority::operator_attestation());

    // Restore write permission BEFORE any assertion can panic and skip
    // cleanup, so the tempdir's Drop can still remove its contents.
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(original_mode))
        .expect("restore dir permissions");

    let _err = result.expect_err("a conversion on a non-writable directory must fail loudly");
    assert!(
        !target.exists(),
        "a failed migration must not leave a partially-created target"
    );
    assert!(
        legacy.is_file(),
        "a failed migration must leave the legacy data exactly where it was, unrenamed"
    );
}

#[cfg(unix)]
#[test]
fn memory_store_open_requires_explicit_offline_conversion_and_preserves_durable_row() {
    let dir = tempfile::tempdir().expect("tempdir");
    let legacy_path = dir.path().join(LEGACY_MEMORY_DB_FILENAME);
    let canonical_path = dir.path().join(MEMORY_DB_FILENAME);

    // Simulate a pre-#1132 install: a real, schema-initialized DB sitting
    // under the OLD literal filename, created by opening that path
    // directly (not through the seam — the seam never fires for a
    // non-canonical target, by design).
    {
        let mut store = crate::MemoryStore::open(legacy_path.to_str().expect("utf8 path"))
            .expect("create legacy-named db");
        let now = chrono::Utc::now().to_rfc3339();
        let marker_entry = crate::MemoryEntry {
            id: "rename-on-open-marker".to_string(),
            path: "/facts/rename-on-open".to_string(),
            summary: "marker".to_string(),
            text: "rename-on-open marker row".to_string(),
            importance: 0.5,
            timestamp: now.clone(),
            valid_from: now,
            valid_until: None,
            category: "fact".to_string(),
            topic: "test".to_string(),
            keywords: vec![],
            persons: vec![],
            entities: vec![],
            location: String::new(),
            source: "manual".to_string(),
            scope: "project".to_string(),
            archived: false,
            access_count: 0,
            scored_count: 0,
            last_access: None,
            last_use_at: None,
            revision: 1,
            vector: None,
            retention_policy: Some("durable".to_string()),
            domain: None,
            metadata: serde_json::json!({}),
            recall_count: 0,
            query_diversity: 0,
            tier: "raw".to_string(),
        };
        assert_eq!(
            store
                .insert_if_absent(&marker_entry)
                .expect("insert durable marker through MemoryStore"),
            crate::db::InsertMemoryResult::Inserted,
            "the legacy fixture must create one durable marker, not coalesce or replace one"
        );
        let marker_before_migration = store
            .get(&marker_entry.id)
            .expect("read durable marker before migration")
            .expect("durable marker must exist before migration");
        assert_eq!(marker_before_migration.summary, marker_entry.summary);
        assert_eq!(
            marker_before_migration.retention_policy.as_deref(),
            Some("durable"),
            "test precondition: marker must be durable before migration"
        );
    }
    assert!(
        legacy_path.is_file(),
        "test precondition: legacy file exists"
    );
    assert!(
        !canonical_path.exists(),
        "test precondition: canonical name not yet created"
    );

    // Even a clean-looking legacy DB cannot be moved by an ordinary
    // MemoryStore open: old binaries are not fenced by this process.
    match crate::MemoryStore::open(canonical_path.to_str().expect("utf8 path")) {
        Err(err) => assert!(err
            .to_string()
            .contains("offline filename conversion required")),
        Ok(_) => panic!("ordinary open must refuse real legacy filename"),
    }
    assert!(!canonical_path.exists());
    assert!(legacy_path.is_file());

    assert_eq!(
        convert_legacy_filename_offline(
            &canonical_path,
            &OfflineFilenameAuthority::operator_attestation(),
        )
        .expect("offline conversion"),
        OfflineFilenameOutcome::Converted
    );
    let migrated_store = crate::MemoryStore::open(canonical_path.to_str().expect("utf8 path"))
        .expect("open after explicit conversion");

    assert!(
        canonical_path.is_file(),
        "the legacy file must now be the file at the canonical path"
    );
    let legacy_meta = std::fs::symlink_metadata(&legacy_path).expect("legacy metadata");
    assert!(
        legacy_meta.file_type().is_symlink(),
        "the old name must be left behind as a compat symlink"
    );

    let marker = migrated_store
        .get("rename-on-open-marker")
        .expect("read marker through canonical store")
        .expect("marker must survive the rename");
    assert_eq!(marker.summary, "marker");
    assert_eq!(marker.retention_policy.as_deref(), Some("durable"));

    // Reopening does not perform a second conversion.
    drop(migrated_store);
    let reopened = crate::MemoryStore::open(canonical_path.to_str().expect("utf8 path"))
        .expect("reopen canonical path");
    let marker_again = reopened
        .get("rename-on-open-marker")
        .expect("read marker through reopened canonical store")
        .expect("marker must still be there on reopen");
    assert_eq!(marker_again.summary, "marker");
    assert_eq!(marker_again.retention_policy.as_deref(), Some("durable"));
}
