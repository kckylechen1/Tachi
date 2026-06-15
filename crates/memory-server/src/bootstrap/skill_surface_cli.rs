use crate::cli::SkillSurfaceAction;
use rusqlite::Connection;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

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

pub(super) async fn run_skill_surface_command(
    action: SkillSurfaceAction,
) -> Result<(), Box<dyn std::error::Error>> {
    match action {
        SkillSurfaceAction::Status { hosts, home, json } => {
            let home = crate::utils::resolve_home_arg(home)?;
            let report = build_skill_surface_report(&home, &hosts)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                print_skill_surface_report(&report);
            }
            Ok(())
        }
        SkillSurfaceAction::Sources { json } => {
            let report = build_skill_source_report()?;
            if json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                print_skill_source_report(&report);
            }
            Ok(())
        }
    }
}

fn build_skill_surface_report(
    home: &Path,
    host_filters: &[String],
) -> Result<SkillSurfaceReport, String> {
    let hosts = crate::utils::normalize_supported_values(
        host_filters,
        SUPPORTED_HOSTS,
        "skill-surface host",
    )?;
    let specs = skill_store_specs(home, &hosts);
    let mut stores = Vec::new();
    let mut entries = Vec::new();

    for spec in &specs {
        let (summary, mut scanned) = scan_skill_store(spec);
        stores.push(summary);
        entries.append(&mut scanned);
    }

    let drift_groups = build_drift_groups(&entries);
    let cc_switch_db = home.join(".cc-switch").join("cc-switch.db");
    let cc_switch_skills = read_cc_switch_skills(&cc_switch_db)?;
    let projection_status =
        build_cc_switch_projection_status(home, &hosts, &cc_switch_skills, &entries);

    let mut summary = SkillSurfaceSummary {
        stores: stores.len(),
        drift_groups: drift_groups.len(),
        cc_switch_skills: cc_switch_skills.len(),
        ..SkillSurfaceSummary::default()
    };
    for store in &stores {
        summary.entries += store.entries;
        summary.symlinks += store.symlinks;
        summary.broken_symlinks += store.broken_symlinks;
    }
    summary.cc_switch_projection_issues = projection_status
        .iter()
        .map(|status| status.issues.len())
        .sum();

    Ok(SkillSurfaceReport {
        schema_version: "tachi.skill_surface.status.v1".to_string(),
        generated_at: chrono::Utc::now().to_rfc3339(),
        home: home.display().to_string(),
        hosts: hosts.iter().map(|h| h.to_string()).collect(),
        summary,
        stores,
        entries,
        drift_groups,
        cc_switch_db: cc_switch_db
            .exists()
            .then(|| cc_switch_db.display().to_string()),
        cc_switch_skills,
        cc_switch_projection_status: projection_status,
    })
}

fn build_skill_source_report() -> Result<SkillSourceReport, String> {
    let mut summary = SkillSourceReportSummary {
        corpora: SKILL_SOURCE_MANIFESTS.len(),
        ..SkillSourceReportSummary::default()
    };
    let mut corpora = Vec::new();

    for spec in SKILL_SOURCE_MANIFESTS {
        let parsed = parse_skill_source_manifest(spec.content)
            .map_err(|e| format!("parse {}: {e}", spec.path))?;
        let corpus = build_skill_source_corpus_status(spec, parsed);
        accumulate_source_summary(&mut summary, &corpus.summary);
        corpora.push(corpus);
    }

    Ok(SkillSourceReport {
        schema_version: "tachi.skill_surface.sources.v1".to_string(),
        generated_at: chrono::Utc::now().to_rfc3339(),
        summary,
        corpora,
    })
}

