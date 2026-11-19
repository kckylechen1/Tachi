use super::*;
use serde_json::json;

const REM_WIKI_PRODUCER_VERSION: &str = "weekly-wiki-evolver-v1";

fn stable_rem_draft_id(sources: &[RemSourceRef]) -> String {
    stable_rem_draft_id_for_version(sources, REM_WIKI_PRODUCER_VERSION)
}

// Construct historical pending state; production recovery never creates it.
fn seed_pending_rem_operation(
    server: &MemoryServer,
    entry: &MemoryEntry,
    sources: &[RemSourceRef],
) -> Result<(), String> {
    server.with_named_project_store_identity_checked("wiki", |store| {
        store
            .with_immutable_supersession_transaction(|operation| {
                let claimed_at = Utc::now().to_rfc3339();
                for source in sources {
                    let (source_key, source_identity) = stable_rem_source_claim(source);
                    operation.claim_rem_source(
                        &source_key,
                        &source_identity,
                        &entry.id,
                        &claimed_at,
                    )?;
                }
                operation.insert_rem_operation_if_absent(entry)?;
                Ok(())
            })
            .map_err(|error| format!("seed pending REM operation: {error}"))
    })
}

fn test_store(identity: &str) -> RemSourceStore {
    RemSourceStore {
        identity: identity.to_string(),
    }
}

fn occupied_entry(id: &str, sources: &[RemSourceRef]) -> MemoryEntry {
    MemoryEntry {
        id: id.to_string(),
        path: "/wiki/drafts/test-identity".to_string(),
        summary: "REM identity".to_string(),
        text: "REM identity fixture".to_string(),
        importance: 0.65,
        timestamp: "2026-07-31T00:00:00Z".to_string(),
        valid_from: "2026-07-31T00:00:00Z".to_string(),
        valid_until: None,
        category: "experience".to_string(),
        topic: "identity".to_string(),
        keywords: Vec::new(),
        persons: Vec::new(),
        entities: Vec::new(),
        location: String::new(),
        source: "wiki".to_string(),
        scope: "global".to_string(),
        archived: false,
        access_count: 0,
        scored_count: 0,
        last_access: None,
        last_use_at: None,
        revision: 1,
        metadata: json!({
            "artifact_kind": "draft",
            "review_status": "pending",
            "provenance": {
                "model_invocation": {"schema": tachi_llm::MODEL_INVOCATION_SCHEMA_V1}
            },
            "rem": {
                "producer": "weekly_wiki_evolver",
                "source_set_contract": REM_WIKI_SOURCE_SET_CONTRACT,
                "producer_version": REM_WIKI_PRODUCER_VERSION,
                "operation_id": id,
                "operation_status": "pending_sources",
                "sources": sources,
            }
        }),
        vector: None,
        retention_policy: Some("durable".to_string()),
        domain: Some("wiki".to_string()),
        recall_count: 0,
        query_diversity: 0,
        tier: "raw".to_string(),
    }
}

fn source_entry(id: &str) -> MemoryEntry {
    let mut entry = occupied_entry(id, &[]);
    entry.path = format!("/patterns/{id}");
    entry.source = "test".to_string();
    entry.topic = id.to_string();
    entry.metadata = json!({});
    entry.retention_policy = None;
    entry.domain = None;
    entry.tier = "pattern".to_string();
    entry
}

#[test]
fn rem_identity_is_sorted_and_store_aware() {
    let sources = vec![
        RemSourceRef {
            store: test_store("project-db"),
            id: "same".to_string(),
            revision: 1,
        },
        RemSourceRef {
            store: test_store("global-db"),
            id: "same".to_string(),
            revision: 1,
        },
    ];
    let mut reversed = sources.clone();
    reversed.reverse();
    reversed.sort();
    let mut sorted = sources.clone();
    sorted.sort();
    assert_eq!(stable_rem_draft_id(&sorted), stable_rem_draft_id(&reversed));
    assert_ne!(
        stable_rem_draft_id(&sorted),
        stable_rem_draft_id(&[RemSourceRef {
            store: test_store("project-db"),
            id: "same".to_string(),
            revision: 1,
        }])
    );
    let mut newer_revision = sorted.clone();
    newer_revision[0].revision += 1;
    assert_ne!(
        stable_rem_draft_id(&sorted),
        stable_rem_draft_id(&newer_revision),
        "a changed source revision must produce a different draft operation"
    );
}

#[cfg(unix)]
#[test]
fn rem_source_store_identity_collapses_hardlink_aliases() {
    let dir = tempfile::tempdir().unwrap();
    let first = dir.path().join("project-a.db");
    let second = dir.path().join("project-b.db");
    std::fs::write(&first, b"REM physical identity fixture").unwrap();
    std::fs::hard_link(&first, &second).unwrap();

    let runtime_stores = rem_source_store_identities(&first, Some(&second)).unwrap();
    let first_store = runtime_stores.global;
    let second_store = runtime_stores.project.expect("project alias identity");
    assert_eq!(first_store, second_store);

    let first_id = stable_rem_draft_id(&[RemSourceRef {
        store: first_store,
        id: "same-source".to_string(),
        revision: 1,
    }]);
    let second_id = stable_rem_draft_id(&[RemSourceRef {
        store: second_store,
        id: "same-source".to_string(),
        revision: 1,
    }]);
    assert_eq!(first_id, second_id);
}

#[cfg(unix)]
#[test]
fn rem_source_store_identity_rejects_distinct_live_wal_aliases() {
    let dir = tempfile::tempdir().unwrap();
    let first = dir.path().join("global.db");
    let second = dir.path().join("project.db");
    std::fs::write(&first, b"REM physical identity fixture").unwrap();
    std::fs::hard_link(&first, &second).unwrap();
    std::fs::write(format!("{}-wal", first.display()), b"global WAL").unwrap();
    std::fs::write(format!("{}-wal", second.display()), b"project WAL").unwrap();

    let error = rem_source_store_identities(&first, Some(&second))
        .expect_err("dual live WAL aliases must fail closed");
    assert!(error.contains("multiple live WAL/SHM owners"), "{error}");
}

