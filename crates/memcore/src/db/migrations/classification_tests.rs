//! D7 §10 T1: direct BODY classification, separate from admission/funnel tests.
//! T1a reconstructs whole Full predecessors from fixed historical DDL and
//! pins each prefix inventory independently. These are source reconstructions,
//! not receipts of deployed Full database lineage.
//! T1b uses pinned historical literals, never a current initializer/restamp.
use super::*;
use rusqlite::types::Value;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

#[path = "classification_history.rs"]
mod history;

fn connection() -> Connection {
    crate::db::enable_simple_auto_extension().unwrap();
    let conn = Connection::open_in_memory().unwrap();
    crate::db::ensure_reserved_reference_write_guard(&conn).unwrap();
    conn
}

/// Fixed Portable v36 source reconstruction for T9: 2b3951a4 baseline plus
/// repaired v36 delivery literal from 37a7981d^, before the v37 introduction. The search-generation
/// bridge is pinned unversioned maintenance from 2126bacf (§8 A6), not evidence
/// of an old deployed database. No current schema initializer is called here.
/// The caller writes role/profile identity and a matching open marker separately.
pub(crate) fn install_historical_portable_v36(conn: &Connection) {
    crate::db::ensure_reserved_reference_write_guard(conn).unwrap();
    history::install(conn, 37);
    for migration in &MIGRATIONS[..36] {
        mark_run(conn, migration.sentinel).unwrap();
    }
    write_schema_version(conn, 36).unwrap();
    assert_eq!(
        count(
            conn,
            "SELECT count(*) FROM hard_state WHERE namespace='migrations'"
        ),
        36
    );
    for key in [
        "v37_verified_agent_admissions",
        "v38_current_truth",
        "v39_mirror_eval_identity",
    ] {
        assert!(!was_run(conn, key).unwrap());
    }
    crate::db::schema::validate_recall_impression_ledger_schema(conn).unwrap();
    crate::db::schema::validate_typo_fallback_attribution_schema(conn).unwrap();
    crate::db::schema::validate_wiki_recovery_ledgers_schema(conn).unwrap();
    crate::db::schema::validate_memory_outbox_schema(conn).unwrap();
    crate::db::schema::validate_memory_outbox_destination_apply_schema(conn).unwrap();
    crate::db::schema::validate_harness_session_attachments_schema(conn).unwrap();
    crate::db::schema::validate_harness_session_spine_schema(conn).unwrap();
    crate::db::schema::validate_delivery_spine_schema(conn).unwrap();
    crate::db::validate_persistent_trigger_inventory(conn, true).unwrap();
}

fn body(conn: &Connection, index: u32, profile: StoreProfile) -> MigrationReport {
    let migration = &MIGRATIONS[index as usize - 1];
    assert_eq!(migration.index, index);
    let mut report = MigrationReport::default();
    (migration.run)(
        conn,
        &catalogue::MigrationContext {
            db_label: "classification",
            path: Path::new("/tmp/d7-classification.db"),
            profile,
        },
        &mut report,
    )
    .unwrap_or_else(|e| panic!("v{index} BODY failed: {e}"));
    report
}

fn count(conn: &Connection, sql: &str) -> i64 {
    conn.query_row(sql, [], |row| row.get(0)).unwrap()
}

