//! Pin tests for the #1585 portable-kernel boundary and the #1579 store
//! identity stamp.
//!
//! Every test here runs against a **persistent** database. That is deliberate:
//! the whole point of both issues is what is written into the file and read
//! back out of it on the next open, and an in-memory store cannot exercise a
//! second open at all.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use rusqlite::Connection;
use serde_json::json;

use crate::db::{DbOpenContext, MigrationAuthority, OpenIntent, StoreProfile};
use crate::error::MemoryError;
use crate::path_router::UNKNOWN_DB_LABEL;
use crate::{GcConfig, MemoryEntry, MemoryStore};

// ── fixtures ────────────────────────────────────────────────────────────────

fn temp_dir(name: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "memcore-profile-{name}-{}-{nanos}",
        std::process::id()
    ));
    std::fs::create_dir_all(&path).expect("temp dir");
    path
}

fn db_in(dir: &Path, leaf: &str) -> String {
    dir.join(leaf).to_str().expect("utf8 db path").to_string()
}

fn ctx(required: StoreProfile, migration: MigrationAuthority) -> DbOpenContext {
    DbOpenContext {
        intent: OpenIntent::OpenExisting,
        migration,
        required_profile: required.into(),
    }
}

fn deny(required: StoreProfile) -> DbOpenContext {
    ctx(required, MigrationAuthority::Deny)
}

fn allow(required: StoreProfile) -> DbOpenContext {
    ctx(
        required,
        MigrationAuthority::Allow {
            approved_by: "test:#1585".to_string(),
        },
    )
}

/// Product tables a `PortableKernel` store must never carry. The four
/// third-bucket tables plus one representative of every deny-list family.
const PRODUCT_TABLES: &[&str] = &[
    "audit_log",
    "agent_known_state",
    "llm_usage",
    "sandbox_rules",
    "sandbox_policies",
    "sandbox_exec_audit",
    "hub_capabilities",
    "vault_entries",
    "vault_key_health",
    // tachi#1680 D1's four provider-account tables. Listed in full rather than
    // by one representative: they arrived as a group in a single commit, and
    // the failure mode this list guards (a chunk tagged Portable so a portable
    // kernel silently grows a product table, or tagged Product so a full store
    // silently lacks one) is per-chunk, not per-family.
    "provider_accounts",
    "provider_account_aliases",
    "provider_account_events",
    "account_custody",
    // tachi#1681 D1's six model-broker catalog tables. Listed in full for the
    // same per-chunk reason as the provider-account group above: the failure
    // mode this list guards is per-chunk, not per-family, and the frozen
    // design's initial "four" was a cross-vendor-review-caught undercount —
    // model_deployment_health's DDL moved into this same PR-A.
    "model_deployments",
    "model_deployment_events",
    "model_aliases",
    "model_alias_bindings",
    // tachi#1681 D2 review (CP4) — the alias group's append-only log.
    "model_alias_events",
    "pricing_snapshots",
    "model_deployment_health",
    "foundry_jobs",
    "exec_envs",
    "exec_env_resources",
    "dispatch_outcomes",
    "dispatch_adjudications",
    "mirror_eval_runs",
    "session_claims",
    "agent_identities",
    "identity_admissions",
    "identity_admission_verification_receipts",
    "identity_admission_verification_revocations",
    "a2a_envelopes",
    "a2a_delivery_receipts",
];

/// Kernel tables every store must carry, whatever its profile.
const KERNEL_TABLES: &[&str] = &[
    "memories",
    "memories_fts",
    "memories_symbolic_fts",
    "memory_edges",
    "edge_observations",
    "hard_state",
    "access_history",
    "derived_items",
    "processed_events",
    "tachi_events",
    "recall_cache",
    "recall_impression_groups",
    "recall_impressions",
    "rem_source_claims",
    "exact_dedupe_apply_lineage",
];

fn raw(db_path: &str) -> Connection {
    Connection::open(db_path).expect("raw open")
}

/// Every object name in `sqlite_schema` — tables, indexes, triggers, views.
/// A set (not a Vec) so subset comparisons mean what they say.
fn schema_object_names(conn: &Connection) -> BTreeSet<String> {
    let mut stmt = conn
        .prepare("SELECT name FROM main.sqlite_schema WHERE name NOT LIKE 'sqlite_%' ORDER BY name")
        .expect("prepare schema query");
    let rows = stmt
        .query_map([], |row| row.get::<_, String>(0))
        .expect("query schema");
    rows.map(|r| r.expect("schema row")).collect()
}

/// The full `(type, name, tbl_name, sql)` tuple set — a byte-level snapshot,
/// used where the assertion is "nothing at all changed".
fn schema_snapshot(conn: &Connection) -> Vec<(String, String, String, Option<String>)> {
    let mut stmt = conn
        .prepare(
            "SELECT type, name, tbl_name, sql FROM main.sqlite_schema \
             WHERE name NOT LIKE 'sqlite_%' ORDER BY type, name",
        )
        .expect("prepare snapshot");
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
            ))
        })
        .expect("query snapshot");
    rows.map(|r| r.expect("snapshot row")).collect()
}

fn table_exists(conn: &Connection, table: &str) -> bool {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM main.sqlite_schema WHERE type = 'table' AND name = ?1)",
        [table],
        |row| row.get::<_, bool>(0),
    )
    .expect("table existence query")
}

fn user_version(conn: &Connection) -> u32 {
    conn.query_row("PRAGMA user_version", [], |row| row.get(0))
        .expect("user_version")
}

fn marked_sentinels(conn: &Connection) -> BTreeSet<String> {
    let mut stmt = conn
        .prepare("SELECT key FROM hard_state WHERE namespace = 'migrations' ORDER BY key")
        .expect("prepare sentinel query");
    let rows = stmt
        .query_map([], |row| row.get::<_, String>(0))
        .expect("query sentinels");
    rows.map(|r| r.expect("sentinel row")).collect()
}

fn identity_stamp(conn: &Connection, key: &str) -> Option<String> {
    conn.query_row(
        "SELECT json_extract(value_json, '$.value') FROM hard_state \
         WHERE namespace = 'store_identity' AND key = ?1",
        [key],
        |row| row.get::<_, String>(0),
    )
    .ok()
}