#[cfg(unix)]
#[test]
fn rem_source_store_identity_routes_shared_store_through_live_project_alias() {
    let dir = tempfile::tempdir().unwrap();
    let global = dir.path().join("global.db");
    let project = dir.path().join("project.db");
    std::fs::write(&global, b"REM physical identity fixture").unwrap();
    std::fs::hard_link(&global, &project).unwrap();
    std::fs::write(format!("{}-wal", project.display()), b"project WAL").unwrap();

    let runtime_stores = rem_source_store_identities(&global, Some(&project)).unwrap();
    assert_eq!(
        runtime_stores.project.as_ref(),
        Some(&runtime_stores.global)
    );
    assert_eq!(runtime_stores.shared_route, RemSourceRole::Project);

    let sources = vec![RemSourceRef {
        store: runtime_stores.global.clone(),
        id: "source".to_string(),
        revision: 7,
    }];
    let (global_sources, project_sources) =
        route_rem_sources(&sources, &runtime_stores, "draft").unwrap();
    assert!(global_sources.is_empty());
    assert_eq!(project_sources, vec![("source".to_string(), 7)]);
}

#[cfg(unix)]
#[test]
fn runtime_rem_source_stores_rejects_a_path_replaced_after_open() {
    let dir = tempfile::tempdir().unwrap();
    let global = dir.path().join("global.db");
    let replacement = dir.path().join("replacement.db");
    let server = MemoryServer::new(global.clone(), None).expect("open runtime server");
    std::fs::write(&replacement, b"replacement physical database identity")
        .expect("create replacement inode");
    std::fs::rename(&replacement, &global).expect("replace configured database path");

    let error = runtime_rem_source_stores(&server)
        .expect_err("a long-lived connection must not be rebound by path alone");
    assert!(error.contains("identity changed after open"), "{error}");
}

#[cfg(unix)]
#[test]
fn rem_recovery_rejects_a_named_wiki_path_replaced_after_open() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    let wiki_db = home.join("projects/wiki").join(memcore::MEMORY_DB_FILENAME);
    std::fs::create_dir_all(wiki_db.parent().unwrap()).expect("create Wiki store directory");
    drop(
        memcore::MemoryStore::open(wiki_db.to_str().unwrap()).expect("initialize named Wiki store"),
    );
    let server = MemoryServer::new_with_home_for_test(home.join("global.db"), None, home)
        .expect("open runtime server");
    server
        .with_named_project_store("wiki", |_| Ok(()))
        .expect("cache named Wiki store");

    let replacement = dir.path().join("replacement.db");
    std::fs::write(&replacement, b"replacement Wiki database identity")
        .expect("create replacement inode");
    std::fs::rename(&replacement, &wiki_db).expect("replace named Wiki database path");
    let error = recover_pending_rem_operations(&server)
        .expect_err("REM recovery must not read a detached Wiki connection");
    assert!(error.contains("physical identity check failed"), "{error}");
}

#[test]
fn recovery_fails_loudly_for_a_pending_operation_without_sources() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    let wiki_db = home.join("projects/wiki").join(memcore::MEMORY_DB_FILENAME);
    std::fs::create_dir_all(wiki_db.parent().unwrap()).expect("create Wiki store directory");
    drop(
        memcore::MemoryStore::open(wiki_db.to_str().unwrap()).expect("initialize named Wiki store"),
    );
    let server = MemoryServer::new_with_home_for_test(home.join("global.db"), None, home)
        .expect("open runtime server");
    let draft_id = stable_rem_draft_id(&[]);
    seed_pending_rem_operation(&server, &occupied_entry(&draft_id, &[]), &[])
        .expect("seed malformed legacy pending operation");

    let error = recover_pending_rem_operations(&server)
        .expect_err("missing REM sources must not look recovered");
    assert!(error.contains(&draft_id), "{error}");
    assert!(error.contains("source set"), "{error}");
}

#[test]
fn recovery_allows_first_run_without_wiki_db_when_no_source_markers_exist() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    let server = MemoryServer::new_with_home_for_test(home.join("global.db"), None, home)
        .expect("open runtime server");

    let recovery =
        recover_pending_rem_operations(&server).expect("first run has no ledger to recover");
    assert_eq!(recovery, RemRecoveryReport::default());
}

#[test]
fn recovery_fails_loudly_when_wiki_db_is_missing_after_source_marker_commit() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    let server = MemoryServer::new_with_home_for_test(home.join("global.db"), None, home)
        .expect("open runtime server");
    server
        .with_global_store(|store| {
            store
                .upsert(&source_entry("orphaned-source"))
                .map_err(|error| error.to_string())
        })
        .expect("seed source");
    let revision = server
        .with_global_store_read(|store| {
            store
                .get("orphaned-source")
                .map_err(|error| error.to_string())?
                .map(|entry| entry.revision)
                .ok_or_else(|| "source missing".to_string())
        })
        .expect("read source revision");
    server
        .with_global_store(|store| {
            store
                .mark_rem_processed_for_draft_at_revisions(
                    &[("orphaned-source".to_string(), revision)],
                    "2026-07-31T00:00:01Z",
                    "wiki-rem:orphaned",
                )
                .map_err(|error| error.to_string())
        })
        .expect("simulate source marker committed before Wiki ledger was available");

    let error = recover_pending_rem_operations(&server)
        .expect_err("missing Wiki coordination DB must not report recovered/no work");
    assert!(
        error.contains("Wiki operation store is unavailable"),
        "{error}"
    );
    assert!(error.contains("source markers"), "{error}");
}

