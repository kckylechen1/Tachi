//! #1105: live per-relation evidence and commit-reachability shipped checks
//! that REPLACE #1002 v1's conservative defaults — every relation state
//! normalized to `Unknown`, `scope_collisions` always empty,
//! `shipped_evidence` always `None` (see `refinery_ops::mod`'s own module
//! doc, "Known scope gap" paragraph, for the exact v1 behavior this leaf
//! closes).
//!
//! Reuses the EXISTING `gh` read surface rather than inventing a new
//! GitHub primitive (#1002's own frozen contract: "Do not add a GitHub
//! primitive or a new execution backend"):
//!   - `tachi_gh(issue_read)` (via `handle_tachi_gh`, already used for the
//!     primary issue) for each related target's live OPEN/CLOSED state;
//!   - the #1000 zombie-scan's own merged-PR fetch
//!     (`gh_ops::fetch_merged_prs` + `gh_ops::extract_referenced_issue_numbers`
//!     — the same "does a merged PR's title/body/commit messages/merge-commit
//!     message reference issue #N" text scan `scan_zombies` already uses,
//!     generalized here from "the open-issue zombie set" to any target
//!     number) for shipped-PR candidates;
//!   - the `DocRefResolver` already threaded through `build_refinery_packet`
//!     (its new `is_commit_reachable`, canon doc §5 delivery-state ownership
//!     table: "shipped ... requires reachability from an owner-controlled
//!     main/release ref") for the commit-reachability verification itself —
//!     no second I/O boundary, and live vs. test behavior stays governed by
//!     the SAME resolver injection `build_refinery_packet` already uses.
//!
//! Everything in this file is a PURE, deterministic assembly step over
//! already-fetched data (mirrors `doc_resolver`'s split between pure
//! resolve logic and the live shelling in `GitRefResolver`): the async `gh`
//! orchestration lives in `mod.rs::handle_refine_issues`, and ANY failure
//! there (a `gh` error, an unparseable response, a git error inside the
//! injected resolver) degrades that ONE relation/shipped-check back to the
//! v1-conservative value — `derive_live_relation_signals` never panics and
//! never fabricates a signal from absent data; a target_ref with no entry
//! in `related_live_states` stays `Unknown`, exactly like v1.
//!
//! fix-round-2 (cross-vendor adversarial review, PR #1191, codex-b4d8f
//! checkpoint 1, BLOCKING): "degrades back to the v1-conservative value" is
//! now load-bearing for the CLOSED+shipped-check case too, not just a
//! missing `related_live_states` entry — see [`ShippedCheckOutcome`] and
//! [`derive_live_relation_signals`]'s `merged_pr_scan_complete` parameter
//! for exactly which conditions may produce the confirmed-negative
//! `ClosedUnshipped` versus degrade to `Unknown`. A collection/reachability
//! outcome this file cannot actually confirm — a merged-PR fetch failure, a
//! bounded/truncated scan (the merged-PR pool is capped, canon doc §4.3
//! manual/on-demand posture, not an unbounded scan), or a `git`-level
//! `Unavailable` reachability check — is Unknown, never a manufactured
//! negative.
//!
//! `scope_collision` derivation (this leaf's own convention, not
//! canon-frozen — same posture as `parse.rs`'s `[state]` annotation syntax):
//! a `Duplicate-Of:` relation whose live-verified target state is `Open`
//! (both issues still live/unresolved, genuinely competing for the same
//! scope) is reported as a scope collision. This is deliberately NOT a
//! free-text/title-similarity classifier — canon doc §10 explicitly
//! abandons "free-text substring verdict classifiers" — it is a structured
//! signal keyed off the already-frozen `Duplicate-Of:` relation kind plus a
//! real live cross-reference lookup.

use super::disposition::RelatedIssueStateV1;
use super::doc_resolver::{CommitReachability, DocRefResolver};
use tachi_params::RepoRevisionV1;

