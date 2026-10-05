use super::*;
use std::cell::RefCell;
use std::rc::Rc;

fn missing(error: MemoryError) -> Vec<String> {
    match error {
        MemoryError::CurrentSchemaIncomplete { missing, .. } => missing,
        other => panic!("current-schema refusal expected, got {other:?}"),
    }
}

fn refused(result: Result<MemoryStore, MemoryError>) -> MemoryError {
    match result {
        Err(error) => error,
        Ok(store) => {
            drop(store);
            panic!("damaged current input must be refused");
        }
    }
}

/// A dropped source table can leave its external-content FTS unreadable.
/// Retain that error and every readable table/shadow's raw values, rather than
/// making the preservation oracle itself fail before admission executes.
fn damaged_snapshot(conn: &Connection) -> Value {
    let content: BTreeMap<_, _> = tables(conn)
        .into_iter()
        .map(|name| {
            let read = (|| -> rusqlite::Result<Vec<Vec<String>>> {
                let mut stmt = conn.prepare(&format!("SELECT * FROM {}", quote(&name)))?;
                let width = stmt.column_count();
                let mut rows = stmt
                    .query_map([], |row| {
                        (0..width)
                            .map(|i| row.get_ref(i).map(|value| format!("{value:?}")))
                            .collect::<rusqlite::Result<Vec<_>>>()
                    })?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                rows.sort();
                Ok(rows)
            })();
            let value = match read {
                Ok(rows) => json!(rows),
                Err(error) => json!({"read_error":error.to_string()}),
            };
            (name, value)
        })
        .collect();
    json!({
        "schema":query_rows(conn, "SELECT type,name,tbl_name,sql FROM main.sqlite_schema ORDER BY type,name"),
        "content":content,
        "user_version":query_rows(conn,"PRAGMA user_version"),
        "schema_version":query_rows(conn,"PRAGMA schema_version"),
        "journal_mode":query_rows(conn,"PRAGMA journal_mode"),
    })
}

fn clone_fixture(source: &Connection, dir: &Path, case: usize) -> PathBuf {
    let dir = dir.join(case.to_string());
    std::fs::create_dir(&dir).unwrap();
    let path = dir.join("tachi-memory.db");
    let mut destination = Connection::open(&path).unwrap();
    let backup = rusqlite::backup::Backup::new(source, &mut destination).unwrap();
    assert_eq!(backup.step(-1).unwrap(), rusqlite::backup::StepResult::Done);
    drop(backup);
    path
}

#[test]
fn runtime_presence_catalog_matches_frozen_initializer_inventory() {
    for profile in [StoreProfile::PortableKernel, StoreProfile::TachiFull] {
        let (_dir, path) = fixture(profile);
        let conn = Connection::open(path).unwrap();
        let classified = crate::db::schema_inventory::classified_inventory(&conn);
        let required = classified.required.as_object().unwrap();
        let runtime = crate::db::required_inventory(profile).unwrap();
        assert_eq!(
            runtime.object_keys(),
            required.keys().cloned().collect::<Vec<_>>()
        );
        for (table, columns) in runtime.table_columns() {
            // Independently decode the existing lossless snapshot format in
            // this test only; production never parses these diagnostic bytes.
            let expected: Vec<String> = required[&format!("table:{table}")]["columns"]
                .as_array()
                .unwrap()
                .iter()
                .map(|row| {
                    let value = row[1].as_str().unwrap();
                    let bytes: Vec<u8> = serde_json::from_str(
                        value
                            .strip_prefix("Text(")
                            .unwrap()
                            .strip_suffix(')')
                            .unwrap(),
                    )
                    .unwrap();
                    String::from_utf8(bytes).unwrap()
                })
                .collect();
            assert_eq!(
                columns
                    .into_iter()
                    .collect::<std::collections::BTreeSet<_>>(),
                expected
                    .into_iter()
                    .collect::<std::collections::BTreeSet<_>>(),
                "required columns for {profile:?}/{table}"
            );
        }
    }
}