#[test]
fn recovery_fails_loudly_when_valid_wiki_db_lost_the_marker_ledger() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    let wiki_db = home.join("projects/wiki").join(memcore::MEMORY_DB_FILENAME);
    std::fs::create_dir_all(wiki_db.parent().unwrap()).expect("create Wiki store directory");
    drop(
        memcore::MemoryStore::open(wiki_db.to_str().unwrap())
            .expect("initialize replacement Wiki store"),
    );
    let server = MemoryServer::new_with_home_for_test(home.join("global.db"), None, home)
        .expect("open runtime server");
    server
        .with_global_store(|store| {
            store
                .upsert(&source_entry("orphaned-source"))
                .map_err(|error| error.to_string())
        })
        .expect("seed source");
    let revision = server
        .with_global_store_read(|store| {
            store
                .get("orphaned-source")
                .map_err(|error| error.to_string())?
                .map(|entry| entry.revision)
                .ok_or_else(|| "source missing".to_string())
        })
        .expect("read source revision");
    server
        .with_global_store(|store| {
            store
                .mark_rem_processed_for_draft_at_revisions(
                    &[("orphaned-source".to_string(), revision)],
                    "2026-07-31T00:00:01Z",
                    "wiki-rem:missing-ledger",
                )
                .map_err(|error| error.to_string())
        })
        .expect("simulate source marker with replaced Wiki ledger");

    let error = recover_pending_rem_operations(&server)
        .expect_err("valid but replaced Wiki DB must not hide orphaned markers");
    assert!(error.contains("source marker orphaned-source"), "{error}");
    assert!(error.contains("wiki-rem:missing-ledger"), "{error}");
    assert!(error.contains("missing Wiki operation ledger"), "{error}");
}

#[test]
fn recovery_rejects_completed_marker_not_vouched_for_its_current_revision() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    let wiki_db = home.join("projects/wiki").join(memcore::MEMORY_DB_FILENAME);
    std::fs::create_dir_all(wiki_db.parent().unwrap()).expect("create Wiki store directory");
    drop(memcore::MemoryStore::open(wiki_db.to_str().unwrap()).expect("initialize Wiki store"));
    let server = MemoryServer::new_with_home_for_test(home.join("global.db"), None, home)
        .expect("open runtime server");
    server
        .with_global_store(|store| {
            store
                .upsert(&source_entry("source"))
                .map_err(|error| error.to_string())
        })
        .expect("seed source");
    let first_revision = server
        .with_global_store_read(|store| {
            store
                .get("source")
                .map_err(|error| error.to_string())?
                .map(|entry| entry.revision)
                .ok_or_else(|| "source missing".to_string())
        })
        .expect("read first revision");
    let runtime = runtime_rem_source_stores(&server).expect("bind runtime source store");
    let sources = vec![RemSourceRef {
        store: runtime.global,
        id: "source".to_string(),
        revision: first_revision,
    }];
    let draft_id = stable_rem_draft_id(&sources);
    seed_pending_rem_operation(&server, &occupied_entry(&draft_id, &sources), &sources)
        .expect("persist completed operation ledger");
    server
        .with_global_store(|store| {
            store
                .mark_rem_processed_for_draft_at_revisions(
                    &[("source".to_string(), first_revision)],
                    "2026-07-31T00:00:01Z",
                    &draft_id,
                )
                .map_err(|error| error.to_string())
        })
        .expect("mark first source revision");
    server
        .with_named_project_store("wiki", |store| {
            store
                .complete_rem_wiki_operation(&draft_id, "2026-07-31T00:00:02Z")
                .map(|_| ())
                .map_err(|error| error.to_string())
        })
        .expect("complete first operation");
    server
        .with_global_store(|store| {
            let mut source = store
                .get("source")
                .map_err(|error| error.to_string())?
                .ok_or_else(|| "source missing".to_string())?;
            source.text = "source changed after completed REM operation".to_string();
            store.upsert(&source).map_err(|error| error.to_string())
        })
        .expect("advance source revision");
    let current_revision = server
        .with_global_store_read(|store| {
            store
                .get("source")
                .map_err(|error| error.to_string())?
                .map(|entry| entry.revision)
                .ok_or_else(|| "source missing".to_string())
        })
        .expect("read current revision");
    server
        .with_global_store(|store| {
            store
                .mark_rem_processed_for_draft_at_revisions(
                    &[("source".to_string(), current_revision)],
                    "2026-07-31T00:00:03Z",
                    &draft_id,
                )
                .map_err(|error| error.to_string())
        })
        .expect("shape current marker with old operation owner");

    let error = recover_pending_rem_operations(&server)
        .expect_err("old completed ledger must not vouch for current source revision");
    assert!(
        error.contains(&format!("source marker source revision {current_revision}")),
        "{error}"
    );
    assert!(error.contains(&draft_id), "{error}");
    assert!(
        error.contains("absent from Wiki operation ledger"),
        "{error}"
    );
}

