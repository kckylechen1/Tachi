//! #878-A — machine-checkable completion predicate.
//!
//! The OS exit code is the only hard signal a dispatch produces, but it is not
//! sufficient: a vendor can exit 0 (or a leader can self-report
//! `outcome="success"`) while the actual contract deliverable is missing. That
//! is a FALSE SUCCESS, and today it lands `TASK_STATE_COMPLETED` + `reviewed=true`
//! by fiat. This module provides a single machine-checkable source of truth,
//! evaluated at every site that is about to write COMPLETED:
//!
//!   * Pass       — the declared predicate is satisfied → earned COMPLETED.
//!   * Fail       — self-report/exit0 claims success but the predicate is not
//!                  satisfied → intercept as FALSE SUCCESS, route to FAILED.
//!   * Unverified — no predicate declared → COMPLETED but `reviewed=false`
//!                  (the watchdog's conservative "no one objected" posture).
//!
//! Both entry points are pure so the full outcome×verdict matrix is unit
//! testable without a server (see `#[cfg(test)]` below).

use std::path::{Component, Path, PathBuf};

use crate::tool_params::CompletionPredicate;
use serde_json::Value;

/// Verdict of evaluating a (possibly absent) completion predicate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PredicateVerdict {
    /// The declared predicate is satisfied.
    Pass,
    /// The declared predicate is NOT satisfied; the string is a human reason.
    Fail(String),
    /// No predicate was declared, so success could not be machine-verified.
    Unverified,
}

impl PredicateVerdict {
    /// Short machine tag for status/eval metadata.
    pub(crate) fn tag(&self) -> &'static str {
        match self {
            PredicateVerdict::Pass => "pass",
            PredicateVerdict::Fail(_) => "fail",
            PredicateVerdict::Unverified => "unverified",
        }
    }
}

/// Reject absolute paths and any `..`/root/prefix component so an artifact
/// predicate can only ever probe *inside* the dispatch cwd. Mirrors the
/// defensive posture of the trusted-command path checks elsewhere in dispatch.
fn is_safe_relative_path(path: &str) -> bool {
    if path.trim().is_empty() {
        return false;
    }
    let p = Path::new(path);
    if p.is_absolute() {
        return false;
    }
    p.components()
        .all(|c| matches!(c, Component::Normal(_) | Component::CurDir))
}

/// Evaluate a (possibly absent) completion predicate.
///
/// * `pred`    — the declared predicate, or `None` (→ [`PredicateVerdict::Unverified`]).
/// * `run_dir` — the dispatch run directory (unused for the artifact form; kept
///               for symmetry / future forms that read run artifacts directly).
/// * `cwd`     — the dispatch working directory an `ArtifactNonEmpty` path is
///               resolved against; falls back to the process cwd when `None`.
/// * `output`  — the run's `result.md` contents, matched by `OutputMatches`.
pub(crate) fn evaluate_completion_predicate(
    pred: Option<&CompletionPredicate>,
    _run_dir: &Path,
    cwd: Option<&Path>,
    output: &str,
) -> PredicateVerdict {
    let Some(pred) = pred else {
        return PredicateVerdict::Unverified;
    };
    match pred {
        CompletionPredicate::ArtifactNonEmpty { path } => {
            if !is_safe_relative_path(path) {
                return PredicateVerdict::Fail(format!(
                    "unsafe artifact predicate path '{path}': must be a relative path without '..'"
                ));
            }
            let base: PathBuf = match cwd {
                Some(dir) => dir.to_path_buf(),
                None => std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            };
            let full = base.join(path);
            match std::fs::metadata(&full) {
                Ok(meta) if meta.is_file() && meta.len() > 0 => PredicateVerdict::Pass,
                Ok(meta) if meta.is_file() => PredicateVerdict::Fail(format!(
                    "expected artifact '{path}' exists but is empty"
                )),
                Ok(_) => PredicateVerdict::Fail(format!(
                    "expected artifact '{path}' is not a regular file"
                )),
                Err(_) => PredicateVerdict::Fail(format!("expected artifact '{path}' is missing")),
            }
        }
        CompletionPredicate::OutputMatches { pattern } => match regex::Regex::new(pattern) {
            Ok(re) => {
                if re.is_match(output) {
                    PredicateVerdict::Pass
                } else {
                    PredicateVerdict::Fail(format!(
                        "run output did not match required pattern '{pattern}'"
                    ))
                }
            }
            Err(err) => {
                PredicateVerdict::Fail(format!("invalid predicate regex '{pattern}': {err}"))
            }
        },
    }
}

