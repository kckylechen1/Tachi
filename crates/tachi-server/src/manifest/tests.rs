use super::*;
use crate::doctor::{DbClassification, DoctorFinding, DoctorReport, JobBreakdown, SummaryByClass};
use std::path::Path;
use tempfile::tempdir;

mod alias_drift;
mod gc_flow;
mod gc_hygiene;
mod registry;
mod schema_kind;
mod sweep_plan;

fn mk_finding(path: &str, class: DbClassification, scope: &str) -> DoctorFinding {
    DoctorFinding {
        path: path.to_string(),
        classification: class,
        file_size: 4096,
        has_wal: false,
        mem_count: Some(1),
        vec_rowid_count: Some(1),
        none_domain_count: Some(0),
        cross_domain_suspect_count: Some(0),
        cross_domain_suspect_sample: Vec::new(),
        jobs: JobBreakdown::default(),
        schema_kind: "tachi".to_string(),
        error: None,
        scope_hint: scope.to_string(),
    }
}

fn mk_report(findings: Vec<DoctorFinding>) -> DoctorReport {
    DoctorReport {
        scanned_roots: vec![],
        findings,
        summary: SummaryByClass::default(),
        warnings: vec![],
        auto_fix_actions: vec![],
        quarantine_dir: Some("/tmp/q".to_string()),
        generated_at: "2026-04-28T00:00:00+00:00".to_string(),
    }
}