fn build_skill_source_corpus_status(
    spec: &SkillSourceManifestSpec,
    parsed: ParsedSkillSourceManifest,
) -> SkillSourceCorpusStatus {
    let mut summary = SkillSourceReportSummary {
        corpora: 1,
        skills: parsed.skills.len(),
        ..SkillSourceReportSummary::default()
    };
    let skills: Vec<SkillSourceSkillStatus> = parsed
        .skills
        .into_iter()
        .map(|entry| {
            let metadata_status = skill_source_metadata_status(&entry.source);
            match entry.source.kind.as_deref() {
                Some("upstream_skill_repo") => summary.upstream_managed += 1,
                Some("tachi_native_contract") => summary.native_contracts += 1,
                _ => {}
            }
            if entry.source.update_policy.as_deref() == Some("local_review") {
                summary.local_review += 1;
            }
            if metadata_status == "missing_metadata" {
                summary.missing_metadata += 1;
            }
            SkillSourceSkillStatus {
                id: entry.id,
                name: entry.name,
                local_path: entry.local_path,
                source: entry.source,
                metadata_status,
                latest_status: SOURCE_STATUS_NOT_CHECKED.to_string(),
                diff_status: SOURCE_STATUS_NOT_CHECKED.to_string(),
            }
        })
        .collect();

    SkillSourceCorpusStatus {
        corpus: if parsed.corpus.is_empty() {
            spec.corpus.to_string()
        } else {
            parsed.corpus
        },
        manifest_path: spec.path.to_string(),
        updated: parsed.updated,
        upstream: SkillSourceUpstreamStatus {
            repo: parsed.upstream.repo,
            pinned_ref: parsed.upstream.pinned_ref,
            pinned_sha: parsed.upstream.pinned_sha,
            update_policy: parsed.upstream.update_policy,
            latest_status: SOURCE_STATUS_NOT_CHECKED.to_string(),
            diff_status: SOURCE_STATUS_NOT_CHECKED.to_string(),
            status_reason: SOURCE_STATUS_NOT_CHECKED_REASON.to_string(),
        },
        skills,
        summary,
    }
}

fn accumulate_source_summary(
    total: &mut SkillSourceReportSummary,
    next: &SkillSourceReportSummary,
) {
    total.skills += next.skills;
    total.upstream_managed += next.upstream_managed;
    total.native_contracts += next.native_contracts;
    total.local_review += next.local_review;
    total.missing_metadata += next.missing_metadata;
}

fn skill_source_metadata_status(source: &SkillSourceMetadata) -> String {
    match source.kind.as_deref() {
        Some("upstream_skill_repo") => {
            if source.repo.is_some()
                && source.path.is_some()
                && source.pinned_ref.is_some()
                && source.pinned_sha.is_some()
                && source.update_policy.is_some()
            {
                "pinned_upstream".to_string()
            } else {
                "missing_metadata".to_string()
            }
        }
        Some("tachi_native_contract") => {
            if source.update_policy.is_some() {
                "native_contract".to_string()
            } else {
                "missing_metadata".to_string()
            }
        }
        Some(_) => {
            if source.update_policy.is_some() {
                "local_or_external".to_string()
            } else {
                "missing_metadata".to_string()
            }
        }
        None => "missing_metadata".to_string(),
    }
}

