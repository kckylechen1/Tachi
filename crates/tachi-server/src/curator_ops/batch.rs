//! Batch orchestration core: per-candidate packet assembly, budget
//! enforcement with explicit (never-silent) overflow reporting, and
//! idempotent verdict storage.
//!
//! Deliberately pure w.r.t. GitHub/lane I/O — the only side effects are (a)
//! calling the injected `LaneRunner` and (b) writing verdict rows through
//! the already-tested `verdict` module. This keeps `run_curator_batch` fully
//! testable against a fake runner (#1002 scout recommendation).

use super::runner::LaneRunner;
use super::verdict::{save_curator_verdict, CuratorVerdict, CuratorVerdictKind};
use crate::server_state::MemoryServer;
use chrono::Utc;
use serde::Serialize;

/// One candidate issue to re-verify. `evidence_on_file` is whatever the
/// #1000 freshness layer (or an explicit caller) already knows about it —
/// e.g. a zombie hit's `Refs #N` PR, or a stale-candidate's vanished
/// file:line anchor — so the packet handed to the lane is bounded, not the
/// full issue body re-fetched from scratch.
#[derive(Debug, Clone)]
pub(crate) struct CuratorCandidate {
    pub issue_ref: String,
    pub issue_summary: String,
    pub evidence_on_file: Vec<String>,
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

fn build_packet(candidate: &CuratorCandidate, head_sha: &str) -> String {
    let evidence = if candidate.evidence_on_file.is_empty() {
        "(none on file)".to_string()
    } else {
        candidate.evidence_on_file.join("\n- ")
    };
    format!(
        "Re-verify this GitHub issue's claim against HEAD ({head_sha}).\n\n\
         Issue: {issue_ref}\n\
         Summary: {summary}\n\n\
         Evidence already on file:\n- {evidence}\n\n\
         Produce a file:line-anchored verdict: does the claim still hold \
         (\"still_valid\"), is it already fixed on a merged PR/commit \
         (\"fixed_pending_closure\", cite the PR/commit), or is the proposed \
         fix stale relative to HEAD/doctrine (\"stale_spec\", cite what \
         superseded it)? Do not close the issue, do not edit its body.",
        head_sha = head_sha,
        issue_ref = candidate.issue_ref,
        summary = candidate.issue_summary,
    )
}

/// Very small heuristic classifier over the lane's raw evidence text, used
/// only when the caller hasn't pre-classified the outcome (the live wiring
/// path can substitute a structured lane response later — #1002 v1 keeps
/// this minimal per its own "orchestration shell, not a new inference
/// engine" scope). Defaults to `StillValid` (never silently claims a fix)
/// when no explicit signal is found.
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

fn draft_comment(issue_ref: &str, verdict: CuratorVerdictKind, evidence_text: &str) -> String {
    match verdict {
        CuratorVerdictKind::StillValid => format!(
            "Curator re-verification ({issue_ref}): claim still holds against HEAD.\n\n{evidence_text}"
        ),
        CuratorVerdictKind::FixedPendingClosure => format!(
            "Curator re-verification ({issue_ref}): appears already fixed. \
             Evidence below — closing is a leader/owner action, not automatic.\n\n{evidence_text}"
        ),
        CuratorVerdictKind::StaleSpec => format!(
            "Curator re-verification ({issue_ref}): proposed fix is stale relative to HEAD. \
             A draft of what superseded it is below — spec rewrite is a leader/owner action.\n\n{evidence_text}"
        ),
    }
}

/// Run one bounded batch: for each candidate, in order, check
/// budget/idempotency, call the lane, classify + store a verdict row.
/// Stops dispatching new candidates once the budget is exhausted but always
/// finishes the report — every remaining candidate lands in `skipped` with
/// a reason (never a silent truncation).
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

        let verdict_kind = classify_evidence(&outcome.evidence_text);
        let comment = draft_comment(&candidate.issue_ref, verdict_kind, &outcome.evidence_text);

        let row = CuratorVerdict {
            issue_ref: candidate.issue_ref.clone(),
            verified_at_sha: params.head_sha.clone(),
            verdict: verdict_kind,
            evidence_refs: vec![outcome.evidence_text.clone()],
            draft_comment: comment,
            lane: outcome.lane_id.clone(),
            checked_at: Utc::now().to_rfc3339(),
        };
        save_curator_verdict(server, &row)?;

        total_cost_tokens = total_cost_tokens.saturating_add(outcome.cost_tokens);
        verified.push(CuratorBatchResultRow {
            issue_ref: candidate.issue_ref.clone(),
            verdict: verdict_kind,
            lane: outcome.lane_id,
            cost_tokens: outcome.cost_tokens,
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
    use super::super::runner::LaneOutcome;
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
        let c = draft_comment("o/r#1", CuratorVerdictKind::FixedPendingClosure, "evidence");
        assert!(c.contains("leader/owner"));
        assert!(!c.to_ascii_lowercase().contains("closed this issue"));
    }
}