/// Whether a commit-reachability shipped check was performed for a CLOSED
/// related issue, and what it found. Distinct from a plain `bool` so
/// "checked, confirmed unshipped" (a real negative signal) never collapses
/// into the same value as "never checked" (cross-repo relations, or a
/// target this leaf's scope doesn't cover — #1105 limits the shipped check
/// to same-repo relations, see `derive_live_relation_signals`) — conflating
/// the two would let an honestly-unchecked relation manufacture a false
/// `ClosedUnshipped` (itself a real disposition input, e.g. DORMANT).
///
/// fix-round-2 (cross-vendor adversarial review, PR #1191, codex-b4d8f
/// checkpoint 1, BLOCKING): the original two-outcome-plus-`NotChecked` shape
/// let `NotShipped` mean BOTH "the merged-PR search actually completed and
/// genuinely found nothing" (a real negative) AND "the search was
/// incomplete/failed/uncertain" (the merged-PR fetch itself errored, the
/// scan was bounded and might be missing an older shipping PR, or a
/// candidate's own commit-reachability check was `Unavailable` rather than a
/// confirmed `NotReachable`) — every one of those uncertain cases was
/// silently promoted to the SAME confirmed-negative `ClosedUnshipped` a real
/// check would produce, which is not fail-closed. `Incomplete` is the new
/// bucket for all of those: it maps to `Unknown` (v1's own default),
/// EXACTLY like `NotChecked` — see `classify_related_state`. `NotShipped`
/// now only fires when [`derive_live_relation_signals`] can show the
/// merged-PR search was actually exhaustive (see `merged_pr_scan_complete`)
/// AND no candidate's reachability check came back `Unavailable`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ShippedCheckOutcome {
    Shipped,
    NotShipped,
    Incomplete,
    NotChecked,
}

/// Derive a related issue's live state from its fetched `state` field
/// ("OPEN"/"CLOSED", case-insensitive — `None` when the lookup was never
/// attempted or failed) plus the shipped-check outcome for CLOSED issues.
/// Any other/missing live state degrades to `Unknown` (fail-closed, v1's
/// own default). `Incomplete` (fix-round-2: uncertain collection/
/// reachability) degrades to `Unknown` exactly like `NotChecked` — only a
/// genuinely confirmed `Shipped`/`NotShipped` outcome may produce
/// `ClosedShipped`/`ClosedUnshipped`.
pub(crate) fn classify_related_state(
    live_issue_state: Option<&str>,
    shipped: ShippedCheckOutcome,
) -> RelatedIssueStateV1 {
    match live_issue_state {
        Some(s) if s.eq_ignore_ascii_case("OPEN") => RelatedIssueStateV1::Open,
        Some(s) if s.eq_ignore_ascii_case("CLOSED") => match shipped {
            ShippedCheckOutcome::Shipped => RelatedIssueStateV1::ClosedShipped,
            ShippedCheckOutcome::NotShipped => RelatedIssueStateV1::ClosedUnshipped,
            ShippedCheckOutcome::Incomplete | ShippedCheckOutcome::NotChecked => {
                RelatedIssueStateV1::Unknown
            }
        },
        _ => RelatedIssueStateV1::Unknown,
    }
}

/// Every already-fetched merged PR whose title, body, commit messages, or
/// merge-commit message references `target_number` — the same text surface
/// `gh_ops::issue_freshness::scan_zombies` scans (#1000), generalized here
/// to any target number instead of only the currently-open zombie set.
pub(crate) fn merged_prs_referencing(
    merged_prs: &[crate::gh_ops::MergedPr],
    target_number: u64,
) -> Vec<&crate::gh_ops::MergedPr> {
    merged_prs
        .iter()
        .filter(|pr| {
            let mut text = format!("{}\n{}", pr.title, pr.body);
            for commit_message in &pr.commit_messages {
                text.push('\n');
                text.push_str(commit_message);
            }
            if !pr.merge_commit_message.is_empty() {
                text.push('\n');
                text.push_str(&pr.merge_commit_message);
            }
            crate::gh_ops::extract_referenced_issue_numbers(&text).contains(&target_number)
        })
        .collect()
}

