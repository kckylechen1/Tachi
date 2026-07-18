use chrono::Utc;
use std::fs;
use std::path::{Path, PathBuf};

use super::classify::classify_one_with_provider;
use super::{auto_fix_safe, DbClassification, DoctorFinding, DoctorReport, SummaryByClass};
use crate::memory_search_ops::routing_config::RoutingConfigProvider;

// ─── Scanning ────────────────────────────────────────────────────────────────

/// Default scan roots for `tachi doctor`. Mirrors `tidy` plus Antigravity.
pub fn default_scan_roots(home: &Path, git_root: Option<&Path>) -> Vec<PathBuf> {
    let mut roots = vec![
        home.join(".tachi"),
        home.join(".openclaw"),
        home.join(".sigil"),
        home.join(".gemini").join("antigravity"),
    ];
    if let Some(root) = git_root {
        let project_tachi = root.join(".tachi");
        if project_tachi.exists() {
            roots.push(project_tachi);
        }
        let project_sigil = root.join(".sigil");
        if project_sigil.exists() {
            roots.push(project_sigil);
        }
    }
    roots.into_iter().filter(|p| p.exists()).collect()
}

/// File-name patterns we treat as "this is a backup, do not classify by content."
/// Returns true if the basename suggests a backup/legacy artifact.
pub fn is_backup_filename(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    if n.contains(".bak") || n.contains(".broken") || n.contains(".corrupted") {
        return true;
    }
    if n.ends_with(".old.sqlite") || n.contains(".old.db") {
        return true;
    }
    if n.contains("pre-split") || n.contains(".pre-split.") {
        return true;
    }
    if n.contains(".checkpointed.db") {
        return true;
    }
    if n.starts_with("memory.db.") || n.starts_with(&format!("{}.", memcore::MEMORY_DB_FILENAME)) {
        // memory.db.20260330_211247, tachi-memory.db.20260330_211247, etc.
        return true;
    }
    false
}

/// Walk roots and return every candidate DB file (memory.db, *.sqlite,
/// *.db.bak.*, *.broken.*, *.corrupted.*, *.old.sqlite).
pub fn collect_candidates(roots: &[PathBuf], max_depth: usize) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for root in roots {
        walk_one(root, &mut out, max_depth);
    }
    out.sort();
    out.dedup();
    out
}

fn path_is_under(path: &Path, root: &Path) -> bool {
    let path = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let root = fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    path == root || path.starts_with(root)
}

fn walk_one(root: &Path, out: &mut Vec<PathBuf>, max_depth: usize) {
    if !root.exists() {
        return;
    }
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if path.is_file() {
            if is_db_candidate_filename(name) {
                out.push(path);
            }
            continue;
        }
        if path.is_dir() && max_depth > 0 {
            // Skip obvious noise dirs to bound the walk.
            if matches!(name, "node_modules" | "target" | ".git" | "__pycache__") {
                continue;
            }
            walk_one(&path, out, max_depth - 1);
        }
    }
}

fn is_db_candidate_filename(name: &str) -> bool {
    if memcore::is_memory_db_filename(name) {
        return true;
    }
    if name.ends_with(".sqlite") || name.ends_with(".db") {
        return true;
    }
    // memory.db.bak.<ts>, tachi-memory.db.migration-bak.<ts>, etc. — a suffixed
    // backup doesn't end in .db/.sqlite so the generic check above misses it.
    if name.starts_with("memory.db.")
        || name.starts_with(&format!("{}.", memcore::MEMORY_DB_FILENAME))
    {
        return true;
    }
    false
}

// ─── Top-level scan API ──────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy)]
pub struct ScanOptions {
    pub auto_fix: bool,
    pub max_depth: usize,
}

impl Default for ScanOptions {
    fn default() -> Self {
        Self {
            auto_fix: false,
            max_depth: 10,
        }
    }
}

pub fn scan(roots: &[PathBuf], quarantine_root: &Path, options: ScanOptions) -> DoctorReport {
    let candidates: Vec<PathBuf> = collect_candidates(roots, options.max_depth)
        .into_iter()
        .filter(|path| !path_is_under(path, quarantine_root))
        .collect();
    let routing_config = RoutingConfigProvider::new(crate::path_utils::tachi_home());
    let findings: Vec<DoctorFinding> = candidates
        .iter()
        .map(|path| classify_one_with_provider(path, &routing_config))
        .collect();

    let mut summary = SummaryByClass::default();
    summary.total_databases = findings.len();
    for f in &findings {
        match f.classification {
            DbClassification::Healthy => summary.healthy += 1,
            DbClassification::VecExtensionMissing => summary.vec_extension_missing += 1,
            DbClassification::WalOrphan => summary.wal_orphan += 1,
            DbClassification::Corrupt => summary.corrupt += 1,
            DbClassification::LegacySchema => summary.legacy_schema += 1,
            DbClassification::Placeholder => summary.placeholder += 1,
            DbClassification::Backup => summary.backup += 1,
        }
        if let Some(c) = f.mem_count {
            summary.total_memories += c;
        }
        summary.total_jobs += f.jobs.total;
    }

    let auto_fix_actions = if options.auto_fix {
        auto_fix_safe(&findings, quarantine_root)
    } else {
        Vec::new()
    };

    DoctorReport {
        scanned_roots: roots.iter().map(|p| p.display().to_string()).collect(),
        findings,
        summary,
        warnings: Vec::new(),
        auto_fix_actions,
        quarantine_dir: Some(quarantine_root.display().to_string()),
        generated_at: Utc::now().to_rfc3339(),
    }
}