fn parse_skill_source_manifest(content: &str) -> Result<ParsedSkillSourceManifest, String> {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Section {
        Top,
        Upstream,
        Skills,
        SkillSource,
    }

    let mut section = Section::Top;
    let mut manifest = ParsedSkillSourceManifest::default();
    let mut current: Option<ParsedSkillSourceEntry> = None;

    for raw in content.lines() {
        let line = raw.trim_end();
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        if !line.starts_with(' ') {
            if let Some(entry) = current.take() {
                manifest.skills.push(entry);
            }
            section = match trimmed {
                "upstream:" => Section::Upstream,
                "skills:" => Section::Skills,
                _ => Section::Top,
            };

            if let Some((key, value)) = yaml_key_value(trimmed) {
                match key {
                    "updated" => manifest.updated = value,
                    "corpus" => manifest.corpus = value.unwrap_or_default(),
                    _ => {}
                }
            }
            continue;
        }

        if line.starts_with("  - id:") && matches!(section, Section::Skills | Section::SkillSource)
        {
            if let Some(entry) = current.take() {
                manifest.skills.push(entry);
            }
            let mut entry = ParsedSkillSourceEntry::default();
            entry.id = yaml_scalar(line.trim_start_matches("  - id:").trim()).unwrap_or_default();
            current = Some(entry);
            section = Section::Skills;
            continue;
        }

        match section {
            Section::Upstream => {
                if let Some((key, value)) = yaml_key_value(trimmed) {
                    assign_skill_source_field(&mut manifest.upstream, key, value);
                }
            }
            Section::Skills => {
                if trimmed == "source:" {
                    section = Section::SkillSource;
                    continue;
                }
                if let Some(entry) = current.as_mut() {
                    if let Some((key, value)) = yaml_key_value(trimmed) {
                        match key {
                            "name" => entry.name = value.unwrap_or_default(),
                            "local_path" => entry.local_path = value.unwrap_or_default(),
                            _ => {}
                        }
                    }
                }
            }
            Section::SkillSource => {
                if let Some(entry) = current.as_mut() {
                    if let Some((key, value)) = yaml_key_value(trimmed) {
                        assign_skill_source_field(&mut entry.source, key, value);
                    }
                }
            }
            Section::Top => {}
        }
    }

    if let Some(entry) = current {
        manifest.skills.push(entry);
    }

    if manifest.corpus.is_empty() {
        return Err("missing corpus".to_string());
    }
    if manifest.skills.is_empty() {
        return Err("missing skills".to_string());
    }

    Ok(manifest)
}

fn yaml_key_value(line: &str) -> Option<(&str, Option<String>)> {
    let (key, value) = line.split_once(':')?;
    Some((key.trim(), yaml_scalar(value.trim())))
}

fn yaml_scalar(value: &str) -> Option<String> {
    if value.is_empty() || value == "null" {
        return None;
    }
    Some(value.trim_matches('"').trim_matches('\'').to_string())
}

fn assign_skill_source_field(source: &mut SkillSourceMetadata, key: &str, value: Option<String>) {
    match key {
        "kind" => source.kind = value,
        "repo" => source.repo = value,
        "path" => source.path = value,
        "pinned_ref" => source.pinned_ref = value,
        "pinned_sha" => source.pinned_sha = value,
        "update_policy" => source.update_policy = value,
        "local_overlay" => source.local_overlay = value,
        _ => {}
    }
}

fn skill_store_specs(home: &Path, hosts: &[&str]) -> Vec<SkillStoreSpec> {
    let mut specs = vec![
        SkillStoreSpec {
            id: "cc-switch",
            role: "source",
            path: home.join(".cc-switch").join("skills"),
            format: SkillStoreFormat::SkillMd,
        },
        SkillStoreSpec {
            id: "tachi",
            role: "source",
            path: home.join(".tachi").join("skills"),
            format: SkillStoreFormat::SkillMd,
        },
        SkillStoreSpec {
            id: "agents",
            role: "source",
            path: home.join(".agents").join("skills"),
            format: SkillStoreFormat::SkillMd,
        },
    ];

    for host in hosts {
        match *host {
            "claude" => specs.push(SkillStoreSpec {
                id: "claude",
                role: "host",
                path: home.join(".claude").join("skills"),
                format: SkillStoreFormat::SkillMd,
            }),
            "codex" => specs.push(SkillStoreSpec {
                id: "codex",
                role: "host",
                path: home.join(".codex").join("skills"),
                format: SkillStoreFormat::SkillMd,
            }),
            "gemini" => specs.push(SkillStoreSpec {
                id: "gemini",
                role: "host",
                path: home.join(".gemini").join("skills"),
                format: SkillStoreFormat::SkillMd,
            }),
            "cursor" => specs.push(SkillStoreSpec {
                id: "cursor",
                role: "host",
                path: home.join(".cursor").join("rules"),
                format: SkillStoreFormat::CursorMdc,
            }),
            "antigravity" => specs.push(SkillStoreSpec {
                id: "antigravity",
                role: "host",
                path: home.join(".gemini").join("antigravity").join("skills"),
                format: SkillStoreFormat::None,
            }),
            _ => {}
        }
    }

    specs
}