#[test]
fn wiki_completion_failure_compensates_source_markers_and_keeps_pending_operation() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    let wiki_db = home.join("projects/wiki").join(memcore::MEMORY_DB_FILENAME);
    std::fs::create_dir_all(wiki_db.parent().unwrap()).expect("create Wiki store directory");
    drop(
        memcore::MemoryStore::open(wiki_db.to_str().unwrap()).expect("initialize named Wiki store"),
    );
    let server = MemoryServer::new_with_home_for_test(home.join("global.db"), None, home)
        .expect("open runtime server");
    server
        .with_global_store(|store| {
            store
                .upsert(&source_entry("global-source"))
                .map_err(|error| error.to_string())
        })
        .expect("seed global source");
    let runtime = runtime_rem_source_stores(&server).expect("bind runtime source stores");
    let revision = server
        .with_global_store_read(|store| {
            store
                .get("global-source")
                .map_err(|error| error.to_string())?
                .map(|entry| entry.revision)
                .ok_or_else(|| "global source missing".to_string())
        })
        .expect("read global source revision");
    let sources = vec![RemSourceRef {
        store: runtime.global,
        id: "global-source".to_string(),
        revision,
    }];
    let draft_id = stable_rem_draft_id(&sources);
    seed_pending_rem_operation(&server, &occupied_entry(&draft_id, &sources), &sources)
        .expect("persist pending REM operation");

    *FAIL_REM_WIKI_COMPLETION_ONCE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(draft_id.clone());
    let error = complete_rem_operation(&server, &draft_id, &sources)
        .expect_err("Wiki completion failure must compensate source markers");
    assert!(
        error.contains("injected REM Wiki completion failure"),
        "{error}"
    );
    assert!(error.contains("markers were compensated"), "{error}");

    let source = server
        .with_global_store_read(|store| {
            store
                .get("global-source")
                .map_err(|error| error.to_string())?
                .ok_or_else(|| "global source missing".to_string())
        })
        .expect("read compensated source");
    assert!(source.metadata["rem"]["processed"].is_null());
    let pending = server
        .with_named_project_store_read("wiki", |store| {
            store
                .get(&draft_id)
                .map_err(|error| error.to_string())?
                .ok_or_else(|| "REM operation missing".to_string())
        })
        .expect("read pending operation");
    assert_eq!(
        pending.metadata["rem"]["operation_status"],
        "pending_sources"
    );

    // Fresh, substantial patterns must remain ordinary source memories while
    // maintenance finishes the historical operation already in the ledger.
    server
        .with_global_store(|store| {
            for id in ["fresh-pattern-a", "fresh-pattern-b"] {
                let mut entry = source_entry(id);
                entry.topic = "fresh-patterns".to_string();
                entry.text = "A newly recorded pattern with enough substantive text for the retired weekly synthesis producer.".to_string();
                store.upsert(&entry).map_err(|error| error.to_string())?;
            }
            Ok(())
        })
        .expect("seed fresh patterns alongside the pending operation");

    let recovery = recover_pending_rem_operations(&server)
        .expect("recovery must finish the compensated pending operation");
    assert_eq!(recovery.completed, 1);
    assert_eq!(recovery.aborted_stale, 0);
    assert_eq!(recovery.foreign_pending_skipped, 0);
    server
        .with_named_project_store_read("wiki", |store| {
            let draft_count: i64 = store
                .connection()
                .query_row(
                    "SELECT COUNT(*) FROM memories WHERE path LIKE '/wiki/drafts/%'",
                    [],
                    |row| row.get(0),
                )
                .map_err(|error| error.to_string())?;
            assert_eq!(draft_count, 1, "recovery must not produce another draft");
            let recovered = store
                .get(&draft_id)
                .map_err(|error| error.to_string())?
                .expect("historical draft remains readable");
            assert_eq!(recovered.text, pending.text);
            assert_eq!(recovered.metadata["review_status"], "pending");
            Ok(())
        })
        .expect("read unchanged historical draft after recovery");
    server
        .with_global_store_read(|store| {
            for id in ["fresh-pattern-a", "fresh-pattern-b"] {
                let entry = store
                    .get(id)
                    .map_err(|error| error.to_string())?
                    .expect("fresh pattern remains readable");
                assert!(entry.metadata.get("rem").is_none());
                assert_eq!(entry.topic, "fresh-patterns");
            }
            Ok(())
        })
        .expect("fresh patterns remain outside REM processing");
    let operation_log = server
        .with_named_project_store_read("wiki", |store| {
            store
                .get("wiki-operation-log")
                .map_err(|error| error.to_string())?
                .ok_or_else(|| "Wiki operation log missing after recovery".to_string())
        })
        .expect("read recovered REM operation log");
    assert!(
        operation_log
            .text
            .contains("recovered weekly REM draft completion"),
        "recovery completion must remain visible in the Wiki operation log"
    );
}

#[test]
fn rem_completion_refuses_a_missing_source_claim_before_marking_sources() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    let wiki_db = home.join("projects/wiki").join(memcore::MEMORY_DB_FILENAME);
    std::fs::create_dir_all(wiki_db.parent().unwrap()).expect("create Wiki store directory");
    drop(memcore::MemoryStore::open(wiki_db.to_str().unwrap()).expect("initialize Wiki store"));
    let server = MemoryServer::new_with_home_for_test(home.join("global.db"), None, home)
        .expect("open runtime server");
    server
        .with_global_store(|store| {
            store
                .upsert(&source_entry("claimed-source"))
                .map_err(|error| error.to_string())
        })
        .expect("seed source");
    let runtime = runtime_rem_source_stores(&server).expect("bind runtime stores");
    let revision = server
        .with_global_store_read(|store| {
            store
                .get("claimed-source")
                .map_err(|error| error.to_string())?
                .map(|entry| entry.revision)
                .ok_or_else(|| "source missing".to_string())
        })
        .expect("read source revision");
    let sources = vec![RemSourceRef {
        store: runtime.global,
        id: "claimed-source".to_string(),
        revision,
    }];
    let draft_id = stable_rem_draft_id(&sources);
    seed_pending_rem_operation(&server, &occupied_entry(&draft_id, &sources), &sources)
        .expect("persist REM operation");
    server
        .with_named_project_store("wiki", |store| {
            store
                .connection()
                .execute(
                    "DELETE FROM rem_source_claims WHERE draft_id = ?1",
                    [&draft_id],
                )
                .map_err(|error| error.to_string())?;
            Ok(())
        })
        .expect("simulate incomplete legacy claim ledger");

    let error = complete_rem_operation(&server, &draft_id, &sources)
        .expect_err("missing claim must fail before source marker writes");
    assert!(error.contains("source claim ledger mismatch"), "{error}");
    let source = server
        .with_global_store_read(|store| {
            store
                .get("claimed-source")
                .map_err(|error| error.to_string())?
                .ok_or_else(|| "source missing".to_string())
        })
        .expect("read unmarked source");
    assert!(source.metadata["rem"]["processed"].is_null());
}