/// Load completion-predicate context from the dispatch run ledger (`status.json`).
///
/// Returns `(run_dir, declared_predicate, cwd)`.
pub(crate) fn resolve_completion_predicate_context(
    dispatch_id: &str,
) -> (
    Option<PathBuf>,
    Option<CompletionPredicate>,
    Option<PathBuf>,
) {
    let run_dir = crate::path_utils::tachi_home()
        .join("runs")
        .join(dispatch_id);
    if !run_dir.is_dir() {
        return (None, None, None);
    }
    let status_path = run_dir.join("status.json");
    let status = match crate::task_lifecycle::read_json_file(&status_path) {
        Ok(Some(v)) => v,
        _ => {
            return (Some(run_dir), None, None);
        }
    };
    let pred = status
        .get("completion_predicate")
        .cloned()
        .filter(|v| !v.is_null())
        .and_then(|v| serde_json::from_value::<CompletionPredicate>(v).ok());
    let cwd = status
        .get("cwd")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(PathBuf::from);
    (Some(run_dir), pred, cwd)
}

/// Pure kanban state mapping for a completion, folding the predicate verdict in.
///
/// Returns `(kanban_state, reviewed, override_reason)`.
///
/// The predicate only re-decides the `success` outcome — the sole self-report
/// that can *falsely* claim success. `failure`/`partial`/`aborted` keep their
/// existing mapping and stay `reviewed=true` (an explicit `tachi_complete` is a
/// deliberate, reviewed close). `override_reason` is `Some` only when the
/// predicate intercepts a false success.
pub(crate) fn resolve_completion_state(
    outcome: &str,
    verdict: &PredicateVerdict,
) -> (&'static str, bool, Option<String>) {
    match outcome {
        "success" => match verdict {
            PredicateVerdict::Pass => ("TASK_STATE_COMPLETED", true, None),
            PredicateVerdict::Fail(reason) => (
                "TASK_STATE_FAILED",
                false,
                Some(format!(
                    "self-reported success but predicate unsatisfied: {reason}"
                )),
            ),
            // No predicate declared: land COMPLETED, but not reviewed — success
            // could not be machine-verified, so it must not earn reviewed=true.
            PredicateVerdict::Unverified => ("TASK_STATE_COMPLETED", false, None),
        },
        "failure" => ("TASK_STATE_FAILED", true, None),
        "partial" => ("TASK_STATE_INPUT_REQUIRED", true, None),
        "aborted" => ("TASK_STATE_CANCELED", true, None),
        _ => ("TASK_STATE_FAILED", true, None),
    }
}