fn scan_skill_store(spec: &SkillStoreSpec) -> (SkillStoreSummary, Vec<SkillEntryStatus>) {
    let mut summary = SkillStoreSummary {
        id: spec.id.to_string(),
        role: spec.role.to_string(),
        path: spec.path.display().to_string(),
        format: format_name(spec.format).to_string(),
        exists: spec.path.exists(),
        entries: 0,
        skills_with_content: 0,
        symlinks: 0,
        broken_symlinks: 0,
        missing_skill_files: 0,
    };
    let mut entries = Vec::new();

    if !spec.path.exists() || matches!(spec.format, SkillStoreFormat::None) {
        return (summary, entries);
    }

    match spec.format {
        SkillStoreFormat::SkillMd => scan_skill_md_store(spec, &mut summary, &mut entries),
        SkillStoreFormat::CursorMdc => scan_cursor_store(spec, &mut summary, &mut entries),
        SkillStoreFormat::None => {}
    }

    (summary, entries)
}

fn scan_skill_md_store(
    spec: &SkillStoreSpec,
    summary: &mut SkillStoreSummary,
    entries: &mut Vec<SkillEntryStatus>,
) {
    let Ok(read_dir) = std::fs::read_dir(&spec.path) else {
        return;
    };

    for entry in read_dir.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') {
            continue;
        }

        let path = entry.path();
        let Ok(meta) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        if !(meta.is_dir() || meta.file_type().is_symlink()) {
            continue;
        }

        summary.entries += 1;
        let is_symlink = meta.file_type().is_symlink();
        if is_symlink {
            summary.symlinks += 1;
        }

        let symlink_target = if is_symlink {
            std::fs::read_link(&path)
                .ok()
                .map(|target| resolve_link_target(&path, &target).display().to_string())
        } else {
            None
        };
        let target_exists = symlink_target
            .as_ref()
            .map(|target| Path::new(target).exists());
        if target_exists == Some(false) {
            summary.broken_symlinks += 1;
        }

        let skill_file = path.join("SKILL.md");
        let content = std::fs::read_to_string(&skill_file).ok();
        let mut issues = Vec::new();
        if target_exists == Some(false) {
            issues.push("broken_symlink".to_string());
        }
        if content.is_none() {
            summary.missing_skill_files += 1;
            issues.push("missing_SKILL.md".to_string());
        } else {
            summary.skills_with_content += 1;
        }

        entries.push(SkillEntryStatus {
            store: spec.id.to_string(),
            role: spec.role.to_string(),
            name,
            path: path.display().to_string(),
            is_symlink,
            symlink_target,
            target_exists,
            hash: content.map(|content| crate::utils::stable_hash(&content)),
            issues,
        });
    }
}

fn scan_cursor_store(
    spec: &SkillStoreSpec,
    summary: &mut SkillStoreSummary,
    entries: &mut Vec<SkillEntryStatus>,
) {
    let Ok(read_dir) = std::fs::read_dir(&spec.path) else {
        return;
    };

    for entry in read_dir.flatten() {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("mdc") {
            continue;
        }
        let file_name = entry.file_name().to_string_lossy().to_string();
        let name = file_name
            .strip_prefix("tachi-")
            .unwrap_or(&file_name)
            .trim_end_matches(".mdc")
            .to_string();
        let content = std::fs::read_to_string(&path).ok();

        summary.entries += 1;
        if content.is_some() {
            summary.skills_with_content += 1;
        } else {
            summary.missing_skill_files += 1;
        }

        entries.push(SkillEntryStatus {
            store: spec.id.to_string(),
            role: spec.role.to_string(),
            name,
            path: path.display().to_string(),
            is_symlink: false,
            symlink_target: None,
            target_exists: None,
            hash: content.map(|content| crate::utils::stable_hash(&content)),
            issues: Vec::new(),
        });
    }
}

