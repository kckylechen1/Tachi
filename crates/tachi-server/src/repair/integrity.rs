//! R5 — `PRAGMA integrity_check` + `quick_check`.
//!
//! If integrity_check fails, the rule emits an `integrity_fail` finding and
//! the dispatcher will SKIP further mutating rules on this DB.

use serde_json::json;

use super::{DbContext, Finding, RepairError, RepairRule, RuleReport};

pub struct IntegrityCheck;

impl RepairRule for IntegrityCheck {
    fn id(&self) -> &'static str {
        "R5"
    }
    fn name(&self) -> &'static str {
        "Integrity check"
    }
    fn mutates(&self) -> bool {
        false
    }

    fn dry_run(&self, ctx: &mut DbContext) -> Result<RuleReport, RepairError> {
        check(ctx)
    }

    fn apply(&self, ctx: &mut DbContext) -> Result<RuleReport, RepairError> {
        check(ctx)
    }
}

fn check(ctx: &mut DbContext) -> Result<RuleReport, RepairError> {
    let mut r = RuleReport::new("R5", "Integrity check", ctx.label.clone());

    // Heavy corruption can fail PRAGMA preparation itself (e.g. an FTS5
    // vtable constructor returns SQLITE_CORRUPT when its shadow tables are
    // unreadable). Treat those Err paths as positive integrity findings so
    // the rule reports `integrity_fail` instead of bubbling up an opaque
    // RepairError that the dispatcher can't act on.
    let integrity = match run_pragma(ctx, "PRAGMA integrity_check;") {
        Ok(rows) => rows,
        Err(e) => vec![format!("integrity_check failed to run: {e}")],
    };
    let quick = match run_pragma(ctx, "PRAGMA quick_check;") {
        Ok(rows) => rows,
        Err(e) => vec![format!("quick_check failed to run: {e}")],
    };

    let integrity_ok = integrity
        .first()
        .map(|s| s.as_str() == "ok")
        .unwrap_or(false);
    let quick_ok = quick.first().map(|s| s.as_str() == "ok").unwrap_or(false);

    if integrity_ok && quick_ok {
        return Ok(r);
    }

    let mut detail = json!({});
    if !integrity_ok {
        detail["integrity_check"] = json!(integrity.iter().take(20).cloned().collect::<Vec<_>>());
    }
    if !quick_ok {
        detail["quick_check"] = json!(quick.iter().take(20).cloned().collect::<Vec<_>>());
    }
    r.findings
        .push(Finding::new("integrity_fail", 1).with_detail(detail));
    Ok(r)
}

fn run_pragma(ctx: &DbContext, sql: &str) -> Result<Vec<String>, RepairError> {
    let mut stmt = ctx.conn.prepare(sql)?;
    let rows = stmt
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}