fn tables(conn: &Connection) -> BTreeSet<String> {
    let mut stmt = conn.prepare("SELECT name FROM sqlite_schema WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name").unwrap();
    stmt.query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

/// Seed each ordinary table independently; constraints still run. Shadow FTS
/// tables are populated by their virtual-table insert, never by direct writes.
fn populate(conn: &Connection) {
    populate_tables(conn, &tables(conn));
}

fn populate_tables(conn: &Connection, names: &BTreeSet<String>) {
    fn visit(
        conn: &Connection,
        table: &str,
        names: &BTreeSet<String>,
        seen: &mut BTreeSet<String>,
        ordered: &mut Vec<String>,
    ) {
        if !seen.insert(table.into()) {
            return;
        }
        let mut stmt = conn
            .prepare(&format!("PRAGMA foreign_key_list(\"{table}\")"))
            .unwrap();
        let dependencies: Vec<String> = stmt
            .query_map([], |row| row.get(2))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        for dependency in dependencies {
            if names.contains(&dependency) {
                visit(conn, &dependency, names, seen, ordered);
            }
        }
        ordered.push(table.into());
    }
    let mut ordered = Vec::new();
    let mut seen = BTreeSet::new();
    for name in names {
        visit(conn, name, names, &mut seen, &mut ordered);
    }
    for table in &ordered {
        if table.starts_with("memories_fts_")
            || table.starts_with("memories_symbolic_fts_")
            || table == "memories_fts"
            || table == "memories_symbolic_fts"
            || table == "memory_embeddings"
            || table.starts_with("memory_embeddings_")
        {
            continue;
        }
        if count(conn, &format!("SELECT count(*) FROM \"{table}\"")) != 0 {
            continue;
        }
        let sql: String = conn
            .query_row(
                "SELECT sql FROM sqlite_schema WHERE name=?1",
                [table],
                |row| row.get(0),
            )
            .unwrap();
        let mut stmt = conn
            .prepare(&format!("PRAGMA table_info(\"{table}\")"))
            .unwrap();
        let cols: Vec<(String, String, bool, Option<String>, bool)> = stmt
            .query_map([], |row| {
                Ok((
                    row.get(1)?,
                    row.get(2)?,
                    row.get::<_, i64>(3)? != 0,
                    row.get(4)?,
                    row.get::<_, i64>(5)? != 0,
                ))
            })
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        let enum_sql = sql.split_whitespace().collect::<Vec<_>>().join(" ");
        let mut keys = Vec::new();
        let mut values = Vec::new();
        for (name, kind, required, default, primary) in cols {
            if primary && kind == "INTEGER" {
                continue;
            }
            if !primary && (!required || default.is_some()) {
                continue;
            }
            keys.push(format!("\"{name}\""));
            let value = if kind.contains("INT") {
                Value::Integer(1)
            } else if kind.contains("REAL") {
                Value::Real(0.5)
            } else if let Some(start) = enum_sql.find(&format!("{name} IN (")) {
                let tail = enum_sql[start + name.len() + 5..]
                    .trim_start()
                    .strip_prefix('\'')
                    .expect("literal enum");
                Value::Text(tail.split('\'').next().unwrap().into())
            } else if name == "application" {
                Value::Text("applied".into())
            } else if name == "capabilities_json" {
                Value::Text(r#"{"observe":true,"wait":true,"prompt":false,"cancel":false,"resume":false,"load":false,"events":true,"artifacts":false}"#.into())
            } else if name.ends_with("_json") || name == "value_json" {
                Value::Text("{}".into())
            } else {
                Value::Text("seed".into())
            };
            values.push(value);
        }
        let marks = vec!["?"; values.len()].join(",");
        let insert = if keys.is_empty() {
            format!("INSERT INTO \"{table}\" DEFAULT VALUES")
        } else {
            format!(
                "INSERT INTO \"{table}\" ({}) VALUES ({marks})",
                keys.join(",")
            )
        };
        conn.execute(&insert, rusqlite::params_from_iter(values))
            .unwrap_or_else(|e| panic!("populate {table}: {e}; {insert}"));
    }
    if names.contains("memories") {
        conn.execute("UPDATE memories SET source='wiki', text='wiki bytes preserved', path='/wiki/classification'", []).unwrap();
    }
    for fts in ["memories_fts", "memories_symbolic_fts"] {
        if names.contains(fts) {
            conn.execute_batch(&format!("INSERT INTO {fts}(id,path,summary,text,keywords,entities) SELECT id,path,summary,text,keywords,entities FROM memories")).unwrap();
        }
    }
    for table in names {
        assert!(
            count(conn, &format!("SELECT count(*) FROM \"{table}\"")) > 0,
            "unpopulated Portable table {table}"
        );
    }
}

#[derive(Debug, PartialEq, Eq)]
struct PortableSnapshot {
    objects: Vec<(String, String, String, Option<String>)>,
    content: BTreeMap<String, [u8; 32]>,
}

fn snapshot(conn: &Connection, portable: &BTreeSet<String>) -> PortableSnapshot {
    let mut stmt = conn
        .prepare("SELECT type,name,tbl_name,sql FROM sqlite_schema ORDER BY type,name")
        .unwrap();
    let objects = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
            ))
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
        .into_iter()
        .filter(|(_, _, owner, _)| portable.contains(owner))
        .collect();
    let mut content = BTreeMap::new();
    for table in portable {
        let mut stmt = conn.prepare(&format!("SELECT * FROM \"{table}\"")).unwrap();
        let columns = stmt.column_count();
        let mut rows: Vec<String> = stmt
            .query_map([], |row| {
                (0..columns)
                    .map(|i| row.get::<_, Value>(i))
                    .collect::<Result<Vec<_>, _>>()
                    .map(|v| format!("{v:?}"))
            })
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        rows.sort();
        let mut hash = Sha256::new();
        for row in rows {
            hash.update((row.len() as u64).to_le_bytes());
            hash.update(row);
        }
        content.insert(table.clone(), hash.finalize().into());
    }
    PortableSnapshot { objects, content }
}

