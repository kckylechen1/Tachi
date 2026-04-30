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

/// Schema kind detected by inspecting `sqlite_master`. Used by the GC pass to
/// reclassify entries that were tagged `tachi` by historical bugs but actually
/// hold legacy OpenClaw `chunks` tables (audit B8).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SchemaKind {
    /// `memories` table with the canonical Tachi columns present.
    Tachi,
    /// `chunks` table present, no `memories` table — legacy OpenClaw store.
    /// Wire form is `openclaw_legacy` for backward compatibility with the
    /// historical `schema_kind` string in `~/.tachi/manifest.json`.
    #[serde(rename = "openclaw_legacy")]
    OpenclawChunks,
    /// Neither table found, or open/probe failed.
    Unknown,
}

impl SchemaKind {
    /// Stable string form used in the manifest JSON. Matches the historical
    /// `schema_kind` values so existing entries deserialize without churn.
    pub fn as_str(&self) -> &'static str {
        match self {
            // Historical manifest used `tachi` / `openclaw_legacy` / `unknown`.
            // Keep `openclaw_legacy` on the wire for backward compatibility;
            // the enum variant is named after the table for clarity.
            SchemaKind::Tachi => "tachi",
            SchemaKind::OpenclawChunks => "openclaw_legacy",
            SchemaKind::Unknown => "unknown",
        }
    }
}

/// Canonicalize a DB path. Falls back to the input path if the file does not
/// exist (e.g. during GC of an entry whose file was moved). Never errors.
///
/// Used as the dedup key everywhere a manifest entry is added or compared.
pub fn canonicalize_db_path(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

/// Returns `Some(reason)` if the path looks like a test fixture / vendored
/// artifact and should NOT be tracked in the live manifest. Patterns:
///
///   * `/node_modules/.../tmp/*.db` (vite zread fixtures, audit B12)
///   * `/node_modules/` with parent dir named `tmp`/`tests`/`test-fixtures`
///   * filename matches `*.test.db` or `*.fixture.db`
///   * filename is `feature-daemon-global.db` AND path contains `vitejs` or
///     `zread` (belt-and-suspenders for the vite zread fixture pattern)
pub fn should_skip_path(p: &Path) -> Option<&'static str> {
    let path_str = p.to_string_lossy();
    let file_name = p
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    let parent_name = p
        .parent()
        .and_then(|pp| pp.file_name())
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();

    // node_modules + tmp anywhere in path
    if path_str.contains("/node_modules/") && path_str.contains("/tmp/") {
        return Some("node_modules tmp fixture");
    }
    // node_modules .db with fixture-shaped parent
    if path_str.contains("/node_modules/")
        && file_name.ends_with(".db")
        && matches!(parent_name.as_str(), "tmp" | "tests" | "test-fixtures")
    {
        return Some("node_modules test fixture");
    }
    // generic *.test.db / *.fixture.db
    if file_name.ends_with(".test.db") {
        return Some("*.test.db");
    }
    if file_name.ends_with(".fixture.db") {
        return Some("*.fixture.db");
    }
    // vite zread fixture (B12 belt + suspenders)
    if file_name == "feature-daemon-global.db"
        && (path_str.contains("vitejs") || path_str.contains("zread"))
    {
        return Some("vite zread feature-daemon-global.db fixture");
    }
    None
}

