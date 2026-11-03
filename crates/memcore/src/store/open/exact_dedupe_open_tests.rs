use super::*;

#[cfg(unix)]
#[test]
fn physical_identity_validation_rejects_aba_path_swap_across_open() {
    let dir = tempfile::tempdir().unwrap();
    let path_a = dir.path().join("store_a.db");
    let path_b = dir.path().join("store_b.db");
    let path_temp = dir.path().join("store_temp.db");

    // Seed two completely independent valid stores:
    // A has identity A (inode A)
    // B has identity B (inode B)
    drop(MemoryStore::open(path_a.to_str().unwrap()).unwrap());
    drop(MemoryStore::open(path_b.to_str().unwrap()).unwrap());

    let p_a1 = path_a.clone();
    let p_b1 = path_b.clone();
    let p_t1 = path_temp.clone();

    let p_a2 = path_a.clone();
    let p_b2 = path_b.clone();
    let p_t2 = path_temp.clone();

    // 1. After physical_identity_before_open samples path_a (inode A),
    //    swap path_a to point to store B!
    // 2. Immediately after SQLite opens the connection to B (and before
    //    validate_physical_db_identity_across_open checks the identity),
    //    swap path_a back to point to original A!
    let _aba_guard = MemoryStore::install_startup_aba_hook_for_tests(
        path_a.to_str().unwrap(),
        move || {
            std::fs::rename(&p_a1, &p_t1).expect("rename A to temp");
            std::fs::rename(&p_b1, &p_a1).expect("rename B to A");
        },
        move || {
            std::fs::rename(&p_a2, &p_b2).expect("rename current path_a (B) back to B");
            std::fs::rename(&p_t2, &p_a2).expect("rename temp (A) back to path_a");
        },
    );

    // 3. Attempt open.
    let result = MemoryStore::open_with_label_and_context(
        path_a.to_str().unwrap(),
        "global",
        &DbOpenContext::open_existing_allow("test:aba"),
    );

    // On main: this passes because validate_physical_db_identity_across_open
    // only samples the path (which is back to A).
    // With handle identity fix: the opened handle's inode (B) is checked and rejected!
    let err = match result {
        Ok(_) => panic!("ABA swap must be detected and rejected"),
        Err(err) => err,
    };
    assert!(
        err.to_string().contains("identity changed"),
        "unexpected error: {err}"
    );
}

#[cfg(windows)]
#[test]
fn windows_physical_identity_verifies_opened_handle() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("memory.db");
    let store = MemoryStore::open(path.to_str().unwrap()).unwrap();
    let opened = store
        .opened_physical_db_identity()
        .expect("opened identity");
    assert!(
        opened.starts_with("windows:"),
        "expected windows identity, got {opened}"
    );
    assert!(physical_db_identity_is_stable(opened));
    assert!(store.verify_opened_physical_db_identity(&path).is_ok());
    assert!(physical_db_identity_at_path(&dir.path().join("missing.db")).is_none());
}

#[cfg(unix)]
#[test]
fn partial_unix_file_identity_is_not_mutation_authority() {
    assert!(!has_stable_unix_file_identity(7, 0));
    assert!(!has_stable_unix_file_identity(0, 11));
    assert!(!has_stable_unix_file_identity(0, 0));
    assert!(has_stable_unix_file_identity(7, 11));
}

#[cfg(unix)]
#[test]
fn physical_identity_validation_rejects_path_replacement_during_open() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("memory.db");
    let replacement = dir.path().join("replacement.db");
    let conn_orig = Connection::open(&path).unwrap();
    conn_orig.execute_batch("PRAGMA user_version = 1;").unwrap();
    drop(conn_orig);
    let before = physical_db_identity_at_open(path.to_str().unwrap());
    let conn_repl = Connection::open(&replacement).unwrap();
    conn_repl.execute_batch("PRAGMA user_version = 2;").unwrap();
    std::fs::rename(&replacement, &path).unwrap();

    let error =
        validate_physical_db_identity_across_open(&conn_repl, path.to_str().unwrap(), before)
            .expect_err("path replacement must not be recorded as the opened connection");
    assert!(error.to_string().contains("identity changed"));
}

#[cfg(unix)]
#[test]
fn fresh_reopen_rejects_valid_database_substituted_after_initialization() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("memory.db");
    drop(MemoryStore::open(path.to_str().unwrap()).unwrap());
    let initialized_identity = physical_db_identity_at_open(path.to_str().unwrap())
        .expect("initialized DB has physical identity");

    let replacement = dir.path().join("replacement.db");
    drop(MemoryStore::open(replacement.to_str().unwrap()).unwrap());
    std::fs::rename(&replacement, &path).unwrap();

    let error = match MemoryStore::reopen_initialized_file_store(
        path.to_str().unwrap(),
        db::StoreIdentity {
            db_label: "unknown".to_string(),
            profile: db::StoreProfile::TachiFull,
        },
        false,
        initialized_identity,
        None,
    ) {
        Ok(_) => panic!("fresh reopen accepted a valid substituted DB"),
        Err(error) => error,
    };
    assert!(
        error
            .to_string()
            .contains("identity changed before identity-bound reopen"),
        "unexpected refusal: {error}"
    );
}

#[test]
fn detached_handle_verification_requires_a_stable_file_identity() {
    assert!(physical_db_identity_is_stable("unix:1:2"));
    assert!(physical_db_identity_is_stable("windows:1:2"));
    assert!(!physical_db_identity_is_stable("path:/tmp/memory.db"));
}

fn open_compat_summaries(db_path: &str) -> Result<MemoryStore, MemoryError> {
    MemoryStore::open_read_only_existing_schema_compat(
        db_path,
        ReadOnlyBackfillOperation::Summaries,
    )
}

