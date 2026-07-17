//! Exact current-work anchor grounding for `tachi_memory(action='ask')`
//! (#1071 — kckylechen1/tachi#1071).
//!
//! Frozen design authority: `docs/engineering/architecture/issue-refinery-memory-lanes.md`
//! §6 (recall and ask are evidence composition). This module implements the
//! "resolve exact issue/PR/doc/path/id/SHA anchors before semantic
//! expansion" requirement, scoped to exact GitHub issue anchors only (see
//! [`extract_exact_issue_anchors`] for why bare `#N` is out of scope here).
//!
//! # Why this is split pure-core / live-wrapper
//!
//! [`compile_current_work_anchor`] is a pure function over an already-fetched
//! (or already-failed) `gh issue view --json ...` result — it never touches
//! the network and is fully unit-tested with fixture JSON, mirroring
//! `refinery_ops::build_refinery_packet`'s injected-result pattern (#1002).
//! [`resolve_current_work_anchors`] is the thin live wrapper that calls
//! [`crate::gh_ops::read_issue_snapshot_bounded`] (timeout + kill-on-drop,
//! see that function's doc comment for why a live gh call must never be a
//! bare, un-bounded shell-out on this hot path) and feeds the result into
//! the pure core. Per this repo's existing policy for live git/gh shelling
//! (see `refinery_ops::doc_resolver::GitRefResolver`'s own doc comment),
//! the live wrapper itself is not unit-tested against a real `gh` process.
//!
//! # Known scope gaps (flagged, not hidden)
//!
//! - Bare `#N` anchors are not resolved — `ask` has no default-repo context.
//! - Only issue anchors are resolved; PR/doc/path/SHA anchors (also named
//!   in the frozen contract's "Required behavior" bullet) are not
//!   implemented in this leaf.
//! - `claim_coverage` is a coarse 0.0/1.0 "did the anchor resolve" signal,
//!   not per-claim source-span coverage (see `recall_evidence` module doc).
//! - Independent per-source-kind candidate budgets (canon doc §6.1 steps
//!   2-4) are not implemented; only `current_work` gets a dedicated,
//!   never-truncated partition (anchor rows are prepended before the
//!   existing scaffold's top-k/sort logic runs, so they cannot be crowded
//!   out by generic evidence volume — see [`prepend_anchor_evidence_rows`]).

use serde_json::{json, Value};

use crate::task_lifecycle::{parse_issue_ref, GithubTarget};
use crate::tool_params::{
    AuthorityClassV1, ContradictionV1, GroundingStatusV1, RecallEvidenceV1, SourceKindV1,
};
use crate::MemoryServer;

/// A query mentioning more exact anchors than this is still answered, but
/// only the first N are live-resolved — an unbounded fan-out of `gh`
/// processes (even timeout-bounded ones) is not a reasonable cost for a
/// single `ask` call.
const MAX_RESOLVED_ANCHORS_PER_ASK: usize = 3;

/// Punctuation that commonly trails a token in free-text prose but is never
/// part of a GitHub owner/repo/issue-number/URL token itself.
const TRAILING_TOKEN_PUNCTUATION: &[char] =
    &['.', ',', ';', ':', '!', '?', ')', ']', '}', '\'', '"', '`'];

/// Pure: scan free-text `query` for exact, unambiguous GitHub issue anchors —
/// `owner/repo#N` and `https://github.com/owner/repo/issues/N` tokens.
///
/// Deliberately does NOT resolve bare `#N` — unlike `tachi_task`'s
/// `issue_ref`/`params.repo` fields, `tachi_memory(action='ask')` carries no
/// default-repo context, so a bare `#N` in free text has no safe repo to
/// resolve it against (resolving it against an arbitrary "current" repo
/// guess would itself be a grounding hazard, not a fix for one).
pub(crate) fn extract_exact_issue_anchors(query: &str) -> Vec<GithubTarget> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for raw_token in query.split_whitespace() {
        let token = raw_token.trim_matches(|c: char| TRAILING_TOKEN_PUNCTUATION.contains(&c));
        if token.is_empty() || token.starts_with('#') {
            continue;
        }
        if let Some(target) = parse_issue_ref(token, None) {
            if seen.insert((target.repo.clone(), target.number)) {
                out.push(target);
            }
        }
    }
    out
}

