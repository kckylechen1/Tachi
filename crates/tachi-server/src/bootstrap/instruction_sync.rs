//! Layered global instruction & constitution sync (`tachi.instruction_bundle.v1`).
//!
//! Renders shared constitution + carrier-scoped adapters + optional host-local
//! `machine.local.md` overlay into host global instruction targets with strict
//! symlink containment, portability/secret screening, byte-0 frontmatter
//! preservation, and fail-closed no-clobber preflight.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::fs;
use std::io::Read;
use std::path::{Component, Path, PathBuf};

const BUNDLE_SCHEMA: &str = "tachi.instruction_bundle.v1";
const PLAN_SCHEMA: &str = "tachi.harness.plan.v1";
const SYNC_SCHEMA: &str = "tachi.harness.sync.v1";
const RECEIPT_SCHEMA: &str = "tachi.harness_sync_receipt.v1";
const STAMP_PREFIX: &str = "<!-- tachi:instruction-sync v1 ";
const STAMP_SUFFIX: &str = " -->";
const MACHINE_LOCAL_START: &str = "<!-- tachi:machine-local:start -->";
const MACHINE_LOCAL_END: &str = "<!-- tachi:machine-local:end -->";
const MAX_INPUT_FILE_BYTES: u64 = 256 * 1024;
const MAX_TARGET_BUDGET_BYTES: u64 = 65_536;