fn stamp_v22_without_v23_guards(conn: &Connection) {
    conn.execute_batch(
        "DROP TRIGGER IF EXISTS memories_reserved_refs_insert_guard;
         DROP TRIGGER IF EXISTS memories_reserved_refs_update_guard;
         DELETE FROM hard_state
          WHERE namespace = 'migrations'
            AND key = 'v23_reserved_reference_guards';
         PRAGMA user_version = 22;",
    )
    .unwrap();
}

fn test_memory_entry(id: &str) -> MemoryEntry {
    MemoryEntry {
        id: id.to_string(),
        path: "/test/read-only-compat".to_string(),
        summary: String::new(),
        text: "read-only compatibility fixture".to_string(),
        importance: 0.5,
        timestamp: "2026-07-26T00:00:00Z".to_string(),
        valid_from: "2026-07-26T00:00:00Z".to_string(),
        valid_until: None,
        category: "fact".to_string(),
        topic: "test".to_string(),
        keywords: Vec::new(),
        persons: Vec::new(),
        entities: Vec::new(),
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
        retention_policy: None,
        domain: None,
        recall_count: 0,
        query_diversity: 0,
        tier: "raw".to_string(),
    }
}

#[test]
fn read_only_existing_schema_compat_opens_stamped_older_without_write_authority() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("stamped-older.db");
    drop(MemoryStore::open(&path.to_string_lossy()).unwrap());

    let offline = Connection::open(&path).unwrap();
    offline
        .execute_batch(
            "DROP TRIGGER memories_reserved_refs_insert_guard;
             DROP TRIGGER memories_reserved_refs_update_guard;
             DELETE FROM hard_state
              WHERE namespace = 'migrations'
                AND key = 'v23_reserved_reference_guards';
             PRAGMA user_version = 22;",
        )
        .unwrap();
    drop(offline);

    let mut store = open_compat_summaries(&path.to_string_lossy())
        .expect("stamped older DB must be inspectable without migration authority");
    assert_eq!(
        db::migrations::read_schema_version(store.connection()).unwrap(),
        22,
        "compatibility open must not rewrite the older stamp"
    );

    let error = store
        .upsert(&test_memory_entry("read-only-write-attempt"))
        .expect_err("compatibility open must remain read-only");
    assert!(
        error.to_string().to_ascii_lowercase().contains("readonly"),
        "unexpected read-only write refusal: {error}"
    );

    let offline = Connection::open(&path).unwrap();
    assert_eq!(
        db::migrations::read_schema_version(&offline).unwrap(),
        22,
        "rejected write must leave the older stamp unchanged"
    );
}

#[test]
fn read_only_existing_schema_compat_rejects_version_only_v22_at_open() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("version-only-v22.db");
    let offline = Connection::open(&path).unwrap();
    offline.execute_batch("PRAGMA user_version = 22;").unwrap();
    drop(offline);

    let error = match open_compat_summaries(&path.to_string_lossy()) {
        Ok(_) => panic!("version-only v22 DB was accepted as backfill-compatible"),
        Err(error) => error,
    };
    assert!(
        error
            .to_string()
            .contains("read-only backfill compatibility requires table 'memories'"),
        "unexpected version-only v22 refusal: {error}"
    );
}

#[test]
fn read_only_existing_schema_compat_rejects_malformed_v22_at_open() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("malformed-v22.db");
    let offline = Connection::open(&path).unwrap();
    offline
        .execute_batch(
            "CREATE TABLE memories(id TEXT PRIMARY KEY);
             PRAGMA user_version = 22;",
        )
        .unwrap();
    drop(offline);

    let error = match open_compat_summaries(&path.to_string_lossy()) {
        Ok(_) => panic!("malformed v22 memories schema was accepted"),
        Err(error) => error,
    };
    assert!(
        error
            .to_string()
            .contains("read-only backfill compatibility requires memories schema"),
        "unexpected malformed v22 refusal: {error}"
    );
}

#[test]
fn read_only_existing_schema_compat_rejects_negative_user_version_at_open() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("negative-version.db");
    drop(MemoryStore::open(&path.to_string_lossy()).unwrap());

    let offline = Connection::open(&path).unwrap();
    offline.execute_batch("PRAGMA user_version = -1;").unwrap();
    drop(offline);

    let error = match open_compat_summaries(&path.to_string_lossy()) {
        Ok(_) => panic!("negative user_version was normalized into an accepted v0 DB"),
        Err(error) => error,
    };
    assert!(
        error.to_string().contains(
            "read-only backfill compatibility refuses negative PRAGMA user_version -1"
        ),
        "unexpected negative-version refusal: {error}"
    );
}

#[test]
fn read_only_existing_schema_compat_preserves_unstamped_current_shape_policy() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("unstamped-current-shape.db");
    drop(MemoryStore::open(&path.to_string_lossy()).unwrap());

    let offline = Connection::open(&path).unwrap();
    offline.execute_batch("PRAGMA user_version = 0;").unwrap();
    drop(offline);

    let store = open_compat_summaries(&path.to_string_lossy())
        .expect("documented v0 current-shape DB must remain readable");
    assert_eq!(
        db::migrations::read_schema_version(store.connection()).unwrap(),
        0,
        "read-only compatibility open must preserve the unstamped value"
    );
}