fn resolve_link_target(link_path: &Path, target: &Path) -> PathBuf {
    if target.is_absolute() {
        target.to_path_buf()
    } else {
        link_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(target)
    }
}

fn format_name(format: SkillStoreFormat) -> &'static str {
    match format {
        SkillStoreFormat::SkillMd => "skill_md",
        SkillStoreFormat::CursorMdc => "cursor_mdc",
        SkillStoreFormat::None => "none",
    }
}

fn build_drift_groups(entries: &[SkillEntryStatus]) -> Vec<SkillDriftGroup> {
    let mut by_name: BTreeMap<String, BTreeMap<String, BTreeSet<String>>> = BTreeMap::new();
    for entry in entries {
        let Some(hash) = entry.hash.as_deref() else {
            continue;
        };
        by_name
            .entry(entry.name.clone())
            .or_default()
            .entry(hash.to_string())
            .or_default()
            .insert(entry.store.clone());
    }

    by_name
        .into_iter()
        .filter_map(|(name, hashes)| {
            if hashes.len() <= 1 {
                return None;
            }
            Some(SkillDriftGroup {
                name,
                hashes: hashes
                    .into_iter()
                    .map(|(hash, stores)| SkillHashGroup {
                        hash,
                        stores: stores.into_iter().collect(),
                    })
                    .collect(),
            })
        })
        .collect()
}

fn read_cc_switch_skills(db_path: &Path) -> Result<Vec<CcSwitchSkillRow>, String> {
    if !db_path.exists() {
        return Ok(Vec::new());
    }

    let conn = Connection::open(db_path).map_err(|e| format!("open cc-switch db: {e}"))?;
    let table_exists: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='skills')",
            [],
            |row| row.get::<_, i64>(0),
        )
        .map(|value| value != 0)
        .map_err(|e| format!("inspect cc-switch skills table: {e}"))?;
    if !table_exists {
        return Ok(Vec::new());
    }

    let mut stmt = conn
        .prepare(
            "SELECT name, directory, enabled_claude, enabled_codex, enabled_gemini,
                    enabled_opencode, enabled_hermes, content_hash
             FROM skills
             ORDER BY name",
        )
        .map_err(|e| format!("prepare cc-switch skill query: {e}"))?;

    let rows = stmt
        .query_map([], |row| {
            let mut enabled_hosts = Vec::new();
            for (idx, host) in [
                (2, "claude"),
                (3, "codex"),
                (4, "gemini"),
                (5, "opencode"),
                (6, "hermes"),
            ] {
                let enabled: i64 = row.get(idx)?;
                if enabled != 0 {
                    enabled_hosts.push(host.to_string());
                }
            }
            Ok(CcSwitchSkillRow {
                name: row.get(0)?,
                directory: row.get(1)?,
                enabled_hosts,
                content_hash: row.get(7)?,
            })
        })
        .map_err(|e| format!("query cc-switch skills: {e}"))?;

    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|e| format!("read cc-switch skill row: {e}"))
}

