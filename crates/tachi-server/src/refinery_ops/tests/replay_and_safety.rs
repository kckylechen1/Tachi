//! #1002 acceptance criteria 5 (anti-replay staleness) and 6 (model-only
//! reasoning can never mark a HEAD claim verified).

use super::super::build_refinery_packet;
use super::super::disposition::{propose_disposition, RefinerySignalsV1};
use super::super::doc_resolver::{DocRefResolver, DocResolution, GitRefResolver, NullDocResolver};
use super::super::fixtures::{gh_issue_json, minimal_evidence, spec_ref_line, FixtureDocResolver};
use tachi_params::{
    check_proposal_replay, CanonicalDocRefV1, CurrentGroundStateV1, GroundingStatusV1,
    RepoRevisionV1, StalenessReasonV1,
};

const CAPTURED_AT: &str = "2026-07-13T00:00:00Z";

#[test]
fn a_fresh_proposal_replays_cleanly_against_its_own_pinned_state() {
    let body = "Fixture body for replay guard test.".to_string();
    let gh_json = gh_issue_json(
        9301,
        "Replay fixture",
        &body,
        "OPEN",
        &[],
        None,
        CAPTURED_AT,
        &[],
    );
    // R4-2: `check_proposal_replay` now rejects an empty repo-revision axis
    // outright, so a "replays cleanly" fixture must have a real one pinned.
    let resolver = FixtureDocResolver::new().with_repo_revision(RepoRevisionV1 {
        repo: "owner/repo".to_string(),
        git_ref: "origin/main".to_string(),
        commit_sha: "headsha0".to_string(),
        verified_at: CAPTURED_AT.to_string(),
    });
    let (_evidence, proposal) =
        build_refinery_packet("owner/repo", 9301, &gh_json, &resolver, CAPTURED_AT)
            .expect("build_refinery_packet");

    let current = CurrentGroundStateV1 {
        issue_snapshot_hash: proposal.based_on_issue_snapshot_hash.clone(),
        repo_revisions: proposal.based_on_repo_revisions.clone(),
        doc_revisions: proposal.based_on_doc_revisions.clone(),
    };
    assert!(check_proposal_replay(&proposal, &current).is_ok());
}

#[test]
fn source_and_proposal_hashes_bind_same_blob_to_its_exact_section() {
    let evidence = minimal_evidence("owner/repo#9300", "OPEN", &[]);
    let repo_revision = RepoRevisionV1 {
        repo: "owner/repo".to_string(),
        git_ref: "origin/main".to_string(),
        commit_sha: "headsha0".to_string(),
        verified_at: CAPTURED_AT.to_string(),
    };
    let section_three = CanonicalDocRefV1 {
        repo: "owner/repo".to_string(),
        trusted_ref: "origin/main".to_string(),
        commit_sha: "doccommit0".to_string(),
        path: "docs/spec.md".to_string(),
        blob_sha: "same-blob".to_string(),
        section: "3".to_string(),
        authority_receipt: "fixture:pre-registered".to_string(),
        verified_reachable_at: CAPTURED_AT.to_string(),
    };
    let mut section_four = section_three.clone();
    section_four.section = "4-issue-refinery-1002".to_string();

    let proposal_three = propose_disposition(
        &evidence,
        &RefinerySignalsV1::default(),
        vec![repo_revision.clone()],
        vec![section_three],
        &[],
        CAPTURED_AT,
    )
    .expect("section three proposal");
    let proposal_four = propose_disposition(
        &evidence,
        &RefinerySignalsV1::default(),
        vec![repo_revision],
        vec![section_four],
        &[],
        CAPTURED_AT,
    )
    .expect("section four proposal");

    assert_ne!(
        proposal_three.source_bundle_hash, proposal_four.source_bundle_hash,
        "same blob at a different canonical section is a different source bundle"
    );
    assert_ne!(
        proposal_three.proposal_hash, proposal_four.proposal_hash,
        "the proposal hash must inherit the exact canonical section identity"
    );

    let mut other_ref_revision = proposal_three.based_on_repo_revisions[0].clone();
    other_ref_revision.git_ref = "refs/heads/not-main".to_string();
    let other_ref_proposal = propose_disposition(
        &evidence,
        &RefinerySignalsV1::default(),
        vec![other_ref_revision],
        proposal_three.based_on_doc_revisions.clone(),
        &[],
        CAPTURED_AT,
    )
    .expect("other-ref proposal");
    assert_ne!(
        proposal_three.source_bundle_hash, other_ref_proposal.source_bundle_hash,
        "same commit under a different repo ref is a different source bundle"
    );
}

