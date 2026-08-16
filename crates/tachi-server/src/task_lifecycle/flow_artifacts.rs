use super::*;
use serde_json::{json, Value};

// ─── Run-root resolution ─────────────────────────────────────────────────────

/// Resolve the runs root directory.
///
/// Order:
/// 1. `$TACHI_RUN_ROOT`
/// 2. `<repo_root>/.tachi/runs/` if a git repo is detected via `git rev-parse --show-toplevel`
/// 3. `$TACHI_HOME/runs/`
/// 4. `$HOME/.tachi/runs/`
/// 5. `<temp>/tachi/runs/`
pub(crate) fn flow_runs_root() -> PathBuf {
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
    Ok(flow_runs_root().join(flow_id))
}

/// Cross-flow closure-debt scan. Walks every flow run dir and surfaces:
///   - `unclosed_loop`: work produced a `result.md` but close_loop never ran
///     (issue/PR not written back, lesson not sunk, spec drift not flagged), and
///   - `spec_drift`: a flow that closed with an unresolved spec advisory
///     (docs referenced but no spec recorded — the canonical spec may be stale).
///
/// This is the session-start safety net for an agent's cross-session
/// forgetfulness: a per-flow briefing only sees the flow in scope, which is
/// exactly when a reminder is NOT needed. Output is capped at `limit`; if more
/// debt exists, a final summary item reports the overflow (never a silent cap).
pub(crate) fn scan_open_loops(limit: usize) -> Vec<Value> {
    let runs_root = flow_runs_root();
    let Ok(entries) = std::fs::read_dir(&runs_root) else {
        return Vec::new();
    };
    let mut debts = Vec::new();
    for entry in entries.flatten() {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        let flow_id = entry.file_name().to_string_lossy().to_string();
        let close_loop_path = dir.join("close_loop.json");
        let has_close_loop = close_loop_path.exists();
        if dir.join("result.md").exists() && !has_close_loop {
            let status = read_status_for_briefing(&dir);
            let Some(issue_ref) = status_string(&status, "issue_ref") else {
                continue;
            };
            let pr_ref = status_string(&status, "pr_ref");
            debts.push(json!({
                "kind": "unclosed_loop",
                "flow_id": flow_id,
                "detail": "Flow produced a result but close_loop has not run: issue/PR not written back, lesson not sunk to wiki, spec drift not flagged.",
                "action": close_loop_action_hint(&flow_id, &issue_ref, pr_ref.as_deref()),
                "authority": "closure_debt",
                "issue_ref": issue_ref,
                "pr_ref": pr_ref,
            }));
        } else if has_close_loop {
            let spec_unresolved = std::fs::read_to_string(&close_loop_path)
                .ok()
                .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
                .and_then(|v| {
                    v.get("closure_actions")
                        .and_then(|c| c.get("spec_advisory"))
                        .and_then(|s| s.get("status"))
                        .and_then(Value::as_str)
                        .map(|status| status == "advisory")
                })
                .unwrap_or(false);
            if spec_unresolved {
                debts.push(json!({
                    "kind": "spec_drift",
                    "flow_id": flow_id,
                    "detail": "Loop closed with docs referenced but no spec recorded — the canonical spec may be stale.",
                    "action": "Update the canonical spec, then re-run close_loop with spec_paths once corrected.",
                    "authority": "closure_debt",
                }));
            }
        }
    }
    debts.sort_by(|a, b| {
        let a_id = a.get("flow_id").and_then(Value::as_str).unwrap_or("");
        let b_id = b.get("flow_id").and_then(Value::as_str).unwrap_or("");
        a_id.cmp(b_id)
    });
    if debts.len() > limit {
        let overflow = debts.len() - limit;
        debts.truncate(limit);
        debts.push(json!({
            "kind": "more",
            "detail": format!("{overflow} more closure-debt item(s) not shown (showing {limit})."),
        }));
    }
    debts
}

fn status_string(status: &Value, key: &str) -> Option<String> {
    status
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn close_loop_action_hint(flow_id: &str, issue_ref: &str, pr_ref: Option<&str>) -> String {
    let mut hint = format!(
        "tachi_gh(action='close_loop', flow_id='{}', issue_ref='{}'",
        pseudo_call_quote(flow_id),
        pseudo_call_quote(issue_ref)
    );
    if let Some(pr_ref) = pr_ref {
        hint.push_str(&format!(", pr_ref='{}'", pseudo_call_quote(pr_ref)));
    }
    hint.push(')');
    hint
}

fn pseudo_call_quote(value: &str) -> String {
    value.replace('\\', "\\\\").replace('\'', "\\'")
}

fn read_status_for_briefing(run_dir: &std::path::Path) -> Value {
    let status_path = run_dir.join("status.json");
    match std::fs::read_to_string(&status_path) {
        Ok(s) => serde_json::from_str(&s).unwrap_or_else(|err| {
            tracing::warn!(
                path = %status_path.display(),
                error = %err,
                "flow status JSON parse failed; continuing with empty status"
            );
            json!({})
        }),
        Err(err) => {
            if err.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!(
                    path = %status_path.display(),
                    error = %err,
                    "flow status read failed; continuing with empty status"
                );
            }
            json!({})
        }
    }
}

mod close_loop;
mod dispatch_markers;
mod github;
mod refs;

pub(crate) use close_loop::mark_task_close_loop;
pub(crate) use dispatch_markers::{
    mark_task_dispatch, mark_task_dispatch_completion, verified_dispatch_marker_revision,
};
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