fn entry(id: &str, path: &str) -> MemoryEntry {
    MemoryEntry {
        id: id.into(),
        path: path.into(),
        summary: "portable smoke".into(),
        text: "portable kernel smoke fact about kernels".into(),
        importance: 0.7,
        timestamp: chrono::Utc::now().to_rfc3339(),
        valid_from: String::new(),
        valid_until: None,
        category: "fact".into(),
        topic: String::new(),
        keywords: vec!["portable".into()],
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
        metadata: json!({ "keywords": ["portable"], "entities": [] }),
        recall_count: 0,
        query_diversity: 0,
        tier: "raw".into(),
    }
}

// ── D7: fresh create ────────────────────────────────────────────────────────

#[test]
fn portable_fresh_create_omits_product_tables() {
    let dir = temp_dir("fresh-portable");
    let path = db_in(&dir, "memory.db");

    let store = MemoryStore::open_with_context(&path, &deny(StoreProfile::PortableKernel))
        .expect("fresh portable create");
    assert_eq!(store.store_profile(), StoreProfile::PortableKernel);
    drop(store);

    let conn = raw(&path);
    for table in PRODUCT_TABLES {
        assert!(
            !table_exists(&conn, table),
            "portable store must not create product table {table}"
        );
    }
    for table in KERNEL_TABLES {
        assert!(
            table_exists(&conn, table),
            "portable store is missing kernel table {table}"
        );
    }
    assert_eq!(
        identity_stamp(&conn, "profile").as_deref(),
        Some("portable_kernel"),
        "fresh portable create must stamp its profile"
    );
    assert_eq!(
        user_version(&conn),
        crate::db::migrations::EXPECTED_SCHEMA_VERSION,
        "a portable store is a complete stamped-28 database"
    );
}

#[test]
fn production_open_authorizer_migrates_v36_verified_admission_inventory() {
    let dir = temp_dir("v37-production-authorizer");
    let path = db_in(&dir, "memory.db");
    drop(
        MemoryStore::open_with_context(&path, &deny(StoreProfile::TachiFull))
            .expect("fresh full create"),
    );
    {
        let conn = raw(&path);
        conn.execute_batch(
            "DROP TABLE identity_admission_verification_revocations;
             DROP TABLE identity_admission_verification_receipts;
             DROP INDEX idx_identity_admissions_verified_binding;
             DROP TRIGGER identity_verified_admissions_no_replace;
             DROP TRIGGER identity_verified_admissions_no_update;
             DROP TRIGGER identity_verified_admissions_no_delete;
             DELETE FROM hard_state
              WHERE namespace='migrations' AND key='v37_verified_agent_admissions';
             PRAGMA user_version=36;",
        )
        .expect("construct a stamped v36 product store");
    }

    drop(
        MemoryStore::open_with_context(&path, &allow(StoreProfile::TachiFull))
            .expect("production authorizer must admit the complete canonical v37 migration"),
    );
    let reopened = MemoryStore::open_with_context(&path, &deny(StoreProfile::TachiFull))
        .expect("current production inventory must reopen without migration authority");
    let trigger_count: i64 = reopened
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM sqlite_schema WHERE type='trigger' AND (
                 name LIKE 'identity_verification_receipts_%'
                 OR name LIKE 'identity_verified_admissions_%'
                 OR name LIKE 'identity_verification_revocations_%'
             )",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(trigger_count, 9);
}

#[test]
fn production_reopen_rejects_altered_identity_admission_conflict_target() {
    let dir = temp_dir("v37-altered-conflict-target");
    let path = db_in(&dir, "memory.db");
    drop(
        MemoryStore::open_with_context(&path, &deny(StoreProfile::TachiFull))
            .expect("fresh full create"),
    );
    {
        let conn = raw(&path);
        conn.execute_batch(
            "PRAGMA foreign_keys=OFF;
             DROP TABLE identity_admission_verification_revocations;
             DROP TABLE identity_admission_verification_receipts;
             DROP INDEX idx_identity_admissions_verified_binding;
             DROP TRIGGER identity_verified_admissions_no_replace;
             DROP TRIGGER identity_verified_admissions_no_update;
             DROP TRIGGER identity_verified_admissions_no_delete;
             CREATE TABLE identity_admissions_altered (
                 admission_id TEXT PRIMARY KEY,
                 agent_identity_id TEXT,
                 connection_id TEXT NOT NULL,
                 state TEXT NOT NULL CHECK (
                     state IN ('self_asserted', 'verified', 'rejected', 'unavailable')
                 ),
                 rejection_evidence TEXT,
                 created_at TEXT NOT NULL DEFAULT '',
                 UNIQUE(created_at)
             );
             INSERT INTO identity_admissions_altered
                 (admission_id, agent_identity_id, connection_id, state,
                  rejection_evidence, created_at)
                 SELECT admission_id, agent_identity_id, connection_id, state,
                        rejection_evidence, created_at
                 FROM identity_admissions;
             DROP TABLE identity_admissions;
             ALTER TABLE identity_admissions_altered RENAME TO identity_admissions;
             CREATE INDEX idx_identity_admissions_connection
                 ON identity_admissions(connection_id);",
        )
        .expect("replace the canonical historical conflict target offline");
        crate::db::install_verified_admission_schema(&conn)
            .expect("restore every canonical v37 object around the altered historical table");
    }

    let error = MemoryStore::open_with_context(&path, &deny(StoreProfile::TachiFull))
        .err()
        .expect("current production reopen must reject an altered conflict target");
    assert!(
        error
            .to_string()
            .contains("unexpected unique conflict targets"),
        "{error}"
    );
}

#[test]
fn full_tachi_refuses_portable_store() {
    let dir = temp_dir("refuse-portable");
    let path = db_in(&dir, "memory.db");
    drop(
        MemoryStore::open_with_context(&path, &deny(StoreProfile::PortableKernel))
            .expect("fresh portable create"),
    );

    let before = schema_snapshot(&raw(&path));

    let err = MemoryStore::open_with_context(&path, &deny(StoreProfile::TachiFull))
        .err()
        .expect("a full-profile caller must refuse a portable store");
    match &err {
        MemoryError::StoreProfileMismatch {
            required, stored, ..
        } => {
            assert_eq!(required, "tachi_full");
            assert_eq!(stored, "portable_kernel");
        }
        other => panic!("expected StoreProfileMismatch, got {other:?}"),
    }

    assert_eq!(
        before,
        schema_snapshot(&raw(&path)),
        "a refused open must not have touched sqlite_schema"
    );
}

