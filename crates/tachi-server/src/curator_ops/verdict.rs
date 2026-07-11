//! Curator verdict storage (`state_kv`, namespace `issue_curator`) — same
//! shape/precedent as `gh_ops::issue_freshness`'s `FreshnessVerdict` rows,
//! kept in a separate namespace because the curator's verdict is a
//! *semantic re-verification* (three-tier judgment + drafted writeback),
//! not a mechanical scan hit.
//!
//! `verified_at_sha` + overwrite-by-`issue_ref` semantics make re-runs
//! idempotent: re-verifying the same issue at the same HEAD sha just
//! replaces the row (no duplicate accumulation), matching the #1000
//! precedent (`save_freshness_verdict`) exactly.

use crate::server_state::MemoryServer;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

pub(crate) const CURATOR_VERDICT_NS: &str = "issue_curator";

/// Three-tier judgment per #1002's frozen spec (仍成立 / 已修待收口 / 方案过时),
/// plus two non-verdict placeholder states that a caller may persist when no
/// lane evidence exists yet. Codex review finding #1 (BROKEN, severity-max):
/// a curator that writes `still_valid` (or either other real verdict) with no
/// evidence is the exact disease (#1002) it exists to cure. `PendingEvidence`
/// and `LaneFailed` exist so "the lane hasn't reported back yet" / "the lane
/// errored" are representable WITHOUT smuggling a fabricated real verdict —
/// see `persist_curator_verdict`'s non-empty-evidence gate below, which is
/// the structural enforcement (not just a convention).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CuratorVerdictKind {
    /// 仍成立 — claim still holds against HEAD. Requires non-empty evidence.
    StillValid,
    /// 已修待收口 — a merged PR/commit already covers this; advisory only,
    /// the curator never closes the issue itself. Requires non-empty evidence.
    FixedPendingClosure,
    /// 方案过时 — the issue's proposed fix no longer matches HEAD/doctrine.
    /// Requires non-empty evidence.
    StaleSpec,
    /// No terminal lane evidence exists yet (dispatch still running when the
    /// batch's wait window elapsed). NOT a real verdict — briefing/writeback
    /// code must treat this as "nothing to say yet," never as `still_valid`.
    PendingEvidence,
    /// The lane errored or returned a terminal-but-failed state (no usable
    /// evidence text). NOT a real verdict — same non-actionable posture as
    /// `PendingEvidence`.
    LaneFailed,
}

impl CuratorVerdictKind {
    /// Real three-tier verdicts require non-empty lane evidence to persist
    /// (Finding #1's structural gate). `PendingEvidence`/`LaneFailed` are the
    /// only kinds a caller may persist without one.
    pub(crate) fn is_real_verdict(self) -> bool {
        matches!(
            self,
            CuratorVerdictKind::StillValid
                | CuratorVerdictKind::FixedPendingClosure
                | CuratorVerdictKind::StaleSpec
        )
    }

