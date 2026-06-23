use super::alias::{
    plan_c_alias_db_for_root, plan_c_dir_name_from_root, plan_c_project_root_from_local_db,
};
use super::home::tachi_home;
use super::types::{
    active_memory_count, canonical_paths_equal, file_len, same_file_identity, PlanCLinkOutcome,
    PlanCSplitBrain,
};
use std::path::Path;

/// Global Plan C symlink for a repo-local project DB (Unix only).
#[cfg(unix)]
pub(crate) fn ensure_plan_c_symlink(local_db: &Path, project_root: &Path) -> PlanCLinkOutcome {
    let projects_root = tachi_home().join("projects");
    if local_db.starts_with(&projects_root) {
        return PlanCLinkOutcome::Skipped("local db is already under the Plan C projects root");
    }
    // Prefer the hashed alias dir, but keep using a pre-existing legacy un-hashed
    // dir so repos created before the hash suffix are not orphaned.
    let Some(global_link) = plan_c_alias_db_for_root(project_root) else {
        return PlanCLinkOutcome::Skipped("project root has no directory name");
    };
    let Some(global_project_dir) = global_link.parent().map(Path::to_path_buf) else {
        return PlanCLinkOutcome::Skipped("project root has no directory name");
    };
    if std::fs::create_dir_all(&global_project_dir).is_err() {
        return PlanCLinkOutcome::Skipped("failed to create Plan C project directory");
    }
    let link_is_correct = global_link.is_symlink()
        && std::fs::read_link(&global_link)
            .is_ok_and(|target| target == local_db || canonical_paths_equal(&target, local_db));
    if link_is_correct {
        return PlanCLinkOutcome::AlreadyLinked;
    }
    if global_link.is_symlink() {
        let _ = std::fs::remove_file(&global_link);
    } else if global_link.exists() {
        if let Some(split_brain) = plan_c_split_brain(local_db, project_root) {
            tracing::warn!(
                project = %split_brain.project_name,
                canonical_db = %split_brain.canonical_db.display(),
                alias_db = %split_brain.alias_db.display(),
                canonical_rows = ?split_brain.canonical_rows,
                alias_rows = ?split_brain.alias_rows,
                "Plan C global alias exists as a regular file and diverges from repo-local DB"
            );
            return PlanCLinkOutcome::SplitBrain(split_brain);
        }
        tracing::warn!(
            path = %global_link.display(),
            "Plan C global link exists as a regular file; skipping symlink"
        );
        return PlanCLinkOutcome::Skipped("Plan C global link exists as a regular file");
    }
    if let Err(e) = std::os::unix::fs::symlink(local_db, &global_link) {
        tracing::warn!(error = %e, path = %global_link.display(), "Failed to create Plan C symlink");
        return PlanCLinkOutcome::Failed {
            path: global_link,
            error: e.to_string(),
        };
    }
    PlanCLinkOutcome::Created(global_link)
}

#[cfg(not(unix))]
pub(crate) fn ensure_plan_c_symlink(_local_db: &Path, _project_root: &Path) -> PlanCLinkOutcome {
    PlanCLinkOutcome::Skipped("Plan C symlink unsupported on non-Unix hosts")
}

pub(crate) fn plan_c_split_brain_for_local_db(local_db: &Path) -> Option<PlanCSplitBrain> {
    let project_root = plan_c_project_root_from_local_db(local_db)?;
    plan_c_split_brain(local_db, &project_root)
}

pub(crate) fn plan_c_split_brain(local_db: &Path, project_root: &Path) -> Option<PlanCSplitBrain> {
    let projects_root = tachi_home().join("projects");
    if local_db.starts_with(&projects_root) {
        return None;
    }
    // Inspect the alias that would actually be used (hashed, or a pre-existing
    // legacy un-hashed dir) so split-brain detection matches creation behavior.
    let alias_db = plan_c_alias_db_for_root(project_root)?;
    // Report the name of the alias dir we actually inspected so the warning's
    // project name matches the on-disk alias (legacy vs hashed).
    let project_name = alias_db
        .parent()
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str())
        .map(str::to_string)
        .or_else(|| plan_c_dir_name_from_root(project_root))?;
    let alias_meta = std::fs::symlink_metadata(&alias_db).ok()?;
    if !alias_meta.file_type().is_file() {
        return None;
    }
    if same_file_identity(local_db, &alias_db) {
        return None;
    }
    Some(PlanCSplitBrain {
        project_name,
        canonical_db: local_db.to_path_buf(),
        alias_db: alias_db.clone(),
        canonical_rows: active_memory_count(local_db),
        alias_rows: active_memory_count(&alias_db),
        canonical_bytes: file_len(local_db),
        alias_bytes: Some(alias_meta.len()),
    })
}
