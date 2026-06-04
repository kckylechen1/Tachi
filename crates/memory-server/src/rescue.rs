//! Branch #6 — Antigravity rescue: split a multi-project memory.db into the
//! per-project Tachi DBs (`~/.tachi/projects/<name>/memory.db`).
//!
//! Background: `~/.gemini/antigravity/memory.db` accumulated 700+ entries from
//! many different projects (hapi trading, quant analyzer, hyperion, openclaw,
//! sigil, tachi, plus residual user/kanban notes). The owner wants those
//! memories routed into their canonical project DBs so the corresponding
//! agents (and only those agents) can see them, with a hard split between
//! coding context and trading context.
//!
//! Design rules:
//!   1. **Plan-first.** `plan_rescue` produces a deterministic `RescuePlan`
//!      enumerating every source row's target DB. No writes.
//!   2. **Insert-only.** `apply_rescue` writes to target DBs only. The source
//!      DB is renamed (`.bak.<ts>`) on success but never deleted.
//!   3. **Trading isolation.** Anything routed to the `hapi` DB gets
//!      `domain = 'equity_trading'` and `scope = 'user'` so role-sandboxed
//!      coding agents do NOT match it via search.
//!   4. **Idempotent.** New row ids are deterministic (UUID v5 of source-id +
//!      target name) so re-running plan against partially-applied state
//!      surfaces collisions cleanly instead of double-inserting.
//!   5. **Schema-aware.** Target schemas may include `domain` /
//!      `retention_policy` columns that the legacy source lacks — we detect
//!      and conditionally populate them. Legacy source `persons` is folded into
//!      `entities`; new target writes do not populate the physical column.

use rusqlite::{params, Connection, OpenFlags};
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use memory_core::types::{MemoryCategory, MemoryScope, MemorySource};

/// A row pulled from the source DB, normalised for routing.
#[derive(Debug, Clone)]
pub struct SourceRow {
    pub id: String,
    pub path: String,
    pub summary: String,
    pub text: String,
    pub importance: f64,
    pub timestamp: String,
    pub category: String,
    pub topic: String,
    pub keywords: String,
    pub persons: String,
    pub entities: String,
    pub location: String,
    pub source: String,
    pub scope: String,
    pub archived: i64,
    pub created_at: String,
    pub updated_at: String,
    pub access_count: i64,
    pub last_access: Option<String>,
    pub metadata: String,
    pub revision: i64,
}

