//! Manifest v1 — single source of truth for which memory.db files Tachi owns.
//!
//! Stored at `~/.tachi/manifest.json`. JSON (not TOML) to avoid adding a new
//! workspace dependency; comments simulated via `_comment` keys where useful.
//!
//! Schema:
//! {
//!   "schema_version": 1,
//!   "generated_at": "<ISO-8601>",
//!   "_comment": "Tachi-owned memory DBs. Do not hand-edit while server runs.",
//!   "dbs": [
//!     {
//!       "path": "/Users/.../.tachi/global/memory.db",
//!       "role": "global",            // global | project | agent | foundry | unknown
//!       "owner": "tachi",            // tachi | openclaw-agent:<name> | antigravity | external
//!       "schema_kind": "tachi",      // tachi | openclaw_legacy | unknown
//!       "vec_enabled": true,
//!       "allow_write": true,
//!       "last_doctor_at": "<ISO-8601>",
//!       "last_classification": "healthy",
//!       "scope_hint": "global",
//!       "notes": ""
//!     }, ...
//!   ]
//! }
//!
//! Branch #2 deliverable: load/save manifest, populate from a doctor::DoctorReport,
//! lookup by role/scope_hint, and a CLI subcommand `tachi manifest` (show | init |
//! refresh). Branch #3 will route runtime save/recall through manifest lookups.

use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

use crate::doctor::{DbClassification, DoctorFinding, DoctorReport};

pub const MANIFEST_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub schema_version: u32,
    pub generated_at: String,
    #[serde(rename = "_comment", default, skip_serializing_if = "String::is_empty")]
    pub comment: String,
    pub dbs: Vec<DbEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DbEntry {
    pub path: String,
    pub role: DbRole,
    pub owner: String,
    pub schema_kind: String,
    pub vec_enabled: bool,
    pub allow_write: bool,
    pub last_doctor_at: String,
    pub last_classification: String,
    pub scope_hint: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub notes: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DbRole {
    Global,
    Project,
    Agent,
    Foundry,
    Unknown,
}

impl Manifest {
    pub fn empty() -> Self {
        Self {
            schema_version: MANIFEST_SCHEMA_VERSION,
            generated_at: Utc::now().to_rfc3339(),
            comment: "Tachi-owned memory DBs. Managed by `tachi doctor` / `tachi manifest`."
                .to_string(),
            dbs: Vec::new(),
        }
    }

    pub fn default_path(home: &Path) -> PathBuf {
        home.join(".tachi").join("manifest.json")
    }

    pub fn load(path: &Path) -> std::io::Result<Self> {
        let bytes = fs::read(path)?;
        let m: Manifest = serde_json::from_slice(&bytes).map_err(|e| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("manifest parse: {e}"),
            )
        })?;
        Ok(m)
    }

    pub fn load_or_empty(path: &Path) -> Self {
        Self::load(path).unwrap_or_else(|_| Self::empty())
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(self).map_err(|e| {
            std::io::Error::new(
                std::io::ErrorKind::Other,
                format!("manifest serialize: {e}"),
            )
        })?;
        // Atomic-ish write via tmp + rename.
        let tmp = path.with_extension("json.tmp");
        fs::write(&tmp, json.as_bytes())?;
        fs::rename(&tmp, path)?;
        Ok(())
    }

    /// Populate from a doctor scan. Healthy + WalOrphan + LegacySchema get
    /// recorded; placeholder/backup/corrupt are excluded (they are not "owned"
    /// in the runtime sense). Existing entries' `notes` are preserved.
    pub fn populate_from_doctor(&mut self, report: &DoctorReport) {
        use std::collections::HashMap;
        let prior_notes: HashMap<String, String> = self
            .dbs
            .iter()
            .map(|e| (e.path.clone(), e.notes.clone()))
            .collect();

        let mut new_dbs: Vec<DbEntry> = Vec::new();
        for f in &report.findings {
            if !should_record(f) {
                continue;
            }
            let role = classify_role(f);
            let owner = derive_owner(&f.scope_hint);
            // WalOrphan is permitted for write: a non-empty -wal sidecar is the
            // *expected* state for any SQLite DB held open by a running daemon.
            // SQLite performs WAL recovery automatically on next open, so the
            // classification reflects "needs inspection" rather than corruption.
            // Without this, a healthy DB held by the live daemon trips
            // check_writable and self-locks the CLI (see PR-A).
            let allow_write = matches!(
                f.classification,
                DbClassification::Healthy | DbClassification::WalOrphan
            );
            let entry = DbEntry {
                path: f.path.clone(),
                role,
                owner,
                schema_kind: f.schema_kind.clone(),
                vec_enabled: f.vec_rowid_count.is_some(),
                allow_write,
                last_doctor_at: report.generated_at.clone(),
                last_classification: f.classification.as_str().to_string(),
                scope_hint: f.scope_hint.clone(),
                notes: prior_notes.get(&f.path).cloned().unwrap_or_default(),
            };
            new_dbs.push(entry);
        }
        new_dbs.sort_by(|a, b| a.path.cmp(&b.path));
        self.dbs = new_dbs;
        self.generated_at = Utc::now().to_rfc3339();
        if self.schema_version == 0 {
            self.schema_version = MANIFEST_SCHEMA_VERSION;
        }
    }

    /// Look up a single owned DB by exact path. Used by runtime resolvers in branch #3.
    pub fn lookup(&self, path: &str) -> Option<&DbEntry> {
        self.dbs.iter().find(|e| e.path == path)
    }

    /// Look up the global DB entry (if any). At most one expected per manifest.
    pub fn global(&self) -> Option<&DbEntry> {
        self.dbs.iter().find(|e| e.role == DbRole::Global)
    }

    /// All entries with a given role.
    pub fn by_role(&self, role: DbRole) -> Vec<&DbEntry> {
        self.dbs.iter().filter(|e| e.role == role).collect()
    }

    /// Manifest-aware write guard. Returns Ok if the path is recorded with
    /// allow_write=true; returns an explanatory Err otherwise. Callers may
    /// choose to bypass for known-safe init paths (e.g. fresh user setup).
    pub fn check_writable(&self, path: &str) -> Result<&DbEntry, ManifestGuardError> {
        match self.lookup(path) {
            Some(e) if e.allow_write => Ok(e),
            Some(e) => Err(ManifestGuardError::WriteForbidden {
                path: path.to_string(),
                last_classification: e.last_classification.clone(),
            }),
            None => Err(ManifestGuardError::NotInManifest {
                path: path.to_string(),
            }),
        }
    }
}