pub(super) const SUPPORTED_SYNC_HOSTS: &[&str] = &[
    "constitution",
    "codex",
    "claude",
    "gemini",
    "antigravity",
    "cursor",
];

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct BundleManifest {
    schema_version: String,
    revision: String,
    #[serde(default)]
    constitution: Option<ConstitutionDecl>,
    #[serde(default)]
    targets: Vec<TargetDecl>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct ConstitutionDecl {
    source: String,
    #[serde(default = "default_constitution_target")]
    target: String,
    max_bytes: u64,
}

fn default_constitution_target() -> String {
    ".agents/constitution/core.md".to_string()
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct TargetDecl {
    id: String,
    host: String,
    source: String,
    target: String,
    #[serde(default)]
    include_constitution_import: bool,
    #[serde(default)]
    include_machine_local: bool,
    max_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum SyncStampState {
    Absent,
    ValidClean {
        id: String,
        revision: String,
        source_sha256: String,
        body_sha256: String,
    },
    TamperedOrEdited,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum TargetCondition {
    InSync,
    Create,
    UpdateManaged,
    ConflictUnmanaged,
    ConflictSymlink {
        link_target: String,
        link_target_b64: String,
    },
    ConflictModifiedManaged,
}

impl TargetCondition {
    fn as_str(&self) -> &'static str {
        match self {
            Self::InSync => "in_sync",
            Self::Create => "create",
            Self::UpdateManaged => "update_managed",
            Self::ConflictUnmanaged => "conflict_unmanaged",
            Self::ConflictSymlink { .. } => "conflict_symlink",
            Self::ConflictModifiedManaged => "conflict_modified_managed",
        }
    }

    fn is_conflict(&self) -> bool {
        matches!(
            self,
            Self::ConflictUnmanaged | Self::ConflictSymlink { .. } | Self::ConflictModifiedManaged
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileFingerprint {
    dev: u64,
    ino: u64,
    mtime: i64,
    mtime_nsec: i64,
    size: u64,
}

#[derive(Debug, Clone)]
struct PlannedTarget {
    id: String,
    host: String,
    source_rel: String,
    target_rel: String,
    target_path: PathBuf,
    condition: TargetCondition,
    preflight_fingerprint: Option<FileFingerprint>,
    rendered: String,
    rendered_sha256: String,
    source_sha256: String,
    rendered_bytes: u64,
    max_bytes: u64,
}

#[derive(Debug, Clone, Serialize)]
pub(super) struct SyncTargetReport {
    pub(super) id: String,
    pub(super) host: String,
    pub(super) source: String,
    pub(super) target: String,
    pub(super) path: String,
    pub(super) condition: String,
    pub(super) action: String,
    pub(super) rendered_bytes: u64,
    pub(super) max_bytes: u64,
    pub(super) source_sha256: String,
    pub(super) rendered_sha256: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) previous_symlink_target: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) backup_path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Default)]
pub(super) struct SyncSummary {
    pub(super) targets: usize,
    pub(super) in_sync: usize,
    pub(super) create: usize,
    pub(super) update_managed: usize,
    pub(super) conflicts: usize,
    pub(super) written: usize,
    pub(super) adopted: usize,
}

#[derive(Debug, Clone, Serialize)]
pub(super) struct HarnessSyncReport {
    pub(super) schema_version: String,
    pub(super) generated_at: String,
    pub(super) revision: String,
    pub(super) home: String,
    pub(super) source_dir: String,
    pub(super) bundle_sha256: String,
    pub(super) machine_local_path: Option<String>,
    pub(super) machine_local_sha256: Option<String>,
    pub(super) status: String,
    pub(super) summary: SyncSummary,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) backup_dir: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) receipt_path: Option<String>,
    pub(super) targets: Vec<SyncTargetReport>,
}

pub(super) struct SyncOptions {
    pub(super) source_dir: Option<PathBuf>,
    pub(super) machine_local: Option<PathBuf>,
    pub(super) hosts: Vec<String>,
    pub(super) home: Option<PathBuf>,
    pub(super) adopt: bool,
}

pub(super) fn inspect_sync_stamp(content: &str) -> SyncStampState {
    let trimmed_end = content.trim_end_matches('\n');
    let last_line_start = trimmed_end.rfind('\n').map_or(0, |idx| idx + 1);
    let last_line = &trimmed_end[last_line_start..];

    if !last_line.starts_with(STAMP_PREFIX) {
        if content.contains("<!-- tachi:instruction-sync") {
            return SyncStampState::TamperedOrEdited;
        }
        return SyncStampState::Absent;
    }

    if !last_line.ends_with(STAMP_SUFFIX)
        || last_line.len() < STAMP_PREFIX.len() + STAMP_SUFFIX.len()
    {
        return SyncStampState::TamperedOrEdited;
    }

    let body = &content[..last_line_start];
    if body.contains("<!-- tachi:instruction-sync") {
        return SyncStampState::TamperedOrEdited;
    }

    let inner = &last_line[STAMP_PREFIX.len()..last_line.len() - STAMP_SUFFIX.len()];
    let mut id = None;
    let mut revision = None;
    let mut source_sha256 = None;
    let mut body_sha256 = None;

    for token in inner.split_whitespace() {
        let Some((key, value)) = token.split_once('=') else {
            return SyncStampState::TamperedOrEdited;
        };
        match key {
            "id" if id.is_none() => id = Some(value.to_string()),
            "revision" if revision.is_none() => revision = Some(value.to_string()),
            "source_sha256" if source_sha256.is_none() => source_sha256 = Some(value.to_string()),
            "body_sha256" if body_sha256.is_none() => body_sha256 = Some(value.to_string()),
            _ => return SyncStampState::TamperedOrEdited,
        }
    }

    let (Some(id), Some(revision), Some(source_sha256), Some(body_sha256)) =
        (id, revision, source_sha256, body_sha256)
    else {
        return SyncStampState::TamperedOrEdited;
    };

    if !is_sha256_hex(&source_sha256) || !is_sha256_hex(&body_sha256) {
        return SyncStampState::TamperedOrEdited;
    }

    let actual_body_sha = sha256_hex(body.as_bytes());
    if actual_body_sha != body_sha256 {
        return SyncStampState::TamperedOrEdited;
    }

    SyncStampState::ValidClean {
        id,
        revision,
        source_sha256,
        body_sha256,
    }
}

pub(super) fn run_plan(options: &SyncOptions) -> Result<HarnessSyncReport, String> {
    let ctx = prepare_plan(options)?;
    let status = if ctx.summary.conflicts > 0 {
        "blocked_by_conflicts"
    } else if ctx.summary.create > 0 || ctx.summary.update_managed > 0 {
        "ready"
    } else {
        "in_sync"
    };

    Ok(HarnessSyncReport {
        schema_version: PLAN_SCHEMA.to_string(),
        generated_at: memcore::time_compat::rfc3339_compat(jiff::Timestamp::now()),
        revision: ctx.revision,
        home: ctx.home.display().to_string(),
        source_dir: ctx.source_dir.display().to_string(),
        bundle_sha256: ctx.bundle_sha256,
        machine_local_path: ctx
            .machine_local_path
            .as_ref()
            .map(|p| p.display().to_string()),
        machine_local_sha256: ctx.machine_local_sha256,
        status: status.to_string(),
        summary: ctx.summary,
        backup_dir: None,
        receipt_path: None,
        targets: ctx
            .targets
            .into_iter()
            .map(|target| {
                let previous_symlink_target = match &target.condition {
                    TargetCondition::ConflictSymlink { link_target, .. } => {
                        Some(link_target.clone())
                    }
                    _ => None,
                };
                SyncTargetReport {
                    id: target.id,
                    host: target.host,
                    source: target.source_rel,
                    target: target.target_rel,
                    path: target.target_path.display().to_string(),
                    condition: target.condition.as_str().to_string(),
                    action: "planned".to_string(),
                    rendered_bytes: target.rendered_bytes,
                    max_bytes: target.max_bytes,
                    source_sha256: target.source_sha256,
                    rendered_sha256: target.rendered_sha256,
                    previous_symlink_target,
                    backup_path: None,
                }
            })
            .collect(),
    })
}

pub(super) fn run_sync(options: &SyncOptions) -> Result<HarnessSyncReport, String> {
    let ctx = prepare_plan(options)?;

    if ctx.summary.conflicts > 0 && !options.adopt {
        let conflicts = ctx
            .targets
            .iter()
            .filter(|t| t.condition.is_conflict())
            .map(|t| format!("{} ({})", t.target_rel, t.condition.as_str()))
            .collect::<Vec<_>>()
            .join(", ");
        return Err(format!(
            "preflight blocked by {} conflict(s): {conflicts}; no targets modified (pass --adopt to back up and replace)",
            ctx.summary.conflicts
        ));
    }

    #[cfg(unix)]
    let home_fd = open_dir_nofollow(&ctx.home)?;

    let needs_backup = options.adopt && ctx.summary.conflicts > 0;
    let backup_rel_dir = if needs_backup {
        let uuid_str = uuid::Uuid::new_v4().as_simple().to_string();
        let run_id = format!("{}-{}", jiff::Timestamp::now().as_second(), &uuid_str[..12]);
        let base_rel = Path::new(".tachi/backups/harness-sync");
        let run_rel = base_rel.join(&run_id);
        #[cfg(unix)]
        create_unique_dir_under_fd(&home_fd, base_rel, &run_id, 0o700)?;
        #[cfg(not(unix))]
        {
            ensure_safe_dir_under_home(&ctx.home, base_rel, true)?;
            fs::create_dir(ctx.home.join(&run_rel)).map_err(|err| {
                format!(
                    "create unique backup directory '{}': {err}",
                    run_rel.display()
                )
            })?;
        }
        Some(run_rel)
    } else {
        None
    };
    let backup_dir = backup_rel_dir.as_ref().map(|rel| ctx.home.join(rel));

    let mut summary = ctx.summary.clone();
    let mut reports = Vec::with_capacity(ctx.targets.len());

    for target in &ctx.targets {
        let previous_symlink_target = match &target.condition {
            TargetCondition::ConflictSymlink { link_target, .. } => Some(link_target.clone()),
            _ => None,
        };

        if target.condition == TargetCondition::InSync {
            reports.push(SyncTargetReport {
                id: target.id.clone(),
                host: target.host.clone(),
                source: target.source_rel.clone(),
                target: target.target_rel.clone(),
                path: target.target_path.display().to_string(),
                condition: target.condition.as_str().to_string(),
                action: "unchanged".to_string(),
                rendered_bytes: target.rendered_bytes,
                max_bytes: target.max_bytes,
                source_sha256: target.source_sha256.clone(),
                rendered_sha256: target.rendered_sha256.clone(),
                previous_symlink_target,
                backup_path: None,
            });
            continue;
        }

        // Recheck condition immediately before mutation to guard against TOCTOU swaps.
        let (current_condition, current_fp) =
            inspect_target_condition(&ctx.home, &target.target_rel, &target.rendered)?;
        if current_condition != target.condition || current_fp != target.preflight_fingerprint {
            return Err(format!(
                "target {} changed after preflight ({} -> {}); aborting sync",
                target.target_path.display(),
                target.condition.as_str(),
                current_condition.as_str()
            ));
        }

        let mut recorded_backup: Option<String> = None;
        if target.condition.is_conflict() {
            let backup_rel_root = backup_rel_dir
                .as_ref()
                .ok_or_else(|| "internal error: missing backup directory".to_string())?;
            let backup_rel_file = match &target.condition {
                TargetCondition::ConflictSymlink {
                    link_target,
                    link_target_b64,
                } => {
                    let rel = backup_rel_root.join(format!("symlinks/{}.json", target.target_rel));
                    let payload = serde_json::json!({
                        "target": target.target_rel,
                        "symlink_target": link_target,
                        "symlink_target_bytes_base64": link_target_b64,
                    });
                    let bytes = serde_json::to_vec_pretty(&payload)
                        .map_err(|e| format!("serialize symlink backup: {e}"))?;
                    write_rel_file_anchored(
                        &ctx.home,
                        #[cfg(unix)]
                        &home_fd,
                        &rel,
                        &bytes,
                        true,
                        true,
                        None,
                    )?;
                    rel
                }
                TargetCondition::ConflictUnmanaged | TargetCondition::ConflictModifiedManaged => {
                    let rel = backup_rel_root.join("files").join(&target.target_rel);
                    #[cfg(unix)]
                    let existing_bytes = read_file_under_dir_fd(&home_fd, &target.target_rel)?;
                    #[cfg(not(unix))]
                    let existing_bytes = read_regular_file_bytes_nofollow(&target.target_path)?;
                    write_rel_file_anchored(
                        &ctx.home,
                        #[cfg(unix)]
                        &home_fd,
                        &rel,
                        &existing_bytes,
                        true,
                        true,
                        None,
                    )?;
                    rel
                }
                _ => unreachable!(),
            };
            recorded_backup = Some(ctx.home.join(backup_rel_file).display().to_string());
            summary.adopted += 1;
        }

        let is_create = target.condition == TargetCondition::Create;
        write_rel_file_anchored(
            &ctx.home,
            #[cfg(unix)]
            &home_fd,
            Path::new(&target.target_rel),
            target.rendered.as_bytes(),
            false,
            is_create,
            target.preflight_fingerprint,
        )
        .map_err(|e| format!("write target {}: {e}", target.target_path.display()))?;
        summary.written += 1;

        let action = if target.condition.is_conflict() {
            "adopted"
        } else if target.condition == TargetCondition::Create {
            "created"
        } else {
            "updated"
        };

        reports.push(SyncTargetReport {
            id: target.id.clone(),
            host: target.host.clone(),
            source: target.source_rel.clone(),
            target: target.target_rel.clone(),
            path: target.target_path.display().to_string(),
            condition: target.condition.as_str().to_string(),
            action: action.to_string(),
            rendered_bytes: target.rendered_bytes,
            max_bytes: target.max_bytes,
            source_sha256: target.source_sha256.clone(),
            rendered_sha256: target.rendered_sha256.clone(),
            previous_symlink_target,
            backup_path: recorded_backup,
        });
    }

    let receipt_rel = Path::new(".tachi/harness-sync-receipt.json");
    let receipt_path = ctx.home.join(receipt_rel);
    let synced_at = memcore::time_compat::rfc3339_compat(jiff::Timestamp::now());
    let receipt = serde_json::json!({
        "schema_version": RECEIPT_SCHEMA,
        "synced_at": synced_at,
        "revision": ctx.revision,
        "home": ctx.home.display().to_string(),
        "source_dir": ctx.source_dir.display().to_string(),
        "bundle_sha256": ctx.bundle_sha256,
        "machine_local_path": ctx.machine_local_path.as_ref().map(|p| p.display().to_string()),
        "machine_local_sha256": ctx.machine_local_sha256,
        "backup_dir": backup_dir.as_ref().map(|p| p.display().to_string()),
        "targets": reports,
    });
    let receipt_bytes = serde_json::to_vec_pretty(&receipt)
        .map_err(|e| format!("serialize harness sync receipt: {e}"))?;
    write_rel_file_anchored(
        &ctx.home,
        #[cfg(unix)]
        &home_fd,
        receipt_rel,
        &receipt_bytes,
        true,
        false,
        None,
    )?;

    Ok(HarnessSyncReport {
        schema_version: SYNC_SCHEMA.to_string(),
        generated_at: synced_at,
        revision: ctx.revision,
        home: ctx.home.display().to_string(),
        source_dir: ctx.source_dir.display().to_string(),
        bundle_sha256: ctx.bundle_sha256,
        machine_local_path: ctx
            .machine_local_path
            .as_ref()
            .map(|p| p.display().to_string()),
        machine_local_sha256: ctx.machine_local_sha256,
        status: "synced".to_string(),
        summary,
        backup_dir: backup_dir.map(|p| p.display().to_string()),
        receipt_path: Some(receipt_path.display().to_string()),
        targets: reports,
    })
}

pub(super) fn print_sync_report(report: &HarnessSyncReport) {
    let verb = if report.schema_version == PLAN_SCHEMA {
        "plan"
    } else {
        "sync"
    };
    println!("Tachi harness {verb} ({})", report.status);
    println!("  revision: {}", report.revision);
    println!("  home: {}", report.home);
    println!("  source_dir: {}", report.source_dir);
    if let Some(local) = &report.machine_local_path {
        println!("  machine_local: {local}");
    } else {
        println!("  machine_local: (none)");
    }
    println!(
        "  targets: {} (in_sync {}, create {}, update {}, conflicts {}, written {}, adopted {})",
        report.summary.targets,
        report.summary.in_sync,
        report.summary.create,
        report.summary.update_managed,
        report.summary.conflicts,
        report.summary.written,
        report.summary.adopted
    );
    if let Some(backup) = &report.backup_dir {
        println!("  backup_dir: {backup}");
    }
    if let Some(receipt) = &report.receipt_path {
        println!("  receipt: {receipt}");
    }
    println!();
    for target in &report.targets {
        println!(
            "{:<13} {:<24} {:<10} {:<25} bytes={}/{} {}",
            target.host,
            target.id,
            target.action,
            target.condition,
            target.rendered_bytes,
            target.max_bytes,
            target.path
        );
        if let Some(link) = &target.previous_symlink_target {
            println!("  previous_symlink: {link}");
        }
        if let Some(backup) = &target.backup_path {
            println!("  backup: {backup}");
        }
    }
}

struct PreparedPlan {
    home: PathBuf,
    source_dir: PathBuf,
    revision: String,
    bundle_sha256: String,
    machine_local_path: Option<PathBuf>,
    machine_local_sha256: Option<String>,
    summary: SyncSummary,
    targets: Vec<PlannedTarget>,
}

fn prepare_plan(options: &SyncOptions) -> Result<PreparedPlan, String> {
    let home = resolve_and_validate_home(options.home.clone())?;
    let home_str = home.display().to_string();
    if home_str.contains('\n') || home_str.contains('\r') || home_str.contains("-->") {
        return Err("home path contains unsupported control characters".to_string());
    }

    let raw_source_dir = options
        .source_dir
        .clone()
        .unwrap_or_else(|| home.join(".tachi").join("instructions"));
    reject_user_symlink_ancestors(&raw_source_dir, "source_dir")?;
    let source_meta = fs::symlink_metadata(&raw_source_dir)
        .map_err(|e| format!("inspect source_dir '{}': {e}", raw_source_dir.display()))?;
    if source_meta.file_type().is_symlink() {
        return Err(format!(
            "symlink source_dir is not allowed: {}",
            raw_source_dir.display()
        ));
    }
    if !source_meta.file_type().is_dir() {
        return Err(format!(
            "source_dir is not a directory: {}",
            raw_source_dir.display()
        ));
    }
    let source_dir = raw_source_dir.canonicalize().map_err(|e| {
        format!(
            "canonicalize source_dir '{}': {e}",
            raw_source_dir.display()
        )
    })?;
    if source_dir == home {
        return Err("source_dir must not equal home directory".to_string());
    }
    #[cfg(unix)]
    let source_dir_fd = open_dir_nofollow(&source_dir)?;

    #[cfg(unix)]
    let manifest_bytes = read_file_under_dir_fd(&source_dir_fd, "bundle.json")?;
    #[cfg(not(unix))]
    let manifest_bytes = read_relative_regular_file(&source_dir, "bundle.json")?;
    let bundle_sha256 = sha256_hex(&manifest_bytes);
    let manifest: BundleManifest =
        serde_json::from_slice(&manifest_bytes).map_err(|e| format!("parse bundle.json: {e}"))?;

    if manifest.schema_version != BUNDLE_SCHEMA {
        return Err(format!(
            "unsupported bundle schema_version; expected '{BUNDLE_SCHEMA}'"
        ));
    }
    validate_identifier(&manifest.revision, 128, "revision")?;
    screen_secrets("revision", &manifest.revision)?;

    let LoadedMachineLocal {
        path: machine_local_path,
        content: machine_local_content,
        sha256: machine_local_sha256,
    } = load_machine_local(&home, options.machine_local.as_deref())?;

    let selected_hosts = crate::utils::normalize_supported_values(
        &options.hosts,
        SUPPORTED_SYNC_HOSTS,
        "harness sync host",
    )?;
    let selected_host_set: BTreeSet<&str> = selected_hosts.iter().copied().collect();

    let mut all_decls: Vec<TargetDecl> = Vec::new();
    let constitution_target_rel = if let Some(ref constitution) = manifest.constitution {
        validate_relative_path(&constitution.source, "constitution.source")?;
        validate_relative_path(&constitution.target, "constitution.target")?;
        validate_host_target_prefix("constitution", &constitution.target)?;
        validate_budget(constitution.max_bytes, "constitution")?;
        all_decls.push(TargetDecl {
            id: "constitution-core".to_string(),
            host: "constitution".to_string(),
            source: constitution.source.clone(),
            target: constitution.target.clone(),
            include_constitution_import: false,
            include_machine_local: false,
            max_bytes: constitution.max_bytes,
        });
        Some(constitution.target.clone())
    } else {
        None
    };

    for decl in &manifest.targets {
        validate_identifier(&decl.id, 64, "target.id")?;
        if !SUPPORTED_SYNC_HOSTS.contains(&decl.host.as_str()) || decl.host == "constitution" {
            return Err(format!(
                "target '{}' has unsupported host '{}'",
                decl.id, decl.host
            ));
        }
        validate_relative_path(&decl.source, &format!("target '{}'.source", decl.id))?;
        validate_relative_path(&decl.target, &format!("target '{}'.target", decl.id))?;
        validate_host_target_prefix(&decl.host, &decl.target)?;
        validate_budget(decl.max_bytes, &decl.id)?;
        if decl.include_constitution_import && constitution_target_rel.is_none() {
            return Err(format!(
                "target '{}' sets include_constitution_import=true without a bundle constitution",
                decl.id
            ));
        }
        all_decls.push(decl.clone());
    }

    if all_decls.is_empty() {
        return Err(
            "bundle.json must declare at least one constitution or target entry".to_string(),
        );
    }

    validate_unique_ids_and_targets(&all_decls, &home, &source_dir)?;

    let mut planned = Vec::new();
    let mut summary = SyncSummary::default();

    for decl in all_decls {
        if !selected_host_set.contains(decl.host.as_str()) {
            continue;
        }
        #[cfg(unix)]
        let raw_bytes = read_file_under_dir_fd(&source_dir_fd, &decl.source)?;
        #[cfg(not(unix))]
        let raw_bytes = read_relative_regular_file(&source_dir, &decl.source)?;
        let raw_text = String::from_utf8(raw_bytes)
            .map_err(|_| format!("source '{}' is not valid UTF-8", decl.source))?;
        let normalized_source = normalize_lf(&raw_text);

        validate_shared_source_content(&decl.source, &normalized_source)?;
        if decl.include_constitution_import && starts_with_yaml_frontmatter(&normalized_source) {
            return Err(format!(
                "target '{}' starts with YAML frontmatter and cannot set include_constitution_import=true",
                decl.id
            ));
        }

        let mut body = normalized_source.replace("{{HOME}}", &home_str);
        if decl.include_constitution_import {
            let const_rel = constitution_target_rel
                .as_deref()
                .ok_or_else(|| "missing constitution target".to_string())?;
            if !selected_host_set.contains("constitution") {
                check_existing_target_parents_no_symlink(&home, Path::new(const_rel))?;
                let const_path = home.join(const_rel);
                let const_meta = fs::symlink_metadata(&const_path).map_err(|_| {
                    format!(
                        "target '{}' requires constitution '{}' to exist when 'constitution' is excluded from --host",
                        decl.id, const_rel
                    )
                })?;
                if const_meta.file_type().is_symlink() || !const_meta.file_type().is_file() {
                    return Err(format!(
                        "target '{}' requires constitution '{}' to be a regular non-symlink file when 'constitution' is excluded from --host",
                        decl.id, const_rel
                    ));
                }
            }
            body = format!("@{home_str}/{const_rel}\n\n{body}");
        }
        if decl.include_machine_local {
            if let Some(local_text) = machine_local_content.as_deref() {
                let trimmed = local_text.trim();
                if !trimmed.is_empty() {
                    if !body.ends_with('\n') {
                        body.push('\n');
                    }
                    body.push('\n');
                    body.push_str(MACHINE_LOCAL_START);
                    body.push('\n');
                    body.push_str(trimmed);
                    body.push('\n');
                    body.push_str(MACHINE_LOCAL_END);
                    body.push('\n');
                }
            }
        }
        if !body.ends_with('\n') {
            body.push('\n');
        }

        let source_sha256 = sha256_hex(normalized_source.as_bytes());
        let body_sha256 = sha256_hex(body.as_bytes());
        let stamp = format!(
            "{STAMP_PREFIX}id={} revision={} source_sha256={} body_sha256={}{STAMP_SUFFIX}\n",
            decl.id, manifest.revision, source_sha256, body_sha256
        );
        let rendered = format!("{body}{stamp}");
        let rendered_bytes = rendered.len() as u64;
        if rendered_bytes > decl.max_bytes {
            return Err(format!(
                "density_budget_exceeded: target '{}' rendered {} bytes (max_bytes {})",
                decl.id, rendered_bytes, decl.max_bytes
            ));
        }
        let rendered_sha256 = sha256_hex(rendered.as_bytes());

        check_existing_target_parents_no_symlink(&home, Path::new(&decl.target))?;
        let target_path = home.join(&decl.target);
        let (condition, preflight_fingerprint) =
            inspect_target_condition(&home, &decl.target, &rendered)?;

        summary.targets += 1;
        match condition {
            TargetCondition::InSync => summary.in_sync += 1,
            TargetCondition::Create => summary.create += 1,
            TargetCondition::UpdateManaged => summary.update_managed += 1,
            TargetCondition::ConflictUnmanaged
            | TargetCondition::ConflictSymlink { .. }
            | TargetCondition::ConflictModifiedManaged => summary.conflicts += 1,
        }

        planned.push(PlannedTarget {
            id: decl.id,
            host: decl.host,
            source_rel: decl.source,
            target_rel: decl.target,
            target_path,
            condition,
            preflight_fingerprint,
            rendered,
            rendered_sha256,
            source_sha256,
            rendered_bytes,
            max_bytes: decl.max_bytes,
        });
    }

    Ok(PreparedPlan {
        home,
        source_dir,
        revision: manifest.revision,
        bundle_sha256,
        machine_local_path,
        machine_local_sha256,
        summary,
        targets: planned,
    })
}

fn resolve_and_validate_home(home_arg: Option<PathBuf>) -> Result<PathBuf, String> {
    let raw = match home_arg {
        Some(path) => path,
        None => crate::utils::resolve_home_arg(None)?,
    };
    reject_user_symlink_ancestors(&raw, "home")?;
    let meta =
        fs::symlink_metadata(&raw).map_err(|e| format!("inspect home '{}': {e}", raw.display()))?;
    if meta.file_type().is_symlink() {
        return Err(format!("symlink home is not allowed: {}", raw.display()));
    }
    if !meta.file_type().is_dir() {
        return Err(format!("home is not a directory: {}", raw.display()));
    }
    raw.canonicalize()
        .map_err(|e| format!("canonicalize home '{}': {e}", raw.display()))
}

fn reject_user_symlink_ancestors(path: &Path, label: &str) -> Result<(), String> {
    let abs = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|e| format!("resolve cwd for {label}: {e}"))?
            .join(path)
    };
    let mut cur = PathBuf::new();
    for comp in abs.components() {
        cur.push(comp);
        if matches!(comp, Component::RootDir | Component::Prefix(_)) {
            continue;
        }
        #[cfg(target_os = "macos")]
        if cur == Path::new("/tmp") || cur == Path::new("/var") || cur == Path::new("/etc") {
            continue;
        }
        if let Ok(meta) = fs::symlink_metadata(&cur) {
            if meta.file_type().is_symlink() {
                return Err(format!(
                    "symlink ancestor is not allowed for {label}: {}",
                    cur.display()
                ));
            }
        }
    }
    Ok(())
}

