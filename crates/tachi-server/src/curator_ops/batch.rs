//! Batch orchestration core: per-candidate packet assembly, budget
//! enforcement with explicit (never-silent) overflow reporting, and
//! idempotent verdict storage.
//!
//! Deliberately pure w.r.t. GitHub/lane I/O — the only side effects are (a)
//! calling the injected `LaneRunner` and (b) writing verdict rows through
//! the already-tested `verdict` module. This keeps `run_curator_batch` fully
//! testable against a fake runner (#1002 scout recommendation).

use super::runner::{LaneRunner, ReverifyOutcome};
use super::verdict::{persist_curator_verdict, CuratorVerdictKind};
use crate::server_state::MemoryServer;
use serde::Serialize;

/// One candidate issue to re-verify. `issue_body` + `file_line_anchors` are
/// the full issue text and its extracted `path:line` references (codex
/// review Finding #2: the prior packet only carried a synthetic summary +
/// saved references, never the issue's actual body/anchors) — see
/// `entry.rs`'s packet-building callers for how these get fetched.
/// `evidence_on_file` is whatever the #1000 freshness layer (or an explicit
/// caller) already knows about it — e.g. a zombie hit's `Refs #N` PR, or a
/// stale-candidate's vanished file:line anchor — bounding the packet without
/// re-deriving that part from scratch.
#[derive(Debug, Clone, Default)]
pub(crate) struct CuratorCandidate {
    pub issue_ref: String,
    pub issue_summary: String,
    pub evidence_on_file: Vec<String>,
    /// Full issue body text (+ comments, concatenated) — Finding #2 fix.
    pub issue_body: String,
    /// `path:line` anchors extracted from the issue body — Finding #2 fix.
    pub file_line_anchors: Vec<(String, u64)>,
}