#[derive(Debug, Clone)]
pub enum ManifestGuardError {
    NotInManifest {
        path: String,
    },
    WriteForbidden {
        path: String,
        last_classification: String,
    },
}

impl std::fmt::Display for ManifestGuardError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotInManifest { path } => write!(
                f,
                "path '{path}' is not in tachi manifest; refuse to create. Run `tachi doctor` or `tachi manifest refresh` first."
            ),
            Self::WriteForbidden { path, last_classification } => write!(
                f,
                "path '{path}' is in manifest but not writable (last_classification={last_classification})"
            ),
        }
    }
}

impl std::error::Error for ManifestGuardError {}

/// Result of a manifest-guided sweep operation.
#[derive(Debug, Clone, serde::Serialize)]
pub struct SweepReport {
    pub planned: Vec<SweepAction>,
    pub applied: Vec<SweepAction>,
    pub skipped: Vec<SweepAction>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct SweepAction {
    pub path: String,
    pub reason: String,
    pub quarantine_to: Option<String>,
    pub note: String,
}

/// Plan a sweep: identify Placeholder/Backup files from a fresh doctor scan
/// that are NOT in the manifest, and propose moving them to quarantine.
/// Manifest-recorded entries are NEVER swept (they are owned).
///
/// Safety rules (paranoid by default):
///   1. The file must be classified Placeholder or Backup.
///   2. The file must NOT be in the manifest (owned files are sacred).
///   3. The file must live under a "Tachi-owned root" — either:
///        a. its parent directory contains a manifest-recorded Tachi DB, OR
///        b. its path matches one of the well-known Tachi roots
///           (`~/.tachi`, `~/.openclaw/extensions/tachi`, `~/.gemini/antigravity`).
///      This prevents sweeping unrelated sqlite files like `5min.db`,
///      `rust_gateway.db`, `cursor_mcp.db` which belong to other tools.
///   4. Quarantine target names are made unique with a numeric suffix to avoid
///      collisions when multiple swept files share the same basename.
pub fn plan_sweep(
    report: &DoctorReport,
    manifest: &Manifest,
    quarantine_dir: &Path,
) -> SweepReport {
    let mut planned = Vec::new();
    let mut skipped = Vec::new();
    let mut used_names: std::collections::HashSet<String> = std::collections::HashSet::new();

    // Build the set of parent directories that already host an owned Tachi DB.
    let mut owned_parents: std::collections::HashSet<String> = std::collections::HashSet::new();
    for e in &manifest.dbs {
        if let Some(p) = std::path::Path::new(&e.path).parent() {
            owned_parents.insert(p.to_string_lossy().to_string());
        }
    }

    for f in &report.findings {
        if manifest.lookup(&f.path).is_some() {
            skipped.push(SweepAction {
                path: f.path.clone(),
                reason: format!("{:?}", f.classification),
                quarantine_to: None,
                note: "in manifest — owned, never swept".to_string(),
            });
            continue;
        }
        let should_sweep = matches!(
            f.classification,
            DbClassification::Placeholder | DbClassification::Backup
        );
        if !should_sweep {
            continue;
        }

        // Tachi-owned-root gate.
        let parent = std::path::Path::new(&f.path)
            .parent()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default();
        let in_tachi_root = parent.contains("/.tachi/")
            || parent.ends_with("/.tachi")
            || parent.contains("/.openclaw/extensions/tachi")
            || parent.contains("/.gemini/antigravity");
        let neighbor_owned = owned_parents.contains(&parent);

        if !in_tachi_root && !neighbor_owned {
            skipped.push(SweepAction {
                path: f.path.clone(),
                reason: format!("{:?}", f.classification),
                quarantine_to: None,
                note: "outside Tachi-owned roots — refusing to sweep".to_string(),
            });
            continue;
        }

        // Build a collision-free quarantine name.
        let base = std::path::Path::new(&f.path)
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "db".into());
        let mut candidate = format!("swept-{}", base);
        let mut n = 1;
        while used_names.contains(&candidate) || quarantine_dir.join(&candidate).exists() {
            candidate = format!("swept-{}.{}", base, n);
            n += 1;
        }
        used_names.insert(candidate.clone());
        let qpath = quarantine_dir
            .join(&candidate)
            .to_string_lossy()
            .to_string();

        planned.push(SweepAction {
            path: f.path.clone(),
            reason: format!("{:?}", f.classification),
            quarantine_to: Some(qpath),
            note: f.error.clone().unwrap_or_default(),
        });
    }