#[test]
fn read_only_compat_operations_treat_missing_vector_as_optional_only_when_unused() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("v22-without-vector.db");
    drop(MemoryStore::open(&path.to_string_lossy()).unwrap());

    let offline = Connection::open(&path).unwrap();
    offline.execute("DROP TABLE memories_vec", []).unwrap();
    stamp_v22_without_v23_guards(&offline);
    drop(offline);

    for operation in [
        ReadOnlyBackfillOperation::Summaries,
        ReadOnlyBackfillOperation::Metadata,
    ] {
        let store = MemoryStore::open_read_only_existing_schema_compat(
            &path.to_string_lossy(),
            operation,
        )
        .unwrap_or_else(|error| {
            panic!("{operation:?} must not require optional vector capability: {error}")
        });
        assert_eq!(
            db::migrations::read_schema_version(store.connection()).unwrap(),
            22
        );
    }

    let error = match MemoryStore::open_read_only_existing_schema_compat(
        &path.to_string_lossy(),
        ReadOnlyBackfillOperation::Vectors,
    ) {
        Ok(_) => panic!("vector dry-run accepted a DB without memories_vec"),
        Err(error) => error,
    };
    assert!(
        error
            .to_string()
            .contains("vector dry-run requires virtual table 'memories_vec'"),
        "unexpected missing-vector refusal: {error}"
    );
}

#[test]
fn read_only_compat_rejects_memories_view_with_required_column_names() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("v22-memories-view.db");
    drop(MemoryStore::open(&path.to_string_lossy()).unwrap());

    let offline = Connection::open(&path).unwrap();
    offline
        .execute_batch(
            "DROP TABLE memories;
             CREATE VIEW memories AS
             SELECT '' AS id, '' AS path, '' AS source, '' AS topic,
                    '{}' AS metadata, '' AS text, '' AS summary,
                    1 AS revision, '[]' AS keywords, '' AS scope,
                    '' AS category, 0 AS archived
             WHERE 0;",
        )
        .unwrap();
    stamp_v22_without_v23_guards(&offline);
    drop(offline);

    let error = match open_compat_summaries(&path.to_string_lossy()) {
        Ok(_) => panic!("a view impersonated the memories table"),
        Err(error) => error,
    };
    assert!(
        error
            .to_string()
            .contains("summary dry-run requires ordinary table 'memories'"),
        "unexpected memories-view refusal: {error}"
    );
}

#[test]
fn read_only_compat_rejects_ordinary_tables_impersonating_virtual_capabilities() {
    for (table, replacement, operation, expected) in [
        (
            "memories_vec",
            "CREATE TABLE memories_vec(id TEXT PRIMARY KEY, embedding BLOB);",
            ReadOnlyBackfillOperation::Vectors,
            "vector dry-run requires virtual table 'memories_vec'",
        ),
        (
            "memories_fts",
            "CREATE TABLE memories_fts(
                id TEXT, path TEXT, summary TEXT, text TEXT,
                keywords TEXT, entities TEXT
             );",
            ReadOnlyBackfillOperation::Fts,
            "FTS dry-run requires virtual table 'memories_fts'",
        ),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(format!("ordinary-{table}.db"));
        drop(MemoryStore::open(&path.to_string_lossy()).unwrap());

        let offline = Connection::open(&path).unwrap();
        offline
            .execute_batch(&format!("DROP TABLE {table}; {replacement}"))
            .unwrap();
        stamp_v22_without_v23_guards(&offline);
        drop(offline);

        let error = match MemoryStore::open_read_only_existing_schema_compat(
            &path.to_string_lossy(),
            operation,
        ) {
            Ok(_) => panic!("ordinary table impersonated {table}"),
            Err(error) => error,
        };
        assert!(
            error.to_string().contains(expected),
            "unexpected ordinary-{table} refusal: {error}"
        );
    }
}

#[test]
fn read_only_compat_rejects_wrong_virtual_modules_and_shapes() {
    for (table, replacement, operation, expected) in [
        (
            "memories_vec",
            "CREATE VIRTUAL TABLE memories_vec USING fts5(id, embedding);",
            ReadOnlyBackfillOperation::Vectors,
            "vector dry-run requires vec0 memories_vec shape",
        ),
        (
            "memories_fts",
            "CREATE VIRTUAL TABLE memories_fts USING fts5(id);",
            ReadOnlyBackfillOperation::Fts,
            "FTS dry-run requires fts5 memories_fts shape",
        ),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(format!("wrong-virtual-{table}.db"));
        drop(MemoryStore::open(&path.to_string_lossy()).unwrap());

        let offline = Connection::open(&path).unwrap();
        offline
            .execute_batch(&format!("DROP TABLE {table}; {replacement}"))
            .unwrap();
        stamp_v22_without_v23_guards(&offline);
        drop(offline);

        let error = match MemoryStore::open_read_only_existing_schema_compat(
            &path.to_string_lossy(),
            operation,
        ) {
            Ok(_) => panic!("wrong virtual module/shape impersonated {table}"),
            Err(error) => error,
        };
        assert!(
            error.to_string().contains(expected),
            "unexpected wrong-{table} refusal: {error}"
        );
    }
}

#[test]
fn existing_read_write_does_not_recreate_missing_memories_vec() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("current.db");
    let store = MemoryStore::open(&path.to_string_lossy()).unwrap();
    let migration = db::authorize_schema_migration(&store.reserved_reference_write).unwrap();
    store
        .connection()
        .execute("DROP TABLE memories_vec", [])
        .unwrap();
    drop(migration);
    drop(store);

    let maintenance = MemoryStore::open_existing_read_write(&path.to_string_lossy()).unwrap();
    assert!(!maintenance.vec_available);
    let table_count: i64 = maintenance
        .connection()
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE type='table' AND name='memories_vec'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(table_count, 0);
}