#[test]
fn every_required_object_refuses_without_repair_for_both_profiles() {
    for profile in [StoreProfile::PortableKernel, StoreProfile::TachiFull] {
        let (_base_dir, base) = fixture(profile);
        let source = Connection::open(base).unwrap();
        let cases = tempfile::tempdir().unwrap();
        let keys = crate::db::required_inventory(profile)
            .unwrap()
            .object_keys();
        for (i, key) in keys.iter().enumerate() {
            let path = clone_fixture(&source, cases.path(), i);
            let conn = Connection::open(&path).unwrap();
            let (kind, name) = key.split_once(':').unwrap();
            conn.execute_batch(&format!("DROP {} {}", kind.to_uppercase(), quote(name)))
                .unwrap();
            let earlier = crate::db::validate_input_trigger_inventory(&conn)
                .and_then(|_| crate::db::migrations::validate_current_schema_integrity(&conn))
                .err()
                .map(|error| error.to_string());
            let before = damaged_snapshot(&conn);
            let artifacts = migration_artifacts(&path);
            drop(conn);
            let error = refused(MemoryStore::open_with_context(
                path.to_str().unwrap(),
                &context(profile, MigrationAuthority::Deny),
            ));
            let after = damaged_snapshot(&Connection::open(&path).unwrap());
            assert!(
                before == after,
                "{profile:?}/{key}: admission changed damaged input"
            );
            assert_eq!(
                artifacts,
                migration_artifacts(&path),
                "{profile:?}/{key}: artifacts"
            );
            if let Some(earlier) = earlier {
                assert_eq!(
                    error.to_string(),
                    earlier,
                    "existing validator precedence for {key}"
                );
            } else {
                assert!(missing(error).contains(key), "refusal must name {key}");
            }
        }
        eprintln!(
            "#1995 required-object census {profile:?}: {} cases",
            keys.len()
        );
    }
}

#[test]
fn allowlisted_objects_rebuild_for_both_profiles() {
    for profile in [StoreProfile::PortableKernel, StoreProfile::TachiFull] {
        let (_base_dir, base) = fixture(profile);
        let source = Connection::open(&base).unwrap();
        crate::db::configure_connection(&source).unwrap();
        crate::db::ensure_reserved_reference_write_guard(&source).unwrap();
        source.execute_batch("INSERT INTO memories(id,path,text,timestamp,category,source,scope) VALUES ('fixture','/fixture/admission','source intact','2026-10-05T00:00:00Z','fact','manual','general')").unwrap();
        let classified = crate::db::schema_inventory::classified_inventory(&source);
        let keys: Vec<_> = classified
            .classes
            .iter()
            .filter(|(_, value)| value["class"] == "derived")
            .map(|(key, _)| key.clone())
            .collect();
        let cases = tempfile::tempdir().unwrap();
        for (i, key) in keys.iter().enumerate() {
            let path = clone_fixture(&source, cases.path(), i);
            let conn = Connection::open(&path).unwrap();
            let (kind, name) = key.split_once(':').unwrap();
            let generation: i64 = conn
                .query_row(
                    "SELECT generation FROM memory_search_generation WHERE id=1",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            conn.execute_batch(&format!("DROP {} {}", kind.to_uppercase(), quote(name)))
                .unwrap();
            drop(conn);
            let store = MemoryStore::open_with_context(
                path.to_str().unwrap(),
                &context(profile, MigrationAuthority::Deny),
            )
            .unwrap();
            let present: bool = store
                .connection()
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type=?1 AND name=?2)",
                    [kind, name],
                    |r| r.get(0),
                )
                .unwrap();
            assert!(present, "{profile:?}/{key}: rebuild missing");
            if name == "memories_fts" || name == "memories_symbolic_fts" {
                assert!(
                    store.search_generation().unwrap() > generation,
                    "FTS rebuild must invalidate cache"
                );
            }
        }
        eprintln!(
            "#1995 derived-object rebuild census {profile:?}: {} cases",
            keys.len()
        );
    }
}

#[test]
fn missing_required_column_refuses_without_inventing_approval() {
    for authority in [
        MigrationAuthority::Deny,
        MigrationAuthority::Allow {
            approved_by: "test:#1995".into(),
        },
    ] {
        let (_dir, path) = fixture(StoreProfile::TachiFull);
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch("INSERT INTO hub_capabilities(id,type,name,review_status) VALUES ('cap-fixture','skill','fixture','rejected'); DROP INDEX idx_hub_cap_review_status; ALTER TABLE hub_capabilities DROP COLUMN review_status;").unwrap();
        drop(conn);
        assert_refused_without_repair(&path, authority, "hub_capabilities.review_status");
    }
}

