use chrono::Utc;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use super::autofix::auto_fix_authorized;
use super::classify::{classify_one_immutable_with_provider, classify_one_with_provider};
use super::{
    AutoFixAction, DbClassification, DoctorFinding, DoctorReport, JobBreakdown, SummaryByClass,
};
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
        let is_symlink = fs::symlink_metadata(&path)
            .map(|metadata| metadata.file_type().is_symlink())
            .unwrap_or(false);
        if path.is_file() || (is_symlink && is_db_candidate_filename(name)) {
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
    if name.ends_with(".migration-marker") {
        return false;
    }
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
    scan_with_open_mode(roots, quarantine_root, options, false)
}

/// CLI doctor's default scan. SQLite WAL readers normally create or update
/// `-wal`/`-shm`; immutable opens deliberately trade live-WAL visibility for
/// a byte-stable diagnostic and leave the visible WAL as degradation evidence.
pub(crate) fn scan_strict_read_only(
    roots: &[PathBuf],
    quarantine_root: &Path,
    options: ScanOptions,
) -> DoctorReport {
    scan_with_open_mode(roots, quarantine_root, options, true)
}

fn scan_with_open_mode(
    roots: &[PathBuf],
    quarantine_root: &Path,
    options: ScanOptions,
    immutable: bool,
) -> DoctorReport {
    let candidates: Vec<PathBuf> = collect_candidates(roots, options.max_depth)
        .into_iter()
        .filter(|path| !path_is_under(path, quarantine_root))
        .collect();
    let inventory = crate::physical_db_identity::classify_paths(candidates);
    let resolved_aliases = inventory
        .stores
        .iter()
        .map(|store| store.aliases.len())
        .sum::<usize>();
    let unresolved_path_count = inventory.unresolved_paths.len();
    let routing_config = RoutingConfigProvider::new(crate::path_utils::tachi_home());
    let mut physical_stores = inventory.stores;
    let mut read_representative_findings = Vec::new();
    let mut mutation_authorized_findings = Vec::new();
    let mut mutation_authorities = BTreeMap::new();
    let mut mutation_refusals = Vec::new();
    let mut findings = Vec::new();

    for physical_store in &mut physical_stores {
        let open_path = PathBuf::from(&physical_store.open_path);
        let classified = if immutable {
            classify_one_immutable_with_provider(&open_path, &routing_config)
        } else {
            classify_one_with_provider(&open_path, &routing_config)
        };
        physical_store.open_failure_kind = classified
            .error
            .as_ref()
            .map(|error| crate::physical_db_identity::classify_open_failure(error));

        let mut read_representative = classified.clone();
        read_representative.path = physical_store.open_path.clone();
        read_representative.scope_hint =
            super::classify::scope_hint_for(Path::new(&physical_store.open_path));
        read_representative_findings.push(read_representative);

        // Autofix authority is the exact discovered primary alias. The
        // representative/open path above may be a canonical symlink target
        // or a different hardlink selected for WAL visibility and is
        // read-only evidence only.
        if physical_store.mutation_state
            == crate::physical_db_identity::PhysicalStoreMutationState::AmbiguousPhysicalStore
        {
            mutation_refusals.push(AutoFixAction {
                path: physical_store.primary_path.clone(),
                action: "ambiguous_physical_store".to_string(),
                outcome: "skipped".to_string(),
                note: format!(
                    "invariant: ambiguous_physical_store has multiple live sidecar owners ({}) and disables every mutation path until adjudicated",
                    physical_store.sidecar_paths.join(", ")
                ),
                destination: None,
            });
        } else if physical_store
            .aliases
            .contains(&physical_store.primary_path)
        {
            let mut mutation_finding = classified.clone();
            mutation_finding.path = physical_store.primary_path.clone();
            mutation_finding.scope_hint =
                super::classify::scope_hint_for(Path::new(&physical_store.primary_path));

            if mutation_finding.classification == DbClassification::WalOrphan
                && !physical_store
                    .sidecar_paths
                    .contains(&physical_store.primary_path)
            {
                mutation_refusals.push(AutoFixAction {
                    path: physical_store.primary_path.clone(),
                    action: "checkpoint_wal_copy".to_string(),
                    outcome: "skipped".to_string(),
                    note: "invariant: WAL is visible only through the read-only inventory open path; the discovered primary alias has no matching sidecar evidence and is not authorized for checkpoint mutation".to_string(),
                    destination: None,
                });
            } else {
                if let Some(authority) = physical_store.mutation_authority.clone() {
                    mutation_authorities.insert(mutation_finding.path.clone(), authority);
                    mutation_authorized_findings.push(mutation_finding);
                } else {
                    mutation_refusals.push(AutoFixAction {
                        path: physical_store.primary_path.clone(),
                        action: "doctor_autofix".to_string(),
                        outcome: "skipped".to_string(),
                        note: "invariant: doctor mutation path has no scan-captured physical authority"
                            .to_string(),
                        destination: None,
                    });
                }
            }
        } else {
            mutation_refusals.push(AutoFixAction {
                path: physical_store.primary_path.clone(),
                action: "doctor_autofix".to_string(),
                outcome: "skipped".to_string(),
                note: "invariant: doctor mutation path must be an exact discovered primary alias"
                    .to_string(),
                destination: None,
            });
        }

        for alias in &physical_store.aliases {
            let mut alias_finding = classified.clone();
            alias_finding.path = alias.clone();
            alias_finding.scope_hint = super::classify::scope_hint_for(Path::new(alias));
            findings.push(alias_finding);
        }
    }

    for unresolved in inventory.unresolved_paths {
        findings.push(DoctorFinding {
            path: unresolved.path.display().to_string(),
            classification: DbClassification::Corrupt,
            file_size: 0,
            has_wal: false,
            mem_count: None,
            vec_rowid_count: None,
            none_domain_count: None,
            cross_domain_suspect_count: None,
            cross_domain_suspect_sample: Vec::new(),
            jobs: JobBreakdown::default(),
            schema_kind: "unknown".to_string(),
            error: Some(format!(
                "{}: {}",
                unresolved.failure_kind.as_str(),
                unresolved.error
            )),
            scope_hint: super::classify::scope_hint_for(&unresolved.path),
        });
    }
    findings.sort_by(|a, b| a.path.cmp(&b.path));

    let mut summary = SummaryByClass::default();
    summary.total_databases = physical_stores.len();
    summary.total_aliases = resolved_aliases;
    summary.resolved_aliases = resolved_aliases;
    summary.unresolved_paths = unresolved_path_count;
    summary.path_appearances = resolved_aliases + unresolved_path_count;
    for f in &read_representative_findings {
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
        let mut actions = auto_fix_authorized(
            &mutation_authorized_findings,
            &mutation_authorities,
            quarantine_root,
        );
        actions.extend(mutation_refusals);
        actions
    } else {
        Vec::new()
    };

    DoctorReport {
        scanned_roots: roots.iter().map(|p| p.display().to_string()).collect(),
        findings,
        physical_stores,
        summary,
        warnings: Vec::new(),
        auto_fix_actions,
        quarantine_dir: Some(quarantine_root.display().to_string()),
        generated_at: Utc::now().to_rfc3339(),
    }
}
