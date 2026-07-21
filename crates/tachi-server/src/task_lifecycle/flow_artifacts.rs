use super::*;

// ─── Run-root resolution ─────────────────────────────────────────────────────

/// Resolve the runs root directory.
///
/// Order:
/// 1. `$TACHI_RUN_ROOT`
/// 2. `<repo_root>/.tachi/runs/` if a git repo is detected via `git rev-parse --show-toplevel`
/// 3. `$TACHI_HOME/runs/`
/// 4. `$HOME/.tachi/runs/`
/// 5. `<temp>/tachi/runs/`
pub(crate) fn shell_runs_root() -> PathBuf {
    resolve_runs_root_from(
        || std::env::var("TACHI_RUN_ROOT").ok().map(PathBuf::from),
        || crate::path_utils::cached_git_root().cloned(),
        || std::env::var("TACHI_HOME").ok().map(PathBuf::from),
        || std::env::var("HOME").ok().map(PathBuf::from),
        || std::env::temp_dir(),
    )
}

fn resolve_runs_root_from(
    run_root: impl FnOnce() -> Option<PathBuf>,
    git_root: impl FnOnce() -> Option<PathBuf>,
    tachi_home: impl FnOnce() -> Option<PathBuf>,
    home: impl FnOnce() -> Option<PathBuf>,
    temp: impl FnOnce() -> PathBuf,
) -> PathBuf {
    run_root()
        .or_else(|| git_root().map(|root| root.join(".tachi").join("runs")))
        .or_else(|| tachi_home().map(|root| root.join("runs")))
        .or_else(|| home().map(|root| root.join(".tachi").join("runs")))
        .unwrap_or_else(|| temp().join("tachi").join("runs"))
}

pub(crate) fn validate_flow_id(id: &str) -> Result<(), String> {
    if !id.starts_with("flow_")
        || id.contains('/')
        || id.contains('\\')
        || id.contains("..")
        || !id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Err(format!(
            "Invalid flow_id: '{}'. Expected a safe id starting with 'flow_' and containing only ASCII letters, numbers, '_' or '-'. Example: flow_20260609T014037Z_tachi_dispatch_ux_smoke",
            id
        ));
    }
    Ok(())
}

pub(crate) fn run_dir_for_flow_id(flow_id: &str) -> Result<PathBuf, String> {
    validate_flow_id(flow_id)?;
    Ok(shell_runs_root().join(flow_id))
}

mod close_loop;
mod dispatch_markers;
mod github;
mod refs;

pub(crate) use close_loop::mark_task_close_loop;
pub(crate) use dispatch_markers::{mark_task_dispatch, mark_task_dispatch_completion};
pub(crate) use github::{write_intake_flow_artifacts, write_link_pr_artifacts};
pub(crate) use refs::{flow_status_doc_refs, resolve_link_pr_issue_ref};

pub(in crate::task_lifecycle) use github::intake_briefing_params;
#[cfg(test)]
pub(in crate::task_lifecycle) use refs::{initial_merge_state_for_pr, normalize_issue_ref};

#[cfg(test)]
mod tests {
    use super::{resolve_runs_root_from, validate_flow_id};
    use std::path::PathBuf;

    #[test]
    fn runs_root_sources_keep_frozen_precedence_and_paths() {
        let selected = resolve_runs_root_from(
            || Some(PathBuf::from("explicit")),
            || panic!("Git root must stay lazy after an explicit override"),
            || panic!("TACHI_HOME must stay lazy after an explicit override"),
            || panic!("HOME must stay lazy after an explicit override"),
            || panic!("temp must stay lazy after an explicit override"),
        );
        assert_eq!(selected, PathBuf::from("explicit"));

        let selected = resolve_runs_root_from(
            || None,
            || Some(PathBuf::from("repo")),
            || panic!("TACHI_HOME must stay lazy after a Git-root match"),
            || panic!("HOME must stay lazy after a Git-root match"),
            || panic!("temp must stay lazy after a Git-root match"),
        );
        assert_eq!(selected, PathBuf::from("repo/.tachi/runs"));

        let selected = resolve_runs_root_from(
            || None,
            || None,
            || Some(PathBuf::from("tachi-home")),
            || panic!("HOME must stay lazy after a TACHI_HOME match"),
            || panic!("temp must stay lazy after a TACHI_HOME match"),
        );
        assert_eq!(selected, PathBuf::from("tachi-home/runs"));

        let selected = resolve_runs_root_from(
            || None,
            || None,
            || None,
            || Some(PathBuf::from("home")),
            || panic!("temp must stay lazy after a HOME match"),
        );
        assert_eq!(selected, PathBuf::from("home/.tachi/runs"));

        let selected =
            resolve_runs_root_from(|| None, || None, || None, || None, || PathBuf::from("temp"));
        assert_eq!(selected, PathBuf::from("temp/tachi/runs"));
    }

    #[test]
    fn flow_id_rejects_path_traversal() {
        for invalid in [
            "../../etc",
            "flow_../../etc",
            "/tmp/evil",
            "flow_/tmp/evil",
            "flow_..",
            "flow_bad/name",
            "flow_bad\\name",
            "flow_bad name",
            "flow_bad.name",
            "flow_坏",
            "notflow_20260505",
        ] {
            assert!(
                validate_flow_id(invalid).is_err(),
                "expected invalid flow_id to be rejected: {invalid}"
            );
        }
        assert!(validate_flow_id("flow_20260505T000000Z_demo-1").is_ok());
        assert_eq!(
            validate_flow_id("flow_坏").unwrap_err(),
            "Invalid flow_id: 'flow_坏'. Expected a safe id starting with 'flow_' and containing only ASCII letters, numbers, '_' or '-'. Example: flow_20260609T014037Z_tachi_dispatch_ux_smoke"
        );
    }
}