#[test]
fn existing_opens_refuse_missing_reserved_reference_guards_before_repair() {
    type StoreOpener = fn(&str) -> Result<MemoryStore, MemoryError>;
    let openers: [(&str, StoreOpener); 3] = [
        ("ordinary", MemoryStore::open),
        ("maintenance", MemoryStore::open_existing_read_write),
        ("read-only-existing-schema-compat", open_compat_summaries),
    ];

    for (label, open) in openers {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(format!("{label}.db"));
        drop(MemoryStore::open(&path.to_string_lossy()).unwrap());

        let offline = Connection::open(&path).unwrap();
        offline
            .execute_batch(
                "DROP TRIGGER memories_reserved_refs_insert_guard;
                 DROP TRIGGER memories_reserved_refs_update_guard;",
            )
            .unwrap();
        drop(offline);

        let error = match open(&path.to_string_lossy()) {
            Ok(_) => panic!("{label} open silently repaired missing evidence guards"),
            Err(error) => error,
        };
        assert!(
            error
                .to_string()
                .contains("required trigger 'memories_reserved_refs_insert_guard' is missing"),
            "unexpected {label} refusal: {error}"
        );

        let offline = Connection::open(&path).unwrap();
        let remaining: i64 = offline
            .query_row(
                "SELECT COUNT(*) FROM main.sqlite_schema
                 WHERE type = 'trigger'
                   AND name IN (
                       'memories_reserved_refs_insert_guard',
                       'memories_reserved_refs_update_guard'
                   )",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(remaining, 0, "{label} open repaired before refusing");
    }
}

#[test]
fn partial_memories_schema_is_existing_and_refused_before_guard_repair() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("partial-memories.db");
    let offline = Connection::open(&path).unwrap();
    offline
        .execute_batch(
            "CREATE TABLE memories(
                id TEXT PRIMARY KEY,
                metadata TEXT NOT NULL DEFAULT '{}'
             );",
        )
        .unwrap();
    drop(offline);

    let error = match MemoryStore::open(&path.to_string_lossy()) {
        Ok(_) => panic!("partial memories schema was initialized and repaired"),
        Err(error) => error,
    };
    assert!(
        error
            .to_string()
            .contains("required trigger 'memories_reserved_refs_insert_guard' is missing"),
        "unexpected partial-schema refusal: {error}"
    );

    let offline = Connection::open(&path).unwrap();
    let schema: Vec<(String, String)> = offline
        .prepare(
            "SELECT type, name FROM main.sqlite_schema
             WHERE name NOT LIKE 'sqlite_%'
             ORDER BY type, name",
        )
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(
        schema,
        vec![("table".to_string(), "memories".to_string())],
        "refused open must not initialize or repair partial schema"
    );
    assert_eq!(db::migrations::read_schema_version(&offline).unwrap(), 0);
}

#[test]
fn authorized_v22_migration_installs_missing_reference_guards() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("authorized-migration.db");
    drop(MemoryStore::open(&path.to_string_lossy()).unwrap());

    let offline = Connection::open(&path).unwrap();
    offline
        .execute_batch(
            "DROP TRIGGER memories_reserved_refs_insert_guard;
             DROP TRIGGER memories_reserved_refs_update_guard;
             DELETE FROM hard_state
              WHERE namespace = 'migrations'
                AND key = 'v23_reserved_reference_guards';
             PRAGMA user_version = 22;",
        )
        .unwrap();
    drop(offline);

    let context = DbOpenContext::open_existing_allow("test:trigger-repair");
    let store = MemoryStore::open_with_context(&path.to_string_lossy(), &context)
        .expect("authorized migration repairs canonical guards");
    db::validate_persistent_trigger_inventory(store.connection(), true)
        .expect("authorized migration restores complete canonical inventory");
    assert_eq!(
        db::migrations::read_schema_version(store.connection()).unwrap(),
        db::migrations::EXPECTED_SCHEMA_VERSION
    );
}

#[test]
fn current_schema_reopen_preserves_canonical_reference_guards() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("current-reopen.db");
    drop(MemoryStore::open(&path.to_string_lossy()).unwrap());
    let offline = Connection::open(&path).unwrap();
    db::validate_persistent_trigger_inventory(&offline, true)
        .expect("fresh committed DB has canonical guards before reopen");
    drop(offline);

    let reopened = MemoryStore::open(&path.to_string_lossy())
        .expect("current schema with canonical guards must reopen");
    db::validate_persistent_trigger_inventory(reopened.connection(), true)
        .expect("current reopen preserves canonical guards");
}

#[test]
fn open_refuses_unknown_or_spoofed_persistent_triggers() {
    for (label, trigger_sql) in [
        (
            "unknown-auxiliary",
            "CREATE TRIGGER malicious_access_chain
             AFTER INSERT ON access_history
             BEGIN
               UPDATE memories SET path = '/wiki/persistent-forged';
             END;",
        ),
        (
            "spoofed-guard-case",
            "DROP TRIGGER memories_reserved_refs_update_guard;
             CREATE TRIGGER MeMoRiEs_ReSeRvEd_ReFs_UpDaTe_GuArD
             AFTER UPDATE ON memories
             BEGIN
               UPDATE memories SET source = 'wiki' WHERE id = NEW.id;
             END;",
        ),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(format!("{label}.db"));
        drop(MemoryStore::open(&path.to_string_lossy()).unwrap());

        let offline = Connection::open(&path).unwrap();
        offline.execute_batch(trigger_sql).unwrap();
        drop(offline);

        for (opener_label, open) in [
            (
                "ordinary",
                MemoryStore::open as fn(&str) -> Result<MemoryStore, MemoryError>,
            ),
            ("read-only-existing-schema-compat", open_compat_summaries),
        ] {
            let error = match open(&path.to_string_lossy()) {
                Ok(_) => panic!("persistent trigger {label} was exposed by {opener_label}"),
                Err(error) => error,
            };
            assert!(
                error
                    .to_string()
                    .contains("unsafe persistent trigger inventory"),
                "unexpected {label}/{opener_label} refusal: {error}"
            );
        }
    }
}