/// Pick the shipped evidence among `referencing_prs`: the lowest PR-numbered
/// candidate (deterministic tie-break) whose merge commit is confirmed
/// reachable from `trusted_ref` via `resolver.is_commit_reachable` (#1105
/// item 2, canon doc §5). Returns `(evidence, any_unavailable)`: `evidence`
/// is `None` when no candidate has BOTH a merge commit and a
/// confirmed-reachable one — "referenced by a merged PR" alone is not
/// "shipped" (canon doc §5: `shipped` requires the reachability check, not
/// merely `merged` delivery state). `any_unavailable` (fix-round-2, PR #1191
/// checkpoint 1) is `true` iff at least one candidate's reachability check
/// came back `CommitReachability::Unavailable` rather than a confirmed
/// `Reachable`/`NotReachable` — a caller MUST treat `(None, true)` as
/// "could not determine", never as a confirmed negative (see
/// `derive_live_relation_signals`'s `ShippedCheckOutcome::Incomplete`).
pub(crate) fn pick_shipped_evidence(
    repo: &str,
    trusted_ref: &str,
    referencing_prs: &[&crate::gh_ops::MergedPr],
    resolver: &dyn DocRefResolver,
    verified_at: &str,
) -> (Option<RepoRevisionV1>, bool) {
    let mut any_unavailable = false;
    let evidence = referencing_prs
        .iter()
        .filter_map(|pr| {
            let sha = pr.merge_commit_sha.as_deref()?;
            match resolver.is_commit_reachable(repo, sha, trusted_ref) {
                CommitReachability::Reachable => Some((pr.number, sha.to_string())),
                CommitReachability::NotReachable => None,
                CommitReachability::Unavailable(_) => {
                    any_unavailable = true;
                    None
                }
            }
        })
        .min_by_key(|(pr_number, _)| *pr_number)
        .map(|(_, commit_sha)| RepoRevisionV1 {
            repo: repo.to_string(),
            git_ref: trusted_ref.to_string(),
            commit_sha,
            verified_at: verified_at.to_string(),
        });
    (evidence, any_unavailable)
}

/// One relation target this issue's body/comments declared, resolved to a
/// concrete `(repo, number)` GitHub target — `None` repo/number means the
/// target_ref text could not be parsed into a real target (stays `Unknown`,
/// never looked up).
#[derive(Debug, Clone)]
pub(crate) struct RelationLookupTarget {
    pub(crate) target_ref: String,
    pub(crate) repo: String,
    pub(crate) number: u64,
}

/// Live signals #1105 derives to replace v1's conservative defaults.
/// `Default` (empty map, `None` shipped evidence) is EXACTLY v1's behavior —
/// used both as the base for `build_refinery_packet`'s back-compat wrapper
/// (fixture tests keep calling it with zero live signals) and as the safe
/// fallback whenever the live async orchestration can't populate a field.
#[derive(Debug, Clone, Default)]
pub(crate) struct LiveRelationSignals {
    /// target_ref (exactly as it appears in the relation line) -> its
    /// live-derived state. A target_ref absent here was never looked up
    /// (parse failure) or its lookup failed — the caller (`mod.rs`) must
    /// treat that the same as "not present", which
    /// `build_refinery_packet_with_live_signals` already does via
    /// `HashMap::get(...).unwrap_or(Unknown)`.
    pub(crate) related_states: std::collections::HashMap<String, RelatedIssueStateV1>,
    /// This issue's OWN shipped evidence (#1105 items 1+2): a merged PR
    /// referencing this issue, with a commit-reachability-verified merge
    /// commit. `None` when no such evidence was found, or the live lookup
    /// failed/was skipped — identical to v1's always-`None` default.
    pub(crate) own_shipped_evidence: Option<RepoRevisionV1>,
}