#[test]
fn project_marker_failure_compensates_the_committed_global_marker() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    let wiki_db = home.join("projects/wiki").join(memcore::MEMORY_DB_FILENAME);
    std::fs::create_dir_all(wiki_db.parent().unwrap()).expect("create Wiki store directory");
    drop(
        memcore::MemoryStore::open(wiki_db.to_str().unwrap()).expect("initialize named Wiki store"),
    );
    let global_db = home.join("global.db");
    let project_db = home.join("project.db");
    let server = MemoryServer::new_with_home_for_test(global_db, Some(project_db), home.clone())
        .expect("open runtime server");
    server
        .with_global_store(|store| {
            store
                .upsert(&source_entry("global-source"))
                .map_err(|error| error.to_string())
        })
        .expect("seed global source");
    server
        .with_project_store(|store| {
            store
                .upsert(&source_entry("project-source"))
                .map_err(|error| error.to_string())
        })
        .expect("seed project source");

    let runtime = runtime_rem_source_stores(&server).expect("bind runtime source stores");
    let global_revision = server
        .with_global_store_read(|store| {
            store
                .get("global-source")
                .map_err(|error| error.to_string())?
                .map(|entry| entry.revision)
                .ok_or_else(|| "global source missing".to_string())
        })
        .expect("read global revision");
    let project_revision = server
        .with_project_store_read(|store| {
            store
                .get("project-source")
                .map_err(|error| error.to_string())?
                .map(|entry| entry.revision)
                .ok_or_else(|| "project source missing".to_string())
        })
        .expect("read project revision");
    let mut sources = vec![
        RemSourceRef {
            store: runtime.global,
            id: "global-source".to_string(),
            revision: global_revision,
        },
        RemSourceRef {
            store: runtime.project.expect("project store identity"),
            id: "project-source".to_string(),
            revision: project_revision,
        },
    ];
    sources.sort();
    let draft_id = stable_rem_draft_id(&sources);
    let draft = occupied_entry(&draft_id, &sources);
    seed_pending_rem_operation(&server, &draft, &sources).expect("persist REM operation");

    server
        .with_project_store(|store| {
            let mut changed = store
                .get("project-source")
                .map_err(|error| error.to_string())?
                .ok_or_else(|| "project source missing".to_string())?;
            changed.text = "project source changed after REM synthesis".to_string();
            store.upsert(&changed).map_err(|error| error.to_string())
        })
        .expect("change project source revision");

    let error = complete_rem_operation(&server, &draft_id, &sources)
        .expect_err("project revision failure must keep the operation pending");
    assert!(error.contains("source revision changed"), "{error}");
    assert!(error.contains("markers were compensated"), "{error}");
    let global = server
        .with_global_store_read(|store| {
            store
                .get("global-source")
                .map_err(|error| error.to_string())?
                .ok_or_else(|| "global source missing".to_string())
        })
        .expect("read compensated global source");
    assert!(global.metadata["rem"]["processed"].is_null());
    server
        .with_named_project_store("wiki", |store| {
            let pending = store
                .get(&draft_id)
                .map_err(|error| error.to_string())?
                .ok_or_else(|| "REM operation missing".to_string())?;
            assert_eq!(
                pending.metadata["rem"]["operation_status"],
                "pending_sources"
            );
            Ok(())
        })
        .expect("verify pending operation");

    let recovery = recover_pending_rem_operations(&server)
        .expect("second-run recovery must retire the stale operation");
    assert_eq!(recovery.completed, 0);
    assert_eq!(recovery.aborted_stale, 1);
    server
        .with_named_project_store("wiki", |store| {
            let aborted = store
                .get_with_options(&draft_id, true)
                .map_err(|error| error.to_string())?
                .ok_or_else(|| "aborted REM operation missing".to_string())?;
            assert!(aborted.archived);
            assert_eq!(
                aborted.metadata["rem"]["operation_status"],
                "aborted_stale_sources"
            );
            Ok(())
        })
        .expect("verify stale operation retirement");

    let next_project_revision = server
        .with_project_store_read(|store| {
            store
                .get("project-source")
                .map_err(|error| error.to_string())?
                .map(|entry| entry.revision)
                .ok_or_else(|| "project source missing".to_string())
        })
        .expect("read revised project source");
    let mut next_sources = vec![
        sources
            .iter()
            .find(|source| source.id == "global-source")
            .expect("global source identity")
            .clone(),
        RemSourceRef {
            store: sources
                .iter()
                .find(|source| source.id == "project-source")
                .expect("project source identity")
                .store
                .clone(),
            id: "project-source".to_string(),
            revision: next_project_revision,
        },
    ];
    next_sources.sort();
    let next_draft_id = stable_rem_draft_id(&next_sources);
    assert_ne!(next_draft_id, draft_id);
    seed_pending_rem_operation(
        &server,
        &occupied_entry(&next_draft_id, &next_sources),
        &next_sources,
    )
    .expect("released claims must admit the revised source-set operation");
}