#[test]
fn fresh_schema_initialization_installs_exact_canonical_trigger_inventory() {
    let store = MemoryStore::open_in_memory().expect("initialize fresh store");
    let triggers: Vec<(String, String)> = store
        .connection()
        .prepare(
            "SELECT name, tbl_name
             FROM main.sqlite_schema
             WHERE type = 'trigger'
             ORDER BY name",
        )
        .expect("prepare trigger inventory")
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .expect("query trigger inventory")
        .collect::<Result<_, _>>()
        .expect("read trigger inventory");
    assert_eq!(
        triggers,
        vec![
            (
                "identity_verification_receipts_no_delete".to_string(),
                "identity_admission_verification_receipts".to_string(),
            ),
            (
                "identity_verification_receipts_no_replace".to_string(),
                "identity_admission_verification_receipts".to_string(),
            ),
            (
                "identity_verification_receipts_no_update".to_string(),
                "identity_admission_verification_receipts".to_string(),
            ),
            (
                "identity_verification_revocations_no_delete".to_string(),
                "identity_admission_verification_revocations".to_string(),
            ),
            (
                "identity_verification_revocations_no_replace".to_string(),
                "identity_admission_verification_revocations".to_string(),
            ),
            (
                "identity_verification_revocations_no_update".to_string(),
                "identity_admission_verification_revocations".to_string(),
            ),
            (
                "identity_verified_admissions_no_delete".to_string(),
                "identity_admissions".to_string(),
            ),
            (
                "identity_verified_admissions_no_replace".to_string(),
                "identity_admissions".to_string(),
            ),
            (
                "identity_verified_admissions_no_update".to_string(),
                "identity_admissions".to_string(),
            ),
            (
                "memories_reserved_refs_insert_guard".to_string(),
                "memories".to_string(),
            ),
            (
                "memories_reserved_refs_update_guard".to_string(),
                "memories".to_string(),
            ),
            (
                "memory_access_search_generation_after_delete".to_string(),
                "access_history".to_string(),
            ),
            (
                "memory_access_search_generation_after_insert".to_string(),
                "access_history".to_string(),
            ),
            (
                "memory_access_search_generation_after_update".to_string(),
                "access_history".to_string(),
            ),
            (
                "memory_edge_search_generation_after_delete".to_string(),
                "memory_edges".to_string(),
            ),
            (
                "memory_edge_search_generation_after_insert".to_string(),
                "memory_edges".to_string(),
            ),
            (
                "memory_edge_search_generation_after_update".to_string(),
                "memory_edges".to_string(),
            ),
            (
                "memory_search_generation_after_delete".to_string(),
                "memories".to_string(),
            ),
            (
                "memory_search_generation_after_insert".to_string(),
                "memories".to_string(),
            ),
            (
                "memory_search_generation_after_update".to_string(),
                "memories".to_string(),
            ),
            (
                "typed_record_kinds_insert_guard".to_string(),
                "hard_state".to_string(),
            ),
            (
                "typed_record_kinds_no_delete".to_string(),
                "hard_state".to_string(),
            ),
            (
                "typed_record_kinds_no_update".to_string(),
                "hard_state".to_string(),
            ),
            (
                "typed_record_supersessions_insert_guard".to_string(),
                "typed_record_supersessions".to_string(),
            ),
            (
                "typed_record_supersessions_no_delete".to_string(),
                "typed_record_supersessions".to_string(),
            ),
            (
                "typed_record_supersessions_no_update".to_string(),
                "typed_record_supersessions".to_string(),
            ),
            (
                "typed_records_insert_guard".to_string(),
                "typed_records".to_string(),
            ),
            (
                "typed_records_no_delete".to_string(),
                "typed_records".to_string(),
            ),
            (
                "typed_records_no_update".to_string(),
                "typed_records".to_string(),
            ),
        ],
        "fresh schema initialization must install the exact canonical trigger inventory"
    );
    db::validate_persistent_trigger_inventory(store.connection(), true)
        .expect("fresh schema trigger inventory must be canonical");
}

#[test]
fn existing_read_write_rejects_spoofed_current_version_without_initializing_schema() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("spoofed.db");
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch(&format!(
        "CREATE TABLE decoy(value TEXT);
         CREATE TABLE memories(id TEXT PRIMARY KEY, metadata TEXT NOT NULL DEFAULT '{{}}');
         PRAGMA user_version = {}",
        db::migrations::EXPECTED_SCHEMA_VERSION
    ))
    .unwrap();
    db::install_reserved_reference_guard(&conn).unwrap();
    drop(conn);

    let error = match MemoryStore::open_existing_read_write(&path.to_string_lossy()) {
        Ok(_) => panic!("spoofed schema was accepted"),
        Err(error) => error,
    };
    assert!(
        error.to_string().contains("no such table: hard_state"),
        "unexpected spoofed-schema refusal: {error}"
    );

    let conn = Connection::open(&path).unwrap();
    let tables: Vec<String> = conn
        .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(tables, vec!["decoy", "memories"]);
}

// ── #1599: upsert_batch ─────────────────────────────────────────────────

#[test]
fn upsert_batch_empty_slice_is_a_successful_no_op() {
    let mut store = MemoryStore::open_in_memory().expect("open_in_memory");
    store.upsert_batch(&[]).expect("empty batch is Ok");
    let stats = store.stats(true).expect("stats");
    assert_eq!(stats.total, 0, "empty batch must not write a row");
}

