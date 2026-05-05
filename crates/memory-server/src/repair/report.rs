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

        let body = json!({
            "summary": {
                "apply_mode": self.apply_mode,
                "total_findings": total_findings,
                "total_applied": total_applied,
                "total_errors": total_errors,
                "exit_code": exit,
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