fn preservation(before: &PortableSnapshot, after: &PortableSnapshot) -> Result<(), &'static str> {
    if before.objects != after.objects {
        return Err("Portable schema changed");
    }
    if before.content != after.content {
        return Err("Portable content changed");
    }
    Ok(())
}

fn full_predecessor(index: u32) -> (Connection, BTreeSet<String>) {
    let conn = connection();
    let prefix = history::full_prefix(index);
    history::install_full(&conn, &prefix);
    let expected: BTreeSet<String> = prefix.tables.iter().map(|name| (*name).into()).collect();
    assert_eq!(
        tables(&conn),
        expected,
        "incomplete historical Full predecessor v{} from {}",
        index - 1,
        prefix.source
    );
    let portable: BTreeSet<String> = prefix.portable.iter().map(|name| (*name).into()).collect();
    assert!(portable.contains("memories"));
    assert!(
        expected.contains("exec_envs"),
        "Full Product surface required"
    );
    for key in &MIGRATIONS[..index as usize - 1] {
        mark_run(&conn, key.sentinel).unwrap();
    }
    write_schema_version(&conn, index - 1).unwrap();
    assert!(!was_run(&conn, MIGRATIONS[index as usize - 1].sentinel).unwrap());
    populate_tables(&conn, &portable);
    // Drive actual data-changing Product branches on their whole predecessor.
    match index {
        12 => conn.execute_batch("INSERT INTO session_claims(claim_id,session_client,issue_ref,flow_id,heartbeat_at) VALUES('old','client','issue','flow','1'),('new','client','issue','flow','2');").unwrap(),
        14 | 16 | 17 => conn.execute_batch("INSERT INTO dispatch_outcomes(outcome_id,execution_outcome,idempotency_key,vendor,model) VALUES('o','completed','key','codex','historical');").unwrap(),
        15 => { conn.execute("INSERT INTO exec_envs(env_id,path) VALUES('env','/wt/historical')",[]).unwrap(); },
        21 => { conn.execute("INSERT INTO session_claims(claim_id) VALUES('claim')",[]).unwrap(); conn.execute("INSERT INTO exec_envs(env_id,path) VALUES('env','/wt/historical')",[]).unwrap(); },
        32 => crate::db::schema::validate_a2a_mailbox_v31_schema(&conn).unwrap(),
        _ => (),
    }
    assert_eq!(count(&conn, "PRAGMA user_version"), i64::from(index - 1));
    assert!(
        count(
            &conn,
            "SELECT count(*) FROM memories WHERE source='wiki' AND text='wiki bytes preserved'"
        ) > 0
    );
    for table in &portable {
        assert!(
            count(&conn, &format!("SELECT count(*) FROM \"{table}\"")) > 0,
            "empty historical Portable {table}"
        );
    }
    eprintln!(
        "T1a Full@{} source={} full_tables={} populated_portable_tables={}",
        index - 1,
        prefix.source,
        expected.len(),
        portable.len()
    );
    (conn, portable)
}

