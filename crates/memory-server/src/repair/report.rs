//! Report types + ReportBuilder for `tachi repair`.

use serde::Serialize;
use serde_json::json;

use crate::manifest::DbEntry;

use super::DbContext;

#[derive(Debug, Serialize, Clone)]
pub struct Finding {
    pub kind: String,
    pub count: usize,
    #[serde(skip_serializing_if = "serde_json::Value::is_null")]
    pub detail: serde_json::Value,
}

impl Finding {
    pub fn new(kind: impl Into<String>, count: usize) -> Self {
        Finding {
            kind: kind.into(),
            count,
            detail: serde_json::Value::Null,
        }
    }

    pub fn with_detail(mut self, detail: serde_json::Value) -> Self {
        self.detail = detail;
        self
    }
}

#[derive(Debug, Serialize, Clone)]
pub struct RuleReport {
    pub rule_id: &'static str,
    pub rule_name: &'static str,
    pub db_label: String,
    pub findings: Vec<Finding>,
    pub applied: usize,
    pub skipped: usize,
    pub errors: Vec<String>,
}

impl RuleReport {
    pub fn new(rule_id: &'static str, rule_name: &'static str, db_label: String) -> Self {
        RuleReport {
            rule_id,
            rule_name,
            db_label,
            findings: Vec::new(),
            applied: 0,
            skipped: 0,
            errors: Vec::new(),
        }
    }

    pub fn finding_total(&self) -> usize {
        self.findings.iter().map(|f| f.count).sum()
    }

    pub fn is_clean(&self) -> bool {
        self.findings.is_empty() && self.errors.is_empty()
    }
}

pub struct ReportBuilder {
    apply_mode: bool,
    rule_reports: Vec<RuleReport>,
    notes: Vec<String>,
    open_errors: Vec<(String, String)>,
}

impl ReportBuilder {
    pub fn new(apply_mode: bool) -> Self {
        ReportBuilder {
            apply_mode,
            rule_reports: Vec::new(),
            notes: Vec::new(),
            open_errors: Vec::new(),
        }
    }

    pub fn push(&mut self, r: RuleReport) {
        self.rule_reports.push(r);
    }

    pub fn push_error(
        &mut self,
        ctx: &DbContext,
        rule_id: &'static str,
        rule_name: &'static str,
        msg: String,
    ) {
        let mut r = RuleReport::new(rule_id, rule_name, ctx.label.clone());
        r.errors.push(msg);
        self.rule_reports.push(r);
    }

    pub fn push_skip(
        &mut self,
        ctx: &DbContext,
        rule_id: &'static str,
        rule_name: &'static str,
        reason: &str,
    ) {
        let mut r = RuleReport::new(rule_id, rule_name, ctx.label.clone());
        r.skipped = 1;
        r.notes_into_error(reason);
        self.rule_reports.push(r);
    }

    pub fn push_open_error(&mut self, entry: &DbEntry, msg: String) {
        self.open_errors
            .push((super::inventory::label_for(entry), msg));
    }

    pub fn note(&mut self, s: String) {
        self.notes.push(s);
    }

    /// Render report and return process exit code.
    pub fn render(&self, json_out: bool) -> i32 {
        if json_out {
            self.render_json()
        } else {
            self.render_human()
        }
    }

    fn render_json(&self) -> i32 {
        let total_findings: usize = self.rule_reports.iter().map(|r| r.finding_total()).sum();
        let total_applied: usize = self.rule_reports.iter().map(|r| r.applied).sum();
        let total_errors: usize = self
            .rule_reports
            .iter()
            .map(|r| r.errors.len())
            .sum::<usize>()
            + self.open_errors.len();
        let exit = compute_exit(self.apply_mode, total_findings, total_applied, total_errors);
        let maintenance_status =
            maintenance_status(self.apply_mode, total_findings, total_applied, total_errors);
        let by_rule = findings_by_rule(&self.rule_reports);

        let body = json!({
            "summary": {
                "apply_mode": self.apply_mode,
                "status": maintenance_status,
                "maintenance_status": maintenance_status,
                "total_findings": total_findings,
                "total_applied": total_applied,
                "total_errors": total_errors,
                "health_blocking": total_errors > 0,
                "findings_are_health_deductions": false,
                "health_relationship": "repair findings are maintenance recommendations unless total_errors is non-zero; tachi status health covers active runtime blockers",
                "exit_code": exit,
            },
            "maintenance": {
                "status": maintenance_status,
                "requires_apply": !self.apply_mode && total_findings > 0,
                "safe_to_ignore_for_runtime_health": total_errors == 0,
                "by_rule": by_rule,
            },
            "notes": self.notes,
            "open_errors": self.open_errors.iter().map(|(l, m)| json!({"db": l, "error": m})).collect::<Vec<_>>(),
            "reports": self.rule_reports,
        });
        match serde_json::to_string_pretty(&body) {
            Ok(rendered) => println!("{rendered}"),
            Err(err) => eprintln!("failed to render repair report JSON: {err}"),
        }
        exit
    }