#[test]
fn stale_recovery_compensates_crash_left_partial_marker_before_releasing_claims() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    let wiki_db = home.join("projects/wiki").join(memcore::MEMORY_DB_FILENAME);
    std::fs::create_dir_all(wiki_db.parent().unwrap()).expect("create Wiki store directory");
    drop(memcore::MemoryStore::open(wiki_db.to_str().unwrap()).expect("initialize Wiki store"));
    let global_db = home.join("global.db");
    let project_db = home.join("project.db");
    let server = MemoryServer::new_with_home_for_test(global_db, Some(project_db), home.clone())
        .expect("open runtime server");
    server
        .with_global_store(|store| {
            store
                .upsert(&source_entry("global-source"))
                .map_err(|error| error.to_string())
        })
        .expect("seed global source");
    server
        .with_project_store(|store| {
            store
                .upsert(&source_entry("project-source"))
                .map_err(|error| error.to_string())
        })
        .expect("seed project source");

    let runtime = runtime_rem_source_stores(&server).expect("bind runtime source stores");
    let global_revision = server
        .with_global_store_read(|store| {
            store
                .get("global-source")
                .map_err(|error| error.to_string())?
                .map(|entry| entry.revision)
                .ok_or_else(|| "global source missing".to_string())
        })
        .expect("read global revision");
    let project_revision = server
        .with_project_store_read(|store| {
            store
                .get("project-source")
                .map_err(|error| error.to_string())?
                .map(|entry| entry.revision)
                .ok_or_else(|| "project source missing".to_string())
        })
        .expect("read project revision");
    let mut sources = vec![
        RemSourceRef {
            store: runtime.global,
            id: "global-source".to_string(),
            revision: global_revision,
        },
        RemSourceRef {
            store: runtime.project.expect("project store identity"),
            id: "project-source".to_string(),
            revision: project_revision,
        },
    ];
    sources.sort();
    let draft_id = stable_rem_draft_id(&sources);
    seed_pending_rem_operation(&server, &occupied_entry(&draft_id, &sources), &sources)
        .expect("persist REM operation");
    server
        .with_global_store(|store| {
            store
                .mark_rem_processed_for_draft_at_revisions(
                    &[("global-source".to_string(), global_revision)],
                    "2026-07-31T00:00:01Z",
                    &draft_id,
                )
                .map_err(|error| error.to_string())
        })
        .expect("simulate crash after global marker commit");
    server
        .with_project_store(|store| {
            let mut changed = store
                .get("project-source")
                .map_err(|error| error.to_string())?
                .ok_or_else(|| "project source missing".to_string())?;
            changed.text = "project source changed after crash".to_string();
            store.upsert(&changed).map_err(|error| error.to_string())
        })
        .expect("change unmarked project source revision");

    let recovery = recover_pending_rem_operations(&server)
        .expect("stale recovery must compensate markers before aborting");
    assert_eq!(recovery.completed, 0);
    assert_eq!(recovery.aborted_stale, 1);
    assert_eq!(recovery.foreign_pending_skipped, 0);
    let global = server
        .with_global_store_read(|store| {
            store
                .get("global-source")
                .map_err(|error| error.to_string())?
                .ok_or_else(|| "global source missing".to_string())
        })
        .expect("read compensated global source");
    assert!(global.metadata["rem"]["processed"].is_null());
    server
        .with_named_project_store("wiki", |store| {
            let aborted = store
                .get_with_options(&draft_id, true)
                .map_err(|error| error.to_string())?
                .ok_or_else(|| "aborted REM operation missing".to_string())?;
            assert!(aborted.archived);
            assert_eq!(
                aborted.metadata["rem"]["operation_status"],
                "aborted_stale_sources"
            );
            Ok(())
        })
        .expect("verify stale operation retirement");

    let next_project_revision = server
        .with_project_store_read(|store| {
            store
                .get("project-source")
                .map_err(|error| error.to_string())?
                .map(|entry| entry.revision)
                .ok_or_else(|| "project source missing".to_string())
        })
        .expect("read revised project source");
    let mut next_sources = vec![
        sources
            .iter()
            .find(|source| source.id == "global-source")
            .expect("global source identity")
            .clone(),
        RemSourceRef {
            store: sources
                .iter()
                .find(|source| source.id == "project-source")
                .expect("project source identity")
                .store
                .clone(),
            id: "project-source".to_string(),
            revision: next_project_revision,
        },
    ];
    next_sources.sort();
    let next_draft_id = stable_rem_draft_id(&next_sources);
    assert_ne!(next_draft_id, draft_id);
    seed_pending_rem_operation(
        &server,
        &occupied_entry(&next_draft_id, &next_sources),
        &next_sources,
    )
    .expect("compensated marker and released claims must admit revised source-set operation");
}