#[test]
fn every_product_body_preserves_all_populated_portable_objects_and_content() {
    let product = [12, 14, 15, 16, 17, 18, 20, 21, 31, 32, 37, 38, 39];
    for index in product {
        let (conn, portable) = full_predecessor(index);
        let before = snapshot(&conn, &portable);
        let report = body(&conn, index, StoreProfile::TachiFull);
        match index {
            12 => assert_eq!(report.session_claims_duplicates_deduped, 1),
            14 => assert_eq!(report.dispatch_outcomes_reported_outcome_added, 1),
            15 => assert_eq!(report.exec_envs_env_class_added, 1),
            16 => assert_eq!(report.dispatch_outcomes_identity_receipt_added, 1),
            17 => {
                assert_eq!(report.dispatch_outcomes_attribution_basis_backfilled, 1);
                assert_eq!(count(&conn,"SELECT count(*) FROM dispatch_outcomes WHERE identity_attribution_basis='fallback_unreceipted'"),1);
            }
            18 => assert_eq!(report.dispatch_adjudications_created, 1),
            20 => assert_eq!(report.mirror_eval_tables_created, 1),
            21 => assert_eq!(report.identity_workclaim_columns_added, 11),
            31 => assert_eq!(report.a2a_mailbox_schema_objects_created, 5),
            32 => assert_eq!(report.a2a_body_retention_schema_objects_rebuilt, 5),
            37 => assert_eq!(report.verified_admission_schema_objects_created, 14),
            38 => assert_eq!(report.current_truth_schema_objects_created, 5),
            39 => assert_eq!(report.mirror_eval_identity_columns_added, 4),
            _ => unreachable!(),
        }
        match index {
            12 => assert_eq!(count(&conn,"SELECT count(*) FROM sqlite_schema WHERE name='idx_session_claims_identity_active'"),1),
            14 => assert!(table_has_column(&conn,"dispatch_outcomes","reported_outcome").unwrap()),
            15 => { assert!(table_has_column(&conn,"exec_envs","env_class").unwrap()); assert_eq!(count(&conn,"SELECT count(*) FROM exec_envs WHERE env_class='edit-only'"),1); },
            16 => assert!(table_has_column(&conn,"dispatch_outcomes","identity_receipt").unwrap()),
            17 => assert!(table_has_column(&conn,"dispatch_outcomes","identity_attribution_basis").unwrap()),
            18 => assert_eq!(count(&conn,"SELECT count(*) FROM sqlite_schema WHERE name IN ('dispatch_adjudications','dispatch_adjudication_signatures')"),2),
            20 => assert_eq!(count(&conn,"SELECT count(*) FROM sqlite_schema WHERE name IN ('mirror_eval_runs','mirror_eval_observations','mirror_eval_adjudications')"),3),
            21 => assert!(table_has_column(&conn,"session_claims","worktree_path").unwrap()),
            31 => crate::db::schema::validate_a2a_mailbox_v31_schema(&conn).unwrap(),
            32 => crate::db::schema::validate_a2a_mailbox_schema(&conn).unwrap(),
            37 => crate::db::verified_admissions::validate_verified_admission_schema(&conn).unwrap(),
            38 => crate::db::schema::validate_current_truth_schema(&conn).unwrap(),
            39 => crate::db::schema::validate_mirror_eval_identity_schema(&conn).unwrap(),
            _ => unreachable!(),
        }
        let after = snapshot(&conn, &portable);
        assert_eq!(
            preservation(&before, &after),
            Ok(()),
            "v{index} modified Portable schema/content"
        );
        eprintln!("T1a Product v{index}: full BODY effect and all Portable schema/content hashes preserved");
    }
}

