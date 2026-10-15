use super::*;
use memcore::MemoryStore;
use std::path::Path;

fn register(server: &crate::MemoryServer, db: &Path, alias: &str) {
    crate::project_db_ops::register_repo_local_manifest_entry_in_home(
        db,
        alias,
        &server.tachi_home_dir(),
    )
    .expect("register fixture project");
}

fn stamp_bytes(db: &Path) -> String {
    let connection = crate::physical_db_identity::open_read_only_connection(db).unwrap();
    connection
        .query_row(
            "SELECT value_json FROM hard_state WHERE namespace='store_identity' AND key='role'",
            [],
            |row| row.get(0),
        )
        .unwrap()
}

#[tokio::test]
async fn project_store_identity_keeps_old_stamp_across_binding_read_write_and_recall() {
    // Reproduce Hyperion: a gen-3-stamped repo DB, current manifest, and
    // sessions arriving through both old and current aliases. No real DBs.
    for prefixed in [false, true] {
        let server = make_server();
        let fixture = crate::test_support::non_skipped_fixture_tempdir("project-identity-");
        let root = fixture.path().join("Quant_Analyzer_2026");
        std::fs::create_dir_all(root.join(".git")).unwrap();
        let db = root.join(".tachi").join(memcore::MEMORY_DB_FILENAME);
        std::fs::create_dir_all(db.parent().unwrap()).unwrap();
        let current = crate::path_utils::plan_c_dir_name_from_root(&root).unwrap();
        let previous = crate::path_utils::plan_c_previous_dir_name_from_root(&root).unwrap();
        let role = if prefixed {
            format!("project:{previous}")
        } else {
            previous.clone()
        };
        {
            let mut store = MemoryStore::open_with_label(db.to_str().unwrap(), &role).unwrap();
            let mut entry = make_entry("durable-project-identity-needle");
            entry.path = "/scratch/tachi/identity-regression".to_string();
            entry.summary = "DurableProjectIdentityNeedle project recall".to_string();
            entry.text =
                "DurableProjectIdentityNeedle survives alias generation upgrades".to_string();
            entry.keywords = vec!["DurableProjectIdentityNeedle".to_string()];
            store.upsert(&entry).unwrap();
        }
        register(&server, &db, &current);
        let before = stamp_bytes(&db);
        let inode_before = std::fs::canonicalize(&db).unwrap();
        let health = crate::status_ops::collect_snapshot_scoped(
            &server.tachi_home_dir(),
            &server.global_db_path_buf(),
            Some(&db),
            false,
        );
        let project_health = health
            .dbs
            .iter()
            .find(|entry| std::fs::canonicalize(&entry.path).ok().as_ref() == Some(&inode_before))
            .unwrap();
        assert!(project_health.error.is_none(), "{:?}", project_health.error);
        for incoming in [&previous, &current] {
            let (bound, resolved) = server
                .resolve_server_named_project_binding(incoming)
                .unwrap();
            assert_eq!(
                bound, current,
                "session alias normalization stays compatible"
            );
            assert_eq!(std::fs::canonicalize(resolved).unwrap(), inode_before);
            server.set_session_identity(None, Some(bound.clone()), None);
            for checked in [false, true] {
                let read = |store: &mut MemoryStore| {
                    assert_eq!(store.db_label(), role);
                    assert!(store
                        .get("durable-project-identity-needle")
                        .unwrap()
                        .is_some());
                    Ok(())
                };
                if checked {
                    server.with_named_project_store_read_identity_checked(&bound, read)
                } else {
                    server.with_named_project_store_read(&bound, read)
                }
                .unwrap();
                let write = |store: &mut MemoryStore| {
                    assert_eq!(store.db_label(), role);
                    let mut entry = make_entry("identity-write-roundtrip");
                    entry.text = "Independent write boundary probe retains original project role"
                        .to_string();
                    entry.path = "/scratch/tachi/identity-write-roundtrip".to_string();
                    store.upsert(&entry).map_err(|error| error.to_string())
                };
                if checked {
                    server.with_named_project_store_identity_checked(&bound, write)
                } else {
                    server.with_named_project_store(&bound, write)
                }
                .unwrap();
            }
            let mut params = tachi_memory_params("briefing");
            params.format = Some("json".to_string());
            params.project = Some(bound.clone());
            params.query = Some("DurableProjectIdentityNeedle".to_string());
            params.compact = true;
            let body = crate::facade_memory_ops::handle_tachi_memory(&server, params)
                .await
                .unwrap();
            let result: Value = serde_json::from_str(&body).unwrap();
            assert!(
                result["memories"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|row| row["id"] == "durable-project-identity-needle"),
                "{result}"
            );
            for resources in [false, true] {
                let mut params = tachi_memory_params("search");
                params.format = Some("json".to_string());
                params.project = Some(bound.clone());
                params.scope = Some("memory".to_string());
                params.query = Some("DurableProjectIdentityNeedle".to_string());
                let body = if resources {
                    crate::facade_memory_ops::handle_tachi_memory_with_resources(
                        &server, params, None,
                    )
                    .await
                    .unwrap()
                    .0
                } else {
                    crate::facade_memory_ops::handle_tachi_memory(&server, params)
                        .await
                        .unwrap()
                };
                let result: Value = serde_json::from_str(&body).unwrap();
                let memory = result["sections"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|section| section["name"] == "Memory")
                    .unwrap();
                assert!(memory.get("error").is_none_or(Value::is_null), "{result}");
                assert!(
                    memory["rows"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|row| row["id"] == "durable-project-identity-needle"),
                    "{result}"
                );
            }
        }
        assert_eq!(stamp_bytes(&db), before, "identity must never be rewritten");
        server
            .with_named_project_store_read(&current, |store| {
                assert!(store.get("identity-write-roundtrip").unwrap().is_some());
                Ok(())
            })
            .unwrap();
    }
}

