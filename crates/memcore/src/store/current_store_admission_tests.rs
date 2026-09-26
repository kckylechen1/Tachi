//! tachi#1995: a store already stamped at the current schema version that is
//! missing an object `init_schema_inner` would recreate must be refused on
//! open, not silently repaired.
//!
//! Every fixture is a real file store built through `MemoryStore::open_with_context`
//! (`CreateFresh`), damaged with raw SQL on a plain `rusqlite::Connection`, and
//! reopened under `OpenExisting + Deny`, the fail-closed default. The two
//! `#[ignore]`d regression tests assert the CORRECT behaviour (the open is
//! refused and nothing in the file changes), so they are red on main until
//! current-store admission covers these objects.
//!
//! The control test is green on main: it proves the "schema and rows are
//! unchanged" comparison is not vacuous, because a healthy current store
//! reopens under `Deny` without changing either.
//!
//! The diagnostic sweep asserts nothing about outcomes. It records, for every
//! schema object in a fresh full-profile store, every evolutionary column that
//! `init_schema_inner` ensures, and a few row-level repair steps, what today's
//! open does when that object is missing or damaged. It is evidence for the
//! #1995 classification spec, not a contract.

use std::path::{Path, PathBuf};

use rusqlite::{params, Connection};
use serde_json::json;

use crate::db::exec_env::{
    get_exec_env_worktree_identity, insert_exec_env, EnvClass, NewExecEnvLease,
};
use crate::db::migrations::{read_schema_version, EXPECTED_SCHEMA_VERSION};
use crate::db::session_claims::{insert_claim, NewSessionClaim};
use crate::db::DbOpenContext;
use crate::{MemoryEntry, MemoryStore};

// ── fixtures ────────────────────────────────────────────────────────────────

fn db_path(dir: &Path, leaf: &str) -> String {
    dir.join(leaf).to_str().expect("utf8 db path").to_string()
}

fn raw(path: &str) -> Connection {
    let conn = Connection::open(path).expect("open raw connection");
    conn.busy_timeout(std::time::Duration::from_secs(5))
        .expect("busy timeout");
    conn
}

/// Build a current-version full-profile store through the real API and return
/// its path. The stamp is asserted so a fixture regression cannot quietly turn
/// these into older-store (migration-gate) tests.
fn fresh_current_store(dir: &Path, leaf: &str) -> String {
    let path = db_path(dir, leaf);
    let store = MemoryStore::open_with_context(&path, &DbOpenContext::create_fresh())
        .expect("create fresh current-version store");
    drop(store);
    let conn = raw(&path);
    assert_eq!(
        read_schema_version(&conn).expect("read stamp"),
        EXPECTED_SCHEMA_VERSION,
        "fixture must be stamped at the current schema version"
    );
    path
}

fn reopen_deny(path: &str) -> Result<(), String> {
    MemoryStore::open_with_context(path, &DbOpenContext::open_existing_deny())
        .map(drop)
        .map_err(|error| error.to_string())
}

type SchemaRow = (String, String, Option<String>);

fn schema_snapshot(conn: &Connection) -> Vec<SchemaRow> {
    let mut stmt = conn
        .prepare("SELECT type, name, sql FROM main.sqlite_schema ORDER BY type, name")
        .expect("prepare schema snapshot");
    stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .expect("query schema snapshot")
        .collect::<Result<Vec<_>, _>>()
        .expect("read schema snapshot")
}

fn schema_names(rows: &[SchemaRow]) -> Vec<String> {
    rows.iter()
        .map(|(kind, name, _)| format!("{kind}:{name}"))
        .collect()
}

fn schema_diff(before: &[SchemaRow], after: &[SchemaRow]) -> (Vec<String>, Vec<String>) {
    let before_names = schema_names(before);
    let after_names = schema_names(after);
    let added = after_names
        .iter()
        .filter(|name| !before_names.contains(name))
        .cloned()
        .collect();
    let removed = before_names
        .iter()
        .filter(|name| !after_names.contains(name))
        .cloned()
        .collect();
    (added, removed)
}

fn object_present(conn: &Connection, kind: &str, name: &str) -> bool {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM main.sqlite_schema WHERE type = ?1 AND name = ?2)",
        params![kind, name],
        |row| row.get(0),
    )
    .expect("probe sqlite_schema")
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ClaimRow {
    claim_id: String,
    state: String,
    mode: Option<String>,
    release_reason: Option<String>,
    released_at: Option<String>,
}

