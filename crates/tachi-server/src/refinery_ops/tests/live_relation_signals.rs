//! #1105 acceptance: live per-relation evidence and commit-reachability
//! shipped checks REPLACE #1002 v1's conservative defaults, through the
//! FULL pipeline (`build_refinery_packet_with_live_signals` — the exact
//! function `handle_refine_issues` calls once its async orchestrator has
//! populated `LiveRelationSignals`). `tests::disposition_rules`'s "prose
//! ... advisory ... before #1105" tests cover the complementary v1
//! no-live-signal path (`build_refinery_packet`, `LiveRelationSignals::default()`).
//!
//! RED-before-#1105 framing: every assertion below was impossible pre-#1105
//! — v1 always normalized relation state to `Unknown`, `scope_collisions`
//! was always empty, and `shipped_evidence` was always `None`, so none of
//! these dispositions (CLOSE_SUPERSEDED / DORMANT / BLOCKED / MERGE_CANDIDATE
//! / CLOSE_FIXED, all driven by a live-derived signal) were reachable through
//! `build_refinery_packet` at all before this leaf.

use super::super::build_refinery_packet_with_live_signals;
use super::super::disposition::RelatedIssueStateV1;
use super::super::fixtures::{gh_issue_json, FixtureDocResolver};
use super::super::live_signals::LiveRelationSignals;
use tachi_params::DispositionV1;

const CAPTURED_AT: &str = "2026-07-17T00:00:00Z";

fn live_signals_with_related_state(
    target_ref: &str,
    state: RelatedIssueStateV1,
) -> LiveRelationSignals {
    let mut related_states = std::collections::HashMap::new();
    related_states.insert(target_ref.to_string(), state);
    LiveRelationSignals {
        related_states,
        own_shipped_evidence: None,
    }
}

#[test]
fn live_verified_closed_shipped_supersedes_produces_close_superseded() {
    let body = "This work is superseded by the landed replacement.\n\n\
                Supersedes: owner/repo#7001\n"
        .to_string();
    let gh_json = gh_issue_json(
        9401,
        "Live-verified superseded-and-shipped fixture",
        &body,
        "OPEN",
        &[],
        None,
        CAPTURED_AT,
        &[],
    );
    let resolver = FixtureDocResolver::new();
    let live_signals =
        live_signals_with_related_state("owner/repo#7001", RelatedIssueStateV1::ClosedShipped);
    let (_evidence, proposal) = build_refinery_packet_with_live_signals(
        "owner/repo",
        9401,
        &gh_json,
        &resolver,
        CAPTURED_AT,
        &live_signals,
    )
    .expect("build_refinery_packet_with_live_signals");
    assert_eq!(proposal.disposition, DispositionV1::CloseSuperseded);
}

#[test]
fn live_verified_closed_unshipped_supersedes_produces_dormant_with_contradiction() {
    let body = "This work is superseded but the replacement did not land.\n\n\
                Supersedes: owner/repo#7002\n"
        .to_string();
    let gh_json = gh_issue_json(
        9402,
        "Live-verified superseded-but-unshipped fixture",
        &body,
        "OPEN",
        &[],
        None,
        CAPTURED_AT,
        &[],
    );
    let resolver = FixtureDocResolver::new();
    let live_signals =
        live_signals_with_related_state("owner/repo#7002", RelatedIssueStateV1::ClosedUnshipped);
    let (_evidence, proposal) = build_refinery_packet_with_live_signals(
        "owner/repo",
        9402,
        &gh_json,
        &resolver,
        CAPTURED_AT,
        &live_signals,
    )
    .expect("build_refinery_packet_with_live_signals");
    assert_eq!(proposal.disposition, DispositionV1::Dormant);
    assert!(proposal
        .contradictions
        .iter()
        .any(|c| c.description.contains("not shipped yet")));
}