#[test]
fn retired_sticky_write_guard_precedes_cross_project_and_policy_bypasses() {
    let dir = tempfile::tempdir().expect("temp db dir");
    let path = dir.path().join("retired-sticky-guard.db");
    let policy = crate::KernelPolicy {
        path_validation_escape_hatch: true,
        ..Default::default()
    };
    let mut store = MemoryStore::open_with_label(&path.to_string_lossy(), "global")
        .expect("open labelled store")
        .with_kernel_policy(policy);

    let mut rejected = |id: &str, path: &str, category: &str| {
        let mut entry = test_memory_entry(id);
        entry.path = path.to_string();
        entry.category = category.to_string();
        entry.metadata = serde_json::json!({"allow_cross_project": true});
        let error = store
            .upsert(&entry)
            .expect_err("ordinary upsert must reject retired sticky input");
        assert!(
            error.to_string().contains("tachi_a2a"),
            "unexpected error: {error}"
        );
        assert!(store.get(id).expect("get after refusal").is_none());
    };

    rejected("sticky-path-root", "/sticky", "fact");
    rejected("sticky-path-alias", "//STICKY///legacy/", "fact");
    rejected("sticky-category-case", "/notes/ordinary", " Sticky ");

    let mut insert_only = test_memory_entry("sticky-insert-only");
    insert_only.path = "/sticky/legacy".to_string();
    insert_only.metadata = serde_json::json!({"allow_cross_project": true});
    let error = store
        .insert_if_absent(&insert_only)
        .expect_err("insert-only writer must reject retired path");
    assert!(error.to_string().contains("tachi_a2a"), "{error}");

    let mut idless = test_memory_entry("sticky-idless");
    idless.category = "STICKY".to_string();
    let error = store
        .upsert_idless(&idless, "retired-sticky-identity")
        .expect_err("id-less writer must reject retired category");
    assert!(error.to_string().contains("tachi_a2a"), "{error}");

    let mut batch = test_memory_entry("sticky-batch");
    batch.path = "/sticky/batch".to_string();
    let error = store
        .upsert_batch(&[batch])
        .expect_err("batch writer must reject retired path");
    assert!(error.to_string().contains("tachi_a2a"), "{error}");

    let mut validated = test_memory_entry("sticky-validated");
    validated.category = "sticky".to_string();
    let error = store
        .upsert_with_validated_reference_mutations(
            &validated,
            None,
            &serde_json::Map::new(),
            &[],
        )
        .expect_err("validated-reference writer must reject retired category");
    assert!(error.to_string().contains("tachi_a2a"), "{error}");

    let mut ordinary = test_memory_entry("ordinary-near-sticky");
    ordinary.path = "/stickiness/allowed".to_string();
    ordinary.category = "fact".to_string();
    ordinary.metadata = serde_json::json!({"allow_cross_project": true});
    store
        .upsert(&ordinary)
        .expect("near-match path remains allowed");
}

#[test]
fn upsert_batch_atomically_writes_main_rows_fts_and_vector_projections() {
    let mut store = MemoryStore::open_in_memory().expect("open_in_memory");
    // Each row needs a distinct body. memcore's write path runs
    // write-time near-duplicate consolidation
    // (`memcore::db::memory_crud::merge_into_jaccard_candidate`,
    // crates/memcore/src/db/memory_crud.rs:284; token-Jaccard > 0.9 at
    // :324) on every net-new id within the same transaction. Giving both
    // rows of a batch the shared `test_memory_entry` body ("read-only
    // compatibility fixture", token-Jaccard 1.0) merges the second row
    // into the first at write time and stamps it `superseded_by` before
    // this test's assertions ever run (tachi#1571/#1572's exact
    // pattern) — `store.get` still returns the superseded row, but
    // ordinary `search` correctly excludes it, which is what made
    // `hit_ids.contains("batch-ok-2")` fail. Distinct bodies keep both
    // rows live while the shared prefix keeps them both matching the FTS
    // query below.
    let mut e1 = test_memory_entry("batch-ok-1");
    // This test exercises ordinary searchable batch records, not the
    // opt-in-only synthetic /test namespace used by the helper.
    e1.path = "/notes/batch-one".to_string();
    e1.text = "read-only compatibility fixture batch entry one".to_string();
    e1.vector = Some(vec![0.25_f32; 1024]);
    let mut e2 = test_memory_entry("batch-ok-2");
    e2.path = "/notes/batch-two".to_string();
    e2.text = "read-only compatibility fixture batch entry two".to_string();
    e2.vector = Some(vec![0.75_f32; 1024]);

    store
        .upsert_batch(&[e1, e2])
        .expect("a valid batch must succeed as a single transaction");

    assert!(store.get("batch-ok-1").expect("get").is_some());
    assert!(store.get("batch-ok-2").expect("get").is_some());

    // Both rows must still be live, not silently folded into each other
    // by write-time near-duplicate merge: `MemoryStore::get` returns
    // superseded rows too, so this is the assertion that would actually
    // catch a regression back to a shared body.
    for id in ["batch-ok-1", "batch-ok-2"] {
        let superseded_by: Option<String> = store
            .connection()
            .query_row(
                "SELECT superseded_by FROM memories WHERE id = ?1",
                [id],
                |row| row.get(0),
            )
            .unwrap_or_else(|error| panic!("row {id} must exist: {error}"));
        assert!(
            superseded_by.is_none(),
            "row {id} was superseded by {superseded_by:?}; the batch's rows merged into \
             one another instead of staying independent"
        );
    }

    // FTS projection: the fixture's shared text prefix is queryable, and
    // both distinct-bodied rows are live hits.
    let hits = store
        .search("read-only compatibility fixture", None)
        .expect("fts search");
    let hit_ids: std::collections::BTreeSet<String> =
        hits.into_iter().map(|r| r.entry.id).collect();
    assert!(hit_ids.contains("batch-ok-1"));
    assert!(hit_ids.contains("batch-ok-2"));
    assert_eq!(
        hit_ids.len(),
        2,
        "expected exactly the batch's two rows as FTS hits, got {hit_ids:?}"
    );

    // Vector projection: both rows landed in memories_vec, not just `memories`.
    let vec_ids: std::collections::BTreeSet<String> = store
        .connection()
        .prepare("SELECT id FROM memories_vec ORDER BY id")
        .expect("prepare")
        .query_map([], |row| row.get::<_, String>(0))
        .expect("query")
        .collect::<Result<_, _>>()
        .expect("read memories_vec ids");
    assert!(vec_ids.contains("batch-ok-1"));
    assert!(vec_ids.contains("batch-ok-2"));
    assert_eq!(
        vec_ids.len(),
        2,
        "expected exactly the batch's two rows in memories_vec, got {vec_ids:?}"
    );
}