    fn render_human(&self) -> i32 {
        for n in &self.notes {
            println!("{n}");
        }
        for (label, err) in &self.open_errors {
            println!("[X] {label}: open failed: {err}");
        }

        // Group by db_label.
        let mut by_db: std::collections::BTreeMap<&str, Vec<&RuleReport>> =
            std::collections::BTreeMap::new();
        for r in &self.rule_reports {
            by_db.entry(r.db_label.as_str()).or_default().push(r);
        }

        let mut total_findings = 0usize;
        let mut total_applied = 0usize;
        let mut total_errors = self.open_errors.len();

        for (db, reports) in &by_db {
            let dirty = reports.iter().any(|r| !r.is_clean());
            let marker = if dirty { "[!]" } else { "[OK]" };
            println!("\n{marker} {db}");
            for r in reports {
                total_applied += r.applied;
                total_errors += r.errors.len();
                let n = r.finding_total();
                total_findings += n;

                if !r.errors.is_empty() {
                    println!(
                        "  [X] {} ({}): {}",
                        r.rule_id,
                        r.rule_name,
                        r.errors.join("; ")
                    );
                    continue;
                }
                if r.skipped > 0 {
                    println!("  [-] {} ({}): skipped", r.rule_id, r.rule_name);
                    continue;
                }
                if n == 0 {
                    println!("  [OK] {} ({}): clean", r.rule_id, r.rule_name);
                } else if self.apply_mode {
                    println!(
                        "  [+] {} ({}): {} finding{} | applied={}",
                        r.rule_id,
                        r.rule_name,
                        n,
                        if n == 1 { "" } else { "s" },
                        r.applied
                    );
                    for f in &r.findings {
                        println!("       - {} × {}", f.kind, f.count);
                    }
                } else {
                    println!(
                        "  [!] {} ({}): {} finding{} (dry-run)",
                        r.rule_id,
                        r.rule_name,
                        n,
                        if n == 1 { "" } else { "s" }
                    );
                    for f in &r.findings {
                        println!("       - {} × {}", f.kind, f.count);
                    }
                }
            }
        }

        let exit = compute_exit(self.apply_mode, total_findings, total_applied, total_errors);
        println!(
            "\nSummary: dbs={} findings={} applied={} errors={} mode={} exit={}",
            by_db.len(),
            total_findings,
            total_applied,
            total_errors,
            if self.apply_mode { "apply" } else { "dry-run" },
            exit
        );
        if !self.apply_mode && total_findings > 0 {
            println!(
                "Maintenance: findings are repair recommendations, not health deductions unless errors are present."
            );
            println!("Re-run with --apply to perform repairs (per-DB backup auto-taken).");
        }
        exit
    }
}

impl RuleReport {
    fn notes_into_error(&mut self, msg: &str) {
        // Stash skip reason in errors so the JSON & human render show it.
        self.errors.push(msg.to_string());
    }
}

fn compute_exit(apply_mode: bool, findings: usize, applied: usize, errors: usize) -> i32 {
    if errors > 0 {
        return 2;
    }
    if !apply_mode && findings > 0 {
        return 1;
    }
    if apply_mode && findings > 0 && applied == 0 {
        return 1;
    }
    0
}

fn maintenance_status(
    apply_mode: bool,
    findings: usize,
    applied: usize,
    errors: usize,
) -> &'static str {
    if errors > 0 {
        "error"
    } else if apply_mode && applied > 0 {
        "applied"
    } else if findings > 0 {
        "maintenance_recommended"
    } else {
        "clean"
    }
}

fn findings_by_rule(rule_reports: &[RuleReport]) -> Vec<serde_json::Value> {
    let mut by_rule: std::collections::BTreeMap<
        (&'static str, &'static str),
        (usize, usize, usize, std::collections::BTreeSet<String>),
    > = std::collections::BTreeMap::new();
    for report in rule_reports {
        let entry = by_rule
            .entry((report.rule_id, report.rule_name))
            .or_insert((0, 0, 0, std::collections::BTreeSet::new()));
        entry.0 += report.finding_total();
        entry.1 += report.errors.len();
        entry.2 += report.skipped;
        if report.finding_total() > 0 || !report.errors.is_empty() || report.skipped > 0 {
            entry.3.insert(report.db_label.clone());
        }
    }
    by_rule
        .into_iter()
        .filter_map(|((rule_id, rule_name), (findings, errors, skipped, dbs))| {
            (findings > 0 || errors > 0 || skipped > 0).then(|| {
                json!({
                    "rule_id": rule_id,
                    "rule_name": rule_name,
                    "findings": findings,
                    "errors": errors,
                    "skipped": skipped,
                    "dbs": dbs.into_iter().collect::<Vec<_>>(),
                })
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maintenance_status_separates_runtime_health_from_repair_findings() {
        assert_eq!(maintenance_status(false, 0, 0, 0), "clean");
        assert_eq!(
            maintenance_status(false, 3, 0, 0),
            "maintenance_recommended"
        );
        assert_eq!(maintenance_status(true, 3, 3, 0), "applied");
        assert_eq!(maintenance_status(false, 0, 0, 1), "error");
    }

    #[test]
    fn findings_by_rule_groups_impacted_dbs_for_product_summaries() {
        let mut first = RuleReport::new("R2", "Retention backfill", "global".to_string());
        first.findings.push(Finding::new("missing_retention", 2));
        let mut second = RuleReport::new("R2", "Retention backfill", "project:tachi".to_string());
        second.findings.push(Finding::new("missing_retention", 3));
        let clean = RuleReport::new("R9", "Domain repair", "project:clean".to_string());

        let grouped = findings_by_rule(&[first, second, clean]);

        assert_eq!(grouped.len(), 1);
        assert_eq!(grouped[0]["rule_id"], "R2");
        assert_eq!(grouped[0]["findings"], 5);
        assert_eq!(
            grouped[0]["dbs"],
            serde_json::json!(["global", "project:tachi"])
        );
    }
}
