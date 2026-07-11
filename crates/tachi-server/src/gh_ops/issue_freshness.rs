//! Issue freshness layer (#1000): zombie scan + stale-candidate heuristics.
//!
//! GitHub remains the only source of truth for issue *content* — this module
//! never mirrors issue bodies. It only ever produces judgment-free verdict
//! rows: `(issue_ref, verified_at_sha, verdict, evidence_refs)`. Durable
//! conclusions write back to GitHub's own medium (label/comment) so they are
//! visible without a Tachi consumer. Closing an issue is always a
//! leader/owner action — this module never auto-closes anything.
//!
//! Two independent detectors, both read-only:
//!   - `scan_zombies`: reverse-scans merged PR bodies/titles for `Refs #N` /
//!     `(#N)` references, cross-checked against the open-issue set. A hit
//!     means "a merged PR says it fixed #N, and #N is still open" — the
//!     mechanical prototype of what the leader did by hand on 2026-07-11.
//!   - `scan_stale_candidates`: heuristic-only (never a verdict) — issues
//!     whose referenced file:line anchors have vanished from HEAD, or whose
//!     referenced gate issues are already closed. Produces a review queue,
//!     not a judgment.

use super::*;

/// One "already fixed, still open" hit: a merged PR/commit referenced the
/// issue but nobody closed it.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ZombieHit {
    pub issue_number: u64,
    pub pr_number: u64,
    pub pr_title: String,
    pub merge_commit_sha: Option<String>,
}

/// Minimal shape of a merged PR needed for the reverse scan — deliberately
/// narrow so fixture data (tests) and live `gh pr list --json` output share
/// the same struct.
#[derive(Debug, Clone, Default)]
pub(crate) struct MergedPr {
    pub number: u64,
    pub title: String,
    pub body: String,
    pub merge_commit_sha: Option<String>,
}

/// Extract issue numbers referenced via `Refs #N`, `Ref #N`, `Refs #N, #M`,
/// or `(#N)` from PR title+body text. Case-insensitive on the `Ref(s)` word;
/// `#N` and `(#N)` forms are literal. Does not distinguish "Refs" (our
/// judgment) from "Fixes"/"Closes" (GitHub's own auto-close keywords) — a
/// commit closed via `Fixes #N` already closes the issue on merge, so it
/// would never show up as an open zombie regardless of which keyword matched.
pub(crate) fn extract_referenced_issue_numbers(text: &str) -> Vec<u64> {
    let mut out = Vec::new();
    let bytes = text.as_bytes();
    let lower = text.to_ascii_lowercase();
    let mut i = 0usize;
    while i < lower.len() {
        // "ref" / "refs" / "reference" / "references" / "referenced" followed
        // by (optional colon/space run) then one or more "#N" tokens.
        let matched_kw = ["references", "referenced", "reference", "refs", "ref"]
            .iter()
            .find(|kw| lower[i..].starts_with(**kw));
        if let Some(kw) = matched_kw {
            let mut j = i + kw.len();
            // require a word boundary after the keyword (not "refactor")
            let boundary_ok = bytes
                .get(j)
                .map(|b| !b.is_ascii_alphanumeric())
                .unwrap_or(true);
            if boundary_ok {
                // consume separators (space, colon, comma) then repeated "#N"
                let mut consumed_any = false;
                loop {
                    while bytes
                        .get(j)
                        .is_some_and(|b| matches!(b, b' ' | b':' | b',' | b'\t'))
                    {
                        j += 1;
                    }
                    if bytes.get(j) == Some(&b'#') {
                        let start = j + 1;
                        let mut end = start;
                        while bytes.get(end).is_some_and(u8::is_ascii_digit) {
                            end += 1;
                        }
                        if end > start {
                            if let Ok(n) = text[start..end].parse::<u64>() {
                                out.push(n);
                                consumed_any = true;
                            }
                            j = end;
                            continue;
                        }
                    }
                    break;
                }
                if consumed_any {
                    i = j;
                    continue;
                }
            }
        }
        i += 1;
    }
    // "(#N)" form, independent of the "Refs" keyword (e.g. GitHub auto-link
    // style used in some commit trailers).
    let mut k = 0usize;
    let chars: Vec<char> = text.chars().collect();
    while k < chars.len() {
        if chars[k] == '(' && chars.get(k + 1) == Some(&'#') {
            let start = k + 2;
            let mut end = start;
            while chars.get(end).is_some_and(|c| c.is_ascii_digit()) {
                end += 1;
            }
            if end > start && chars.get(end) == Some(&')') {
                let digits: String = chars[start..end].iter().collect();
                if let Ok(n) = digits.parse::<u64>() {
                    out.push(n);
                }
            }
        }
        k += 1;
    }
    out.sort_unstable();
    out.dedup();
    out
}