struct LoadedMachineLocal {
    path: Option<PathBuf>,
    content: Option<String>,
    sha256: Option<String>,
}

fn load_machine_local(home: &Path, explicit: Option<&Path>) -> Result<LoadedMachineLocal, String> {
    let (path, required) = match explicit {
        Some(p) => {
            reject_user_symlink_ancestors(p, "machine_local")?;
            (p.to_path_buf(), true)
        }
        None => (home.join(".tachi").join("machine.local.md"), false),
    };

    if explicit.is_none() {
        check_existing_target_parents_no_symlink(home, Path::new(".tachi/machine.local.md"))?;
    }

    let meta = match fs::symlink_metadata(&path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound && !required => {
            return Ok(LoadedMachineLocal {
                path: None,
                content: None,
                sha256: None,
            });
        }
        Err(e) => {
            return Err(format!("inspect machine_local '{}': {e}", path.display()));
        }
    };

    if meta.file_type().is_symlink() {
        return Err(format!(
            "symlink machine_local is not allowed: {}",
            path.display()
        ));
    }
    if !meta.file_type().is_file() {
        return Err(format!(
            "machine_local is not a regular file: {}",
            path.display()
        ));
    }

    let bytes = read_regular_file_bytes_nofollow(&path)?;
    let text = String::from_utf8(bytes)
        .map_err(|_| format!("machine_local '{}' is not valid UTF-8", path.display()))?;
    let normalized = normalize_lf(&text);

    if normalized.contains("<!-- tachi:instruction-sync")
        || normalized.contains("<!-- tachi:machine-local:")
    {
        return Err(format!(
            "machine_local '{}' must not contain Tachi sync markers",
            path.display()
        ));
    }
    screen_secrets("machine_local", &normalized)?;

    let digest = sha256_hex(normalized.as_bytes());
    Ok(LoadedMachineLocal {
        path: Some(path),
        content: Some(normalized),
        sha256: Some(digest),
    })
}