    SweepReport {
        planned,
        applied: Vec::new(),
        skipped,
    }
}

/// Execute a sweep plan: move each planned file to its quarantine target.
/// Returns the report with `applied` populated. Entries that fail to move
/// are recorded in `skipped` with the error reason.
pub fn apply_sweep(mut report: SweepReport, quarantine_dir: &Path) -> SweepReport {
    if let Err(e) = std::fs::create_dir_all(quarantine_dir) {
        for p in report.planned.drain(..) {
            report.skipped.push(SweepAction {
                note: format!("quarantine_dir create failed: {e}"),
                ..p
            });
        }
        return report;
    }
    let planned = std::mem::take(&mut report.planned);
    for action in planned {
        let Some(target) = action.quarantine_to.clone() else {
            report.skipped.push(action);
            continue;
        };
        match std::fs::rename(&action.path, &target) {
            Ok(_) => report.applied.push(action),
            Err(e) => {
                let note = format!("rename failed: {e}");
                report.skipped.push(SweepAction { note, ..action });
            }
        }
    }
    report
}

fn should_record(f: &DoctorFinding) -> bool {
    // Only record DBs that look like Tachi/OpenClaw memory stores. Random
    // sqlite files (kline_cache.db, rust_gateway.db, run snapshots, etc.) are
    // not "owned" by Tachi runtime and must not be in the manifest.
    let class_ok = matches!(
        f.classification,
        DbClassification::Healthy | DbClassification::WalOrphan | DbClassification::LegacySchema
    );
    let schema_ok = matches!(f.schema_kind.as_str(), "tachi" | "openclaw_legacy");
    class_ok && schema_ok
}