// ── D7: THE load-bearing one ────────────────────────────────────────────────

/// The single most damaging failure mode in this design: a portable opener
/// holding migration authority must migrate a full store **as full**. If the
/// effective profile were the REQUIRED one, `v18_dispatch_adjudications` would
/// early-return, its sentinel would still be marked, and the database would
/// come back "complete" with its product tables silently missing.
///
/// The fixture rolls a real full store back to schema 27 and additionally
/// un-runs a PRODUCT migration (v18: drop its tables, clear its sentinel), so
/// the migration walk has actual product work to do.
#[test]
fn portable_opens_full_store_and_migrates_as_full() {
    let dir = temp_dir("portable-opens-full");
    let path = db_in(&dir, "memory.db");
    drop(
        MemoryStore::open_with_context(&path, &deny(StoreProfile::TachiFull))
            .expect("fresh full create"),
    );

    {
        let conn = raw(&path);
        conn.execute_batch(
            "DROP TABLE IF EXISTS dispatch_adjudication_signatures;
             DROP TABLE IF EXISTS dispatch_adjudications;
             DROP TABLE IF EXISTS rem_source_claims;
             DROP TABLE IF EXISTS exact_dedupe_apply_lineage;
             DELETE FROM hard_state
               WHERE namespace = 'migrations'
                 AND key IN ('v18_dispatch_adjudications', 'v28_wiki_recovery_ledgers');
             PRAGMA user_version = 27;",
        )
        .expect("roll the fixture back to a schema-27 full store");
        assert!(!table_exists(&conn, "dispatch_adjudications"));
    }

    let store = MemoryStore::open_with_context(&path, &allow(StoreProfile::PortableKernel))
        .expect("portable opener with authority must open and migrate a full store");
    assert_eq!(
        store.store_profile(),
        StoreProfile::TachiFull,
        "effective profile must be the STORED one, never the required one"
    );
    drop(store);

    let conn = raw(&path);
    assert!(
        table_exists(&conn, "dispatch_adjudications"),
        "the product migration must have run: effective profile was stored (TachiFull)"
    );
    assert!(
        table_exists(&conn, "dispatch_adjudication_signatures"),
        "v18's second table must be back too"
    );
    assert!(
        table_exists(&conn, "rem_source_claims"),
        "the portable v28 migration must have run as well"
    );
    assert_eq!(
        user_version(&conn),
        crate::db::migrations::EXPECTED_SCHEMA_VERSION
    );
    let sentinels = marked_sentinels(&conn);
    for key in crate::db::migrations::MIGRATION_SENTINEL_KEYS {
        assert!(sentinels.contains(*key), "sentinel {key} was not marked");
    }
    assert_eq!(
        identity_stamp(&conn, "profile").as_deref(),
        Some("tachi_full"),
        "the stamp must not have been rewritten by the portable opener"
    );
}

#[test]
fn full_store_full_opener_schema_unchanged() {
    let dir = temp_dir("full-idempotent");
    let path = db_in(&dir, "memory.db");
    drop(
        MemoryStore::open_with_context(&path, &deny(StoreProfile::TachiFull))
            .expect("fresh full create"),
    );
    let after_create = schema_snapshot(&raw(&path));

    drop(
        MemoryStore::open_with_context(&path, &deny(StoreProfile::TachiFull))
            .expect("reopen full store"),
    );
    assert_eq!(
        after_create,
        schema_snapshot(&raw(&path)),
        "reopening a full store with a full opener must leave sqlite_schema identical"
    );

    let conn = raw(&path);
    for table in PRODUCT_TABLES.iter().chain(KERNEL_TABLES.iter()) {
        assert!(
            table_exists(&conn, table),
            "full store is missing {table} after the D3 chunking"
        );
    }
}

// ── D7: the nine live databases ─────────────────────────────────────────────

#[test]
fn unstamped_live_db_adopts_tachi_full_without_version_bump() {
    let dir = temp_dir("adopt-unstamped");
    let path = db_in(&dir, "memory.db");
    drop(
        MemoryStore::open_with_context(&path, &deny(StoreProfile::TachiFull))
            .expect("fresh full create"),
    );

    // Simulate a pre-#1585 database: complete, stamped 28, no identity rows.
    {
        let conn = raw(&path);
        conn.execute(
            "DELETE FROM hard_state WHERE namespace = 'store_identity'",
            [],
        )
        .expect("strip the identity stamps");
        assert_eq!(identity_stamp(&conn, "profile"), None);
    }
    let (version_before, sentinels_before) = {
        let conn = raw(&path);
        (user_version(&conn), marked_sentinels(&conn))
    };

    let store = MemoryStore::open_with_context(&path, &deny(StoreProfile::TachiFull))
        .expect("an unstamped live database must be adopted, not refused");
    assert_eq!(store.store_profile(), StoreProfile::TachiFull);
    drop(store);

    let conn = raw(&path);
    assert_eq!(
        identity_stamp(&conn, "profile").as_deref(),
        Some("tachi_full"),
        "adoption must write the profile row"
    );
    assert_eq!(
        user_version(&conn),
        version_before,
        "adoption must not bump the schema version"
    );
    assert_eq!(
        marked_sentinels(&conn),
        sentinels_before,
        "adoption must not change the sentinel set"
    );
}

#[test]
fn portable_refuses_unstamped_store() {
    let dir = temp_dir("portable-refuses-unstamped");
    let path = db_in(&dir, "memory.db");
    drop(
        MemoryStore::open_with_context(&path, &deny(StoreProfile::TachiFull))
            .expect("fresh full create"),
    );
    {
        let conn = raw(&path);
        conn.execute(
            "DELETE FROM hard_state WHERE namespace = 'store_identity'",
            [],
        )
        .expect("strip the identity stamps");
    }

    let err = MemoryStore::open_with_context(&path, &deny(StoreProfile::PortableKernel))
        .err()
        .expect("a portable caller must not adopt an unstamped (pre-#1585, full) store");
    assert!(
        matches!(err, MemoryError::StoreProfileUnstamped { .. }),
        "expected StoreProfileUnstamped, got {err:?}"
    );
}