/// Rescue routing decision for a single row.
#[derive(Debug, Clone, Serialize)]
pub struct RescueAssignment {
    pub source_id: String,
    pub source_path: String,
    /// Target project DB short name (e.g. "hapi", "quant", "antigravity").
    pub target: String,
    /// Reason / matched rule (for diff-readability).
    pub reason: String,
    /// Whether this assignment is a trading isolation row (domain override).
    pub trading: bool,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct RescuePlan {
    pub source_path: String,
    pub source_total: usize,
    /// Per-target row counts.
    pub per_target: BTreeMap<String, usize>,
    /// All routing decisions in source order.
    pub assignments: Vec<RescueAssignment>,
    /// Rows that the classifier explicitly punted on (currently 0 — fallback
    /// always routes to `antigravity`).
    pub unrouted: usize,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct RescueApplyReport {
    pub plan: RescuePlan,
    pub written_per_target: BTreeMap<String, usize>,
    pub skipped_existing: usize,
    pub errors: Vec<String>,
    pub source_backed_up_to: Option<String>,
}

/// Classify a single row into a target project DB.
///
/// The rules are checked top-down; the first match wins. The fallback bucket
/// is `antigravity` so no row is ever lost.
pub fn classify(row: &SourceRow) -> RescueAssignment {
    let p = row.path.to_ascii_lowercase();
    let t_lc = row.text.to_ascii_lowercase();

    // --- 1. Trading / hapi (highest priority — must be isolated). ---
    // hapi is a private trading agent; treat any hapi-flavoured path or text
    // as equity_trading domain regardless of where it ended up.
    if p.starts_with("/hapi/")
        || p == "/hapi"
        || p.starts_with("/project/quant/hapi-history")
        || p.starts_with("/project/股票交易")
        || p.starts_with("/project/交易策略")
        || p.starts_with("/project/quant/stocks")
        || p.starts_with("/project/quant/positions")
    {
        return RescueAssignment {
            source_id: row.id.clone(),
            source_path: row.path.clone(),
            target: "hapi".into(),
            reason: format!("path-prefix trading: {}", row.path),
            trading: true,
        };
    }

    // --- 2. Quant analyzer. ---
    if p.starts_with("/project/quant_analyzer_2026")
        || p.starts_with("/project/quant-analyzer")
        || p.starts_with("/quant_analyzer_2026")
        || p.starts_with("/project/quant/")
        || p == "/project/quant"
    {
        return RescueAssignment {
            source_id: row.id.clone(),
            source_path: row.path.clone(),
            target: "quant".into(),
            reason: format!("path-prefix quant: {}", row.path),
            trading: false,
        };
    }

    // --- 3. Hyperion. ---
    if p.starts_with("/project/hyperion") || p.starts_with("/antigravity/hyperion") {
        return RescueAssignment {
            source_id: row.id.clone(),
            source_path: row.path.clone(),
            target: "hyperion".into(),
            reason: format!("path-prefix hyperion: {}", row.path),
            trading: false,
        };
    }

    // --- 4. OpenClaw. ---
    if p.starts_with("/project/openclaw") || p.starts_with("/openclaw") {
        return RescueAssignment {
            source_id: row.id.clone(),
            source_path: row.path.clone(),
            target: "openclaw".into(),
            reason: format!("path-prefix openclaw: {}", row.path),
            trading: false,
        };
    }

    // --- 5. Sigil. ---
    if p.starts_with("/project/sigil") || p.starts_with("/sigil") {
        return RescueAssignment {
            source_id: row.id.clone(),
            source_path: row.path.clone(),
            target: "sigil".into(),
            reason: format!("path-prefix sigil: {}", row.path),
            trading: false,
        };
    }

    // --- 6. Tachi (own infrastructure notes). ---
    if p.starts_with("/tachi/") || p == "/tachi" || p.starts_with("/tachi-desktop") {
        return RescueAssignment {
            source_id: row.id.clone(),
            source_path: row.path.clone(),
            target: "tachi".into(),
            reason: format!("path-prefix tachi: {}", row.path),
            trading: false,
        };
    }

    // --- 7. Keyword fallback for ambiguous /project/* and root entries. ---
    // We check the body text only after path rules so well-pathed rows always
    // win. These keyword sets are intentionally narrow to avoid misrouting.
    let body_first_512: String = t_lc.chars().take(512).collect();
    let combined = format!("{} {}", p, body_first_512);

    let kw_trading = [
        "hapi",
        "持仓",
        "买入",
        "卖出",
        "止损",
        "止盈",
        "策略",
        "回测",
        "trading agent",
        "stock symbol",
        "ticker",
    ];
    if kw_trading.iter().any(|k| combined.contains(k)) {
        return RescueAssignment {
            source_id: row.id.clone(),
            source_path: row.path.clone(),
            target: "hapi".into(),
            reason: "keyword: trading vocab in body".into(),
            trading: true,
        };
    }

    let kw_quant = [
        "quant_analyzer",
        "quant-analyzer",
        "v8 engine",
        "v8-engine",
        "score_setup",
    ];
    if kw_quant.iter().any(|k| combined.contains(k)) {
        return RescueAssignment {
            source_id: row.id.clone(),
            source_path: row.path.clone(),
            target: "quant".into(),
            reason: "keyword: quant analyzer".into(),
            trading: false,
        };
    }

    // --- 8. Antigravity-specific or residual (user prefs, kanban, notes). ---
    // Everything that doesn't match a project bucket lands here so the
    // antigravity DB remains the catch-all for the orchestrator's own
    // memory + any rows we couldn't confidently route.
    RescueAssignment {
        source_id: row.id.clone(),
        source_path: row.path.clone(),
        target: "antigravity".into(),
        reason: "fallback: non-project / residual".into(),
        trading: false,
    }
}

/// Read all rows from the source `memories` table.
fn read_source_rows(source: &Path) -> Result<Vec<SourceRow>, String> {
    let conn = Connection::open_with_flags(
        source,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| format!("open source DB: {e}"))?;
    let persons_expr = if conn
        .query_row(
            "SELECT 1 FROM pragma_table_info('memories') WHERE name='persons' LIMIT 1",
            [],
            |_| Ok(true),
        )
        .unwrap_or(false)
    {
        "persons"
    } else {
        "'[]' AS persons"
    };
    let location_expr = if conn
        .query_row(
            "SELECT 1 FROM pragma_table_info('memories') WHERE name='location' LIMIT 1",
            [],
            |_| Ok(true),
        )
        .unwrap_or(false)
    {
        "location"
    } else {
        "'' AS location"
    };

    let sql = format!(
        "SELECT id, path, summary, text, importance, timestamp, category, topic,
                keywords, {persons_expr}, entities, {location_expr}, source, scope, archived,
                created_at, updated_at, access_count, last_access, metadata, revision
         FROM memories
         WHERE archived = 0"
    );
    let mut stmt = conn
        .prepare(&sql)
        .map_err(|e| format!("prepare source select: {e}"))?;

    let rows = stmt
        .query_map([], |r| {
            Ok(SourceRow {
                id: r.get(0)?,
                path: r.get(1)?,
                summary: r.get(2)?,
                text: r.get(3)?,
                importance: r.get(4)?,
                timestamp: r.get(5)?,
                category: r.get(6)?,
                topic: r.get(7)?,
                keywords: r.get(8)?,
                persons: r.get(9)?,
                entities: r.get(10)?,
                location: r.get(11)?,
                source: r.get(12)?,
                scope: r.get(13)?,
                archived: r.get(14)?,
                created_at: r.get(15)?,
                updated_at: r.get(16)?,
                access_count: r.get(17)?,
                last_access: r.get(18)?,
                metadata: r.get(19)?,
                revision: r.get(20)?,
            })
        })
        .map_err(|e| format!("query source rows: {e}"))?;

    let mut out = Vec::new();
    for r in rows {
        out.push(r.map_err(|e| format!("decode source row: {e}"))?);
    }
    Ok(out)
}

/// Build a deterministic rescue plan without touching any target DB.
pub fn plan_rescue(source: &Path) -> Result<RescuePlan, String> {
    let rows = read_source_rows(source)?;
    let mut plan = RescuePlan {
        source_path: source.display().to_string(),
        source_total: rows.len(),
        ..Default::default()
    };
    for row in &rows {
        let a = classify(row);
        *plan.per_target.entry(a.target.clone()).or_insert(0) += 1;
        plan.assignments.push(a);
    }
    Ok(plan)
}

/// Compute deterministic new id: prepend "rescue:<target>:" to the source id.
/// Keeps the original id discoverable in target metadata for traceability.
fn make_target_id(source_id: &str, target: &str) -> String {
    format!("rescue-{target}-{source_id}")
}

/// Detect whether a target DB has the post-migration columns we need.
struct TargetCaps {
    has_domain: bool,
    has_retention_policy: bool,
    has_location: bool,
}

fn merge_legacy_persons_into_entities(persons_raw: &str, entities_raw: &str) -> String {
    let persons: Vec<String> = serde_json::from_str(persons_raw).unwrap_or_default();
    let mut entities: Vec<String> = serde_json::from_str(entities_raw).unwrap_or_default();
    memory_core::types::fold_person_names_into_entities(&mut entities, persons);
    serde_json::to_string(&entities).unwrap_or_else(|_| "[]".to_string())
}

fn detect_target_caps(conn: &Connection) -> Result<TargetCaps, String> {
    let mut has_domain = false;
    let mut has_retention_policy = false;
    let mut has_location = false;
    let mut stmt = conn
        .prepare("PRAGMA table_info(memories)")
        .map_err(|e| format!("inspect target memories schema: {e}"))?;
    let rows = stmt
        .query_map([], |r| r.get::<_, String>(1))
        .map_err(|e| format!("read target memories schema: {e}"))?;
    for col in rows {
        let col = col.map_err(|e| format!("decode target memories schema column: {e}"))?;
        if col == "domain" {
            has_domain = true;
        }
        if col == "retention_policy" {
            has_retention_policy = true;
        }
        if col == "location" {
            has_location = true;
        }
    }
    Ok(TargetCaps {
        has_domain,
        has_retention_policy,
        has_location,
    })
}

/// Apply the plan: insert each row into its target DB. Skips rows whose
/// computed target id already exists (idempotent re-runs). The source DB is
/// renamed `<source>.bak.<rfc3339>` after a fully successful pass.
pub fn apply_rescue(
    source: &Path,
    targets_root: &Path,
    plan: RescuePlan,
) -> Result<RescueApplyReport, String> {
    let mut report = RescueApplyReport {
        plan: plan.clone(),
        ..Default::default()
    };

    // Re-read rows so we have full payloads (the plan only carries metadata).
    let rows = read_source_rows(source)?;
    let by_id: std::collections::HashMap<String, &SourceRow> =
        rows.iter().map(|r| (r.id.clone(), r)).collect();

    // Open / cache one connection per target.
    let mut conns: BTreeMap<String, (Connection, TargetCaps)> = BTreeMap::new();
    for target in plan.per_target.keys() {
        let path = targets_root.join(target).join("memory.db");
        if !path.exists() {
            report.errors.push(format!(
                "target DB missing: {} (skipping {} rows for this target)",
                path.display(),
                plan.per_target[target]
            ));
            continue;
        }
        let conn =
            Connection::open(&path).map_err(|e| format!("open target {}: {e}", path.display()))?;
        let caps = detect_target_caps(&conn)?;
        conns.insert(target.clone(), (conn, caps));
    }

    for assignment in &plan.assignments {
        let row = match by_id.get(&assignment.source_id) {
            Some(r) => r,
            None => {
                report
                    .errors
                    .push(format!("missing source row id={}", assignment.source_id));
                continue;
            }
        };
        let (conn, caps) = match conns.get(&assignment.target) {
            Some(c) => c,
            None => continue, // already errored above
        };
        let new_id = make_target_id(&row.id, &assignment.target);

        // Idempotency check.
        let exists: bool = conn
            .query_row(
                "SELECT 1 FROM memories WHERE id = ?1",
                params![new_id],
                |_| Ok(true),
            )
            .unwrap_or(false);
        if exists {
            report.skipped_existing += 1;
            continue;
        }

        // Build the metadata blob, annotating provenance + isolation hints.
        let mut meta_val: serde_json::Value =
            serde_json::from_str(&row.metadata).unwrap_or_else(|_| serde_json::json!({}));
        if let Some(obj) = meta_val.as_object_mut() {
            obj.insert(
                "rescue".into(),
                serde_json::json!({
                    "from": source.display().to_string(),
                    "from_id": row.id,
                    "from_path": row.path,
                    "target": assignment.target,
                    "reason": assignment.reason,
                    "trading": assignment.trading,
                    "at": chrono::Utc::now().to_rfc3339(),
                }),
            );
        }

        let scope_final = if assignment.trading {
            "user".to_string()
        } else {
            MemoryScope::normalize(&row.scope).to_string()
        };
        // B1/B2: normalize legacy source/category to satisfy CHECK constraints.
        let source_final = MemorySource::parse_or_external(&row.source).to_string();
        let category_final = MemoryCategory::normalize(&row.category).to_string();
        let entities_final = merge_legacy_persons_into_entities(&row.persons, &row.entities);
        let path_final =
            memory_core::types::apply_location_relocation(&row.path, &row.location, &mut meta_val);
        let meta_str = meta_val.to_string();

        let result = if caps.has_domain && caps.has_retention_policy {
            if caps.has_location {
                conn.execute(
                    "INSERT INTO memories
                     (id, path, summary, text, importance, timestamp, category, topic, keywords,
                      entities, location, source, scope, archived, created_at, updated_at,
                      access_count, last_access, revision, metadata, retention_policy, domain)
                     VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21,?22)",
                    params![
                        new_id,
                        path_final,
                        row.summary,
                        row.text,
                        row.importance,
                        row.timestamp,
                        category_final,
                        row.topic,
                        row.keywords,
                        entities_final,
                        "",
                        source_final,
                        scope_final,
                        row.archived,
                        row.created_at,
                        row.updated_at,
                        row.access_count,
                        row.last_access,
                        row.revision,
                        meta_str,
                        "durable",
                        if assignment.trading { Some("equity_trading") } else { None::<&str> },
                    ],
                )
            } else {
                conn.execute(
                    "INSERT INTO memories
                     (id, path, summary, text, importance, timestamp, category, topic, keywords,
                      entities, source, scope, archived, created_at, updated_at,
                      access_count, last_access, revision, metadata, retention_policy, domain)
                     VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21)",
                    params![
                        new_id,
                        path_final,
                        row.summary,
                        row.text,
                        row.importance,
                        row.timestamp,
                        category_final,
                        row.topic,
                        row.keywords,
                        entities_final,
                        source_final,
                        scope_final,
                        row.archived,
                        row.created_at,
                        row.updated_at,
                        row.access_count,
                        row.last_access,
                        row.revision,
                        meta_str,
                        "durable",
                        if assignment.trading { Some("equity_trading") } else { None::<&str> },
                    ],
                )
            }
        } else if caps.has_location {
            conn.execute(
                "INSERT INTO memories
                 (id, path, summary, text, importance, timestamp, category, topic, keywords,
                  entities, location, source, scope, archived, created_at, updated_at,
                  access_count, last_access, metadata, revision)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20)",
                params![
                    new_id,
                    path_final,
                    row.summary,
                    row.text,
                    row.importance,
                    row.timestamp,
                    category_final,
                    row.topic,
                    row.keywords,
                    entities_final,
                    "",
                    source_final,
                    scope_final,
                    row.archived,
                    row.created_at,
                    row.updated_at,
                    row.access_count,
                    row.last_access,
                    meta_str,
                    row.revision,
                ],
            )
        } else {
            conn.execute(
                "INSERT INTO memories
                 (id, path, summary, text, importance, timestamp, category, topic, keywords,
                  entities, source, scope, archived, created_at, updated_at,
                  access_count, last_access, metadata, revision)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19)",
                params![
                    new_id,
                    path_final,
                    row.summary,
                    row.text,
                    row.importance,
                    row.timestamp,
                    category_final,
                    row.topic,
                    row.keywords,
                    entities_final,
                    source_final,
                    scope_final,
                    row.archived,
                    row.created_at,
                    row.updated_at,
                    row.access_count,
                    row.last_access,
                    meta_str,
                    row.revision,
                ],
            )
        };

        match result {
            Ok(_) => {
                *report
                    .written_per_target
                    .entry(assignment.target.clone())
                    .or_insert(0) += 1;
            }
            Err(e) => {
                report.errors.push(format!(
                    "insert into {} (source_id={}): {e}",
                    assignment.target, row.id
                ));
            }
        }
    }

    // Backup the source DB if there were any successful writes and no errors.
    if report.errors.is_empty() && !report.written_per_target.is_empty() {
        let ts = chrono::Utc::now().format("%Y%m%dT%H%M%SZ").to_string();
        let backup_path: PathBuf = source.with_extension(format!("db.bak.{ts}"));
        if let Err(e) = std::fs::rename(source, &backup_path) {
            report
                .errors
                .push(format!("backup rename failed: {e} (source preserved)"));
        } else {
            report.source_backed_up_to = Some(backup_path.display().to_string());
        }
    }

    Ok(report)
}

/// Render a human-readable summary of a plan or apply report.
pub fn render_plan(plan: &RescuePlan) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "rescue plan for {}\n  source rows (non-archived): {}\n  per-target routing:\n",
        plan.source_path, plan.source_total
    ));
    for (target, n) in &plan.per_target {
        out.push_str(&format!("    {:>16} <- {}\n", target, n));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
