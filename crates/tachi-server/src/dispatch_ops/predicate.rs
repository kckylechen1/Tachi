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
            match crate::dispatch_ops::regular_file_len_within(&base, &full) {
                Ok(Some(len)) if len > 0 => PredicateVerdict::Pass,
                Ok(Some(_)) => PredicateVerdict::Fail(format!(
                    "expected artifact '{path}' exists but is empty"
                )),
                Ok(None) => {
                    PredicateVerdict::Fail(format!("expected artifact '{path}' is missing"))
                }
                Err(reason) => PredicateVerdict::Fail(format!(
                    "refusing completion artifact predicate '{path}': {reason}"
                )),
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
/// `home` is the caller's resolved Tachi home directory (from
/// `MemoryServer::tachi_home_dir()` at both call sites — #1096 leaf-2a: this
/// always runs after a server exists, so it reads the server-bound identity
/// instead of re-deriving it from env).
///
/// Returns `(run_dir, declared_predicate, cwd)`.
///
/// tachi#1173 k2 fix: `dispatch_id` here is caller-supplied (via
/// `TachiCompleteParams::dispatch_id` on `tachi_complete`, at both call
/// sites — one directly on the leader-supplied `tachi_complete`, the other
/// on the dispatch's own freshly-minted id from `execution.rs`) and was
/// joined directly onto `home.join("runs")` with no validation -- the same
/// path-traversal shape tachi#1173's board autopsy review closed in
/// `board::runs::collect_run_task_by_id` (eb473fd0). Gated the same way,
/// fail-closed to the existing "not found" branch (no separate warn: an
/// out-of-allowlist id gets no signal about what does/doesn't exist on
/// disk, same posture as the character-allowlist rejection elsewhere).
pub(crate) fn resolve_completion_predicate_context(
    home: &Path,
    dispatch_id: &str,
) -> Result<
    (
        Option<PathBuf>,
        Option<CompletionPredicate>,
        Option<PathBuf>,
    ),
    String,
> {
    if !crate::dispatch_ops::is_valid_dispatch_id(dispatch_id) {
        return Ok((None, None, None));
    }
    let runs_dir = home.join("runs");
    let run_dir = runs_dir.join(dispatch_id);
    if !run_dir.is_dir() {
        // #1096 leaf-2a round-2 (codex B2): loud, not a silent fall-through
        // to Unverified. This is either a genuinely stale/foreign dispatch_id
        // OR `home` disagreeing with wherever the run was actually written —
        // exactly the failure mode the frozen/live-read split (see the
        // `home_dir` invariant doc) would produce if that invariant were
        // ever violated. Does not change control flow: still resolves to
        // `PredicateVerdict::Unverified` same as before this line existed.
        tracing::warn!(
            dispatch_id = %dispatch_id,
            run_dir = %run_dir.display(),
            "completion predicate context: run directory not found under resolved home; \
             falling through to Unverified"
        );
        return Ok((None, None, None));
    }
    if !crate::dispatch_ops::canonical_dir_is_within(&run_dir, &runs_dir) {
        return Err(format!(
            "refusing completion predicate context for dispatch_id={dispatch_id}: \
             run directory escapes {}",
            runs_dir.display()
        ));
    }
    let run_dir = run_dir.canonicalize().map_err(|error| {
        format!(
            "refusing completion predicate context for dispatch_id={dispatch_id}: \
             resolve run directory {}: {error}",
            run_dir.display()
        )
    })?;
    let status_path = run_dir.join("status.json");
    let status_raw = match crate::dispatch_ops::read_text_file_within(&runs_dir, &status_path)? {
        Some(raw) => raw,
        None => return Ok((Some(run_dir), None, None)),
    };
    let status: Value = serde_json::from_str(&status_raw).map_err(|error| {
        format!(
            "refusing completion predicate context for dispatch_id={dispatch_id}: \
             parse {}: {error}",
            status_path.display()
        )
    })?;
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
    Ok((Some(run_dir), pred, cwd))
}

/// Resolve the dispatch ledger context and evaluate its predicate using a
/// descriptor-bound read of `result.md`. A missing result uses
/// `fallback_output`; containment, symlink-race, UTF-8, or status parse
/// failures are returned to the caller and may not degrade to Unverified.
pub(crate) fn evaluate_completion_predicate_for_dispatch(
    home: &Path,
    dispatch_id: &str,
    fallback_output: &str,
) -> Result<(bool, PredicateVerdict), String> {
    let (run_dir, predicate, cwd) = resolve_completion_predicate_context(home, dispatch_id)?;
    let output = match run_dir.as_deref() {
        Some(run_dir) => crate::dispatch_ops::read_text_file_within(
            &home.join("runs"),
            &run_dir.join("result.md"),
        )?
        .unwrap_or_else(|| fallback_output.to_string()),
        None => fallback_output.to_string(),
    };
    let empty_run_dir = PathBuf::new();
    let verdict = evaluate_completion_predicate(
        predicate.as_ref(),
        run_dir.as_deref().unwrap_or(&empty_run_dir),
        cwd.as_deref(),
        &output,
    );
    Ok((predicate.is_some(), verdict))
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

    #[cfg(unix)]
    #[test]
    fn artifact_predicate_refuses_outward_final_leaf_symlink() {
        let dir = td();
        let outside = td();
        std::fs::create_dir_all(dir.path().join("out")).unwrap();
        std::fs::write(dir.path().join("out/report.md"), "ordinary report").unwrap();
        std::fs::write(outside.path().join("report.md"), "outside report").unwrap();
        let pred = CompletionPredicate::ArtifactNonEmpty {
            path: "out/report.md".to_string(),
        };

        assert_eq!(
            evaluate_completion_predicate(Some(&pred), dir.path(), Some(dir.path()), ""),
            PredicateVerdict::Pass
        );

        std::fs::remove_file(dir.path().join("out/report.md")).unwrap();
        std::os::unix::fs::symlink(
            outside.path().join("report.md"),
            dir.path().join("out/report.md"),
        )
        .unwrap();

        assert!(matches!(
            evaluate_completion_predicate(Some(&pred), dir.path(), Some(dir.path()), ""),
            PredicateVerdict::Fail(reason) if reason.contains("refusing")
        ));
    }

    #[cfg(unix)]
    #[test]
    fn artifact_predicate_refuses_outward_symlinked_parent_component() {
        let dir = td();
        let outside = td();
        std::fs::write(outside.path().join("report.md"), "outside report").unwrap();
        std::os::unix::fs::symlink(outside.path(), dir.path().join("out")).unwrap();
        let pred = CompletionPredicate::ArtifactNonEmpty {
            path: "out/report.md".to_string(),
        };

        assert!(matches!(
            evaluate_completion_predicate(Some(&pred), dir.path(), Some(dir.path()), ""),
            PredicateVerdict::Fail(reason) if reason.contains("refusing")
        ));
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

    /// tachi#1173 k2 fix discriminator: a caller-supplied `dispatch_id`
    /// containing a path-traversal or absolute-path payload must be rejected
    /// fail-closed by `resolve_completion_predicate_context` -- and must
    /// never resolve `run_dir` to a decoy directory planted outside
    /// `home/runs` that a successful escape would have read (status.json,
    /// then downstream result.md/cwd).
    #[test]
    fn resolve_completion_predicate_context_rejects_path_traversal_dispatch_id() {
        let tmp = td();
        let home = tmp.path();
        let runs_dir = home.join("runs");
        std::fs::create_dir_all(&runs_dir).expect("create runs dir");

        let decoy_dir = home.join("decoy");
        std::fs::create_dir_all(&decoy_dir).expect("create decoy dir");
        std::fs::write(
            decoy_dir.join("status.json"),
            serde_json::json!({
                "completion_predicate": {"type": "output_matches", "pattern": ".*"},
                "cwd": "/should/never/be/read",
            })
            .to_string(),
        )
        .expect("write decoy status.json");

        for malicious in [
            "../decoy",
            "../../decoy",
            "..",
            "",
            "/etc/passwd",
            "a/../../decoy",
        ] {
            let (run_dir, pred, cwd) = resolve_completion_predicate_context(home, malicious)
                .expect("invalid ids are a quiet miss");
            assert!(
                run_dir.is_none() && pred.is_none() && cwd.is_none(),
                "dispatch_id {malicious:?} must be rejected fail-closed, not resolved \
                 outside home/runs; got run_dir={run_dir:?} pred={pred:?} cwd={cwd:?}"
            );
        }
    }

    /// The gate must not break the ordinary path: a dispatch_id shaped like
    /// a real one, with a real status.json under it, still resolves.
    #[test]
    fn resolve_completion_predicate_context_still_resolves_legit_dispatch_id() {
        let tmp = td();
        let home = tmp.path();
        let dispatch_id = "20260718T101010Z-claude-abc12345";
        let run_dir = home.join("runs").join(dispatch_id);
        std::fs::create_dir_all(&run_dir).expect("create run dir");
        std::fs::write(
            run_dir.join("status.json"),
            serde_json::json!({"cwd": "/legit/cwd"}).to_string(),
        )
        .expect("write status.json");

        let (resolved_run_dir, _pred, cwd) =
            resolve_completion_predicate_context(home, dispatch_id).expect("context read");
        assert_eq!(resolved_run_dir, Some(run_dir.canonicalize().unwrap()));
        assert_eq!(cwd, Some(PathBuf::from("/legit/cwd")));
    }

    #[cfg(unix)]
    #[test]
    fn completion_status_context_keeps_opened_object_across_post_open_swap() {
        let tmp = td();
        let outside = td();
        let home = tmp.path();
        let dispatch_id = "20260718T101011Z-status-race";
        let run_dir = home.join("runs").join(dispatch_id);
        let status_path = run_dir.join("status.json");
        let outside_status = outside.path().join("status.json");
        std::fs::create_dir_all(&run_dir).unwrap();
        std::fs::write(
            &status_path,
            serde_json::json!({
                "completion_predicate": {"type": "output_matches", "pattern": "inside"},
                "cwd": "/inside/cwd",
            })
            .to_string(),
        )
        .unwrap();
        std::fs::write(
            &outside_status,
            serde_json::json!({
                "completion_predicate": {"type": "output_matches", "pattern": "outside"},
                "cwd": "/outside/cwd",
            })
            .to_string(),
        )
        .unwrap();
        crate::dispatch_ops::install_secure_read_hook(
            crate::dispatch_ops::SecureReadHookStage::AfterOpen,
            status_path.clone(),
            move |opened| {
                std::fs::remove_file(opened).unwrap();
                std::os::unix::fs::symlink(&outside_status, opened).unwrap();
            },
        );

        let (_, predicate, cwd) = resolve_completion_predicate_context(home, dispatch_id)
            .expect("descriptor-bound context read");
        assert_eq!(cwd, Some(PathBuf::from("/inside/cwd")));
        assert!(matches!(
            predicate,
            Some(CompletionPredicate::OutputMatches { pattern }) if pattern == "inside"
        ));
    }

    #[cfg(unix)]
    #[test]
    fn completion_result_refuses_pre_open_swap_and_never_matches_outside_bytes() {
        let tmp = td();
        let outside = td();
        let home = tmp.path();
        let dispatch_id = "20260718T101012Z-result-pre-open-race";
        let run_dir = home.join("runs").join(dispatch_id);
        let result_path = run_dir.join("result.md");
        let outside_result = outside.path().join("result.md");
        std::fs::create_dir_all(&run_dir).unwrap();
        std::fs::write(
            run_dir.join("status.json"),
            serde_json::json!({
                "completion_predicate": {"type": "output_matches", "pattern": "outside bytes"},
            })
            .to_string(),
        )
        .unwrap();
        std::fs::write(&result_path, "inside bytes").unwrap();
        std::fs::write(&outside_result, "outside bytes").unwrap();
        crate::dispatch_ops::install_secure_read_hook(
            crate::dispatch_ops::SecureReadHookStage::AfterValidation,
            result_path.clone(),
            move |validated| {
                std::fs::remove_file(validated).unwrap();
                std::os::unix::fs::symlink(&outside_result, validated).unwrap();
            },
        );

        let error = evaluate_completion_predicate_for_dispatch(home, dispatch_id, "fallback")
            .expect_err("pre-open swap must refuse instead of reading outside bytes");
        assert!(error.contains("refusing descriptor-bound read"), "{error}");
    }

    #[cfg(unix)]
    #[test]
    fn completion_result_keeps_opened_object_across_post_open_swap() {
        let tmp = td();
        let outside = td();
        let home = tmp.path();
        let dispatch_id = "20260718T101013Z-result-post-open-race";
        let run_dir = home.join("runs").join(dispatch_id);
        let result_path = run_dir.join("result.md");
        let outside_result = outside.path().join("result.md");
        std::fs::create_dir_all(&run_dir).unwrap();
        std::fs::write(
            run_dir.join("status.json"),
            serde_json::json!({
                "completion_predicate": {"type": "output_matches", "pattern": "inside bytes"},
            })
            .to_string(),
        )
        .unwrap();
        std::fs::write(&result_path, "inside bytes").unwrap();
        std::fs::write(&outside_result, "outside bytes").unwrap();
        crate::dispatch_ops::install_secure_read_hook(
            crate::dispatch_ops::SecureReadHookStage::AfterOpen,
            result_path.clone(),
            move |opened| {
                std::fs::remove_file(opened).unwrap();
                std::os::unix::fs::symlink(&outside_result, opened).unwrap();
            },
        );

        let (declared, verdict) =
            evaluate_completion_predicate_for_dispatch(home, dispatch_id, "fallback")
                .expect("descriptor-bound result read");
        assert!(declared);
        assert_eq!(verdict, PredicateVerdict::Pass);
    }
}
