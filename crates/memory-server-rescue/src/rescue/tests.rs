use crate::{apply_rescue, classify, plan_rescue, SourceRow};
use rusqlite::{params, Connection};

fn row(id: &str, path: &str, text: &str) -> SourceRow {
    SourceRow {
        id: id.into(),
        path: path.into(),
        summary: String::new(),
        text: text.into(),
        importance: 0.7,
        timestamp: "2026-01-01T00:00:00Z".into(),
        category: "fact".into(),
        topic: String::new(),
        keywords: "[]".into(),
        persons: "[]".into(),
        entities: "[]".into(),
        location: String::new(),
        source: "manual".into(),
        scope: "general".into(),
        archived: 0,
        created_at: "2026-01-01T00:00:00Z".into(),
        updated_at: "2026-01-01T00:00:00Z".into(),
        access_count: 0,
        last_access: None,
        metadata: "{}".into(),
        revision: 1,
    }
}

#[test]
fn classifier_routes_hapi_paths_to_trading() {
    let r = row("a", "/hapi/strategy", "");
    let a = classify(&r);
    assert_eq!(a.target, "hapi");
    assert!(a.trading);
}

#[test]
fn classifier_routes_quant_paths_to_quant() {
    let r = row("b", "/project/quant/v8-engine", "");
    let a = classify(&r);
    assert_eq!(a.target, "quant");
    assert!(!a.trading);
}

#[test]
fn classifier_routes_chinese_trading_paths_to_hapi() {
    let r = row("c", "/project/股票交易", "");
    let a = classify(&r);
    assert_eq!(a.target, "hapi");
    assert!(a.trading);
}

#[test]
fn classifier_falls_back_to_antigravity() {
    let r = row("d", "/user/preferences", "language: en");
    let a = classify(&r);
    assert_eq!(a.target, "antigravity");
    assert!(!a.trading);
    assert!(a.reason.contains("fallback"));
}

#[test]
fn classifier_keyword_catches_trading_jargon_in_unrouted_path() {
    let r = row("e", "/notes", "记录今天的持仓和买入价位");
    let a = classify(&r);
    assert_eq!(a.target, "hapi");
    assert!(a.trading);
}

#[test]
fn classifier_routes_hyperion_migration_marker() {
    let r = row("f", "/antigravity/hyperion_migration", "");
    let a = classify(&r);
    assert_eq!(a.target, "hyperion");
}

#[test]
fn classifier_routes_openclaw_pitfalls() {
    let r = row("g", "/project/openclaw/踩坑", "");
    let a = classify(&r);
    assert_eq!(a.target, "openclaw");
}