fn claim_rows(conn: &Connection) -> Vec<ClaimRow> {
    let mut stmt = conn
        .prepare(
            "SELECT claim_id, state, mode, release_reason, released_at
             FROM session_claims ORDER BY claim_id",
        )
        .expect("prepare claims");
    stmt.query_map([], |row| {
        Ok(ClaimRow {
            claim_id: row.get(0)?,
            state: row.get(1)?,
            mode: row.get(2)?,
            release_reason: row.get(3)?,
            released_at: row.get(4)?,
        })
    })
    .expect("query claims")
    .collect::<Result<Vec<_>, _>>()
    .expect("read claims")
}

fn legacy_claim(claim_id: &str, created_at: &str) -> NewSessionClaim {
    NewSessionClaim {
        claim_id: claim_id.to_string(),
        session_client: Some("client-1995".to_string()),
        issue_ref: Some("kckylechen1/tachi#1995".to_string()),
        flow_id: None,
        dispatch_id: None,
        branch: "test/current-store-silent-repair-1995".to_string(),
        declared_file_scope: None,
        created_at: created_at.to_string(),
    }
}

// ── control: the comparison below is not vacuous ────────────────────────────

/// A healthy current store reopens under `Deny` and neither its schema nor its
/// claim rows change. This is the baseline the two red tests compare against:
/// if a clean reopen rewrote `sqlite_schema` on its own, "the refused open left
/// the file unchanged" would prove nothing.
#[test]
fn undamaged_current_store_reopens_under_deny_without_changing_schema_or_claims() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = fresh_current_store(dir.path(), "control.db");
    {
        let conn = raw(&path);
        insert_claim(
            &conn,
            &legacy_claim("claim-control", "2026-01-01T00:00:00Z"),
        )
        .expect("seed one legacy claim");
    }
    let (schema_before, claims_before) = {
        let conn = raw(&path);
        (schema_snapshot(&conn), claim_rows(&conn))
    };

    let outcome = reopen_deny(&path);

    let conn = raw(&path);
    assert_eq!(outcome, Ok(()), "a healthy current store must open");
    assert_eq!(schema_snapshot(&conn), schema_before);
    assert_eq!(claim_rows(&conn), claims_before);
}

// ── (a) data-rewriting repair: unique-index dedupe ──────────────────────────

/// `idx_session_claims_identity_active` is a partial UNIQUE index. Rebuilding
/// it on a store holding two active modeless claims for the same identity
/// triple needs `dedupe_session_claims_identity_conflicts` to release one of
/// them first. That rewrite is a legitimate legacy migration step, but on a
/// store that already claims the current version it changes live claim state
/// without any migration authority.
#[test]
#[ignore = "tachi#1995: red until current-store admission refuses damaged state"]
fn current_store_missing_claim_identity_index_with_duplicates_is_refused_not_deduped() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = fresh_current_store(dir.path(), "claims.db");
    {
        let conn = raw(&path);
        conn.execute_batch("DROP INDEX idx_session_claims_identity_active;")
            .expect("damage: drop the partial unique identity index");
        insert_claim(&conn, &legacy_claim("claim-older", "2026-01-01T00:00:00Z"))
            .expect("seed older active modeless claim");
        insert_claim(&conn, &legacy_claim("claim-newer", "2026-02-01T00:00:00Z"))
            .expect("seed newer active modeless claim for the same identity");
    }
    let (schema_before, claims_before) = {
        let conn = raw(&path);
        (schema_snapshot(&conn), claim_rows(&conn))
    };
    assert_eq!(
        claims_before
            .iter()
            .filter(|claim| claim.state == "active" && claim.mode.is_none())
            .count(),
        2,
        "precondition: two active modeless claims share one identity triple"
    );
    assert!(
        !schema_names(&schema_before).contains(&"index:idx_session_claims_identity_active".into()),
        "precondition: the unique identity index is missing"
    );

    let outcome = reopen_deny(&path);

    let conn = raw(&path);
    let schema_after = schema_snapshot(&conn);
    let claims_after = claim_rows(&conn);
    let (added, removed) = schema_diff(&schema_before, &schema_after);
    assert!(
        outcome.is_err() && claims_after == claims_before && schema_after == schema_before,
        "a current-version store missing idx_session_claims_identity_active must be refused \
         under OpenExisting+Deny with no rows or schema changed\n\
         open outcome: {outcome:?}\n\
         claims before: {claims_before:#?}\n\
         claims after:  {claims_after:#?}\n\
         schema objects added: {added:?}\n\
         schema objects removed: {removed:?}"
    );
}