fn missing_anchor_row(source_ref: &str, resolved_at: &str, reason: &str) -> RecallEvidenceV1 {
    RecallEvidenceV1 {
        kind: SourceKindV1::Issue,
        authority: AuthorityClassV1::CurrentWork,
        lifecycle: "unknown".to_string(),
        source_ref: source_ref.to_string(),
        source_revision: String::new(),
        valid_at: resolved_at.to_string(),
        retrieval_score: 0.0,
        claim_coverage: 0.0,
        contradictions: vec![ContradictionV1 {
            description: reason.to_string(),
            evidence_refs: Vec::new(),
        }],
        grounding_status: GroundingStatusV1::MissingAnchor,
    }
}

/// Pure: turn an already-fetched (or already-failed) `gh issue view --json
/// ...` result into a typed [`RecallEvidenceV1`] current-work row. Never
/// touches the network — see module doc.
pub(crate) fn compile_current_work_anchor(
    target: &GithubTarget,
    gh_result: Result<&Value, &str>,
    resolved_at: &str,
) -> RecallEvidenceV1 {
    let source_ref = format!("{}#{}", target.repo, target.number);
    let issue = match gh_result {
        Ok(issue) => issue,
        Err(reason) => return missing_anchor_row(&source_ref, resolved_at, reason),
    };

    // #1071 fix-round checkpoint 3: a snapshot that parses as JSON but is
    // missing the fields `read_issue_snapshot_bounded` always requests
    // (`number,title,state,body,labels,milestone,updatedAt,comments`) is
    // itself a malformed/truncated `gh` response — treating it as fully
    // `Grounded`/`claim_coverage: 1.0` would fabricate completeness the
    // response never actually proved. `body`/`milestone`/`comments` are
    // deliberately NOT required here: they are legitimately absent/empty on
    // a real, healthy issue (no body text, no milestone, zero comments);
    // `title`/`state`/`labels`/`updatedAt` are not.
    let number_matches = issue.get("number").and_then(Value::as_u64) == Some(target.number);
    let state = issue.get("state").and_then(Value::as_str);
    let has_title = issue.get("title").and_then(Value::as_str).is_some();
    let has_updated_at = issue.get("updatedAt").and_then(Value::as_str).is_some();
    let has_labels = issue.get("labels").is_some();
    if !number_matches || state.is_none() || !has_title || !has_updated_at || !has_labels {
        return missing_anchor_row(
            &source_ref,
            resolved_at,
            "gh issue view result did not include the complete requested field set \
             (number/state/title/updatedAt/labels) — malformed or truncated gh response",
        );
    }
    let state = state.unwrap_or("unknown").to_ascii_lowercase();
    let updated_at = issue
        .get("updatedAt")
        .and_then(Value::as_str)
        .unwrap_or(resolved_at)
        .to_string();

    // Semantic-state revision basis (title/state/body/labels/updatedAt) —
    // deliberately excludes identity fields (number/repo), same rationale
    // as #1002's `IssueSnapshotV1::compute_snapshot_hash`: identity churn
    // (e.g. a transfer) must not by itself change the revision the anchor
    // is pinned to.
    let revision_basis = json!({
        "title": issue.get("title"),
        "state": issue.get("state"),
        "body": issue.get("body"),
        "labels": issue.get("labels"),
        "updatedAt": issue.get("updatedAt"),
    });
    let source_revision = crate::tool_params::canonical_json_sha256(&revision_basis)
        .unwrap_or_else(|_| "unknown".to_string());

    RecallEvidenceV1 {
        kind: SourceKindV1::Issue,
        authority: AuthorityClassV1::CurrentWork,
        lifecycle: state,
        source_ref,
        source_revision,
        valid_at: updated_at,
        retrieval_score: 1.0,
        claim_coverage: 1.0,
        contradictions: Vec::new(),
        grounding_status: GroundingStatusV1::Grounded,
    }
}