// ── D7: identity (#1579) ────────────────────────────────────────────────────

#[test]
fn wiki_identity_survives_relocation() {
    let origin = temp_dir("wiki-origin");
    let elsewhere = temp_dir("wiki-elsewhere");
    let wiki_dir = origin.join("wiki");
    std::fs::create_dir_all(&wiki_dir).expect("wiki dir");
    let original = db_in(&wiki_dir, "memory.db");

    let store =
        MemoryStore::open_with_label_and_context(&original, "wiki", &deny(StoreProfile::TachiFull))
            .expect("create the wiki corpus store");
    assert!(store.is_wiki_corpus_store());
    drop(store);

    // Move the file into a directory whose name says nothing about wiki.
    let relocated_dir = elsewhere.join("some-random-project");
    std::fs::create_dir_all(&relocated_dir).expect("relocated dir");
    let relocated = db_in(&relocated_dir, "memory.db");
    std::fs::rename(&original, &relocated).expect("relocate the database file");

    // Open it with NO claim at all: identity must come from the file.
    let moved = MemoryStore::open_with_context(&relocated, &deny(StoreProfile::TachiFull))
        .expect("open the relocated wiki store");
    assert_eq!(
        moved.db_label(),
        "wiki",
        "the role stamp travels inside the file"
    );
    assert!(
        moved.is_wiki_corpus_store(),
        "relocation must not strip wiki authority"
    );
}

#[test]
fn directory_name_confers_no_wiki_authority() {
    // A database that has never been told it is the wiki corpus, sitting in a
    // directory called `wiki`. Before #1579 the parent directory's name made
    // it one.
    let root = temp_dir("impostor-wiki");
    let wiki_dir = root.join("wiki");
    std::fs::create_dir_all(&wiki_dir).expect("wiki dir");
    let path = db_in(&wiki_dir, "memory.db");

    let store = MemoryStore::open_with_context(&path, &deny(StoreProfile::TachiFull))
        .expect("create an unlabelled store under a `wiki` directory");
    assert_eq!(
        store.db_label(),
        UNKNOWN_DB_LABEL,
        "a directory name is evidence about a directory, not about a database"
    );
    assert!(!store.is_wiki_corpus_store());
    drop(store);

    let conn = raw(&path);
    assert_eq!(
        identity_stamp(&conn, "role"),
        None,
        "an unlabelled open must confer nothing"
    );
}

/// Legacy stamp spelling: live project DBs were stamped `project:<name>` by
/// the foundry scheduler's write-open, which used the manifest `scope_hint`
/// display text as the store label. The named-project doors claim the bare
/// `<name>`. Both spellings describe the SAME manifest-resolved project, so
/// the bare claim must open the store (read AND write) with the stamp kept
/// authoritative — while a claim naming any other project, including a
/// different Plan-C hash suffix, must keep refusing.
#[test]
fn legacy_project_scope_stamp_opens_under_bare_name_claim() {
    let dir = temp_dir("legacy-scope-stamp");
    let path = db_in(&dir, "memory.db");
    // The exact poisoning door: a write-open whose label is scope_hint text.
    drop(
        MemoryStore::open_with_label_and_context(
            &path,
            "project:legacy-proj",
            &deny(StoreProfile::TachiFull),
        )
        .expect("stamp the store the way the foundry scheduler did"),
    );

    // Read door: `with_named_project_store_read` claims the bare name.
    let read = MemoryStore::open_read_only_with_label(&path, "legacy-proj")
        .expect("bare project claim must read a project:-prefixed stamp");
    assert_eq!(
        read.db_label(),
        "project:legacy-proj",
        "the stored spelling stays authoritative; the claim is not rewritten into the stamp"
    );
    drop(read);

    // Write door: `with_named_project_store` claims the same bare name and
    // must agree with the read door on one identity.
    let write = MemoryStore::open_with_label_and_context(
        &path,
        "legacy-proj",
        &allow(StoreProfile::TachiFull),
    )
    .expect("bare project claim must also open for write");
    assert_eq!(write.db_label(), "project:legacy-proj");
    drop(write);

    // The stamp is untouched by the compatible opens: still one authority.
    let conn = raw(&path);
    assert_eq!(
        identity_stamp(&conn, "role").as_deref(),
        Some("project:legacy-proj")
    );

    // A different project identity — including the different-hash shape the
    // 2026-09 live audit found (yaya-20994… vs project:yaya-1489…) — is NOT
    // the same project and must keep refusing.
    let err = MemoryStore::open_read_only_with_label(&path, "legacy-other")
        .err()
        .expect("a different project name must still conflict");
    assert!(
        matches!(err, MemoryError::StoreRoleConflict { .. }),
        "expected StoreRoleConflict, got {err:?}"
    );
    let err = MemoryStore::open_read_only_with_label(&path, "yaya-20994e76035f4528deda42ce")
        .err()
        .expect("a different Plan-C hash identity must still conflict");
    assert!(
        matches!(err, MemoryError::StoreRoleConflict { .. }),
        "expected StoreRoleConflict, got {err:?}"
    );

    // The exact live 2026-09 different-hash shape: a `project:`-prefixed
    // stamp of one hash is NOT satisfiable by the bare claim of another.
    let dir_b = temp_dir("legacy-scope-stamp-other-hash");
    let path_b = db_in(&dir_b, "memory.db");
    drop(
        MemoryStore::open_with_label_and_context(
            &path_b,
            "project:yaya-14890056",
            &deny(StoreProfile::TachiFull),
        )
        .expect("stamp a different-hash project role"),
    );
    let err = MemoryStore::open_read_only_with_label(&path_b, "yaya-20994e76035f4528deda42ce")
        .err()
        .expect("different Plan-C hash identities are different projects");
    assert!(
        matches!(err, MemoryError::StoreRoleConflict { .. }),
        "expected StoreRoleConflict, got {err:?}"
    );
}

