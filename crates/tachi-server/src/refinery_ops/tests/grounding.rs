//! #1002 acceptance criteria 1-3: exact-anchor resolution, missing-anchor
//! degradation, and full byte-accounted coverage (no truncation).

use super::super::build_refinery_packet;
use super::super::fixtures::{gh_issue_json, spec_ref_line, FixtureDocResolver};
use tachi_params::{AnchorKindV1, DispositionV1, GroundingStatusV1};

const CAPTURED_AT: &str = "2026-07-13T00:00:00Z";

/// The real commit + blob SHA of this leaf's own canon doc, as pinned by
/// this dispatch packet's `Spec-Ref` (worktree HEAD `12102bd9...` /
/// `docs/engineering/architecture/issue-refinery-memory-lanes.md`). Using
/// the real SHAs here (rather than an arbitrary placeholder) makes this
/// "exact #1002 anchor" test a faithful replay of #1002's own grounding,
/// not just a synthetic shape check.
const ISSUE_1002_COMMIT_SHA: &str = "12102bd92e2e6f6959aa1b20858b4fe8f2889585";
const ISSUE_1002_DOC_PATH: &str = "docs/engineering/architecture/issue-refinery-memory-lanes.md";
const ISSUE_1002_BLOB_SHA: &str = "d303db5446a30136003a0f518dde0ed9bac4ea0f";

#[test]
fn exact_1002_anchor_resolves_snapshot_and_linked_spec() {
    let body = format!(
        "Issue Refinery v1 lands the typed evidence/disposition packet.\n\n\
         Spec-Ref: kckylechen1/tachi:{ISSUE_1002_DOC_PATH}@{ISSUE_1002_COMMIT_SHA}/{ISSUE_1002_BLOB_SHA}#3\n"
    );
    let gh_json = gh_issue_json(
        "Issue Refinery v1",
        &body,
        "OPEN",
        &["feature"],
        None,
        "2026-07-13T00:00:00Z",
        &[],
    );
    let resolver = FixtureDocResolver::new().with_resolved(
        "kckylechen1/tachi",
        ISSUE_1002_COMMIT_SHA,
        ISSUE_1002_DOC_PATH,
        ISSUE_1002_BLOB_SHA,
        "3",
        "origin/main",
    );

    let (evidence, proposal) =
        build_refinery_packet("kckylechen1/tachi", 1002, &gh_json, &resolver, CAPTURED_AT)
            .expect("build_refinery_packet");

    assert_eq!(evidence.issue_ref, "kckylechen1/tachi#1002");
    assert_eq!(evidence.grounding_status, GroundingStatusV1::Grounded);
    assert_eq!(evidence.linked_specs.len(), 1);
    assert_eq!(evidence.linked_specs[0].path, ISSUE_1002_DOC_PATH);
    assert_eq!(evidence.linked_specs[0].commit_sha, ISSUE_1002_COMMIT_SHA);
    assert!(!evidence.issue_snapshot_hash.is_empty());
    assert!(!evidence.issue_body_hash.is_empty());

    // The claim containing the Spec-Ref line carries a CanonicalDoc anchor.
    let has_doc_anchor = evidence.claims.iter().any(|c| {
        c.anchors
            .iter()
            .any(|a| a.kind == AnchorKindV1::CanonicalDoc)
    });
    assert!(
        has_doc_anchor,
        "the paragraph declaring Spec-Ref must carry a resolved CanonicalDoc anchor"
    );

    assert_eq!(proposal.grounding_status, GroundingStatusV1::Grounded);
    assert_eq!(proposal.based_on_doc_revisions.len(), 1);
    assert!(proposal.preview_only, "V1 never applies — always preview");
}

#[test]
fn missing_anchor_degrades_grounding_and_forces_decision_required() {
    let body = format!(
        "This work depends on a spec that was never actually committed.\n\n{}",
        spec_ref_line("owner/repo")
    );
    let gh_json = gh_issue_json(
        "Dangling spec pin",
        &body,
        "OPEN",
        &[],
        None,
        "2026-07-13T00:00:00Z",
        &[],
    );
    // Resolver has nothing registered — every anchor is Unresolved.
    let resolver = FixtureDocResolver::new();

    let (evidence, proposal) =
        build_refinery_packet("owner/repo", 9001, &gh_json, &resolver, CAPTURED_AT)
            .expect("build_refinery_packet");

    assert_eq!(evidence.grounding_status, GroundingStatusV1::MissingAnchor);
    assert!(
        evidence.linked_specs.is_empty(),
        "an unresolved Spec-Ref must not appear in linked_specs"
    );
    assert_eq!(proposal.grounding_status, GroundingStatusV1::MissingAnchor);
    assert_eq!(
        proposal.disposition,
        DispositionV1::DecisionRequired,
        "missing_anchor must never resolve to a confident KEEP/CLOSE_*/ROUTER disposition"
    );
    assert!(proposal.preview_only);
    assert!(
        proposal.proposed_labels.is_empty() && proposal.proposed_doc_deltas.is_empty(),
        "a missing-anchor packet must carry no actionable recommendation"
    );
    assert!(
        !proposal.contradictions.is_empty(),
        "the resolver's unresolved-anchor reason must be surfaced as a real \
         contradiction, not silently discarded"
    );
}

