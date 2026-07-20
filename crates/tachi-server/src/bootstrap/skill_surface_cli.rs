use serde::Serialize;
use std::path::PathBuf;

mod command;
mod print;
mod projection;
mod sources;
mod stores;
mod sync_plan;
#[cfg(test)]
mod tests;

pub(super) use command::run_skill_surface_command;

const SUPPORTED_HOSTS: &[&str] = &["claude", "codex", "gemini", "cursor", "antigravity"];
const SOURCE_STATUS_NOT_CHECKED: &str = "not_checked";
const SOURCE_STATUS_NOT_CHECKED_REASON: &str =
    "read-only pinned manifest status; no network fetch or upstream diff was run";

struct SkillSourceManifestSpec {
    corpus: &'static str,
    /// Repo-relative path of the corpus manifest. Read at *runtime* through
    /// the crate-level skill source resolver (see `sources::read_skill_source_manifest`),
    /// not embedded at compile time — `skill/` is a host-absolute git symlink
    /// (kckylechen1/tachi#895), so `include_str!` against it is neither
    /// hermetic (same SHA, different binary per host) nor portable (a
    /// checkout without that host path can't build at all).
    path: &'static str,
}

const SKILL_SOURCE_MANIFESTS: &[SkillSourceManifestSpec] = &[
    SkillSourceManifestSpec {
        corpus: "superpowers",
        path: "skill/superpowers/manifest.yaml",
    },
    SkillSourceManifestSpec {
        corpus: "waza",
        path: "skill/waza/manifest.yaml",
    },
];

/// Resolve + read a `SkillSourceManifestSpec`'s manifest content at call
/// time, through the same runtime resolver that flow-stage injection and
/// `builtins::helpers` capability seeding already use.
///
/// Unlike builtin-capability seeding, this is an on-demand CLI read (`tachi
/// skill-surface sources` / `sync-plan`), not a server-boot seed pass — an
/// unresolvable manifest here surfaces as a clear command error, not a
/// silently-degraded stub.
fn read_skill_source_manifest_content(spec: &SkillSourceManifestSpec) -> Result<String, String> {
    let resolved = crate::skill_source_resolver::resolve_vendored_skill_path(spec.path)
        .ok_or_else(|| {
            format!(
            "{MISSING_VENDORED_MANIFEST_PREFIX}{}: not found in repo root, cwd, cargo manifest \
             dir, or the central vendored-skills library (set $TACHI_SKILLS_ROOT, or mount \
             ~/.agents/vendored-skills)",
            spec.path
        )
        })?;
    std::fs::read_to_string(&resolved).map_err(|e| format!("read {}: {e}", resolved.display()))
}

/// Marker prefix for the one *expected* `build_skill_source_report()` error
/// class: the optional vendored-skills library (tachi#895) simply isn't
/// mounted on this host. Every other error (manifest resolved but unreadable,
/// manifest present but malformed YAML, etc.) is *unexpected* and must not be
/// swallowed the same way (tachi#911 tail sweep, #909 residual: a prior guard
/// skipped on any `Err` wholesale, which would also hide a real parser
/// regression behind a "library not mounted" message).
const MISSING_VENDORED_MANIFEST_PREFIX: &str = "manifest ";

/// Classify a `build_skill_source_report`/`read_skill_source_manifest_content`
/// error string: `true` only for the expected "optional fixture not mounted"
/// case, `false` for anything else (which callers should propagate or log at
/// WARN instead of silently skipping). Only the test-hermeticity guard in
/// `tests.rs` needs this today — the real CLI command (`command.rs`) already
/// propagates every `build_skill_source_report()` error via `?`, which is
/// the correct "don't skip" behavior for a live command.
#[cfg(test)]
fn is_missing_vendored_manifest_error(err: &str) -> bool {
    err.starts_with(MISSING_VENDORED_MANIFEST_PREFIX) && err.contains("not found in repo root")
}

#[derive(Debug, Clone)]
struct SkillStoreSpec {
    id: &'static str,
    role: &'static str,
    path: PathBuf,
    format: SkillStoreFormat,
}

#[derive(Debug, Clone, Copy)]
enum SkillStoreFormat {
    SkillMd,
    CursorMdc,
    None,
}