/// Live wrapper: live-resolve up to [`MAX_RESOLVED_ANCHORS_PER_ASK`] exact
/// anchors via [`crate::gh_ops::read_issue_snapshot_bounded`] (each call
/// individually timeout-bounded and kill-on-drop — see that function's doc
/// comment). Never panics and never propagates a network error to the
/// caller: a failed/timed-out resolve becomes a `MissingAnchor` row, not an
/// `Err`, because "the requested anchor could not be confirmed live" is
/// itself the correct, honest answer `ask` must surface — not a facade
/// failure.
///
/// #1071 fix-round checkpoint 2: a query naming MORE than
/// [`MAX_RESOLVED_ANCHORS_PER_ASK`] anchors must never silently drop the
/// extras — every requested anchor gets a row. Anchors beyond the cap are
/// NOT live-fetched (an unbounded `gh` fan-out is still not a reasonable
/// per-call cost), but they ARE surfaced as `MissingAnchor` rows with an
/// explicit "beyond cap, not resolved" reason, so `compute_anchor_gated_confidence`
/// correctly caps confidence instead of silently evaluating a truncated
/// anchor set as if it were complete.
pub(crate) async fn resolve_current_work_anchors(
    server: &MemoryServer,
    targets: &[GithubTarget],
) -> Vec<RecallEvidenceV1> {
    let resolved_at = chrono::Utc::now().to_rfc3339();
    let mut out = Vec::with_capacity(targets.len());
    for (index, target) in targets.iter().enumerate() {
        if index >= MAX_RESOLVED_ANCHORS_PER_ASK {
            let source_ref = format!("{}#{}", target.repo, target.number);
            out.push(missing_anchor_row(
                &source_ref,
                &resolved_at,
                &format!(
                    "requested anchor beyond the {MAX_RESOLVED_ANCHORS_PER_ASK}-anchor \
                     live-resolve cap for a single ask call — not fetched, treated as \
                     unresolved rather than silently dropped"
                ),
            ));
            continue;
        }
        let fetch =
            crate::gh_ops::read_issue_snapshot_bounded(server, &target.repo, target.number).await;
        let row = match &fetch {
            Ok(value) => compile_current_work_anchor(target, Ok(value), &resolved_at),
            Err(err) => compile_current_work_anchor(target, Err(err.as_str()), &resolved_at),
        };
        out.push(row);
    }
    out
}

/// Confidence rule (frozen contract): "`ask` confidence comes only from
/// required-anchor coverage, claim coverage, authority, and contradictions
/// ... Synthesis may lower or summarize confidence, never raise it." When
/// the query requested exact anchors, this REPLACES (not blends with) the
/// generic evidence-volume heuristic — a pile of unrelated advisory hits
/// must never manufacture "high" confidence about a specific anchor that
/// failed to resolve (RED corpus case 1).
///
/// #1071 fix-round checkpoint 3: literally checks all four factors the
/// frozen rule names — `grounding_status` (required-anchor coverage),
/// `claim_coverage`, `authority`, and `contradictions` — rather than only
/// `grounding_status`. `authority` is checked because only `CurrentWork`-
/// authority rows may grant "high" confidence here: an anchor row this
/// leaf did not itself construct with `CurrentWork` authority has no basis
/// for being treated as required-anchor coverage at all (RED corpus case
/// 4, "same ids but wrong authority").
pub(crate) fn compute_anchor_gated_confidence(
    required_anchors: &[RecallEvidenceV1],
) -> &'static str {
    let any_ungrounded = required_anchors.iter().any(|anchor| {
        anchor.grounding_status == GroundingStatusV1::MissingAnchor
            || anchor.authority != AuthorityClassV1::CurrentWork
            || anchor.claim_coverage < 1.0
            || !anchor.contradictions.is_empty()
    });
    if any_ungrounded {
        "low"
    } else {
        "high"
    }
}

/// Overall grounding status for the whole `ask` response: `missing_anchor`
/// if ANY requested exact anchor failed to resolve, `grounded` otherwise
/// (including when no exact anchors were requested at all — plain semantic
/// queries are unaffected by this leaf, see module doc scope gaps).
pub(crate) fn overall_grounding_status(required_anchors: &[RecallEvidenceV1]) -> GroundingStatusV1 {
    if required_anchors
        .iter()
        .any(|anchor| anchor.grounding_status == GroundingStatusV1::MissingAnchor)
    {
        GroundingStatusV1::MissingAnchor
    } else {
        GroundingStatusV1::Grounded
    }
}