#[derive(Debug, Clone)]
pub(crate) struct CuratorBatchParams {
    pub candidates: Vec<CuratorCandidate>,
    pub head_sha: String,
    /// Hard cap on total self-reported `cost_tokens` for this batch. `None`
    /// = unbounded (caller's explicit choice, not a silent default).
    pub budget_tokens: Option<u64>,
    /// Skip re-verifying a candidate whose verdict is already fresh at the
    /// current `head_sha` — makes repeated batch runs idempotent instead of
    /// re-spending budget on unchanged state.
    pub skip_if_fresh: bool,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct CuratorBatchResultRow {
    pub issue_ref: String,
    pub verdict: CuratorVerdictKind,
    pub lane: String,
    pub cost_tokens: u64,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct CuratorBatchReport {
    pub verified: Vec<CuratorBatchResultRow>,
    /// Never a silent cap: every skipped candidate is named + reasoned
    /// (`scan_open_loops` / #1000 convention).
    pub skipped: Vec<(String, String)>,
    pub total_cost_tokens: u64,
    pub budget_tokens: Option<u64>,
}

/// Builds the lane packet. Codex review Finding #2 (BUG): the prior version
/// carried only a synthetic `issue_summary` + `evidence_on_file` — never the
/// issue's actual body or its complete file:line anchors, so explicit
/// `curator_issue_refs` candidates got an EMPTY evidence list and even
/// freshness-sourced candidates never saw the real issue text. This version
/// requires the caller (`entry.rs`) to have already fetched `issue_body` +
/// `file_line_anchors` (fixture-testable, no live `gh` call inside this pure
/// fn) and includes both in the packet.
fn build_packet(candidate: &CuratorCandidate, head_sha: &str) -> String {
    let evidence = if candidate.evidence_on_file.is_empty() {
        "(none on file)".to_string()
    } else {
        candidate.evidence_on_file.join("\n- ")
    };
    let body = if candidate.issue_body.trim().is_empty() {
        "(issue body unavailable — re-verify from summary/anchors only)".to_string()
    } else {
        candidate.issue_body.clone()
    };
    let anchors = if candidate.file_line_anchors.is_empty() {
        "(none extracted)".to_string()
    } else {
        candidate
            .file_line_anchors
            .iter()
            .map(|(path, line)| format!("{path}:{line}"))
            .collect::<Vec<_>>()
            .join("\n- ")
    };
    format!(
        "Re-verify this GitHub issue's claim against HEAD ({head_sha}).\n\n\
         Issue: {issue_ref}\n\
         Summary: {summary}\n\n\
         Full issue text (body + comments):\n{body}\n\n\
         Anchors extracted from the issue text:\n- {anchors}\n\n\
         Evidence already on file:\n- {evidence}\n\n\
         Produce a file:line-anchored verdict: does the claim still hold \
         (\"still_valid\"), is it already fixed on a merged PR/commit \
         (\"fixed_pending_closure\", cite the PR/commit), or is the proposed \
         fix stale relative to HEAD/doctrine (\"stale_spec\", cite what \
         superseded it)? Do not close the issue, do not edit its body.",
        head_sha = head_sha,
        issue_ref = candidate.issue_ref,
        summary = candidate.issue_summary,
        body = body,
        anchors = anchors,
        evidence = evidence,
    )
}

/// Very small heuristic classifier over the lane's raw evidence text, used
/// only when the caller hasn't pre-classified the outcome (the live wiring
/// path can substitute a structured lane response later — #1002 v1 keeps
/// this minimal per its own "orchestration shell, not a new inference
/// engine" scope). Defaults to `StillValid` (never silently claims a fix)
/// when no explicit signal is found. ONLY called once non-empty terminal
/// evidence exists (Finding #1) — never on a placeholder.
fn classify_evidence(evidence_text: &str) -> CuratorVerdictKind {
    let lower = evidence_text.to_ascii_lowercase();
    if lower.contains("stale_spec") || lower.contains("stale-spec") {
        CuratorVerdictKind::StaleSpec
    } else if lower.contains("fixed_pending_closure") || lower.contains("already fixed") {
        CuratorVerdictKind::FixedPendingClosure
    } else {
        CuratorVerdictKind::StillValid
    }
}

/// Run one bounded batch: for each candidate, in order, check
/// budget/idempotency, call the lane, classify + store a verdict row.
/// Stops dispatching new candidates once the budget is exhausted but always
/// finishes the report — every remaining candidate lands in `skipped` with
/// a reason (never a silent truncation).
///
/// Codex review Finding #1 fix: a candidate's `ReverifyOutcome` is one of
/// three shapes — `Evidence` (real, classifiable, persisted as one of the
/// three real verdicts), `Timeout`/`LaneFailed` (no usable evidence,
/// persisted ONLY as `pending_evidence`/`lane_failed` via
/// `persist_curator_verdict`'s structural gate — never coerced into
/// `still_valid`). All three land in `report.verified` (they ARE a stored,
/// auditable row — just not an actionable real verdict) so a caller can see
/// exactly what happened to every dispatched candidate, not just the ones
/// that got real verdicts.
pub(crate) async fn run_curator_batch(
    server: &MemoryServer,
    runner: &dyn LaneRunner,
    params: CuratorBatchParams,
) -> Result<CuratorBatchReport, String> {
    let mut verified = Vec::new();
    let mut skipped = Vec::new();
    let mut total_cost_tokens: u64 = 0;
    let mut budget_exhausted = false;

    for candidate in &params.candidates {
        if budget_exhausted {
            skipped.push((
                candidate.issue_ref.clone(),
                format!(
                    "batch budget exhausted ({total_cost_tokens}/{} tokens spent before this candidate)",
                    params.budget_tokens.unwrap_or_default()
                ),
            ));
            continue;
        }

        if params.skip_if_fresh {
            if let Ok(Some(existing)) =
                super::verdict::get_curator_verdict(server, &candidate.issue_ref)
            {
                if existing.verified_at_sha == params.head_sha {
                    skipped.push((
                        candidate.issue_ref.clone(),
                        format!(
                            "already re-verified at {} (idempotent skip)",
                            params.head_sha
                        ),
                    ));
                    continue;
                }
            }
        }

        let packet = build_packet(candidate, &params.head_sha);
        let outcome = match runner.reverify(server, &candidate.issue_ref, &packet).await {
            Ok(o) => o,
            Err(e) => {
                skipped.push((candidate.issue_ref.clone(), format!("lane error: {e}")));
                continue;
            }
        };

        let (verdict_kind, evidence_text, lane_id, cost_tokens) = match outcome {
            ReverifyOutcome::Evidence(lane_outcome) => (
                classify_evidence(&lane_outcome.evidence_text),
                lane_outcome.evidence_text,
                lane_outcome.lane_id,
                lane_outcome.cost_tokens,
            ),
            ReverifyOutcome::Timeout { lane_id } => (
                CuratorVerdictKind::PendingEvidence,
                String::new(),
                lane_id,
                0,
            ),
            ReverifyOutcome::LaneFailed { lane_id, reason } => {
                (CuratorVerdictKind::LaneFailed, reason, lane_id, 0)
            }
        };

        persist_curator_verdict(
            server,
            &candidate.issue_ref,
            &params.head_sha,
            verdict_kind,
            &evidence_text,
            &lane_id,
        )?;

        total_cost_tokens = total_cost_tokens.saturating_add(cost_tokens);
        verified.push(CuratorBatchResultRow {
            issue_ref: candidate.issue_ref.clone(),
            verdict: verdict_kind,
            lane: lane_id,
            cost_tokens,
        });

        if let Some(budget) = params.budget_tokens {
            if total_cost_tokens >= budget {
                budget_exhausted = true;
            }
        }
    }

    Ok(CuratorBatchReport {
        verified,
        skipped,
        total_cost_tokens,
        budget_tokens: params.budget_tokens,
    })
}

#[cfg(test)]
mod tests {
    use super::super::runner::fake::FakeLaneRunner;
    use super::super::runner::{LaneOutcome, ReverifyOutcome};
    use super::*;

    fn test_server() -> MemoryServer {
        let db = std::env::temp_dir().join(format!(
            "issue-curator-batch-test-{}.sqlite",
            uuid::Uuid::new_v4()
        ));
        MemoryServer::new(db, None).expect("test server")
    }

    fn candidate(issue_ref: &str) -> CuratorCandidate {
        CuratorCandidate {
            issue_ref: issue_ref.to_string(),
            issue_summary: format!("summary for {issue_ref}"),
            evidence_on_file: vec!["Refs #980".to_string()],
            issue_body: format!("full issue body for {issue_ref}"),
            file_line_anchors: vec![("src/x.rs".to_string(), 12)],
        }
    }

    #[tokio::test]
    async fn verifies_all_candidates_and_persists_verdicts() {
        let server = test_server();
        let runner = FakeLaneRunner::new(vec![
            (
                "o/r#979",
                LaneOutcome {
                    evidence_text: "already fixed on PR #980, file:line src/x.rs:12".into(),
                    cost_tokens: 100,
                    lane_id: "codex_55_review".into(),
                },
            ),
            (
                "o/r#987",
                LaneOutcome {
                    evidence_text: "reproduced flake at src/y.rs:40, still_valid".into(),
                    cost_tokens: 50,
                    lane_id: "codex_55_review".into(),
                },
            ),
        ]);

        let params = CuratorBatchParams {
            candidates: vec![candidate("o/r#979"), candidate("o/r#987")],
            head_sha: "abc123".into(),
            budget_tokens: None,
            skip_if_fresh: false,
        };

        let report = run_curator_batch(&server, &runner, params)
            .await
            .expect("batch run");

        assert_eq!(report.verified.len(), 2);
        assert!(report.skipped.is_empty());
        assert_eq!(report.total_cost_tokens, 150);

        let stored = super::super::verdict::list_curator_verdicts(&server).expect("list");
        assert_eq!(stored.len(), 2);
        assert!(stored
            .iter()
            .any(|v| v.issue_ref == "o/r#979"
                && v.verdict == CuratorVerdictKind::FixedPendingClosure));
        assert!(stored
            .iter()
            .any(|v| v.issue_ref == "o/r#987" && v.verdict == CuratorVerdictKind::StillValid));
    }

    #[tokio::test]
    async fn budget_cap_stops_dispatch_and_reports_skipped_not_silent() {
        let server = test_server();
        let runner = FakeLaneRunner::new(vec![
            (
                "o/r#1",
                LaneOutcome {
                    evidence_text: "still_valid".into(),
                    cost_tokens: 80,
                    lane_id: "codex_55_review".into(),
                },
            ),
            (
                "o/r#2",
                LaneOutcome {
                    evidence_text: "still_valid".into(),
                    cost_tokens: 80,
                    lane_id: "codex_55_review".into(),
                },
            ),
            (
                "o/r#3",
                LaneOutcome {
                    evidence_text: "still_valid".into(),
                    cost_tokens: 80,
                    lane_id: "codex_55_review".into(),
                },
            ),
        ]);

        let params = CuratorBatchParams {
            candidates: vec![candidate("o/r#1"), candidate("o/r#2"), candidate("o/r#3")],
            head_sha: "abc123".into(),
            // Cost is only known AFTER a lane runs (self-reported on
            // completion) — the budget check is necessarily post-hoc: #1
            // (cumulative 80) is under budget, #2 (cumulative 160) crosses
            // it and trips the cap for everything after, #3 is skipped.
            budget_tokens: Some(100),
            skip_if_fresh: false,
        };

        let report = run_curator_batch(&server, &runner, params)
            .await
            .expect("batch run");

        assert_eq!(
            report.verified.len(),
            2,
            "candidates fit until cumulative cost crosses budget post-hoc"
        );
        assert_eq!(
            report.skipped.len(),
            1,
            "remaining candidate must be reported skipped, not silently dropped"
        );
        assert!(report
            .skipped
            .iter()
            .any(|(id, reason)| id == "o/r#3" && reason.contains("budget exhausted")));
        assert_eq!(report.total_cost_tokens, 160);
        assert_eq!(report.budget_tokens, Some(100));

        // Only the verified candidates persisted verdict rows.
        let stored = super::super::verdict::list_curator_verdicts(&server).expect("list");
        assert_eq!(stored.len(), 2);
    }

    #[tokio::test]
    async fn skip_if_fresh_makes_rerun_idempotent() {
        let server = test_server();
        let runner = FakeLaneRunner::new(vec![(
            "o/r#5",
            LaneOutcome {
                evidence_text: "still_valid".into(),
                cost_tokens: 10,
                lane_id: "codex_55_review".into(),
            },
        )]);

        let first = CuratorBatchParams {
            candidates: vec![candidate("o/r#5")],
            head_sha: "sha-same".into(),
            budget_tokens: None,
            skip_if_fresh: true,
        };
        let report1 = run_curator_batch(&server, &runner, first)
            .await
            .expect("first run");
        assert_eq!(report1.verified.len(), 1);

        // Re-run at the SAME sha: must skip (idempotent), not re-spend budget.
        let second = CuratorBatchParams {
            candidates: vec![candidate("o/r#5")],
            head_sha: "sha-same".into(),
            budget_tokens: None,
            skip_if_fresh: true,
        };
        let report2 = run_curator_batch(&server, &runner, second)
            .await
            .expect("second run");
        assert_eq!(report2.verified.len(), 0);
        assert_eq!(report2.skipped.len(), 1);
        assert!(report2.skipped[0].1.contains("idempotent skip"));

        // Re-run at a DIFFERENT sha: must re-verify (not treated as fresh).
        let third = CuratorBatchParams {
            candidates: vec![candidate("o/r#5")],
            head_sha: "sha-different".into(),
            budget_tokens: None,
            skip_if_fresh: true,
        };
        let report3 = run_curator_batch(&server, &runner, third)
            .await
            .expect("third run");
        assert_eq!(
            report3.verified.len(),
            1,
            "different HEAD sha must trigger re-verification"
        );
    }

    #[tokio::test]
    async fn lane_error_is_skipped_with_reason_not_fatal_to_batch() {
        let server = test_server();
        // FakeLaneRunner returns an error for any issue_ref not scripted.
        let runner = FakeLaneRunner::new(vec![(
            "o/r#ok",
            LaneOutcome {
                evidence_text: "still_valid".into(),
                cost_tokens: 5,
                lane_id: "codex_55_review".into(),
            },
        )]);

        let params = CuratorBatchParams {
            candidates: vec![candidate("o/r#unscripted"), candidate("o/r#ok")],
            head_sha: "abc123".into(),
            budget_tokens: None,
            skip_if_fresh: false,
        };

        let report = run_curator_batch(&server, &runner, params)
            .await
            .expect("batch run should not fail wholesale on one lane error");

        assert_eq!(report.verified.len(), 1);
        assert_eq!(report.skipped.len(), 1);
        assert!(report.skipped[0].0 == "o/r#unscripted");
        assert!(report.skipped[0].1.contains("lane error"));
    }

    #[test]
    fn classify_evidence_defaults_to_still_valid_never_silently_claims_fixed() {
        assert_eq!(
            classify_evidence("nothing conclusive here"),
            CuratorVerdictKind::StillValid
        );
        assert_eq!(
            classify_evidence("this is already fixed on main"),
            CuratorVerdictKind::FixedPendingClosure
        );
        assert_eq!(
            classify_evidence("marked stale_spec by owner"),
            CuratorVerdictKind::StaleSpec
        );
    }

    #[test]
    fn draft_comment_never_claims_auto_close() {
        let c = super::super::verdict::draft_comment(
            "o/r#1",
            CuratorVerdictKind::FixedPendingClosure,
            "evidence",
        );
        assert!(c.contains("leader/owner"));
        assert!(!c.to_ascii_lowercase().contains("closed this issue"));
    }

    #[test]
    fn build_packet_carries_issue_body_and_anchors() {
        // Finding #2 (BUG): the packet handed to the lane must include the
        // full issue body + extracted file:line anchors, not just a
        // synthetic summary.
        let mut c = candidate("o/r#42");
        c.issue_body = "The bug is in the retry loop, see anchor below.".to_string();
        c.file_line_anchors = vec![("src/retry.rs".to_string(), 88)];
        let packet = build_packet(&c, "sha1");
        assert!(
            packet.contains("The bug is in the retry loop"),
            "packet must contain full issue body: {packet}"
        );
        assert!(
            packet.contains("src/retry.rs:88"),
            "packet must contain extracted file:line anchors: {packet}"
        );
    }

    #[test]
    fn build_packet_handles_missing_body_and_anchors_explicitly() {
        let mut c = candidate("o/r#42");
        c.issue_body = String::new();
        c.file_line_anchors = Vec::new();
        let packet = build_packet(&c, "sha1");
        assert!(packet.contains("issue body unavailable"));
        assert!(packet.contains("none extracted"));
    }

    // ─── Finding #1's invariant: empty evidence can never persist a real
    // verdict. Red/green pair over `run_curator_batch`'s outcome-routing. ───

    #[tokio::test]
    async fn timeout_persists_pending_evidence_never_still_valid() {
        let server = test_server();
        let runner = FakeLaneRunner::new_outcomes(vec![(
            "o/r#100",
            ReverifyOutcome::Timeout {
                lane_id: "codex_55_review".to_string(),
            },
        )]);

        let params = CuratorBatchParams {
            candidates: vec![candidate("o/r#100")],
            head_sha: "abc123".into(),
            budget_tokens: None,
            skip_if_fresh: false,
        };

        let report = run_curator_batch(&server, &runner, params)
            .await
            .expect("batch run");

        // The candidate is reported (not silently dropped)...
        assert_eq!(report.verified.len(), 1);
        assert_eq!(
            report.verified[0].verdict,
            CuratorVerdictKind::PendingEvidence
        );

        // ...and the ONLY thing persisted is pending_evidence — never a real
        // verdict, and specifically never still_valid (the exact disease
        // Finding #1 named).
        let stored = super::super::verdict::get_curator_verdict(&server, "o/r#100")
            .expect("get")
            .expect("row persisted");
        assert_eq!(stored.verdict, CuratorVerdictKind::PendingEvidence);
        assert_ne!(stored.verdict, CuratorVerdictKind::StillValid);
        assert!(!stored.verdict.is_real_verdict());
    }

    #[tokio::test]
    async fn lane_failed_persists_lane_failed_never_a_real_verdict() {
        let server = test_server();
        let runner = FakeLaneRunner::new_outcomes(vec![(
            "o/r#101",
            ReverifyOutcome::LaneFailed {
                lane_id: "codex_55_review".to_string(),
                reason: "dispatch terminal state TASK_STATE_FAILED, no result.md".to_string(),
            },
        )]);

        let params = CuratorBatchParams {
            candidates: vec![candidate("o/r#101")],
            head_sha: "abc123".into(),
            budget_tokens: None,
            skip_if_fresh: false,
        };

        let report = run_curator_batch(&server, &runner, params)
            .await
            .expect("batch run");

        assert_eq!(report.verified.len(), 1);
        assert_eq!(report.verified[0].verdict, CuratorVerdictKind::LaneFailed);

        let stored = super::super::verdict::get_curator_verdict(&server, "o/r#101")
            .expect("get")
            .expect("row persisted");
        assert_eq!(stored.verdict, CuratorVerdictKind::LaneFailed);
        assert!(!stored.verdict.is_real_verdict());
        assert!(stored.evidence_refs[0].contains("TASK_STATE_FAILED"));
    }

    #[tokio::test]
    async fn real_evidence_still_persists_real_verdicts_after_the_fix() {
        // Non-regression: the happy path (real terminal evidence) must still
        // classify + persist a real verdict, exactly as before Finding #1's
        // fix — the gate must not make legitimate verdicts unreachable.
        let server = test_server();
        let runner = FakeLaneRunner::new(vec![(
            "o/r#979",
            LaneOutcome {
                evidence_text: "already fixed on PR #980, file:line src/x.rs:12".into(),
                cost_tokens: 100,
                lane_id: "codex_55_review".into(),
            },
        )]);

        let params = CuratorBatchParams {
            candidates: vec![candidate("o/r#979")],
            head_sha: "abc123".into(),
            budget_tokens: None,
            skip_if_fresh: false,
        };

        let report = run_curator_batch(&server, &runner, params)
            .await
            .expect("batch run");
        assert_eq!(
            report.verified[0].verdict,
            CuratorVerdictKind::FixedPendingClosure
        );
        let stored = super::super::verdict::get_curator_verdict(&server, "o/r#979")
            .expect("get")
            .expect("row persisted");
        assert!(stored.verdict.is_real_verdict());
        assert!(!stored.evidence_refs.is_empty());
    }
}
