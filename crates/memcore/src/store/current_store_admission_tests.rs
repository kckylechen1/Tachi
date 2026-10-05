//! #1995 preparation only: production admission is intentionally unchanged.
//! The ignored tests assert the required refusal, NOT successful silent repair.
//! All databases and migration artifacts are owned temporary fixtures.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use rusqlite::Connection;
use serde_json::{json, Value};

use crate::db::{DbOpenContext, MigrationAuthority, OpenIntent, StoreProfile};
use crate::{MemoryError, MemoryStore};

fn context(profile: StoreProfile, migration: MigrationAuthority) -> DbOpenContext {
    DbOpenContext {
        intent: OpenIntent::OpenExisting,
        migration,
        required_profile: profile.into(),
    }
}

fn fixture(profile: StoreProfile) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("owned fixture directory");
    let path = dir.path().join("tachi-memory.db");
    provision(profile, &path);
    (dir, path)
}

fn provision(profile: StoreProfile, path: &Path) {
    drop(
        MemoryStore::open_with_context(
            path.to_str().unwrap(),
            &context(profile, MigrationAuthority::Deny),
        )
        .expect("create fresh fixture through production funnel"),
    );
    let conn = Connection::open(path).unwrap();
    let stamp: u32 = conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(stamp, crate::db::migrations::EXPECTED_SCHEMA_VERSION);
}

use crate::db::schema_inventory::{query_rows, quote, schema_inventory, tables};

fn logical_snapshot(conn: &Connection) -> Value {
    let content: BTreeMap<_, _> = tables(conn)
        .into_iter()
        .map(|name| {
            let rows = query_rows(conn, &format!("SELECT * FROM {}", quote(&name)));
            (name, rows)
        })
        .collect();
    json!({
        "schema": schema_inventory(conn),
        "user_version": query_rows(conn, "PRAGMA user_version"),
        "schema_version": query_rows(conn, "PRAGMA schema_version"),
        "journal_mode": query_rows(conn, "PRAGMA journal_mode"),
        "content": content,
    })
}

fn migration_artifacts(path: &Path) -> BTreeMap<String, Vec<u8>> {
    let name = path.file_name().unwrap().to_str().unwrap();
    std::fs::read_dir(path.parent().unwrap())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter_map(|entry| {
            let leaf = entry.file_name().unwrap().to_str().unwrap().to_string();
            (leaf == format!("{name}.migration-marker")
                || leaf.starts_with(&format!("{name}.migration-bak.")))
            .then(|| (leaf, std::fs::read(entry).unwrap()))
        })
        .collect()
}

fn seed_claim(conn: &Connection) {
    conn.execute_batch(
        "INSERT INTO session_claims(claim_id, session_client, issue_ref, flow_id, heartbeat_at)
         VALUES ('claim-older', 'fixture-client', 'fixture-1995', NULL, '2026-01-01T00:00:00Z');",
    )
    .unwrap();
}

fn assert_refused_without_repair(path: &Path, authority: MigrationAuthority, missing: &str) {
    let before = logical_snapshot(&Connection::open(path).unwrap());
    let artifacts = migration_artifacts(path);
    let result = MemoryStore::open_with_context(
        path.to_str().unwrap(),
        &context(StoreProfile::TachiFull, authority),
    );
    let error = match result {
        Ok(store) => {
            drop(store);
            None
        }
        Err(error) => Some(error),
    };
    let after = logical_snapshot(&Connection::open(path).unwrap());
    // Read back even after an unexpected Ok, so the reproduction reports the
    // actual mutation rather than stopping at the returned Result alone.
    assert_eq!(
        before, after,
        "damaged current input was repaired; result={error:?}"
    );
    assert_eq!(
        artifacts,
        migration_artifacts(path),
        "backup/marker changed"
    );
    let error: MemoryError = error.expect("damaged current input must be refused");
    // The typed variant does not exist until the production fix. Keep this
    // red test compilable now, but do not accept an arbitrary refusal later.
    let debug = format!("{error:?}");
    assert!(debug.starts_with("CurrentSchemaIncomplete"), "{debug}");
    assert!(
        debug.contains(missing),
        "refusal must name {missing}: {debug}"
    );
}

