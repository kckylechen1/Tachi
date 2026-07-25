use super::alias::{
    plan_c_alias_db_for_root, plan_c_dir_name_from_root, plan_c_project_root_from_local_db,
};
use super::home::tachi_home;
use super::types::{
    active_memory_count, canonical_paths_equal, file_len, same_file_identity,
    PlanCAliasInspection, PlanCAliasIntegrity, PlanCLinkOutcome, PlanCSplitBrain,
};
use std::path::Path;

/// Global Plan C symlink for a repo-local project DB (Unix only).
#[cfg(unix)]
pub(crate) fn ensure_plan_c_symlink(local_db: &Path, project_root: &Path) -> PlanCLinkOutcome {
    ensure_plan_c_symlink_with_hook(local_db, project_root, |_| {})
}

#[cfg(unix)]
fn ensure_plan_c_symlink_with_hook(
    local_db: &Path,
    project_root: &Path,
    before_symlink: impl FnOnce(&Path),
) -> PlanCLinkOutcome {
    let projects_root = tachi_home().join("projects");
    if local_db.starts_with(&projects_root) {
        return PlanCLinkOutcome::Skipped("local db is already under the Plan C projects root");
    }
    let global_link = match inspect_plan_c_alias(local_db, project_root) {
        PlanCAliasInspection::MatchingSymlink => return PlanCLinkOutcome::AlreadyLinked,
        PlanCAliasInspection::SplitBrain(issue) => return PlanCLinkOutcome::SplitBrain(issue),
        PlanCAliasInspection::Integrity(issue) => {
            return PlanCLinkOutcome::AliasIntegrity(issue);
        }
        PlanCAliasInspection::Absent => match plan_c_alias_db_for_root(project_root) {
            Ok(path) => path,
            Err(error) => {
                return PlanCLinkOutcome::AliasIntegrity(PlanCAliasIntegrity::IdentityUnresolved {
                    project_root: project_root.to_path_buf(),
                    error,
                });
            }
        },
    };
    let Some(global_project_dir) = global_link.parent().map(Path::to_path_buf) else {
        return PlanCLinkOutcome::Skipped("project root has no directory name");
    };
    if let Err(error) = std::fs::create_dir_all(&global_project_dir) {
        return PlanCLinkOutcome::Failed {
            path: global_project_dir,
            error: format!("failed to create Plan C project directory: {error}"),
        };
    }
    before_symlink(&global_link);
    if let Err(error) = std::os::unix::fs::symlink(local_db, &global_link) {
        tracing::warn!(error = %error, path = %global_link.display(), "Failed to create Plan C symlink; re-inspecting alias state");
        return match inspect_plan_c_alias(local_db, project_root) {
            PlanCAliasInspection::MatchingSymlink => PlanCLinkOutcome::AlreadyLinked,
            PlanCAliasInspection::SplitBrain(issue) => PlanCLinkOutcome::SplitBrain(issue),
            PlanCAliasInspection::Integrity(issue) => PlanCLinkOutcome::AliasIntegrity(issue),
            PlanCAliasInspection::Absent => PlanCLinkOutcome::Failed {
                path: global_link,
                error: format!(
                    "Plan C alias symlink creation failed and re-inspection found no alias: {error}"
                ),
            },
        };
    }
    PlanCLinkOutcome::Created(global_link)
}

#[cfg(all(test, unix))]
pub(super) fn ensure_plan_c_symlink_with_test_hook(
    local_db: &Path,
    project_root: &Path,
    before_symlink: impl FnOnce(&Path),
) -> PlanCLinkOutcome {
    ensure_plan_c_symlink_with_hook(local_db, project_root, before_symlink)
}

#[cfg(not(unix))]
pub(crate) fn ensure_plan_c_symlink(_local_db: &Path, _project_root: &Path) -> PlanCLinkOutcome {
    PlanCLinkOutcome::Skipped("Plan C symlink unsupported on non-Unix hosts")
}

pub(crate) fn inspect_plan_c_alias_for_local_db(local_db: &Path) -> PlanCAliasInspection {
    let Some(project_root) = plan_c_project_root_from_local_db(local_db) else {
        return PlanCAliasInspection::Absent;
    };
    inspect_plan_c_alias(local_db, &project_root)
}

pub(crate) fn inspect_plan_c_alias(
    local_db: &Path,
    project_root: &Path,
) -> PlanCAliasInspection {
    let projects_root = tachi_home().join("projects");
    if local_db.starts_with(&projects_root) {
        return PlanCAliasInspection::Absent;
    }
    let alias_db = match plan_c_alias_db_for_root(project_root) {
        Ok(alias_db) => alias_db,
        Err(error) => {
            return PlanCAliasInspection::Integrity(PlanCAliasIntegrity::IdentityUnresolved {
                project_root: project_root.to_path_buf(),
                error,
            });
        }
    };
    let alias_meta = match std::fs::symlink_metadata(&alias_db) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return PlanCAliasInspection::Absent;
        }
        Err(error) => {
            return PlanCAliasInspection::Integrity(PlanCAliasIntegrity::SymlinkUnresolvable {
                alias_db,
                expected_db: local_db.to_path_buf(),
                error: error.to_string(),
            });
        }
    };
    if alias_meta.file_type().is_symlink() {
        let target = match std::fs::read_link(&alias_db) {
            Ok(target) => target,
            Err(error) => {
                return PlanCAliasInspection::Integrity(
                    PlanCAliasIntegrity::SymlinkUnresolvable {
                        alias_db,
                        expected_db: local_db.to_path_buf(),
                        error: error.to_string(),
                    },
                );
            }
        };
        if target == local_db || canonical_paths_equal(&alias_db, local_db) {
            return PlanCAliasInspection::MatchingSymlink;
        }
        let actual_db = match std::fs::canonicalize(&alias_db) {
            Ok(path) => path,
            Err(error) => {
                return PlanCAliasInspection::Integrity(
                    PlanCAliasIntegrity::SymlinkUnresolvable {
                        alias_db,
                        expected_db: local_db.to_path_buf(),
                        error: error.to_string(),
                    },
                );
            }
        };
        return PlanCAliasInspection::Integrity(PlanCAliasIntegrity::WrongTarget {
            alias_db,
            expected_db: local_db.to_path_buf(),
            actual_db,
        });
    }
    if alias_meta.file_type().is_file() && !same_file_identity(local_db, &alias_db) {
        let Some(project_name) = alias_db
            .parent()
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
            .map(str::to_string)
            .or_else(|| plan_c_dir_name_from_root(project_root))
        else {
            return PlanCAliasInspection::Integrity(PlanCAliasIntegrity::UnexpectedAliasType {
                alias_db,
                file_type: "regular file without a project identity",
            });
        };
        return PlanCAliasInspection::SplitBrain(PlanCSplitBrain {
            project_name,
            canonical_db: local_db.to_path_buf(),
            alias_db: alias_db.clone(),
            canonical_rows: active_memory_count(local_db),
            alias_rows: active_memory_count(&alias_db),
            canonical_bytes: file_len(local_db),
            alias_bytes: Some(alias_meta.len()),
        });
    }
    let file_type = if alias_meta.file_type().is_file() {
        "regular file"
    } else if alias_meta.file_type().is_dir() {
        "directory"
    } else {
        "non-file"
    };
    PlanCAliasInspection::Integrity(PlanCAliasIntegrity::UnexpectedAliasType {
        alias_db,
        file_type,
    })
}