fn classify_role(f: &DoctorFinding) -> DbRole {
    let s = f.scope_hint.as_str();
    if s == "global" {
        DbRole::Global
    } else if s.starts_with("project:") {
        DbRole::Project
    } else if s.starts_with("openclaw-agent") {
        DbRole::Agent
    } else if f.path.contains("/foundry/") || f.path.contains("/foundry.db") {
        DbRole::Foundry
    } else if s.starts_with("antigravity") {
        // Antigravity DB is rescued in branch #6 → routed under projects/. Mark as project for now.
        DbRole::Project
    } else {
        DbRole::Unknown
    }
}

fn derive_owner(scope_hint: &str) -> String {
    if let Some(rest) = scope_hint.strip_prefix("openclaw-agent:") {
        return format!("openclaw-agent:{}", rest);
    }
    if let Some(rest) = scope_hint.strip_prefix("openclaw-agent-local:") {
        return format!("openclaw-agent:{}", rest);
    }
    if scope_hint.starts_with("antigravity") {
        return "antigravity".to_string();
    }
    if scope_hint.starts_with("project:") || scope_hint == "global" {
        return "tachi".to_string();
    }
    "external".to_string()
}

pub fn render_manifest(m: &Manifest) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    let _ = writeln!(
        out,
        "tachi manifest v{}  generated_at={}  dbs={}",
        m.schema_version,
        m.generated_at,
        m.dbs.len()
    );
    for e in &m.dbs {
        let _ = writeln!(
            out,
            "  [{}] {}  owner={}  schema={}  vec={}  write={}  last={}  scope={}",
            role_str(&e.role),
            e.path,
            e.owner,
            e.schema_kind,
            e.vec_enabled,
            e.allow_write,
            e.last_classification,
            e.scope_hint,
        );
    }
    out
}