#[test]
fn missing_anchor_wins_precedence_even_over_a_protected_router_label() {
    // Even a signal that would otherwise force ROUTER must not out-rank an
    // unresolved anchor — grounding failure is checked first (canon doc §4.1).
    let body = format!(
        "Umbrella tracking issue.\n\n{}",
        spec_ref_line("owner/repo")
    );
    let gh_json = gh_issue_json(
        "Router umbrella",
        &body,
        "OPEN",
        &["router"],
        None,
        "2026-07-13T00:00:00Z",
        &[],
    );
    let resolver = FixtureDocResolver::new();
    let (_evidence, proposal) =
        build_refinery_packet("owner/repo", 9002, &gh_json, &resolver, CAPTURED_AT)
            .expect("build_refinery_packet");
    assert_eq!(proposal.disposition, DispositionV1::DecisionRequired);
}

#[test]
fn coverage_accounts_for_every_byte_with_a_long_tail_paragraph_and_never_truncates() {
    // Larger than the legacy daily-distill 800-char truncation limit this
    // workflow must not repeat (canon doc §2.2).
    let long_tail = "x".repeat(5_000);
    let body = format!("Short lead paragraph.\n\n{long_tail}\n\nTrailing paragraph.");
    let gh_json = gh_issue_json(
        "Long tail issue",
        &body,
        "OPEN",
        &[],
        None,
        "2026-07-13T00:00:00Z",
        &[],
    );
    let resolver = FixtureDocResolver::new();
    let (evidence, _proposal) =
        build_refinery_packet("owner/repo", 9003, &gh_json, &resolver, CAPTURED_AT)
            .expect("build_refinery_packet");

    let normalized_body = &evidence.issue_snapshot.body;
    assert_eq!(evidence.coverage.source_bytes, normalized_body.len());

    // Full accounting identity: every byte is claimed or explicitly omitted.
    let omitted_bytes: usize = evidence
        .coverage
        .omitted_spans
        .iter()
        .map(|s| s.len())
        .sum();
    assert_eq!(
        evidence.coverage.covered_bytes + omitted_bytes,
        evidence.coverage.source_bytes
    );

    // The long-tail paragraph must appear whole in exactly one claim — not
    // truncated, not tail-dropped.
    let long_tail_claim = evidence
        .claims
        .iter()
        .find(|c| c.text.contains('x'))
        .expect("long-tail paragraph must produce a claim");
    assert_eq!(long_tail_claim.text, long_tail);
    assert_eq!(long_tail_claim.text.len(), 5_000);
}

#[test]
fn coverage_zero_omission_for_a_single_paragraph_body() {
    let body = "One single paragraph with no blank lines at all.".to_string();
    let gh_json = gh_issue_json(
        "Single paragraph",
        &body,
        "OPEN",
        &[],
        None,
        "2026-07-13T00:00:00Z",
        &[],
    );
    let resolver = FixtureDocResolver::new();
    let (evidence, _proposal) =
        build_refinery_packet("owner/repo", 9004, &gh_json, &resolver, CAPTURED_AT)
            .expect("build_refinery_packet");
    assert_eq!(evidence.claims.len(), 1);
    assert!(evidence.coverage.omitted_spans.is_empty());
    assert_eq!(
        evidence.coverage.covered_bytes,
        evidence.coverage.source_bytes
    );
    assert_eq!(evidence.claims[0].text, body);
}

// ─── F3 (build-seat REQUEST-CHANGES): coverage must include comment bytes ──

