//! Read-only Hub registry inspection (formerly the standalone `tachi-hub` binary).
//! Invoked via `tachi hub <subcommand>`.

use std::path::{Path, PathBuf};

use rusqlite::Connection;

use crate::cli::HubAction;

pub(crate) fn expand_path(raw: &str) -> PathBuf {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    if raw == "~" {
        home
    } else if let Some(rest) = raw.strip_prefix("~/") {
        home.join(rest)
    } else {
        PathBuf::from(raw)
    }
}

pub(crate) fn resolve_hub_db(db_override: Option<&PathBuf>, app_home: &Path) -> PathBuf {
    if let Some(p) = db_override {
        return expand_path(p.to_string_lossy().as_ref());
    }
    if let Ok(p) = std::env::var("MEMORY_DB_PATH") {
        return expand_path(&p);
    }
    app_home.join("global/memory.db")
}

pub(crate) fn run(action: &HubAction, db_path: &PathBuf, app_home: &Path) -> Result<(), String> {
    if !matches!(action, HubAction::Doctor { .. }) && !db_path.exists() {
        return Err(format!(
            "DB not found: {}. Run `tachi setup` or set TACHI_HOME.",
            db_path.display()
        ));
    }

    match action {
        HubAction::List {
            cap_type,
            all,
            json: false,
        } => cmd_list(db_path, cap_type.as_deref(), *all).map_err(|e| e.to_string()),
        HubAction::List { json: true, .. } => {
            Err("JSON hub list is handled in cli_tool".into())
        }
        HubAction::Show { id } => cmd_show(db_path, id).map_err(|e| e.to_string()),
        HubAction::Packs { all } => cmd_packs(db_path, *all).map_err(|e| e.to_string()),
        HubAction::Bindings => cmd_bindings(db_path).map_err(|e| e.to_string()),
        HubAction::Stats { json: false } => cmd_stats(db_path).map_err(|e| e.to_string()),
        HubAction::Doctor { fix } => cmd_doctor(app_home, *fix).map_err(|e| e.to_string()),
        HubAction::Register { .. }
        | HubAction::Enable { .. }
        | HubAction::Disable { .. }
        | HubAction::Stats { json: true } => {
            Err("handled by MemoryStore in cli_tool".into())
        }
    }
}