/// Cross-check merged PRs against the open-issue set: any issue number
/// referenced by a merged PR that is still in `open_issue_numbers` is a
/// zombie (fixed, never closed). Pure function — no I/O — so it is directly
/// fixture-testable against the #979/#947 acceptance anchor without hitting
/// live GitHub.
pub(crate) fn scan_zombies(merged_prs: &[MergedPr], open_issue_numbers: &[u64]) -> Vec<ZombieHit> {
    let mut hits = Vec::new();
    for pr in merged_prs {
        let text = format!("{}\n{}", pr.title, pr.body);
        for issue_number in extract_referenced_issue_numbers(&text) {
            if open_issue_numbers.contains(&issue_number) {
                hits.push(ZombieHit {
                    issue_number,
                    pr_number: pr.number,
                    pr_title: pr.title.clone(),
                    merge_commit_sha: pr.merge_commit_sha.clone(),
                });
            }
        }
    }
    hits.sort_by(|a, b| {
        a.issue_number
            .cmp(&b.issue_number)
            .then(a.pr_number.cmp(&b.pr_number))
    });
    hits.dedup();
    hits
}

/// One stale-spec candidate: NOT a verdict, just a reason to re-read the
/// issue against current HEAD.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct StaleCandidate {
    pub issue_number: u64,
    pub reason: String,
    pub evidence: Vec<String>,
}

/// Minimal open-issue shape used by the stale-candidate heuristics.
#[derive(Debug, Clone, Default)]
pub(crate) struct OpenIssueForStaleCheck {
    pub number: u64,
    /// `path:line` anchors parsed out of the issue body (already extracted
    /// by the caller — this module does not parse markdown).
    pub file_line_anchors: Vec<(String, u64)>,
    /// Gate/dependency issue numbers this issue's body references as blockers.
    pub gate_issue_numbers: Vec<u64>,
}

/// Heuristic-only stale-candidate scan (Scope item 2). Two independent
/// signals, either one is enough to surface a candidate:
///   - a referenced `file:line` anchor no longer exists at that line count
///     (or the file itself is gone) on HEAD;
///   - a referenced gate/dependency issue is already closed.
///
/// `existing_file_line_counts` maps repo-relative path -> current line count
/// (None means file does not exist on HEAD). `closed_issue_numbers` is the
/// set of issue numbers known to be closed. Never produces a verdict —
/// only a reason + evidence to re-read the issue.
pub(crate) fn scan_stale_candidates(
    issues: &[OpenIssueForStaleCheck],
    existing_file_line_counts: &std::collections::HashMap<String, Option<u64>>,
    closed_issue_numbers: &[u64],
) -> Vec<StaleCandidate> {
    let mut out = Vec::new();
    for issue in issues {
        let mut evidence = Vec::new();
        for (path, line) in &issue.file_line_anchors {
            match existing_file_line_counts.get(path) {
                Some(Some(current_lines)) if line > current_lines => {
                    evidence.push(format!(
                        "{path}:{line} anchor exceeds current file length ({current_lines} lines)"
                    ));
                }
                Some(None) | None if existing_file_line_counts.contains_key(path) => {
                    evidence.push(format!("{path} no longer exists on HEAD"));
                }
                _ => {}
            }
        }
        for gate in &issue.gate_issue_numbers {
            if closed_issue_numbers.contains(gate) {
                evidence.push(format!("gate dependency #{gate} is already closed"));
            }
        }
        if !evidence.is_empty() {
            let reason = if evidence.len() > 1 {
                "anchor drift + gate closure".to_string()
            } else {
                evidence[0].clone()
            };
            out.push(StaleCandidate {
                issue_number: issue.number,
                reason,
                evidence,
            });
        }
    }
    out.sort_by_key(|c| c.issue_number);
    out
}