#[test]
fn live_verified_open_dependency_produces_blocked() {
    let body = "Dependency state is now live-verified.\n\n\
                Depends-On: owner/repo#7004\n"
        .to_string();
    let gh_json = gh_issue_json(
        9403,
        "Live-verified open dependency fixture",
        &body,
        "OPEN",
        &[],
        None,
        CAPTURED_AT,
        &[],
    );
    let resolver = FixtureDocResolver::new();
    let live_signals =
        live_signals_with_related_state("owner/repo#7004", RelatedIssueStateV1::Open);
    let (_evidence, proposal) = build_refinery_packet_with_live_signals(
        "owner/repo",
        9403,
        &gh_json,
        &resolver,
        CAPTURED_AT,
        &live_signals,
    )
    .expect("build_refinery_packet_with_live_signals");
    assert_eq!(proposal.disposition, DispositionV1::Blocked);
}

/// A prose `[state]` annotation that DISAGREES with the live-verified state
/// is surfaced as a real contradiction — the live cross-reference is
/// authority, the prose annotation is not, but the disagreement itself is
/// not silently discarded.
#[test]
fn prose_annotation_disagreeing_with_live_state_is_a_contradiction() {
    let body = "This work is claimed shipped in prose, but is not, live.\n\n\
                Supersedes: owner/repo#7005 [closed_shipped]\n"
        .to_string();
    let gh_json = gh_issue_json(
        9404,
        "Prose/live disagreement fixture",
        &body,
        "OPEN",
        &[],
        None,
        CAPTURED_AT,
        &[],
    );
    let resolver = FixtureDocResolver::new();
    // Live-verified: still OPEN (not actually shipped), contradicting the
    // owner's own stale `[closed_shipped]` prose annotation.
    let live_signals =
        live_signals_with_related_state("owner/repo#7005", RelatedIssueStateV1::Open);
    let (_evidence, proposal) = build_refinery_packet_with_live_signals(
        "owner/repo",
        9404,
        &gh_json,
        &resolver,
        CAPTURED_AT,
        &live_signals,
    )
    .expect("build_refinery_packet_with_live_signals");
    assert!(
        proposal.contradictions.iter().any(|c| c
            .description
            .contains("disagrees with the live-verified state")),
        "got: {:?}",
        proposal.contradictions
    );
}

/// A prose annotation that AGREES with the live-verified state must NOT be
/// reported as a contradiction — only a genuine disagreement is.
#[test]
fn prose_annotation_agreeing_with_live_state_is_not_a_contradiction() {
    let body = "Dependency is live-verified open, matching prose.\n\n\
                Depends-On: owner/repo#7006 [open]\n"
        .to_string();
    let gh_json = gh_issue_json(
        9405,
        "Prose/live agreement fixture",
        &body,
        "OPEN",
        &[],
        None,
        CAPTURED_AT,
        &[],
    );
    let resolver = FixtureDocResolver::new();
    let live_signals =
        live_signals_with_related_state("owner/repo#7006", RelatedIssueStateV1::Open);
    let (_evidence, proposal) = build_refinery_packet_with_live_signals(
        "owner/repo",
        9405,
        &gh_json,
        &resolver,
        CAPTURED_AT,
        &live_signals,
    )
    .expect("build_refinery_packet_with_live_signals");
    assert!(
        !proposal.contradictions.iter().any(|c| c
            .description
            .contains("disagrees with the live-verified state")),
        "got: {:?}",
        proposal.contradictions
    );
}

/// `Duplicate-Of:` a still-OPEN issue is this leaf's own structured
/// scope_collision signal (see `live_signals` module doc) -> MERGE_CANDIDATE.
#[test]
fn live_open_duplicate_of_relation_is_a_scope_collision_and_merge_candidate() {
    let body = "This duplicates prior open work.\n\n\
                Duplicate-Of: owner/repo#7007\n"
        .to_string();
    let gh_json = gh_issue_json(
        9406,
        "Live duplicate-of-open fixture",
        &body,
        "OPEN",
        &[],
        None,
        CAPTURED_AT,
        &[],
    );
    let resolver = FixtureDocResolver::new();
    let live_signals =
        live_signals_with_related_state("owner/repo#7007", RelatedIssueStateV1::Open);
    let (_evidence, proposal) = build_refinery_packet_with_live_signals(
        "owner/repo",
        9406,
        &gh_json,
        &resolver,
        CAPTURED_AT,
        &live_signals,
    )
    .expect("build_refinery_packet_with_live_signals");
    assert_eq!(proposal.disposition, DispositionV1::MergeCandidate);
}