#[test]
fn conflicting_role_claim_is_refused_not_resolved() {
    let dir = temp_dir("role-conflict");
    let path = db_in(&dir, "memory.db");
    drop(
        MemoryStore::open_with_label_and_context(&path, "wiki", &deny(StoreProfile::TachiFull))
            .expect("stamp the store as wiki"),
    );

    let err =
        MemoryStore::open_with_label_and_context(&path, "global", &deny(StoreProfile::TachiFull))
            .err()
            .expect("a claim that disagrees with the stamp must refuse, not win or lose silently");
    match &err {
        MemoryError::StoreRoleConflict {
            claimed, stored, ..
        } => {
            assert_eq!(claimed, "global");
            assert_eq!(stored, "wiki");
        }
        other => panic!("expected StoreRoleConflict, got {other:?}"),
    }

    // And the stamp is untouched: the refusal is not a re-negotiation.
    let conn = raw(&path);
    assert_eq!(identity_stamp(&conn, "role").as_deref(), Some("wiki"));
}

#[test]
fn store_identity_namespace_is_write_once_at_the_api() {
    let dir = temp_dir("write-once");
    let path = db_in(&dir, "memory.db");
    let store =
        MemoryStore::open_with_label_and_context(&path, "wiki", &deny(StoreProfile::TachiFull))
            .expect("stamp the store as wiki");

    let overwrite =
        crate::db::set_state(store.connection(), "store_identity", "role", "\"global\"")
            .expect_err("set_state must refuse the store_identity namespace");
    assert!(
        overwrite.to_string().contains("write-once"),
        "refusal must name the reason: {overwrite}"
    );
    let removal = crate::db::delete_state(store.connection(), "store_identity", "role")
        .expect_err("delete_state must refuse the store_identity namespace");
    assert!(removal.to_string().contains("write-once"), "{removal}");

    // CAS (set_state_if_version) must refuse the namespace too, not just the
    // unconditional set_state/delete_state pair (kckylechen1/Sigil#1585 review).
    let cas = crate::db::set_state_if_version(
        store.connection(),
        "store_identity",
        "role",
        "\"global\"",
        1,
    )
    .expect_err("set_state_if_version must refuse the store_identity namespace");
    assert!(cas.to_string().contains("write-once"), "{cas}");

    // The backfill must skip store_identity rows outright rather than stamp
    // them with an expires_at that would hand the reaper an unstamp path.
    let backfilled = crate::db::backfill_missing_expires_at(
        store.connection(),
        "store_identity",
        "2999-01-01T00:00:00Z",
        None,
    )
    .expect("backfill call against store_identity must not error, just no-op");
    assert_eq!(
        backfilled, 0,
        "backfill_missing_expires_at must not touch store_identity rows"
    );
    let (role_value, _) = crate::db::get_state(store.connection(), "store_identity", "role")
        .expect("get role stamp")
        .expect("role stamp exists");
    assert!(
        !role_value.contains("expires_at"),
        "store_identity rows must never acquire expires_at, even when backfill is asked: {role_value}"
    );

    // Ordinary namespaces are unaffected.
    store
        .set_state("scratch", "k", "\"v\"")
        .expect("ordinary set_state");
}

/// The write-once guard on `set_state`/`delete_state`/`set_state_if_version`
/// (above) is only half the API surface: `MemoryStore::insert_state_if_absent`
/// is a *forgery* vector rather than an overwrite one — a bare `&MemoryStore`
/// holder could otherwise stamp a brand-new `store_identity` key that the
/// schema-init transaction never wrote, since `ON CONFLICT DO NOTHING` would
/// happily succeed against an absent key. `crate::db::state::insert_state_if_absent`
/// itself is deliberately unguarded (it is the exact primitive the legitimate
/// stamp writer calls), so the refusal must live on the pub
/// `MemoryStore::insert_state_if_absent` wrapper — this pins that it does.
#[test]
fn store_identity_pub_wrapper_refuses_forged_insert() {
    let dir = temp_dir("write-once-forged-insert");
    let path = db_in(&dir, "memory.db");
    let store =
        MemoryStore::open_with_label_and_context(&path, "wiki", &deny(StoreProfile::TachiFull))
            .expect("stamp the store as wiki");

    // A key the schema-init transaction never wrote: if the pub wrapper had no
    // guard, `ON CONFLICT DO NOTHING` would find no existing row and this
    // would silently succeed, planting a forged stamp.
    let forged = store
        .insert_state_if_absent("store_identity", "not_a_real_stamp", "\"forged\"")
        .expect_err("the pub insert_state_if_absent wrapper must refuse the namespace typed");
    assert!(
        forged.to_string().contains("write-once"),
        "refusal must name the reason: {forged}"
    );

    // Confirm it is a real refusal, not an incidental error: nothing was
    // written under that key.
    let conn = raw(&path);
    assert_eq!(
        identity_stamp(&conn, "not_a_real_stamp"),
        None,
        "a refused insert must not have planted the forged row"
    );

    // Ordinary namespaces are unaffected by this wrapper either.
    assert!(store
        .insert_state_if_absent("scratch", "k2", "\"v\"")
        .expect("ordinary insert_state_if_absent"));
}