fn build_cc_switch_projection_status(
    home: &Path,
    hosts: &[&str],
    cc_switch_skills: &[CcSwitchSkillRow],
    entries: &[SkillEntryStatus],
) -> Vec<CcSwitchProjectionStatus> {
    let host_set: BTreeSet<&str> = hosts.iter().copied().collect();
    let check_hosts: Vec<&str> = ["claude", "codex", "gemini"]
        .into_iter()
        .filter(|host| host_set.contains(host))
        .collect();
    let cc_switch_root = home.join(".cc-switch").join("skills");

    cc_switch_skills
        .iter()
        .map(|skill| {
            let mut issues = Vec::new();
            let mut checks = Vec::new();
            let source_hash = find_entry(entries, "cc-switch", &skill.name)
                .and_then(|entry| entry.hash.as_deref());
            for host in &check_hosts {
                let expected = skill.enabled_hosts.iter().any(|h| h == host);
                let actual = find_entry(entries, host, &skill.name);
                if expected && actual.is_none() {
                    issues.push(format!("missing_projection:{host}"));
                } else if !expected && actual.is_some() {
                    issues.push(format!("unexpected_projection:{host}"));
                }

                let check = projection_check(
                    home,
                    host,
                    &skill.name,
                    actual,
                    &cc_switch_root,
                    source_hash,
                );
                if expected
                    && check.exists
                    && !check.points_to_cc_switch
                    && !check.content_matches_cc_switch
                {
                    issues.push(format!("projection_content_drift:{host}"));
                }
                checks.push(check);
            }

            let status = if issues.is_empty() {
                if skill
                    .enabled_hosts
                    .iter()
                    .any(|host| check_hosts.contains(&host.as_str()))
                {
                    "synced"
                } else {
                    "disabled"
                }
            } else {
                "drift"
            };

            CcSwitchProjectionStatus {
                name: skill.name.clone(),
                enabled_hosts: skill.enabled_hosts.clone(),
                checks,
                status: status.to_string(),
                issues,
            }
        })
        .collect()
}

fn find_entry<'a>(
    entries: &'a [SkillEntryStatus],
    store: &str,
    name: &str,
) -> Option<&'a SkillEntryStatus> {
    entries
        .iter()
        .find(|entry| entry.store == store && entry.name == name)
}

fn projection_check(
    home: &Path,
    host: &str,
    name: &str,
    entry: Option<&SkillEntryStatus>,
    cc_switch_root: &Path,
    cc_switch_hash: Option<&str>,
) -> HostProjectionCheck {
    let path = host_skill_path(home, host, name);
    let exists = entry.is_some();
    let is_symlink = entry.is_some_and(|entry| entry.is_symlink);
    let symlink_target = entry.and_then(|entry| entry.symlink_target.clone());
    let points_to_cc_switch = symlink_target
        .as_deref()
        .is_some_and(|target| Path::new(target).starts_with(cc_switch_root));
    let content_matches_cc_switch = entry
        .and_then(|entry| entry.hash.as_deref())
        .zip(cc_switch_hash)
        .is_some_and(|(actual, expected)| actual == expected);

    HostProjectionCheck {
        host: host.to_string(),
        path: path.display().to_string(),
        exists,
        is_symlink,
        symlink_target,
        points_to_cc_switch,
        content_matches_cc_switch,
    }
}

fn host_skill_path(home: &Path, host: &str, name: &str) -> PathBuf {
    match host {
        "claude" => home.join(".claude").join("skills").join(name),
        "codex" => home.join(".codex").join("skills").join(name),
        "gemini" => home.join(".gemini").join("skills").join(name),
        _ => home.join(format!(".{host}")).join("skills").join(name),
    }
}