/// Render each resolved anchor as a synthetic evidence row shaped like the
/// rows `evidence_format::evidence_rows`/`evidence_score` already expect
/// (`relevance`, `path`, `summary`, `topic`, `section`), and PREPEND them
/// ahead of the generic search evidence. `build_thinking_scaffold` re-sorts
/// by score, so a grounded anchor's `relevance=1.0` naturally sorts first;
/// prepending (rather than only score-boosting) additionally guarantees the
/// anchor wins ties against any other row that also normalizes to `1.0`
/// (RED corpus case 3 — "exact current work wins its partition"), because
/// `sort_by`'s stability preserves original order on an exact score tie.
pub(crate) fn prepend_anchor_evidence_rows(
    evidence: Value,
    required_anchors: &[RecallEvidenceV1],
) -> Value {
    if required_anchors.is_empty() {
        return evidence;
    }
    let mut rows: Vec<Value> = required_anchors
        .iter()
        .map(|anchor| {
            json!({
                "section": "current_work",
                "id": anchor.source_ref,
                "path": format!("github:{}", anchor.source_ref),
                "topic": "current_work",
                "summary": format!(
                    "{} — lifecycle={} grounding={}",
                    anchor.source_ref,
                    anchor.lifecycle,
                    anchor.grounding_status.as_str(),
                ),
                "relevance": anchor.retrieval_score,
                "authority": anchor.authority.as_str(),
                "grounding_status": anchor.grounding_status.as_str(),
                "source_revision": anchor.source_revision,
                "valid_at": anchor.valid_at,
                "contradictions": anchor.contradictions,
            })
        })
        .collect();
    match evidence {
        Value::Array(existing) => {
            rows.extend(existing);
            Value::Array(rows)
        }
        other => {
            rows.push(other);
            Value::Array(rows)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_owner_repo_hash_number_from_free_text() {
        let anchors =
            extract_exact_issue_anchors("what is the status of kckylechen1/tachi#1002 right now?");
        assert_eq!(anchors.len(), 1);
        assert_eq!(anchors[0].repo, "kckylechen1/tachi");
        assert_eq!(anchors[0].number, 1002);
    }

    #[test]
    fn extracts_github_issue_url_and_trims_trailing_punctuation() {
        let anchors = extract_exact_issue_anchors(
            "see https://github.com/kckylechen1/tachi/issues/1002, thanks.",
        );
        assert_eq!(anchors.len(), 1);
        assert_eq!(anchors[0].repo, "kckylechen1/tachi");
        assert_eq!(anchors[0].number, 1002);
    }

    #[test]
    fn dedups_the_same_anchor_mentioned_twice() {
        let anchors =
            extract_exact_issue_anchors("kckylechen1/tachi#1002 and again kckylechen1/tachi#1002");
        assert_eq!(anchors.len(), 1);
    }

    #[test]
    fn bare_hash_number_is_never_resolved_no_default_repo() {
        let anchors = extract_exact_issue_anchors("what about #1002?");
        assert!(
            anchors.is_empty(),
            "bare #N has no safe default repo on the ask surface"
        );
    }

    #[test]
    fn plain_query_with_no_anchors_extracts_nothing() {
        assert!(extract_exact_issue_anchors("what did we implement").is_empty());
    }

    fn target(repo: &str, number: u64) -> GithubTarget {
        GithubTarget {
            repo: repo.to_string(),
            number,
        }
    }

    /// RED corpus case 1/2 core: a `gh` failure (network/auth/not-found —
    /// modeled here as an injected `Err`, never a real network call, see
    /// module doc) must compile to `MissingAnchor`, never `Grounded`.
    #[test]
    fn compile_current_work_anchor_missing_on_fetch_error() {
        let row = compile_current_work_anchor(
            &target("o/r", 1002),
            Err("gh failed (exit 1): could not resolve to an issue"),
            "2026-07-16T00:00:00Z",
        );
        assert_eq!(row.grounding_status, GroundingStatusV1::MissingAnchor);
        assert_eq!(row.authority, AuthorityClassV1::CurrentWork);
        assert_eq!(row.retrieval_score, 0.0);
        assert!(!row.contradictions.is_empty());
    }

    #[test]
    fn compile_current_work_anchor_grounded_on_matching_issue() {
        let gh_result = json!({
            "number": 1002,
            "title": "Issue Refinery v1",
            "state": "OPEN",
            "body": "body text",
            "labels": [],
            "updatedAt": "2026-07-13T12:00:00Z",
        });
        let row = compile_current_work_anchor(
            &target("o/r", 1002),
            Ok(&gh_result),
            "2026-07-16T00:00:00Z",
        );
        assert_eq!(row.grounding_status, GroundingStatusV1::Grounded);
        assert_eq!(row.lifecycle, "open");
        assert_eq!(row.retrieval_score, 1.0);
        assert_eq!(row.claim_coverage, 1.0);
        assert!(row.contradictions.is_empty());
        assert_eq!(row.valid_at, "2026-07-13T12:00:00Z");
        assert!(!row.source_revision.is_empty());
    }

    /// F1-style guard (mirrors #1002's `validate_gh_issue_result`): a
    /// response that doesn't identify the REQUESTED issue by number must
    /// never be silently treated as grounded.
    #[test]
    fn compile_current_work_anchor_missing_when_number_mismatches() {
        let gh_result = json!({
            "number": 999,
            "state": "OPEN",
        });
        let row = compile_current_work_anchor(
            &target("o/r", 1002),
            Ok(&gh_result),
            "2026-07-16T00:00:00Z",
        );
        assert_eq!(row.grounding_status, GroundingStatusV1::MissingAnchor);
    }

    /// A truncated/malformed `gh` response that still parses as *some* JSON
    /// object but is missing `state` must not be treated as grounded either.
    #[test]
    fn compile_current_work_anchor_missing_when_state_absent() {
        let gh_result = json!({ "number": 1002 });
        let row = compile_current_work_anchor(
            &target("o/r", 1002),
            Ok(&gh_result),
            "2026-07-16T00:00:00Z",
        );
        assert_eq!(row.grounding_status, GroundingStatusV1::MissingAnchor);
    }

    /// #1071 fix-round checkpoint 3 (exact codex repro): a `gh` response
    /// that identifies the right issue by number/state but is missing
    /// `title`/`labels`/`updatedAt` (a truncated/partial snapshot, e.g. a
    /// flaky `gh` call that returns before the full JSON body streams) must
    /// NOT be treated as `Grounded`/`claim_coverage: 1.0` — the leaf only
    /// ever requests the complete field set, so a partial response is
    /// itself evidence of malformation, not a smaller-but-valid claim.
    #[test]
    fn compile_current_work_anchor_missing_on_partial_snapshot() {
        let gh_result = json!({ "number": 1002, "state": "OPEN" });
        let row = compile_current_work_anchor(
            &target("o/r", 1002),
            Ok(&gh_result),
            "2026-07-16T00:00:00Z",
        );
        assert_eq!(
            row.grounding_status,
            GroundingStatusV1::MissingAnchor,
            "a partial snapshot missing title/labels/updatedAt must never be Grounded"
        );
        assert_eq!(row.claim_coverage, 0.0);
        assert!(!row.contradictions.is_empty());
    }

    /// RED corpus case 1: a resolved-but-missing anchor must force
    /// confidence down regardless of how much OTHER evidence exists —
    /// `compute_anchor_gated_confidence` never looks at evidence volume at
    /// all, by construction.
    #[test]
    fn confidence_is_low_when_any_required_anchor_is_missing() {
        let grounded = compile_current_work_anchor(
            &target("o/r", 1),
            Ok(
                &json!({"number": 1, "state": "OPEN", "title": "t", "labels": [], "updatedAt": "t"}),
            ),
            "t0",
        );
        let missing = compile_current_work_anchor(&target("o/r", 2), Err("not found"), "t0");
        assert_eq!(
            compute_anchor_gated_confidence(&[grounded.clone(), missing]),
            "low"
        );
        assert_eq!(compute_anchor_gated_confidence(&[grounded]), "high");
    }

    /// #1071 fix-round checkpoint 3: `compute_anchor_gated_confidence` must
    /// literally check `claim_coverage`, `authority`, and `contradictions`
    /// — not only `grounding_status` — per the frozen rule's exact wording.
    /// These rows are hand-constructed (not producible by this leaf's own
    /// live path today) specifically to prove the FUNCTION itself enforces
    /// each factor defensively, not just the paths that currently exist.
    #[test]
    fn confidence_is_low_when_claim_coverage_authority_or_contradictions_are_off() {
        let clean = RecallEvidenceV1 {
            kind: SourceKindV1::Issue,
            authority: AuthorityClassV1::CurrentWork,
            lifecycle: "open".to_string(),
            source_ref: "o/r#1".to_string(),
            source_revision: "abc".to_string(),
            valid_at: "t".to_string(),
            retrieval_score: 1.0,
            claim_coverage: 1.0,
            contradictions: Vec::new(),
            grounding_status: GroundingStatusV1::Grounded,
        };
        assert_eq!(
            compute_anchor_gated_confidence(std::slice::from_ref(&clean)),
            "high"
        );

        let mut partial_claim = clean.clone();
        partial_claim.claim_coverage = 0.5;
        assert_eq!(
            compute_anchor_gated_confidence(&[partial_claim]),
            "low",
            "claim_coverage < 1.0 must cap confidence even when grounding_status is Grounded"
        );

        let mut wrong_authority = clean.clone();
        wrong_authority.authority = AuthorityClassV1::Advisory;
        assert_eq!(
            compute_anchor_gated_confidence(&[wrong_authority]),
            "low",
            "non-CurrentWork authority must never grant required-anchor coverage"
        );

        let mut contradicted = clean;
        contradicted.contradictions = vec![ContradictionV1 {
            description: "conflicts with another source".to_string(),
            evidence_refs: Vec::new(),
        }];
        assert_eq!(
            compute_anchor_gated_confidence(&[contradicted]),
            "low",
            "a non-empty contradictions list must cap confidence"
        );
    }

    #[test]
    fn overall_grounding_status_reflects_any_missing_anchor() {
        let grounded = compile_current_work_anchor(
            &target("o/r", 1),
            Ok(
                &json!({"number": 1, "state": "OPEN", "title": "t", "labels": [], "updatedAt": "t"}),
            ),
            "t0",
        );
        let missing = compile_current_work_anchor(&target("o/r", 2), Err("not found"), "t0");
        assert_eq!(
            overall_grounding_status(&[grounded.clone()]),
            GroundingStatusV1::Grounded
        );
        assert_eq!(
            overall_grounding_status(&[grounded, missing]),
            GroundingStatusV1::MissingAnchor
        );
    }

    /// RED corpus case 3: with a large pile of high-scoring generic
    /// evidence (simulating old-compound-distill crowding, each already at
    /// the same normalized top score), the anchor row must still be first
    /// after `prepend_anchor_evidence_rows` + a stable score sort.
    #[test]
    fn anchor_row_wins_its_partition_against_crowding_evidence() {
        let grounded = compile_current_work_anchor(
            &target("o/r", 1002),
            Ok(
                &json!({"number": 1002, "state": "OPEN", "title": "t", "labels": [], "updatedAt": "t"}),
            ),
            "t0",
        );
        let crowding_rows: Vec<Value> = (0..20)
            .map(|i| {
                json!({
                    "id": format!("old-distill-{i}"),
                    "path": "/notes/old",
                    "topic": "memory",
                    "summary": "an old compound distill row",
                    "relevance": 1.0,
                })
            })
            .collect();
        let evidence = prepend_anchor_evidence_rows(json!(crowding_rows), &[grounded]);
        let rows = evidence.as_array().expect("array");
        assert_eq!(rows.len(), 21);
        assert_eq!(rows[0]["id"], json!("o/r#1002"));
    }
}