#[test]
fn preservation_oracle_rejects_both_misclassified_product_bodies() {
    let poisons = [
        Migration {
            index: 40,
            sentinel: "test_misclassified_alter",
            scope: MigrationScope::Product,
            run: |conn, _, _| {
                conn.execute_batch("ALTER TABLE memories ADD COLUMN wrongly_product_scoped TEXT")?;
                Ok(())
            },
        },
        Migration {
            index: 40,
            sentinel: "test_misclassified_rewrite",
            scope: MigrationScope::Product,
            run: |conn, _, _| {
                conn.execute_batch("UPDATE memories SET text='wrongly rewritten'")?;
                Ok(())
            },
        },
    ];
    for migration in poisons {
        let (conn, portable) = full_predecessor(39);
        let before = snapshot(&conn, &portable);
        assert_eq!(migration.scope, MigrationScope::Product);
        (migration.run)(
            &conn,
            &catalogue::MigrationContext {
                db_label: "poisoned-full",
                path: Path::new("/tmp/d7-classification.db"),
                profile: StoreProfile::TachiFull,
            },
            &mut MigrationReport::default(),
        )
        .unwrap();
        let after = snapshot(&conn, &portable);
        let expected = if migration.sentinel.ends_with("alter") {
            "Portable schema changed"
        } else {
            "Portable content changed"
        };
        assert_eq!(
            preservation(&before, &after),
            Err(expected),
            "T1a oracle admitted {}",
            migration.sentinel
        );
        if migration.sentinel.ends_with("rewrite") {
            assert_ne!(after.content["memories"], before.content["memories"]);
        }
        eprintln!(
            "T1c {}: rejected by same T1a preservation oracle ({expected})",
            migration.sentinel
        );
    }
}