/// A comment that carries a structured marker (so it lands in
/// `selected_comment_revisions` and feeds Spec-Ref/relation parsing) must
/// ALSO have its bytes counted in `coverage` — previously only
/// `snapshot.body` was claim-split/covered, so a selected comment's bytes
/// vanished from `source_bytes` entirely (not even in `omitted_spans`).
#[test]
fn coverage_includes_selected_comment_bytes_not_just_the_body() {
    let body = "Original issue body with no relations of its own.".to_string();
    let comment_body = "Additional context in a follow-up comment.\n\nRelated: owner/repo#9999\n";
    let gh_json = gh_issue_json(
        "Comment coverage fixture",
        &body,
        "OPEN",
        &[],
        None,
        CAPTURED_AT,
        &[("c1", "someone", "2026-07-13T00:00:00Z", None, comment_body)],
    );
    let resolver = FixtureDocResolver::new();
    let (evidence, _proposal) =
        build_refinery_packet("owner/repo", 9005, &gh_json, &resolver, CAPTURED_AT)
            .expect("build_refinery_packet");

    assert_eq!(
        evidence.issue_snapshot.selected_comment_revisions.len(),
        1,
        "the comment carries a Related: marker and must be selected"
    );

    // source_bytes must be strictly larger than the body alone — the
    // comment's bytes are genuinely counted, not silently dropped.
    assert!(
        evidence.coverage.source_bytes > evidence.issue_snapshot.body.len(),
        "coverage.source_bytes ({}) must exceed the body alone ({}) once a selected \
         comment is present",
        evidence.coverage.source_bytes,
        evidence.issue_snapshot.body.len()
    );

    // Full accounting identity still holds over the LARGER (body+comment)
    // source.
    let omitted_bytes: usize = evidence
        .coverage
        .omitted_spans
        .iter()
        .map(|s| s.len())
        .sum();
    assert_eq!(
        evidence.coverage.covered_bytes + omitted_bytes,
        evidence.coverage.source_bytes
    );

    // The comment's own text shows up in some claim (not merely accounted
    // for as an anonymous omitted span) — this leaf fully claim-izes it.
    let has_comment_claim = evidence
        .claims
        .iter()
        .any(|c| c.text.contains("Additional context"));
    assert!(
        has_comment_claim,
        "the selected comment's text must appear in at least one claim"
    );
}

// ─── F5 (build-seat REQUEST-CHANGES): comment snapshot hash basis ──────────

/// A comment whose TEXT is unchanged but whose `updatedAt` changed (a real
/// edit that happens to preserve text, or any GH-side update timestamp
/// bump) must still change the semantic snapshot hash — `createdAt` alone
/// (immutable per-comment) can never detect this.
#[test]
fn comment_updated_at_change_with_identical_text_changes_the_snapshot_hash() {
    let body = "Body with no relations of its own.".to_string();
    let comment_body = "Comment text.\n\nRelated: owner/repo#8888\n";
    let resolver = FixtureDocResolver::new();

    let gh_json_v1 = gh_issue_json(
        "Comment hash fixture",
        &body,
        "OPEN",
        &[],
        None,
        CAPTURED_AT,
        &[(
            "c1",
            "someone",
            "2026-07-13T00:00:00Z",
            Some("2026-07-13T00:00:00Z"),
            comment_body,
        )],
    );
    let (evidence_v1, _proposal_v1) =
        build_refinery_packet("owner/repo", 9006, &gh_json_v1, &resolver, CAPTURED_AT)
            .expect("build_refinery_packet v1");

    let gh_json_v2 = gh_issue_json(
        "Comment hash fixture",
        &body,
        "OPEN",
        &[],
        None,
        CAPTURED_AT,
        &[(
            "c1",
            "someone",
            "2026-07-13T00:00:00Z",
            Some("2026-07-14T00:00:00Z"),
            comment_body,
        )],
    );
    let (evidence_v2, _proposal_v2) =
        build_refinery_packet("owner/repo", 9006, &gh_json_v2, &resolver, CAPTURED_AT)
            .expect("build_refinery_packet v2");

    assert_eq!(
        evidence_v1.issue_snapshot.selected_comment_revisions[0].body,
        evidence_v2.issue_snapshot.selected_comment_revisions[0].body,
        "test setup: comment text must be identical across v1/v2"
    );
    assert_ne!(
        evidence_v1.issue_snapshot.selected_comment_revisions[0].updated_at,
        evidence_v2.issue_snapshot.selected_comment_revisions[0].updated_at,
        "the parser must prefer updatedAt over createdAt"
    );
    assert_ne!(
        evidence_v1.issue_snapshot_hash, evidence_v2.issue_snapshot_hash,
        "a comment updatedAt change with identical text must still change the \
         semantic snapshot hash"
    );
}