/// Fetch merged PRs for `repo` via `gh pr list --state merged` and run the
/// zombie scan against the currently-open issue numbers. `limit` bounds how
/// many merged PRs are fetched (most-recently-merged first, per `gh`'s
/// default ordering). Live-GitHub I/O boundary — kept thin so the scan logic
/// above stays independently fixture-tested.
pub(crate) fn fetch_and_scan_zombies(
    server: &MemoryServer,
    repo: &str,
    limit: u32,
) -> Result<Vec<ZombieHit>, String> {
    let open_issue_numbers = fetch_open_issue_numbers(server, repo)?;
    if open_issue_numbers.is_empty() {
        return Ok(Vec::new());
    }
    let merged_prs = fetch_merged_prs(server, repo, limit)?;
    Ok(scan_zombies(&merged_prs, &open_issue_numbers))
}

fn fetch_open_issue_numbers(server: &MemoryServer, repo: &str) -> Result<Vec<u64>, String> {
    let (mut cmd, token) = build_gh_command(server)?;
    cmd.args(["issue", "list"])
        .args(["--repo", repo])
        .args(["--state", "open"])
        .args(["--limit", "500"])
        .args(["--json", "number"]);
    let output = run_gh_json(cmd, &token)?;
    let value: Value =
        serde_json::from_str(&output).map_err(|e| format!("parse issue list json: {e}"))?;
    let numbers = value
        .as_array()
        .map(|rows| {
            rows.iter()
                .filter_map(|r| r.get("number").and_then(Value::as_u64))
                .collect()
        })
        .unwrap_or_default();
    Ok(numbers)
}

