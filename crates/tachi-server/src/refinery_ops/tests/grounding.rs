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
const ISSUE_1002_STALE_COMMIT_SHA: &str = "50e14052d5f737a41362f51bdd6ee5cb5a8e0e60";
const ISSUE_1002_DOC_PATH: &str = "docs/engineering/architecture/issue-refinery-memory-lanes.md";
const ISSUE_1002_BLOB_SHA: &str = "d303db5446a30136003a0f518dde0ed9bac4ea0f";

#[test]
fn exact_1002_anchor_resolves_snapshot_and_linked_spec() {
    let body = format!(
        "Issue Refinery v1 lands the typed evidence/disposition packet.\n\n\
         Spec-Ref: kckylechen1/tachi:{ISSUE_1002_DOC_PATH}@{ISSUE_1002_COMMIT_SHA}/{ISSUE_1002_BLOB_SHA}#3\n"
    );
    let gh_json = gh_issue_json(
        1002,
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
fn exact_1002_owner_amendment_supersedes_the_stale_body_pin() {
    let stale_body = format!(
        "Issue Refinery v1 original pin.\n\n\
         Spec-Ref: kckylechen1/tachi:{ISSUE_1002_DOC_PATH}@{ISSUE_1002_STALE_COMMIT_SHA}/{ISSUE_1002_BLOB_SHA}#3\n"
    );
    let amendment = format!(
        "Spec-Ref amendment (leader): #1070 merged. Updated pin: Spec-Ref: \
         kckylechen1/tachi:{ISSUE_1002_DOC_PATH}@{ISSUE_1002_COMMIT_SHA}/{ISSUE_1002_BLOB_SHA}#4-issue-refinery-1002 \
         — dispatching v1 against this revision."
    );
    let gh_json = gh_issue_json(
        1002,
        "Issue Refinery v1",
        &stale_body,
        "OPEN",
        &["feature"],
        None,
        "2026-07-13T00:00:00Z",
        &[(
            "IC_owner_amendment",
            "kckylechen1",
            "2026-07-13T01:00:00Z",
            None,
            &amendment,
        )],
    );
    let resolver = FixtureDocResolver::new().with_resolved(
        "kckylechen1/tachi",
        ISSUE_1002_COMMIT_SHA,
        ISSUE_1002_DOC_PATH,
        ISSUE_1002_BLOB_SHA,
        "4-issue-refinery-1002",
        "origin/main",
    );

    let (evidence, proposal) =
        build_refinery_packet("kckylechen1/tachi", 1002, &gh_json, &resolver, CAPTURED_AT)
            .expect("build_refinery_packet");

    assert_eq!(evidence.grounding_status, GroundingStatusV1::Grounded);
    assert_eq!(evidence.linked_specs.len(), 1);
    assert_eq!(
        evidence.linked_specs[0].section, "4-issue-refinery-1002",
        "the later owner amendment must be the sole authoritative pin"
    );
    assert_eq!(proposal.based_on_doc_revisions.len(), 1);
}

#[test]
fn untrusted_updated_pin_substring_cannot_establish_a_canonical_anchor() {
    let body = "Issue body has no canonical pin.";
    let attacker_comment = format!(
        "arbitrary prose Updated pin: Spec-Ref: \
         kckylechen1/tachi:{ISSUE_1002_DOC_PATH}@{ISSUE_1002_COMMIT_SHA}/{ISSUE_1002_BLOB_SHA}#4-issue-refinery-1002"
    );
    let gh_json = gh_issue_json(
        1003,
        "Untrusted amendment fixture",
        body,
        "OPEN",
        &[],
        None,
        CAPTURED_AT,
        &[(
            "IC_untrusted",
            "not-the-repo-owner",
            CAPTURED_AT,
            None,
            &attacker_comment,
        )],
    );
    let resolver = FixtureDocResolver::new().with_resolved(
        "kckylechen1/tachi",
        ISSUE_1002_COMMIT_SHA,
        ISSUE_1002_DOC_PATH,
        ISSUE_1002_BLOB_SHA,
        "4-issue-refinery-1002",
        "origin/main",
    );

    let (evidence, proposal) =
        build_refinery_packet("kckylechen1/tachi", 1003, &gh_json, &resolver, CAPTURED_AT)
            .expect("build_refinery_packet");

    assert!(evidence.linked_specs.is_empty());
    assert!(proposal.based_on_doc_revisions.is_empty());
    assert_eq!(
        evidence.issue_snapshot.selected_comment_revisions.len(),
        1,
        "the untrusted comment remains snapshot/coverage evidence even though it is not authority"
    );
}

#[test]
fn partial_or_wrong_typed_github_snapshot_degrades_to_missing_anchor() {
    let base = gh_issue_json(
        1004,
        "Complete snapshot fixture",
        "Body text.",
        "OPEN",
        &["feature"],
        Some("M1"),
        CAPTURED_AT,
        &[(
            "IC_valid",
            "owner",
            CAPTURED_AT,
            None,
            "Related: owner/repo#1",
        )],
    );
    let resolver = FixtureDocResolver::new();

    for field in ["body", "labels", "comments", "milestone", "updatedAt"] {
        let mut partial = base.clone();
        partial
            .as_object_mut()
            .expect("fixture object")
            .remove(field);
        let (evidence, proposal) =
            build_refinery_packet("owner/repo", 1004, &partial, &resolver, CAPTURED_AT)
                .expect("partial packet still returns preview evidence");
        assert_eq!(
            evidence.grounding_status,
            GroundingStatusV1::MissingAnchor,
            "missing semantic field {field} must fail closed"
        );
        assert_eq!(proposal.disposition, DispositionV1::DecisionRequired);
        assert!(proposal
            .contradictions
            .iter()
            .any(|c| c.description.contains(field)));
    }

    let wrong_typed = [
        ("body", serde_json::json!([])),
        ("labels", serde_json::json!({})),
        ("comments", serde_json::json!({})),
        ("milestone", serde_json::json!({"title": 7})),
        ("updatedAt", serde_json::json!(7)),
    ];
    for (field, wrong_value) in wrong_typed {
        let mut invalid = base.clone();
        invalid[field] = wrong_value;
        let (evidence, proposal) =
            build_refinery_packet("owner/repo", 1004, &invalid, &resolver, CAPTURED_AT)
                .expect("invalid packet still returns preview evidence");
        assert_eq!(evidence.grounding_status, GroundingStatusV1::MissingAnchor);
        assert_eq!(proposal.disposition, DispositionV1::DecisionRequired);
        assert!(proposal
            .contradictions
            .iter()
            .any(|c| c.description.contains(field)));
    }

    let mut invalid_comment = base;
    invalid_comment["comments"][0]["author"] = serde_json::json!({"login": 7});
    let (evidence, proposal) =
        build_refinery_packet("owner/repo", 1004, &invalid_comment, &resolver, CAPTURED_AT)
            .expect("invalid nested comment still returns preview evidence");
    assert_eq!(evidence.grounding_status, GroundingStatusV1::MissingAnchor);
    assert_eq!(proposal.disposition, DispositionV1::DecisionRequired);
    assert!(proposal
        .contradictions
        .iter()
        .any(|c| c.description.contains("comments[0].author")));
}

#[test]
fn missing_anchor_degrades_grounding_and_forces_decision_required() {
    let body = format!(
        "This work depends on a spec that was never actually committed.\n\n{}",
        spec_ref_line("owner/repo")
    );
    let gh_json = gh_issue_json(
        9001,
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
        9002,
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
        9003,
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
        9004,
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
        9005,
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

// ─── F5/R4-5 (build-seat REQUEST-CHANGES): comment snapshot hash basis ─────
//
// codex verified `gh issue view --json comments` only ever exposes
// `createdAt`, never `updatedAt` — canon doc's comment revision shape
// `{comment_id, updated_at, body_hash}` load-bears on `body_hash` for edit
// detection, not `updated_at` (`parse.rs` still prefers `updatedAt` over
// `createdAt` when present — forward-looking, currently inert against the
// real gh CLI — see its own doc comment). The real, always-available edit
// signal is `body_hash`: same `comment_id`, different `body` text, must
// still change the semantic snapshot hash.

#[test]
fn comment_body_change_with_same_comment_id_changes_the_snapshot_hash_via_body_hash() {
    let base_body = "Body with no relations of its own.".to_string();
    let resolver = FixtureDocResolver::new();

    let gh_json_v1 = gh_issue_json(
        9006,
        "Comment hash fixture",
        &base_body,
        "OPEN",
        &[],
        None,
        CAPTURED_AT,
        &[(
            "c1",
            "someone",
            "2026-07-13T00:00:00Z",
            None,
            "Original comment text.\n\nRelated: owner/repo#8888\n",
        )],
    );
    let (evidence_v1, _proposal_v1) =
        build_refinery_packet("owner/repo", 9006, &gh_json_v1, &resolver, CAPTURED_AT)
            .expect("build_refinery_packet v1");

    let gh_json_v2 = gh_issue_json(
        9006,
        "Comment hash fixture",
        &base_body,
        "OPEN",
        &[],
        None,
        CAPTURED_AT,
        &[(
            "c1",
            "someone",
            "2026-07-13T00:00:00Z",
            None,
            "Edited comment text (same id, createdAt unchanged).\n\nRelated: owner/repo#8888\n",
        )],
    );
    let (evidence_v2, _proposal_v2) =
        build_refinery_packet("owner/repo", 9006, &gh_json_v2, &resolver, CAPTURED_AT)
            .expect("build_refinery_packet v2");

    assert_eq!(
        evidence_v1.issue_snapshot.selected_comment_revisions[0].comment_id,
        evidence_v2.issue_snapshot.selected_comment_revisions[0].comment_id,
        "test setup: same comment id across v1/v2 (realistic edit-in-place)"
    );
    assert_eq!(
        evidence_v1.issue_snapshot.selected_comment_revisions[0].updated_at,
        evidence_v2.issue_snapshot.selected_comment_revisions[0].updated_at,
        "test setup: createdAt (the only real timestamp gh exposes) is unchanged, \
         mirroring the real gh CLI constraint"
    );
    assert_ne!(
        evidence_v1.issue_snapshot.selected_comment_revisions[0].body_hash,
        evidence_v2.issue_snapshot.selected_comment_revisions[0].body_hash,
        "body_hash must differ when the comment text differs"
    );
    assert_ne!(
        evidence_v1.issue_snapshot_hash, evidence_v2.issue_snapshot_hash,
        "a comment body edit must change the semantic snapshot hash even when \
         no usable updatedAt timestamp exists — body_hash is the real signal"
    );
}

/// Forward-looking: when `updatedAt` IS present (a future gh CLI version,
/// or a different source), the parser still prefers it over `createdAt` —
/// this proves that logic still works, not that it's exercised by the real
/// `gh` CLI today (see module doc note above).
#[test]
fn comment_updated_at_is_preferred_over_created_at_when_present() {
    let body = "Body with no relations of its own.".to_string();
    let comment_body = "Comment text.\n\nRelated: owner/repo#7777\n";
    let gh_json = gh_issue_json(
        9007,
        "updatedAt preference fixture",
        &body,
        "OPEN",
        &[],
        None,
        CAPTURED_AT,
        &[(
            "c1",
            "someone",
            "2026-07-13T00:00:00Z",
            Some("2026-07-20T00:00:00Z"),
            comment_body,
        )],
    );
    let resolver = FixtureDocResolver::new();
    let (evidence, _proposal) =
        build_refinery_packet("owner/repo", 9007, &gh_json, &resolver, CAPTURED_AT)
            .expect("build_refinery_packet");
    assert_eq!(
        evidence.issue_snapshot.selected_comment_revisions[0].updated_at, "2026-07-20T00:00:00Z",
        "updatedAt must win over createdAt when both are present"
    );
}