/// Open the SQLite file read-only and decide its schema kind by inspecting
/// `sqlite_master`. Returns `Unknown` if the file cannot be opened or probed.
///
/// Tachi requires both `memories` table presence AND at least the canonical
/// columns (`id`, `text`, `path`, `timestamp`) to call it Tachi — this avoids
/// classifying an OpenClaw legacy DB that happens to also have a stub
/// `memories` view as Tachi.
pub fn classify_db_schema(path: &Path) -> SchemaKind {
    use rusqlite::OpenFlags;
    if !path.exists() {
        return SchemaKind::Unknown;
    }
    // Read-only, no WAL writes. URI form mirrors doctor.rs.
    let uri = format!(
        "file:{}?mode=ro&immutable=1",
        path.to_string_lossy().replace('?', "%3F")
    );
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI;
    let conn = match rusqlite::Connection::open_with_flags(&uri, flags) {
        Ok(c) => c,
        Err(_) => return SchemaKind::Unknown,
    };

    let has_memories: bool = conn
        .query_row(
            "select count(*) from sqlite_master where type='table' and name='memories'",
            [],
            |r| r.get::<_, i64>(0),
        )
        .map(|n| n > 0)
        .unwrap_or(false);
    let has_chunks: bool = conn
        .query_row(
            "select count(*) from sqlite_master where type='table' and name='chunks'",
            [],
            |r| r.get::<_, i64>(0),
        )
        .map(|n| n > 0)
        .unwrap_or(false);

    if has_memories {
        // Verify canonical columns are present before declaring Tachi.
        let cols: std::collections::HashSet<String> = conn
            .prepare("pragma table_info(memories)")
            .and_then(|mut stmt| {
                let rows = stmt.query_map([], |r| r.get::<_, String>(1))?;
                let mut out = std::collections::HashSet::new();
                for row in rows.flatten() {
                    out.insert(row);
                }
                Ok(out)
            })
            .unwrap_or_default();
        let required = ["id", "text", "path", "timestamp"];
        if required.iter().all(|c| cols.contains(*c)) {
            return SchemaKind::Tachi;
        }
        // memories table exists but missing canonical columns → fall through.
    }
    if has_chunks {
        return SchemaKind::OpenclawChunks;
    }
    SchemaKind::Unknown
}

/// Report from `gc_manifest`. Counts mutations applied during the GC pass.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GcReport {
    pub canonicalized: usize,
    pub removed_missing: usize,
    pub removed_fixture: usize,
    pub schema_kind_fixed: usize,
    pub dedup_collapsed: usize,
    /// Total entries before GC.
    pub entries_before: usize,
    /// Total entries after GC.
    pub entries_after: usize,
    /// True if GC was aborted (e.g. >50% removal sanity guard tripped).
    pub aborted: bool,
    /// Optional human-readable abort/skip reason.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub abort_reason: Option<String>,
}