/// Assemble `LiveRelationSignals` from already-fetched pieces — the
/// deterministic, I/O-free (beyond the injected `resolver`'s own
/// `is_commit_reachable`, which is itself test-injectable) step the async
/// orchestrator calls after it has fetched everything.
///
/// `related_live_states`: target_ref -> live `state` string ("OPEN"/
/// "CLOSED"), populated only for target_refs whose `issue_read` succeeded.
/// `targets`: every relation target this issue declared, already resolved
/// to `(repo, number)` (or omitted if unparseable — the caller filters
/// those out before calling this).
/// `merged_prs`: recently-merged PRs for `base_repo` (already fetched via
/// `gh_ops::fetch_merged_prs`) — the shipped-evidence candidate pool.
/// `merged_pr_scan_complete` (fix-round-2, PR #1191 checkpoint 1): `true`
/// iff `merged_prs` is KNOWN to be the repo's ENTIRE merged-PR history (the
/// caller's `gh pr list --limit N` returned strictly fewer than `N` results,
/// meaning nothing was cut off by the bound) AND the fetch itself did not
/// error. The merged-PR scan this leaf runs is bounded (`mod.rs`'s
/// `LIVE_SIGNAL_MERGED_PR_SCAN_LIMIT`) precisely because it is manual/
/// on-demand, never resident (canon doc §4.3) — "not found in the most
/// recent N merged PRs" can NEVER be promoted to a confirmed negative unless
/// the caller can show there was no Nth-PR cutoff at all. When `false` (scan
/// was truncated, or the fetch failed and the caller passed an empty/partial
/// `merged_prs`), a same-repo CLOSED target with no reachable evidence
/// degrades to `ShippedCheckOutcome::Incomplete` (-> `Unknown`), never
/// `NotShipped` (-> `ClosedUnshipped`).
///
/// Shipped-ness (commit-reachability) is only ever evaluated for targets in
/// `base_repo` — a cross-repo relation can still be reported Open/Closed
/// from `related_live_states`, but never ClosedShipped/ClosedUnshipped
/// (this leaf's commit-reachability check is local-git, single-repo; a
/// cross-repo relation stays `Unknown` on close rather than a fabricated
/// "checked, not shipped").
///
/// fix-round-2: this function already had 8 parameters before
/// `merged_pr_scan_complete` (a 9th) was added to close checkpoint 1's
/// fail-closed gap — `#[allow(clippy::too_many_arguments)]` here matches the
/// same pattern `doc_resolver::resolve` already uses in this leaf, rather
/// than leaving `cargo clippy --locked ... -D warnings` (the exact command
/// this PR's own body asks Oz to run) newly red because of a 9th positional
/// arg.
#[allow(clippy::too_many_arguments)]
pub(crate) fn derive_live_relation_signals(
    base_repo: &str,
    base_number: u64,
    trusted_ref: &str,
    targets: &[RelationLookupTarget],
    related_live_states: &std::collections::HashMap<String, String>,
    merged_prs: &[crate::gh_ops::MergedPr],
    merged_pr_scan_complete: bool,
    resolver: &dyn DocRefResolver,
    captured_at: &str,
) -> LiveRelationSignals {
    let mut related_states = std::collections::HashMap::new();
    for target in targets {
        let live_state = related_live_states
            .get(&target.target_ref)
            .map(String::as_str);
        let shipped = if target.repo == base_repo {
            let referencing = merged_prs_referencing(merged_prs, target.number);
            let (evidence, any_unavailable) =
                pick_shipped_evidence(base_repo, trusted_ref, &referencing, resolver, captured_at);
            match evidence {
                Some(_) => ShippedCheckOutcome::Shipped,
                None if merged_pr_scan_complete && !any_unavailable => {
                    ShippedCheckOutcome::NotShipped
                }
                None => ShippedCheckOutcome::Incomplete,
            }
        } else {
            ShippedCheckOutcome::NotChecked
        };
        related_states.insert(
            target.target_ref.clone(),
            classify_related_state(live_state, shipped),
        );
    }

    // `own_shipped_evidence` "not found" already IS v1's own default
    // (`None`) regardless of scan completeness — unlike `related_states`
    // there is no stronger "confirmed unshipped" claim to guard here, so the
    // `any_unavailable` half of `pick_shipped_evidence`'s return is not
    // needed for this half of the derivation.
    let own_referencing = merged_prs_referencing(merged_prs, base_number);
    let (own_shipped_evidence, _) = pick_shipped_evidence(
        base_repo,
        trusted_ref,
        &own_referencing,
        resolver,
        captured_at,
    );

    LiveRelationSignals {
        related_states,
        own_shipped_evidence,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gh_ops::MergedPr;
    use crate::refinery_ops::fixtures::FixtureDocResolver;

    const REPO: &str = "kckylechen1/tachi";
    const TRUSTED_REF: &str = "origin/main";
    const CAPTURED_AT: &str = "2026-07-17T00:00:00Z";

    fn merged_pr(number: u64, body: &str, merge_commit_sha: &str) -> MergedPr {
        MergedPr {
            number,
            title: String::new(),
            body: body.to_string(),
            merge_commit_sha: Some(merge_commit_sha.to_string()),
            commit_messages: Vec::new(),
            merge_commit_message: String::new(),
        }
    }

    #[test]
    fn classify_related_state_open_wins_regardless_of_shipped_outcome() {
        assert_eq!(
            classify_related_state(Some("OPEN"), ShippedCheckOutcome::NotChecked),
            RelatedIssueStateV1::Open
        );
        assert_eq!(
            classify_related_state(Some("open"), ShippedCheckOutcome::Shipped),
            RelatedIssueStateV1::Open,
            "case-insensitive"
        );
    }

    #[test]
    fn classify_related_state_closed_with_shipped_evidence_is_closed_shipped() {
        assert_eq!(
            classify_related_state(Some("CLOSED"), ShippedCheckOutcome::Shipped),
            RelatedIssueStateV1::ClosedShipped
        );
    }

    #[test]
    fn classify_related_state_closed_without_shipped_evidence_is_closed_unshipped() {
        assert_eq!(
            classify_related_state(Some("CLOSED"), ShippedCheckOutcome::NotShipped),
            RelatedIssueStateV1::ClosedUnshipped
        );
    }

    /// The whole point of the three-way `ShippedCheckOutcome`: a CLOSED
    /// cross-repo relation that was never checked must NOT silently become
    /// `ClosedUnshipped` (a real negative disposition input) — it stays
    /// `Unknown`, honestly reporting "we don't know", exactly like v1.
    #[test]
    fn classify_related_state_closed_but_not_checked_stays_unknown() {
        assert_eq!(
            classify_related_state(Some("CLOSED"), ShippedCheckOutcome::NotChecked),
            RelatedIssueStateV1::Unknown
        );
    }

    /// fix-round-2 (PR #1191 checkpoint 1, required item 1): `Incomplete`
    /// (uncertain collection/reachability — a bounded/partial merged-PR
    /// scan, a merged-PR fetch failure, or an `Unavailable` reachability
    /// check) must degrade to `Unknown` exactly like `NotChecked`, NEVER to
    /// the confirmed-negative `ClosedUnshipped`.
    #[test]
    fn classify_related_state_closed_but_incomplete_stays_unknown() {
        assert_eq!(
            classify_related_state(Some("CLOSED"), ShippedCheckOutcome::Incomplete),
            RelatedIssueStateV1::Unknown,
            "an incomplete/uncertain shipped-check must not manufacture a confirmed negative"
        );
    }

    #[test]
    fn classify_related_state_missing_or_unrecognized_state_is_unknown() {
        assert_eq!(
            classify_related_state(None, ShippedCheckOutcome::NotChecked),
            RelatedIssueStateV1::Unknown
        );
        assert_eq!(
            classify_related_state(Some("MERGED"), ShippedCheckOutcome::Shipped),
            RelatedIssueStateV1::Unknown,
            "a PR-shaped state string must not be treated as a related issue's open/closed state"
        );
    }

    #[test]
    fn merged_prs_referencing_matches_body_title_and_commit_messages() {
        let prs = vec![
            merged_pr(10, "Refs #1105", "sha-10"),
            MergedPr {
                commit_messages: vec!["fix: thing\n\nRefs #1105".to_string()],
                ..merged_pr(11, "unrelated body", "sha-11")
            },
            merged_pr(12, "Refs #9999", "sha-12"),
        ];
        let hits = merged_prs_referencing(&prs, 1105);
        let numbers: Vec<u64> = hits.iter().map(|pr| pr.number).collect();
        assert_eq!(numbers, vec![10, 11]);
    }

    #[test]
    fn merged_prs_referencing_returns_empty_when_nothing_references_target() {
        let prs = vec![merged_pr(10, "Refs #1", "sha-10")];
        assert!(merged_prs_referencing(&prs, 999).is_empty());
    }

    #[test]
    fn pick_shipped_evidence_requires_reachability_not_just_a_reference() {
        let prs = [merged_pr(10, "Refs #1105", "sha-unreachable")];
        let referencing: Vec<&MergedPr> = prs.iter().collect();
        let resolver = FixtureDocResolver::new(); // nothing registered reachable
        let (evidence, any_unavailable) =
            pick_shipped_evidence(REPO, TRUSTED_REF, &referencing, &resolver, CAPTURED_AT);
        assert!(
            evidence.is_none(),
            "a referencing PR whose merge commit was never confirmed reachable must not count as shipped"
        );
        assert!(
            !any_unavailable,
            "an unregistered fixture commit is a confirmed NotReachable, not Unavailable"
        );
    }

    #[test]
    fn pick_shipped_evidence_returns_lowest_numbered_reachable_candidate() {
        let prs = [
            merged_pr(20, "Refs #1105", "sha-20"),
            merged_pr(15, "Refs #1105", "sha-15"),
        ];
        let referencing: Vec<&MergedPr> = prs.iter().collect();
        let resolver = FixtureDocResolver::new()
            .with_reachable_commit(REPO, "sha-20", TRUSTED_REF)
            .with_reachable_commit(REPO, "sha-15", TRUSTED_REF);
        let (evidence, any_unavailable) =
            pick_shipped_evidence(REPO, TRUSTED_REF, &referencing, &resolver, CAPTURED_AT);
        let evidence = evidence.expect("both reachable, must pick one");
        assert_eq!(
            evidence.commit_sha, "sha-15",
            "lowest PR number wins the tie-break"
        );
        assert_eq!(evidence.repo, REPO);
        assert_eq!(evidence.git_ref, TRUSTED_REF);
        assert!(!any_unavailable);
    }

    /// fix-round-2 (PR #1191 checkpoint 1): a candidate whose reachability
    /// check is `Unavailable` (a real git failure/absent-locally commit, the
    /// fixture equivalent of `GitRefResolver` hitting a `git` error) must
    /// set `any_unavailable`, even though the same call also has no positive
    /// evidence — the caller must NOT read `(None, true)` as a confirmed
    /// negative.
    #[test]
    fn pick_shipped_evidence_reports_unavailable_reachability_separately_from_not_reachable() {
        let prs = [merged_pr(10, "Refs #1105", "sha-flaky")];
        let referencing: Vec<&MergedPr> = prs.iter().collect();
        let resolver = FixtureDocResolver::new().with_unavailable_commit(
            REPO,
            "sha-flaky",
            TRUSTED_REF,
            "git cat-file -e failed: no such file or directory",
        );
        let (evidence, any_unavailable) =
            pick_shipped_evidence(REPO, TRUSTED_REF, &referencing, &resolver, CAPTURED_AT);
        assert!(evidence.is_none());
        assert!(
            any_unavailable,
            "an Unavailable reachability check must be surfaced, not silently dropped"
        );
    }

    #[test]
    fn derive_live_relation_signals_gives_own_shipped_evidence_when_reachable() {
        let merged_prs = vec![merged_pr(50, "Refs #1105", "sha-50")];
        let resolver = FixtureDocResolver::new().with_reachable_commit(REPO, "sha-50", TRUSTED_REF);
        let signals = derive_live_relation_signals(
            REPO,
            1105,
            TRUSTED_REF,
            &[],
            &std::collections::HashMap::new(),
            &merged_prs,
            true,
            &resolver,
            CAPTURED_AT,
        );
        let evidence = signals
            .own_shipped_evidence
            .expect("a reachable referencing merge commit must produce shipped evidence");
        assert_eq!(evidence.commit_sha, "sha-50");
    }

    #[test]
    fn derive_live_relation_signals_v1_default_matches_empty_inputs() {
        // No targets, no merged PRs, resolver with nothing registered — the
        // whole point of #1105's `Default` posture: zero live data in must
        // produce EXACTLY v1's always-empty/None signals, never a
        // fabricated one.
        let resolver = FixtureDocResolver::new();
        let signals = derive_live_relation_signals(
            REPO,
            1105,
            TRUSTED_REF,
            &[],
            &std::collections::HashMap::new(),
            &[],
            true,
            &resolver,
            CAPTURED_AT,
        );
        assert!(signals.related_states.is_empty());
        assert!(signals.own_shipped_evidence.is_none());
    }

    #[test]
    fn derive_live_relation_signals_cross_repo_closed_target_stays_unknown_not_unshipped() {
        let targets = vec![RelationLookupTarget {
            target_ref: "other/repo#1".to_string(),
            repo: "other/repo".to_string(),
            number: 1,
        }];
        let mut live_states = std::collections::HashMap::new();
        live_states.insert("other/repo#1".to_string(), "CLOSED".to_string());
        let resolver = FixtureDocResolver::new();
        let signals = derive_live_relation_signals(
            REPO,
            1105,
            TRUSTED_REF,
            &targets,
            &live_states,
            &[],
            true,
            &resolver,
            CAPTURED_AT,
        );
        assert_eq!(
            signals.related_states.get("other/repo#1").copied(),
            Some(RelatedIssueStateV1::Unknown),
            "a closed cross-repo relation this leaf never shipped-checked must stay Unknown"
        );
    }

    /// The genuinely-confirmable case: the merged-PR scan is KNOWN complete
    /// (`merged_pr_scan_complete = true`, the fixture equivalent of `gh pr
    /// list --limit N` returning strictly fewer than `N` results — nothing
    /// was cut off by the bound) and every candidate's reachability check
    /// resolved (no `Unavailable`) — a same-repo CLOSED target with no
    /// referencing merged PR is a REAL checked negative.
    #[test]
    fn derive_live_relation_signals_complete_scan_no_evidence_confirms_unshipped() {
        let targets = vec![
            RelationLookupTarget {
                target_ref: "#10".to_string(),
                repo: REPO.to_string(),
                number: 10,
            },
            RelationLookupTarget {
                target_ref: "#11".to_string(),
                repo: REPO.to_string(),
                number: 11,
            },
        ];
        let mut live_states = std::collections::HashMap::new();
        live_states.insert("#10".to_string(), "CLOSED".to_string());
        live_states.insert("#11".to_string(), "CLOSED".to_string());
        let merged_prs = vec![merged_pr(99, "Refs #10", "sha-99")];
        let resolver = FixtureDocResolver::new().with_reachable_commit(REPO, "sha-99", TRUSTED_REF);
        let signals = derive_live_relation_signals(
            REPO,
            1105,
            TRUSTED_REF,
            &targets,
            &live_states,
            &merged_prs,
            true, // scan complete: gh returned every merged PR, none cut off
            &resolver,
            CAPTURED_AT,
        );
        assert_eq!(
            signals.related_states.get("#10").copied(),
            Some(RelatedIssueStateV1::ClosedShipped)
        );
        assert_eq!(
            signals.related_states.get("#11").copied(),
            Some(RelatedIssueStateV1::ClosedUnshipped),
            "#11 was closed, the merged-PR scan was exhaustive, and no PR referenced it — a real, checked negative"
        );
    }

    /// fix-round-2 (cross-vendor adversarial review, PR #1191, codex-b4d8f
    /// checkpoint 1, "Bounded 100-PR window: 'not found in partial scan'
    /// treated as confirmed negative. Not fail-closed."): the SAME inputs as
    /// the complete-scan test above, except `merged_pr_scan_complete =
    /// false` (the realistic case: `gh pr list --limit 100` returned exactly
    /// 100, or the merged-PR fetch itself failed and the caller degraded to
    /// an empty list) — #11 must stay `Unknown`, NOT `ClosedUnshipped`. This
    /// is the collector-level failure-discrimination test #1191 required:
    /// an incomplete/failed collection degrades that ONE signal back to v1's
    /// conservative default, exactly like a `gh`/`git` call that errored
    /// outright.
    #[test]
    fn derive_live_relation_signals_incomplete_scan_stays_unknown_not_unshipped() {
        let targets = vec![
            RelationLookupTarget {
                target_ref: "#10".to_string(),
                repo: REPO.to_string(),
                number: 10,
            },
            RelationLookupTarget {
                target_ref: "#11".to_string(),
                repo: REPO.to_string(),
                number: 11,
            },
        ];
        let mut live_states = std::collections::HashMap::new();
        live_states.insert("#10".to_string(), "CLOSED".to_string());
        live_states.insert("#11".to_string(), "CLOSED".to_string());
        let merged_prs = vec![merged_pr(99, "Refs #10", "sha-99")];
        let resolver = FixtureDocResolver::new().with_reachable_commit(REPO, "sha-99", TRUSTED_REF);
        let signals = derive_live_relation_signals(
            REPO,
            1105,
            TRUSTED_REF,
            &targets,
            &live_states,
            &merged_prs,
            false, // scan bounded/incomplete (or the fetch itself failed)
            &resolver,
            CAPTURED_AT,
        );
        assert_eq!(
            signals.related_states.get("#10").copied(),
            Some(RelatedIssueStateV1::ClosedShipped),
            "positive evidence found within the scan is still trustworthy regardless of completeness"
        );
        assert_eq!(
            signals.related_states.get("#11").copied(),
            Some(RelatedIssueStateV1::Unknown),
            "#11 was closed but the merged-PR scan was incomplete — 'not found in a partial scan' \
             must never be promoted to a confirmed negative (v1 degradation)"
        );
    }

    /// The same v1-degradation guarantee, this time driven by an
    /// `Unavailable` reachability check (a real git failure) rather than an
    /// incomplete scan — even with `merged_pr_scan_complete = true`, a
    /// candidate whose reachability could not be determined must not let
    /// "no confirmed-reachable candidate" collapse into a confirmed
    /// negative.
    #[test]
    fn derive_live_relation_signals_unavailable_reachability_stays_unknown_not_unshipped() {
        let targets = vec![RelationLookupTarget {
            target_ref: "#12".to_string(),
            repo: REPO.to_string(),
            number: 12,
        }];
        let mut live_states = std::collections::HashMap::new();
        live_states.insert("#12".to_string(), "CLOSED".to_string());
        let merged_prs = vec![merged_pr(100, "Refs #12", "sha-flaky")];
        let resolver = FixtureDocResolver::new().with_unavailable_commit(
            REPO,
            "sha-flaky",
            TRUSTED_REF,
            "git merge-base --is-ancestor failed",
        );
        let signals = derive_live_relation_signals(
            REPO,
            1105,
            TRUSTED_REF,
            &targets,
            &live_states,
            &merged_prs,
            true, // scan complete, but the one candidate's reachability was Unavailable
            &resolver,
            CAPTURED_AT,
        );
        assert_eq!(
            signals.related_states.get("#12").copied(),
            Some(RelatedIssueStateV1::Unknown),
            "an Unavailable reachability check must not be promoted to a confirmed negative even \
             when the merged-PR scan itself was exhaustive"
        );
    }
}