fn open_ro(db: &PathBuf) -> Result<Connection, Box<dyn std::error::Error>> {
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

fn cmd_list(
    db: &PathBuf,
    type_filter: Option<&str>,
    show_all: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let conn = open_ro(db)?;
    let mut sql = String::from(
        "SELECT id, type, name, version, description, enabled, review_status, health_status, uses
         FROM hub_capabilities WHERE 1=1",
    );
    if !show_all {
        sql.push_str(" AND enabled = 1");
    }
    if type_filter.is_some() {
        sql.push_str(" AND type = ?1");
    }
    sql.push_str(" ORDER BY type, name");

    let mut stmt = conn.prepare(&sql)?;
    let rows: Vec<(
        String,
        String,
        String,
        i64,
        String,
        i32,
        String,
        String,
        i64,
    )> = if let Some(t) = type_filter {
        stmt.query_map([t], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
                r.get(6)?,
                r.get(7)?,
                r.get(8)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?
    } else {
        stmt.query_map([], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
                r.get(6)?,
                r.get(7)?,
                r.get(8)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?
    };

    if rows.is_empty() {
        println!("(no capabilities)");
        return Ok(());
    }

    println!(
        "{:<32} {:<7} {:<28} {:>3} {:<9} {:<8} {:>5}  description",
        "id", "type", "name", "v", "review", "health", "uses"
    );
    println!("{}", "─".repeat(120));
    for (id, ty, name, ver, desc, enabled, review, health, uses) in &rows {
        let id_disp = if *enabled == 0 {
            format!("{} (off)", id)
        } else {
            id.clone()
        };
        println!(
            "{:<32} {:<7} {:<28} {:>3} {:<9} {:<8} {:>5}  {}",
            truncate(&id_disp, 32),
            truncate(ty, 7),
            truncate(name, 28),
            ver,
            truncate(review, 9),
            truncate(health, 8),
            uses,
            truncate(desc, 60)
        );
    }
    println!();
    println!("{} capabilities shown", rows.len());
    Ok(())
}

fn cmd_show(db: &PathBuf, id: &str) -> Result<(), Box<dyn std::error::Error>> {
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

fn cmd_packs(db: &PathBuf, show_all: bool) -> Result<(), Box<dyn std::error::Error>> {
    let conn = open_ro(db)?;
    let sql = if show_all {
        "SELECT id, name, source, version, skill_count, enabled, installed_at FROM packs ORDER BY name"
    } else {
        "SELECT id, name, source, version, skill_count, enabled, installed_at FROM packs WHERE enabled = 1 ORDER BY name"
    };
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, i64>(4)?,
                r.get::<_, i32>(5)?,
                r.get::<_, String>(6)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    if rows.is_empty() {
        println!("(no packs installed)");
        return Ok(());
    }

    println!(
        "{:<28} {:<24} {:<14} {:>6}  {}",
        "id", "name", "version", "skills", "source"
    );
    println!("{}", "─".repeat(110));
    for (id, name, src, ver, count, enabled, _at) in &rows {
        let mark = if *enabled == 0 { " (off)" } else { "" };
        println!(
            "{:<28} {:<24} {:<14} {:>6}  {}{}",
            truncate(id, 28),
            truncate(name, 24),
            truncate(ver, 14),
            count,
            truncate(src, 40),
            mark
        );
    }
    Ok(())
}

fn cmd_bindings(db: &PathBuf) -> Result<(), Box<dyn std::error::Error>> {
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
        "{:<32} → {:<32} {:>5} {:<8} {}",
        "vc_id", "capability_id", "prio", "enabled", "created_at"
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

pub(crate) fn cmd_stats(db: &PathBuf) -> Result<(), Box<dyn std::error::Error>> {
    let conn = open_ro(db)?;

    let count = |sql: &str| -> rusqlite::Result<i64> { conn.query_row(sql, [], |r| r.get(0)) };

    let memories = count("SELECT COUNT(*) FROM memories").unwrap_or(0);
    let edges = count("SELECT COUNT(*) FROM edges").unwrap_or(0);
    let caps_total = count("SELECT COUNT(*) FROM hub_capabilities").unwrap_or(0);
    let caps_enabled =
        count("SELECT COUNT(*) FROM hub_capabilities WHERE enabled = 1").unwrap_or(0);
    let skills = count("SELECT COUNT(*) FROM hub_capabilities WHERE type='skill'").unwrap_or(0);
    let plugins = count("SELECT COUNT(*) FROM hub_capabilities WHERE type='plugin'").unwrap_or(0);
    let mcps = count("SELECT COUNT(*) FROM hub_capabilities WHERE type='mcp'").unwrap_or(0);
    let packs = count("SELECT COUNT(*) FROM packs").unwrap_or(0);
    let bindings = count("SELECT COUNT(*) FROM virtual_capability_bindings").unwrap_or(0);

    println!("Tachi Hub stats — {}", db.display());
    println!("  memories          : {memories}");
    println!("  edges             : {edges}");
    println!("  capabilities      : {caps_total} ({caps_enabled} enabled)");
    println!("    └─ skill  : {skills}");
    println!("    └─ plugin : {plugins}");
    println!("    └─ mcp    : {mcps}");
    println!("  packs             : {packs}");
    println!("  virtual bindings  : {bindings}");
    Ok(())
}

fn cmd_doctor(app_home: &Path, fix: bool) -> Result<(), Box<dyn std::error::Error>> {
    let mut dbs: Vec<PathBuf> = Vec::new();
    let global = app_home.join("global/memory.db");
    if global.exists() {
        dbs.push(global);
    }
    let projects = app_home.join("projects");
    if projects.is_dir() {
        for entry in std::fs::read_dir(&projects)? {
            let entry = entry?;
            let p = entry.path().join("memory.db");
            if p.exists() {
                dbs.push(p);
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

        let missing_vec: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM memories WHERE vector IS NULL OR length(vector) = 0",
                [],
                |r| r.get(0),
            )
            .unwrap_or(0);
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