/// A `Duplicate-Of:` a CLOSED issue is not a live scope collision (nothing
/// live is competing for the scope anymore) — must stay KEEP, not
/// MERGE_CANDIDATE.
#[test]
fn duplicate_of_a_closed_issue_is_not_a_scope_collision() {
    let body = "This duplicates prior work that has since closed.\n\n\
                Duplicate-Of: owner/repo#7008\n"
        .to_string();
    let gh_json = gh_issue_json(
        9407,
        "Duplicate-of-closed fixture",
        &body,
        "OPEN",
        &[],
        None,
        CAPTURED_AT,
        &[],
    );
    let resolver = FixtureDocResolver::new();
    let live_signals =
        live_signals_with_related_state("owner/repo#7008", RelatedIssueStateV1::ClosedUnshipped);
    let (_evidence, proposal) = build_refinery_packet_with_live_signals(
        "owner/repo",
        9407,
        &gh_json,
        &resolver,
        CAPTURED_AT,
        &live_signals,
    )
    .expect("build_refinery_packet_with_live_signals");
    assert_eq!(proposal.disposition, DispositionV1::Keep);
}

/// This issue's OWN `shipped_evidence` (#1105 items 1+2) -> CLOSE_FIXED,
/// through the full pipeline (not the classifier-bypass `propose()` helper).
#[test]
fn own_shipped_evidence_produces_close_fixed_through_full_pipeline() {
    let body = "Fixed a while ago; nobody closed the issue.".to_string();
    let gh_json = gh_issue_json(
        9408,
        "Own shipped-evidence fixture",
        &body,
        "OPEN",
        &[],
        None,
        CAPTURED_AT,
        &[],
    );
    let resolver = FixtureDocResolver::new();
    let live_signals = LiveRelationSignals {
        related_states: std::collections::HashMap::new(),
        own_shipped_evidence: Some(tachi_params::RepoRevisionV1 {
            repo: "owner/repo".to_string(),
            git_ref: "origin/main".to_string(),
            commit_sha: "deadbeef".to_string(),
            verified_at: CAPTURED_AT.to_string(),
        }),
    };
    let (_evidence, proposal) = build_refinery_packet_with_live_signals(
        "owner/repo",
        9408,
        &gh_json,
        &resolver,
        CAPTURED_AT,
        &live_signals,
    )
    .expect("build_refinery_packet_with_live_signals");
    assert_eq!(proposal.disposition, DispositionV1::CloseFixed);
}

/// Fail-closed contract: `LiveRelationSignals::default()` through the
/// `_with_live_signals` entry point produces the EXACT SAME proposal as the
/// v1 back-compat `build_refinery_packet` wrapper — proving the wrapper is a
/// real equivalence, not just a same-shaped stand-in, and that "no live data
/// collected" degrades safely to v1 rather than to something new.
#[test]
fn default_live_signals_matches_v1_back_compat_wrapper_exactly() {
    let body = "Ordinary issue, no relations.".to_string();
    let gh_json = gh_issue_json(
        9409,
        "Equivalence fixture",
        &body,
        "OPEN",
        &[],
        None,
        CAPTURED_AT,
        &[],
    );
    let resolver = FixtureDocResolver::new();
    let (evidence_a, proposal_a) = build_refinery_packet_with_live_signals(
        "owner/repo",
        9409,
        &gh_json,
        &resolver,
        CAPTURED_AT,
        &LiveRelationSignals::default(),
    )
    .expect("with_live_signals");
    let (evidence_b, proposal_b) =
        super::super::build_refinery_packet("owner/repo", 9409, &gh_json, &resolver, CAPTURED_AT)
            .expect("v1 wrapper");
    assert_eq!(evidence_a, evidence_b);
    assert_eq!(proposal_a, proposal_b);
}