#[test]
#[ignore = "#1995 known red: current open dedupes claims before recreating the index"]
fn missing_claim_identity_index_refuses_without_releasing_claims() {
    for authority in [
        MigrationAuthority::Deny,
        MigrationAuthority::Allow {
            approved_by: "test:#1995".into(),
        },
    ] {
        let (_dir, path) = fixture(StoreProfile::TachiFull);
        let conn = Connection::open(&path).unwrap();
        seed_claim(&conn);
        conn.execute_batch(
            "DROP INDEX idx_session_claims_identity_active;
             INSERT INTO session_claims(claim_id, session_client, issue_ref, flow_id, heartbeat_at)
             VALUES ('claim-newer', 'fixture-client', 'fixture-1995', NULL, '2026-01-02T00:00:00Z');",
        ).unwrap();
        assert_eq!(
            conn.query_row(
                "SELECT count(*) FROM session_claims WHERE state='active' AND mode IS NULL",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            2
        );
        drop(conn);
        assert_refused_without_repair(&path, authority, "idx_session_claims_identity_active");
    }
}

#[test]
#[ignore = "#1995 known red: current open recreates the missing state table empty"]
fn missing_worktree_identity_table_refuses_without_recreating_it() {
    for authority in [
        MigrationAuthority::Deny,
        MigrationAuthority::Allow {
            approved_by: "test:#1995".into(),
        },
    ] {
        let (_dir, path) = fixture(StoreProfile::TachiFull);
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "INSERT INTO exec_envs(env_id, path) VALUES ('env-fixture', '/fixture/not-a-live-worktree');
             INSERT INTO exec_env_worktree_identities(env_id, device, inode) VALUES ('env-fixture', 1, 2);",
        ).unwrap();
        assert_eq!(
            conn.query_row(
                "SELECT count(*) FROM exec_env_worktree_identities",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            1
        );
        conn.execute_batch("DROP TABLE exec_env_worktree_identities")
            .unwrap();
        drop(conn);
        assert_refused_without_repair(&path, authority, "exec_env_worktree_identities");
    }
}

#[test]
fn healthy_current_reopen_preserves_populated_full_store() {
    let (_dir, path) = fixture(StoreProfile::TachiFull);
    // Fresh schema init writes its marker before optional vec provisioning.
    // One ordinary reopen legitimately aligns that marker (and may back up).
    // Converge through the actual funnel BEFORE taking the healthy baseline;
    // never do this after damage in either refusal probe.
    drop(
        MemoryStore::open_with_context(
            path.to_str().unwrap(),
            &context(StoreProfile::TachiFull, MigrationAuthority::Deny),
        )
        .unwrap(),
    );
    let conn = Connection::open(&path).unwrap();
    seed_claim(&conn);
    conn.execute_batch("INSERT INTO exec_envs(env_id, path) VALUES ('env-fixture', '/fixture/not-a-live-worktree'); INSERT INTO exec_env_worktree_identities(env_id, device, inode) VALUES ('env-fixture', 1, 2);").unwrap();
    let before = logical_snapshot(&conn);
    drop(conn);
    let artifacts = migration_artifacts(&path);
    drop(
        MemoryStore::open_with_context(
            path.to_str().unwrap(),
            &context(StoreProfile::TachiFull, MigrationAuthority::Deny),
        )
        .unwrap(),
    );
    assert_eq!(before, logical_snapshot(&Connection::open(&path).unwrap()));
    assert_eq!(artifacts, migration_artifacts(&path));
}

#[test]
fn enumerate_fresh_and_reopened_schema_for_both_profiles() {
    let mut seed = BTreeMap::new();
    for profile in [StoreProfile::PortableKernel, StoreProfile::TachiFull] {
        let (_dir, path) = fixture(profile);
        let fresh_conn = Connection::open(&path).unwrap();
        let fresh = schema_inventory(&fresh_conn);
        let fresh_classified = crate::db::schema_inventory::classified_inventory(&fresh_conn);
        drop(fresh_conn);
        drop(
            MemoryStore::open_with_context(
                path.to_str().unwrap(),
                &context(profile, MigrationAuthority::Deny),
            )
            .unwrap(),
        );
        let reopened = schema_inventory(&Connection::open(&path).unwrap());
        assert_eq!(
            fresh, reopened,
            "fresh/reopened inventory diverges: {profile:?}"
        );
        let conn = Connection::open(&path).unwrap();
        let classified = crate::db::schema_inventory::classified_inventory(&conn);
        assert_eq!(fresh_classified.required, classified.required);
        assert_eq!(
            fresh_classified.frozen_classification(),
            classified.frozen_classification(),
            "fresh/reopened classification diverges: {profile:?}"
        );
        assert!(classified.required.get("table:derived_items").is_some());
        assert!(classified
            .required
            .get("table:memory_search_generation")
            .is_some());
        assert!(classified
            .required
            .get("trigger:memory_search_generation_after_update")
            .is_some());
        assert_eq!(
            classified
                .required
                .get("table:exec_env_worktree_identities")
                .is_some(),
            profile.includes_product()
        );
        println!(
            "CLASSIFICATION_CENSUS:{}",
            json!({"profile":format!("{profile:?}"), "classes":classified.classes,"validator_required":classified.validator_required})
        );
        seed.insert(format!("{profile:?}"),json!({"required":classified.required,"classification":classified.frozen_classification()}));
        println!(
            "{}",
            json!({"profile": format!("{profile:?}"), "version": crate::db::migrations::EXPECTED_SCHEMA_VERSION, "inventory": reopened})
        );
    }
    println!(
        "REQUIRED_INITIAL_SEED:{}",
        json!({(crate::db::migrations::EXPECTED_SCHEMA_VERSION.to_string()): &seed})
    );
    for profile in [StoreProfile::PortableKernel, StoreProfile::TachiFull] {
        assert_eq!(
            seed[&format!("{profile:?}")],
            crate::db::schema_inventory::golden(profile),
            "required inventory changed without a versioned migration: {profile:?}"
        );
    }
}

#[test]
fn inventory_growth_discriminators_run_in_real_initializer() {
    use crate::db::schema_inventory::{
        arm_growth, classified_inventory, growth_fires, GrowthMutation,
    };
    for profile in [StoreProfile::PortableKernel, StoreProfile::TachiFull] {
        for mutation in [
            GrowthMutation::BeforeMemoryRebuild,
            GrowthMutation::ElsewhereColumn,
            GrowthMutation::InlineTable,
        ] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("tachi-memory.db");
            let guard = arm_growth(mutation, &path);
            provision(profile, &path);
            let conn = Connection::open(&path).unwrap();
            let fresh = classified_inventory(&conn).required;
            let fresh_memory_column: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM pragma_table_info('memories') WHERE name='inventory_unversioned_column')", [], |r|r.get(0)).unwrap();
            drop(conn);
            drop(
                MemoryStore::open_with_context(
                    path.to_str().unwrap(),
                    &context(profile, MigrationAuthority::Deny),
                )
                .unwrap(),
            );
            let reopened = classified_inventory(&Connection::open(&path).unwrap()).required;
            assert_eq!(
                growth_fires(),
                2,
                "mutation must reach both real initializer calls"
            );
            match mutation {
                GrowthMutation::BeforeMemoryRebuild => {
                    assert_eq!(
                        fresh,
                        crate::db::schema_inventory::golden(profile)["required"],
                        "fresh rebuild must restore the frozen baseline"
                    );
                    assert!(
                        !fresh_memory_column,
                        "fresh enum rebuild must drop the injected column"
                    );
                    let conn = Connection::open(&path).unwrap();
                    let present: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM pragma_table_info('memories') WHERE name='inventory_unversioned_column')", [], |r|r.get(0)).unwrap();
                    assert!(
                        present,
                        "converged initializer must keep the injected column"
                    );
                    assert_ne!(
                        fresh, reopened,
                        "fresh rebuild must discard the injected column"
                    );
                }
                GrowthMutation::ElsewhereColumn => {
                    assert_eq!(fresh, reopened);
                    assert_ne!(
                        fresh,
                        crate::db::schema_inventory::golden(profile)["required"],
                        "unversioned column must violate frozen inventory"
                    );
                }
                GrowthMutation::InlineTable => {
                    assert_eq!(fresh, reopened);
                    assert!(fresh
                        .get("table:memories_fts_inventory_unversioned_table")
                        .is_some());
                    assert_ne!(
                        fresh,
                        crate::db::schema_inventory::golden(profile)["required"],
                        "unversioned inline table must violate frozen inventory"
                    );
                }
            }
            println!("GROWTH_DISCRIMINATOR:{profile:?}:{mutation:?}:fires=2:PASS");
            drop(guard);
            assert_eq!(
                growth_fires(),
                0,
                "RAII must release the thread-local mutation"
            );
        }
    }
}

#[test]
fn optional_family_absence_preserves_required_inventory() {
    for profile in [StoreProfile::PortableKernel, StoreProfile::TachiFull] {
        let (_dir, path) = fixture(profile);
        let conn = Connection::open(&path).unwrap();
        let before = crate::db::schema_inventory::classified_inventory(&conn);
        conn.execute_batch(
            "DROP TABLE IF EXISTS memories_vec; DROP INDEX IF EXISTS idx_memories_path_active_ts",
        )
        .unwrap();
        let after = crate::db::schema_inventory::classified_inventory(&conn);
        assert_eq!(before.required, after.required);
        assert_eq!(
            before.frozen_classification(),
            after.frozen_classification()
        );
        assert_eq!(
            after.required,
            crate::db::schema_inventory::golden(profile)["required"]
        );
    }
}
