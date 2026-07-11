//! Live entry point wiring `curator_ops::run_curator_batch` to the
//! `tachi_gh(action='issue_curator_batch')` facade action.
//!
//! Candidate sourcing: an explicit `curator_issue_refs` list overrides;
//! otherwise candidates default to the #1000 freshness layer's already
//! stored verdict rows (zombie hits + stale-candidate heuristics) so the
//! curator re-verifies exactly what the mechanical scan already flagged as
//! worth a second look — no re-scanning GitHub from scratch.
//!
//! Writeback (comment + label) is gated behind `curator_post_writeback`,
//! default `false` (#1002 non-goal: curator never auto-writes without an
//! explicit per-run opt-in), and reuses the *existing* `issue_comment` /
//! `issue_label` handlers verbatim rather than issuing raw `gh` calls here.

use super::batch::{run_curator_batch, CuratorBatchParams, CuratorCandidate};
use super::runner::DispatchLaneRunner;
use super::verdict::CuratorVerdictKind;
use crate::server_state::MemoryServer;
use serde_json::json;
use tachi_params::TachiGhParams;

fn head_sha() -> String {
    std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}

fn default_candidates(server: &MemoryServer, repo: &str) -> Vec<CuratorCandidate> {
    // Reuse #1000's already-stored freshness rows (state_kv, namespace
    // `issue_freshness`) as the candidate source — the mechanical layer's
    // job is exactly "surface candidates," the curator's job is exactly
    // "semantically re-verify them" (#1000/#1002 division of labor).
    crate::gh_ops::list_freshness_verdicts(server)
        .unwrap_or_default()
        .into_iter()
        .filter(|v| v.issue_ref.starts_with(repo))
        .map(|v| CuratorCandidate {
            issue_ref: v.issue_ref,
            issue_summary: format!("freshness scan verdict: {}", v.verdict),
            evidence_on_file: v.evidence_refs,
        })
        .collect()
}

pub(crate) async fn handle_issue_curator_batch(
    server: &MemoryServer,
    params: TachiGhParams,
) -> Result<String, String> {
    let repo = params
        .repo
        .clone()
        .ok_or_else(|| "issue_curator_batch requires 'repo' parameter".to_string())?;

    let candidates = if !params.curator_issue_refs.is_empty() {
        params
            .curator_issue_refs
            .iter()
            .map(|issue_ref| CuratorCandidate {
                issue_ref: issue_ref.clone(),
                issue_summary: "explicit caller-supplied candidate".to_string(),
                evidence_on_file: Vec::new(),
            })
            .collect()
    } else {
        default_candidates(server, &repo)
    };

    if candidates.is_empty() {
        return serde_json::to_string(&json!({
            "tool": "tachi_gh_issue_curator_batch",
            "repo": repo,
            "verified": [],
            "skipped": [],
            "total_cost_tokens": 0,
            "budget_tokens": params.curator_budget_tokens,
            "note": "no candidates: pass curator_issue_refs explicitly, or run issue_freshness_scan first",
        }))
        .map_err(|e| format!("serialize: {e}"));
    }

    let profile = params
        .curator_profile
        .clone()
        .unwrap_or_else(|| "codex_55_review".to_string());
    let runner = DispatchLaneRunner::new(profile);

    let batch_params = CuratorBatchParams {
        candidates,
        head_sha: head_sha(),
        budget_tokens: params.curator_budget_tokens,
        skip_if_fresh: true,
    };

    let report = run_curator_batch(server, &runner, batch_params).await?;

    // Writeback: gated, best-effort, reuses the existing issue_comment /
    // issue_label handlers verbatim (#1002: no new GitHub write primitive
    // for the curator itself — only the orchestration is new).
    let mut writeback_results = Vec::new();
    if params.curator_post_writeback {
        for row in &report.verified {
            let Ok(Some(stored)) = super::verdict::get_curator_verdict(server, &row.issue_ref)
            else {
                continue;
            };
            let number = stored
                .issue_ref
                .rsplit('#')
                .next()
                .and_then(|n| n.parse::<u64>().ok());
            let Some(number) = number else {
                writeback_results.push(json!({
                    "issue_ref": row.issue_ref,
                    "error": "could not parse issue number from issue_ref",
                }));
                continue;
            };

            let comment_result = crate::gh_ops::handle_gh_comment(
                server,
                "issue",
                tachi_params::GhCommentParams {
                    repo: repo.clone(),
                    number,
                    body: Some(stored.draft_comment.clone()),
                    dry_run: false,
                },
            )
            .await;

            let label_result = crate::gh_ops::handle_gh_label(
                server,
                tachi_params::GhLabelParams {
                    repo: repo.clone(),
                    number,
                    labels: vec![row.verdict.as_label().to_string()],
                    mode: Some("add".to_string()),
                },
            )
            .await;

            writeback_results.push(json!({
                "issue_ref": row.issue_ref,
                "comment_posted": comment_result.is_ok(),
                "comment_error": comment_result.err(),
                "label_applied": label_result.is_ok(),
                "label_error": label_result.err(),
            }));
        }
    }

    serde_json::to_string(&json!({
        "tool": "tachi_gh_issue_curator_batch",
        "repo": repo,
        "verified": report.verified.iter().map(|r| json!({
            "issue_ref": r.issue_ref,
            "verdict": verdict_label(r.verdict),
            "lane": r.lane,
            "cost_tokens": r.cost_tokens,
        })).collect::<Vec<_>>(),
        "skipped": report.skipped.iter().map(|(id, reason)| json!({
            "issue_ref": id,
            "reason": reason,
        })).collect::<Vec<_>>(),
        "total_cost_tokens": report.total_cost_tokens,
        "budget_tokens": report.budget_tokens,
        "post_writeback": params.curator_post_writeback,
        "writeback_results": writeback_results,
    }))
    .map_err(|e| format!("serialize: {e}"))
}

fn verdict_label(kind: CuratorVerdictKind) -> &'static str {
    match kind {
        CuratorVerdictKind::StillValid => "still_valid",
        CuratorVerdictKind::FixedPendingClosure => "fixed_pending_closure",
        CuratorVerdictKind::StaleSpec => "stale_spec",
    }
}