fn legacy_predecessor(conn: &Connection, index: u32) {
    // v1–11 predate the schema counter and profiles. This is an unprofiled
    // historical-shaped component surrogate, not a deployed P@0..10 image.
    // Literal columns mirror schema/migration_tests.rs legacy helpers and
    // migrations.rs v5/v7/v8/v9/packs/domains fixtures at 2126bacf.
    conn.execute_batch("CREATE TABLE memories(id TEXT PRIMARY KEY,path TEXT DEFAULT '/',scope TEXT,summary TEXT DEFAULT '',text TEXT DEFAULT '',timestamp TEXT DEFAULT '',topic TEXT DEFAULT '',keywords TEXT DEFAULT '[]',entities TEXT DEFAULT '[]',metadata TEXT DEFAULT '{}',source TEXT DEFAULT 'manual'); INSERT INTO memories(id,path,scope,text) VALUES('m','notes\\x','self','legacy'),('local-a','/facts/a','general','a'),('local-b','/facts/b','general','b'); CREATE VIRTUAL TABLE memories_fts USING fts5(id UNINDEXED,path,summary,text,keywords,entities,tokenize='unicode61'); INSERT INTO memories_fts(id,path,text) SELECT id,path,text FROM memories;").unwrap();
    match index {
        1 | 2 => (),
        3 => { conn.execute("UPDATE memories SET path='/handoff' WHERE id='m'",[]).unwrap(); },
        4 => { conn.execute("UPDATE memories SET path='/facts/foreign',metadata=?1 WHERE id='m'",[r#"{"provenance":{"db_path":"/other/database.db"}}"#]).unwrap(); },
        5 => conn.execute_batch("ALTER TABLE memories ADD COLUMN indexed_tags TEXT DEFAULT '[]'; ALTER TABLE memories ADD COLUMN domain_key TEXT DEFAULT ''; UPDATE memories SET indexed_tags='[\"legacy\"]',domain_key='legacy';").unwrap(),
        6 | 8 => conn.execute_batch("ALTER TABLE memories ADD COLUMN persons TEXT DEFAULT '[]'; UPDATE memories SET persons='[\"Kyle\"]' WHERE id='m';").unwrap(),
        7 => conn.execute_batch("ALTER TABLE memories ADD COLUMN domain TEXT; ALTER TABLE memories ADD COLUMN indexed_tags TEXT DEFAULT '[]'; ALTER TABLE memories ADD COLUMN domain_key TEXT DEFAULT ''; UPDATE memories SET indexed_tags='[\"rust\"]',domain_key='legacy' WHERE id='m';").unwrap(),
        9 => conn.execute_batch("ALTER TABLE memories ADD COLUMN location TEXT DEFAULT ''; UPDATE memories SET path='/',location='/scratch/legacy' WHERE id='m';").unwrap(),
        10 => conn.execute_batch("CREATE TABLE packs(id TEXT PRIMARY KEY,name TEXT); CREATE TABLE agent_projections(agent TEXT,pack_id TEXT); INSERT INTO packs VALUES('pack','legacy'); INSERT INTO agent_projections VALUES('agent','pack');").unwrap(),
        11 => conn.execute_batch("ALTER TABLE memories ADD COLUMN domain TEXT; UPDATE memories SET domain='kept'; CREATE TABLE domains(key TEXT PRIMARY KEY,name TEXT); INSERT INTO domains VALUES('legacy','domain');").unwrap(),
        _ => unreachable!(),
    }
}

#[test]
fn every_portable_body_runs_on_pinned_historical_predecessor() {
    let portable = [
        1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 19, 22, 23, 24, 25, 26, 27, 28, 29, 30, 33, 34, 35,
        36,
    ];
    for index in portable {
        let conn = connection();
        if index <= 11 {
            legacy_predecessor(&conn, index);
        } else {
            history::install(&conn, index);
            populate(&conn);
            write_schema_version(&conn, index - 1).unwrap();
        }
        if index == 22 {
            conn.execute(
                "UPDATE memories_symbolic_fts SET text='stale predecessor projection'",
                [],
            )
            .unwrap();
        }
        if index == 26 {
            assert!(table_has_column(&conn, "recall_impression_groups", "query_hash").unwrap());
            assert!(
                !table_has_column(&conn, "recall_impression_groups", "query_fingerprint").unwrap()
            );
        }
        let report = body(&conn, index, StoreProfile::PortableKernel);
        match index {
            1 => assert!(report.paths_normalized > 0),
            2 => assert_eq!(report.scopes_fixed, 1),
            3 => assert_eq!(report.handoff_paths_standardized, 1),
            4 => {
                assert_eq!(report.quarantined, 1);
                assert!(!report.quarantine_skipped_sanity_guard);
            }
            5 => assert_eq!(report.hypertachi_legacy_columns_dropped, 2),
            6 => assert_eq!(report.persons_folded_into_entities, 1),
            7 => {
                assert!(report.legacy_columns_reconciled >= 4);
                assert_eq!(count(&conn,"SELECT count(*) FROM memories WHERE keywords='[\"rust\"]' AND domain='legacy'"),1);
            }
            8 => assert_eq!(report.persons_columns_dropped, 2),
            9 => {
                assert_eq!(report.locations_relocated, 1);
                assert_eq!(report.location_columns_dropped, 1);
            }
            10 => assert_eq!(report.pack_tables_dropped, 2),
            11 => {
                assert_eq!(report.domains_table_dropped, 1);
                assert_eq!(
                    count(&conn, "SELECT count(*) FROM memories WHERE domain='kept'"),
                    3
                );
            }
            13 => {
                assert_eq!(report.hard_state_index_added, 1);
                assert_eq!(
                    count(
                        &conn,
                        "SELECT count(*) FROM sqlite_schema WHERE name='idx_hard_state_ns_updated'"
                    ),
                    1
                );
            }
            19 => {
                assert_eq!(report.idless_identity_constraint_added, 1);
                assert!(table_has_column(&conn, "memories", "idless_identity").unwrap());
            }
            22 => {
                assert!(report.memories_symbolic_fts_rows > 0);
                assert_eq!(count(&conn,"SELECT count(*) FROM memories_symbolic_fts WHERE text='stale predecessor projection'"),0);
                assert_eq!(
                    count(&conn, "SELECT count(*) FROM memories_symbolic_fts"),
                    count(&conn, "SELECT count(*) FROM memories WHERE id IS NOT NULL")
                );
            }
            23 => {
                assert_eq!(report.reserved_reference_guards_installed, 2);
                crate::db::validate_persistent_trigger_inventory(&conn, true).unwrap();
            }
            24 => {
                assert_eq!(report.scored_count_column_added, 1);
                assert!(table_has_column(&conn, "memories", "scored_count").unwrap());
            }
            25 => {
                assert_eq!(report.recall_impression_schema_objects_created, 6);
                assert!(table_has_column(&conn, "recall_impression_groups", "query_hash").unwrap());
            }
            26 => {
                assert_eq!(count(&conn,"SELECT count(*) FROM recall_impression_groups WHERE legacy_query_bucket='seed' AND query_fingerprint IS NULL"),1);
                assert_eq!(report.recall_impression_replay_identity_columns_added, 7);
                crate::db::schema::validate_recall_impression_ledger_schema(&conn).unwrap();
            }
            27 => {
                assert_eq!(report.typo_fallback_attribution_columns_added, 7);
                crate::db::schema::validate_typo_fallback_attribution_schema(&conn).unwrap();
            }
            28 => {
                assert_eq!(report.wiki_recovery_schema_objects_created, 3);
                crate::db::schema::validate_wiki_recovery_ledgers_schema(&conn).unwrap();
            }
            29 => {
                assert_eq!(report.memory_outbox_schema_objects_created, 4);
                crate::db::schema::validate_memory_outbox_schema(&conn).unwrap();
            }
            30 => {
                assert_eq!(
                    report.memory_outbox_destination_apply_schema_objects_created,
                    2
                );
                crate::db::schema::validate_memory_outbox_destination_apply_schema(&conn).unwrap();
            }
            33 => {
                assert_eq!(report.harness_session_attachments_schema_objects_created, 3);
                crate::db::schema::validate_harness_session_attachments_schema(&conn).unwrap();
            }
            34 => {
                assert_eq!(report.harness_session_spine_schema_objects_created, 7);
                crate::db::schema::validate_harness_session_spine_schema(&conn).unwrap();
            }
            35 => {
                assert_eq!(report.harness_session_spine_receipt_tables_rebuilt, 2);
                crate::db::schema::validate_harness_session_spine_schema(&conn).unwrap();
                assert_eq!(count(&conn,"SELECT count(*) FROM harness_session_interventions WHERE capability_source='legacy_unknown'"),1);
            }
            36 => {
                assert_eq!(report.delivery_spine_schema_objects_created, 7);
                crate::db::schema::validate_delivery_spine_schema(&conn).unwrap();
            }
            _ => unreachable!(),
        }
        // Validate each applicable Portable prefix, rather than claim that a
        // v13 component meets today's complete v39 schema admission.
        eprintln!("T1b Portable v{index}: historical prefix BODY effect and applicable validators confirmed");
        if index >= 26 {
            crate::db::schema::validate_recall_impression_ledger_schema(&conn).unwrap();
        }
        if index >= 27 {
            crate::db::schema::validate_typo_fallback_attribution_schema(&conn).unwrap();
        }
        if index >= 28 {
            crate::db::schema::validate_wiki_recovery_ledgers_schema(&conn).unwrap();
        }
        if index >= 29 {
            crate::db::schema::validate_memory_outbox_schema(&conn).unwrap();
        }
        if index >= 30 {
            crate::db::schema::validate_memory_outbox_destination_apply_schema(&conn).unwrap();
        }
        if index >= 33 {
            crate::db::schema::validate_harness_session_attachments_schema(&conn).unwrap();
        }
    }
}

#[test]
fn reconstructed_v36_fixture_contains_only_its_historical_sentinel_prefix() {
    let conn = connection();
    install_historical_portable_v36(&conn);
    populate(&conn);
    assert_eq!(count(&conn, "PRAGMA user_version"), 36);
    assert_eq!(
        count(
            &conn,
            "SELECT count(*) FROM sqlite_schema WHERE name='delivery_intents'"
        ),
        1
    );
    assert_eq!(
        count(
            &conn,
            "SELECT count(*) FROM sqlite_schema WHERE name='current_truth_assertions'"
        ),
        0
    );
}
