use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

use crate::doctor::{DbClassification, DoctorFinding, DoctorReport};

use super::{canonicalize_db_path, should_skip_path, MANIFEST_SCHEMA_VERSION};

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
        crate::utils::write_owner_only_file_atomic(path, json.as_bytes()).map_err(|e| {
            std::io::Error::new(
                std::io::ErrorKind::Other,
                format!("manifest atomic write: {e}"),
            )
        })
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
            // #736: for a repo-local `<repo>/.tachi/memory.db` DB, derive the
            // display/scope_hint identity FRESH from the project root every
            // refresh, instead of trusting whichever alias directory this
            // finding happened to be scanned under. `scope_hint_for` just
            // echoes the literal on-disk alias dir name — if that dir
            // predates a hash-derivation change (issue #736) or the scan
            // never reaches a same-named alias at all, the stored hint goes
            // stale and two dirs for the same DB can display two different
            // names. Recomputing from the root makes the label a pure
            // function of the canonical file's identity: one DB, one name,
            // always current, regardless of alias-dir presence or scan order.
            //
            // This is a PURE in-memory relabel: it reads the project root
            // path and re-derives the name, but never creates, deletes, or
            // moves any alias directory or symlink. A plain `tachi status` /
            // `doctor` / `manifest refresh` must not mutate the filesystem.
            // Physical alias-dir retirement (adopting the new-name dir,
            // retiring drifted old-hash dirs) is a separate `--fix`-gated
            // operation tracked in #743.
            let scope_hint = if f.scope_hint == "global" {
                "global".to_string()
            } else if let Some(project_root) =
                crate::path_utils::plan_c_project_root_from_local_db(&canon)
            {
                let current_name = crate::path_utils::plan_c_dir_name_from_root(&project_root)
                    .or_else(|| crate::path_utils::plan_c_legacy_dir_name_from_root(&project_root));
                match current_name {
                    Some(name) => format!("project:{name}"),
                    None => f.scope_hint.clone(),
                }
            } else {
                f.scope_hint.clone()
            };

            // FIX-D (#736): derive role/owner from the CORRECTED scope hint,
            // not the stale one on the finding. Otherwise a repo-local DB
            // whose stored hint drifted to an old-hash name would show a
            // `project:*` scope but `DbRole::Unknown` — a fresh mismatch this
            // relabel would introduce. Scope and role must agree.
            let role = classify_role(&scope_hint, &f.path);
            let owner = derive_owner(&scope_hint);
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
                scope_hint,
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

    /// Resolve the preferred writable agent DB for an OpenClaw agent id.
    ///
    /// This prefers the canonical extension path:
    /// `~/.openclaw/extensions/tachi/data/agents/<agent_id>/memory.db`
    /// then local agent paths under `~/.openclaw/agents/`, and only then
    /// other writable agent DBs whose owner/scope matches the id.
    pub fn resolve_agent_db_path(&self, agent_id: &str) -> Option<PathBuf> {
        let agent_id = agent_id.trim();
        if agent_id.is_empty() {
            return None;
        }
        let owner_ext = format!("openclaw-agent:{agent_id}");
        let owner_local = format!("openclaw-agent-local:{agent_id}");
        let ext_suffix = format!("/.openclaw/extensions/tachi/data/agents/{agent_id}/memory.db");
        let local_mem_suffix = format!("/.openclaw/agents/{agent_id}/memory/memory.db");
        let local_tachi_suffix = format!("/.openclaw/agents/{agent_id}/.tachi/memory.db");

        self.dbs
            .iter()
            .filter(|e| {
                e.allow_write
                    && e.schema_kind == "tachi"
                    && e.role == DbRole::Agent
                    && (e.owner == owner_ext
                        || e.owner == owner_local
                        || e.scope_hint.ends_with(&format!(":{agent_id}")))
            })
            .max_by_key(|e| {
                let path = e.path.replace('\\', "/");
                let mut score = 0i32;
                if path.ends_with(&ext_suffix) {
                    score += 400;
                }
                if path.ends_with(&local_mem_suffix) {
                    score += 300;
                }
                if path.ends_with(&local_tachi_suffix) {
                    score += 250;
                }
                if e.owner == owner_ext {
                    score += 100;
                }
                if e.owner == owner_local {
                    score += 50;
                }
                score
            })
            .map(|e| PathBuf::from(&e.path))
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

/// Classify a DB's role from its scope hint and path.
///
/// Takes the scope hint as an explicit argument (rather than reading it off
/// the `DoctorFinding`) so callers can pass the #736-corrected scope hint —
/// a repo-local DB whose stored hint drifted to an old-hash name must end up
/// with a role consistent with its CURRENT `project:*` label, not
/// `DbRole::Unknown`.
fn classify_role(scope_hint: &str, path: &str) -> DbRole {
    let s = scope_hint;
    if s == "global" {
        DbRole::Global
    } else if s.starts_with("project:") {
        DbRole::Project
    } else if s.starts_with("openclaw-agent") {
        DbRole::Agent
    } else if path.contains("/foundry/") || path.contains("/foundry.db") {
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