/// End-to-end: build a fake source DB on disk + minimal target DBs,
/// run plan + apply, assert per-target row counts and trading isolation.
#[test]
fn apply_routes_rows_into_target_dbs_with_trading_isolation() {
    let tmp = tempfile::tempdir().unwrap();
    let source_path = tmp.path().join("source.db");
    let targets_root = tmp.path().join("projects");
    std::fs::create_dir_all(&targets_root).unwrap();
    for t in ["hapi", "quant", "antigravity"] {
        std::fs::create_dir_all(targets_root.join(t)).unwrap();
    }

    // ---- Build the source DB with the legacy 21-column schema. ----
    let src = Connection::open(&source_path).unwrap();
    src.execute_batch(
        "CREATE TABLE memories (
                id TEXT PRIMARY KEY, path TEXT NOT NULL DEFAULT '/',
                summary TEXT NOT NULL DEFAULT '', text TEXT NOT NULL DEFAULT '',
                importance REAL NOT NULL DEFAULT 0.7, timestamp TEXT NOT NULL,
                category TEXT NOT NULL DEFAULT 'fact', topic TEXT NOT NULL DEFAULT '',
                keywords TEXT NOT NULL DEFAULT '[]', persons TEXT NOT NULL DEFAULT '[]',
                entities TEXT NOT NULL DEFAULT '[]', location TEXT NOT NULL DEFAULT '',
                source TEXT NOT NULL DEFAULT 'manual', scope TEXT NOT NULL DEFAULT 'general',
                archived INTEGER NOT NULL DEFAULT 0, created_at TEXT NOT NULL DEFAULT '',
                updated_at TEXT NOT NULL DEFAULT '', access_count INTEGER NOT NULL DEFAULT 0,
                last_access TEXT, metadata TEXT NOT NULL DEFAULT '{}',
                revision INTEGER NOT NULL DEFAULT 1
            );",
    )
    .unwrap();
    for (id, path) in [
        ("a", "/hapi/strategy"),
        ("b", "/project/quant/v8-engine"),
        ("c", "/user/preferences"),
    ] {
        src.execute(
                "INSERT INTO memories (id, path, timestamp, created_at, updated_at)
                 VALUES (?1, ?2, '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
                params![id, path],
            )
            .unwrap();
    }
    drop(src);

    // ---- Build target DBs with the new schema (incl. domain, no persons). ----
    let target_schema = "CREATE TABLE memories (
            id TEXT PRIMARY KEY, path TEXT NOT NULL DEFAULT '/',
            summary TEXT NOT NULL DEFAULT '', text TEXT NOT NULL DEFAULT '',
            importance REAL NOT NULL DEFAULT 0.7, timestamp TEXT NOT NULL,
            category TEXT NOT NULL DEFAULT 'fact', topic TEXT NOT NULL DEFAULT '',
            keywords TEXT NOT NULL DEFAULT '[]', entities TEXT NOT NULL DEFAULT '[]',
            source TEXT NOT NULL DEFAULT 'manual', scope TEXT NOT NULL DEFAULT 'general',
            archived INTEGER NOT NULL DEFAULT 0, created_at TEXT NOT NULL DEFAULT '',
            updated_at TEXT NOT NULL DEFAULT '', access_count INTEGER NOT NULL DEFAULT 0,
            last_access TEXT, revision INTEGER NOT NULL DEFAULT 1,
            metadata TEXT NOT NULL DEFAULT '{}',
            retention_policy TEXT, domain TEXT
        );";
    for t in ["hapi", "quant", "antigravity"] {
        let c = Connection::open(targets_root.join(t).join("memory.db")).unwrap();
        c.execute_batch(target_schema).unwrap();
    }

    // ---- Plan + apply. ----
    let plan = plan_rescue(&source_path).unwrap();
    assert_eq!(plan.source_total, 3);
    assert_eq!(plan.per_target.get("hapi").copied().unwrap_or(0), 1);
    assert_eq!(plan.per_target.get("quant").copied().unwrap_or(0), 1);
    assert_eq!(plan.per_target.get("antigravity").copied().unwrap_or(0), 1);

    let report = apply_rescue(&source_path, &targets_root, plan).unwrap();
    assert!(report.errors.is_empty(), "errors: {:?}", report.errors);
    assert_eq!(report.written_per_target.values().sum::<usize>(), 3);
    assert!(report.source_backed_up_to.is_some());

    // Verify trading isolation: hapi row got domain=equity_trading + scope=user.
    let hapi = Connection::open(targets_root.join("hapi/memory.db")).unwrap();
    let (scope, domain): (String, Option<String>) = hapi
        .query_row(
            "SELECT scope, domain FROM memories WHERE id = 'rescue-hapi-a'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(scope, "user");
    assert_eq!(domain.as_deref(), Some("equity_trading"));

    // Quant row should NOT carry the trading domain.
    let quant = Connection::open(targets_root.join("quant/memory.db")).unwrap();
    let q_domain: Option<String> = quant
        .query_row(
            "SELECT domain FROM memories WHERE id = 'rescue-quant-b'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(q_domain.is_none());

    // Re-running on the (now-renamed) source path should be impossible
    // because the original was renamed; verify backup exists and source
    // is gone.
    assert!(!source_path.exists());
    assert!(std::path::Path::new(&report.source_backed_up_to.unwrap()).exists());
}

#[test]
fn apply_rejects_retired_sticky_source_rows_before_target_writes_or_backup() {
    let tmp = tempfile::tempdir().unwrap();
    let source_path = tmp.path().join("source.db");
    let targets_root = tmp.path().join("projects");
    let src = Connection::open(&source_path).unwrap();
    src.execute_batch(
        "CREATE TABLE memories (
            id TEXT PRIMARY KEY,path TEXT NOT NULL,summary TEXT NOT NULL DEFAULT '',
            text TEXT NOT NULL DEFAULT '',importance REAL NOT NULL DEFAULT 0.7,
            timestamp TEXT NOT NULL,category TEXT NOT NULL DEFAULT 'fact',
            topic TEXT NOT NULL DEFAULT '',keywords TEXT NOT NULL DEFAULT '[]',
            persons TEXT NOT NULL DEFAULT '[]',entities TEXT NOT NULL DEFAULT '[]',
            location TEXT NOT NULL DEFAULT '',source TEXT NOT NULL DEFAULT 'manual',
            scope TEXT NOT NULL DEFAULT 'general',archived INTEGER NOT NULL DEFAULT 0,
            created_at TEXT NOT NULL DEFAULT '',updated_at TEXT NOT NULL DEFAULT '',
            access_count INTEGER NOT NULL DEFAULT 0,last_access TEXT,
            metadata TEXT NOT NULL DEFAULT '{}',revision INTEGER NOT NULL DEFAULT 1
        );
        INSERT INTO memories (id,path,timestamp,category,created_at,updated_at)
        VALUES ('legacy-sticky','/sticky/legacy','2026-01-01T00:00:00Z','sticky',
                '2026-01-01T00:00:00Z','2026-01-01T00:00:00Z');",
    )
    .unwrap();
    drop(src);

    let plan = plan_rescue(&source_path).expect("plan legacy source");
    let error = apply_rescue(&source_path, &targets_root, plan)
        .expect_err("rescue must not copy retired sticky history");
    assert!(error.contains("tachi_a2a"), "{error}");
    assert!(
        source_path.exists(),
        "a refused rescue must not rename its source"
    );
    assert!(!targets_root.exists(), "refusal must precede target writes");
}