// ── (b) state-bearing table recreated empty ─────────────────────────────────

/// `exec_env_worktree_identities` carries the device/inode identity captured
/// when a worktree lease was published. Nothing else in the store can rebuild
/// it. Recreating it empty on open turns "the table is gone" into "every lease
/// has no recorded identity", which callers cannot tell apart from a lease
/// that never had one.
#[test]
#[ignore = "tachi#1995: red until current-store admission refuses damaged state"]
fn current_store_missing_worktree_identity_table_is_refused_not_recreated_empty() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = fresh_current_store(dir.path(), "exec-envs.db");
    let worktree = dir.path().join("worktree-1995");
    std::fs::create_dir_all(&worktree).expect("worktree dir");
    {
        let store = MemoryStore::open_with_context(&path, &DbOpenContext::open_existing_deny())
            .expect("reopen healthy store");
        insert_exec_env(
            store.connection(),
            &NewExecEnvLease {
                env_id: "env-1995".to_string(),
                kind: "worktree".to_string(),
                path: worktree.to_str().expect("utf8 worktree").to_string(),
                repo_root: dir.path().to_str().expect("utf8 dir").to_string(),
                branch: "test/current-store-silent-repair-1995".to_string(),
                base_sha: "2898c651".to_string(),
                dispatch_id: None,
                env_class: EnvClass::EditOnly,
                created_at: String::new(),
            },
        )
        .expect("publish a worktree lease with a captured identity");
        assert!(
            get_exec_env_worktree_identity(store.connection(), "env-1995")
                .expect("read identity")
                .is_some(),
            "precondition: the lease has a recorded worktree identity"
        );
    }
    {
        let conn = raw(&path);
        let rows: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM exec_env_worktree_identities",
                [],
                |row| row.get(0),
            )
            .expect("count identities");
        assert_eq!(rows, 1, "precondition: one identity row exists");
        conn.execute_batch("DROP TABLE exec_env_worktree_identities;")
            .expect("damage: drop the worktree identity table");
    }
    let schema_before = {
        let conn = raw(&path);
        schema_snapshot(&conn)
    };

    let outcome = reopen_deny(&path);

    let conn = raw(&path);
    let schema_after = schema_snapshot(&conn);
    let (added, removed) = schema_diff(&schema_before, &schema_after);
    let table_after = object_present(&conn, "table", "exec_env_worktree_identities");
    let identity_rows_after: Option<i64> = if table_after {
        Some(
            conn.query_row(
                "SELECT COUNT(*) FROM exec_env_worktree_identities",
                [],
                |row| row.get(0),
            )
            .expect("count identities after"),
        )
    } else {
        None
    };
    let lease_rows_after: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM exec_envs WHERE env_id = 'env-1995'",
            [],
            |row| row.get(0),
        )
        .expect("count leases after");
    assert!(
        outcome.is_err() && schema_after == schema_before,
        "a current-version store missing exec_env_worktree_identities must be refused \
         under OpenExisting+Deny, not have the table recreated\n\
         open outcome: {outcome:?}\n\
         exec_env_worktree_identities present after open: {table_after}\n\
         identity rows after open: {identity_rows_after:?} (1 before the drop)\n\
         exec_envs rows for env-1995 after open: {lease_rows_after}\n\
         schema objects added: {added:?}\n\
         schema objects removed: {removed:?}"
    );
}

// ── diagnostic sweep (asserts nothing about outcomes) ───────────────────────