#[test]
fn an_edited_issue_body_after_the_proposal_was_built_makes_it_stale() {
    let resolver = FixtureDocResolver::new();

    let original_body = "Original body text before the edit.".to_string();
    let gh_json_v1 = gh_issue_json(
        9302,
        "Edit-then-replay fixture",
        &original_body,
        "OPEN",
        &[],
        None,
        "2026-07-13T00:00:00Z",
        &[],
    );
    let (_evidence_v1, proposal) =
        build_refinery_packet("owner/repo", 9302, &gh_json_v1, &resolver, CAPTURED_AT)
            .expect("build_refinery_packet v1");

    // Simulate a later edit to the same issue: the body changed, so a fresh
    // build now produces a different issue_snapshot_hash.
    let edited_body = "Body text was edited after the proposal was generated.".to_string();
    let gh_json_v2 = gh_issue_json(
        9302,
        "Edit-then-replay fixture",
        &edited_body,
        "OPEN",
        &[],
        None,
        "2026-07-14T00:00:00Z",
        &[],
    );
    let (evidence_v2, _proposal_v2) =
        build_refinery_packet("owner/repo", 9302, &gh_json_v2, &resolver, CAPTURED_AT)
            .expect("build_refinery_packet v2");

    assert_ne!(
        proposal.based_on_issue_snapshot_hash, evidence_v2.issue_snapshot_hash,
        "an edited body must change the semantic snapshot hash"
    );

    let current = CurrentGroundStateV1 {
        issue_snapshot_hash: evidence_v2.issue_snapshot_hash,
        repo_revisions: proposal.based_on_repo_revisions.clone(),
        doc_revisions: proposal.based_on_doc_revisions.clone(),
    };
    let err = check_proposal_replay(&proposal, &current)
        .expect_err("the old proposal must not replay against the edited state");
    assert!(!err.is_empty());
}

#[test]
fn a_doc_blob_sha_drift_after_the_proposal_was_built_makes_it_stale() {
    const COMMIT_SHA: &str = "12102bd92e2e6f6959aa1b20858b4fe8f2889585";
    const DOC_PATH: &str = "docs/engineering/architecture/issue-refinery-memory-lanes.md";
    let resolver = FixtureDocResolver::new().with_resolved(
        "owner/repo",
        COMMIT_SHA,
        DOC_PATH,
        "deadbeefblobsha0001",
        "3",
        "origin/main",
    );

    let body = format!(
        "Spec-pinned fixture.\n\nSpec-Ref: owner/repo:{DOC_PATH}@{COMMIT_SHA}/deadbeefblobsha0001#3\n"
    );
    let gh_json = gh_issue_json(
        9303,
        "Doc drift fixture",
        &body,
        "OPEN",
        &[],
        None,
        CAPTURED_AT,
        &[],
    );
    let (_evidence, proposal) =
        build_refinery_packet("owner/repo", 9303, &gh_json, &resolver, CAPTURED_AT)
            .expect("build_refinery_packet");
    assert_eq!(proposal.based_on_doc_revisions.len(), 1);

    // Simulate the doc being edited (new blob sha) after the proposal pinned
    // the old one.
    let mut drifted_doc_revisions = proposal.based_on_doc_revisions.clone();
    drifted_doc_revisions[0].blob_sha = "a-completely-different-blob-sha".to_string();

    let current = CurrentGroundStateV1 {
        issue_snapshot_hash: proposal.based_on_issue_snapshot_hash.clone(),
        repo_revisions: proposal.based_on_repo_revisions.clone(),
        doc_revisions: drifted_doc_revisions,
    };
    let err = check_proposal_replay(&proposal, &current)
        .expect_err("a drifted doc blob sha must reject replay");
    assert!(!err.is_empty());
}