#[test]
fn upsert_batch_rolls_back_the_whole_batch_when_a_later_entry_fails() {
    let mut store = MemoryStore::open_in_memory().expect("open_in_memory");
    let ok_entry = test_memory_entry("batch-rollback-ok");
    // `wiki-rem:` ids are reserved for the dedicated REM insert-once seam
    // (see `is_reserved_wiki_rem_id`, enforced inside
    // `upsert_prepared_within_tx`). This entry passes
    // `validate_write_path` (its path is ordinary) but fails once its
    // write actually reaches the transaction body, proving the whole
    // `BEGIN IMMEDIATE` transaction — not just pre-validation — rolls
    // back the earlier, otherwise-valid entry too.
    let bad_entry = test_memory_entry("wiki-rem:batch-rollback-bad");

    let error = store
        .upsert_batch(&[ok_entry, bad_entry])
        .expect_err("a batch with a later invalid entry must fail entirely");
    assert!(
        matches!(error, MemoryError::InvalidArg(_)),
        "unexpected error variant: {error:?}"
    );

    assert!(
        store.get("batch-rollback-ok").expect("get").is_none(),
        "the earlier, individually-valid entry must not have been persisted"
    );
    let stats = store.stats(true).expect("stats");
    assert_eq!(
        stats.total, 0,
        "a rolled-back batch must leave zero rows behind"
    );
}

#[test]
fn upsert_batch_refuses_an_anchor_namespace_id_and_persists_nothing() {
    // tachi#1599 checkpoint 4: single-row `upsert` refuses `anchor:`-ids
    // at the top level (memory_crud.rs:2561-2566, reserved for
    // `ensure_anchor`); `upsert_batch` must mirror that guard in its
    // pre-walk instead of silently letting the shared `upsert_within_tx`
    // seam create/overwrite a reserved anchor row.
    let mut reference_store = MemoryStore::open_in_memory().expect("open_in_memory");
    let reference_error = reference_store
        .upsert(&test_memory_entry("anchor:batch-guard-bad"))
        .expect_err("single-row upsert must refuse an anchor: id");

    let mut store = MemoryStore::open_in_memory().expect("open_in_memory");
    let ok_entry = test_memory_entry("batch-anchor-guard-ok");
    let anchor_entry = test_memory_entry("anchor:batch-guard-bad");

    let error = store
        .upsert_batch(&[ok_entry, anchor_entry])
        .expect_err("a batch containing an anchor: id must be refused");
    assert!(
        matches!(error, MemoryError::InvalidArg(_)),
        "unexpected error variant: {error:?}"
    );
    assert_eq!(
        error.to_string(),
        reference_error.to_string(),
        "batch refusal must match the single-row refusal's error shape"
    );

    assert!(
        store.get("batch-anchor-guard-ok").expect("get").is_none(),
        "the earlier, individually-valid entry must not have been persisted"
    );
    let stats = store.stats(true).expect("stats");
    assert_eq!(
        stats.total, 0,
        "a batch refused for an anchor: id must leave zero rows behind, and no \
         transaction should even have opened"
    );
}

#[test]
fn upsert_batch_refuses_a_blank_id_and_persists_nothing() {
    // tachi#1599 checkpoint 4: single-row `upsert` refuses a blank/empty
    // id at the top level (memory_crud.rs:2550-2554); `upsert_batch`
    // must mirror that guard in its pre-walk for the same reason as the
    // `anchor:` guard above.
    let mut reference_store = MemoryStore::open_in_memory().expect("open_in_memory");
    let reference_error = reference_store
        .upsert(&test_memory_entry("   "))
        .expect_err("single-row upsert must refuse a blank id");

    let mut store = MemoryStore::open_in_memory().expect("open_in_memory");
    let ok_entry = test_memory_entry("batch-blank-guard-ok");
    let blank_entry = test_memory_entry("   ");

    let error = store
        .upsert_batch(&[ok_entry, blank_entry])
        .expect_err("a batch containing a blank id must be refused");
    assert!(
        matches!(error, MemoryError::InvalidArg(_)),
        "unexpected error variant: {error:?}"
    );
    assert_eq!(
        error.to_string(),
        reference_error.to_string(),
        "batch refusal must match the single-row refusal's error shape"
    );

    assert!(
        store.get("batch-blank-guard-ok").expect("get").is_none(),
        "the earlier, individually-valid entry must not have been persisted"
    );
    let stats = store.stats(true).expect("stats");
    assert_eq!(
        stats.total, 0,
        "a batch refused for a blank id must leave zero rows behind, and no \
         transaction should even have opened"
    );
}

// ── #1602: the refusals live at the shared transactional seam ───────────
//
// `upsert_batch_with_precommit` has no pre-walk of its own — it goes
// straight to `upsert_batch_in_tx`. So every assertion below is a direct
// test of the guard inside
// `db::upsert_within_tx`/`memory_crud::upsert_prepared_within_tx`: if the
// refusal were only at `upsert_with_idless_identity` (single-row) or in
// `upsert_batch`'s pre-walk, these batches would commit.