fn role_str(r: &DbRole) -> &'static str {
    match r {
        DbRole::Global => "global",
        DbRole::Project => "project",
        DbRole::Agent => "agent",
        DbRole::Foundry => "foundry",
        DbRole::Unknown => "unknown",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doctor::{JobBreakdown, SummaryByClass};
    use tempfile::tempdir;

    fn mk_finding(path: &str, class: DbClassification, scope: &str) -> DoctorFinding {
        DoctorFinding {
            path: path.to_string(),
            classification: class,
            file_size: 4096,
            has_wal: false,
            has_shm: false,
            mem_count: Some(1),
            archived_count: Some(0),
            vec_rowid_count: Some(1),
            none_domain_count: Some(0),
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
            auto_fix_actions: vec![],
            quarantine_dir: Some("/tmp/q".to_string()),
            generated_at: "2026-04-28T00:00:00+00:00".to_string(),
        }
    }

    #[test]
    fn populate_filters_and_classifies_roles() {
        let mut m = Manifest::empty();
        let report = mk_report(vec![
            mk_finding(
                "/u/.tachi/global/memory.db",
                DbClassification::Healthy,
                "global",
            ),
            mk_finding(
                "/u/.tachi/projects/quant/memory.db",
                DbClassification::Healthy,
                "project:quant",
            ),
            mk_finding(
                "/u/.openclaw/extensions/tachi/data/agents/main/memory.db",
                DbClassification::Healthy,
                "openclaw-agent:main",
            ),
            mk_finding(
                "/u/.tachi/junk.db",
                DbClassification::Placeholder,
                "tachi-other",
            ),
            mk_finding(
                "/u/.tachi/foo.db.bak",
                DbClassification::Backup,
                "tachi-other",
            ),
            mk_finding(
                "/u/.tachi/dead.db",
                DbClassification::Corrupt,
                "tachi-other",
            ),
        ]);
        m.populate_from_doctor(&report);
        assert_eq!(m.dbs.len(), 3, "only healthy/wal/legacy should be recorded");
        assert_eq!(
            m.global().map(|e| e.path.as_str()),
            Some("/u/.tachi/global/memory.db")
        );
        assert_eq!(m.by_role(DbRole::Project).len(), 1);
        assert_eq!(m.by_role(DbRole::Agent).len(), 1);
        let agent = m.by_role(DbRole::Agent)[0];
        assert_eq!(agent.owner, "openclaw-agent:main");
    }

    #[test]
    fn save_and_load_roundtrip_preserves_notes() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("manifest.json");
        let mut m = Manifest::empty();
        m.populate_from_doctor(&mk_report(vec![mk_finding(
            "/u/.tachi/global/memory.db",
            DbClassification::Healthy,
            "global",
        )]));
        m.dbs[0].notes = "primary global store".to_string();
        m.save(&path).unwrap();

        let loaded = Manifest::load(&path).unwrap();
        assert_eq!(loaded.dbs.len(), 1);
        assert_eq!(loaded.dbs[0].notes, "primary global store");

        // Re-populating should preserve the note.
        let mut m2 = loaded.clone();
        m2.populate_from_doctor(&mk_report(vec![mk_finding(
            "/u/.tachi/global/memory.db",
            DbClassification::Healthy,
            "global",
        )]));
        assert_eq!(m2.dbs[0].notes, "primary global store");
    }

    #[test]
    fn allow_write_for_healthy_and_wal_orphan() {
        // PR-A: WalOrphan is now write-allowed because a non-empty -wal file
        // is the expected state for any DB held open by the live daemon, and
        // SQLite recovers WAL automatically on next open. LegacySchema still
        // blocks (real schema mismatch).
        let mut m = Manifest::empty();
        m.populate_from_doctor(&mk_report(vec![
            mk_finding("/u/a.db", DbClassification::Healthy, "project:a"),
            mk_finding("/u/b.db", DbClassification::WalOrphan, "project:b"),
            mk_finding("/u/c.db", DbClassification::LegacySchema, "project:c"),
        ]));
        let by_path: std::collections::HashMap<_, _> = m
            .dbs
            .iter()
            .map(|e| (e.path.clone(), e.allow_write))
            .collect();
        assert_eq!(by_path["/u/a.db"], true);
        assert_eq!(by_path["/u/b.db"], true, "WalOrphan must be writable");
        assert_eq!(by_path["/u/c.db"], false);
    }

    #[test]
    fn lookup_returns_entry() {
        let mut m = Manifest::empty();
        m.populate_from_doctor(&mk_report(vec![mk_finding(
            "/u/.tachi/global/memory.db",
            DbClassification::Healthy,
            "global",
        )]));
        assert!(m.lookup("/u/.tachi/global/memory.db").is_some());
        assert!(m.lookup("/u/missing.db").is_none());
    }

    #[test]
    fn check_writable_enforces_allow_write() {
        let mut m = Manifest::empty();
        m.populate_from_doctor(&mk_report(vec![
            mk_finding("/u/healthy.db", DbClassification::Healthy, "project:a"),
            mk_finding("/u/orphan.db", DbClassification::WalOrphan, "project:b"),
            mk_finding("/u/legacy.db", DbClassification::LegacySchema, "project:c"),
        ]));
        // PR-A: WalOrphan now writable (live daemon holds non-empty WAL).
        assert!(m.check_writable("/u/healthy.db").is_ok());
        assert!(
            m.check_writable("/u/orphan.db").is_ok(),
            "WalOrphan must be writable — SQLite recovers WAL on open"
        );
        // LegacySchema still blocks writes (real schema mismatch).
        match m.check_writable("/u/legacy.db") {
            Err(ManifestGuardError::WriteForbidden { .. }) => {}
            other => panic!("expected WriteForbidden for legacy, got {other:?}"),
        }
        match m.check_writable("/u/never-seen.db") {
            Err(ManifestGuardError::NotInManifest { .. }) => {}
            other => panic!("expected NotInManifest, got {other:?}"),
        }
    }

    #[test]
    fn plan_sweep_skips_owned_and_targets_only_placeholders_and_backups() {
        let mut m = Manifest::empty();
        m.populate_from_doctor(&mk_report(vec![mk_finding(
            "/u/.tachi/global/memory.db",
            DbClassification::Healthy,
            "global",
        )]));
        // doctor sees: 1 owned (must be skipped), 1 placeholder, 1 backup, 1 corrupt (ignored)
        let report = mk_report(vec![
            mk_finding(
                "/u/.tachi/global/memory.db",
                DbClassification::Healthy,
                "global",
            ),
            mk_finding(
                "/u/.tachi/junk.db",
                DbClassification::Placeholder,
                "tachi-other",
            ),
            mk_finding(
                "/u/.tachi/foo.db.bak",
                DbClassification::Backup,
                "tachi-other",
            ),
            mk_finding(
                "/u/.tachi/dead.db",
                DbClassification::Corrupt,
                "tachi-other",
            ),
        ]);
        let qdir = std::path::Path::new("/tmp/q");
        let plan = plan_sweep(&report, &m, qdir);
        assert_eq!(
            plan.planned.len(),
            2,
            "placeholder + backup should be planned"
        );
        let paths: Vec<_> = plan.planned.iter().map(|a| a.path.as_str()).collect();
        assert!(paths.contains(&"/u/.tachi/junk.db"));
        assert!(paths.contains(&"/u/.tachi/foo.db.bak"));
        assert_eq!(plan.skipped.len(), 1, "owned global.db should be skipped");
        assert_eq!(plan.skipped[0].path, "/u/.tachi/global/memory.db");
    }

    #[test]
    fn plan_sweep_refuses_files_outside_tachi_roots() {
        // Manifest with one owned DB in /home/user/.tachi/global/
        let mut m = Manifest::empty();
        m.populate_from_doctor(&mk_report(vec![mk_finding(
            "/home/user/.tachi/global/memory.db",
            DbClassification::Healthy,
            "global",
        )]));
        // Doctor sees a placeholder INSIDE a Tachi root → should plan,
        // and another placeholder OUTSIDE Tachi roots → should skip.
        let report = mk_report(vec![
            mk_finding(
                "/home/user/.tachi/junk.db",
                DbClassification::Placeholder,
                "tachi-other",
            ),
            mk_finding(
                "/home/user/Desktop/Project/data/cache.db",
                DbClassification::Placeholder,
                "tachi-other",
            ),
        ]);
        let plan = plan_sweep(&report, &m, std::path::Path::new("/tmp/q"));
        assert_eq!(
            plan.planned.len(),
            1,
            "only the in-Tachi-root file should be planned"
        );
        assert_eq!(plan.planned[0].path, "/home/user/.tachi/junk.db");
        let outside_skip = plan
            .skipped
            .iter()
            .find(|a| a.path == "/home/user/Desktop/Project/data/cache.db");
        assert!(
            outside_skip.is_some(),
            "outside-roots file must be skipped, not planned"
        );
        assert!(outside_skip
            .unwrap()
            .note
            .contains("outside Tachi-owned roots"));
    }

    #[test]
    fn plan_sweep_assigns_unique_quarantine_names_for_collisions() {
        let m = Manifest::empty();
        let report = mk_report(vec![
            mk_finding(
                "/home/user/.tachi/a/dup.db",
                DbClassification::Placeholder,
                "tachi-other",
            ),
            mk_finding(
                "/home/user/.tachi/b/dup.db",
                DbClassification::Placeholder,
                "tachi-other",
            ),
            mk_finding(
                "/home/user/.tachi/c/dup.db",
                DbClassification::Placeholder,
                "tachi-other",
            ),
        ]);
        let dir = tempdir().unwrap();
        let plan = plan_sweep(&report, &m, dir.path());
        assert_eq!(plan.planned.len(), 3);
        let names: std::collections::HashSet<_> = plan
            .planned
            .iter()
            .filter_map(|a| a.quarantine_to.clone())
            .collect();
        assert_eq!(names.len(), 3, "quarantine targets must be unique");
    }

    #[test]
    fn apply_sweep_moves_files_to_quarantine() {
        let dir = tempdir().unwrap();
        let qdir = dir.path().join("quarantine");
        // Path must look like it's inside a Tachi root so the safety gate allows it.
        let tachi_dir = dir.path().join(".tachi");
        std::fs::create_dir_all(&tachi_dir).unwrap();
        let bad = tachi_dir.join("placeholder.db");
        std::fs::write(&bad, b"").unwrap();

        let m = Manifest::empty(); // empty manifest → bad is unowned, but inside .tachi
        let report = mk_report(vec![mk_finding(
            bad.to_string_lossy().as_ref(),
            DbClassification::Placeholder,
            "tachi-other",
        )]);
        let plan = plan_sweep(&report, &m, &qdir);
        assert_eq!(
            plan.planned.len(),
            1,
            "placeholder under .tachi should be planned"
        );
        let result = apply_sweep(plan, &qdir);
        assert_eq!(result.applied.len(), 1, "placeholder should be moved");
        assert!(!bad.exists(), "original placeholder gone");
    }
}