/// Run a GC / hygiene pass over the manifest file at `manifest_path`:
///   1. Load the manifest (returns Ok with empty report if file missing).
///   2. Write a one-shot `{manifest_path}.bak` backup (best-effort).
///   3. For each entry: canonicalize path, drop if file is missing, drop if
///      `should_skip_path` matches, re-classify `schema_kind`.
///   4. Dedup by canonical path (prefer entry with most-recent `last_doctor_at`).
///   5. Sanity guard: if would remove >50% of entries, ABORT, return a report
///      flagged `aborted: true` and DO NOT mutate the on-disk manifest.
///   6. Atomic save (write `.tmp`, rename).
///
/// Idempotent: a second invocation reports all-zero counters.
pub fn gc_manifest(manifest_path: &Path) -> std::io::Result<GcReport> {
    if !manifest_path.exists() {
        return Ok(GcReport::default());
    }
    let mut manifest = Manifest::load(manifest_path)?;
    let mut report = GcReport {
        entries_before: manifest.dbs.len(),
        ..GcReport::default()
    };

    // 2. Best-effort backup. Don't fail the GC if backup write fails (e.g.
    //    read-only fs); the user still has the original manifest because we
    //    write atomically below.
    let backup_path: PathBuf = {
        let mut s = manifest_path.as_os_str().to_os_string();
        s.push(".bak");
        PathBuf::from(s)
    };
    if let Ok(orig_bytes) = std::fs::read(manifest_path) {
        let _ = std::fs::write(&backup_path, &orig_bytes);
    }

    // 3. Per-entry pass.
    let original = std::mem::take(&mut manifest.dbs);
    // Map canonical path -> chosen entry. Last-write-wins preferring
    // most-recent `last_doctor_at`.
    let mut by_canon: std::collections::HashMap<PathBuf, DbEntry> =
        std::collections::HashMap::new();
    let mut order: Vec<PathBuf> = Vec::new();
    let mut removed_total: usize = 0;

    for mut entry in original {
        let raw_path = PathBuf::from(&entry.path);

        // (a) fixture skip-list — applies before any I/O.
        if let Some(reason) = should_skip_path(&raw_path) {
            tracing::debug!(target: "tachi::manifest::gc", path=%entry.path, reason, "skip fixture");
            report.removed_fixture += 1;
            removed_total += 1;
            continue;
        }
        // (b) missing on disk → drop.
        if !raw_path.exists() {
            tracing::debug!(target: "tachi::manifest::gc", path=%entry.path, "drop missing");
            report.removed_missing += 1;
            removed_total += 1;
            continue;
        }
        // (c) canonicalize.
        let canon = canonicalize_db_path(&raw_path);
        let canon_str = canon.to_string_lossy().to_string();
        if canon_str != entry.path {
            tracing::debug!(target: "tachi::manifest::gc", from=%entry.path, to=%canon_str, "canonicalize");
            entry.path = canon_str.clone();
            report.canonicalized += 1;
        }
        // (d) re-classify schema. Only correct mismatches; don't downgrade
        //     on a transient open failure (Unknown).
        let detected = classify_db_schema(&canon);
        if detected != SchemaKind::Unknown && detected.as_str() != entry.schema_kind {
            tracing::debug!(
                target: "tachi::manifest::gc",
                path=%entry.path,
                from=%entry.schema_kind,
                to=%detected.as_str(),
                "schema_kind fix"
            );
            entry.schema_kind = detected.as_str().to_string();
            report.schema_kind_fixed += 1;
        }
        // (e) dedup by canonical path.
        match by_canon.get(&canon) {
            Some(existing) => {
                // Prefer the entry with the most recent last_doctor_at.
                let keep_new = entry.last_doctor_at > existing.last_doctor_at;
                if keep_new {
                    by_canon.insert(canon.clone(), entry);
                } // else discard the new one
                report.dedup_collapsed += 1;
                removed_total += 1;
                tracing::debug!(target: "tachi::manifest::gc", path=%canon.display(), "dedup collapse");
            }
            None => {
                by_canon.insert(canon.clone(), entry);
                order.push(canon);
            }
        }
    }

    // 5. Sanity guard.
    let kept = by_canon.len();
    if report.entries_before > 0 && removed_total * 2 > report.entries_before {
        report.aborted = true;
        report.abort_reason = Some(format!(
            "would remove {removed_total}/{} entries (>50%); aborting GC, on-disk manifest unchanged",
            report.entries_before
        ));
        report.entries_after = report.entries_before;
        tracing::error!(
            target: "tachi::manifest::gc",
            removed = removed_total,
            before = report.entries_before,
            "aborting GC: sanity guard tripped"
        );
        return Ok(report);
    }

    // Re-assemble in original-encounter order; sort by path for stable output
    // (matches populate_from_doctor's behaviour).
    let mut new_dbs: Vec<DbEntry> = order
        .into_iter()
        .filter_map(|k| by_canon.remove(&k))
        .collect();
    new_dbs.sort_by(|a, b| a.path.cmp(&b.path));
    manifest.dbs = new_dbs;
    manifest.generated_at = Utc::now().to_rfc3339();
    report.entries_after = manifest.dbs.len();
    let _ = kept; // silence unused on some configs

    // 6. Atomic save.
    manifest.save(manifest_path)?;
    Ok(report)
}

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
    ///
    /// Hygiene applied at registration time (PR-2):
    ///   * Paths are canonicalized via [`canonicalize_db_path`] before dedup,
    ///     collapsing symlink aliases.
    ///   * Paths matching [`should_skip_path`] (test fixtures, vendored
    ///     `node_modules` artifacts) are dropped.
    pub fn populate_from_doctor(&mut self, report: &DoctorReport) {
        use std::collections::HashMap;
        let prior_notes: HashMap<String, String> = self
            .dbs
            .iter()
            .map(|e| (e.path.clone(), e.notes.clone()))
            .collect();

        // Canonical path -> entry. Last-write-wins by `last_doctor_at` (within
        // a single populate, all entries share the report timestamp, so first
        // wins; this is a defensive placeholder for future per-entry stamps).
        let mut by_canon: HashMap<PathBuf, DbEntry> = HashMap::new();
        let mut order: Vec<PathBuf> = Vec::new();
        for f in &report.findings {
            if !should_record(f) {
                continue;
            }
            let raw_path = std::path::Path::new(&f.path);
            if let Some(reason) = should_skip_path(raw_path) {
                tracing::debug!(
                    target: "tachi::manifest",
                    path = %f.path,
                    reason,
                    "skip fixture path during populate"
                );
                continue;
            }
            let canon = canonicalize_db_path(raw_path);
            let canon_str = canon.to_string_lossy().to_string();
            if by_canon.contains_key(&canon) {
                tracing::debug!(
                    target: "tachi::manifest",
                    alias = %f.path,
                    canon = %canon_str,
                    "skip symlink alias for already-registered canonical path"
                );
                continue;
            }
            let role = classify_role(f);
            let owner = derive_owner(&f.scope_hint);
            let allow_write = matches!(
                f.classification,
                DbClassification::Healthy | DbClassification::WalOrphan
            );
            // Prefer prior notes keyed under either the old (pre-canonicalize)
            // path or the canonical path.
            let notes = prior_notes
                .get(&canon_str)
                .cloned()
                .or_else(|| prior_notes.get(&f.path).cloned())
                .unwrap_or_default();
            let entry = DbEntry {
                path: canon_str.clone(),
                role,
                owner,
                schema_kind: f.schema_kind.clone(),
                vec_enabled: f.vec_rowid_count.is_some(),
                allow_write,
                last_doctor_at: report.generated_at.clone(),
                last_classification: f.classification.as_str().to_string(),
                scope_hint: f.scope_hint.clone(),
                notes,
            };
            by_canon.insert(canon.clone(), entry);
            order.push(canon);
        }
        let mut new_dbs: Vec<DbEntry> = order
            .into_iter()
            .filter_map(|k| by_canon.remove(&k))
            .collect();
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

    /// All entries with a given role. Currently only used by tests; gated
    /// with cfg(test) to keep the build warning-free.
    #[cfg(test)]
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

    // ─── PR-2: hygiene / GC tests ──────────────────────────────────────────

    #[cfg(unix)]
    #[test]
    fn canonicalize_db_path_collapses_symlink() {
        let dir = tempdir().unwrap();
        let real = dir.path().join("real.db");
        std::fs::write(&real, b"x").unwrap();
        let link = dir.path().join("alias.db");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let real_canon = canonicalize_db_path(&real);
        let link_canon = canonicalize_db_path(&link);
        assert_eq!(real_canon, link_canon, "symlink alias must canonicalize to target");
    }

    #[test]
    fn canonicalize_db_path_falls_back_for_missing_files() {
        let p = Path::new("/definitely/does/not/exist/here.db");
        // Should not panic, returns the input path unchanged.
        assert_eq!(canonicalize_db_path(p), p.to_path_buf());
    }

    #[test]
    fn should_skip_path_matches_fixture_patterns() {
        // node_modules + tmp
        assert!(should_skip_path(Path::new(
            "/repo/node_modules/vitejs/vite/tmp/feature-daemon-global.db"
        ))
        .is_some());
        // node_modules + .test.db
        assert!(should_skip_path(Path::new(
            "/repo/node_modules/foo/tests/bar.db"
        ))
        .is_some());
        // *.test.db anywhere
        assert!(should_skip_path(Path::new("/some/path/foo.test.db")).is_some());
        // *.fixture.db anywhere
        assert!(should_skip_path(Path::new("/some/path/foo.fixture.db")).is_some());
        // vite zread belt-and-suspenders
        assert!(should_skip_path(Path::new(
            "/x/zread/.cache/feature-daemon-global.db"
        ))
        .is_some());
        // Real-looking Tachi DB must NOT be skipped.
        assert!(should_skip_path(Path::new("/Users/me/.tachi/global/memory.db")).is_none());
        assert!(should_skip_path(Path::new("/repo/.tachi/memory.db")).is_none());
        assert!(should_skip_path(Path::new(
            "/Users/me/.openclaw/agents/x/memory/memory.db"
        ))
        .is_none());
    }

    #[test]
    fn classify_db_schema_distinguishes_kinds() {
        let dir = tempdir().unwrap();

        // Tachi-shaped DB.
        let tachi_path = dir.path().join("tachi.db");
        let conn = rusqlite::Connection::open(&tachi_path).unwrap();
        conn.execute_batch(
            "CREATE TABLE memories (
                id TEXT PRIMARY KEY, path TEXT NOT NULL DEFAULT '/',
                summary TEXT, text TEXT, importance REAL, timestamp TEXT NOT NULL,
                category TEXT, topic TEXT
            );",
        )
        .unwrap();
        drop(conn);
        assert_eq!(classify_db_schema(&tachi_path), SchemaKind::Tachi);

        // OpenClaw chunks-shaped DB.
        let chunks_path = dir.path().join("chunks.db");
        let conn = rusqlite::Connection::open(&chunks_path).unwrap();
        conn.execute_batch("CREATE TABLE chunks (id TEXT PRIMARY KEY);")
            .unwrap();
        drop(conn);
        assert_eq!(
            classify_db_schema(&chunks_path),
            SchemaKind::OpenclawChunks
        );

        // Empty SQLite file → Unknown.
        let empty_path = dir.path().join("empty.db");
        let conn = rusqlite::Connection::open(&empty_path).unwrap();
        drop(conn);
        assert_eq!(classify_db_schema(&empty_path), SchemaKind::Unknown);

        // Missing file → Unknown.
        let missing = dir.path().join("nope.db");
        assert_eq!(classify_db_schema(&missing), SchemaKind::Unknown);
    }

    #[test]
    fn schema_kind_serde_roundtrip() {
        let json = serde_json::to_string(&SchemaKind::Tachi).unwrap();
        assert_eq!(json, "\"tachi\"");
        // Wire form: openclaw_legacy (preserves backward-compat with the
        // existing manifest schema_kind string).
        let json = serde_json::to_string(&SchemaKind::OpenclawChunks).unwrap();
        assert_eq!(json, "\"openclaw_legacy\"");
        let back: SchemaKind = serde_json::from_str("\"openclaw_legacy\"").unwrap();
        assert_eq!(back, SchemaKind::OpenclawChunks);
    }

    #[test]
    fn gc_manifest_full_flow() {
        use std::os::unix::fs::symlink;
        let dir = tempdir().unwrap();
        let manifest_path = dir.path().join("manifest.json");

        // Build five fake DBs on disk, plus one missing entry, plus one fixture.
        // (a) good Tachi DB
        let good = dir.path().join("good.db");
        let conn = rusqlite::Connection::open(&good).unwrap();
        conn.execute_batch(
            "CREATE TABLE memories (id TEXT PRIMARY KEY, path TEXT, summary TEXT, text TEXT, importance REAL, timestamp TEXT NOT NULL);",
        )
        .unwrap();
        drop(conn);

        // (b) symlink alias to (a) — should be deduped
        let alias = dir.path().join("alias.db");
        symlink(&good, &alias).unwrap();

        // (c) fixture
        let fixtures_dir = dir.path().join("node_modules/vite/tmp");
        std::fs::create_dir_all(&fixtures_dir).unwrap();
        let fixture = fixtures_dir.join("feature-daemon-global.db");
        std::fs::write(&fixture, b"sqlite-stub").unwrap();

        // (d) misclassified entry: chunks DB tagged tachi
        let chunks = dir.path().join("legacy.db");
        let conn = rusqlite::Connection::open(&chunks).unwrap();
        conn.execute_batch("CREATE TABLE chunks (id TEXT PRIMARY KEY);")
            .unwrap();
        drop(conn);

        // (e) entry with a path that no longer exists
        let missing = dir.path().join("ghost.db");
        // do not create

        // Build a manifest by hand to exercise the GC pass directly.
        let now = "2026-04-30T00:00:00+00:00".to_string();
        let mk = |p: &std::path::Path, schema: &str| DbEntry {
            path: p.to_string_lossy().to_string(),
            role: DbRole::Unknown,
            owner: "tachi".to_string(),
            schema_kind: schema.to_string(),
            vec_enabled: false,
            allow_write: true,
            last_doctor_at: now.clone(),
            last_classification: "healthy".to_string(),
            scope_hint: "test".to_string(),
            notes: String::new(),
        };
        let manifest = Manifest {
            schema_version: MANIFEST_SCHEMA_VERSION,
            generated_at: now.clone(),
            comment: String::new(),
            dbs: vec![
                mk(&good, "tachi"),
                mk(&alias, "tachi"),     // dup-of-good
                mk(&fixture, "tachi"),   // fixture → drop
                mk(&chunks, "tachi"),    // mis-tagged → fix
                mk(&missing, "tachi"),   // missing → drop
            ],
        };
        manifest.save(&manifest_path).unwrap();

        // Sanity guard: this would remove >50% (3 of 5). Confirm the guard
        // trips and the manifest is preserved untouched.
        let report = gc_manifest(&manifest_path).unwrap();
        assert!(report.aborted, "5-entry manifest with 3 removals must trip sanity guard");
        let reloaded = Manifest::load(&manifest_path).unwrap();
        assert_eq!(reloaded.dbs.len(), 5, "aborted GC must not mutate manifest");

        // Add four more good entries so removals (3) stay below 50%.
        for i in 0..4 {
            let extra = dir.path().join(format!("extra{i}.db"));
            let conn = rusqlite::Connection::open(&extra).unwrap();
            conn.execute_batch(
                "CREATE TABLE memories (id TEXT PRIMARY KEY, path TEXT, summary TEXT, text TEXT, importance REAL, timestamp TEXT NOT NULL);",
            )
            .unwrap();
            drop(conn);
            let mut m2 = Manifest::load(&manifest_path).unwrap();
            m2.dbs.push(mk(&extra, "tachi"));
            m2.save(&manifest_path).unwrap();
        }

        let report = gc_manifest(&manifest_path).unwrap();
        assert!(!report.aborted, "non-aborted: {:?}", report.abort_reason);
        assert_eq!(report.removed_fixture, 1, "the vite fixture must be dropped");
        assert_eq!(report.removed_missing, 1, "the missing entry must be dropped");
        assert_eq!(report.dedup_collapsed, 1, "alias must collapse onto good.db");
        assert_eq!(report.schema_kind_fixed, 1, "chunks DB must be re-tagged");

        // Backup file written.
        let bak = {
            let mut s = manifest_path.as_os_str().to_os_string();
            s.push(".bak");
            std::path::PathBuf::from(s)
        };
        assert!(bak.exists(), "manifest.json.bak must exist after GC");

        // Idempotency: a second run should report all zeros.
        let report2 = gc_manifest(&manifest_path).unwrap();
        assert_eq!(report2.canonicalized, 0);
        assert_eq!(report2.removed_missing, 0);
        assert_eq!(report2.removed_fixture, 0);
        assert_eq!(report2.schema_kind_fixed, 0);
        assert_eq!(report2.dedup_collapsed, 0);
        assert!(!report2.aborted);

        // Final manifest contents: good, chunks (re-tagged), and the four extras.
        let final_m = Manifest::load(&manifest_path).unwrap();
        assert_eq!(final_m.dbs.len(), 6);
        let good_canon = std::fs::canonicalize(&good).unwrap().to_string_lossy().to_string();
        let chunks_canon = std::fs::canonicalize(&chunks).unwrap().to_string_lossy().to_string();
        let chunks_entry = final_m.dbs.iter().find(|e| e.path == chunks_canon).unwrap();
        assert_eq!(chunks_entry.schema_kind, "openclaw_legacy");
        assert!(final_m.dbs.iter().any(|e| e.path == good_canon));
    }
}