#[test]
fn project_store_identity_refuses_foreign_special_and_same_basename_roles() {
    for role in [
        "wiki",
        "global",
        "OtherRepo-12345678",
        "Quant_Analyzer_2026-deadbeef",
    ] {
        let server = make_server();
        let fixture = crate::test_support::non_skipped_fixture_tempdir("project-identity-");
        let root = fixture.path().join("Quant_Analyzer_2026");
        std::fs::create_dir_all(root.join(".git")).unwrap();
        let db = root.join(".tachi").join(memcore::MEMORY_DB_FILENAME);
        std::fs::create_dir_all(db.parent().unwrap()).unwrap();
        drop(MemoryStore::open_with_label(db.to_str().unwrap(), role).unwrap());
        let current = crate::path_utils::plan_c_dir_name_from_root(&root).unwrap();
        register(&server, &db, &current);
        let before = stamp_bytes(&db);
        let error = server
            .with_named_project_store_read::<()>(&current, |_| {
                panic!("foreign role must never reach a reader")
            })
            .unwrap_err();
        assert!(error.contains("store role conflict"), "{error}");
        let error = server
            .with_named_project_store::<()>(&current, |_| {
                panic!("foreign role must never reach a writer")
            })
            .unwrap_err();
        assert!(error.contains("store role conflict"), "{error}");
        assert_eq!(stamp_bytes(&db), before);
    }
}

#[cfg(unix)]
#[test]
fn project_store_identity_does_not_absorb_a_regular_standalone_store() {
    let fixture = crate::test_support::non_skipped_fixture_tempdir("project-identity-");
    let root = fixture.path().join("EnclosingRepo");
    std::fs::create_dir_all(root.join(".git")).unwrap();
    let home = root.join("data/tachi");
    let global = home.join("global").join(memcore::MEMORY_DB_FILENAME);
    std::fs::create_dir_all(global.parent().unwrap()).unwrap();
    drop(MemoryStore::open_with_label(global.to_str().unwrap(), "global").unwrap());
    let server = crate::MemoryServer::new_with_home_for_test(global, None, home.clone()).unwrap();
    let current = crate::path_utils::plan_c_dir_name_from_root(&root).unwrap();
    let old = crate::path_utils::plan_c_previous_dir_name_from_root(&root).unwrap();
    let db = home
        .join("projects")
        .join(&current)
        .join(memcore::MEMORY_DB_FILENAME);
    std::fs::create_dir_all(db.parent().unwrap()).unwrap();
    drop(MemoryStore::open_with_label(db.to_str().unwrap(), &old).unwrap());
    let alias = home
        .join("projects")
        .join(&old)
        .join(memcore::MEMORY_DB_FILENAME);
    std::fs::create_dir_all(alias.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(&db, &alias).unwrap();
    let before = stamp_bytes(&db);
    let (session, _) = server
        .resolve_server_named_project_binding(&current)
        .unwrap();
    assert_eq!(session, current);
    let error = server
        .with_named_project_store_read::<()>(&current, |_| {
            panic!("a regular standalone store cannot borrow its enclosing repository's identity")
        })
        .unwrap_err();
    assert!(error.contains("store role conflict"), "{error}");
    server
        .with_named_project_store_read(&old, |store| {
            assert_eq!(store.db_label(), old);
            Ok(())
        })
        .unwrap();
    assert_eq!(stamp_bytes(&db), before);
}