fn print_skill_surface_report(report: &SkillSurfaceReport) {
    println!("Tachi skill surface status");
    println!("  home: {}", report.home);
    println!("  hosts: {}", report.hosts.join(", "));
    println!(
        "  stores: {}  entries: {}  symlinks: {}  broken_symlinks: {}",
        report.summary.stores,
        report.summary.entries,
        report.summary.symlinks,
        report.summary.broken_symlinks
    );
    println!(
        "  cc-switch skills: {}  projection issues: {}  drift groups: {}",
        report.summary.cc_switch_skills,
        report.summary.cc_switch_projection_issues,
        report.summary.drift_groups
    );
    println!();

    println!("Stores:");
    for store in &report.stores {
        println!(
            "  {:<10} {:<6} {:<10} exists={} entries={} content={} symlinks={} broken={} missing_files={}  {}",
            store.id,
            store.role,
            store.format,
            store.exists,
            store.entries,
            store.skills_with_content,
            store.symlinks,
            store.broken_symlinks,
            store.missing_skill_files,
            store.path
        );
    }

    let problem_entries: Vec<&SkillEntryStatus> = report
        .entries
        .iter()
        .filter(|entry| !entry.issues.is_empty())
        .collect();
    if !problem_entries.is_empty() {
        println!();
        println!("Entry issues:");
        for entry in problem_entries {
            println!(
                "  {:<10} {:<24} {} ({})",
                entry.store,
                entry.name,
                entry.issues.join(", "),
                entry.path
            );
        }
    }

    if !report.drift_groups.is_empty() {
        println!();
        println!("Same-name hash drift:");
        for group in &report.drift_groups {
            println!("  {}", group.name);
            for hash in &group.hashes {
                println!("    {} -> {}", hash.hash, hash.stores.join(", "));
            }
        }
    }

    let projection_issues: Vec<&CcSwitchProjectionStatus> = report
        .cc_switch_projection_status
        .iter()
        .filter(|status| !status.issues.is_empty())
        .collect();
    if !projection_issues.is_empty() {
        println!();
        println!("CC Switch projection issues:");
        for status in projection_issues {
            println!("  {:<24} {}", status.name, status.issues.join(", "));
        }
    }
}

fn print_skill_source_report(report: &SkillSourceReport) {
    println!("Tachi skill source status");
    println!(
        "  corpora: {}  skills: {}  upstream-managed: {}  native-contracts: {}",
        report.summary.corpora,
        report.summary.skills,
        report.summary.upstream_managed,
        report.summary.native_contracts
    );
    println!(
        "  local-review: {}  missing-metadata: {}",
        report.summary.local_review, report.summary.missing_metadata
    );
    println!("  update check: {SOURCE_STATUS_NOT_CHECKED}");
    println!();

    println!("Corpora:");
    for corpus in &report.corpora {
        println!(
            "  {:<12} {:<24} ref={} sha={} policy={} skills={}",
            corpus.corpus,
            corpus.upstream.repo.as_deref().unwrap_or("local"),
            corpus.upstream.pinned_ref.as_deref().unwrap_or("local"),
            short_sha(corpus.upstream.pinned_sha.as_deref()),
            corpus
                .upstream
                .update_policy
                .as_deref()
                .unwrap_or("unknown"),
            corpus.summary.skills
        );
    }

    println!();
    println!("Skills:");
    for corpus in &report.corpora {
        for skill in &corpus.skills {
            let source = &skill.source;
            println!(
                "  {:<12} {:<34} {:<22} repo={} ref={} sha={} overlay={}",
                corpus.corpus,
                skill.name,
                source.kind.as_deref().unwrap_or("unknown"),
                source.repo.as_deref().unwrap_or("local"),
                source.pinned_ref.as_deref().unwrap_or("local"),
                short_sha(source.pinned_sha.as_deref()),
                source.local_overlay.as_deref().unwrap_or("none")
            );
        }
    }
}

