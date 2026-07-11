//! Live entry point wiring `curator_ops::run_curator_batch` to the
//! `tachi_gh(action='issue_curator_batch')` facade action.
//!
//! Candidate sourcing: an explicit `curator_issue_refs` list overrides;
//! otherwise candidates default to the #1000 freshness layer's already
//! stored verdict rows (zombie hits + stale-candidate heuristics) so the
//! curator re-verifies exactly what the mechanical scan already flagged as
//! worth a second look — no re-scanning GitHub from scratch.
//!
//! Packet enrichment (codex review Finding #2, BUG): every candidate — both
//! explicit `curator_issue_refs` and freshness-sourced ones — gets its full
//! issue body + comments fetched via the SAME `gh issue view --json
//! body,comments` idiom `gh_ops::issues::handle_gh_issue_read` already uses
//! (reused verbatim, not reinvented here), plus `path:line` anchors
//! extracted from that text via the existing `gh_ops::issue_freshness`
//! extractors. `enrich_candidate` is the pure half (fixture-tested, no live
//! `gh` call); `fetch_issue_body_and_anchors` is the thin async wrapper that
//! does the actual `gh` call and is NOT exercised by this crate's test
//! suite (matching the #1002 "no live GitHub call off this crate's tests"
//! contract already established for `DispatchLaneRunner`).
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
            issue_body: String::new(),
            file_line_anchors: Vec::new(),
        })
        .collect()
}

/// Extracts the issue number out of an `issue_ref` of the form
/// `owner/repo#123` (or a bare URL ending in `#123`) — same convention
/// `entry.rs`'s writeback path already uses (`rsplit('#')`).
fn parse_issue_number(issue_ref: &str) -> Option<u64> {
    issue_ref.rsplit('#').next()?.parse::<u64>().ok()
}

/// Pure half of Finding #2's fix: given the already-fetched `gh issue view
/// --json body,comments` response (as `handle_gh_issue_read` returns it —
/// `{"result": {"body": ..., "comments": [{"body": ...}, ...]}}`),
/// concatenates body + comment text and extracts `path:line` anchors from
/// the combined text. Fixture-testable with a hand-built `serde_json::Value`
/// — no live `gh` call needed to exercise this.
fn enrich_candidate_from_issue_json(
    candidate: &mut CuratorCandidate,
    issue_json: &serde_json::Value,
) {
    let result = issue_json.get("result").unwrap_or(issue_json);
    let body = result.get("body").and_then(|b| b.as_str()).unwrap_or("");
    let comments_text: String = result
        .get("comments")
        .and_then(|c| c.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|c| c.get("body").and_then(|b| b.as_str()))
                .collect::<Vec<_>>()
                .join("\n\n")
        })
        .unwrap_or_default();

    let full_text = if comments_text.is_empty() {
        body.to_string()
    } else {
        format!("{body}\n\n---comments---\n{comments_text}")
    };

    candidate.file_line_anchors = crate::gh_ops::extract_file_line_anchors(&full_text);
    candidate.issue_body = full_text;
}

/// Thin async wrapper: fetches one issue's full body + comments via the
/// existing `gh_ops::handle_gh_issue_read` handler (same `gh` idiom the
/// #1000 freshness layer/parent branch already uses) and enriches the
/// candidate in place. Best-effort — a fetch failure (e.g. no network, no
/// `gh` auth) leaves `issue_body`/`file_line_anchors` empty rather than
/// failing the whole batch; `build_packet` already renders that case
/// explicitly ("(issue body unavailable...)"), so the lane still knows
/// evidence was missing rather than silently getting a truncated packet.
async fn fetch_issue_body_and_anchors(
    server: &MemoryServer,
    repo: &str,
    candidate: &mut CuratorCandidate,
) {
    let Some(issue_number) = parse_issue_number(&candidate.issue_ref) else {
        return;
    };
    let raw = crate::gh_ops::handle_gh_issue_read(
        server,
        tachi_params::GhIssueReadParams {
            repo: repo.to_string(),
            issue_number,
        },
    )
    .await;
    let Ok(raw) = raw else {
        return;
    };
    let Ok(issue_json) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return;
    };
    enrich_candidate_from_issue_json(candidate, &issue_json);
}