/// Columns `init_schema_inner` (and `init_product_schema_columns`) re-adds
/// through `ensure_column` when missing, in source order.
const ENSURED_COLUMNS: &[(&str, &str)] = &[
    ("recall_cache", "generation_fingerprint"),
    ("memories", "archived"),
    ("memories", "created_at"),
    ("memories", "updated_at"),
    ("memories", "scored_count"),
    ("memories", "revision"),
    ("memories", "valid_from"),
    ("memories", "valid_until"),
    ("memories", "retention_policy"),
    ("memories", "domain"),
    ("memories", "superseded_by"),
    ("memories", "idless_identity"),
    ("memories", "recall_count"),
    ("memories", "query_diversity"),
    ("memories", "tier"),
    ("memories", "last_use_at"),
    ("access_history", "query_hash"),
    ("access_history", "event_kind"),
    ("memory_edges", "valid_from"),
    ("memory_edges", "valid_to"),
    ("derived_items", "summary"),
    ("derived_items", "importance"),
    ("derived_items", "scope"),
    ("derived_items", "created_at"),
    ("hub_capabilities", "review_status"),
    ("hub_capabilities", "health_status"),
    ("hub_capabilities", "last_error"),
    ("hub_capabilities", "last_success_at"),
    ("hub_capabilities", "last_failure_at"),
    ("hub_capabilities", "fail_streak"),
    ("hub_capabilities", "active_version"),
    ("hub_capabilities", "exposure_mode"),
    ("vault_entries", "allowed_agents"),
    ("exec_envs", "agent_identity_id"),
    ("exec_envs", "claim_id"),
    ("session_claims", "mode"),
];

fn sweep_entry(id: &str) -> MemoryEntry {
    MemoryEntry {
        id: id.into(),
        path: "/scratch/1995".into(),
        summary: "sweep fixture".into(),
        text: "a memory row so row-level repair steps have something to touch".into(),
        importance: 0.7,
        timestamp: "2026-01-01T00:00:00Z".into(),
        valid_from: String::new(),
        valid_until: None,
        category: "fact".into(),
        topic: String::new(),
        keywords: vec!["sweep".into()],
        persons: vec![],
        entities: vec![],
        location: String::new(),
        source: "manual".into(),
        scope: "general".into(),
        archived: false,
        access_count: 0,
        scored_count: 0,
        last_access: None,
        last_use_at: None,
        revision: 1,
        vector: None,
        retention_policy: None,
        domain: None,
        metadata: json!({ "keywords": ["sweep"], "entities": [] }),
        recall_count: 0,
        query_diversity: 0,
        tier: "raw".into(),
    }
}

fn has_column(conn: &Connection, table: &str, column: &str) -> bool {
    let sql = format!("SELECT 1 FROM pragma_table_info('{table}') WHERE name = ?1");
    conn.query_row(&sql, [column], |_| Ok(()))
        .map(|_| true)
        .unwrap_or(false)
}

fn short(text: &str) -> String {
    let one_line = text.replace('\n', " ");
    if one_line.chars().count() > 150 {
        format!("{}…", one_line.chars().take(150).collect::<String>())
    } else {
        one_line
    }
}

enum Damage {
    Drop {
        kind: String,
        name: String,
    },
    DropColumn {
        table: String,
        column: String,
    },
    Rows {
        label: &'static str,
        sql: &'static str,
        probe: &'static str,
    },
}

impl Damage {
    fn label(&self) -> String {
        match self {
            Damage::Drop { kind, name } => format!("drop {kind} {name}"),
            Damage::DropColumn { table, column } => format!("drop column {table}.{column}"),
            Damage::Rows { label, .. } => format!("rows: {label}"),
        }
    }

    fn apply(&self, conn: &Connection) -> Result<(), String> {
        let sql = match self {
            Damage::Drop { kind, name } => format!("DROP {} \"{name}\"", kind.to_uppercase()),
            Damage::DropColumn { table, column } => {
                format!("ALTER TABLE \"{table}\" DROP COLUMN \"{column}\"")
            }
            Damage::Rows { sql, .. } => (*sql).to_string(),
        };
        conn.execute_batch(&sql).map_err(|error| error.to_string())
    }

    fn after(&self, conn: &Connection) -> String {
        match self {
            Damage::Drop { kind, name } => {
                format!("present_after={}", object_present(conn, kind, name))
            }
            Damage::DropColumn { table, column } => {
                format!("present_after={}", has_column(conn, table, column))
            }
            Damage::Rows { probe, .. } => {
                match conn.query_row(probe, [], |row| row.get::<_, Option<String>>(0)) {
                    Ok(value) => format!("probe_after={value:?}"),
                    Err(error) => format!("probe_after=ERR({})", short(&error.to_string())),
                }
            }
        }
    }
}