/// Belt-and-braces exclusion pin (`db::reap_expired_state`'s fifth exclusion,
/// #1579/#1585 review round 2): a `store_identity` row is never written with
/// an `expires_at` field by the real stamp writer, but this test simulates
/// **pre-guard damage** — a row that acquired one anyway, by hand-inserting
/// through a raw, unguarded second connection to the same file (the sanctioned
/// fixture-only escape hatch documented on `MemoryStore::connection`) — and
/// asserts the generic, cross-namespace reaper still will not delete it, while
/// an ordinary namespace's equally-expired row is removed in the same call.
#[test]
fn store_identity_row_with_expires_at_survives_reap() {
    let dir = temp_dir("write-once-survives-reap");
    let path = db_in(&dir, "memory.db");
    let store =
        MemoryStore::open_with_label_and_context(&path, "wiki", &deny(StoreProfile::TachiFull))
            .expect("stamp the store as wiki");

    let past = "2000-01-01T00:00:00Z";
    {
        // Simulated pre-guard damage: a store_identity row carrying
        // expires_at, planted directly via raw SQL (never through the typed
        // API, which refuses this namespace outright).
        let conn = raw(&path);
        conn.execute(
            "INSERT INTO hard_state (namespace, key, value_json, version, created_at, updated_at)
             VALUES ('store_identity', 'damaged_stamp', ?1, 1, ?2, ?2)",
            rusqlite::params![
                format!(r#"{{"value":"forged","expires_at":"{past}"}}"#),
                past,
            ],
        )
        .expect("hand-insert the damaged store_identity row");
        // Positive control: an ordinary namespace's equally-expired row must
        // still be reapable, so this test cannot pass vacuously (e.g. because
        // reap_expired_state stopped removing anything at all).
        conn.execute(
            "INSERT INTO hard_state (namespace, key, value_json, version, created_at, updated_at)
             VALUES ('scratch', 'stale_key', ?1, 1, ?2, ?2)",
            rusqlite::params![format!(r#"{{"value":"v","expires_at":"{past}"}}"#), past,],
        )
        .expect("hand-insert the ordinary expired row");
    }

    let now = chrono::Utc::now().to_rfc3339();
    let removed = store
        .reap_expired_state(&now)
        .expect("reap_expired_state must not error on the damaged row");
    assert_eq!(
        removed, 1,
        "exactly the ordinary expired row must be removed, not the store_identity one"
    );

    let conn = raw(&path);
    assert_eq!(
        identity_stamp(&conn, "damaged_stamp").as_deref(),
        Some("forged"),
        "a store_identity row must survive reap_expired_state even with expires_at set"
    );
    assert_eq!(
        crate::db::get_state(&conn, "scratch", "stale_key").expect("get_state"),
        None,
        "the ordinary namespace's equally-expired row must have been reaped"
    );
}

// ── D7: profile-invariant completeness ──────────────────────────────────────

#[test]
fn both_profiles_agree_on_sentinels_and_portable_schema_is_a_strict_subset() {
    let dir = temp_dir("profile-parity");
    let portable_path = db_in(&dir, "portable.db");
    let full_path = db_in(&dir, "full.db");
    drop(
        MemoryStore::open_with_context(&portable_path, &deny(StoreProfile::PortableKernel))
            .expect("fresh portable create"),
    );
    drop(
        MemoryStore::open_with_context(&full_path, &deny(StoreProfile::TachiFull))
            .expect("fresh full create"),
    );

    let portable = raw(&portable_path);
    let full = raw(&full_path);

    // 1. The sentinel set is profile-INVARIANT. This is what makes a portable
    //    database "complete" to `validate_current_schema_integrity`.
    let expected: BTreeSet<String> = crate::db::migrations::MIGRATION_SENTINEL_KEYS
        .iter()
        .map(|k| (*k).to_string())
        .collect();
    assert_eq!(marked_sentinels(&portable), expected);
    assert_eq!(marked_sentinels(&full), expected);
    assert_eq!(user_version(&portable), user_version(&full));
    assert_eq!(
        user_version(&portable),
        crate::db::migrations::EXPECTED_SCHEMA_VERSION,
        "both profiles carry the current v32 stamp even though PortableKernel owns no A2A tables"
    );

    // 2. The portable schema is a STRICT subset of the full one — subset (no
    //    object a full store lacks) and strict (the product objects really are
    //    absent, so this is not a vacuous pass).
    let portable_objects = schema_object_names(&portable);
    let full_objects = schema_object_names(&full);
    assert!(
        portable_objects.is_subset(&full_objects),
        "portable-only objects: {:?}",
        portable_objects
            .difference(&full_objects)
            .collect::<Vec<_>>()
    );
    assert!(
        portable_objects.len() < full_objects.len(),
        "portable schema must be strictly smaller than full"
    );
    for table in PRODUCT_TABLES {
        assert!(full_objects.contains(*table), "full store lacks {table}");
        assert!(
            !portable_objects.contains(*table),
            "portable store carries product table {table}"
        );
    }
    for table in KERNEL_TABLES {
        assert!(portable_objects.contains(*table), "portable lacks {table}");
    }
}

// ── D7: runtime smoke — catches a misclassified table ───────────────────────

/// The classification in D4 is a judgement call per table, and a wrong call
/// shows up as `no such table` at runtime, not at open. Drive the portable
/// surface that touches the most tables — CRUD, GC, and truth maintenance —
/// against a real persistent portable database.
#[test]
fn portable_store_survives_crud_gc_and_maintenance() {
    let dir = temp_dir("portable-smoke");
    let path = db_in(&dir, "memory.db");
    let mut store = MemoryStore::open_with_context(&path, &deny(StoreProfile::PortableKernel))
        .expect("fresh portable create");
    assert_eq!(store.store_profile(), StoreProfile::PortableKernel);

    // CRUD
    store
        .upsert(&entry("smoke-1", "/scratch/smoke"))
        .expect("upsert");
    store
        .upsert(&entry("smoke-2", "/scratch/smoke"))
        .expect("upsert 2");
    // #1599: the atomic batch upsert is ungated, so it must be callable here
    // too — this is the assertion that it introduces no admin dependency.
    store
        .upsert_batch(&[
            entry("smoke-3", "/scratch/smoke"),
            entry("smoke-4", "/scratch/smoke"),
        ])
        .expect("upsert_batch on a PortableKernel store");
    assert!(store.get("smoke-3").expect("get").is_some());
    assert!(store.get("smoke-4").expect("get").is_some());
    store
        .upsert_batch(&[])
        .expect("empty upsert_batch is a successful no-op");
    assert!(store.get("smoke-1").expect("get").is_some());
    // Search is exercised for its table reach (FTS + symbolic FTS + recall
    // cache + access_history), not for relevance — asserting a hit here would
    // couple this smoke to scorer thresholds that have nothing to do with the
    // profile boundary.
    store.search("portable", None).expect("search");
    let stats = store.stats(true).expect("stats");
    assert!(stats.total >= 2);

    // Events + derived: portable receipt sinks.
    store
        .list_tachi_events(&crate::TachiEventQuery::default())
        .expect("tachi_events read");

    // GC — the audit_log / agent_known_state prunes must be skipped, not fail.
    let gc = store.gc_tables(&GcConfig::default()).expect("gc_tables");
    assert_eq!(
        gc.get("audit_log_pruned").and_then(|v| v.as_u64()),
        Some(0),
        "a portable store reports zero audit_log prunes rather than lying or crashing"
    );
    assert_eq!(
        gc.get("agent_known_state_pruned").and_then(|v| v.as_u64()),
        Some(0)
    );

    // Maintenance sweeps.
    store
        .archive_stale_memories(3650)
        .expect("archive_stale_memories");
    store
        .archive_stale_low_value_memories()
        .expect("archive_stale_low_value_memories");

    // Delete — its agent_known_state cascade is product-guarded.
    assert!(store.delete("smoke-2").expect("delete"));
    assert!(store.get("smoke-2").expect("get after delete").is_none());

    // And the store still reopens cleanly as portable.
    drop(store);
    let reopened = MemoryStore::open_with_context(&path, &deny(StoreProfile::PortableKernel))
        .expect("reopen portable store");
    assert_eq!(reopened.store_profile(), StoreProfile::PortableKernel);
}

/// #1585 integration hardening: `agent_known_state` is a product table, so
/// the store wrappers must refuse typed on a `PortableKernel` store instead
/// of surfacing `no such table` mid-operation.
#[test]
fn portable_store_refuses_agent_state_wrappers_typed() {
    let dir = temp_dir("portable-agent-state");
    let path = db_in(&dir, "memory.db");

    let store = MemoryStore::open_with_context(&path, &deny(StoreProfile::PortableKernel))
        .expect("fresh portable create");

    let write = store.update_agent_known_state("agent-x", &[("m-1".to_string(), 1)]);
    assert!(
        matches!(write, Err(MemoryError::StoreProfileMismatch { .. })),
        "portable agent-state write must refuse typed, got {write:?}"
    );

    let read = store.get_agent_known_revisions("agent-x", &["m-1".to_string()]);
    assert!(
        matches!(read, Err(MemoryError::StoreProfileMismatch { .. })),
        "portable agent-state read must refuse typed, got {read:?}"
    );
}

// ── W1-2: exact-profile admission ───────────────────────────────────────────

fn exact(required: StoreProfile) -> DbOpenContext {
    DbOpenContext::open_existing_deny().with_exact_profile(required)
}

fn file_sha256(path: &str) -> String {
    use sha2::{Digest, Sha256};
    format!(
        "{:x}",
        Sha256::digest(std::fs::read(path).expect("read db bytes"))
    )
}

/// Every `hard_state` row, verbatim: identity stamps, migration sentinels and
/// everything else. A refused open must leave all of it untouched.
fn hard_state_rows(conn: &Connection) -> Vec<(String, String, String)> {
    let mut stmt = conn
        .prepare("SELECT namespace, key, value_json FROM hard_state ORDER BY namespace, key")
        .expect("prepare hard_state dump");
    let rows = stmt
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .expect("query hard_state");
    rows.map(|r| r.expect("hard_state row")).collect()
}

/// Every file next to the database, by name, INCLUDING SQLite's `-wal`/`-shm`
/// sidecars. The clean-store test starts from a cleanly closed store (the last
/// close removed both sidecars), so a sidecar left behind would show up. The
/// unclean-shutdown test is where sidecars legitimately change, and it says so.
fn sibling_names(dir: &Path) -> BTreeSet<String> {
    std::fs::read_dir(dir)
        .expect("read dir")
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect()
}

/// The hazard W1-2 closes, in one test. An `Exact(PortableKernel)` caller
/// opening a full Tachi store must refuse ahead of the migration backup and
/// the connection PRAGMAs, with no memcore DDL, stamp or role write. For a
/// cleanly closed store that also means byte-identical files, no role stamp,
/// no `.migration-bak` and no new sibling (the unclean-shutdown variant below
/// covers what is promised when a WAL is left behind).
///
/// The marker is deleted first, so this open is one the fingerprint heuristic
/// WOULD back up. Only the admission preflight's position ahead of the backup
/// keeps the backup from being written.
#[test]
fn exact_portable_refuses_full_store_and_writes_nothing_to_a_clean_store() {
    let dir = temp_dir("exact-refuses-full");
    let path = db_in(&dir, "memory.db");
    drop(
        MemoryStore::open_with_context(&path, &deny(StoreProfile::TachiFull))
            .expect("fresh full create"),
    );
    std::fs::remove_file(format!("{path}.migration-marker")).expect("drop marker");

    let (before_hash, before_version, before_rows) = {
        let conn = raw(&path);
        (
            file_sha256(&path),
            user_version(&conn),
            hard_state_rows(&conn),
        )
    };
    assert_eq!(
        identity_stamp(&raw(&path), "profile").as_deref(),
        Some("tachi_full")
    );
    assert_eq!(identity_stamp(&raw(&path), "role"), None);
    let before_siblings = sibling_names(&dir);

    let err = MemoryStore::open_with_label_and_context(
        &path,
        "global",
        &exact(StoreProfile::PortableKernel),
    )
    .err()
    .expect("an exact-portable caller must refuse a full store");
    match &err {
        MemoryError::StoreProfileNotExact {
            required, stored, ..
        } => {
            assert_eq!(required, "portable_kernel");
            assert_eq!(stored, "tachi_full");
        }
        other => panic!("expected StoreProfileNotExact, got {other:?}"),
    }

    let conn = raw(&path);
    assert_eq!(
        user_version(&conn),
        before_version,
        "PRAGMA user_version moved"
    );
    assert_eq!(hard_state_rows(&conn), before_rows, "hard_state rows moved");
    assert_eq!(
        identity_stamp(&conn, "role"),
        None,
        "role stamped on refusal"
    );
    drop(conn);
    assert_eq!(file_sha256(&path), before_hash, "database bytes changed");
    assert_eq!(
        sibling_names(&dir),
        before_siblings,
        "sibling artifacts changed"
    );
    assert!(
        !before_siblings.iter().any(|n| n.contains("migration-bak")),
        "no backup may exist, before or after"
    );
}

/// Unclean-shutdown variant. A full store whose last writer died with
/// committed frames still in `-wal` is opened `Exact(PortableKernel)`. memcore
/// must still write no backup, marker, DDL, stamp or role row, and the logical
/// state must be exactly what the crashed writer committed. SQLite's own
/// last-close checkpoint is allowed to fold the frames into the main file and
/// remove `-wal`/`-shm`, so the main-file hash and the sidecar pair are NOT
/// compared for equality here.
#[test]
fn exact_portable_refusal_after_unclean_shutdown_keeps_logical_state() {
    let src = temp_dir("exact-unclean-src");
    let dst = temp_dir("exact-unclean-dst");
    let src_path = db_in(&src, "memory.db");
    let dst_path = db_in(&dst, "memory.db");
    drop(
        MemoryStore::open_with_context(&src_path, &deny(StoreProfile::TachiFull))
            .expect("fresh full create"),
    );

    // An extra open connection means no close below is the LAST close, so
    // the committed frames stay in `-wal`, as a crashed writer leaves them.
    let keeper = raw(&src_path);
    keeper
        .query_row("SELECT count(*) FROM sqlite_schema", [], |r| {
            r.get::<_, i64>(0)
        })
        .expect("keeper read");
    let mut writer = MemoryStore::open_with_context(&src_path, &deny(StoreProfile::TachiFull))
        .expect("writer open");
    writer
        .upsert(&entry("wal-only", "/scratch/wal-only"))
        .expect("commit a row into the WAL");
    drop(writer);
    let before_rows = hard_state_rows(&keeper);
    let before_version = user_version(&keeper);
    let before_schema = schema_snapshot(&keeper);

    std::fs::copy(&src_path, &dst_path).expect("copy main file");
    std::fs::copy(format!("{src_path}-wal"), format!("{dst_path}-wal")).expect("copy wal");
    drop(keeper);
    assert!(
        std::fs::metadata(format!("{dst_path}-wal"))
            .expect("wal metadata")
            .len()
            > 0,
        "fixture: the crash image must carry WAL frames"
    );
    let before_siblings = sibling_names(&dst);

    let err = MemoryStore::open_with_label_and_context(
        &dst_path,
        "global",
        &exact(StoreProfile::PortableKernel),
    )
    .err()
    .expect("an exact-portable caller must refuse a full store");
    assert!(
        matches!(err, MemoryError::StoreProfileNotExact { .. }),
        "expected StoreProfileNotExact, got {err:?}"
    );

    let after_siblings = sibling_names(&dst);
    let added: Vec<_> = after_siblings.difference(&before_siblings).collect();
    let removed: Vec<_> = before_siblings.difference(&after_siblings).collect();
    assert!(
        added.is_empty(),
        "no file may be created (no backup, no marker): {added:?}"
    );
    assert!(
        removed
            .iter()
            .all(|name| name.ends_with("-wal") || name.ends_with("-shm")),
        "only SQLite's own sidecars may disappear: {removed:?}"
    );

    let conn = raw(&dst_path);
    assert_eq!(user_version(&conn), before_version);
    assert_eq!(hard_state_rows(&conn), before_rows);
    assert_eq!(schema_snapshot(&conn), before_schema);
    assert_eq!(
        identity_stamp(&conn, "role"),
        None,
        "role stamped on refusal"
    );
    let wal_row: i64 = conn
        .query_row(
            "SELECT count(*) FROM memories WHERE id = 'wal-only'",
            [],
            |r| r.get(0),
        )
        .expect("count wal-only row");
    assert_eq!(wal_row, 1, "the WAL-committed row must survive the refusal");
}

/// Contrast for the table in the PR: the same full store under the
/// historical `AtLeast(PortableKernel)` requirement is admitted, and the
/// caller's role is stamped into it. That write-once stamp is the side effect
/// Hypermem's unlabelled preflight open exists to avoid.
#[test]
fn at_least_portable_admits_full_store_and_stamps_the_role() {
    let dir = temp_dir("at-least-admits-full");
    let path = db_in(&dir, "memory.db");
    drop(
        MemoryStore::open_with_context(&path, &deny(StoreProfile::TachiFull))
            .expect("fresh full create"),
    );

    let store = MemoryStore::open_with_label_and_context(
        &path,
        "global",
        &deny(StoreProfile::PortableKernel),
    )
    .expect("AtLeast(PortableKernel) admits a TachiFull store");
    assert_eq!(store.store_profile(), StoreProfile::TachiFull);
    drop(store);
    assert_eq!(
        identity_stamp(&raw(&path), "role").as_deref(),
        Some("global")
    );
}

/// The success side: one labelled `Exact(PortableKernel)` open of a portable
/// store is enough. It is admitted, reports the portable profile and stamps the
/// declared role exactly once. A second open changes neither stamp.
#[test]
fn exact_portable_admits_portable_store_and_stamps_role_once() {
    let dir = temp_dir("exact-admits-portable");
    let path = db_in(&dir, "memory.db");
    drop(
        MemoryStore::open_with_context(
            &path,
            &DbOpenContext::create_fresh().with_exact_profile(StoreProfile::PortableKernel),
        )
        .expect("fresh exact-portable create"),
    );
    assert_eq!(identity_stamp(&raw(&path), "role"), None);
    assert!(
        !table_exists(&raw(&path), "audit_log"),
        "an exact-portable fresh build must be portable-shaped"
    );

    let store = MemoryStore::open_with_label_and_context(
        &path,
        "global",
        &exact(StoreProfile::PortableKernel),
    )
    .expect("exact-portable caller admits a portable store");
    assert_eq!(store.store_profile(), StoreProfile::PortableKernel);
    assert_eq!(store.db_label(), "global");
    drop(store);

    let identity_rows = |conn: &Connection| -> Vec<(String, String, String)> {
        hard_state_rows(conn)
            .into_iter()
            .filter(|(namespace, _, _)| namespace == "store_identity")
            .collect()
    };
    let stamped = identity_rows(&raw(&path));
    assert_eq!(
        stamped
            .iter()
            .map(|(_, key, _)| key.as_str())
            .collect::<Vec<_>>(),
        vec!["profile", "role"],
        "exactly one profile stamp and one role stamp"
    );
    assert_eq!(
        identity_stamp(&raw(&path), "role").as_deref(),
        Some("global")
    );
    assert_eq!(
        user_version(&raw(&path)),
        crate::db::migrations::EXPECTED_SCHEMA_VERSION
    );

    drop(
        MemoryStore::open_with_label_and_context(
            &path,
            "global",
            &exact(StoreProfile::PortableKernel),
        )
        .expect("reopen"),
    );
    assert_eq!(
        identity_rows(&raw(&path)),
        stamped,
        "a second open must not re-stamp (write-once, provenance unchanged)"
    );
}