#[test]
fn missing_objects_precede_bad_role_but_preserve_integrity_and_bad_profile_errors() {
    for profile in [StoreProfile::PortableKernel, StoreProfile::TachiFull] {
        let (_dir, path) = fixture(profile);
        let conn = Connection::open(&path).unwrap();
        let table = if profile == StoreProfile::TachiFull {
            "exec_env_worktree_identities"
        } else {
            "derived_items"
        };
        conn.execute_batch(&format!("DROP TABLE {table}; INSERT INTO hard_state(namespace,key,value_json,version,created_at,updated_at) VALUES ('store_identity','role','{{\"value\":7}}',1,'fixture','fixture');")).unwrap();
        drop(conn);
        assert_refused_without_repair_with_profile(&path, profile, MigrationAuthority::Deny, table);
        let conn = Connection::open(&path).unwrap();
        conn.execute("UPDATE hard_state SET value_json='{\"value\":7}' WHERE namespace='store_identity' AND key='profile'",[]).unwrap();
        let role_error = crate::db::store_identity::read_identity(&conn, &path)
            .unwrap_err()
            .to_string();
        let before = logical_snapshot(&conn);
        drop(conn);
        let error = refused(MemoryStore::open_with_context(
            path.to_str().unwrap(),
            &context(profile, MigrationAuthority::Deny),
        ));
        assert_eq!(error.to_string(), role_error);
        assert_eq!(before, logical_snapshot(&Connection::open(&path).unwrap()));
    }
    let (_dir, path) = fixture(StoreProfile::TachiFull);
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch("DROP TABLE derived_items; DROP INDEX idx_delivery_intents_execution;")
        .unwrap();
    let earlier = crate::db::migrations::validate_current_schema_integrity(&conn)
        .unwrap_err()
        .to_string();
    drop(conn);
    assert_eq!(
        refused(MemoryStore::open_with_context(
            path.to_str().unwrap(),
            &context(StoreProfile::TachiFull, MigrationAuthority::Deny)
        ))
        .to_string(),
        earlier
    );
}

#[test]
fn profile_baseline_selection_preserves_adoption_and_refusal_boundaries() {
    for (stored, required) in [
        (StoreProfile::TachiFull, StoreProfile::TachiFull),
        (StoreProfile::PortableKernel, StoreProfile::PortableKernel),
        (StoreProfile::PortableKernel, StoreProfile::TachiFull),
    ] {
        let (_dir, path) = fixture(stored);
        let path = path.canonicalize().unwrap();
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch("DROP TABLE derived_items").unwrap();
        if stored == required {
            conn.execute(
                "DELETE FROM hard_state WHERE namespace='store_identity' AND key='profile'",
                [],
            )
            .unwrap();
        }
        let prior_profile_error =
            crate::db::store_identity::resolve_profile(None, false, required, &path)
                .err()
                .map(|e| e.to_string());
        let before = logical_snapshot(&conn);
        let artifacts = migration_artifacts(&path);
        drop(conn);
        let error = refused(MemoryStore::open_with_context(
            path.to_str().unwrap(),
            &context(required, MigrationAuthority::Deny),
        ));
        if stored == StoreProfile::PortableKernel && stored == required {
            assert_eq!(error.to_string(), prior_profile_error.unwrap());
        } else {
            assert_eq!(missing(error), vec!["table:derived_items"]);
        }
        assert_eq!(before, logical_snapshot(&Connection::open(&path).unwrap()));
        assert_eq!(artifacts, migration_artifacts(&path));
    }
    for payload in [r#"{"value":7}"#, r#"{"value":"unsupported-profile"}"#] {
        let (_dir, path) = fixture(StoreProfile::TachiFull);
        let path = path.canonicalize().unwrap();
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch("DROP TABLE derived_items").unwrap();
        conn.execute("UPDATE hard_state SET value_json=?1 WHERE namespace='store_identity' AND key='profile'", [payload]).unwrap();
        let expected = crate::db::store_identity::read_stamp(&conn, crate::db::STORE_PROFILE_KEY)
            .and_then(|token| {
                crate::db::store_identity::resolve_profile(
                    token
                        .map(|token| crate::db::store_profile::parse_stored_profile(&token, &path))
                        .transpose()?,
                    false,
                    StoreProfile::TachiFull,
                    &path,
                )
                .map(|_| ())
            })
            .unwrap_err()
            .to_string();
        let before = logical_snapshot(&conn);
        drop(conn);
        let error = refused(MemoryStore::open_with_context(
            path.to_str().unwrap(),
            &context(StoreProfile::TachiFull, MigrationAuthority::Deny),
        ));
        assert_eq!(error.to_string(), expected);
        assert_eq!(before, logical_snapshot(&Connection::open(path).unwrap()));
    }
}