fn short_sha(value: Option<&str>) -> String {
    value
        .map(|sha| sha.chars().take(7).collect())
        .unwrap_or_else(|| "local".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_home(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("tachi-{name}-{}-{nanos}", std::process::id()))
    }

    fn write_skill(root: &Path, name: &str, content: &str) {
        let dir = root.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("SKILL.md"), content).unwrap();
    }

    #[test]
    fn skill_surface_reports_same_name_hash_drift() {
        let home = temp_home("skill-surface-drift");
        let cc = home.join(".cc-switch").join("skills");
        let codex = home.join(".codex").join("skills");
        std::fs::create_dir_all(&cc).unwrap();
        std::fs::create_dir_all(&codex).unwrap();
        write_skill(&cc, "check", "one");
        write_skill(&codex, "check", "two");

        let report = build_skill_surface_report(&home, &["codex".to_string()]).unwrap();

        assert!(report
            .drift_groups
            .iter()
            .any(|group| group.name == "check"));

        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn skill_surface_reads_cc_switch_projection_matrix() {
        let home = temp_home("skill-surface-ccswitch");
        let cc_dir = home.join(".cc-switch").join("skills");
        let claude_dir = home.join(".claude").join("skills");
        std::fs::create_dir_all(&cc_dir).unwrap();
        std::fs::create_dir_all(&claude_dir).unwrap();
        write_skill(&cc_dir, "think", "think skill");

        let db_path = home.join(".cc-switch").join("cc-switch.db");
        let conn = Connection::open(&db_path).unwrap();
        conn.execute_batch(
            "CREATE TABLE skills (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                description TEXT,
                directory TEXT NOT NULL,
                enabled_claude BOOLEAN NOT NULL DEFAULT 0,
                enabled_codex BOOLEAN NOT NULL DEFAULT 0,
                enabled_gemini BOOLEAN NOT NULL DEFAULT 0,
                enabled_opencode BOOLEAN NOT NULL DEFAULT 0,
                enabled_hermes BOOLEAN NOT NULL DEFAULT 0,
                content_hash TEXT
            );
            INSERT INTO skills
                (id, name, description, directory, enabled_claude, enabled_codex, enabled_gemini, enabled_opencode, enabled_hermes, content_hash)
            VALUES
                ('local:think', 'think', 'plan', 'think', 1, 0, 0, 0, 0, 'sha');",
        )
        .unwrap();

        let report = build_skill_surface_report(&home, &["claude".to_string()]).unwrap();

        let think = report
            .cc_switch_projection_status
            .iter()
            .find(|status| status.name == "think")
            .unwrap();
        assert_eq!(think.status, "drift");
        assert!(think
            .issues
            .contains(&"missing_projection:claude".to_string()));

        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn skill_source_report_reads_builtin_manifest_status() {
        let report = build_skill_source_report().unwrap();

        assert_eq!(report.schema_version, "tachi.skill_surface.sources.v1");
        assert_eq!(report.summary.corpora, 2);
        assert_eq!(report.summary.skills, 16);
        assert_eq!(report.summary.upstream_managed, 13);
        assert_eq!(report.summary.native_contracts, 3);
        assert_eq!(report.summary.missing_metadata, 0);

        let superpowers = report
            .corpora
            .iter()
            .find(|corpus| corpus.corpus == "superpowers")
            .unwrap();
        assert_eq!(
            superpowers.upstream.repo.as_deref(),
            Some("obra/superpowers")
        );
        assert_eq!(
            superpowers.upstream.pinned_sha.as_deref(),
            Some("6fd4507659784c351abbd2bc264c7162cfd386dc")
        );
        assert!(superpowers
            .skills
            .iter()
            .any(|skill| skill.name == "verification-before-completion"
                && skill.metadata_status == "native_contract"));

        let waza = report
            .corpora
            .iter()
            .find(|corpus| corpus.corpus == "waza")
            .unwrap();
        assert!(waza.skills.iter().any(|skill| skill.name == "check"
            && skill.metadata_status == "pinned_upstream"
            && skill.source.local_overlay.as_deref() == Some("tachi-routing-only")));
        assert!(waza.skills.iter().any(|skill| skill.name == "tachi"
            && skill.metadata_status == "native_contract"
            && skill.source.update_policy.as_deref() == Some("local_review")));
    }

    #[test]
    fn skill_source_manifest_parser_flags_missing_metadata() {
        let parsed = parse_skill_source_manifest(
            r#"
schema_version: 1
corpus: sample
upstream:
  repo: example/source
  pinned_ref: main
  pinned_sha: abc
  update_policy: reviewed_sync
skills:
  - id: skill:sample
    name: sample
    local_path: skill/sample/SKILL.md
    source:
      kind: upstream_skill_repo
      repo: example/source
      path: skills/sample/SKILL.md
      pinned_ref: main
"#,
        )
        .unwrap();

        assert_eq!(parsed.skills.len(), 1);
        assert_eq!(
            skill_source_metadata_status(&parsed.skills[0].source),
            "missing_metadata"
        );
    }
}
