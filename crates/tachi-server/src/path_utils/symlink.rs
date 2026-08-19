use super::alias::{
    plan_c_alias_db_for_root_in_home, plan_c_dir_name_from_root, plan_c_project_root_from_local_db,
};
use super::home::tachi_home;
use super::types::{
    active_memory_count, canonical_paths_equal, file_len, same_file_identity, PlanCAliasInspection,
    PlanCAliasIntegrity, PlanCLinkOutcome, PlanCSplitBrain,
};
use std::path::Path;

/// Global Plan C symlink for a repo-local project DB (Unix only).
#[cfg(unix)]
pub(crate) fn ensure_plan_c_symlink(local_db: &Path, project_root: &Path) -> PlanCLinkOutcome {
    ensure_plan_c_symlink_in_home(local_db, project_root, &tachi_home())
}

#[cfg(unix)]
pub(crate) fn ensure_plan_c_symlink_in_home(
    local_db: &Path,
    project_root: &Path,
    tachi_home: &Path,
) -> PlanCLinkOutcome {
    ensure_plan_c_symlink_with_hook(local_db, project_root, tachi_home, |_path| {
        #[cfg(test)]
        run_plan_c_symlink_hook_for_test(_path)?;
        Ok(())
    })
}

#[cfg(unix)]
fn ensure_plan_c_symlink_with_hook(
    local_db: &Path,
    project_root: &Path,
    tachi_home: &Path,
    before_symlink: impl FnOnce(&Path) -> std::io::Result<()>,
) -> PlanCLinkOutcome {
    let projects_root = tachi_home.join("projects");
    if local_db.starts_with(&projects_root) {
        return PlanCLinkOutcome::Skipped("local db is already under the Plan C projects root");
    }
    let global_link = match inspect_plan_c_alias_in_home(local_db, project_root, tachi_home) {
        PlanCAliasInspection::MatchingSymlink => return PlanCLinkOutcome::AlreadyLinked,
        PlanCAliasInspection::SplitBrain(issue) => return PlanCLinkOutcome::SplitBrain(issue),
        PlanCAliasInspection::Integrity(issue) => {
            return PlanCLinkOutcome::AliasIntegrity(issue);
        }
        PlanCAliasInspection::Absent => {
            match plan_c_alias_db_for_root_in_home(project_root, tachi_home) {
                Ok(path) => path,
                Err(error) => {
                    return PlanCLinkOutcome::AliasIntegrity(
                        PlanCAliasIntegrity::IdentityUnresolved {
                            project_root: project_root.to_path_buf(),
                            error,
                        },
                    );
                }
            }
        }
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
    let symlink_result = before_symlink(&global_link)
        .and_then(|()| std::os::unix::fs::symlink(local_db, &global_link));
    if let Err(error) = symlink_result {
        tracing::warn!(error = %error, path = %global_link.display(), "Failed to create Plan C symlink; re-inspecting alias state");
        return match inspect_plan_c_alias_in_home(local_db, project_root, tachi_home) {
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
    ensure_plan_c_symlink_with_hook(local_db, project_root, &tachi_home(), |path| {
        before_symlink(path);
        Ok(())
    })
}

#[cfg(all(test, unix))]
type PlanCSymlinkHook = Box<dyn FnOnce(&Path) -> std::io::Result<()>>;

#[cfg(all(test, unix))]
thread_local! {
    static PLAN_C_SYMLINK_HOOK: std::cell::RefCell<Option<PlanCSymlinkHook>> =
        std::cell::RefCell::new(None);
}

#[cfg(all(test, unix))]
fn run_plan_c_symlink_hook_for_test(path: &Path) -> std::io::Result<()> {
    let hook = PLAN_C_SYMLINK_HOOK.with(|slot| slot.borrow_mut().take());
    match hook {
        Some(hook) => hook(path),
        None => Ok(()),
    }
}

#[cfg(all(test, unix))]
pub(crate) struct PlanCSymlinkHookGuard;

#[cfg(all(test, unix))]
impl Drop for PlanCSymlinkHookGuard {
    fn drop(&mut self) {
        PLAN_C_SYMLINK_HOOK.with(|slot| {
            slot.borrow_mut().take();
        });
    }
}

#[cfg(all(test, unix))]
pub(crate) fn install_plan_c_symlink_hook_for_test(
    hook: impl FnOnce(&Path) -> std::io::Result<()> + 'static,
) -> PlanCSymlinkHookGuard {
    PLAN_C_SYMLINK_HOOK.with(|slot| {
        let previous = slot.borrow_mut().replace(Box::new(hook));
        assert!(
            previous.is_none(),
            "Plan C symlink test hook already installed"
        );
    });
    PlanCSymlinkHookGuard
}

#[cfg(not(unix))]
pub(crate) fn ensure_plan_c_symlink(_local_db: &Path, _project_root: &Path) -> PlanCLinkOutcome {
    PlanCLinkOutcome::Skipped("Plan C symlink unsupported on non-Unix hosts")
}

#[cfg(not(unix))]
pub(crate) fn ensure_plan_c_symlink_in_home(
    _local_db: &Path,
    _project_root: &Path,
    _tachi_home: &Path,
) -> PlanCLinkOutcome {
    PlanCLinkOutcome::Skipped("Plan C symlink unsupported on non-Unix hosts")
}

/// Ensure the canonical (gen-4) alias directory and symlink exist for `project_root`
/// in `tachi_home`, ensuring convergence between on-disk aliases and manifest records (#1573).
#[cfg(unix)]
pub(crate) fn ensure_plan_c_canonical_alias_in_home(
    local_db: &Path,
    project_root: &Path,
    tachi_home: &Path,
) -> PlanCLinkOutcome {
    let canonical_name = match plan_c_dir_name_from_root(project_root) {
        Some(name) => name,
        None => return PlanCLinkOutcome::Skipped("cannot derive canonical name for project root"),
    };
    let canonical_alias_dir = tachi_home.join("projects").join(&canonical_name);
    let canonical_link = canonical_alias_dir.join(memcore::MEMORY_DB_FILENAME);

    if let Ok(meta) = std::fs::symlink_metadata(&canonical_link) {
        if meta.file_type().is_symlink() {
            if let Ok(target) = std::fs::read_link(&canonical_link) {
                if target == local_db || canonical_paths_equal(&canonical_link, local_db) {
                    return PlanCLinkOutcome::AlreadyLinked;
                }
                let actual_db = match std::fs::canonicalize(&canonical_link) {
                    Ok(path) => path,
                    Err(error) => {
                        return PlanCLinkOutcome::AliasIntegrity(
                            PlanCAliasIntegrity::SymlinkUnresolvable {
                                alias_db: canonical_link,
                                expected_db: local_db.to_path_buf(),
                                error: error.to_string(),
                            },
                        );
                    }
                };
                return PlanCLinkOutcome::AliasIntegrity(PlanCAliasIntegrity::WrongTarget {
                    alias_db: canonical_link,
                    expected_db: local_db.to_path_buf(),
                    actual_db,
                });
            }
        }
        if meta.file_type().is_file() && !same_file_identity(local_db, &canonical_link) {
            return PlanCLinkOutcome::SplitBrain(PlanCSplitBrain {
                project_name: canonical_name,
                canonical_db: local_db.to_path_buf(),
                alias_db: canonical_link.clone(),
                canonical_rows: active_memory_count(local_db),
                alias_rows: active_memory_count(&canonical_link),
                canonical_bytes: file_len(local_db),
                alias_bytes: file_len(&canonical_link),
            });
        }
        return PlanCLinkOutcome::AliasIntegrity(PlanCAliasIntegrity::UnexpectedAliasType {
            alias_db: canonical_link,
            file_type: "non-symlink alias",
        });
    }

    if let Err(error) = std::fs::create_dir_all(&canonical_alias_dir) {
        return PlanCLinkOutcome::Failed {
            path: canonical_alias_dir,
            error: format!("failed to create canonical Plan C project directory: {error}"),
        };
    }
    if let Err(error) = std::os::unix::fs::symlink(local_db, &canonical_link) {
        if error.kind() != std::io::ErrorKind::AlreadyExists {
            return PlanCLinkOutcome::Failed {
                path: canonical_link,
                error: format!("failed to create canonical Plan C symlink: {error}"),
            };
        }
        if canonical_paths_equal(&canonical_link, local_db) {
            return PlanCLinkOutcome::AlreadyLinked;
        }
        return PlanCLinkOutcome::Failed {
            path: canonical_link,
            error: format!(
                "canonical Plan C symlink already exists with different target: {error}"
            ),
        };
    }
    PlanCLinkOutcome::Created(canonical_link)
}

#[cfg(not(unix))]
pub(crate) fn ensure_plan_c_canonical_alias_in_home(
    _local_db: &Path,
    _project_root: &Path,
    _tachi_home: &Path,
) -> PlanCLinkOutcome {
    PlanCLinkOutcome::Skipped("Plan C canonical symlink unsupported on non-Unix hosts")
}

pub(crate) fn inspect_plan_c_alias_for_local_db(local_db: &Path) -> PlanCAliasInspection {
    inspect_plan_c_alias_for_local_db_in_home(local_db, &tachi_home())
}

pub(crate) fn inspect_plan_c_alias_for_local_db_in_home(
    local_db: &Path,
    tachi_home: &Path,
) -> PlanCAliasInspection {
    let Some(project_root) = plan_c_project_root_from_local_db(local_db) else {
        return PlanCAliasInspection::Absent;
    };
    inspect_plan_c_alias_in_home(local_db, &project_root, tachi_home)
}

pub(crate) fn inspect_plan_c_alias_in_home(
    local_db: &Path,
    project_root: &Path,
    tachi_home: &Path,
) -> PlanCAliasInspection {
    let projects_root = tachi_home.join("projects");
    if local_db.starts_with(&projects_root) {
        return PlanCAliasInspection::Absent;
    }
    let alias_db = match plan_c_alias_db_for_root_in_home(project_root, tachi_home) {
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
                return PlanCAliasInspection::Integrity(PlanCAliasIntegrity::SymlinkUnresolvable {
                    alias_db,
                    expected_db: local_db.to_path_buf(),
                    error: error.to_string(),
                });
            }
        };
        if target == local_db || canonical_paths_equal(&alias_db, local_db) {
            return PlanCAliasInspection::MatchingSymlink;
        }
        let actual_db = match std::fs::canonicalize(&alias_db) {
            Ok(path) => path,
            Err(error) => {
                return PlanCAliasInspection::Integrity(PlanCAliasIntegrity::SymlinkUnresolvable {
                    alias_db,
                    expected_db: local_db.to_path_buf(),
                    error: error.to_string(),
                });
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