#[test]
fn stale_recovery_treats_older_completed_marker_as_no_pending_marker() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    let wiki_db = home.join("projects/wiki").join(memcore::MEMORY_DB_FILENAME);
    std::fs::create_dir_all(wiki_db.parent().unwrap()).expect("create Wiki store directory");
    drop(memcore::MemoryStore::open(wiki_db.to_str().unwrap()).expect("initialize Wiki store"));
    let server = MemoryServer::new_with_home_for_test(home.join("global.db"), None, home)
        .expect("open runtime server");
    server
        .with_global_store(|store| {
            store
                .upsert(&source_entry("source"))
                .map_err(|error| error.to_string())
        })
        .expect("seed source");
    let older_revision = server
        .with_global_store_read(|store| {
            store
                .get("source")
                .map_err(|error| error.to_string())?
                .map(|entry| entry.revision)
                .ok_or_else(|| "source missing".to_string())
        })
        .expect("read older revision");
    server
        .with_global_store(|store| {
            store
                .mark_rem_processed_for_draft_at_revisions(
                    &[("source".to_string(), older_revision)],
                    "2026-07-31T00:00:01Z",
                    "wiki-rem:older-complete",
                )
                .map_err(|error| error.to_string())
        })
        .expect("mark older completed operation");
    server
        .with_global_store(|store| {
            let mut source = store
                .get("source")
                .map_err(|error| error.to_string())?
                .ok_or_else(|| "source missing".to_string())?;
            source.text = "source revision included by stale pending draft".to_string();
            store.upsert(&source).map_err(|error| error.to_string())
        })
        .expect("advance source to pending draft revision");

    let runtime = runtime_rem_source_stores(&server).expect("bind runtime source stores");
    let pending_revision = server
        .with_global_store_read(|store| {
            store
                .get("source")
                .map_err(|error| error.to_string())?
                .map(|entry| entry.revision)
                .ok_or_else(|| "source missing".to_string())
        })
        .expect("read pending revision");
    let sources = vec![RemSourceRef {
        store: runtime.global,
        id: "source".to_string(),
        revision: pending_revision,
    }];
    let draft_id = stable_rem_draft_id(&sources);
    seed_pending_rem_operation(&server, &occupied_entry(&draft_id, &sources), &sources)
        .expect("persist pending REM operation");
    server
        .with_global_store(|store| {
            let mut source = store
                .get("source")
                .map_err(|error| error.to_string())?
                .ok_or_else(|| "source missing".to_string())?;
            source.text = "source changed again before stale recovery".to_string();
            store.upsert(&source).map_err(|error| error.to_string())
        })
        .expect("make pending operation stale");

    let recovery =
        recover_pending_rem_operations(&server).expect("older marker must not wedge recovery");
    assert_eq!(recovery.completed, 0);
    assert_eq!(recovery.aborted_stale, 1);
    let source = server
        .with_global_store_read(|store| {
            store
                .get("source")
                .map_err(|error| error.to_string())?
                .ok_or_else(|| "source missing".to_string())
        })
        .expect("read source after stale recovery");
    assert_eq!(
        source.metadata["rem"]["processed_by"],
        "wiki-rem:older-complete"
    );
    assert_eq!(source.metadata["rem"]["processed_revision"], older_revision);
}

#[test]
fn stale_recovery_keeps_operation_pending_when_marker_compensation_fails() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    let wiki_db = home.join("projects/wiki").join(memcore::MEMORY_DB_FILENAME);
    std::fs::create_dir_all(wiki_db.parent().unwrap()).expect("create Wiki store directory");
    drop(memcore::MemoryStore::open(wiki_db.to_str().unwrap()).expect("initialize Wiki store"));
    let global_db = home.join("global.db");
    let project_db = home.join("project.db");
    let server = MemoryServer::new_with_home_for_test(global_db, Some(project_db), home.clone())
        .expect("open runtime server");
    server
        .with_global_store(|store| {
            store
                .upsert(&source_entry("global-source"))
                .map_err(|error| error.to_string())
        })
        .expect("seed global source");
    server
        .with_project_store(|store| {
            store
                .upsert(&source_entry("project-source"))
                .map_err(|error| error.to_string())
        })
        .expect("seed project source");

    let runtime = runtime_rem_source_stores(&server).expect("bind runtime source stores");
    let global_revision = server
        .with_global_store_read(|store| {
            store
                .get("global-source")
                .map_err(|error| error.to_string())?
                .map(|entry| entry.revision)
                .ok_or_else(|| "global source missing".to_string())
        })
        .expect("read global revision");
    let project_revision = server
        .with_project_store_read(|store| {
            store
                .get("project-source")
                .map_err(|error| error.to_string())?
                .map(|entry| entry.revision)
                .ok_or_else(|| "project source missing".to_string())
        })
        .expect("read project revision");
    let mut sources = vec![
        RemSourceRef {
            store: runtime.global,
            id: "global-source".to_string(),
            revision: global_revision,
        },
        RemSourceRef {
            store: runtime.project.expect("project store identity"),
            id: "project-source".to_string(),
            revision: project_revision,
        },
    ];
    sources.sort();
    let draft_id = stable_rem_draft_id(&sources);
    seed_pending_rem_operation(&server, &occupied_entry(&draft_id, &sources), &sources)
        .expect("persist REM operation");
    server
        .with_global_store(|store| {
            store
                .mark_rem_processed_for_draft_at_revisions(
                    &[("global-source".to_string(), global_revision)],
                    "2026-07-31T00:00:01Z",
                    &draft_id,
                )
                .map_err(|error| error.to_string())?;
            store
                .delete("global-source")
                .map_err(|error| error.to_string())?;
            Ok(())
        })
        .expect("simulate uncompensatable disappeared source");
    server
        .with_project_store(|store| {
            let mut changed = store
                .get("project-source")
                .map_err(|error| error.to_string())?
                .ok_or_else(|| "project source missing".to_string())?;
            changed.text = "project source changed after crash".to_string();
            store.upsert(&changed).map_err(|error| error.to_string())
        })
        .expect("change project source revision");

    let error = recover_pending_rem_operations(&server)
        .expect_err("compensation failure must fail closed before stale abort");
    assert!(
        error.contains("stale source marker compensation failed"),
        "{error}"
    );
    assert!(error.contains("keeping operation and source claims pending"));
    assert!(error.contains("disappeared"), "{error}");
    let global = server
        .with_global_store_read(|store| {
            store
                .get("global-source")
                .map_err(|error| error.to_string())?
                .map(|entry| entry.id)
                .ok_or_else(|| "global source missing".to_string())
        })
        .expect_err("global source must still be missing");
    assert_eq!(global, "global source missing");
    server
        .with_named_project_store("wiki", |store| {
            let pending = store
                .get(&draft_id)
                .map_err(|error| error.to_string())?
                .ok_or_else(|| "REM operation missing".to_string())?;
            assert!(!pending.archived);
            assert_eq!(
                pending.metadata["rem"]["operation_status"],
                "pending_sources"
            );
            Ok(())
        })
        .expect("verify stale operation remained pending");
    ensure_rem_draft_winner(&server, &draft_id, &sources)
        .expect("source claims must remain owned by the pending draft");
}