#[test]
fn upsert_batch_with_precommit_refuses_an_anchor_namespace_id_at_the_seam() {
    let mut reference_store = MemoryStore::open_in_memory().expect("open_in_memory");
    let reference_error = reference_store
        .upsert(&test_memory_entry("anchor:precommit-guard-bad"))
        .expect_err("single-row upsert must refuse an anchor: id");

    let mut store = MemoryStore::open_in_memory().expect("open_in_memory");
    let ok_entry = test_memory_entry("precommit-anchor-guard-ok");
    let anchor_entry = test_memory_entry("anchor:precommit-guard-bad");

    let precommit_ran = std::cell::Cell::new(false);
    let error = store
        .upsert_batch_with_precommit(&[ok_entry, anchor_entry], |_tx| {
            precommit_ran.set(true);
            Ok(())
        })
        .expect_err("a precommit batch containing an anchor: id must be refused");
    assert!(
        matches!(error, MemoryError::InvalidArg(_)),
        "unexpected error variant: {error:?}"
    );
    assert_eq!(
        error.to_string(),
        reference_error.to_string(),
        "the seam's refusal must be byte-identical to the single-row refusal"
    );
    assert!(
        !precommit_ran.get(),
        "the row loop must fail before the pre-commit closure runs"
    );

    assert!(
        store
            .get("precommit-anchor-guard-ok")
            .expect("get")
            .is_none(),
        "the earlier, individually-valid entry must roll back with the batch"
    );
    let stats = store.stats(true).expect("stats");
    assert_eq!(
        stats.total, 0,
        "a batch refused mid-loop must leave zero rows behind"
    );
}

#[test]
fn upsert_batch_with_precommit_refuses_a_blank_id_at_the_seam() {
    let mut reference_store = MemoryStore::open_in_memory().expect("open_in_memory");
    let reference_error = reference_store
        .upsert(&test_memory_entry("   "))
        .expect_err("single-row upsert must refuse a blank id");

    let mut store = MemoryStore::open_in_memory().expect("open_in_memory");
    let ok_entry = test_memory_entry("precommit-blank-guard-ok");
    let blank_entry = test_memory_entry("   ");

    let error = store
        .upsert_batch_with_precommit(&[ok_entry, blank_entry], |_tx| Ok(()))
        .expect_err("a precommit batch containing a blank id must be refused");
    assert_eq!(
        error.to_string(),
        reference_error.to_string(),
        "the seam's refusal must be byte-identical to the single-row refusal"
    );

    assert!(
        store
            .get("precommit-blank-guard-ok")
            .expect("get")
            .is_none(),
        "the earlier, individually-valid entry must roll back with the batch"
    );
    let stats = store.stats(true).expect("stats");
    assert_eq!(
        stats.total, 0,
        "a batch refused mid-loop must leave zero rows behind"
    );
}

#[test]
fn preserving_anchor_rows_variant_carries_an_existing_anchor_row_verbatim() {
    // The tidy-migration exemption: a whole-store copy must be able to
    // carry an `anchor:` row that `ensure_anchor` legitimately created in
    // the source database, while the ordinary batch entry point still
    // refuses the very same entry.
    let source = MemoryStore::open_in_memory().expect("open_in_memory");
    let anchor_id = source
        .ensure_anchor(crate::db::AnchorKind::Issue, "kckylechen1/tachi:1602")
        .expect("ensure_anchor creates the source anchor row");
    let anchor_entry = source
        .get(&anchor_id)
        .expect("get")
        .expect("the source anchor row must be readable");

    let mut target = MemoryStore::open_in_memory().expect("open_in_memory");
    let refusal = target
        .upsert_batch_with_precommit(std::slice::from_ref(&anchor_entry), |_tx| Ok(()))
        .expect_err("the ordinary precommit entry point must still refuse anchor: ids");
    assert!(
        matches!(refusal, MemoryError::InvalidArg(_)),
        "unexpected error variant: {refusal:?}"
    );

    let rows_after: usize = target
        .upsert_batch_with_precommit_preserving_anchor_rows(
            &[
                test_memory_entry("migration-carried-ordinary"),
                anchor_entry,
            ],
            |view| view.memory_row_count(),
        )
        .expect("the trusted whole-store-copy variant must carry the anchor row");
    assert_eq!(rows_after, 2, "both copied rows must be visible in-tx");

    assert!(
        target.get(&anchor_id).expect("get").is_some(),
        "the carried anchor row must be durable in the target"
    );
    // The carried row is still a valid anchor: `ensure_anchor`'s
    // read-verify guard (a) accepts it as the existing row for the same
    // (kind, key) instead of failing closed on a mismatch.
    assert_eq!(
        target
            .ensure_anchor(crate::db::AnchorKind::Issue, "kckylechen1/tachi:1602")
            .expect("ensure_anchor must accept the carried row"),
        anchor_id
    );
}

#[test]
fn preserving_anchor_rows_variant_still_refuses_other_reserved_namespaces() {
    // The exemption is anchor-only: `wiki-rem:` and the blank id stay
    // refused even for the trusted whole-store-copy writer.
    let mut store = MemoryStore::open_in_memory().expect("open_in_memory");
    let wiki_rem_error = store
        .upsert_batch_with_precommit_preserving_anchor_rows(
            &[test_memory_entry("wiki-rem:1602-narrowness")],
            |_tx| Ok(()),
        )
        .expect_err("wiki-rem: ids must stay refused");
    assert!(
        wiki_rem_error
            .to_string()
            .contains("reserved 'wiki-rem:' namespace"),
        "unexpected error: {wiki_rem_error}"
    );

    let blank_error = store
        .upsert_batch_with_precommit_preserving_anchor_rows(
            &[test_memory_entry("   ")],
            |_tx| Ok(()),
        )
        .expect_err("a blank id must stay refused");
    assert!(
        blank_error
            .to_string()
            .contains("entry.id must be provided by caller"),
        "unexpected error: {blank_error}"
    );

    let stats = store.stats(true).expect("stats");
    assert_eq!(stats.total, 0, "no refused batch may leave a row behind");
}