#[test]
fn refusal_lists_all_missing_objects_in_stable_order() {
    let (_dir, path) = fixture(StoreProfile::TachiFull);
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch("DROP TABLE derived_items; DROP TABLE exec_env_worktree_identities; DROP INDEX idx_session_claims_identity_active;").unwrap();
    drop(conn);
    let error = refused(MemoryStore::open_with_context(
        path.to_str().unwrap(),
        &context(
            StoreProfile::TachiFull,
            MigrationAuthority::Allow {
                approved_by: "test:#1995".into(),
            },
        ),
    ));
    assert_eq!(
        missing(error),
        vec![
            "index:idx_session_claims_identity_active",
            "table:derived_items",
            "table:exec_env_worktree_identities"
        ]
    );
}

#[test]
fn read_only_maintenance_and_public_initializer_refuse_before_returning_a_handle() {
    type OpenStore = fn(&str) -> Result<MemoryStore, MemoryError>;
    let opens: [OpenStore; 3] = [
        MemoryStore::open_read_only,
        MemoryStore::open_read_only_immutable,
        MemoryStore::open_existing_read_write,
    ];
    for open in opens {
        for drop_trigger in [false, true] {
            let (_dir, path) = fixture(StoreProfile::TachiFull);
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch("DROP TABLE derived_items").unwrap();
            if drop_trigger {
                conn.execute_batch("DROP TRIGGER memories_reserved_refs_insert_guard")
                    .unwrap();
            }
            let before = logical_snapshot(&conn);
            let artifacts = migration_artifacts(&path);
            drop(conn);
            let error = refused(open(path.to_str().unwrap()));
            if drop_trigger {
                assert!(
                    error
                        .to_string()
                        .contains("memories_reserved_refs_insert_guard"),
                    "{error:?}"
                );
            } else {
                assert_eq!(missing(error), vec!["table:derived_items"]);
            }
            assert_eq!(before, logical_snapshot(&Connection::open(&path).unwrap()));
            assert_eq!(artifacts, migration_artifacts(&path));
        }
    }
    let (_dir, path) = fixture(StoreProfile::TachiFull);
    let conn = Connection::open(path).unwrap();
    conn.execute_batch("DROP TABLE derived_items").unwrap();
    let before = logical_snapshot(&conn);
    assert_eq!(
        missing(crate::db::init_schema(&conn).unwrap_err()),
        vec!["table:derived_items"]
    );
    assert_eq!(before, logical_snapshot(&conn));
}

#[test]
fn required_table_dropped_in_transaction_window_is_not_recreated() {
    for profile in [StoreProfile::PortableKernel, StoreProfile::TachiFull] {
        let (_dir, path) = fixture(profile);
        // Converge the marker after optional vector provisioning before arming.
        drop(
            MemoryStore::open_with_context(
                path.to_str().unwrap(),
                &context(profile, MigrationAuthority::Deny),
            )
            .unwrap(),
        );
        let artifacts = migration_artifacts(&path);
        let post_hook = Rc::new(RefCell::new(None));
        let captured = Rc::clone(&post_hook);
        crate::db::schema_test_hooks::arm_before_schema_transaction(move |path| {
            let other = Connection::open(path).unwrap();
            other.execute_batch("DROP TABLE derived_items").unwrap();
            *captured.borrow_mut() = Some(logical_snapshot(&other));
        });
        let error = refused(MemoryStore::open_with_context(
            path.to_str().unwrap(),
            &context(profile, MigrationAuthority::Deny),
        ));
        assert_eq!(missing(error), vec!["table:derived_items"]);
        let after = logical_snapshot(&Connection::open(&path).unwrap());
        assert_eq!(post_hook.borrow().as_ref().unwrap(), &after);
        assert_eq!(artifacts, migration_artifacts(&path));
    }
}

#[test]
fn missing_idless_index_with_duplicates_keeps_transactional_failure() {
    for profile in [StoreProfile::PortableKernel, StoreProfile::TachiFull] {
        let (_dir, path) = fixture(profile);
        let conn = Connection::open(&path).unwrap();
        crate::db::configure_connection(&conn).unwrap();
        crate::db::ensure_reserved_reference_write_guard(&conn).unwrap();
        conn.execute_batch(
            "DROP INDEX idx_memories_idless_identity_active;
            INSERT INTO memories(id,path,text,timestamp,idless_identity) VALUES
            ('one','/fixture/one','first','2026-10-05T00:00:00Z','same'),
            ('two','/fixture/two','second','2026-10-05T00:00:00Z','same');",
        )
        .unwrap();
        let before = logical_snapshot(&conn);
        drop(conn);
        let error = refused(MemoryStore::open_with_context(
            path.to_str().unwrap(),
            &context(profile, MigrationAuthority::Deny),
        ));
        assert!(matches!(error, MemoryError::Sqlite(_)), "{error:?}");
        assert!(error
            .to_string()
            .contains("UNIQUE constraint failed: memories.idless_identity"));
        assert_eq!(before, logical_snapshot(&Connection::open(path).unwrap()));
    }
}