#[derive(Debug, Clone, Serialize)]
struct SkillStoreSummary {
    id: String,
    role: String,
    path: String,
    format: String,
    exists: bool,
    entries: usize,
    skills_with_content: usize,
    symlinks: usize,
    broken_symlinks: usize,
    missing_skill_files: usize,
}

#[derive(Debug, Clone, Serialize)]
struct SkillEntryStatus {
    store: String,
    role: String,
    name: String,
    path: String,
    is_symlink: bool,
    symlink_target: Option<String>,
    target_exists: Option<bool>,
    hash: Option<String>,
    issues: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
struct SkillHashGroup {
    hash: String,
    stores: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
struct SkillDriftGroup {
    name: String,
    hashes: Vec<SkillHashGroup>,
}

#[derive(Debug, Clone, Serialize)]
struct CcSwitchSkillRow {
    name: String,
    directory: String,
    content_hash: Option<String>,
    enabled_hosts: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
struct HostProjectionCheck {
    host: String,
    path: String,
    exists: bool,
    is_symlink: bool,
    symlink_target: Option<String>,
    points_to_cc_switch: bool,
    content_matches_cc_switch: bool,
}

#[derive(Debug, Clone, Serialize)]
struct CcSwitchProjectionStatus {
    name: String,
    enabled_hosts: Vec<String>,
    checks: Vec<HostProjectionCheck>,
    status: String,
    issues: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Default)]
struct SkillSurfaceSummary {
    stores: usize,
    entries: usize,
    symlinks: usize,
    broken_symlinks: usize,
    drift_groups: usize,
    cc_switch_skills: usize,
    cc_switch_projection_issues: usize,
}

#[derive(Debug, Clone, Serialize)]
struct SkillSurfaceReport {
    schema_version: String,
    generated_at: String,
    home: String,
    hosts: Vec<String>,
    summary: SkillSurfaceSummary,
    stores: Vec<SkillStoreSummary>,
    entries: Vec<SkillEntryStatus>,
    drift_groups: Vec<SkillDriftGroup>,
    cc_switch_db: Option<String>,
    cc_switch_skills: Vec<CcSwitchSkillRow>,
    cc_switch_projection_status: Vec<CcSwitchProjectionStatus>,
}

#[derive(Debug, Clone, Serialize, Default)]
struct SkillSourceReportSummary {
    corpora: usize,
    skills: usize,
    upstream_managed: usize,
    native_contracts: usize,
    local_review: usize,
    missing_metadata: usize,
}

#[derive(Debug, Clone, Serialize)]
struct SkillSourceReport {
    schema_version: String,
    generated_at: String,
    summary: SkillSourceReportSummary,
    corpora: Vec<SkillSourceCorpusStatus>,
}

#[derive(Debug, Clone, Serialize)]
struct SkillSourceCorpusStatus {
    corpus: String,
    manifest_path: String,
    updated: Option<String>,
    upstream: SkillSourceUpstreamStatus,
    skills: Vec<SkillSourceSkillStatus>,
    summary: SkillSourceReportSummary,
}

#[derive(Debug, Clone, Serialize)]
struct SkillSourceUpstreamStatus {
    repo: Option<String>,
    pinned_ref: Option<String>,
    pinned_sha: Option<String>,
    update_policy: Option<String>,
    latest_status: String,
    diff_status: String,
    status_reason: String,
}

#[derive(Debug, Clone, Serialize)]
struct SkillSourceSkillStatus {
    id: String,
    name: String,
    local_path: String,
    source: SkillSourceMetadata,
    metadata_status: String,
    latest_status: String,
    diff_status: String,
}

#[derive(Debug, Clone, Serialize, Default)]
struct SkillSourceMetadata {
    kind: Option<String>,
    repo: Option<String>,
    path: Option<String>,
    pinned_ref: Option<String>,
    pinned_sha: Option<String>,
    update_policy: Option<String>,
    local_overlay: Option<String>,
}

#[derive(Debug, Clone, Default)]
struct ParsedSkillSourceManifest {
    updated: Option<String>,
    corpus: String,
    upstream: SkillSourceMetadata,
    skills: Vec<ParsedSkillSourceEntry>,
}

#[derive(Debug, Clone, Default)]
struct ParsedSkillSourceEntry {
    id: String,
    name: String,
    local_path: String,
    source: SkillSourceMetadata,
}