/// #1002 acceptance criterion 6: without repo-tool evidence, the evidence
/// compiler can never mark a claim verified. Exercised across a battery of
/// representative fixtures (plain body, Spec-Ref body, multi-paragraph
/// body, minimal_evidence disposition fixture) — none of them ever
/// constructs `ClaimVerificationV1::Verified`, because `build_issue_evidence`
/// has no repo-tool evidence in hand and the type has no zero-evidence
/// `Verified` constructor (see `tachi_params::ClaimVerificationV1`).
#[test]
fn model_only_evidence_compiler_never_marks_a_claim_verified_across_fixtures() {
    let resolver = FixtureDocResolver::new().with_resolved(
        "owner/repo",
        "12102bd92e2e6f6959aa1b20858b4fe8f2889585",
        "docs/engineering/architecture/issue-refinery-memory-lanes.md",
        "deadbeefblobsha0001",
        "3",
        "origin/main",
    );

    let bodies = [
        "Plain single-paragraph body.".to_string(),
        format!(
            "Multi paragraph body.\n\nSecond paragraph here.\n\n{}",
            spec_ref_line("owner/repo")
        ),
        format!(
            "Spec-pinned paragraph.\n\nSpec-Ref: owner/repo:docs/engineering/architecture/issue-refinery-memory-lanes.md@12102bd92e2e6f6959aa1b20858b4fe8f2889585/deadbeefblobsha0001#3\n"
        ),
        "x".repeat(4_000),
    ];

    for (i, body) in bodies.iter().enumerate() {
        let gh_json = gh_issue_json(
            9400 + i as u64,
            "Verification-safety fixture",
            body,
            "OPEN",
            &[],
            None,
            CAPTURED_AT,
            &[],
        );
        let (evidence, _proposal) = build_refinery_packet(
            "owner/repo",
            9400 + i as u64,
            &gh_json,
            &resolver,
            CAPTURED_AT,
        )
        .unwrap_or_else(|e| panic!("build_refinery_packet fixture {i}: {e}"));
        assert!(
            evidence
                .claims
                .iter()
                .all(|c| !c.verification.is_verified()),
            "fixture {i}: model-only evidence compiler must never mark a claim verified"
        );
    }

    // Same property holds for the disposition-fixture builder used by
    // tests::disposition_rules (a different construction path entirely).
    let disposition_fixture_evidence = minimal_evidence("owner/repo#9500", "OPEN", &[]);
    assert!(disposition_fixture_evidence
        .claims
        .iter()
        .all(|c| !c.verification.is_verified()));
}

// ─── F1/R4-1 (build-seat REQUEST-CHANGES): fail-closed grounding ───────────

/// A `Spec-Ref:` line present in the body but not matching the frozen
/// syntax must NOT be silently skipped — it degrades grounding, same as an
/// unresolvable-but-well-formed one.
#[test]
fn malformed_spec_ref_line_degrades_grounding_instead_of_being_ignored() {
    let body =
        "This work has a broken spec pin.\n\nSpec-Ref: not-even-close-to-the-syntax\n".to_string();
    let gh_json = gh_issue_json(
        9310,
        "Malformed spec-ref fixture",
        &body,
        "OPEN",
        &[],
        None,
        CAPTURED_AT,
        &[],
    );
    let resolver = FixtureDocResolver::new();
    let (evidence, proposal) =
        build_refinery_packet("owner/repo", 9310, &gh_json, &resolver, CAPTURED_AT)
            .expect("build_refinery_packet");
    assert_eq!(evidence.grounding_status, GroundingStatusV1::MissingAnchor);
    assert!(!proposal.contradictions.is_empty());
}

/// A truncated/malformed `gh issue view` response — modeled here directly
/// as the non-object `Value` `gh_ops::issues::handle_gh_issue_read`'s
/// truncation fallback produces (wrapping unparseable output as a JSON
/// *string*) — must degrade grounding, not silently produce an
/// empty-but-`Grounded` snapshot.
#[test]
fn truncated_gh_result_degrades_grounding_instead_of_producing_an_empty_grounded_snapshot() {
    let truncated = serde_json::Value::String(
        "{\"number\":1,\"title\":\"Some issue\",\"body\":\"truncated mid stri".to_string(),
    );
    let resolver = FixtureDocResolver::new();
    let (evidence, proposal) =
        build_refinery_packet("owner/repo", 9311, &truncated, &resolver, CAPTURED_AT)
            .expect("build_refinery_packet must still produce a packet, just missing_anchor");
    assert_eq!(evidence.grounding_status, GroundingStatusV1::MissingAnchor);
    assert!(!proposal.contradictions.is_empty());
}

