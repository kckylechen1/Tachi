use rusqlite::Connection;
use std::path::{Path, PathBuf};

fn open_ro(db: &Path) -> Result<Connection, Box<dyn std::error::Error>> {
    Ok(Connection::open(db)?)
}

fn truncate(s: &str, n: usize) -> String {
    let s = s.replace(['\n', '\r'], " ");
    if s.chars().count() <= n {
        s
    } else {
        let mut out: String = s.chars().take(n.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

pub fn collect_list_filtered_with<F>(
    db: &Path,
    type_filter: Option<&str>,
    show_all: bool,
    include_capability: F,
) -> Result<Vec<memcore::HubCapability>, Box<dyn std::error::Error>>
where
    F: Fn(&memcore::HubCapability) -> bool,
{
    let conn = open_ro(db)?;
    let mut capabilities = memcore::db::hub_list(&conn, type_filter, !show_all)?
        .into_iter()
        .filter(include_capability)
        .collect::<Vec<_>>();
    capabilities.sort_by(|left, right| {
        left.cap_type
            .cmp(&right.cap_type)
            .then_with(|| left.name.cmp(&right.name))
    });
    Ok(capabilities)
}

pub fn cmd_list_filtered<F>(
    db: &Path,
    type_filter: Option<&str>,
    show_all: bool,
    include_capability: F,
) -> Result<(), Box<dyn std::error::Error>>
where
    F: Fn(&memcore::HubCapability) -> bool,
{
    let capabilities = collect_list_filtered_with(db, type_filter, show_all, include_capability)?;

    if capabilities.is_empty() {
        println!("(no capabilities)");
        return Ok(());
    }

    println!(
        "{:<32} {:<7} {:<28} {:>3} {:<9} {:<8} {:>5}  description",
        "id", "type", "name", "v", "review", "health", "uses"
    );
    println!("{}", "─".repeat(120));
    for cap in &capabilities {
        let id_disp = if !cap.enabled {
            format!("{} (off)", cap.id)
        } else {
            cap.id.clone()
        };
        println!(
            "{:<32} {:<7} {:<28} {:>3} {:<9} {:<8} {:>5}  {}",
            truncate(&id_disp, 32),
            truncate(&cap.cap_type, 7),
            truncate(&cap.name, 28),
            cap.version,
            truncate(&cap.review_status, 9),
            truncate(&cap.health_status, 8),
            cap.uses,
            truncate(&cap.description, 60)
        );
    }
    println!();
    println!("{} capabilities shown", capabilities.len());
    Ok(())
}

pub(super) fn cmd_list(
    db: &Path,
    type_filter: Option<&str>,
    show_all: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    cmd_list_filtered(db, type_filter, show_all, |_| true)
}

pub(super) fn cmd_show(db: &Path, id: &str) -> Result<(), Box<dyn std::error::Error>> {
    let conn = open_ro(db)?;
    let mut stmt = conn.prepare(
        "SELECT id, type, name, version, description, enabled, review_status, health_status,
                last_error, last_success_at, last_failure_at, fail_streak, active_version,
                exposure_mode, uses, successes, failures, avg_rating, last_used,
                created_at, updated_at, definition
         FROM hub_capabilities WHERE id = ?1",
    )?;
    let mut rows = stmt.query([id])?;
    let row = rows
        .next()?
        .ok_or_else(|| format!("capability not found: {id}"))?;

    let cap_id: String = row.get(0)?;
    let ty: String = row.get(1)?;
    let name: String = row.get(2)?;
    let ver: i64 = row.get(3)?;
    let desc: String = row.get(4)?;
    let enabled: i32 = row.get(5)?;
    let review: String = row.get(6)?;
    let health: String = row.get(7)?;
    let last_error: Option<String> = row.get(8)?;
    let last_success: Option<String> = row.get(9)?;
    let last_failure: Option<String> = row.get(10)?;
    let fail_streak: i64 = row.get(11)?;
    let active_version: Option<String> = row.get(12)?;
    let exposure: String = row.get(13)?;
    let uses: i64 = row.get(14)?;
    let successes: i64 = row.get(15)?;
    let failures: i64 = row.get(16)?;
    let avg_rating: Option<f64> = row.get(17)?;
    let last_used: Option<String> = row.get(18)?;
    let created: String = row.get(19)?;
    let updated: String = row.get(20)?;
    let definition: String = row.get(21)?;

    println!("╭─ {} ────────────────────────────────────────", cap_id);
    println!("│ name         : {name}");
    println!("│ type         : {ty}");
    println!(
        "│ version      : {ver} (active={})",
        active_version.as_deref().unwrap_or("-")
    );
    println!(
        "│ enabled      : {}",
        if enabled != 0 { "yes" } else { "no" }
    );
    println!("│ review       : {review}");
    println!("│ health       : {health}");
    println!("│ exposure     : {exposure}");
    println!("│ uses/ok/fail : {uses} / {successes} / {failures}  (streak {fail_streak})");
    if let Some(r) = avg_rating {
        println!("│ avg_rating   : {r:.2}");
    }
    if let Some(lu) = last_used {
        println!("│ last_used    : {lu}");
    }
    if let Some(s) = last_success {
        println!("│ last_success : {s}");
    }
    if let Some(f) = last_failure {
        println!("│ last_failure : {f}");
    }
    if let Some(e) = last_error {
        println!("│ last_error   : {}", truncate(&e, 80));
    }
    println!("│ created      : {created}");
    println!("│ updated      : {updated}");
    println!("├─ description ─");
    for line in desc.lines() {
        println!("│ {line}");
    }
    println!("├─ definition (truncated) ─");
    let pretty = serde_json::from_str::<serde_json::Value>(&definition)
        .ok()
        .and_then(|v| serde_json::to_string_pretty(&v).ok())
        .unwrap_or(definition);
    for line in pretty.lines().take(40) {
        println!("│ {line}");
    }
    println!("╰─");
    Ok(())
}

pub(super) fn cmd_bindings(db: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let conn = open_ro(db)?;
    let mut stmt = conn.prepare(
        "SELECT vc_id, capability_id, priority, enabled, created_at
         FROM virtual_capability_bindings ORDER BY vc_id, priority",
    )?;
    let rows = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, i32>(3)?,
                r.get::<_, String>(4)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    if rows.is_empty() {
        println!("(no virtual capability bindings)");
        return Ok(());
    }
    println!(
        "{:<32} → {:<32} {:>5} {:<8} created_at",
        "vc_id", "capability_id", "prio", "enabled"
    );
    println!("{}", "─".repeat(110));
    for (vc, cap, prio, enabled, created) in rows {
        println!(
            "{:<32} → {:<32} {:>5} {:<8} {}",
            truncate(&vc, 32),
            truncate(&cap, 32),
            prio,
            if enabled != 0 { "yes" } else { "no" },
            created
        );
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq)]
pub struct HubStatsSnapshot {
    pub memories: i64,
    pub edges: i64,
    pub capabilities: usize,
    pub enabled_capabilities: usize,
    pub by_type: std::collections::HashMap<String, usize>,
    pub total_uses: u64,
    pub total_successes: u64,
    pub virtual_bindings: i64,
}

pub fn collect_stats_filtered<F>(
    db: &Path,
    include_capability: F,
) -> Result<HubStatsSnapshot, Box<dyn std::error::Error>>
where
    F: Fn(&memcore::HubCapability) -> bool,
{
    let conn = open_ro(db)?;

    let count = |sql: &str| -> rusqlite::Result<i64> { conn.query_row(sql, [], |r| r.get(0)) };

    let memories = count("SELECT COUNT(*) FROM memories").unwrap_or(0);
    let edges = count("SELECT COUNT(*) FROM edges").unwrap_or(0);
    let capabilities = memcore::db::hub_list(&conn, None, false)?
        .into_iter()
        .filter(include_capability)
        .collect::<Vec<_>>();
    let enabled_capabilities = capabilities.iter().filter(|cap| cap.enabled).count();
    let mut by_type = std::collections::HashMap::new();
    for cap in &capabilities {
        *by_type.entry(cap.cap_type.clone()).or_insert(0) += 1;
    }
    let total_uses = capabilities.iter().map(|cap| cap.uses).sum();
    let total_successes = capabilities.iter().map(|cap| cap.successes).sum();
    let virtual_bindings = count("SELECT COUNT(*) FROM virtual_capability_bindings").unwrap_or(0);

    Ok(HubStatsSnapshot {
        memories,
        edges,
        capabilities: capabilities.len(),
        enabled_capabilities,
        by_type,
        total_uses,
        total_successes,
        virtual_bindings,
    })
}

pub fn cmd_stats_filtered<F>(
    db: &Path,
    include_capability: F,
) -> Result<(), Box<dyn std::error::Error>>
where
    F: Fn(&memcore::HubCapability) -> bool,
{
    let stats = collect_stats_filtered(db, include_capability)?;

    println!("Tachi Hub stats — {}", db.display());
    println!("  memories          : {}", stats.memories);
    println!("  edges             : {}", stats.edges);
    println!(
        "  capabilities      : {} ({} enabled)",
        stats.capabilities, stats.enabled_capabilities
    );
    println!(
        "    └─ skill  : {}",
        stats.by_type.get("skill").copied().unwrap_or(0)
    );
    println!(
        "    └─ plugin : {}",
        stats.by_type.get("plugin").copied().unwrap_or(0)
    );
    println!(
        "    └─ mcp    : {}",
        stats.by_type.get("mcp").copied().unwrap_or(0)
    );
    println!("  virtual bindings  : {}", stats.virtual_bindings);
    Ok(())
}

pub fn cmd_stats(db: &Path) -> Result<(), Box<dyn std::error::Error>> {
    cmd_stats_filtered(db, |_| true)
}

pub(super) fn cmd_doctor(app_home: &Path, fix: bool) -> Result<(), Box<dyn std::error::Error>> {
    let mut dbs: Vec<PathBuf> = Vec::new();
    let global = app_home.join("global").join(memcore::MEMORY_DB_FILENAME);
    let global_legacy = app_home
        .join("global")
        .join(memcore::LEGACY_MEMORY_DB_FILENAME);
    if global.exists() {
        dbs.push(global);
    } else if global_legacy.exists() {
        dbs.push(global_legacy);
    }
    let projects = app_home.join("projects");
    if projects.is_dir() {
        for entry in std::fs::read_dir(&projects)? {
            let entry = entry?;
            for name in [
                memcore::MEMORY_DB_FILENAME,
                memcore::LEGACY_MEMORY_DB_FILENAME,
            ] {
                let p = entry.path().join(name);
                if p.exists() {
                    dbs.push(p);
                    break;
                }
            }
        }
    }

    if dbs.is_empty() {
        println!("doctor: no DBs found under {}", app_home.display());
        return Ok(());
    }

    let required_cols: &[(&str, &str)] = &[("retention_policy", "TEXT"), ("domain", "TEXT")];

    let mut total_issues = 0;
    for db in &dbs {
        println!("── {} ──", db.display());
        let conn = Connection::open(db)?;

        let mut existing: Vec<String> = Vec::new();
        let mut stmt = conn.prepare("PRAGMA table_info(memories)")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(1))?;
        for row in rows {
            existing.push(row?);
        }

        for (col, ty) in required_cols {
            if !existing.iter().any(|c| c == col) {
                total_issues += 1;
                if fix {
                    let sql = format!("ALTER TABLE memories ADD COLUMN {col} {ty}");
                    match conn.execute(&sql, []) {
                        Ok(_) => println!("  [fix] added column memories.{col}"),
                        Err(e) => println!("  [fail] adding {col}: {e}"),
                    }
                } else {
                    println!("  [drift] missing column memories.{col} ({ty})  → run with --fix");
                }
            }
        }

        let missing_vec = count_memories_missing_vectors(&conn).unwrap_or(0);
        if missing_vec > 0 {
            println!(
                "  [info] {missing_vec} memories without vectors (run `tachi backfill-vectors --db {}`)",
                db.display()
            );
        }

        let ghost_old: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM ghost_messages WHERE promoted = 0 AND created_at < datetime('now', '-30 days')",
                [],
                |r| r.get(0),
            )
            .unwrap_or(0);
        if ghost_old > 0 {
            println!("  [info] {ghost_old} unpromoted ghost messages older than 30d");
        }

        let kanban_open: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM kanban_cards WHERE status='open' AND created_at < datetime('now', '-14 days')",
                [],
                |r| r.get(0),
            )
            .unwrap_or(0);
        if kanban_open > 0 {
            println!("  [info] {kanban_open} kanban cards open >14d");
        }
    }

    println!();
    if total_issues == 0 {
        println!("doctor: all clear ({} DBs scanned)", dbs.len());
    } else if fix {
        println!(
            "doctor: {} drift issues addressed across {} DBs",
            total_issues,
            dbs.len()
        );
    } else {
        println!(
            "doctor: {} schema drift issues across {} DBs — re-run with --fix to apply",
            total_issues,
            dbs.len()
        );
    }
    Ok(())
}

pub(crate) fn count_memories_missing_vectors(conn: &Connection) -> rusqlite::Result<i64> {
    conn.query_row(
        "SELECT COUNT(*)
         FROM memories m
         WHERE m.id NOT IN (SELECT id FROM memories_vec)
           AND m.source != 'foundry_recall_rerank_cache'",
        [],
        |r| r.get(0),
    )
}