pub(crate) async fn handle_issue_curator_batch(
    server: &MemoryServer,
    params: TachiGhParams,
) -> Result<String, String> {
    let repo = params
        .repo
        .clone()
        .ok_or_else(|| "issue_curator_batch requires 'repo' parameter".to_string())?;

    let mut candidates: Vec<CuratorCandidate> = if !params.curator_issue_refs.is_empty() {
        params
            .curator_issue_refs
            .iter()
            .map(|issue_ref| CuratorCandidate {
                issue_ref: issue_ref.clone(),
                issue_summary: "explicit caller-supplied candidate".to_string(),
                evidence_on_file: Vec::new(),
                issue_body: String::new(),
                file_line_anchors: Vec::new(),
            })
            .collect()
    } else {
        default_candidates(server, &repo)
    };

    // Finding #2 fix: every candidate — explicit AND freshness-sourced —
    // gets its full issue body + comments fetched before the packet is
    // built, so the lane sees the actual issue text and complete anchors,
    // not just a synthetic summary.
    for candidate in &mut candidates {
        fetch_issue_body_and_anchors(server, &repo, candidate).await;
    }

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
    // for the curator itself — only the orchestration is new). Finding #1's
    // gate extends here too: `pending_evidence`/`lane_failed` rows are NOT
    // real verdicts and must never be posted to GitHub as one — only rows
    // whose verdict is a real, evidence-backed tier get written back.
    let mut writeback_results = Vec::new();
    if params.curator_post_writeback {
        for row in report
            .verified
            .iter()
            .filter(|r| r.verdict.is_real_verdict())
        {
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
        CuratorVerdictKind::PendingEvidence => "pending_evidence",
        CuratorVerdictKind::LaneFailed => "lane_failed",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parse_issue_number_handles_owner_repo_hash_number() {
        assert_eq!(parse_issue_number("kckylechen1/tachi#1002"), Some(1002));
    }

    #[test]
    fn parse_issue_number_returns_none_for_malformed_ref() {
        assert_eq!(parse_issue_number("not-a-ref"), None);
    }

    #[test]
    fn enrich_candidate_extracts_body_and_comments_and_anchors() {
        // Finding #2 (BUG): the packet builder must fetch the full issue
        // body + comments and extract file:line anchors from that text —
        // fixture-tested here against a hand-built `gh issue view --json
        // body,comments` response shape (no live `gh` call).
        let issue_json = json!({
            "tool": "tachi_gh_issue_read",
            "repo": "o/r",
            "issue_number": 42,
            "result": {
                "body": "The bug is at curator_ops/runner.rs:107 in the reverify fn.",
                "comments": [
                    {"body": "Confirmed, also see curator_ops/batch.rs:87 for the classifier."},
                ],
            },
        });
        let mut candidate = CuratorCandidate {
            issue_ref: "o/r#42".to_string(),
            issue_summary: "synthetic".to_string(),
            evidence_on_file: Vec::new(),
            issue_body: String::new(),
            file_line_anchors: Vec::new(),
        };

        enrich_candidate_from_issue_json(&mut candidate, &issue_json);

        assert!(candidate
            .issue_body
            .contains("The bug is at curator_ops/runner.rs:107"));
        assert!(candidate
            .issue_body
            .contains("Confirmed, also see curator_ops/batch.rs:87"));
        assert!(candidate
            .file_line_anchors
            .iter()
            .any(|(path, line)| path == "curator_ops/runner.rs" && *line == 107));
        assert!(candidate
            .file_line_anchors
            .iter()
            .any(|(path, line)| path == "curator_ops/batch.rs" && *line == 87));
    }

    #[test]
    fn enrich_candidate_handles_missing_comments_field() {
        let issue_json = json!({
            "result": {
                "body": "no anchors here",
            },
        });
        let mut candidate = CuratorCandidate {
            issue_ref: "o/r#43".to_string(),
            issue_summary: "synthetic".to_string(),
            evidence_on_file: Vec::new(),
            issue_body: String::new(),
            file_line_anchors: Vec::new(),
        };

        enrich_candidate_from_issue_json(&mut candidate, &issue_json);

        assert_eq!(candidate.issue_body, "no anchors here");
        assert!(candidate.file_line_anchors.is_empty());
    }

    #[test]
    fn verdict_label_covers_all_five_kinds() {
        assert_eq!(verdict_label(CuratorVerdictKind::StillValid), "still_valid");
        assert_eq!(
            verdict_label(CuratorVerdictKind::FixedPendingClosure),
            "fixed_pending_closure"
        );
        assert_eq!(verdict_label(CuratorVerdictKind::StaleSpec), "stale_spec");
        assert_eq!(
            verdict_label(CuratorVerdictKind::PendingEvidence),
            "pending_evidence"
        );
        assert_eq!(verdict_label(CuratorVerdictKind::LaneFailed), "lane_failed");
    }
}
