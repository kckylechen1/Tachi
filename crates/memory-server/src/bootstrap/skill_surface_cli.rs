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
    path: &'static str,
    content: &'static str,
}

const SKILL_SOURCE_MANIFESTS: &[SkillSourceManifestSpec] = &[
    SkillSourceManifestSpec {
        corpus: "superpowers",
        path: "skill/superpowers/manifest.yaml",
        content: include_str!("../../../../skill/superpowers/manifest.yaml"),
    },
    SkillSourceManifestSpec {
        corpus: "waza",
        path: "skill/waza/manifest.yaml",
        content: include_str!("../../../../skill/waza/manifest.yaml"),
    },
];

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