#[test]
fn pending_recovery_accepts_only_the_current_runtime_source_stores() {
    let global = test_store("/home/global.db");
    let project_a = test_store("/home/projects/a.db");
    let project_b = test_store("/home/projects/b.db");
    let owned = vec![
        RemSourceRef {
            store: global.clone(),
            id: "global-source".to_string(),
            revision: 1,
        },
        RemSourceRef {
            store: project_a.clone(),
            id: "project-source".to_string(),
            revision: 1,
        },
    ];

    let runtime_a = RuntimeRemSourceStores {
        global: global.clone(),
        project: Some(project_a),
        shared_route: RemSourceRole::Global,
    };
    assert!(rem_sources_belong_to_runtime_stores(&owned, &runtime_a));
    let runtime_b = RuntimeRemSourceStores {
        global: global.clone(),
        project: Some(project_b),
        shared_route: RemSourceRole::Global,
    };
    assert!(!rem_sources_belong_to_runtime_stores(&owned, &runtime_b));
    let global_only = RuntimeRemSourceStores {
        global,
        project: None,
        shared_route: RemSourceRole::Global,
    };
    assert!(!rem_sources_belong_to_runtime_stores(&owned, &global_only));
}

#[test]
fn pending_recovery_reports_foreign_rows_and_rejects_mixed_store_ownership() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    let wiki_db = home.join("projects/wiki").join(memcore::MEMORY_DB_FILENAME);
    std::fs::create_dir_all(wiki_db.parent().unwrap()).expect("create Wiki store directory");
    drop(memcore::MemoryStore::open(wiki_db.to_str().unwrap()).expect("initialize Wiki store"));
    let server = MemoryServer::new_with_home_for_test(home.join("global.db"), None, home)
        .expect("open runtime server");
    let runtime = runtime_rem_source_stores(&server).expect("bind runtime stores");

    let foreign_sources = vec![RemSourceRef {
        store: test_store("foreign-physical-store"),
        id: "foreign-source".to_string(),
        revision: 1,
    }];
    let foreign_id = stable_rem_draft_id(&foreign_sources);
    seed_pending_rem_operation(
        &server,
        &occupied_entry(&foreign_id, &foreign_sources),
        &foreign_sources,
    )
    .expect("persist foreign pending operation");
    let report = recover_pending_rem_operations(&server).expect("skip foreign operation");
    assert_eq!(report.foreign_pending_skipped, 1);
    assert_eq!(report.completed, 0);

    let mut mixed_sources = vec![
        RemSourceRef {
            store: runtime.global,
            id: "current-source".to_string(),
            revision: 1,
        },
        RemSourceRef {
            store: test_store("another-foreign-physical-store"),
            id: "foreign-source-2".to_string(),
            revision: 1,
        },
    ];
    mixed_sources.sort();
    let mixed_id = stable_rem_draft_id(&mixed_sources);
    seed_pending_rem_operation(
        &server,
        &occupied_entry(&mixed_id, &mixed_sources),
        &mixed_sources,
    )
    .expect("persist mixed pending operation");
    let error =
        recover_pending_rem_operations(&server).expect_err("mixed ownership must fail closed");
    assert!(error.contains(&mixed_id), "{error}");
    assert!(error.contains("mixes current and foreign"), "{error}");
}

#[test]
fn source_routing_marks_a_dual_role_physical_store_once() {
    let shared = test_store("same-physical-db");
    let sources = vec![RemSourceRef {
        store: shared.clone(),
        id: "source".to_string(),
        revision: 1,
    }];
    let runtime_stores = RuntimeRemSourceStores {
        global: shared.clone(),
        project: Some(shared),
        shared_route: RemSourceRole::Global,
    };
    let (global, project) = route_rem_sources(&sources, &runtime_stores, "draft").unwrap();
    assert_eq!(global, vec![("source".to_string(), 1)]);
    assert!(project.is_empty());
}

#[test]
fn deterministic_id_occupant_must_match_exact_source_set() {
    let sources = vec![RemSourceRef {
        store: test_store("project-db"),
        id: "source-a".to_string(),
        revision: 1,
    }];
    let id = stable_rem_draft_id(&sources);
    let valid = occupied_entry(&id, &sources);
    validate_existing_rem_draft(&valid, &id, &sources).expect("canonical occupant");

    let collision_sources = vec![RemSourceRef {
        store: test_store("project-db"),
        id: "source-b".to_string(),
        revision: 1,
    }];
    let collision = occupied_entry(&id, &collision_sources);
    let error = validate_existing_rem_draft(&collision, &id, &sources)
        .expect_err("mismatched occupant must fail");
    assert!(error.to_string().contains("rem_wiki_identity_conflict"));

    let mut archived = valid;
    archived.archived = true;
    let error = validate_existing_rem_draft(&archived, &id, &sources)
        .expect_err("archived deterministic winner must fail");
    assert!(error.to_string().contains("occupied row is archived"));
}

#[test]
fn pending_operation_from_an_older_producer_version_still_validates() {
    let sources = vec![RemSourceRef {
        store: test_store("project-db"),
        id: "source-a".to_string(),
        revision: 1,
    }];
    let old_version = "weekly-wiki-evolver-v0";
    let id = stable_rem_draft_id_for_version(&sources, old_version);
    let mut existing = occupied_entry(&id, &sources);
    existing.metadata["rem"]["producer_version"] = json!(old_version);

    validate_existing_rem_draft(&existing, &id, &sources)
        .expect("recovery must honor the operation's persisted producer version");
}