/// R4-1: a well-formed JSON OBJECT whose `number` is wrong-typed (a string,
/// not a number) must not be treated as Grounded either — type-checking,
/// not just shape-checking.
#[test]
fn wrong_typed_number_field_degrades_grounding() {
    let result = serde_json::json!({
        "number": "9312", // string, not a number
        "title": "Some issue",
        "state": "OPEN",
        "body": "Some body",
    });
    let resolver = FixtureDocResolver::new();
    let (evidence, proposal) =
        build_refinery_packet("owner/repo", 9312, &result, &resolver, CAPTURED_AT)
            .expect("build_refinery_packet");
    assert_eq!(evidence.grounding_status, GroundingStatusV1::MissingAnchor);
    assert!(!proposal.contradictions.is_empty());
}

/// R4-1: a minimal object with the RIGHT number but a MISMATCHED number is
/// also rejected — the response must identify the requested issue.
#[test]
fn mismatched_number_degrades_grounding() {
    let result = serde_json::json!({
        "number": 999999,
        "title": "A different issue entirely",
        "state": "OPEN",
    });
    let resolver = FixtureDocResolver::new();
    let (evidence, _proposal) =
        build_refinery_packet("owner/repo", 9313, &result, &resolver, CAPTURED_AT)
            .expect("build_refinery_packet");
    assert_eq!(evidence.grounding_status, GroundingStatusV1::MissingAnchor);
}

/// `GitRefResolver`'s repo-identity check runs BEFORE any git command, so
/// this is deterministic without a real git checkout: a `Spec-Ref:` line
/// declaring a different repo than the resolver's own `known_repo` must
/// never resolve, regardless of what `repo_root` even points at.
#[test]
fn git_ref_resolver_refuses_a_spec_ref_declaring_a_different_repo() {
    let resolver = GitRefResolver {
        repo_root: std::path::PathBuf::from("/nonexistent/not-a-real-checkout"),
        known_repo: "owner/repo".to_string(),
    };
    let resolution = resolver.resolve(
        "someone-else/other-repo",
        "docs/whatever.md",
        "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef",
        "blobsha",
        "1",
        "origin/main",
    );
    match resolution {
        DocResolution::Unresolved { reason } => {
            assert!(reason.contains("repo identity mismatch"), "got: {reason}");
        }
        DocResolution::Resolved(_) => {
            panic!("must never resolve a Spec-Ref against the wrong repo's checkout")
        }
    }
}

// ─── R4-3 (build-seat REQUEST-CHANGES, REGRESSION): no ambient-cwd guessing ─

/// `NullDocResolver` (used by `handle_refine_issues` when it cannot
/// establish a trusted repo root) must fail closed on both trait methods —
/// never resolve anything, never report a repo revision — rather than
/// falling back to some default that could be mistaken for a real result.
#[test]
fn null_doc_resolver_fails_closed_on_every_method() {
    let resolver = NullDocResolver {
        reason: "no reliable repo root found for 'owner/repo'".to_string(),
    };
    match resolver.resolve(
        "owner/repo",
        "docs/x.md",
        "sha1",
        "blob1",
        "1",
        "origin/main",
    ) {
        DocResolution::Unresolved { reason } => {
            assert!(reason.contains("no reliable repo root"));
        }
        DocResolution::Resolved(_) => panic!("NullDocResolver must never resolve"),
    }
    let revision_result = resolver.current_repo_revision("owner/repo", "origin/main");
    assert!(revision_result.is_err());
}

// ─── F2/R4-2 (build-seat REQUEST-CHANGES): the repo-revision replay axis ───