#[test]
fn optional_provisioning_failure_does_not_reject_current_schema() {
    use rusqlite::hooks::{AuthAction, AuthContext, Authorization};
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };
    for profile in [StoreProfile::PortableKernel, StoreProfile::TachiFull] {
        let (_dir, path) = fixture(profile);
        let mut conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "DROP INDEX IF EXISTS idx_memories_path_active_ts; DROP TABLE IF EXISTS memories_vec;",
        )
        .unwrap();
        crate::db::configure_connection(&conn).unwrap();
        crate::db::ensure_reserved_reference_write_guard(&conn).unwrap();
        let denied = Arc::new(AtomicUsize::new(0));
        let count = Arc::clone(&denied);
        // This is an owned, unguarded fixture connection. Deny only the two
        // optional installers, leaving required DDL and input reads intact.
        conn.authorizer(Some(move |ctx: AuthContext<'_>| match ctx.action {
            AuthAction::CreateIndex {
                index_name: "idx_memories_path_active_ts",
                ..
            }
            | AuthAction::CreateVtable {
                table_name: "memories_vec",
                ..
            } => {
                count.fetch_add(1, Ordering::SeqCst);
                Authorization::Deny
            }
            _ => Authorization::Allow,
        }))
        .expect("install owned optional-provisioning failure authorizer");
        crate::db::init_schema_with_label_mut(
            &mut conn,
            "global",
            &path,
            &context(profile, MigrationAuthority::Deny),
        )
        .unwrap();
        assert!(!crate::db::try_load_sqlite_vec(&conn));
        assert!(
            denied.load(Ordering::SeqCst) >= 2,
            "both installers must run"
        );
        for name in ["idx_memories_path_active_ts", "memories_vec"] {
            let present: bool = conn
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name=?1)",
                    [name],
                    |r| r.get(0),
                )
                .unwrap();
            assert!(
                !present,
                "failed optional provisioner must leave {name} absent"
            );
        }
        crate::db::validate_current_schema_presence(&conn, &path, profile.into()).unwrap();
    }
}

#[test]
fn migration_only_stamped_additive_hole_is_refused_without_repair() {
    // Reconstruct the observable pre-38ed99d47 hole: this inline Product table
    // was never installed by the standalone sentinel migration API.
    let (_dir, path) = fixture(StoreProfile::TachiFull);
    let mut conn = Connection::open(&path).unwrap();
    conn.execute_batch("DROP TABLE exec_env_worktree_identities; PRAGMA user_version=38;")
        .unwrap();
    crate::db::migrations::run_data_migrations(&mut conn, "global", &path).unwrap();
    assert_eq!(
        crate::db::migrations::read_schema_version(&conn).unwrap(),
        39
    );
    drop(conn);
    assert_refused_without_repair(
        &path,
        MigrationAuthority::Deny,
        "exec_env_worktree_identities",
    );
}

#[test]
fn fresh_identity_bound_reopen_refuses_post_commit_damage() {
    for profile in [StoreProfile::PortableKernel, StoreProfile::TachiFull] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tachi-memory.db");
        let ran = Rc::new(RefCell::new(false));
        let captured = Rc::clone(&ran);
        crate::db::schema_test_hooks::arm_window_hook(
            crate::db::schema_test_hooks::Window::AfterSchemaCommit,
            move |path| {
                Connection::open(path)
                    .unwrap()
                    .execute_batch("DROP TABLE derived_items")
                    .unwrap();
                *captured.borrow_mut() = true;
            },
        );
        let error = refused(MemoryStore::open_with_context(
            path.to_str().unwrap(),
            &DbOpenContext::create_fresh().with_profile(profile),
        ));
        assert!(*ran.borrow());
        assert_eq!(missing(error), vec!["table:derived_items"]);
        let present: bool = Connection::open(path)
            .unwrap()
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name='derived_items')",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(!present);
    }
}