/// Map a resolved terminal kanban state to the machine `execution_outcome`
/// value recorded on the `dispatch_outcomes` row (#773 Layer-2 ②). This is the
/// MACHINE verdict (post-predicate / terminal-path), sharing the
/// `normalize_dispatch_outcome` vocabulary so every outcome-row column speaks
/// one dialect. Non-terminal states fall through to `"unknown"` — the caller
/// only records outcomes for terminal transitions.
pub(crate) fn execution_outcome_for_kanban_state(state: &str) -> &'static str {
    match state {
        "TASK_STATE_COMPLETED" => "completed",
        "TASK_STATE_FAILED" => "failed",
        "TASK_STATE_CANCELED" => "aborted",
        "TASK_STATE_INPUT_REQUIRED" => "partial",
        _ => "unknown",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn td() -> tempfile::TempDir {
        tempfile::tempdir().expect("temp dir")
    }

    #[test]
    fn execution_outcome_maps_each_terminal_state() {
        // #773 ②: a false-success interception resolves to FAILED, which MUST
        // map to the machine `execution_outcome` value 'failed' recorded on the
        // outcome row — the kill-test-5 invariant. The other terminals map to
        // the shared normalize_dispatch_outcome vocabulary.
        assert_eq!(
            execution_outcome_for_kanban_state("TASK_STATE_COMPLETED"),
            "completed"
        );
        assert_eq!(
            execution_outcome_for_kanban_state("TASK_STATE_FAILED"),
            "failed"
        );
        assert_eq!(
            execution_outcome_for_kanban_state("TASK_STATE_CANCELED"),
            "aborted"
        );
        assert_eq!(
            execution_outcome_for_kanban_state("TASK_STATE_INPUT_REQUIRED"),
            "partial"
        );
        assert_eq!(
            execution_outcome_for_kanban_state("TASK_STATE_WORKING"),
            "unknown"
        );
    }

    #[test]
    fn false_success_resolves_to_failed_execution_outcome() {
        // End-to-end of the ② machine verdict: reported success + predicate
        // Fail → resolved kanban FAILED → execution_outcome 'failed'.
        let (state, _reviewed, override_reason) =
            resolve_completion_state("success", &PredicateVerdict::Fail("missing".into()));
        assert_eq!(state, "TASK_STATE_FAILED");
        assert!(override_reason.is_some(), "false success must be flagged");
        assert_eq!(execution_outcome_for_kanban_state(state), "failed");

        // Passing predicate keeps the honest success as 'completed'.
        let (ok_state, _, ok_reason) = resolve_completion_state("success", &PredicateVerdict::Pass);
        assert_eq!(ok_state, "TASK_STATE_COMPLETED");
        assert!(ok_reason.is_none());
        assert_eq!(execution_outcome_for_kanban_state(ok_state), "completed");
    }

    #[test]
    fn artifact_non_empty_passes_for_non_empty_file() {
        let dir = td();
        std::fs::create_dir_all(dir.path().join("out")).unwrap();
        std::fs::write(dir.path().join("out/report.md"), "hello").unwrap();
        let pred = CompletionPredicate::ArtifactNonEmpty {
            path: "out/report.md".to_string(),
        };
        assert_eq!(
            evaluate_completion_predicate(Some(&pred), dir.path(), Some(dir.path()), ""),
            PredicateVerdict::Pass
        );
    }

    #[test]
    fn artifact_non_empty_fails_when_missing_or_empty() {
        let dir = td();
        let pred = CompletionPredicate::ArtifactNonEmpty {
            path: "out/report.md".to_string(),
        };
        // Missing.
        assert!(matches!(
            evaluate_completion_predicate(Some(&pred), dir.path(), Some(dir.path()), ""),
            PredicateVerdict::Fail(_)
        ));
        // Empty.
        std::fs::create_dir_all(dir.path().join("out")).unwrap();
        std::fs::write(dir.path().join("out/report.md"), "").unwrap();
        assert!(matches!(
            evaluate_completion_predicate(Some(&pred), dir.path(), Some(dir.path()), ""),
            PredicateVerdict::Fail(_)
        ));
    }

    #[test]
    fn artifact_predicate_rejects_path_traversal() {
        let dir = td();
        for bad in ["../secret", "/etc/passwd", "out/../../escape"] {
            let pred = CompletionPredicate::ArtifactNonEmpty {
                path: bad.to_string(),
            };
            assert!(
                matches!(
                    evaluate_completion_predicate(Some(&pred), dir.path(), Some(dir.path()), ""),
                    PredicateVerdict::Fail(_)
                ),
                "path '{bad}' must be rejected"
            );
        }
    }

    #[test]
    fn output_matches_pass_fail_and_bad_regex() {
        let dir = td();
        let pred = CompletionPredicate::OutputMatches {
            pattern: r"ALL TESTS PASSED".to_string(),
        };
        assert_eq!(
            evaluate_completion_predicate(Some(&pred), dir.path(), None, "...\nALL TESTS PASSED\n"),
            PredicateVerdict::Pass
        );
        assert!(matches!(
            evaluate_completion_predicate(Some(&pred), dir.path(), None, "nope"),
            PredicateVerdict::Fail(_)
        ));
        let bad = CompletionPredicate::OutputMatches {
            pattern: r"(".to_string(),
        };
        assert!(matches!(
            evaluate_completion_predicate(Some(&bad), dir.path(), None, "anything"),
            PredicateVerdict::Fail(_)
        ));
    }

    #[test]
    fn none_predicate_is_unverified() {
        let dir = td();
        assert_eq!(
            evaluate_completion_predicate(None, dir.path(), None, ""),
            PredicateVerdict::Unverified
        );
    }

    #[test]
    fn resolve_state_matrix_success_row() {
        assert_eq!(
            resolve_completion_state("success", &PredicateVerdict::Pass),
            ("TASK_STATE_COMPLETED", true, None)
        );
        let (state, reviewed, reason) =
            resolve_completion_state("success", &PredicateVerdict::Fail("x".into()));
        assert_eq!(state, "TASK_STATE_FAILED");
        assert!(!reviewed);
        assert!(reason.expect("reason").contains("predicate unsatisfied"));
        assert_eq!(
            resolve_completion_state("success", &PredicateVerdict::Unverified),
            ("TASK_STATE_COMPLETED", false, None)
        );
    }

    #[test]
    fn resolve_state_matrix_non_success_ignores_predicate() {
        // failure/partial/aborted keep their mapping and stay reviewed for every
        // verdict — the predicate only re-decides self-reported success.
        for verdict in [
            PredicateVerdict::Pass,
            PredicateVerdict::Fail("x".into()),
            PredicateVerdict::Unverified,
        ] {
            assert_eq!(
                resolve_completion_state("failure", &verdict),
                ("TASK_STATE_FAILED", true, None)
            );
            assert_eq!(
                resolve_completion_state("partial", &verdict),
                ("TASK_STATE_INPUT_REQUIRED", true, None)
            );
            assert_eq!(
                resolve_completion_state("aborted", &verdict),
                ("TASK_STATE_CANCELED", true, None)
            );
            assert_eq!(
                resolve_completion_state("garbage", &verdict),
                ("TASK_STATE_FAILED", true, None)
            );
        }
    }

    #[test]
    fn is_safe_relative_path_guard() {
        assert!(is_safe_relative_path("out/report.md"));
        assert!(is_safe_relative_path("./a/b"));
        assert!(!is_safe_relative_path("../x"));
        assert!(!is_safe_relative_path("/abs"));
        assert!(!is_safe_relative_path(""));
        let _ = Path::new("x");
    }
}