/// Records today's open outcome for every single-object damage. Run with
/// `--run-ignored only --nocapture`. Outcomes are printed, never asserted.
#[test]
#[ignore = "tachi#1995 diagnostic: prints today's per-object reopen outcome, asserts nothing about it"]
fn diagnostic_sweep_current_store_single_object_damage_outcomes() {
    let dir = tempfile::tempdir().expect("tempdir");
    let template = fresh_current_store(dir.path(), "template.db");
    {
        let mut store =
            MemoryStore::open_with_context(&template, &DbOpenContext::open_existing_deny())
                .expect("reopen template");
        store
            .upsert(&sweep_entry("sweep-memory-1"))
            .expect("seed one memory");
    }
    let objects: Vec<SchemaRow> = {
        let conn = raw(&template);
        conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
            .expect("checkpoint template");
        schema_snapshot(&conn)
    };
    let virtual_tables: Vec<String> = objects
        .iter()
        .filter(|(_, _, sql)| {
            sql.as_deref()
                .is_some_and(|sql| sql.to_uppercase().starts_with("CREATE VIRTUAL TABLE"))
        })
        .map(|(_, name, _)| name.clone())
        .collect();

    let mut cases: Vec<Damage> = objects
        .iter()
        .filter(|(kind, name, _)| {
            matches!(kind.as_str(), "table" | "index" | "trigger")
                && !name.starts_with("sqlite_")
                && !virtual_tables
                    .iter()
                    .any(|vtab| name.starts_with(&format!("{vtab}_")))
        })
        .map(|(kind, name, _)| Damage::Drop {
            kind: kind.clone(),
            name: name.clone(),
        })
        .collect();
    cases.extend(
        ENSURED_COLUMNS
            .iter()
            .map(|(table, column)| Damage::DropColumn {
                table: (*table).to_string(),
                column: (*column).to_string(),
            }),
    );
    cases.extend([
        Damage::Rows {
            label: "memories.created_at blanked (backfill UPDATE, schema.rs:1575)",
            sql: "UPDATE memories SET created_at = '' WHERE id = 'sweep-memory-1'",
            probe: "SELECT created_at FROM memories WHERE id = 'sweep-memory-1'",
        },
        Damage::Rows {
            label: "memories.revision zeroed (backfill UPDATE, schema.rs:1583)",
            sql: "UPDATE memories SET revision = 0 WHERE id = 'sweep-memory-1'",
            probe: "SELECT CAST(revision AS TEXT) FROM memories WHERE id = 'sweep-memory-1'",
        },
        Damage::Rows {
            label: "memories.valid_from blanked (normalize_memory_validity_columns)",
            sql: "UPDATE memories SET valid_from = '' WHERE id = 'sweep-memory-1'",
            probe: "SELECT valid_from FROM memories WHERE id = 'sweep-memory-1'",
        },
        Damage::Rows {
            label: "memories_fts row deleted (ensure_fts_backfilled)",
            sql: "DELETE FROM memories_fts WHERE id = 'sweep-memory-1'",
            probe: "SELECT CAST(COUNT(*) AS TEXT) FROM memories_fts WHERE id = 'sweep-memory-1'",
        },
        Damage::Rows {
            label: "memories_symbolic_fts row deleted (ensure_fts_backfilled)",
            sql: "DELETE FROM memories_symbolic_fts WHERE id = 'sweep-memory-1'",
            probe: "SELECT CAST(COUNT(*) AS TEXT) FROM memories_symbolic_fts WHERE id = 'sweep-memory-1'",
        },
    ]);

    let mut report = Vec::new();
    for (index, damage) in cases.iter().enumerate() {
        let case_path: PathBuf = dir.path().join(format!("case-{index:03}.db"));
        std::fs::copy(&template, &case_path).expect("copy template");
        let case_path = case_path.to_str().expect("utf8 case path").to_string();
        let applied = {
            let conn = raw(&case_path);
            damage.apply(&conn)
        };
        if let Err(error) = applied {
            report.push(format!(
                "SWEEP | {} | fixture-cannot-damage: {}",
                damage.label(),
                short(&error)
            ));
            continue;
        }
        let outcome = match reopen_deny(&case_path) {
            Ok(()) => "open=Ok".to_string(),
            Err(error) => format!("open=Err({})", short(&error)),
        };
        let after = damage.after(&raw(&case_path));
        report.push(format!("SWEEP | {} | {outcome} | {after}", damage.label()));
    }
    for line in &report {
        println!("{line}");
    }
    println!("SWEEP-COUNT cases={}", report.len());
}