/// The live pipeline (`build_refinery_packet`) really does pin
/// `based_on_repo_revisions` from the resolver when one is available — the
/// axis `check_proposal_replay` needs to detect repo HEAD drift.
#[test]
fn live_pipeline_pins_repo_revision_when_the_resolver_supplies_one() {
    let body = "Fixture body with no Spec-Ref at all.".to_string();
    let gh_json = gh_issue_json(
        9320,
        "Repo revision fixture",
        &body,
        "OPEN",
        &[],
        None,
        CAPTURED_AT,
        &[],
    );
    let resolver = FixtureDocResolver::new().with_repo_revision(RepoRevisionV1 {
        repo: "owner/repo".to_string(),
        git_ref: "origin/main".to_string(),
        commit_sha: "headsha1".to_string(),
        verified_at: CAPTURED_AT.to_string(),
    });
    let (_evidence, proposal) =
        build_refinery_packet("owner/repo", 9320, &gh_json, &resolver, CAPTURED_AT)
            .expect("build_refinery_packet");
    assert_eq!(proposal.based_on_repo_revisions.len(), 1);
    assert_eq!(proposal.based_on_repo_revisions[0].commit_sha, "headsha1");
}

/// End-to-end: a proposal built while the resolver reports HEAD at
/// `headsha1` must be rejected for replay once the (simulated) current
/// state reports the repo has moved to `headsha2` — repo HEAD drift, not
/// just a specific doc's blob sha, must invalidate replay (F2).
#[test]
fn repo_head_drift_after_the_proposal_was_built_rejects_replay_through_the_real_pipeline() {
    let body = "Fixture body with no Spec-Ref at all.".to_string();
    let gh_json = gh_issue_json(
        9321,
        "Repo revision drift fixture",
        &body,
        "OPEN",
        &[],
        None,
        CAPTURED_AT,
        &[],
    );
    let resolver = FixtureDocResolver::new().with_repo_revision(RepoRevisionV1 {
        repo: "owner/repo".to_string(),
        git_ref: "origin/main".to_string(),
        commit_sha: "headsha1".to_string(),
        verified_at: CAPTURED_AT.to_string(),
    });
    let (_evidence, proposal) =
        build_refinery_packet("owner/repo", 9321, &gh_json, &resolver, CAPTURED_AT)
            .expect("build_refinery_packet");

    let mut drifted_repo_revisions = proposal.based_on_repo_revisions.clone();
    drifted_repo_revisions[0].commit_sha = "headsha2".to_string();
    let current = CurrentGroundStateV1 {
        issue_snapshot_hash: proposal.based_on_issue_snapshot_hash.clone(),
        repo_revisions: drifted_repo_revisions,
        doc_revisions: proposal.based_on_doc_revisions.clone(),
    };
    let err =
        check_proposal_replay(&proposal, &current).expect_err("repo HEAD drift must reject replay");
    assert!(!err.is_empty());
}

/// R4-2: when the resolver's repo-revision lookup fails (e.g. a real
/// `git rev-parse` failure), the live pipeline surfaces a real
/// contradiction — AND the resulting proposal (empty repo-revision axis)
/// must fail `check_proposal_replay`, not vacuously pass it.
#[test]
fn repo_revision_lookup_failure_surfaces_a_contradiction_and_rejects_replay() {
    let body = "Fixture body with no Spec-Ref at all.".to_string();
    let gh_json = gh_issue_json(
        9322,
        "Repo revision failure fixture",
        &body,
        "OPEN",
        &[],
        None,
        CAPTURED_AT,
        &[],
    );
    let resolver = FixtureDocResolver::new()
        .with_repo_revision_failure("git rev-parse origin/main failed: unknown revision");
    let (_evidence, proposal) =
        build_refinery_packet("owner/repo", 9322, &gh_json, &resolver, CAPTURED_AT)
            .expect("build_refinery_packet");

    assert!(proposal.preview_only, "V1 is always preview-only");
    assert!(
        proposal
            .contradictions
            .iter()
            .any(|c| c.description.contains("repo revision unavailable")),
        "a failed repo-revision lookup must surface a real contradiction, got: {:?}",
        proposal.contradictions
    );
    assert!(
        proposal.based_on_repo_revisions.is_empty(),
        "test setup: no repo revision should have been pinned"
    );

    let current = CurrentGroundStateV1 {
        issue_snapshot_hash: proposal.based_on_issue_snapshot_hash.clone(),
        repo_revisions: Vec::new(),
        doc_revisions: proposal.based_on_doc_revisions.clone(),
    };
    let err = check_proposal_replay(&proposal, &current)
        .expect_err("an empty repo-revision axis must never pass replay");
    assert!(err
        .iter()
        .any(|r| matches!(r, StalenessReasonV1::RepoRevisionUnavailable { .. })));
}