fn validate_identifier(value: &str, max_len: usize, label: &str) -> Result<(), String> {
    if value.is_empty()
        || value.len() > max_len
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
    {
        return Err(format!(
            "invalid {label} '{value}': expected 1..={max_len} ASCII [A-Za-z0-9._-] characters"
        ));
    }
    Ok(())
}

fn validate_budget(max_bytes: u64, label: &str) -> Result<(), String> {
    if max_bytes == 0 || max_bytes > MAX_TARGET_BUDGET_BYTES {
        return Err(format!(
            "invalid max_bytes {max_bytes} for '{label}': expected 1..={MAX_TARGET_BUDGET_BYTES}"
        ));
    }
    Ok(())
}

fn validate_relative_path(raw: &str, label: &str) -> Result<(), String> {
    if raw.is_empty()
        || raw.contains('\0')
        || raw.starts_with('/')
        || raw.ends_with('/')
        || raw.starts_with('~')
        || raw.contains('\\')
        || raw.contains("//")
    {
        return Err(format!("invalid relative path for {label}: '{raw}'"));
    }
    let path = Path::new(raw);
    let mut has_normal = false;
    for component in path.components() {
        match component {
            Component::Normal(seg) if !seg.is_empty() => has_normal = true,
            _ => {
                return Err(format!(
                    "path traversal or non-normal segment in {label}: '{raw}'"
                ))
            }
        }
    }
    if !has_normal {
        return Err(format!("empty relative path for {label}"));
    }
    Ok(())
}

fn validate_host_target_prefix(host: &str, target: &str) -> Result<(), String> {
    let allowed = match host {
        "constitution" => target.starts_with(".agents/constitution/") && target.ends_with(".md"),
        "codex" => target.starts_with(".codex/"),
        "claude" => target.starts_with(".claude/"),
        "antigravity" => {
            target.starts_with(".gemini/config/") || target.starts_with(".gemini/antigravity/")
        }
        "gemini" => {
            target.starts_with(".gemini/")
                && !target.starts_with(".gemini/config/")
                && !target.starts_with(".gemini/antigravity/")
        }
        "cursor" => target.starts_with(".cursor/rules/"),
        _ => false,
    };
    if !allowed {
        return Err(format!(
            "target path '{target}' is outside the allowed prefix for host '{host}'"
        ));
    }
    Ok(())
}

fn validate_unique_ids_and_targets(
    decls: &[TargetDecl],
    home: &Path,
    source_dir: &Path,
) -> Result<(), String> {
    let mut ids = BTreeSet::new();
    let mut lowered_targets: Vec<PathBuf> = Vec::with_capacity(decls.len());
    let backups_root = home.join(".tachi").join("backups");

    for decl in decls {
        if !ids.insert(decl.id.as_str()) {
            return Err(format!("duplicate target id '{}'", decl.id));
        }
        let abs = home.join(&decl.target);
        if abs == source_dir || abs.starts_with(source_dir) || source_dir.starts_with(&abs) {
            return Err(format!(
                "target '{}' overlaps source_dir '{}'",
                decl.target,
                source_dir.display()
            ));
        }
        if abs.starts_with(&backups_root) {
            return Err(format!(
                "target '{}' must not point inside Tachi backups",
                decl.target
            ));
        }
        let lowered = PathBuf::from(decl.target.to_ascii_lowercase());
        for prior in &lowered_targets {
            if &lowered == prior || lowered.starts_with(prior) || prior.starts_with(&lowered) {
                return Err(format!(
                    "duplicate or overlapping targets (case-insensitive): '{}' and '{}'",
                    prior.display(),
                    decl.target
                ));
            }
        }
        lowered_targets.push(lowered);
    }
    Ok(())
}

fn validate_shared_source_content(source_rel: &str, content: &str) -> Result<(), String> {
    if content.contains("<!-- tachi:instruction-sync")
        || content.contains("<!-- tachi:machine-local:")
    {
        return Err(format!(
            "shared source '{source_rel}' must not contain Tachi sync markers"
        ));
    }
    if content.contains("/Users/") || content.contains("/home/") {
        return Err(format!(
            "shared source '{source_rel}' contains a hardcoded home path (/Users/ or /home/); use '~/', '{{{{HOME}}}}', include_constitution_import, or machine.local.md"
        ));
    }
    screen_secrets(source_rel, content)
}

fn screen_secrets(label: &str, content: &str) -> Result<(), String> {
    const LITERAL_PREFIXES: &[&str] = &[
        "sk-ant-",
        "sk-proj-",
        "ghp_",
        "github_pat_",
        "glpat-",
        "xoxb-",
        "xoxp-",
        "AIza",
        "-----BEGIN ",
    ];
    for prefix in LITERAL_PREFIXES {
        if content.contains(prefix) {
            return Err(format!(
                "secret pattern rejected in '{label}' (redacted); never store credentials in instruction files"
            ));
        }
    }
    if contains_aws_access_key_id(content) {
        return Err(format!(
            "secret pattern rejected in '{label}' (redacted); never store credentials in instruction files"
        ));
    }
    Ok(())
}