    pub(crate) fn as_label(self) -> &'static str {
        match self {
            CuratorVerdictKind::StillValid => "reverified",
            CuratorVerdictKind::FixedPendingClosure => "verified-fixed",
            CuratorVerdictKind::StaleSpec => "stale-spec",
            CuratorVerdictKind::PendingEvidence => "pending-evidence",
            CuratorVerdictKind::LaneFailed => "lane-failed",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct CuratorVerdict {
    pub issue_ref: String,
    pub verified_at_sha: String,
    pub verdict: CuratorVerdictKind,
    pub evidence_refs: Vec<String>,
    /// Prepared writeback comment text. Never posted automatically — posting
    /// requires the caller to pass `post: true` to the batch run, which is
    /// itself default-`false` (#1002 non-goal: curator never auto-writes).
    pub draft_comment: String,
    /// Which lane produced this verdict (e.g. "codex_55_review", "fake" in tests).
    pub lane: String,
    pub checked_at: String,
}

fn curator_key(issue_ref: &str) -> String {
    format!("verdict:{issue_ref}")
}

pub(crate) fn save_curator_verdict(
    server: &MemoryServer,
    verdict: &CuratorVerdict,
) -> Result<(), String> {
    let json = serde_json::to_string(verdict).map_err(|e| format!("serialize verdict: {e}"))?;
    server.with_global_store(|store| -> Result<(), String> {
        store
            .set_state(CURATOR_VERDICT_NS, &curator_key(&verdict.issue_ref), &json)
            .map_err(|e| format!("issue_curator set_state: {e}"))?;
        Ok(())
    })
}

/// Structural gate for Finding #1 (codex review, severity-max): the ONLY way
/// into `state_kv` for a curator verdict row now goes through this function,
/// and it REFUSES to persist a real verdict (`still_valid` /
/// `fixed_pending_closure` / `stale_spec`) unless `evidence_text` is
/// non-empty. `pending_evidence` / `lane_failed` are the only kinds allowed
/// through with empty evidence — this is what makes "no terminal lane result
/// yet" representable without ever fabricating a real verdict.
///
/// `batch.rs` is the only caller; it is intentionally the single choke point
/// so this invariant cannot be bypassed by a future call site that forgets
/// to check.
pub(crate) fn persist_curator_verdict(
    server: &MemoryServer,
    issue_ref: &str,
    verified_at_sha: &str,
    kind: CuratorVerdictKind,
    evidence_text: &str,
    lane: &str,
) -> Result<CuratorVerdict, String> {
    if kind.is_real_verdict() && evidence_text.trim().is_empty() {
        return Err(format!(
            "refusing to persist real verdict {:?} for {issue_ref} with empty evidence \
             (Finding #1 structural gate — use pending_evidence/lane_failed instead)",
            kind
        ));
    }
    let evidence_refs = if evidence_text.trim().is_empty() {
        Vec::new()
    } else {
        vec![evidence_text.to_string()]
    };
    let draft_comment = draft_comment(issue_ref, kind, evidence_text);
    let row = CuratorVerdict {
        issue_ref: issue_ref.to_string(),
        verified_at_sha: verified_at_sha.to_string(),
        verdict: kind,
        evidence_refs,
        draft_comment,
        lane: lane.to_string(),
        checked_at: Utc::now().to_rfc3339(),
    };
    save_curator_verdict(server, &row)?;
    Ok(row)
}

/// Drafted writeback text per verdict kind. `pending_evidence`/`lane_failed`
/// draft comments are informational only — `entry.rs`'s writeback path only
/// ever posts for real verdicts (see its `report.verified` iteration), so
/// these never reach GitHub, but a non-empty draft is still useful in the
/// stored row for a human reading `list_curator_verdicts` / the briefing
/// queue directly.
pub(crate) fn draft_comment(
    issue_ref: &str,
    verdict: CuratorVerdictKind,
    evidence_text: &str,
) -> String {
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
        CuratorVerdictKind::PendingEvidence => format!(
            "Curator re-verification ({issue_ref}): no terminal lane result yet — \
             re-run later, this is not a verdict."
        ),
        CuratorVerdictKind::LaneFailed => format!(
            "Curator re-verification ({issue_ref}): lane errored, no evidence collected — \
             this is not a verdict."
        ),
    }
}

pub(crate) fn list_curator_verdicts(server: &MemoryServer) -> Result<Vec<CuratorVerdict>, String> {
    let rows = server.with_global_store_read(|store| {
        store
            .list_state(CURATOR_VERDICT_NS)
            .map_err(|e| format!("issue_curator list_state: {e}"))
    })?;
    Ok(rows
        .into_iter()
        .filter_map(|row| serde_json::from_str::<CuratorVerdict>(&row.value_json).ok())
        .collect())
}

pub(crate) fn get_curator_verdict(
    server: &MemoryServer,
    issue_ref: &str,
) -> Result<Option<CuratorVerdict>, String> {
    server.with_global_store(|store| -> Result<Option<CuratorVerdict>, String> {
        let raw = store
            .get_state_kv(CURATOR_VERDICT_NS, &curator_key(issue_ref))
            .map_err(|e| format!("issue_curator get_state: {e}"))?;
        Ok(match raw {
            Some((json, _version)) => Some(
                serde_json::from_str(&json)
                    .map_err(|e| format!("issue_curator parse {issue_ref}: {e}"))?,
            ),
            None => None,
        })
    })
}

/// Briefing projection (#1002 scope item 5: "判决产出进 briefing 的「待收口/
/// 待重审」队列"). Splits stored verdicts into the two actionable queues —
/// `pending_closure` (curator thinks it's fixed; a leader/owner reads the
/// evidence and decides whether to close) and `pending_respec`
/// (curator thinks the fix is stale relative to HEAD; a leader/owner
/// decides whether to rewrite the spec) — leaving `still_valid` verdicts out
/// of the actionable queues entirely (nothing for a human to act on there).
/// Same capped + explicit-overflow convention as `scan_open_loops` /
/// `briefing_freshness_queues` — never a silent cap.
pub(crate) fn briefing_curator_queue(server: &MemoryServer, limit: usize) -> Value {
    let verdicts = list_curator_verdicts(server).unwrap_or_default();
    let mut pending_closure: Vec<&CuratorVerdict> = verdicts
        .iter()
        .filter(|v| v.verdict == CuratorVerdictKind::FixedPendingClosure)
        .collect();
    let mut pending_respec: Vec<&CuratorVerdict> = verdicts
        .iter()
        .filter(|v| v.verdict == CuratorVerdictKind::StaleSpec)
        .collect();
    pending_closure.sort_by(|a, b| a.issue_ref.cmp(&b.issue_ref));
    pending_respec.sort_by(|a, b| a.issue_ref.cmp(&b.issue_ref));

    let closure_count = pending_closure.len();
    let respec_count = pending_respec.len();
    let closure_rows: Vec<Value> = pending_closure
        .into_iter()
        .take(limit)
        .map(|v| {
            json!({
                "issue_ref": v.issue_ref,
                "evidence_refs": v.evidence_refs,
                "verified_at_sha": v.verified_at_sha,
                "lane": v.lane,
            })
        })
        .collect();
    let respec_rows: Vec<Value> = pending_respec
        .into_iter()
        .take(limit)
        .map(|v| {
            json!({
                "issue_ref": v.issue_ref,
                "evidence_refs": v.evidence_refs,
                "verified_at_sha": v.verified_at_sha,
                "lane": v.lane,
            })
        })
        .collect();

    json!({
        "pending_closure": {
            "count": closure_count,
            "items": closure_rows,
            "overflow": closure_count.saturating_sub(closure_rows.len()),
        },
        "pending_respec": {
            "count": respec_count,
            "items": respec_rows,
            "overflow": respec_count.saturating_sub(respec_rows.len()),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_server() -> MemoryServer {
        let db = std::env::temp_dir().join(format!(
            "issue-curator-test-{}.sqlite",
            uuid::Uuid::new_v4()
        ));
        MemoryServer::new(db, None).expect("test server")
    }

    fn verdict(issue_ref: &str, kind: CuratorVerdictKind, sha: &str) -> CuratorVerdict {
        CuratorVerdict {
            issue_ref: issue_ref.to_string(),
            verified_at_sha: sha.to_string(),
            verdict: kind,
            evidence_refs: vec!["o/r#980".to_string()],
            draft_comment: "draft text".to_string(),
            lane: "fake".to_string(),
            checked_at: "2026-07-11T00:00:00Z".to_string(),
        }
    }

    #[test]
    fn save_and_list_roundtrips_via_state_kv() {
        let server = test_server();
        save_curator_verdict(
            &server,
            &verdict("o/r#979", CuratorVerdictKind::FixedPendingClosure, "sha1"),
        )
        .expect("save");
        save_curator_verdict(
            &server,
            &verdict("o/r#987", CuratorVerdictKind::StillValid, "sha1"),
        )
        .expect("save");

        let all = list_curator_verdicts(&server).expect("list");
        assert_eq!(all.len(), 2);
        assert!(all
            .iter()
            .any(|v| v.issue_ref == "o/r#979"
                && v.verdict == CuratorVerdictKind::FixedPendingClosure));
    }

    #[test]
    fn rerun_same_issue_overwrites_not_duplicates() {
        let server = test_server();
        save_curator_verdict(
            &server,
            &verdict("o/r#1", CuratorVerdictKind::StillValid, "sha1"),
        )
        .expect("save first");
        save_curator_verdict(
            &server,
            &verdict("o/r#1", CuratorVerdictKind::FixedPendingClosure, "sha2"),
        )
        .expect("save second (idempotent re-run)");

        let all = list_curator_verdicts(&server).expect("list");
        assert_eq!(
            all.len(),
            1,
            "re-verifying the same issue must overwrite, not duplicate"
        );
        assert_eq!(all[0].verdict, CuratorVerdictKind::FixedPendingClosure);
        assert_eq!(all[0].verified_at_sha, "sha2");
    }

    #[test]
    fn get_single_verdict_roundtrips() {
        let server = test_server();
        save_curator_verdict(
            &server,
            &verdict("o/r#42", CuratorVerdictKind::StaleSpec, "sha1"),
        )
        .expect("save");
        let got = get_curator_verdict(&server, "o/r#42")
            .expect("get")
            .expect("present");
        assert_eq!(got.verdict, CuratorVerdictKind::StaleSpec);
    }

    #[test]
    fn get_missing_verdict_returns_none() {
        let server = test_server();
        assert!(get_curator_verdict(&server, "o/r#999").unwrap().is_none());
    }

    #[test]
    fn label_mapping_matches_1002_spec() {
        assert_eq!(CuratorVerdictKind::StillValid.as_label(), "reverified");
        assert_eq!(
            CuratorVerdictKind::FixedPendingClosure.as_label(),
            "verified-fixed"
        );
        assert_eq!(CuratorVerdictKind::StaleSpec.as_label(), "stale-spec");
    }

    #[test]
    fn briefing_queue_splits_pending_closure_and_respec_only() {
        let server = test_server();
        save_curator_verdict(
            &server,
            &verdict("o/r#979", CuratorVerdictKind::FixedPendingClosure, "sha1"),
        )
        .expect("save");
        save_curator_verdict(
            &server,
            &verdict("o/r#527", CuratorVerdictKind::StaleSpec, "sha1"),
        )
        .expect("save");
        // A still_valid verdict must NOT appear in either actionable queue —
        // there's nothing for a human to act on.
        save_curator_verdict(
            &server,
            &verdict("o/r#987", CuratorVerdictKind::StillValid, "sha1"),
        )
        .expect("save");

        let out = briefing_curator_queue(&server, 8);
        assert_eq!(out["pending_closure"]["count"], 1);
        assert_eq!(out["pending_respec"]["count"], 1);
        let closure_refs: Vec<&str> = out["pending_closure"]["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v["issue_ref"].as_str().unwrap())
            .collect();
        assert_eq!(closure_refs, vec!["o/r#979"]);
        let respec_refs: Vec<&str> = out["pending_respec"]["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v["issue_ref"].as_str().unwrap())
            .collect();
        assert_eq!(respec_refs, vec!["o/r#527"]);
    }

    #[test]
    fn briefing_queue_reports_overflow_never_silently_caps() {
        let server = test_server();
        for n in 0..5 {
            save_curator_verdict(
                &server,
                &verdict(
                    &format!("o/r#{n}"),
                    CuratorVerdictKind::FixedPendingClosure,
                    "sha1",
                ),
            )
            .expect("save");
        }
        let out = briefing_curator_queue(&server, 2);
        assert_eq!(out["pending_closure"]["count"], 5);
        assert_eq!(out["pending_closure"]["items"].as_array().unwrap().len(), 2);
        assert_eq!(out["pending_closure"]["overflow"], 3);
    }

    #[test]
    fn briefing_queue_empty_when_no_verdicts_saved() {
        let server = test_server();
        let out = briefing_curator_queue(&server, 8);
        assert_eq!(out["pending_closure"]["count"], 0);
        assert_eq!(out["pending_respec"]["count"], 0);
    }

    // ─── Finding #1's structural gate, at the `persist_curator_verdict`
    // level (unit-level red/green — `batch.rs` has the integration-level
    // pair over the whole `run_curator_batch` path). ───

    #[test]
    fn persist_refuses_real_verdict_with_empty_evidence() {
        let server = test_server();
        let err = persist_curator_verdict(
            &server,
            "o/r#1",
            "sha1",
            CuratorVerdictKind::StillValid,
            "",
            "fake",
        )
        .expect_err("must refuse a real verdict with empty evidence");
        assert!(err.contains("refusing to persist"));
        // Nothing should have been written.
        assert!(get_curator_verdict(&server, "o/r#1").unwrap().is_none());
    }

    #[test]
    fn persist_refuses_fixed_pending_closure_with_whitespace_only_evidence() {
        let server = test_server();
        let err = persist_curator_verdict(
            &server,
            "o/r#2",
            "sha1",
            CuratorVerdictKind::FixedPendingClosure,
            "   \n\t  ",
            "fake",
        )
        .expect_err("whitespace-only evidence must not count as real evidence");
        assert!(err.contains("refusing to persist"));
    }

    #[test]
    fn persist_allows_pending_evidence_with_empty_evidence() {
        let server = test_server();
        let row = persist_curator_verdict(
            &server,
            "o/r#3",
            "sha1",
            CuratorVerdictKind::PendingEvidence,
            "",
            "fake",
        )
        .expect("pending_evidence must be persistable without evidence");
        assert_eq!(row.verdict, CuratorVerdictKind::PendingEvidence);
        assert!(!row.verdict.is_real_verdict());
        let stored = get_curator_verdict(&server, "o/r#3")
            .unwrap()
            .expect("row persisted");
        assert_eq!(stored.verdict, CuratorVerdictKind::PendingEvidence);
    }

    #[test]
    fn persist_allows_lane_failed_with_empty_evidence() {
        let server = test_server();
        let row = persist_curator_verdict(
            &server,
            "o/r#4",
            "sha1",
            CuratorVerdictKind::LaneFailed,
            "",
            "fake",
        )
        .expect("lane_failed must be persistable without evidence");
        assert_eq!(row.verdict, CuratorVerdictKind::LaneFailed);
        assert!(!row.verdict.is_real_verdict());
    }

    #[test]
    fn persist_accepts_real_verdict_with_non_empty_evidence() {
        let server = test_server();
        let row = persist_curator_verdict(
            &server,
            "o/r#5",
            "sha1",
            CuratorVerdictKind::StaleSpec,
            "src/x.rs:1 supersedes the proposed fix",
            "fake",
        )
        .expect("real verdict with real evidence must persist");
        assert_eq!(row.verdict, CuratorVerdictKind::StaleSpec);
        assert!(row.verdict.is_real_verdict());
        assert_eq!(row.evidence_refs.len(), 1);
    }

    #[test]
    fn pending_evidence_and_lane_failed_never_enter_actionable_briefing_queues() {
        let server = test_server();
        persist_curator_verdict(
            &server,
            "o/r#6",
            "sha1",
            CuratorVerdictKind::PendingEvidence,
            "",
            "fake",
        )
        .expect("save");
        persist_curator_verdict(
            &server,
            "o/r#7",
            "sha1",
            CuratorVerdictKind::LaneFailed,
            "",
            "fake",
        )
        .expect("save");
        let out = briefing_curator_queue(&server, 8);
        assert_eq!(out["pending_closure"]["count"], 0);
        assert_eq!(out["pending_respec"]["count"], 0);
    }
}
