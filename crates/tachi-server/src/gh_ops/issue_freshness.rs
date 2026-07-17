//! Issue freshness layer (#1000): zombie scan + stale-candidate heuristics.
//!
//! GitHub remains the only source of truth for issue *content* — this module
//! never mirrors issue bodies. It only ever produces judgment-free rows
//! (`FreshnessRow { issue_ref, kind, verified_at_sha, evidence_refs,
//! checked_at }`, see the storage section below): zombie evidence is a
//! "candidate: fixed-awaiting-closure" claim, never a final verdict —
//! closing is always a leader/owner action. Stale/churn candidates are NOT
//! verdicts at all — they live in a separate candidate-queue rowset (see
//! `STALE_CANDIDATE_NS` below) because the frozen #1000 constraint is
//! "圈候选不判决" (circle the candidate, do not judge it). Durable
//! conclusions write back to GitHub's own medium (label/comment) so they are
//! visible without a Tachi consumer.
//!
//! Three independent detectors, all read-only:
//!   - `scan_zombies`: reverse-scans merged PR bodies/titles **and merged
//!     commit messages** for `Refs #N` / `(#N)` references, cross-checked
//!     against the open-issue set. A hit means "a merged PR/commit says it
//!     fixed #N, and #N is still open" — the mechanical prototype of what the
//!     leader did by hand on 2026-07-11.
//!   - `scan_stale_candidates`: heuristic-only (never a verdict) — issues
//!     whose referenced file:line anchors have vanished from HEAD, or whose
//!     referenced *gate* issues (explicitly marked "gated on"/"blocked
//!     by"/"depends on" in the same sentence as the `#N` — plain `Refs #N` is
//!     related, not a gate) are already closed. Produces a review queue, not
//!     a judgment.
//!   - `scan_same_surface_churn`: heuristic-only — an issue whose file:line
//!     anchors sit on a surface that recent merged PRs have repeatedly
//!     touched, while the issue itself shows zero activity, is a stale-spec
//!     candidate (the code moved on, the issue text did not).

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
    /// Commit messages (headline + body) for every commit `gh` reports as
    /// part of this PR — includes the squash/merge commit. Commit-only
    /// `Refs #N` references (never repeated in the PR title/body) are only
    /// visible here; scanning title+body alone misses them (#1000 codex
    /// review finding 1).
    pub commit_messages: Vec<String>,
    /// The merge commit's own message, read from the local checkout (`git
    /// log -1 --format=%B <mergeCommit.oid>`) — never from `gh` itself,
    /// which only returns `mergeCommit.oid` (no message text). A TRUE merge
    /// commit (2 parents, "Create a merge commit" strategy) is a distinct
    /// commit from every commit in `commit_messages` — its own `Refs #N` is
    /// otherwise invisible to the scan (#1000 round-3 codex review finding
    /// 1). Empty when the SHA is absent, unresolvable, or the local checkout
    /// doesn't have it (e.g. shallow clone, or scanning a fork) — a resolve
    /// failure here degrades to "this one PR's merge-commit message wasn't
    /// checked", never a hard scan failure.
    pub merge_commit_message: String,
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
/// referenced by a merged PR — its title, body, any of its commit messages
/// (a commit-only `Refs #N` that never made it into the PR title/body is
/// otherwise invisible, #1000 codex review finding 1), **or its merge
/// commit's own message** (a TRUE merge commit is a distinct commit from
/// every PR commit — its `Refs #N` is invisible to `commit_messages` alone,
/// #1000 round-3 codex review finding 1) — that is still in
/// `open_issue_numbers` is a zombie (fixed, never closed). Pure function —
/// no I/O — so it is directly fixture-testable against the #979/#947
/// acceptance anchor without hitting live GitHub.
pub(crate) fn scan_zombies(merged_prs: &[MergedPr], open_issue_numbers: &[u64]) -> Vec<ZombieHit> {
    let mut hits = Vec::new();
    for pr in merged_prs {
        let mut text = format!("{}\n{}", pr.title, pr.body);
        for commit_message in &pr.commit_messages {
            text.push('\n');
            text.push_str(commit_message);
        }
        if !pr.merge_commit_message.is_empty() {
            text.push('\n');
            text.push_str(&pr.merge_commit_message);
        }
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
    /// Gate/dependency issue numbers this issue's body references as blockers
    /// — ONLY numbers whose reference is explicitly marked "gated on"/
    /// "blocked by"/"depends on" in the same sentence (see
    /// `extract_gate_issue_numbers`). Plain `Refs #N` is related, not a gate
    /// (#1000 codex review finding 3): closing a merely-related issue must
    /// not falsely mark this issue stale.
    pub gate_issue_numbers: Vec<u64>,
}

/// Result of probing one anchored file's current state on HEAD — kept
/// distinct from a plain `Option<u64>` so "file genuinely does not exist"
/// (a real stale signal) and "read failed for some other reason" (a scan
/// error, NOT a stale signal) don't collapse into the same `None` the way
/// `.ok()` used to (#1000 codex review finding 4).
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum FileProbe {
    /// File exists on HEAD with this many lines.
    Found(u64),
    /// File does not exist on HEAD (`NotFound` I/O error) — a real stale
    /// signal: the anchor's target is genuinely gone.
    NotFound,
    /// File could not be read for a reason other than not-found (permission
    /// denied, not valid UTF-8, etc.) — a scan warning, never a stale signal;
    /// the caller cannot tell from this whether the anchor is still valid.
    Unreadable(String),
}

/// Heuristic-only stale-candidate scan (Scope item 2). Two independent
/// signals, either one is enough to surface a candidate:
///   - a referenced `file:line` anchor no longer exists at that line count
///     (or the file itself is gone) on HEAD;
///   - a referenced gate/dependency issue (explicitly marked as a gate, see
///     `OpenIssueForStaleCheck::gate_issue_numbers`) is already closed.
///
/// `file_probes` maps repo-relative path -> its `FileProbe` outcome.
/// `closed_issue_numbers` is the set of issue numbers known to be closed.
/// Never produces a verdict — only a reason + evidence to re-read the issue.
/// Returns `(candidates, scan_warnings)`: `Unreadable` probes never become
/// stale evidence, but are surfaced as warnings so a read failure isn't
/// silently indistinguishable from "nothing to report" (finding 4).
pub(crate) fn scan_stale_candidates(
    issues: &[OpenIssueForStaleCheck],
    file_probes: &std::collections::HashMap<String, FileProbe>,
    closed_issue_numbers: &[u64],
) -> (Vec<StaleCandidate>, Vec<String>) {
    let mut out = Vec::new();
    let mut warnings = Vec::new();
    for issue in issues {
        let mut evidence = Vec::new();
        for (path, line) in &issue.file_line_anchors {
            match file_probes.get(path) {
                Some(FileProbe::Found(current_lines)) if line > current_lines => {
                    evidence.push(format!(
                        "{path}:{line} anchor exceeds current file length ({current_lines} lines)"
                    ));
                }
                Some(FileProbe::NotFound) => {
                    evidence.push(format!("{path} no longer exists on HEAD"));
                }
                Some(FileProbe::Unreadable(reason)) => {
                    warnings.push(format!(
                        "issue #{}: could not verify anchor {path}: {reason}",
                        issue.number
                    ));
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
    (out, warnings)
}

/// Extract the run of `#N` references that sit **immediately after** a gate
/// phrase in `span` — i.e. only separator characters (space/colon/comma) are
/// allowed between the phrase and the `#N` tokens; as soon as anything else
/// is hit, the run stops. This is what makes gate attribution precise rather
/// than sentence-wide: `phrase_end` is the byte offset right after the
/// matched gate phrase, and only `#N`s starting there (modulo separators)
/// belong to it (#1000 round-3 codex review finding 2 — "gate 短语连坐": a
/// sentence-wide scan like `extract_hash_numbers(whole_sentence)` used to
/// also catch an unrelated `#M` mentioned later in the same sentence, e.g.
/// "gated on #1 (Refs #2)" wrongly attributed #2 as a gate too).
fn extract_hash_numbers_immediately_after(span: &str, phrase_end: usize) -> Vec<u64> {
    let mut out = Vec::new();
    let bytes = span.as_bytes();
    let mut i = phrase_end;
    loop {
        while bytes
            .get(i)
            .is_some_and(|b| matches!(b, b' ' | b':' | b',' | b'\t' | b'(' | b')'))
        {
            i += 1;
        }
        if bytes.get(i) == Some(&b'#') {
            let start = i + 1;
            let mut end = start;
            while bytes.get(end).is_some_and(u8::is_ascii_digit) {
                end += 1;
            }
            if end > start {
                if let Ok(n) = span[start..end].parse::<u64>() {
                    out.push(n);
                }
                i = end;
                continue;
            }
        }
        break;
    }
    out
}

/// Extract issue numbers this text marks as **gates** — i.e. the `#N`
/// reference immediately follows "gated on" / "blocked by" / "depends on"
/// (case-insensitive), with only whitespace/punctuation separators allowed in
/// between. A plain `Refs #N` elsewhere in the text does NOT count: ordinary
/// references are related, not gate dependencies (#1000 codex review finding
/// 3). Attribution is precise to the phrase, not sentence-wide (#1000
/// round-3 codex review finding 2): `gated on #1 (Refs #2)` must flag ONLY
/// #1 as a gate — #2 is a plain Refs mention that happens to share the
/// sentence, not itself gate-marked. "Sentence" is approximated as a
/// newline-or-period-delimited span (matching how issue bodies actually
/// write these clauses) purely to find the phrase's search window; the
/// actual attribution walks forward from the phrase match itself. Byte
/// offsets are tracked by walking the split iterator directly (not
/// `str::find`) so duplicate sentence text within the same body cannot
/// collide on the wrong span.
pub(crate) fn extract_gate_issue_numbers(text: &str) -> Vec<u64> {
    const GATE_PHRASES: [&str; 3] = ["gated on", "blocked by", "depends on"];
    let lower = text.to_ascii_lowercase();
    let mut out = Vec::new();
    let mut offset = 0usize;
    for sentence in lower.split(['\n', '.', ';']) {
        let start = offset;
        let end = (start + sentence.len()).min(text.len());
        offset = end + 1; // skip the one-byte delimiter consumed by split
                          // A sentence can contain more than one gate phrase (rare, but cheap
                          // to support correctly) — walk all matches, not just the first.
        let mut search_from = 0usize;
        while let Some(rel_match) = GATE_PHRASES
            .iter()
            .filter_map(|p| sentence[search_from..].find(p).map(|pos| (pos, p.len())))
            .min_by_key(|(pos, _)| *pos)
        {
            let (rel_pos, phrase_len) = rel_match;
            let phrase_end_in_sentence = search_from + rel_pos + phrase_len;
            if let Some(original_span) = text.get(start..end) {
                out.extend(extract_hash_numbers_immediately_after(
                    original_span,
                    phrase_end_in_sentence,
                ));
            }
            search_from = phrase_end_in_sentence;
        }
    }
    out.sort_unstable();
    out.dedup();
    out
}

/// One same-surface-churn candidate: NOT a verdict — a review-queue reason
/// (#1000 Scope item 2's third heuristic: "同面文件近期落了 N 个 PR 而 issue 零活动").
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ChurnCandidate {
    pub issue_number: u64,
    pub touching_pr_count: usize,
    pub touching_pr_numbers: Vec<u64>,
}

/// Minimal open-issue shape for the churn heuristic: its file-surface
/// (derived from its `file_line_anchors`' paths) plus whether it has had any
/// activity (a comment, or an owner body edit) since `since`.
#[derive(Debug, Clone, Default)]
pub(crate) struct OpenIssueForChurnCheck {
    pub number: u64,
    /// Repo-relative file paths this issue's body anchors to (its "surface").
    pub surface_paths: Vec<String>,
    /// True when the issue has had any activity (comment or body update)
    /// at or after the churn-window cutoff.
    pub has_recent_activity: bool,
}

/// A merged PR's touched-file surface, for the churn heuristic.
#[derive(Debug, Clone, Default)]
pub(crate) struct MergedPrSurface {
    pub pr_number: u64,
    pub touched_paths: Vec<String>,
    /// RFC3339 merge timestamp (`gh pr list --json mergedAt`). Used to
    /// window "recently merged" to the SAME `activity_since` cutoff the
    /// issue-activity side already uses — without this, `limit` (a PR
    /// *count*) was the only bound on "recent", so a repo with high merge
    /// volume could pull in PRs merged months/years ago as "churn" evidence
    /// while `--limit` was still under the count (#1000 round-3 codex
    /// review finding 6).
    pub merged_at: String,
}

/// Third stale-candidate heuristic (#1000 Scope item 2): an issue whose
/// surface (the files its `path:line` anchors point at) has been touched by
/// `churn_threshold` or more *distinct* recently-merged PRs, while the issue
/// itself shows zero activity in that same window, is a stale-spec
/// candidate — the code moved on under it and nobody revisited the issue
/// text. Heuristic-only: never a verdict, only a reason to re-read the issue
/// (same posture as `scan_stale_candidates`).
pub(crate) fn scan_same_surface_churn(
    issues: &[OpenIssueForChurnCheck],
    recent_prs: &[MergedPrSurface],
    churn_threshold: usize,
) -> Vec<ChurnCandidate> {
    // #1000 round-3 codex review finding 6: `churn_threshold == 0` makes
    // `touching_pr_numbers.len() >= churn_threshold` trivially true even for
    // an issue with ZERO touching PRs — every inactive issue with a
    // non-empty file-surface would flag regardless of actual churn
    // evidence. The heuristic's premise is "N *distinct touching* PRs";
    // clamp the effective threshold to 1 here too (defense in depth — the
    // caller in `router.rs` also clamps before calling, but the invariant
    // belongs to the function that owns the "what counts as churn" logic).
    let churn_threshold = churn_threshold.max(1);
    let mut out = Vec::new();
    for issue in issues {
        if issue.has_recent_activity || issue.surface_paths.is_empty() {
            continue;
        }
        let mut touching_pr_numbers: Vec<u64> = recent_prs
            .iter()
            .filter(|pr| {
                pr.touched_paths
                    .iter()
                    .any(|p| issue.surface_paths.contains(p))
            })
            .map(|pr| pr.pr_number)
            .collect();
        touching_pr_numbers.sort_unstable();
        touching_pr_numbers.dedup();
        if touching_pr_numbers.len() >= churn_threshold {
            out.push(ChurnCandidate {
                issue_number: issue.number,
                touching_pr_count: touching_pr_numbers.len(),
                touching_pr_numbers,
            });
        }
    }
    out.sort_by_key(|c| c.issue_number);
    out
}

/// Fetch merged PRs for `repo` via `gh pr list --state merged` and run the
/// zombie scan against the currently-open issue numbers. `limit` bounds how
/// many merged PRs are fetched (most-recently-merged first, per `gh`'s
/// default ordering). `repo_root` is the local checkout used to resolve each
/// merge commit's own message via `git log` (#1000 round-3 codex review
/// finding 1) — `None` (or a checkout that doesn't have the commit, e.g.
/// shallow clone) just means merge-commit-only references degrade silently
/// to invisible for that one PR, never a hard scan failure. Live-GitHub I/O
/// boundary — kept thin so the scan logic above stays independently
/// fixture-tested.
pub(crate) fn fetch_and_scan_zombies(
    server: &MemoryServer,
    repo: &str,
    limit: u32,
    repo_root: Option<&std::path::Path>,
) -> Result<Vec<ZombieHit>, String> {
    let open_issue_numbers = fetch_open_issue_numbers(server, repo)?;
    if open_issue_numbers.is_empty() {
        return Ok(Vec::new());
    }
    let merged_prs = fetch_merged_prs(server, repo, limit, repo_root)?;
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
    Ok(parse_issue_numbers_json(&value))
}

/// Pure parser for `gh issue list --json number` output (#1000 round-3 codex
/// review finding 4: this field-path parsing was still inline/untested,
/// unlike its sibling parsers `parse_merged_prs_json` /
/// `parse_open_issues_with_activity_json` / `parse_merged_pr_surfaces_json`,
/// which were already extracted under finding 8). Shared by both the
/// open-issue-number fetch (zombie arm) and the closed-issue-number fetch
/// (stale arm) — both requests only ever project the single `number` field.
pub(crate) fn parse_issue_numbers_json(value: &Value) -> Vec<u64> {
    value
        .as_array()
        .map(|rows| {
            rows.iter()
                .filter_map(|r| r.get("number").and_then(Value::as_u64))
                .collect()
        })
        .unwrap_or_default()
}

/// #1105: also the "existing gh read surface" reused by
/// `refinery_ops::live_signals` to find merged PRs referencing a specific
/// (any-state, not just currently-open) issue number for its
/// commit-reachability shipped check — the same fetch `fetch_and_scan_zombies`
/// already uses, just exposed so a second caller can run its own
/// `extract_referenced_issue_numbers` cross-check against a different target
/// set than "the open-issue zombie scan".
pub(crate) fn fetch_merged_prs(
    server: &MemoryServer,
    repo: &str,
    limit: u32,
    repo_root: Option<&std::path::Path>,
) -> Result<Vec<MergedPr>, String> {
    let (mut cmd, token) = build_gh_command(server)?;
    // `commits` (not part of the original #1000 shipped fields) carries every
    // commit's messageHeadline/messageBody per PR, including squash commits
    // but NOT a true (2-parent) merge commit, which is its own distinct
    // commit created at merge time — this is how commit-only `Refs #N`
    // references (never repeated in the PR title/body) become visible to
    // the scan (codex review finding 1). `gh pr list --json mergeCommit`
    // itself only returns `{oid}`, no message text, so a true merge
    // commit's own `Refs #N` is resolved separately below via local `git
    // log` (round-3 codex review finding 1).
    cmd.args(["pr", "list"])
        .args(["--repo", repo])
        .args(["--state", "merged"])
        .args(["--limit", &limit.to_string()])
        .args(["--json", "number,title,body,mergeCommit,commits"]);
    let output = run_gh_json(cmd, &token)?;
    let value: Value =
        serde_json::from_str(&output).map_err(|e| format!("parse pr list json: {e}"))?;
    let mut merged_prs = parse_merged_prs_json(&value);
    if let Some(repo_root) = repo_root {
        for merged_pr in &mut merged_prs {
            if let Some(sha) = merged_pr.merge_commit_sha.as_deref() {
                merged_pr.merge_commit_message = resolve_merge_commit_message(repo_root, sha);
            }
        }
    }
    Ok(merged_prs)
}

/// Read a merge commit's own message from the local checkout: `git -C
/// repo_root log -1 --format=%B <sha>`. Best-effort — a resolve failure
/// (commit not present locally, e.g. shallow clone or not yet fetched; `git`
/// not on PATH; not a git repo) degrades silently to an empty string, never
/// a hard error. This is enrichment on top of `gh`'s primary PR data, not
/// the scan's I/O boundary of record (#1000 round-3 codex review finding 1).
fn resolve_merge_commit_message(repo_root: &std::path::Path, sha: &str) -> String {
    if sha.is_empty() {
        return String::new();
    }
    std::process::Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["log", "-1", "--format=%B", sha])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
        .unwrap_or_default()
}

/// Pure parser for `gh pr list --json number,title,body,mergeCommit,commits`
/// output — extracted so field-path parsing is fixture-testable without live
/// GitHub (#1000 codex review finding 8: I/O layer field names unverified).
pub(crate) fn parse_merged_prs_json(value: &Value) -> Vec<MergedPr> {
    value
        .as_array()
        .map(|rows| {
            rows.iter()
                .filter_map(|r| {
                    let number = r.get("number").and_then(Value::as_u64)?;
                    let commit_messages = r
                        .get("commits")
                        .and_then(Value::as_array)
                        .map(|commits| {
                            commits
                                .iter()
                                .map(|c| {
                                    let headline = c
                                        .get("messageHeadline")
                                        .and_then(Value::as_str)
                                        .unwrap_or_default();
                                    let body = c
                                        .get("messageBody")
                                        .and_then(Value::as_str)
                                        .unwrap_or_default();
                                    format!("{headline}\n{body}")
                                })
                                .collect()
                        })
                        .unwrap_or_default();
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
                        commit_messages,
                        // Populated separately (best-effort, local `git log`)
                        // by `fetch_merged_prs` after this pure parse — `gh`
                        // itself never returns a merge commit's message text.
                        merge_commit_message: String::new(),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
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

/// Probe one repo-relative path's current state on HEAD, distinguishing
/// "genuinely gone" from "could not read for some other reason" (#1000 codex
/// review finding 4 — `.ok()` used to collapse both into the same `None`,
/// which meant a permissions error or non-UTF8 file was silently reported as
/// "no longer exists on HEAD", a false stale signal).
fn probe_file(repo_root: &std::path::Path, path: &str) -> FileProbe {
    match std::fs::read_to_string(repo_root.join(path)) {
        Ok(contents) => FileProbe::Found(contents.lines().count() as u64),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => FileProbe::NotFound,
        Err(e) => FileProbe::Unreadable(e.to_string()),
    }
}

/// Live entry point for the stale-candidate scan (Scope item 2). Fetches
/// open issue bodies, parses anchors, checks each anchored file's current
/// line count under `repo_root`, and cross-checks referenced gate issues
/// against the closed set. Heuristic-only — callers must not treat the
/// result as a verdict (no `save_freshness_row` call here); it is a
/// review queue, same posture as `scan_open_loops`'s `spec_drift` kind.
/// Returns `(candidates, scan_warnings)` — warnings surface unreadable
/// anchors distinctly from stale evidence (finding 4).
pub(crate) fn fetch_and_scan_stale_candidates(
    server: &MemoryServer,
    repo: &str,
    repo_root: &std::path::Path,
    limit: u32,
) -> Result<(Vec<StaleCandidate>, Vec<String>), String> {
    let open = fetch_open_issues_with_body(server, repo, limit)?;
    let closed_issue_numbers = fetch_closed_issue_numbers(server, repo)?;

    let mut file_probes: std::collections::HashMap<String, FileProbe> =
        std::collections::HashMap::new();
    let mut issues = Vec::with_capacity(open.len());
    for (number, body) in &open {
        let anchors = extract_file_line_anchors(body);
        for (path, _) in &anchors {
            file_probes
                .entry(path.clone())
                .or_insert_with(|| probe_file(repo_root, path));
        }
        // Only explicitly gate-marked references ("gated on"/"blocked by"/
        // "depends on #N") count as gate dependencies — a plain `Refs #N`
        // is related, not a gate (finding 3).
        let gates = extract_gate_issue_numbers(body);
        issues.push(OpenIssueForStaleCheck {
            number: *number,
            file_line_anchors: anchors,
            gate_issue_numbers: gates,
        });
    }
    Ok(scan_stale_candidates(
        &issues,
        &file_probes,
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
    Ok(parse_issue_numbers_with_body_json(&value))
}

/// Pure parser for `gh issue list --json number,body` output (#1000 round-3
/// codex review finding 4).
pub(crate) fn parse_issue_numbers_with_body_json(value: &Value) -> Vec<(u64, String)> {
    value
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
        .unwrap_or_default()
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
    Ok(parse_issue_numbers_json(&value))
}

/// Window `merged_prs` down to only those merged at/after `activity_since`
/// (#1000 round-3 codex review finding 6): before this, the churn
/// heuristic's "recently merged" set was bounded ONLY by `limit` (a PR
/// *count*), not by the SAME activity window the issue side uses — a repo
/// with high merge volume could pull in PRs merged months/years ago as
/// "churn" evidence while still under the count. A PR with no `mergedAt`
/// (shouldn't happen for `--state merged`, but never trust an external
/// field silently) is treated as NOT recent rather than always-included —
/// an empty string sorts before any real RFC3339 timestamp, so it correctly
/// fails the `>= activity_since` test. Pure function — fixture-testable
/// without live `gh`.
pub(crate) fn filter_merged_prs_since(
    merged_prs: Vec<MergedPrSurface>,
    activity_since: &str,
) -> Vec<MergedPrSurface> {
    merged_prs
        .into_iter()
        .filter(|pr| pr.merged_at.as_str() >= activity_since)
        .collect()
}

/// Live entry point for the third stale heuristic (#1000 Scope item 2,
/// codex review finding 2): open issues whose file-surface has been
/// repeatedly touched by recent merged PRs while the issue itself shows no
/// activity in the same window. `activity_since` is an RFC3339 cutoff —
/// issues with `updatedAt` at/after the cutoff, or a comment at/after it,
/// count as active and are excluded. `churn_threshold` is the minimum number
/// of distinct touching PRs required to flag (default policy: 3, chosen by
/// the caller). Merged PRs are windowed to the same `activity_since` cutoff
/// via `filter_merged_prs_since` (round-3 finding 6) before being handed to
/// the pure `scan_same_surface_churn`.
pub(crate) fn fetch_and_scan_same_surface_churn(
    server: &MemoryServer,
    repo: &str,
    limit: u32,
    activity_since: &str,
    churn_threshold: usize,
) -> Result<Vec<ChurnCandidate>, String> {
    let open = fetch_open_issues_with_activity(server, repo, limit)?;
    let all_merged_prs = fetch_merged_pr_surfaces(server, repo, limit)?;
    let recent_prs = filter_merged_prs_since(all_merged_prs, activity_since);

    let issues: Vec<OpenIssueForChurnCheck> = open
        .into_iter()
        .map(|(number, body, updated_at, last_comment_at)| {
            let surface_paths = extract_file_line_anchors(&body)
                .into_iter()
                .map(|(path, _)| path)
                .collect();
            let has_recent_activity = updated_at.as_str() >= activity_since
                || last_comment_at
                    .as_deref()
                    .is_some_and(|c| c >= activity_since);
            OpenIssueForChurnCheck {
                number,
                surface_paths,
                has_recent_activity,
            }
        })
        .collect();

    Ok(scan_same_surface_churn(
        &issues,
        &recent_prs,
        churn_threshold,
    ))
}

/// `(number, body, updated_at, most_recent_comment_created_at)` — the
/// activity signal for the churn heuristic.
type OpenIssueActivity = (u64, String, String, Option<String>);

/// Fetch open-issue activity rows for the churn heuristic.
fn fetch_open_issues_with_activity(
    server: &MemoryServer,
    repo: &str,
    limit: u32,
) -> Result<Vec<OpenIssueActivity>, String> {
    let (mut cmd, token) = build_gh_command(server)?;
    cmd.args(["issue", "list"])
        .args(["--repo", repo])
        .args(["--state", "open"])
        .args(["--limit", &limit.to_string()])
        .args(["--json", "number,body,updatedAt,comments"]);
    let output = run_gh_json(cmd, &token)?;
    let value: Value =
        serde_json::from_str(&output).map_err(|e| format!("parse issue list json: {e}"))?;
    Ok(parse_open_issues_with_activity_json(&value))
}

/// Pure parser for `gh issue list --json number,body,updatedAt,comments`
/// (#1000 codex review finding 8: fixture-testable field-path parsing).
pub(crate) fn parse_open_issues_with_activity_json(value: &Value) -> Vec<OpenIssueActivity> {
    value
        .as_array()
        .map(|rows| {
            rows.iter()
                .filter_map(|r| {
                    let number = r.get("number").and_then(Value::as_u64)?;
                    let body = r.get("body").and_then(Value::as_str).unwrap_or_default();
                    let updated_at = r
                        .get("updatedAt")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    let last_comment_at = r
                        .get("comments")
                        .and_then(Value::as_array)
                        .and_then(|comments| {
                            comments
                                .iter()
                                .filter_map(|c| c.get("createdAt").and_then(Value::as_str))
                                .max()
                        })
                        .map(str::to_string);
                    Some((
                        number,
                        body.to_string(),
                        updated_at.to_string(),
                        last_comment_at,
                    ))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Fetch the touched-file surface for recently merged PRs, for the churn
/// heuristic's "same surface" cross-check.
fn fetch_merged_pr_surfaces(
    server: &MemoryServer,
    repo: &str,
    limit: u32,
) -> Result<Vec<MergedPrSurface>, String> {
    let (mut cmd, token) = build_gh_command(server)?;
    cmd.args(["pr", "list"])
        .args(["--repo", repo])
        .args(["--state", "merged"])
        .args(["--limit", &limit.to_string()])
        .args(["--json", "number,files,mergedAt"]);
    let output = run_gh_json(cmd, &token)?;
    let value: Value =
        serde_json::from_str(&output).map_err(|e| format!("parse pr list json: {e}"))?;
    Ok(parse_merged_pr_surfaces_json(&value))
}

/// Pure parser for `gh pr list --json number,files` (finding 8).
pub(crate) fn parse_merged_pr_surfaces_json(value: &Value) -> Vec<MergedPrSurface> {
    value
        .as_array()
        .map(|rows| {
            rows.iter()
                .filter_map(|r| {
                    let pr_number = r.get("number").and_then(Value::as_u64)?;
                    let touched_paths = r
                        .get("files")
                        .and_then(Value::as_array)
                        .map(|files| {
                            files
                                .iter()
                                .filter_map(|f| f.get("path").and_then(Value::as_str))
                                .map(str::to_string)
                                .collect()
                        })
                        .unwrap_or_default();
                    let merged_at = r
                        .get("mergedAt")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    Some(MergedPrSurface {
                        pr_number,
                        touched_paths,
                        merged_at,
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

// ─── Candidate storage (state_kv, per scouted precedent) ───────────────────
//
// #1000 codex review finding 5 ("圈候选不判决" — circle the candidate, do not
// judge it): NOTHING this module stores is a final verdict. Even a zombie hit
// is a "candidate: fixed-awaiting-closure" claim — closing an issue is always
// a leader/owner action taken on GitHub itself, never automated here. The
// on-disk shape reflects that: every row's `kind` is one of `"zombie"` /
// `"stale_candidate"` / `"churn_candidate"`, and none of them are named or
// treated as a "verdict".
//
// Two namespaces, not one, so the zombie candidate-claim rowset (evidence: a
// specific merged PR/commit) and the heuristic candidate-queue rowset
// (evidence: free-form reasons, never a specific fixing PR) don't share a
// key/shape that would make the caller's fixing-PR field mean two different
// things depending on kind.
//
// #1000 codex review finding 7 (review candidates never die + kind/issue key
// collision): rows are keyed `{kind}:{issue_ref}` (not just `{issue_ref}`) so
// a zombie row and a stale-candidate row for the *same* issue cannot
// overwrite each other. Each scan is authoritative for its own kind: after a
// scan of kind K produces its current hit set, `reap_stale_kind_rows` deletes
// every existing row of kind K whose issue_ref is NOT in that fresh set — a
// zombie that got closed, or a stale-candidate whose anchor drift got fixed,
// disappears from the briefing on the very next scan instead of living
// forever as a ghost row.
pub(crate) const ZOMBIE_NS: &str = "issue_freshness_zombie";
pub(crate) const STALE_CANDIDATE_NS: &str = "issue_freshness_stale_candidate";

pub(crate) const KIND_ZOMBIE: &str = "zombie";
pub(crate) const KIND_STALE_CANDIDATE: &str = "stale_candidate";
pub(crate) const KIND_CHURN_CANDIDATE: &str = "churn_candidate";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct FreshnessRow {
    pub issue_ref: String,
    /// "zombie" | "stale_candidate" | "churn_candidate" — never "verdict".
    pub kind: String,
    /// Empty for heuristic candidates (they have no single fixing commit).
    pub verified_at_sha: String,
    pub evidence_refs: Vec<String>,
    pub checked_at: String,
}

fn freshness_row_key(kind: &str, issue_ref: &str) -> String {
    format!("{kind}:{issue_ref}")
}

pub(crate) fn save_freshness_row(
    server: &MemoryServer,
    namespace: &str,
    row: &FreshnessRow,
) -> Result<(), String> {
    let json = serde_json::to_string(row).map_err(|e| format!("serialize freshness row: {e}"))?;
    server.with_global_store(|store| -> Result<(), String> {
        store
            .set_state(
                namespace,
                &freshness_row_key(&row.kind, &row.issue_ref),
                &json,
            )
            .map_err(|e| format!("issue_freshness set_state: {e}"))?;
        Ok(())
    })
}

pub(crate) fn list_freshness_rows(
    server: &MemoryServer,
    namespace: &str,
) -> Result<Vec<FreshnessRow>, String> {
    let rows = server.with_global_store_read(|store| {
        store
            .list_state(namespace)
            .map_err(|e| format!("issue_freshness list_state: {e}"))
    })?;
    Ok(rows
        .into_iter()
        .filter_map(|row| serde_json::from_str::<FreshnessRow>(&row.value_json).ok())
        .collect())
}

/// Reap: delete every existing row of `kind` in `namespace` whose issue_ref is
/// NOT present in `current_issue_refs` (the fresh, authoritative hit set this
/// scan just produced). Returns the number of rows actually removed.
/// (#1000 codex review finding 7 — "review candidates never die": without
/// this, a zombie that got closed, or a stale candidate whose drift got
/// fixed, stays in the briefing forever because nothing ever
/// re-scans-and-clears rows that no longer reproduce.)
pub(crate) fn reap_stale_kind_rows(
    server: &MemoryServer,
    namespace: &str,
    kind: &str,
    current_issue_refs: &[String],
) -> Result<usize, String> {
    let existing = list_freshness_rows(server, namespace)?;
    let mut reaped = 0usize;
    for row in existing.into_iter().filter(|r| r.kind == kind) {
        if !current_issue_refs.contains(&row.issue_ref) {
            let removed = server.with_global_store(|store| {
                store
                    .delete_state(namespace, &freshness_row_key(&row.kind, &row.issue_ref))
                    .map_err(|e| format!("issue_freshness delete_state: {e}"))
            })?;
            if removed {
                reaped += 1;
            }
        }
    }
    Ok(reaped)
}

use serde::{Deserialize, Serialize};

/// Briefing projection (Scope item 3): both queues, capped, counts +
/// overflow markers — never a silent cap (same convention as
/// `scan_open_loops`). Reads only the already-stored candidate rows
/// (`state_kv`) — this function does not call `gh` itself; population of
/// those rows happens via `issue_freshness_scan` (a `tachi_gh` action) so
/// briefing reads stay cheap and offline-safe. `stale_candidates` folds both
/// heuristic kinds (`stale_candidate` + `churn_candidate`) into one bucket —
/// they are both "review this, no verdict" reasons from the caller's point of
/// view; the on-disk `kind` distinction only matters for reap correctness.
pub(crate) fn briefing_freshness_queues(server: &MemoryServer, limit: usize) -> Value {
    let zombie_rows_all = list_freshness_rows(server, ZOMBIE_NS).unwrap_or_default();
    let stale_rows_all = list_freshness_rows(server, STALE_CANDIDATE_NS).unwrap_or_default();

    let mut zombies: Vec<&FreshnessRow> = zombie_rows_all
        .iter()
        .filter(|v| v.kind == KIND_ZOMBIE)
        .collect();
    let mut stale: Vec<&FreshnessRow> = stale_rows_all
        .iter()
        .filter(|v| v.kind == KIND_STALE_CANDIDATE || v.kind == KIND_CHURN_CANDIDATE)
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
                "kind": v.kind,
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
            commit_messages: Vec::new(),
            merge_commit_message: String::new(),
        }
    }

    fn pr_with_commits(number: u64, title: &str, body: &str, commit_messages: &[&str]) -> MergedPr {
        MergedPr {
            commit_messages: commit_messages.iter().map(|s| s.to_string()).collect(),
            ..pr(number, title, body)
        }
    }

    /// #1000 round-3 codex review finding 1: a TRUE merge commit is a
    /// distinct commit from every commit `gh` reports in `commits[]` — its
    /// own message (e.g. GitHub's default "Merge pull request #N from
    /// branch" plus a body carrying "Refs #M") is otherwise invisible to the
    /// scan.
    fn pr_with_merge_commit_message(
        number: u64,
        title: &str,
        body: &str,
        merge_commit_message: &str,
    ) -> MergedPr {
        MergedPr {
            merge_commit_message: merge_commit_message.to_string(),
            ..pr(number, title, body)
        }
    }

    /// #1000 codex review finding 8: `gh pr list --json
    /// number,title,body,mergeCommit,commits` field-path parsing is
    /// fixture-tested against the REAL shape `gh` emits — not just asserted
    /// through the live I/O boundary. Shape mirrors `gh pr view --json commits`
    /// output (`commits[].messageHeadline` / `commits[].messageBody`).
    #[test]
    fn parse_merged_prs_json_extracts_commit_messages_from_real_gh_shape() {
        let value = serde_json::json!([
            {
                "number": 980,
                "title": "fix(vault): standard profile allow-list (#979)",
                "body": "Fixes #979.",
                "mergeCommit": { "oid": "abc123" },
                "commits": [
                    {
                        "messageHeadline": "fix(vault): standard profile allow-list",
                        "messageBody": "Refs #979"
                    },
                    {
                        "messageHeadline": "fixup: typo",
                        "messageBody": ""
                    }
                ]
            }
        ]);
        let prs = parse_merged_prs_json(&value);
        assert_eq!(prs.len(), 1);
        assert_eq!(prs[0].number, 980);
        assert_eq!(prs[0].merge_commit_sha.as_deref(), Some("abc123"));
        assert_eq!(prs[0].commit_messages.len(), 2);
        assert!(prs[0].commit_messages[0].contains("Refs #979"));
    }

    #[test]
    fn parse_merged_prs_json_defaults_missing_fields_without_panicking() {
        // `mergeCommit` and `commits` can be null/absent (e.g. a squash-merge
        // strategy or a PR fetched before `commits` was requested).
        let value = serde_json::json!([
            { "number": 1, "title": "t", "body": "b", "mergeCommit": null }
        ]);
        let prs = parse_merged_prs_json(&value);
        assert_eq!(prs.len(), 1);
        assert_eq!(prs[0].merge_commit_sha, None);
        assert!(prs[0].commit_messages.is_empty());
    }

    #[test]
    fn parse_merged_prs_json_skips_rows_missing_required_number_field() {
        let value = serde_json::json!([
            { "title": "no number field" },
            { "number": 2, "title": "t", "body": "b" }
        ]);
        let prs = parse_merged_prs_json(&value);
        assert_eq!(prs.len(), 1);
        assert_eq!(prs[0].number, 2);
    }

    /// #1000 codex review finding 8: `gh issue list --json
    /// number,body,updatedAt,comments` field-path parsing, including the
    /// most-recent-comment extraction from a real multi-comment shape.
    #[test]
    fn parse_open_issues_with_activity_json_extracts_latest_comment() {
        let value = serde_json::json!([
            {
                "number": 500,
                "body": "some body",
                "updatedAt": "2026-01-01T00:00:00Z",
                "comments": [
                    { "createdAt": "2026-02-01T00:00:00Z" },
                    { "createdAt": "2026-03-01T00:00:00Z" },
                    { "createdAt": "2026-01-15T00:00:00Z" }
                ]
            }
        ]);
        let rows = parse_open_issues_with_activity_json(&value);
        assert_eq!(rows.len(), 1);
        let (number, body, updated_at, last_comment_at) = &rows[0];
        assert_eq!(*number, 500);
        assert_eq!(body, "some body");
        assert_eq!(updated_at, "2026-01-01T00:00:00Z");
        assert_eq!(last_comment_at.as_deref(), Some("2026-03-01T00:00:00Z"));
    }

    #[test]
    fn parse_open_issues_with_activity_json_handles_no_comments() {
        let value = serde_json::json!([
            { "number": 1, "body": "", "updatedAt": "2026-01-01T00:00:00Z", "comments": [] }
        ]);
        let rows = parse_open_issues_with_activity_json(&value);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].3, None);
    }

    /// #1000 round-3 codex review finding 4: `gh issue list --json number`
    /// shape (shared by the open-issue-number fetch and the closed-issue-
    /// number fetch — both were still inline/untested, unlike their sibling
    /// parsers extracted under finding 8).
    #[test]
    fn parse_issue_numbers_json_covers_all_input_classes() {
        let cases = [
            (
                "extracts numbers",
                serde_json::json!([{ "number": 979 }, { "number": 947 }]),
                vec![979, 947],
            ),
            (
                "skips rows missing number",
                serde_json::json!([{ "number": 1 }, { "title": "no number field" }]),
                vec![1],
            ),
            ("empty array", serde_json::json!([]), Vec::<u64>::new()),
        ];

        for (name, value, expected) in cases {
            assert_eq!(parse_issue_numbers_json(&value), expected, "{name}");
        }
    }

    /// #1000 round-3 codex review finding 4: `gh issue list --json
    /// number,body` shape.
    #[test]
    fn parse_issue_numbers_with_body_json_covers_all_input_classes() {
        let cases = [
            (
                "extracts number and body",
                serde_json::json!([
                    { "number": 1000, "body": "gated on #894" },
                    { "number": 1001, "body": "" }
                ]),
                vec![(1000, "gated on #894".to_string()), (1001, String::new())],
            ),
            (
                "defaults missing body to empty",
                serde_json::json!([{ "number": 1 }]),
                vec![(1, String::new())],
            ),
            (
                "skips rows missing number",
                serde_json::json!([
                    { "body": "no number here" },
                    { "number": 2, "body": "b" }
                ]),
                vec![(2, "b".to_string())],
            ),
        ];

        for (name, value, expected) in cases {
            assert_eq!(
                parse_issue_numbers_with_body_json(&value),
                expected,
                "{name}"
            );
        }
    }

    /// #1000 codex review finding 8: `gh pr list --json number,files` shape.
    #[test]
    fn parse_merged_pr_surfaces_json_extracts_touched_paths() {
        let value = serde_json::json!([
            {
                "number": 42,
                "files": [
                    { "path": "crates/foo/src/bar.rs", "additions": 3, "deletions": 1 },
                    { "path": "crates/foo/src/baz.rs", "additions": 10, "deletions": 0 }
                ],
                "mergedAt": "2026-07-01T00:00:00Z"
            }
        ]);
        let surfaces = parse_merged_pr_surfaces_json(&value);
        assert_eq!(surfaces.len(), 1);
        assert_eq!(surfaces[0].pr_number, 42);
        assert_eq!(
            surfaces[0].touched_paths,
            vec![
                "crates/foo/src/bar.rs".to_string(),
                "crates/foo/src/baz.rs".to_string()
            ]
        );
        assert_eq!(surfaces[0].merged_at, "2026-07-01T00:00:00Z");
    }

    #[test]
    fn parse_merged_pr_surfaces_json_empty_files_yields_empty_paths() {
        let value = serde_json::json!([{ "number": 1, "files": [] }]);
        let surfaces = parse_merged_pr_surfaces_json(&value);
        assert_eq!(surfaces.len(), 1);
        assert!(surfaces[0].touched_paths.is_empty());
    }

    /// #1000 round-3 codex review finding 6: `mergedAt` absent/null must not
    /// panic — it degrades to an empty string, which correctly sorts before
    /// any real RFC3339 timestamp so the PR is treated as NOT recent by the
    /// churn window filter rather than always-included.
    #[test]
    fn parse_merged_pr_surfaces_json_missing_merged_at_defaults_empty() {
        let value = serde_json::json!([{ "number": 1, "files": [] }]);
        let surfaces = parse_merged_pr_surfaces_json(&value);
        assert_eq!(surfaces[0].merged_at, "");
    }

    #[test]
    fn extract_referenced_issue_numbers_covers_supported_and_rejected_forms() {
        let cases = [
            ("Refs line", "Fixes the bug.\n\nRefs #979", vec![979]),
            (
                "comma-separated Refs",
                "Refs #530, #724, #921",
                vec![530, 724, 921],
            ),
            (
                "space-separated Refs",
                "Refs #968 #517 #963",
                vec![517, 963, 968],
            ),
            (
                "parenthesized trailer",
                "some commit trailer (#42) landed",
                vec![42],
            ),
            ("singular Ref", "Ref #1", vec![1]),
            ("Reference", "Reference: #2", vec![2]),
            ("References", "References #3", vec![3]),
            ("Referenced", "Referenced #4", vec![4]),
            // "Refactor" must not match on "Ref" + word-boundary check.
            ("Refactor is not Ref", "Refactor #5", Vec::<u64>::new()),
            (
                "plain body reference",
                "just a plain body #5",
                Vec::<u64>::new(),
            ),
        ];

        for (name, text, expected) in cases {
            assert_eq!(extract_referenced_issue_numbers(text), expected, "{name}");
        }
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

    /// #1000 codex review finding 1: commit-only references (never repeated
    /// in the PR title/body) must not be missed.
    #[test]
    fn scan_zombies_catches_commit_only_reference() {
        let merged_prs = vec![pr_with_commits(
            42,
            "release: prepare v1.9.0",
            "no issue reference here in title or body",
            &["fix: squashed commit\n\nRefs #979"],
        )];
        let open_issue_numbers = vec![979];
        let hits = scan_zombies(&merged_prs, &open_issue_numbers);
        assert_eq!(
            hits.len(),
            1,
            "expected commit-only Refs to be caught, got: {hits:?}"
        );
        assert_eq!(hits[0].issue_number, 979);
        assert_eq!(hits[0].pr_number, 42);
    }

    #[test]
    fn scan_zombies_dedupes_when_same_ref_in_body_and_commit() {
        let merged_prs = vec![pr_with_commits(
            1,
            "fix",
            "Refs #100",
            &["same commit message\n\nRefs #100"],
        )];
        let hits = scan_zombies(&merged_prs, &[100]);
        assert_eq!(
            hits.len(),
            1,
            "body+commit repeat must dedupe, got: {hits:?}"
        );
    }

    /// #1000 round-3 codex review finding 1: a `Refs #N` that appears ONLY in
    /// the merge commit's own message (not the PR title, body, or any commit
    /// in `commits[]`) must still be caught. This is the exact gap codex
    /// flagged: `gh pr list --json mergeCommit` only returns `{oid}`, no
    /// message text — the scan used to have no way to see a merge-commit-only
    /// reference at all.
    #[test]
    fn scan_zombies_catches_merge_commit_only_reference() {
        let merged_prs = vec![pr_with_merge_commit_message(
            55,
            "release: batch of fixes",
            "no issue reference in the PR title or body",
            "Merge pull request #55 from feature-branch\n\nRefs #1000",
        )];
        let hits = scan_zombies(&merged_prs, &[1000]);
        assert_eq!(
            hits.len(),
            1,
            "expected merge-commit-only Refs to be caught, got: {hits:?}"
        );
        assert_eq!(hits[0].issue_number, 1000);
        assert_eq!(hits[0].pr_number, 55);
    }

    #[test]
    fn scan_zombies_dedupes_when_same_ref_in_body_and_merge_commit() {
        let merged_prs = vec![pr_with_merge_commit_message(
            1,
            "fix",
            "Refs #100",
            "Merge pull request #1\n\nRefs #100",
        )];
        let hits = scan_zombies(&merged_prs, &[100]);
        assert_eq!(
            hits.len(),
            1,
            "body+merge-commit repeat must dedupe, got: {hits:?}"
        );
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
        let mut file_probes = std::collections::HashMap::new();
        file_probes.insert("src/foo.rs".to_string(), FileProbe::Found(50)); // file shrank
        let (out, warnings) = scan_stale_candidates(&issues, &file_probes, &[]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].issue_number, 1);
        assert!(warnings.is_empty());
    }

    #[test]
    fn stale_candidate_flags_deleted_file() {
        let issues = vec![stale_issue(2, &[("src/gone.rs", 10)], &[])];
        let mut file_probes = std::collections::HashMap::new();
        file_probes.insert("src/gone.rs".to_string(), FileProbe::NotFound);
        let (out, warnings) = scan_stale_candidates(&issues, &file_probes, &[]);
        assert_eq!(out.len(), 1);
        assert!(out[0].evidence[0].contains("no longer exists"));
        assert!(warnings.is_empty());
    }

    /// #1000 codex review finding 4: an unreadable (not merely missing) file
    /// must surface as a scan warning, never as a stale signal.
    #[test]
    fn stale_candidate_unreadable_file_is_warning_not_stale_signal() {
        let issues = vec![stale_issue(9, &[("src/perm-denied.rs", 10)], &[])];
        let mut file_probes = std::collections::HashMap::new();
        file_probes.insert(
            "src/perm-denied.rs".to_string(),
            FileProbe::Unreadable("permission denied".to_string()),
        );
        let (out, warnings) = scan_stale_candidates(&issues, &file_probes, &[]);
        assert!(
            out.is_empty(),
            "unreadable file must not become a stale candidate, got: {out:?}"
        );
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("perm-denied.rs"));
        assert!(warnings[0].contains("permission denied"));
    }

    #[test]
    fn stale_candidate_flags_closed_gate_dependency() {
        let issues = vec![stale_issue(3, &[], &[42])];
        let (out, warnings) =
            scan_stale_candidates(&issues, &std::collections::HashMap::new(), &[42]);
        assert_eq!(out.len(), 1);
        assert!(out[0].evidence[0].contains("#42"));
        assert!(warnings.is_empty());
    }

    #[test]
    fn stale_candidate_clean_issue_not_flagged() {
        let issues = vec![stale_issue(4, &[("src/ok.rs", 5)], &[7])];
        let mut file_probes = std::collections::HashMap::new();
        file_probes.insert("src/ok.rs".to_string(), FileProbe::Found(500));
        let (out, _warnings) = scan_stale_candidates(&issues, &file_probes, &[]); // gate 7 not closed
        assert!(out.is_empty());
    }

    /// #1000 codex review finding 3: a plain `Refs #N` in the issue body must
    /// NOT be treated as a gate dependency — only "gated on"/"blocked by"/
    /// "depends on #N" counts.
    #[test]
    fn extract_gate_issue_numbers_ignores_plain_refs() {
        let body = "This work continues #734. Refs #906 #428.";
        assert_eq!(
            extract_gate_issue_numbers(body),
            Vec::<u64>::new(),
            "plain Refs must not be treated as a gate"
        );
    }

    #[test]
    fn extract_gate_issue_numbers_catches_explicit_gate_phrases() {
        assert_eq!(extract_gate_issue_numbers("gated on #894"), vec![894]);
        assert_eq!(extract_gate_issue_numbers("blocked by #12"), vec![12]);
        assert_eq!(extract_gate_issue_numbers("depends on #55"), vec![55]);
        assert_eq!(
            extract_gate_issue_numbers("Some context. Gated On #77 for now."),
            vec![77],
            "case-insensitive"
        );
    }

    /// #1000 round-3 codex review finding 2 ("gate 短语连坐" — gate-phrase
    /// guilt-by-association): the codex-reported regression case — a plain
    /// `Refs #N` sharing the same sentence as a gate phrase must NOT be
    /// swept in as a gate too. Before the fix, `extract_hash_numbers` scanned
    /// the WHOLE gate-marked sentence, so "gated on #1 (Refs #2)" wrongly
    /// attributed #2 as a gate dependency alongside #1.
    #[test]
    fn extract_gate_issue_numbers_does_not_attribute_trailing_refs_in_same_sentence() {
        assert_eq!(
            extract_gate_issue_numbers("gated on #1 (Refs #2)"),
            vec![1],
            "only #1 (immediately after the gate phrase) is a gate; #2 is a plain Refs mention"
        );
    }

    /// Positive control paired with the regression test above: a genuine
    /// second gate phrase in the same sentence must still attribute its own
    /// `#N` correctly (not conflated with the first phrase's target).
    #[test]
    fn extract_gate_issue_numbers_attributes_multiple_gate_phrases_in_one_sentence() {
        assert_eq!(
            extract_gate_issue_numbers("gated on #1, blocked by #2"),
            vec![1, 2],
            "two distinct gate phrases in the same sentence each attribute their own target"
        );
    }

    /// Direction 2 (both directions per the finding's test requirement):
    /// closing a merely-*related* issue (plain Refs) must not falsely mark
    /// this issue stale, but closing an explicit *gate* issue must.
    #[test]
    fn stale_candidate_gate_vs_related_both_directions() {
        // Direction A: plain Refs to a closed issue — NOT a gate, must not flag.
        let related_only = OpenIssueForStaleCheck {
            number: 10,
            file_line_anchors: vec![],
            gate_issue_numbers: extract_gate_issue_numbers("Refs #500 for background"),
        };
        let (out, _) =
            scan_stale_candidates(&[related_only], &std::collections::HashMap::new(), &[500]);
        assert!(
            out.is_empty(),
            "closing a merely-related issue must not flag as stale, got: {out:?}"
        );

        // Direction B: explicit gate to a closed issue — IS a gate, must flag.
        let gated = OpenIssueForStaleCheck {
            number: 11,
            file_line_anchors: vec![],
            gate_issue_numbers: extract_gate_issue_numbers("gated on #500 until it lands"),
        };
        let (out, _) = scan_stale_candidates(&[gated], &std::collections::HashMap::new(), &[500]);
        assert_eq!(
            out.len(),
            1,
            "closing an explicit gate dependency must flag as stale"
        );
    }

    /// #1000 round-3 codex review finding 2, end-to-end through
    /// `scan_stale_candidates`: an issue body reading "gated on #1 (Refs #2)"
    /// must only go stale when the GATE (#1) closes — #2 closing alone (the
    /// plain Refs mention riding along in the same sentence) must not flag
    /// the issue, because #2 was never actually a gate dependency.
    #[test]
    fn stale_candidate_gate_attribution_ignores_trailing_ref_in_gate_sentence() {
        let issue = OpenIssueForStaleCheck {
            number: 20,
            file_line_anchors: vec![],
            gate_issue_numbers: extract_gate_issue_numbers("gated on #1 (Refs #2)"),
        };
        // Only #2 (the trailing plain-Refs mention) is closed — #1 (the real
        // gate) stays open. Must NOT flag: #2 was never a gate dependency.
        let (out, _) =
            scan_stale_candidates(&[issue.clone()], &std::collections::HashMap::new(), &[2]);
        assert!(
            out.is_empty(),
            "closing #2 (a plain Refs mention, not the gate) must not flag issue #20, got: {out:?}"
        );

        // Now the real gate (#1) closes — must flag.
        let (out, _) = scan_stale_candidates(&[issue], &std::collections::HashMap::new(), &[1]);
        assert_eq!(
            out.len(),
            1,
            "closing #1 (the actual gate) must flag issue #20"
        );
    }

    fn churn_issue(
        number: u64,
        surface_paths: &[&str],
        has_recent_activity: bool,
    ) -> OpenIssueForChurnCheck {
        OpenIssueForChurnCheck {
            number,
            surface_paths: surface_paths.iter().map(|s| s.to_string()).collect(),
            has_recent_activity,
        }
    }

    fn pr_surface(pr_number: u64, touched_paths: &[&str]) -> MergedPrSurface {
        MergedPrSurface {
            pr_number,
            touched_paths: touched_paths.iter().map(|s| s.to_string()).collect(),
            merged_at: String::new(),
        }
    }

    /// #1000 Scope item 2 / codex review finding 2: an inactive issue whose
    /// surface has been touched by >= threshold distinct merged PRs is a
    /// churn candidate.
    #[test]
    fn scan_same_surface_churn_flags_inactive_issue_with_enough_touching_prs() {
        let issues = vec![churn_issue(1, &["src/foo.rs"], false)];
        let recent_prs = vec![
            pr_surface(10, &["src/foo.rs"]),
            pr_surface(11, &["src/foo.rs", "src/bar.rs"]),
            pr_surface(12, &["src/foo.rs"]),
        ];
        let out = scan_same_surface_churn(&issues, &recent_prs, 3);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].issue_number, 1);
        assert_eq!(out[0].touching_pr_count, 3);
        assert_eq!(out[0].touching_pr_numbers, vec![10, 11, 12]);
    }

    #[test]
    fn scan_same_surface_churn_ignores_issue_with_recent_activity() {
        // Same surface/PR shape as the flagged case above, but the issue has
        // recent activity — must NOT be flagged even though the code churned.
        let issues = vec![churn_issue(2, &["src/foo.rs"], true)];
        let recent_prs = vec![
            pr_surface(10, &["src/foo.rs"]),
            pr_surface(11, &["src/foo.rs"]),
            pr_surface(12, &["src/foo.rs"]),
        ];
        let out = scan_same_surface_churn(&issues, &recent_prs, 3);
        assert!(
            out.is_empty(),
            "issue with recent activity must not be flagged, got: {out:?}"
        );
    }

    #[test]
    fn scan_same_surface_churn_requires_meeting_threshold() {
        let issues = vec![churn_issue(3, &["src/foo.rs"], false)];
        let recent_prs = vec![
            pr_surface(10, &["src/foo.rs"]),
            pr_surface(11, &["src/foo.rs"]),
        ];
        // Only 2 distinct touching PRs, threshold is 3 — must not flag.
        let out = scan_same_surface_churn(&issues, &recent_prs, 3);
        assert!(
            out.is_empty(),
            "below-threshold churn must not flag, got: {out:?}"
        );
    }

    #[test]
    fn scan_same_surface_churn_ignores_issue_with_no_surface_anchors() {
        let issues = vec![churn_issue(4, &[], false)];
        let recent_prs = vec![
            pr_surface(10, &["src/foo.rs"]),
            pr_surface(11, &["src/foo.rs"]),
            pr_surface(12, &["src/foo.rs"]),
        ];
        let out = scan_same_surface_churn(&issues, &recent_prs, 3);
        assert!(
            out.is_empty(),
            "an issue with no file-surface anchors has nothing to cross-check, must not flag"
        );
    }

    #[test]
    fn scan_same_surface_churn_dedupes_pr_touching_multiple_anchor_paths() {
        // Issue anchors two paths in the SAME PR's file list — that PR must
        // only count once toward the threshold, not twice.
        let issues = vec![churn_issue(5, &["src/foo.rs", "src/bar.rs"], false)];
        let recent_prs = vec![pr_surface(10, &["src/foo.rs", "src/bar.rs"])];
        let out = scan_same_surface_churn(&issues, &recent_prs, 1);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].touching_pr_count, 1);
    }

    /// #1000 round-3 codex review finding 6: `churn_threshold == 0` must NOT
    /// flag an issue with ZERO touching PRs. Before the clamp,
    /// `touching_pr_numbers.len() >= 0` (always true) meant every inactive
    /// issue with a non-empty file-surface flagged regardless of actual
    /// churn evidence — a false-positive storm on any repo with idle specs.
    #[test]
    fn scan_same_surface_churn_threshold_zero_does_not_flag_untouched_surface() {
        let issues = vec![churn_issue(6, &["src/untouched.rs"], false)];
        let recent_prs: Vec<MergedPrSurface> = Vec::new(); // zero touching PRs
        let out = scan_same_surface_churn(&issues, &recent_prs, 0);
        assert!(
            out.is_empty(),
            "threshold=0 with zero touching PRs must not flag, got: {out:?}"
        );
    }

    /// Positive control paired with the threshold=0 regression test: with the
    /// clamp in place (effective threshold 1), an issue that DOES have at
    /// least one genuinely touching PR must still flag at threshold=0 — the
    /// clamp changes the floor, not whether real churn evidence counts.
    #[test]
    fn scan_same_surface_churn_threshold_zero_still_flags_when_actually_touched() {
        let issues = vec![churn_issue(7, &["src/touched.rs"], false)];
        let recent_prs = vec![pr_surface(20, &["src/touched.rs"])];
        let out = scan_same_surface_churn(&issues, &recent_prs, 0);
        assert_eq!(
            out.len(),
            1,
            "threshold=0 clamped to 1 must still flag a genuinely touched issue"
        );
    }

    fn pr_surface_at(pr_number: u64, touched_paths: &[&str], merged_at: &str) -> MergedPrSurface {
        MergedPrSurface {
            pr_number,
            touched_paths: touched_paths.iter().map(|s| s.to_string()).collect(),
            merged_at: merged_at.to_string(),
        }
    }

    /// #1000 round-3 codex review finding 6: a PR merged well BEFORE the
    /// activity window must not count as churn evidence, even though
    /// `--limit` (a PR count) alone would have let it through.
    #[test]
    fn filter_merged_prs_since_excludes_prs_merged_before_cutoff() {
        let merged_prs = vec![
            pr_surface_at(1, &["src/foo.rs"], "2024-01-01T00:00:00Z"), // old
            pr_surface_at(2, &["src/foo.rs"], "2026-07-05T00:00:00Z"), // recent
        ];
        let filtered = filter_merged_prs_since(merged_prs, "2026-06-01T00:00:00Z");
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].pr_number, 2);
    }

    #[test]
    fn filter_merged_prs_since_includes_pr_merged_exactly_at_cutoff() {
        let merged_prs = vec![pr_surface_at(1, &["src/foo.rs"], "2026-06-01T00:00:00Z")];
        let filtered = filter_merged_prs_since(merged_prs, "2026-06-01T00:00:00Z");
        assert_eq!(filtered.len(), 1, "cutoff itself is inclusive (>=)");
    }

    #[test]
    fn filter_merged_prs_since_excludes_pr_with_missing_merged_at() {
        // An empty merged_at (missing/null `mergedAt` field) must be treated
        // as NOT recent, never always-included.
        let merged_prs = vec![pr_surface_at(1, &["src/foo.rs"], "")];
        let filtered = filter_merged_prs_since(merged_prs, "2026-01-01T00:00:00Z");
        assert!(
            filtered.is_empty(),
            "missing mergedAt must not be treated as recent"
        );
    }

    /// End-to-end through the pure scan: a PR merged outside the window must
    /// not count toward the churn threshold, even if it touches the exact
    /// same surface as PRs that DO fall in the window.
    #[test]
    fn scan_same_surface_churn_windowed_prs_below_threshold_does_not_flag() {
        let issues = vec![churn_issue(8, &["src/foo.rs"], false)];
        let all_prs = vec![
            pr_surface_at(10, &["src/foo.rs"], "2026-07-05T00:00:00Z"), // in window
            pr_surface_at(11, &["src/foo.rs"], "2024-01-01T00:00:00Z"), // stale, filtered out
            pr_surface_at(12, &["src/foo.rs"], "2023-01-01T00:00:00Z"), // stale, filtered out
        ];
        let windowed = filter_merged_prs_since(all_prs, "2026-06-01T00:00:00Z");
        // Only 1 PR survives the window — below threshold 3.
        let out = scan_same_surface_churn(&issues, &windowed, 3);
        assert!(
            out.is_empty(),
            "PRs merged outside the activity window must not count toward churn threshold, got: {out:?}"
        );
    }

    #[test]
    fn freshness_row_roundtrips_through_json() {
        let row = FreshnessRow {
            issue_ref: "kckylechen1/tachi#979".to_string(),
            kind: KIND_ZOMBIE.to_string(),
            verified_at_sha: "deadbeef".to_string(),
            evidence_refs: vec!["kckylechen1/tachi#980".to_string()],
            checked_at: "2026-07-11T00:00:00Z".to_string(),
        };
        let json = serde_json::to_string(&row).expect("serialize");
        let back: FreshnessRow = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.issue_ref, row.issue_ref);
        assert_eq!(back.kind, row.kind);
    }

    fn test_server() -> MemoryServer {
        test_server_with_path().0
    }

    /// Same as `test_server()`, but also hands back the on-disk sqlite path
    /// so a test can reach in and break the DB out from under the server
    /// (e.g. `reap_stale_kind_rows_surfaces_a_real_db_failure_as_err` below,
    /// which needs a REAL `delete_state` I/O failure — not a mocked one — to
    /// prove the honesty wiring end-to-end).
    fn test_server_with_path() -> (MemoryServer, std::path::PathBuf) {
        let db = std::env::temp_dir().join(format!(
            "issue-freshness-test-{}.sqlite",
            uuid::Uuid::new_v4()
        ));
        let server = MemoryServer::new(db.clone(), None).expect("test server");
        (server, db)
    }

    fn row(issue_ref: &str, kind: &str, evidence: &[&str]) -> FreshnessRow {
        FreshnessRow {
            issue_ref: issue_ref.to_string(),
            kind: kind.to_string(),
            verified_at_sha: "sha1".to_string(),
            evidence_refs: evidence.iter().map(|s| s.to_string()).collect(),
            checked_at: "2026-07-11T00:00:00Z".to_string(),
        }
    }

    #[test]
    fn save_and_list_freshness_rows_roundtrips_via_state_kv() {
        let server = test_server();
        save_freshness_row(
            &server,
            ZOMBIE_NS,
            &row("o/r#979", KIND_ZOMBIE, &["o/r#980"]),
        )
        .expect("save zombie");
        save_freshness_row(
            &server,
            STALE_CANDIDATE_NS,
            &row("o/r#500", KIND_STALE_CANDIDATE, &[]),
        )
        .expect("save stale");

        let zombies = list_freshness_rows(&server, ZOMBIE_NS).expect("list zombies");
        assert_eq!(zombies.len(), 1);
        assert!(zombies
            .iter()
            .any(|v| v.issue_ref == "o/r#979" && v.kind == KIND_ZOMBIE));

        let stale = list_freshness_rows(&server, STALE_CANDIDATE_NS).expect("list stale");
        assert_eq!(stale.len(), 1);
        assert!(stale
            .iter()
            .any(|v| v.issue_ref == "o/r#500" && v.kind == KIND_STALE_CANDIDATE));
    }

    /// #1000 codex review finding 7: kind+issue_ref keys — a zombie row and a
    /// stale-candidate row for the SAME issue must not collide/overwrite each
    /// other even though (pre-fix) both used to key off `issue_ref` alone.
    #[test]
    fn save_freshness_row_does_not_collide_across_kinds_for_same_issue() {
        let server = test_server();
        save_freshness_row(
            &server,
            STALE_CANDIDATE_NS,
            &row("o/r#1", KIND_STALE_CANDIDATE, &[]),
        )
        .expect("save stale");
        save_freshness_row(
            &server,
            STALE_CANDIDATE_NS,
            &row("o/r#1", KIND_CHURN_CANDIDATE, &["o/r#2"]),
        )
        .expect("save churn");

        let all = list_freshness_rows(&server, STALE_CANDIDATE_NS).expect("list");
        assert_eq!(
            all.len(),
            2,
            "same issue_ref, different kind, must coexist as two rows, got: {all:?}"
        );
        assert!(all.iter().any(|v| v.kind == KIND_STALE_CANDIDATE));
        assert!(all.iter().any(|v| v.kind == KIND_CHURN_CANDIDATE));
    }

    #[test]
    fn save_freshness_row_overwrites_same_kind_and_issue_ref() {
        let server = test_server();
        save_freshness_row(&server, ZOMBIE_NS, &row("o/r#1", KIND_ZOMBIE, &["o/r#2"]))
            .expect("save first");
        save_freshness_row(&server, ZOMBIE_NS, &row("o/r#1", KIND_ZOMBIE, &["o/r#3"]))
            .expect("save second");

        let all = list_freshness_rows(&server, ZOMBIE_NS).expect("list");
        assert_eq!(
            all.len(),
            1,
            "same kind+issue_ref must overwrite, not duplicate"
        );
        assert_eq!(all[0].evidence_refs, vec!["o/r#3".to_string()]);
    }

    /// #1000 codex review finding 7 acceptance test: a zombie that the leader
    /// closed on GitHub must disappear from the briefing after the NEXT scan
    /// reaps it — it must not live forever as a ghost row just because it was
    /// saved once.
    #[test]
    fn reap_stale_kind_rows_drops_rows_missing_from_fresh_hit_set() {
        let server = test_server();
        save_freshness_row(
            &server,
            ZOMBIE_NS,
            &row("o/r#979", KIND_ZOMBIE, &["o/r#980"]),
        )
        .expect("save");
        save_freshness_row(
            &server,
            ZOMBIE_NS,
            &row("o/r#947", KIND_ZOMBIE, &["o/r#981"]),
        )
        .expect("save");

        // Briefing sees both before the reap.
        let before = briefing_freshness_queues(&server, 8);
        assert_eq!(before["zombies"]["count"], 2);

        // Next scan only reproduces #947 (the leader closed #979 by hand) —
        // reap must drop #979's row, keep #947's.
        let reaped =
            reap_stale_kind_rows(&server, ZOMBIE_NS, KIND_ZOMBIE, &["o/r#947".to_string()])
                .expect("reap");
        assert_eq!(reaped, 1, "expected exactly #979's row reaped");

        let after = briefing_freshness_queues(&server, 8);
        assert_eq!(after["zombies"]["count"], 1, "closed zombie must be gone");
        let refs: Vec<&str> = after["zombies"]["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v["issue_ref"].as_str().unwrap())
            .collect();
        assert!(!refs.contains(&"o/r#979"));
        assert!(refs.contains(&"o/r#947"));
    }

    #[test]
    fn reap_stale_kind_rows_only_touches_its_own_kind() {
        let server = test_server();
        save_freshness_row(
            &server,
            STALE_CANDIDATE_NS,
            &row("o/r#1", KIND_STALE_CANDIDATE, &[]),
        )
        .expect("save stale");
        save_freshness_row(
            &server,
            STALE_CANDIDATE_NS,
            &row("o/r#1", KIND_CHURN_CANDIDATE, &[]),
        )
        .expect("save churn");

        // A churn-kind scan that no longer sees #1 must reap ONLY the churn
        // row, leaving the unrelated stale_candidate row for the same issue
        // untouched (each scan is authoritative for its own kind only).
        let reaped = reap_stale_kind_rows(&server, STALE_CANDIDATE_NS, KIND_CHURN_CANDIDATE, &[])
            .expect("reap");
        assert_eq!(reaped, 1);

        let remaining = list_freshness_rows(&server, STALE_CANDIDATE_NS).expect("list");
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].kind, KIND_STALE_CANDIDATE);
    }

    /// PR #1004 round-4 codex review — the merge-blocker: round-3 finding 3
    /// added `reap_errors`/`reap_incomplete_kinds` to the response, but the
    /// implementer never actually wired a test that injects a REAL reap
    /// failure (a mocked `Err` doesn't prove anything about the real
    /// `MemoryStore::delete_state` I/O path). This test poisons the store
    /// handle for real — a second raw connection to the SAME sqlite file
    /// drops the `hard_state` table the `state_kv` operations depend on out
    /// from under the already-open `MemoryServer` — and asserts
    /// `reap_stale_kind_rows` surfaces the resulting I/O error as `Err`,
    /// matching what `router.rs`'s `handle_issue_freshness_scan` needs to
    /// correctly populate `reap_errors` / `reap_incomplete_kinds` instead of
    /// silently reporting "0 reaped, all good" for a kind whose ghost rows
    /// never actually got cleared.
    #[test]
    fn reap_stale_kind_rows_surfaces_a_real_db_failure_as_err() {
        let (server, db_path) = test_server_with_path();
        save_freshness_row(
            &server,
            ZOMBIE_NS,
            &row("o/r#979", KIND_ZOMBIE, &["o/r#980"]),
        )
        .expect("save");

        // Sanity baseline: the row is really there before we poison the
        // store — guards against a vacuously-"passing" test where the save
        // itself silently no-opped.
        let before = list_freshness_rows(&server, ZOMBIE_NS).expect("list before poison");
        assert_eq!(before.len(), 1, "row must exist before the poison step");

        // Poison the store handle: a second raw connection to the same
        // sqlite file drops the table `delete_state`/`list_state` read and
        // write through. The `MemoryServer`'s own connection stays open and
        // "healthy" from Rust's point of view — this is a real DB-level
        // failure (missing table), not a mocked error path.
        let raw = rusqlite::Connection::open(&db_path).expect("raw connection to same db");
        raw.execute_batch("DROP TABLE hard_state;")
            .expect("drop hard_state table out from under the server");
        drop(raw);

        let result = reap_stale_kind_rows(&server, ZOMBIE_NS, KIND_ZOMBIE, &[]);

        let err = result.expect_err(
            "reap against a store whose backing table was dropped must surface a real Err, not silently return Ok(0)",
        );
        assert!(
            err.contains("issue_freshness"),
            "expected reap_stale_kind_rows' own error wrapping (list_state/delete_state), got: {err}"
        );
    }

    #[test]
    fn briefing_freshness_queues_splits_by_kind_with_counts() {
        let server = test_server();
        save_freshness_row(
            &server,
            ZOMBIE_NS,
            &row("o/r#979", KIND_ZOMBIE, &["o/r#980"]),
        )
        .expect("save");
        save_freshness_row(
            &server,
            ZOMBIE_NS,
            &row("o/r#947", KIND_ZOMBIE, &["o/r#981"]),
        )
        .expect("save");
        save_freshness_row(
            &server,
            STALE_CANDIDATE_NS,
            &row("o/r#500", KIND_STALE_CANDIDATE, &[]),
        )
        .expect("save");

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
    fn briefing_freshness_queues_folds_both_heuristic_kinds_into_stale_bucket() {
        let server = test_server();
        save_freshness_row(
            &server,
            STALE_CANDIDATE_NS,
            &row("o/r#1", KIND_STALE_CANDIDATE, &[]),
        )
        .expect("save");
        save_freshness_row(
            &server,
            STALE_CANDIDATE_NS,
            &row("o/r#2", KIND_CHURN_CANDIDATE, &[]),
        )
        .expect("save");

        let out = briefing_freshness_queues(&server, 8);
        assert_eq!(
            out["stale_candidates"]["count"], 2,
            "both stale_candidate and churn_candidate kinds fold into one bucket"
        );
    }

    #[test]
    fn briefing_freshness_queues_reports_overflow_never_silently_caps() {
        let server = test_server();
        for n in 0..5 {
            save_freshness_row(
                &server,
                ZOMBIE_NS,
                &row(&format!("o/r#{n}"), KIND_ZOMBIE, &[]),
            )
            .expect("save");
        }
        let out = briefing_freshness_queues(&server, 2);
        assert_eq!(out["zombies"]["count"], 5);
        assert_eq!(out["zombies"]["items"].as_array().unwrap().len(), 2);
        assert_eq!(out["zombies"]["overflow"], 3);
    }

    #[test]
    fn briefing_freshness_queues_empty_when_no_rows_saved() {
        let server = test_server();
        let out = briefing_freshness_queues(&server, 8);
        assert_eq!(out["zombies"]["count"], 0);
        assert_eq!(out["stale_candidates"]["count"], 0);
        assert!(out["zombies"]["items"].as_array().unwrap().is_empty());
    }
}