fn contains_aws_access_key_id(content: &str) -> bool {
    let bytes = content.as_bytes();
    if bytes.len() < 20 {
        return false;
    }
    for window in bytes.windows(20) {
        if &window[..4] == b"AKIA"
            && window[4..]
                .iter()
                .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
        {
            return true;
        }
    }
    false
}

fn starts_with_yaml_frontmatter(content: &str) -> bool {
    content.starts_with("---\n")
}

fn normalize_lf(content: &str) -> String {
    content.replace("\r\n", "\n")
}

fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn is_sha256_hex(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

#[cfg(not(unix))]
fn read_relative_regular_file(root: &Path, rel: &str) -> Result<Vec<u8>, String> {
    validate_relative_path(rel, "source file")?;
    let mut current = root.to_path_buf();
    let rel_path = Path::new(rel);
    let components: Vec<_> = rel_path.components().collect();
    for (idx, component) in components.iter().enumerate() {
        let Component::Normal(seg) = component else {
            return Err(format!("invalid path segment in '{rel}'"));
        };
        current.push(seg);
        let meta = fs::symlink_metadata(&current)
            .map_err(|e| format!("inspect '{}': {e}", current.display()))?;
        if meta.file_type().is_symlink() {
            return Err(format!(
                "symlink source is not allowed: {}",
                current.display()
            ));
        }
        let is_last = idx + 1 == components.len();
        if is_last {
            if !meta.file_type().is_file() {
                return Err(format!(
                    "source is not a regular file: {}",
                    current.display()
                ));
            }
        } else if !meta.file_type().is_dir() {
            return Err(format!(
                "source parent is not a directory: {}",
                current.display()
            ));
        }
    }
    read_regular_file_bytes_nofollow(&current)
}

pub(super) fn read_regular_file_bytes_nofollow(path: &Path) -> Result<Vec<u8>, String> {
    #[cfg(unix)]
    let file = {
        use std::os::unix::fs::OpenOptionsExt;
        fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(path)
            .map_err(|e| format!("open '{}': {e}", path.display()))?
    };
    #[cfg(not(unix))]
    let file = fs::OpenOptions::new()
        .read(true)
        .open(path)
        .map_err(|e| format!("open '{}': {e}", path.display()))?;

    let meta = file
        .metadata()
        .map_err(|e| format!("stat '{}': {e}", path.display()))?;
    if !meta.file_type().is_file() {
        return Err(format!("not a regular file: {}", path.display()));
    }
    if meta.len() > MAX_INPUT_FILE_BYTES {
        return Err(format!(
            "file '{}' exceeds maximum allowed size ({} > {MAX_INPUT_FILE_BYTES} bytes)",
            path.display(),
            meta.len()
        ));
    }

    let mut bytes = Vec::with_capacity(meta.len() as usize);
    file.take(MAX_INPUT_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("read '{}': {e}", path.display()))?;
    if bytes.len() as u64 > MAX_INPUT_FILE_BYTES {
        return Err(format!(
            "file '{}' grew beyond maximum allowed size ({MAX_INPUT_FILE_BYTES} bytes)",
            path.display()
        ));
    }
    Ok(bytes)
}

fn check_existing_target_parents_no_symlink(home: &Path, rel_target: &Path) -> Result<(), String> {
    let Some(parent) = rel_target.parent() else {
        return Ok(());
    };
    let mut current = home.to_path_buf();
    for component in parent.components() {
        let Component::Normal(seg) = component else {
            return Err(format!(
                "invalid target parent in '{}'",
                rel_target.display()
            ));
        };
        current.push(seg);
        match fs::symlink_metadata(&current) {
            Ok(meta) => {
                if meta.file_type().is_symlink() {
                    return Err(format!("symlink target parent: {}", current.display()));
                }
                if !meta.file_type().is_dir() {
                    return Err(format!(
                        "non-directory target parent: {}",
                        current.display()
                    ));
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => break,
            Err(e) => {
                return Err(format!(
                    "inspect target parent '{}': {e}",
                    current.display()
                ))
            }
        }
    }
    Ok(())
}

#[cfg(not(unix))]
fn ensure_safe_dir_under_home(home: &Path, rel_dir: &Path, owner_only: bool) -> Result<(), String> {
    let mut current = home.to_path_buf();
    for component in rel_dir.components() {
        let Component::Normal(seg) = component else {
            return Err(format!(
                "invalid directory segment in '{}'",
                rel_dir.display()
            ));
        };
        current.push(seg);
        match fs::symlink_metadata(&current) {
            Ok(meta) => {
                if meta.file_type().is_symlink() {
                    return Err(format!("symlink target parent: {}", current.display()));
                }
                if !meta.file_type().is_dir() {
                    return Err(format!(
                        "non-directory target parent: {}",
                        current.display()
                    ));
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                fs::create_dir(&current)
                    .map_err(|err| format!("create directory '{}': {err}", current.display()))?;
                #[cfg(unix)]
                if owner_only {
                    use std::os::unix::fs::PermissionsExt;
                    let _ = fs::set_permissions(&current, fs::Permissions::from_mode(0o700));
                }
            }
            Err(e) => return Err(format!("inspect directory '{}': {e}", current.display())),
        }
    }
    Ok(())
}

fn inspect_target_condition(
    home: &Path,
    target_rel: &str,
    rendered: &str,
) -> Result<(TargetCondition, Option<FileFingerprint>), String> {
    let target_path = home.join(target_rel);
    let meta = match fs::symlink_metadata(&target_path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok((TargetCondition::Create, None))
        }
        Err(e) => return Err(format!("inspect target '{}': {e}", target_path.display())),
    };
    let fp = fingerprint_from_metadata(&meta);

    if meta.file_type().is_symlink() {
        let raw_target = fs::read_link(&target_path)
            .map_err(|e| format!("read symlink target '{}': {e}", target_path.display()))?;
        let link_target = raw_target.display().to_string();
        let link_target_b64 = {
            use base64::Engine;
            #[cfg(unix)]
            let raw_bytes = {
                use std::os::unix::ffi::OsStrExt;
                raw_target.as_os_str().as_bytes()
            };
            #[cfg(not(unix))]
            let raw_bytes = link_target.as_bytes();
            base64::engine::general_purpose::STANDARD.encode(raw_bytes)
        };
        return Ok((
            TargetCondition::ConflictSymlink {
                link_target,
                link_target_b64,
            },
            Some(fp),
        ));
    }
    if !meta.file_type().is_file() {
        return Err(format!(
            "unsupported non-file target at '{}'",
            target_path.display()
        ));
    }

    #[cfg(unix)]
    let bytes = {
        let home_fd = open_dir_nofollow(home)?;
        read_file_under_dir_fd(&home_fd, target_rel)?
    };
    #[cfg(not(unix))]
    let bytes = read_regular_file_bytes_nofollow(&target_path)?;

    let Ok(existing) = String::from_utf8(bytes) else {
        return Ok((TargetCondition::ConflictUnmanaged, Some(fp)));
    };

    let cond = match inspect_sync_stamp(&existing) {
        SyncStampState::ValidClean { .. } => {
            if existing == rendered {
                TargetCondition::InSync
            } else {
                TargetCondition::UpdateManaged
            }
        }
        SyncStampState::TamperedOrEdited => TargetCondition::ConflictModifiedManaged,
        SyncStampState::Absent => TargetCondition::ConflictUnmanaged,
    };
    Ok((cond, Some(fp)))
}

fn fingerprint_from_metadata(meta: &fs::Metadata) -> FileFingerprint {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        FileFingerprint {
            dev: meta.dev(),
            ino: meta.ino(),
            mtime: meta.mtime(),
            mtime_nsec: meta.mtime_nsec(),
            size: meta.size(),
        }
    }
    #[cfg(not(unix))]
    {
        FileFingerprint {
            dev: 0,
            ino: 0,
            mtime: 0,
            mtime_nsec: 0,
            size: meta.len(),
        }
    }
}

#[cfg(unix)]
fn c_component(seg: &std::ffi::OsStr) -> Result<std::ffi::CString, String> {
    use std::os::unix::ffi::OsStrExt;
    std::ffi::CString::new(seg.as_bytes()).map_err(|_| "NUL byte in path component".to_string())
}

#[cfg(unix)]
fn open_dir_nofollow(path: &Path) -> Result<fs::File, String> {
    use std::os::unix::fs::OpenOptionsExt;
    fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
        .map_err(|e| format!("open directory '{}': {e}", path.display()))
}

#[cfg(unix)]
fn open_sub_dir_at(
    root: &fs::File,
    rel_dir: &Path,
    create: bool,
    mode: u32,
) -> Result<fs::File, String> {
    use std::os::fd::{AsRawFd, FromRawFd};
    let mut current = root
        .try_clone()
        .map_err(|e| format!("clone directory fd: {e}"))?;
    for comp in rel_dir.components() {
        let Component::Normal(seg) = comp else {
            return Err(format!(
                "invalid directory segment in '{}'",
                rel_dir.display()
            ));
        };
        let c_name = c_component(seg)?;
        let open_child = |parent: &fs::File| -> std::io::Result<fs::File> {
            // SAFETY: live parent directory fd and NUL-terminated single segment; no O_CREAT.
            let fd = unsafe {
                libc::openat(
                    parent.as_raw_fd(),
                    c_name.as_ptr(),
                    libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                )
            };
            if fd < 0 {
                Err(std::io::Error::last_os_error())
            } else {
                // SAFETY: fresh exclusively owned fd from openat.
                Ok(unsafe { fs::File::from_raw_fd(fd) })
            }
        };
        current = match open_child(&current) {
            Ok(dir) => dir,
            Err(e) if create && e.kind() == std::io::ErrorKind::NotFound => {
                // SAFETY: live directory fd, NUL-terminated name, valid mode.
                if unsafe {
                    libc::mkdirat(current.as_raw_fd(), c_name.as_ptr(), mode as libc::mode_t)
                } < 0
                {
                    let err = std::io::Error::last_os_error();
                    if err.kind() != std::io::ErrorKind::AlreadyExists {
                        return Err(format!("mkdirat '{}': {err}", seg.to_string_lossy()));
                    }
                }
                open_child(&current).map_err(|err| {
                    format!(
                        "symlink or unsafe directory segment '{}': {err}",
                        seg.to_string_lossy()
                    )
                })?
            }
            Err(e) => {
                return Err(format!(
                    "symlink or unsafe directory segment '{}': {e}",
                    seg.to_string_lossy()
                ))
            }
        };
    }
    Ok(current)
}

#[cfg(unix)]
fn create_unique_dir_under_fd(
    root: &fs::File,
    parent_rel: &Path,
    leaf: &str,
    mode: u32,
) -> Result<(), String> {
    use std::os::fd::AsRawFd;
    let parent = open_sub_dir_at(root, parent_rel, true, mode)?;
    let c_leaf = c_component(std::ffi::OsStr::new(leaf))?;
    // SAFETY: live parent fd, NUL-terminated single leaf, valid mode; fails if already exists.
    if unsafe { libc::mkdirat(parent.as_raw_fd(), c_leaf.as_ptr(), mode as libc::mode_t) } < 0 {
        return Err(format!(
            "create unique directory '{leaf}': {}",
            std::io::Error::last_os_error()
        ));
    }
    Ok(())
}

#[cfg(unix)]
fn read_file_under_dir_fd(root: &fs::File, rel: &str) -> Result<Vec<u8>, String> {
    use std::os::fd::{AsRawFd, FromRawFd};
    validate_relative_path(rel, "relative file")?;
    let rel_path = Path::new(rel);
    let parent_rel = rel_path.parent().unwrap_or_else(|| Path::new(""));
    let leaf = rel_path
        .file_name()
        .ok_or_else(|| format!("missing filename in '{rel}'"))?;
    let parent = open_sub_dir_at(root, parent_rel, false, 0)?;
    let c_leaf = c_component(leaf)?;
    // SAFETY: live parent fd, NUL-terminated leaf, read-only no-follow non-blocking open.
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            c_leaf.as_ptr(),
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(format!(
            "openat '{rel}': {}",
            std::io::Error::last_os_error()
        ));
    }
    // SAFETY: fresh exclusively owned fd from openat.
    let file = unsafe { fs::File::from_raw_fd(fd) };
    let meta = file.metadata().map_err(|e| format!("stat '{rel}': {e}"))?;
    if !meta.file_type().is_file() {
        return Err(format!("not a regular file: {rel}"));
    }
    if meta.len() > MAX_INPUT_FILE_BYTES {
        return Err(format!(
            "file '{rel}' exceeds maximum allowed size ({} > {MAX_INPUT_FILE_BYTES} bytes)",
            meta.len()
        ));
    }
    let mut bytes = Vec::with_capacity(meta.len() as usize);
    file.take(MAX_INPUT_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("read '{rel}': {e}"))?;
    if bytes.len() as u64 > MAX_INPUT_FILE_BYTES {
        return Err(format!(
            "file '{rel}' grew beyond maximum allowed size ({MAX_INPUT_FILE_BYTES} bytes)"
        ));
    }
    Ok(bytes)
}

#[cfg(unix)]
fn fstat_fingerprint_at(
    parent: &fs::File,
    c_leaf: &std::ffi::CString,
) -> Result<Option<FileFingerprint>, String> {
    use std::os::fd::AsRawFd;
    let mut stat_buf = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: valid parent fd, NUL-terminated name, uninit stat buffer written by fstatat.
    if unsafe {
        libc::fstatat(
            parent.as_raw_fd(),
            c_leaf.as_ptr(),
            stat_buf.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    } < 0
    {
        let err = std::io::Error::last_os_error();
        if err.kind() == std::io::ErrorKind::NotFound {
            return Ok(None);
        }
        return Err(format!("fstatat: {err}"));
    }
    // SAFETY: fstatat succeeded and initialized stat_buf.
    let st = unsafe { stat_buf.assume_init() };
    Ok(Some(FileFingerprint {
        dev: st.st_dev as u64,
        ino: st.st_ino as u64,
        mtime: st.st_mtime as i64,
        mtime_nsec: st.st_mtime_nsec as i64,
        size: st.st_size as u64,
    }))
}

#[cfg(unix)]
fn write_rel_file_anchored(
    _home: &Path,
    home_fd: &fs::File,
    rel_path: &Path,
    bytes: &[u8],
    owner_only_dirs: bool,
    exclusive: bool,
    expected_fp: Option<FileFingerprint>,
) -> Result<(), String> {
    use std::io::Write;
    use std::os::fd::{AsRawFd, FromRawFd};

    let parent_rel = rel_path.parent().unwrap_or_else(|| Path::new(""));
    let leaf = rel_path
        .file_name()
        .ok_or_else(|| format!("missing filename in '{}'", rel_path.display()))?;
    let dir_mode = if owner_only_dirs { 0o700 } else { 0o755 };
    let parent = open_sub_dir_at(home_fd, parent_rel, true, dir_mode)?;
    let c_leaf = c_component(leaf)?;

    if exclusive {
        // SAFETY: live parent fd, NUL-terminated leaf, O_CREAT|O_EXCL|O_NOFOLLOW prevents clobbering.
        let fd = unsafe {
            libc::openat(
                parent.as_raw_fd(),
                c_leaf.as_ptr(),
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                0o600 as libc::c_uint,
            )
        };
        if fd < 0 {
            return Err(format!(
                "exclusive create '{}': {}",
                rel_path.display(),
                std::io::Error::last_os_error()
            ));
        }
        // SAFETY: fresh owned fd from openat.
        let mut file = unsafe { fs::File::from_raw_fd(fd) };
        file.write_all(bytes)
            .and_then(|_| file.sync_all())
            .map_err(|e| format!("write '{}': {e}", rel_path.display()))?;
        drop(file);
        parent
            .sync_all()
            .map_err(|e| format!("fsync parent dir: {e}"))?;
        return Ok(());
    }

    let tmp_name = format!(".tachi-sync-tmp.{}", uuid::Uuid::new_v4().as_simple());
    let c_tmp = c_component(std::ffi::OsStr::new(&tmp_name))?;
    // SAFETY: live parent fd, NUL-terminated temp name, O_CREAT|O_EXCL|O_NOFOLLOW.
    let tmp_fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            c_tmp.as_ptr(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            0o600 as libc::c_uint,
        )
    };
    if tmp_fd < 0 {
        return Err(format!(
            "open temp '{}': {}",
            tmp_name,
            std::io::Error::last_os_error()
        ));
    }
    let write_res = (|| -> Result<(), String> {
        // SAFETY: fresh owned fd from openat.
        let mut tmp_file = unsafe { fs::File::from_raw_fd(tmp_fd) };
        tmp_file
            .write_all(bytes)
            .and_then(|_| tmp_file.sync_all())
            .map_err(|e| format!("write temp '{}': {e}", tmp_name))?;
        drop(tmp_file);

        if let Some(expected) = expected_fp {
            let current = fstat_fingerprint_at(&parent, &c_leaf)?;
            if current != Some(expected) {
                return Err(format!(
                    "target '{}' changed concurrently before atomic rename; aborting without overwrite",
                    rel_path.display()
                ));
            }
        }

        // SAFETY: live parent fd and NUL-terminated sibling names.
        if unsafe {
            libc::renameat(
                parent.as_raw_fd(),
                c_tmp.as_ptr(),
                parent.as_raw_fd(),
                c_leaf.as_ptr(),
            )
        } < 0
        {
            return Err(format!(
                "renameat '{}': {}",
                rel_path.display(),
                std::io::Error::last_os_error()
            ));
        }
        parent
            .sync_all()
            .map_err(|e| format!("fsync parent dir: {e}"))
    })();

    if write_res.is_err() {
        // SAFETY: best-effort cleanup of sibling temp file in pinned parent fd.
        unsafe {
            libc::unlinkat(parent.as_raw_fd(), c_tmp.as_ptr(), 0);
        }
    }
    write_res
}

#[cfg(not(unix))]
fn write_rel_file_anchored(
    home: &Path,
    rel_path: &Path,
    bytes: &[u8],
    owner_only_dirs: bool,
    exclusive: bool,
    _expected_fp: Option<FileFingerprint>,
) -> Result<(), String> {
    if let Some(parent_rel) = rel_path.parent() {
        if !parent_rel.as_os_str().is_empty() {
            ensure_safe_dir_under_home(home, parent_rel, owner_only_dirs)?;
        }
    }
    let full = home.join(rel_path);
    if exclusive {
        write_new_file_exclusive(&full, bytes)
    } else {
        crate::utils::write_owner_only_file_atomic(&full, bytes)
    }
}

#[cfg(not(unix))]
fn write_new_file_exclusive(path: &Path, bytes: &[u8]) -> Result<(), String> {
    use std::io::Write;
    #[cfg(unix)]
    let mut file = {
        use std::os::unix::fs::OpenOptionsExt;
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .custom_flags(libc::O_NOFOLLOW)
            .mode(0o600)
            .open(path)
            .map_err(|e| format!("create exclusive '{}': {e}", path.display()))?
    };
    #[cfg(not(unix))]
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|e| format!("create exclusive '{}': {e}", path.display()))?;

    file.write_all(bytes)
        .map_err(|e| format!("write '{}': {e}", path.display()))?;
    file.sync_all()
        .map_err(|e| format!("fsync '{}': {e}", path.display()))?;
    drop(file);
    crate::utils::sync_parent_dir(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_dir(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = crate::utils::test_fixture_path(format!(
            "tachi-sync-{name}-{}-{nanos}",
            std::process::id()
        ));
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn write_six_surface_bundle(bundle_dir: &Path, revision: &str) {
        fs::create_dir_all(bundle_dir.join("constitution")).unwrap();
        fs::create_dir_all(bundle_dir.join("adapters/codex")).unwrap();
        fs::create_dir_all(bundle_dir.join("adapters/claude/agents")).unwrap();
        fs::create_dir_all(bundle_dir.join("adapters/claude/skills/dispatch")).unwrap();
        fs::create_dir_all(bundle_dir.join("adapters/antigravity")).unwrap();

        fs::write(
            bundle_dir.join("constitution/core.md"),
            "# Core Constitution\n\n- Evidence first.\n- Never weaken frozen assertions.\n",
        )
        .unwrap();
        fs::write(
            bundle_dir.join("adapters/codex/AGENTS.md"),
            "<!-- CODEX ADAPTER -->\n# Codex\n\nShared baseline: `~/.agents/constitution/core.md`.\n",
        )
        .unwrap();
        fs::write(
            bundle_dir.join("adapters/claude/CLAUDE.md"),
            "# Claude Adapter\n\nKeep turn-zero rules concise.\n",
        )
        .unwrap();
        fs::write(
            bundle_dir.join("adapters/claude/agents/wizard.md"),
            "---\nname: wizard\ndescription: Bounded implementation seat\n---\n\n# Wizard\n",
        )
        .unwrap();
        fs::write(
            bundle_dir.join("adapters/claude/skills/dispatch/SKILL.md"),
            "---\nname: dispatch\ndescription: Subagent dispatch rules\n---\n\n# Dispatch\n",
        )
        .unwrap();
        fs::write(
            bundle_dir.join("adapters/antigravity/AGENTS.md"),
            "# Antigravity Global Rules\n\nLocal CLI path: `{{HOME}}/.local/bin/tool`.\n",
        )
        .unwrap();

        let manifest = serde_json::json!({
            "schema_version": "tachi.instruction_bundle.v1",
            "revision": revision,
            "constitution": {
                "source": "constitution/core.md",
                "target": ".agents/constitution/core.md",
                "max_bytes": 4096
            },
            "targets": [
                {
                    "id": "codex-global-agents",
                    "host": "codex",
                    "source": "adapters/codex/AGENTS.md",
                    "target": ".codex/AGENTS.md",
                    "include_constitution_import": false,
                    "include_machine_local": true,
                    "max_bytes": 12288
                },
                {
                    "id": "claude-global-md",
                    "host": "claude",
                    "source": "adapters/claude/CLAUDE.md",
                    "target": ".claude/CLAUDE.md",
                    "include_constitution_import": true,
                    "include_machine_local": true,
                    "max_bytes": 12288
                },
                {
                    "id": "claude-agent-wizard",
                    "host": "claude",
                    "source": "adapters/claude/agents/wizard.md",
                    "target": ".claude/agents/wizard.md",
                    "include_constitution_import": false,
                    "include_machine_local": true,
                    "max_bytes": 16384
                },
                {
                    "id": "claude-skill-dispatch",
                    "host": "claude",
                    "source": "adapters/claude/skills/dispatch/SKILL.md",
                    "target": ".claude/skills/dispatch/SKILL.md",
                    "include_constitution_import": false,
                    "include_machine_local": false,
                    "max_bytes": 12288
                },
                {
                    "id": "antigravity-global-agents",
                    "host": "antigravity",
                    "source": "adapters/antigravity/AGENTS.md",
                    "target": ".gemini/config/AGENTS.md",
                    "include_constitution_import": false,
                    "include_machine_local": true,
                    "max_bytes": 12288
                }
            ]
        });
        fs::write(
            bundle_dir.join("bundle.json"),
            serde_json::to_string_pretty(&manifest).unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn six_surface_layered_sync_preserves_byte_zero_and_upgrades_cleanly() {
        let root = temp_dir("six-surface");
        let home = root.join("home");
        let bundle = root.join("bundle");
        fs::create_dir_all(home.join(".tachi")).unwrap();
        write_six_surface_bundle(&bundle, "2026-11-28.1");
        fs::write(
            home.join(".tachi/machine.local.md"),
            "## Machine-Local Environment\n\n- Build gate: `/Users/Shared/cargo-gate`\n",
        )
        .unwrap();

        let opts = SyncOptions {
            source_dir: Some(bundle.clone()),
            machine_local: None,
            hosts: vec![],
            home: Some(home.clone()),
            adopt: false,
        };

        let plan = run_plan(&opts).expect("plan succeeds");
        assert_eq!(plan.status, "ready");
        assert_eq!(plan.summary.targets, 6);
        assert_eq!(plan.summary.create, 6);

        let sync = run_sync(&opts).expect("initial sync succeeds");
        assert_eq!(sync.summary.written, 6);
        assert!(home.join(".tachi/harness-sync-receipt.json").is_file());

        let canonical_home = home.canonicalize().unwrap();
        let claude_md = fs::read_to_string(home.join(".claude/CLAUDE.md")).unwrap();
        assert!(claude_md.starts_with(&format!(
            "@{}/.agents/constitution/core.md\n\n# Claude Adapter",
            canonical_home.display()
        )));
        assert!(claude_md.contains("/Users/Shared/cargo-gate"));

        let wizard_md = fs::read_to_string(home.join(".claude/agents/wizard.md")).unwrap();
        assert!(wizard_md.starts_with("---\nname: wizard\n"));
        assert!(wizard_md.contains("/Users/Shared/cargo-gate"));

        let dispatch_md =
            fs::read_to_string(home.join(".claude/skills/dispatch/SKILL.md")).unwrap();
        assert!(dispatch_md.starts_with("---\nname: dispatch\n"));
        assert!(!dispatch_md.contains("/Users/Shared/cargo-gate"));

        let ag_md = fs::read_to_string(home.join(".gemini/config/AGENTS.md")).unwrap();
        assert!(ag_md.contains(&format!("{}/.local/bin/tool", canonical_home.display())));

        // Second plan is in_sync; updating bundle upgrades cleanly without --adopt.
        let second_plan = run_plan(&opts).expect("second plan");
        assert_eq!(second_plan.status, "in_sync");
        assert_eq!(second_plan.summary.in_sync, 6);

        write_six_surface_bundle(&bundle, "2026-11-28.2");
        let upgrade = run_sync(&opts).expect("clean upgrade without --adopt");
        assert_eq!(upgrade.summary.update_managed, 6);
        assert_eq!(upgrade.summary.written, 6);

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn symlink_and_unmanaged_conflicts_fail_closed_and_adopt_safely() {
        let root = temp_dir("adopt-symlink");
        let home = root.join("home");
        let bundle = root.join("bundle");
        let release_file = root.join("pinned-release-codex.md");
        fs::create_dir_all(home.join(".codex")).unwrap();
        fs::create_dir_all(home.join(".claude")).unwrap();
        write_six_surface_bundle(&bundle, "2026-11-28.1");

        let pinned_original = "immutable release checkout bytes\n";
        fs::write(&release_file, pinned_original).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&release_file, home.join(".codex/AGENTS.md")).unwrap();
        fs::write(
            home.join(".claude/CLAUDE.md"),
            "# Hand-written legacy file\n",
        )
        .unwrap();

        let no_adopt = SyncOptions {
            source_dir: Some(bundle.clone()),
            machine_local: None,
            hosts: vec![],
            home: Some(home.clone()),
            adopt: false,
        };
        let err = run_sync(&no_adopt).expect_err("must refuse conflicts without --adopt");
        assert!(err.contains("preflight blocked"));
        // Zero targets written on failed preflight.
        assert!(!home.join(".agents/constitution/core.md").exists());
        assert_eq!(fs::read_to_string(&release_file).unwrap(), pinned_original);

        let adopt = SyncOptions {
            adopt: true,
            ..no_adopt
        };
        let synced = run_sync(&adopt).expect("adopt sync succeeds");
        assert_eq!(synced.summary.adopted, 2);
        assert!(synced.backup_dir.is_some());
        // Symlink was replaced atomically with a regular file without modifying the former target.
        assert!(!fs::symlink_metadata(home.join(".codex/AGENTS.md"))
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(fs::read_to_string(&release_file).unwrap(), pinned_original);

        let backup_root = PathBuf::from(synced.backup_dir.unwrap());
        assert_eq!(
            fs::read_to_string(backup_root.join("files/.claude/CLAUDE.md")).unwrap(),
            "# Hand-written legacy file\n"
        );
        assert!(backup_root.join("symlinks/.codex/AGENTS.md.json").is_file());

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn manual_edit_after_sync_triggers_drift_refusal() {
        let root = temp_dir("manual-drift");
        let home = root.join("home");
        let bundle = root.join("bundle");
        fs::create_dir_all(&home).unwrap();
        write_six_surface_bundle(&bundle, "2026-11-28.1");

        let opts = SyncOptions {
            source_dir: Some(bundle.clone()),
            machine_local: None,
            hosts: vec!["claude".to_string(), "constitution".to_string()],
            home: Some(home.clone()),
            adopt: false,
        };
        run_sync(&opts).expect("initial sync");

        let claude_path = home.join(".claude/CLAUDE.md");
        let edited =
            fs::read_to_string(&claude_path)
                .unwrap()
                .replacen("concise.", "edited manually.", 1);
        fs::write(&claude_path, edited).unwrap();

        let plan = run_plan(&opts).expect("plan detects drift");
        assert_eq!(plan.status, "blocked_by_conflicts");
        assert_eq!(plan.summary.conflicts, 1);
        assert!(plan
            .targets
            .iter()
            .any(|t| t.id == "claude-global-md" && t.condition == "conflict_modified_managed"));

        let err = run_sync(&opts).expect_err("sync refuses hand-edited target");
        assert!(err.contains("conflict_modified_managed"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn guardrails_reject_hardcoded_paths_secrets_frontmatter_import_and_symlink_parents() {
        let root = temp_dir("guardrails");
        let home = root.join("home");
        let bundle = root.join("bundle");
        fs::create_dir_all(&home).unwrap();
        write_six_surface_bundle(&bundle, "2026-11-28.1");

        // 1. Hardcoded /Users/ path in shared adapter fails.
        fs::write(
            bundle.join("adapters/codex/AGENTS.md"),
            "Read /Users/alice/.agents/constitution/core.md\n",
        )
        .unwrap();
        let opts = SyncOptions {
            source_dir: Some(bundle.clone()),
            machine_local: None,
            hosts: vec![],
            home: Some(home.clone()),
            adopt: false,
        };
        let err = run_plan(&opts).expect_err("hardcoded /Users/ must fail");
        assert!(err.contains("hardcoded home path"));

        // 2. Secret literal in shared source fails without echoing secret value.
        write_six_surface_bundle(&bundle, "2026-11-28.1");
        let fake_token = format!("{}{}", "ghp_", "SecretToken1234567890");
        fs::write(
            bundle.join("adapters/codex/AGENTS.md"),
            format!("token={fake_token}\n"),
        )
        .unwrap();
        let err = run_plan(&opts).expect_err("secret literal must fail");
        assert!(err.contains("secret pattern rejected"));
        assert!(!err.contains(&fake_token));

        // 3. include_constitution_import on YAML frontmatter fails.
        write_six_surface_bundle(&bundle, "2026-11-28.1");
        let mut manifest_json: serde_json::Value =
            serde_json::from_slice(&fs::read(bundle.join("bundle.json")).unwrap()).unwrap();
        let wizard_entry = manifest_json["targets"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|item| item["id"] == "claude-agent-wizard")
            .unwrap();
        wizard_entry["include_constitution_import"] = serde_json::json!(true);
        fs::write(
            bundle.join("bundle.json"),
            serde_json::to_string_pretty(&manifest_json).unwrap(),
        )
        .unwrap();
        let err = run_plan(&opts).expect_err("frontmatter + constitution import must fail");
        assert!(err.contains("YAML frontmatter"));

        // 4. Symlinked target parent directory fails closed.
        write_six_surface_bundle(&bundle, "2026-11-28.1");
        let outside = root.join("outside");
        fs::create_dir_all(&outside).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside, home.join(".claude")).unwrap();
        let err = run_plan(&opts).expect_err("symlink parent must fail");
        assert!(err.contains("symlink target parent"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn r1_review_regressions_stamp_panic_double_slash_case_collision_and_host_filter() {
        // 1. Shortened malformed stamp must not panic when slicing.
        assert_eq!(
            inspect_sync_stamp("# body\n<!-- tachi:instruction-sync v1 -->\n"),
            SyncStampState::TamperedOrEdited
        );

        // 2. Double-slash cannot bypass gemini exclusion of .gemini/config/.
        assert!(validate_relative_path(".gemini//config/AGENTS.md", "target").is_err());

        // 3. Case-insensitive duplicate targets are rejected.
        let root = temp_dir("r1-regressions");
        let home = root.join("home");
        let bundle = root.join("bundle");
        fs::create_dir_all(&home).unwrap();
        write_six_surface_bundle(&bundle, "2026-11-28.1");
        let mut manifest_json: serde_json::Value =
            serde_json::from_slice(&fs::read(bundle.join("bundle.json")).unwrap()).unwrap();
        manifest_json["targets"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!({
                "id": "claude-global-lower",
                "host": "claude",
                "source": "adapters/claude/CLAUDE.md",
                "target": ".claude/claude.md",
                "include_constitution_import": false,
                "include_machine_local": false,
                "max_bytes": 4096
            }));
        fs::write(
            bundle.join("bundle.json"),
            serde_json::to_string_pretty(&manifest_json).unwrap(),
        )
        .unwrap();
        let opts = SyncOptions {
            source_dir: Some(bundle.clone()),
            machine_local: None,
            hosts: vec![],
            home: Some(home.clone()),
            adopt: false,
        };
        let err = run_plan(&opts).expect_err("case-insensitive duplicate target must fail");
        assert!(err.contains("case-insensitive"));

        // 4. --host claude without existing constitution refuses broken @import.
        write_six_surface_bundle(&bundle, "2026-11-28.1");
        let claude_only = SyncOptions {
            source_dir: Some(bundle.clone()),
            machine_local: None,
            hosts: vec!["claude".to_string()],
            home: Some(home.clone()),
            adopt: false,
        };
        let err = run_plan(&claude_only).expect_err("missing constitution import must fail");
        assert!(err.contains("requires constitution"));

        // 5. Secret-like revision and Google API key prefix are rejected.
        let fake_rev = format!("{}{}", "ghp_", "RevSecret123456");
        write_six_surface_bundle(&bundle, &fake_rev);
        let err = run_plan(&opts).expect_err("secret in revision must fail");
        assert!(err.contains("secret pattern rejected"));
        assert!(!err.contains(&fake_rev));

        write_six_surface_bundle(&bundle, "2026-11-28.1");
        let fake_google = format!("{}{}", "AIza", "SyFakeKey0123456789");
        fs::write(
            bundle.join("adapters/antigravity/AGENTS.md"),
            format!("key={fake_google}\n"),
        )
        .unwrap();
        let err = run_plan(&opts).expect_err("AIza key must fail");
        assert!(err.contains("secret pattern rejected"));
        assert!(!err.contains(&fake_google));

        // 6. Explicit --machine-local under a symlinked ancestor directory fails closed.
        write_six_surface_bundle(&bundle, "2026-11-28.1");
        let real_overlay_dir = root.join("real-overlay");
        fs::create_dir_all(&real_overlay_dir).unwrap();
        fs::write(real_overlay_dir.join("machine.local.md"), "## Local\n").unwrap();
        #[cfg(unix)]
        {
            let link_overlay_dir = root.join("link-overlay");
            std::os::unix::fs::symlink(&real_overlay_dir, &link_overlay_dir).unwrap();
            let linked_opts = SyncOptions {
                source_dir: Some(bundle.clone()),
                machine_local: Some(link_overlay_dir.join("machine.local.md")),
                hosts: vec![],
                home: Some(home.clone()),
                adopt: false,
            };
            let err =
                run_plan(&linked_opts).expect_err("symlink ancestor on --machine-local must fail");
            assert!(err.contains("symlink ancestor"));
        }

        let _ = fs::remove_dir_all(root);
    }
}