fn fetch_merged_prs(
    server: &MemoryServer,
    repo: &str,
    limit: u32,
) -> Result<Vec<MergedPr>, String> {
    let (mut cmd, token) = build_gh_command(server)?;
    cmd.args(["pr", "list"])
        .args(["--repo", repo])
        .args(["--state", "merged"])
        .args(["--limit", &limit.to_string()])
        .args(["--json", "number,title,body,mergeCommit"]);
    let output = run_gh_json(cmd, &token)?;
    let value: Value =
        serde_json::from_str(&output).map_err(|e| format!("parse pr list json: {e}"))?;
    let prs = value
        .as_array()
        .map(|rows| {
            rows.iter()
                .filter_map(|r| {
                    let number = r.get("number").and_then(Value::as_u64)?;
                    Some(MergedPr {
                        number,
                        title: r
                            .get("title")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                        body: r
                            .get("body")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                        merge_commit_sha: r
                            .get("mergeCommit")
                            .and_then(|mc| mc.get("oid"))
                            .and_then(Value::as_str)
                            .map(str::to_string),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    Ok(prs)
}

/// Extract `path:line` anchors from issue body text (e.g. `crates/foo/src/bar.rs:123`).
/// Conservative: requires a `.rs`/`.ts`/`.py`/`.md` (or similar source-like)
/// extension before the `:line` to avoid false positives on things like URLs
/// or timestamps.
pub(crate) fn extract_file_line_anchors(text: &str) -> Vec<(String, u64)> {
    let mut out = Vec::new();
    let bytes = text.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == b':' {
            // scan backwards for a path-like token (letters/digits/_/./\-//)
            let mut start = i;
            while start > 0
                && matches!(bytes[start - 1], b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'_' | b'.' | b'/' | b'-')
            {
                start -= 1;
            }
            let candidate = &text[start..i];
            let has_src_ext = [".rs", ".ts", ".tsx", ".py", ".md", ".js", ".go"]
                .iter()
                .any(|ext| candidate.ends_with(ext));
            if has_src_ext && candidate.contains('/') {
                let mut end = i + 1;
                while bytes.get(end).is_some_and(u8::is_ascii_digit) {
                    end += 1;
                }
                if end > i + 1 {
                    if let Ok(line) = text[i + 1..end].parse::<u64>() {
                        out.push((candidate.to_string(), line));
                    }
                    i = end;
                    continue;
                }
            }
        }
        i += 1;
    }
    out
}

/// Live entry point for the stale-candidate scan (Scope item 2). Fetches
/// open issue bodies, parses anchors, checks each anchored file's current
/// line count under `repo_root`, and cross-checks referenced gate issues
/// against the closed set. Heuristic-only — callers must not treat the
/// result as a verdict (no `save_freshness_verdict` call here); it is a
/// review queue, same posture as `scan_open_loops`'s `spec_drift` kind.
pub(crate) fn fetch_and_scan_stale_candidates(
    server: &MemoryServer,
    repo: &str,
    repo_root: &std::path::Path,
    limit: u32,
) -> Result<Vec<StaleCandidate>, String> {
    let open = fetch_open_issues_with_body(server, repo, limit)?;
    let closed_issue_numbers = fetch_closed_issue_numbers(server, repo)?;

    let mut file_lines: std::collections::HashMap<String, Option<u64>> =
        std::collections::HashMap::new();
    let mut issues = Vec::with_capacity(open.len());
    for (number, body) in &open {
        let anchors = extract_file_line_anchors(body);
        for (path, _) in &anchors {
            file_lines.entry(path.clone()).or_insert_with(|| {
                std::fs::read_to_string(repo_root.join(path))
                    .ok()
                    .map(|contents| contents.lines().count() as u64)
            });
        }
        let gates = extract_referenced_issue_numbers(body);
        issues.push(OpenIssueForStaleCheck {
            number: *number,
            file_line_anchors: anchors,
            gate_issue_numbers: gates,
        });
    }
    Ok(scan_stale_candidates(
        &issues,
        &file_lines,
        &closed_issue_numbers,
    ))
}

fn fetch_open_issues_with_body(
    server: &MemoryServer,
    repo: &str,
    limit: u32,
) -> Result<Vec<(u64, String)>, String> {
    let (mut cmd, token) = build_gh_command(server)?;
    cmd.args(["issue", "list"])
        .args(["--repo", repo])
        .args(["--state", "open"])
        .args(["--limit", &limit.to_string()])
        .args(["--json", "number,body"]);
    let output = run_gh_json(cmd, &token)?;
    let value: Value =
        serde_json::from_str(&output).map_err(|e| format!("parse issue list json: {e}"))?;
    Ok(value
        .as_array()
        .map(|rows| {
            rows.iter()
                .filter_map(|r| {
                    let number = r.get("number").and_then(Value::as_u64)?;
                    let body = r.get("body").and_then(Value::as_str).unwrap_or_default();
                    Some((number, body.to_string()))
                })
                .collect()
        })
        .unwrap_or_default())
}

fn fetch_closed_issue_numbers(server: &MemoryServer, repo: &str) -> Result<Vec<u64>, String> {
    let (mut cmd, token) = build_gh_command(server)?;
    cmd.args(["issue", "list"])
        .args(["--repo", repo])
        .args(["--state", "closed"])
        .args(["--limit", "500"])
        .args(["--json", "number"]);
    let output = run_gh_json(cmd, &token)?;
    let value: Value =
        serde_json::from_str(&output).map_err(|e| format!("parse issue list json: {e}"))?;
    Ok(value
        .as_array()
        .map(|rows| {
            rows.iter()
                .filter_map(|r| r.get("number").and_then(Value::as_u64))
                .collect()
        })
        .unwrap_or_default())
}

// ─── Verdict storage (state_kv, per scouted precedent) ─────────────────────

pub(crate) const ISSUE_FRESHNESS_NS: &str = "issue_freshness";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct FreshnessVerdict {
    pub issue_ref: String,
    pub verified_at_sha: String,
    /// "zombie" | "stale_candidate"
    pub verdict: String,
    pub evidence_refs: Vec<String>,
    pub checked_at: String,
}

fn issue_freshness_key(issue_ref: &str) -> String {
    format!("verdict:{issue_ref}")
}

pub(crate) fn save_freshness_verdict(
    server: &MemoryServer,
    verdict: &FreshnessVerdict,
) -> Result<(), String> {
    let json = serde_json::to_string(verdict).map_err(|e| format!("serialize verdict: {e}"))?;
    server.with_global_store(|store| -> Result<(), String> {
        store
            .set_state(
                ISSUE_FRESHNESS_NS,
                &issue_freshness_key(&verdict.issue_ref),
                &json,
            )
            .map_err(|e| format!("issue_freshness set_state: {e}"))?;
        Ok(())
    })
}

pub(crate) fn list_freshness_verdicts(
    server: &MemoryServer,
) -> Result<Vec<FreshnessVerdict>, String> {
    let rows = server.with_global_store_read(|store| {
        store
            .list_state(ISSUE_FRESHNESS_NS)
            .map_err(|e| format!("issue_freshness list_state: {e}"))
    })?;
    Ok(rows
        .into_iter()
        .filter_map(|row| serde_json::from_str::<FreshnessVerdict>(&row.value_json).ok())
        .collect())
}

use serde::{Deserialize, Serialize};

/// Briefing projection (Scope item 3): both queues, capped, counts +
/// overflow markers — never a silent cap (same convention as
/// `scan_open_loops`). Reads only the already-stored verdict rows
/// (`state_kv`) — this function does not call `gh` itself; population of
/// those rows happens via `issue_freshness_scan` (a `tachi_gh` action) so
/// briefing reads stay cheap and offline-safe.
pub(crate) fn briefing_freshness_queues(server: &MemoryServer, limit: usize) -> Value {
    let verdicts = list_freshness_verdicts(server).unwrap_or_default();
    let mut zombies: Vec<&FreshnessVerdict> =
        verdicts.iter().filter(|v| v.verdict == "zombie").collect();
    let mut stale: Vec<&FreshnessVerdict> = verdicts
        .iter()
        .filter(|v| v.verdict == "stale_candidate")
        .collect();
    zombies.sort_by(|a, b| a.issue_ref.cmp(&b.issue_ref));
    stale.sort_by(|a, b| a.issue_ref.cmp(&b.issue_ref));

    let zombie_count = zombies.len();
    let stale_count = stale.len();
    let zombie_rows: Vec<Value> = zombies
        .into_iter()
        .take(limit)
        .map(|v| {
            json!({
                "issue_ref": v.issue_ref,
                "evidence_refs": v.evidence_refs,
                "verified_at_sha": v.verified_at_sha,
            })
        })
        .collect();
    let stale_rows: Vec<Value> = stale
        .into_iter()
        .take(limit)
        .map(|v| {
            json!({
                "issue_ref": v.issue_ref,
                "evidence_refs": v.evidence_refs,
                "verified_at_sha": v.verified_at_sha,
            })
        })
        .collect();

    json!({
        "zombies": {
            "count": zombie_count,
            "items": zombie_rows,
            "overflow": zombie_count.saturating_sub(zombie_rows.len()),
        },
        "stale_candidates": {
            "count": stale_count,
            "items": stale_rows,
            "overflow": stale_count.saturating_sub(stale_rows.len()),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pr(number: u64, title: &str, body: &str) -> MergedPr {
        MergedPr {
            number,
            title: title.to_string(),
            body: body.to_string(),
            merge_commit_sha: Some(format!("sha-{number}")),
        }
    }

    #[test]
    fn extracts_refs_hash_n_form() {
        let text = "Fixes the bug.\n\nRefs #979";
        assert_eq!(extract_referenced_issue_numbers(text), vec![979]);
    }

    #[test]
    fn extracts_refs_multiple_comma_separated() {
        let text = "Refs #530, #724, #921";
        assert_eq!(extract_referenced_issue_numbers(text), vec![530, 724, 921]);
    }

    #[test]
    fn extracts_refs_multiple_space_separated() {
        let text = "Refs #968 #517 #963";
        assert_eq!(extract_referenced_issue_numbers(text), vec![517, 963, 968]);
    }

    #[test]
    fn extracts_paren_hash_n_form() {
        let text = "some commit trailer (#42) landed";
        assert_eq!(extract_referenced_issue_numbers(text), vec![42]);
    }

    #[test]
    fn extracts_ref_singular_and_reference_forms() {
        assert_eq!(extract_referenced_issue_numbers("Ref #1"), vec![1]);
        assert_eq!(extract_referenced_issue_numbers("Reference: #2"), vec![2]);
        assert_eq!(extract_referenced_issue_numbers("References #3"), vec![3]);
        assert_eq!(extract_referenced_issue_numbers("Referenced #4"), vec![4]);
    }

    #[test]
    fn does_not_match_refactor_as_ref_keyword() {
        // "Refactor #5" must not match on "Ref" + word-boundary check.
        assert_eq!(
            extract_referenced_issue_numbers("Refactor #5"),
            Vec::<u64>::new()
        );
    }

    #[test]
    fn no_refs_keyword_and_no_parens_yields_empty() {
        assert_eq!(
            extract_referenced_issue_numbers("just a plain body #5"),
            Vec::<u64>::new()
        );
    }

    /// Acceptance anchor from #1000: a replay against this repo's real
    /// 2026-07-11 state must catch #979 and #947 as zombies (fix PRs
    /// #980/#981 merged while issues were open). Fixture data mirrors the
    /// real PR bodies (`gh pr view 980/981 --json body`) captured before
    /// #979/#947 were closed — this test does NOT depend on live GitHub.
    #[test]
    fn acceptance_anchor_980_981_catch_979_947_as_zombies() {
        let merged_prs = vec![
            pr(
                980,
                "fix(vault): standard profile allow-list + FIFO error-order (#979)",
                "Fixes the two independent bugs of #979 (found in the 2026-07-11 full open-issue audit).\n\nRefs #979",
            ),
            pr(
                981,
                "fix(mcp): remote client opt-in allow_proxy (#947)",
                "Implements the owner-ratified option (c) of #947 (2026-07-10 ruling).\n\nRefs #947",
            ),
            // A merged PR referencing an issue that is NOT open must not
            // produce a false positive.
            pr(998, "release: prepare v1.9.0", "Refs #530, #724, #921"),
        ];
        // Open-issue snapshot AS OF the 2026-07-11 audit (before the leader
        // closed #979/#947 by hand): both still open.
        let open_issue_numbers = vec![979, 947, 1001, 1002];

        let hits = scan_zombies(&merged_prs, &open_issue_numbers);

        assert!(
            hits.iter()
                .any(|h| h.issue_number == 979 && h.pr_number == 980),
            "expected #979 flagged as zombie via PR #980, got: {hits:?}"
        );
        assert!(
            hits.iter()
                .any(|h| h.issue_number == 947 && h.pr_number == 981),
            "expected #947 flagged as zombie via PR #981, got: {hits:?}"
        );
        // #530/#724/#921 referenced by #998 are not in the open set (not
        // part of this fixture's open-issue snapshot) — must not appear.
        assert!(
            !hits.iter().any(|h| h.issue_number == 530),
            "issue not in the open snapshot must not be flagged, got: {hits:?}"
        );
        assert_eq!(
            hits.len(),
            2,
            "expected exactly the two real zombies, got: {hits:?}"
        );
    }

    #[test]
    fn scan_zombies_ignores_issues_already_closed() {
        let merged_prs = vec![pr(1, "fix", "Refs #100")];
        let open_issue_numbers = vec![200]; // #100 not open
        assert!(scan_zombies(&merged_prs, &open_issue_numbers).is_empty());
    }

    #[test]
    fn scan_zombies_dedupes_repeat_references() {
        let merged_prs = vec![pr(1, "fix", "Refs #100 #100")];
        let open_issue_numbers = vec![100];
        let hits = scan_zombies(&merged_prs, &open_issue_numbers);
        assert_eq!(hits.len(), 1);
    }

    fn stale_issue(number: u64, anchors: &[(&str, u64)], gates: &[u64]) -> OpenIssueForStaleCheck {
        OpenIssueForStaleCheck {
            number,
            file_line_anchors: anchors.iter().map(|(p, l)| (p.to_string(), *l)).collect(),
            gate_issue_numbers: gates.to_vec(),
        }
    }

    #[test]
    fn stale_candidate_flags_vanished_file_line_anchor() {
        let issues = vec![stale_issue(1, &[("src/foo.rs", 500)], &[])];
        let mut file_lines = std::collections::HashMap::new();
        file_lines.insert("src/foo.rs".to_string(), Some(50)); // file shrank
        let out = scan_stale_candidates(&issues, &file_lines, &[]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].issue_number, 1);
    }

    #[test]
    fn stale_candidate_flags_deleted_file() {
        let issues = vec![stale_issue(2, &[("src/gone.rs", 10)], &[])];
        let mut file_lines = std::collections::HashMap::new();
        file_lines.insert("src/gone.rs".to_string(), None);
        let out = scan_stale_candidates(&issues, &file_lines, &[]);
        assert_eq!(out.len(), 1);
        assert!(out[0].evidence[0].contains("no longer exists"));
    }

    #[test]
    fn stale_candidate_flags_closed_gate_dependency() {
        let issues = vec![stale_issue(3, &[], &[42])];
        let out = scan_stale_candidates(&issues, &std::collections::HashMap::new(), &[42]);
        assert_eq!(out.len(), 1);
        assert!(out[0].evidence[0].contains("#42"));
    }

    #[test]
    fn stale_candidate_clean_issue_not_flagged() {
        let issues = vec![stale_issue(4, &[("src/ok.rs", 5)], &[7])];
        let mut file_lines = std::collections::HashMap::new();
        file_lines.insert("src/ok.rs".to_string(), Some(500));
        let out = scan_stale_candidates(&issues, &file_lines, &[]); // gate 7 not closed
        assert!(out.is_empty());
    }

    #[test]
    fn freshness_verdict_roundtrips_through_json() {
        let verdict = FreshnessVerdict {
            issue_ref: "kckylechen1/tachi#979".to_string(),
            verified_at_sha: "deadbeef".to_string(),
            verdict: "zombie".to_string(),
            evidence_refs: vec!["kckylechen1/tachi#980".to_string()],
            checked_at: "2026-07-11T00:00:00Z".to_string(),
        };
        let json = serde_json::to_string(&verdict).expect("serialize");
        let back: FreshnessVerdict = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.issue_ref, verdict.issue_ref);
        assert_eq!(back.verdict, verdict.verdict);
    }

    fn test_server() -> MemoryServer {
        let db = std::env::temp_dir().join(format!(
            "issue-freshness-test-{}.sqlite",
            uuid::Uuid::new_v4()
        ));
        MemoryServer::new(db, None).expect("test server")
    }

    fn verdict(issue_ref: &str, kind: &str, evidence: &[&str]) -> FreshnessVerdict {
        FreshnessVerdict {
            issue_ref: issue_ref.to_string(),
            verified_at_sha: "sha1".to_string(),
            verdict: kind.to_string(),
            evidence_refs: evidence.iter().map(|s| s.to_string()).collect(),
            checked_at: "2026-07-11T00:00:00Z".to_string(),
        }
    }

    #[test]
    fn save_and_list_freshness_verdicts_roundtrips_via_state_kv() {
        let server = test_server();
        save_freshness_verdict(&server, &verdict("o/r#979", "zombie", &["o/r#980"]))
            .expect("save zombie");
        save_freshness_verdict(&server, &verdict("o/r#500", "stale_candidate", &[]))
            .expect("save stale");

        let all = list_freshness_verdicts(&server).expect("list");
        assert_eq!(all.len(), 2);
        assert!(all
            .iter()
            .any(|v| v.issue_ref == "o/r#979" && v.verdict == "zombie"));
        assert!(all
            .iter()
            .any(|v| v.issue_ref == "o/r#500" && v.verdict == "stale_candidate"));
    }

    #[test]
    fn save_freshness_verdict_overwrites_same_issue_ref() {
        let server = test_server();
        save_freshness_verdict(&server, &verdict("o/r#1", "stale_candidate", &[]))
            .expect("save first");
        save_freshness_verdict(&server, &verdict("o/r#1", "zombie", &["o/r#2"]))
            .expect("save second");

        let all = list_freshness_verdicts(&server).expect("list");
        assert_eq!(all.len(), 1, "same issue_ref must overwrite, not duplicate");
        assert_eq!(all[0].verdict, "zombie");
    }

    #[test]
    fn briefing_freshness_queues_splits_by_verdict_with_counts() {
        let server = test_server();
        save_freshness_verdict(&server, &verdict("o/r#979", "zombie", &["o/r#980"])).expect("save");
        save_freshness_verdict(&server, &verdict("o/r#947", "zombie", &["o/r#981"])).expect("save");
        save_freshness_verdict(&server, &verdict("o/r#500", "stale_candidate", &[])).expect("save");

        let out = briefing_freshness_queues(&server, 8);
        assert_eq!(out["zombies"]["count"], 2);
        assert_eq!(out["stale_candidates"]["count"], 1);
        assert_eq!(out["zombies"]["overflow"], 0);
        let zombie_refs: Vec<&str> = out["zombies"]["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v["issue_ref"].as_str().unwrap())
            .collect();
        assert!(zombie_refs.contains(&"o/r#979"));
        assert!(zombie_refs.contains(&"o/r#947"));
    }

    #[test]
    fn briefing_freshness_queues_reports_overflow_never_silently_caps() {
        let server = test_server();
        for n in 0..5 {
            save_freshness_verdict(&server, &verdict(&format!("o/r#{n}"), "zombie", &[]))
                .expect("save");
        }
        let out = briefing_freshness_queues(&server, 2);
        assert_eq!(out["zombies"]["count"], 5);
        assert_eq!(out["zombies"]["items"].as_array().unwrap().len(), 2);
        assert_eq!(out["zombies"]["overflow"], 3);
    }

    #[test]
    fn briefing_freshness_queues_empty_when_no_verdicts_saved() {
        let server = test_server();
        let out = briefing_freshness_queues(&server, 8);
        assert_eq!(out["zombies"]["count"], 0);
        assert_eq!(out["stale_candidates"]["count"], 0);
        assert!(out["zombies"]["items"].as_array().unwrap().is_empty());
    }
}
