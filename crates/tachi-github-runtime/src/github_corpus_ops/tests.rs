//! RED→GREEN discrimination tests for the #1059 GitHub corpus adapter.
//!
//! Includes review rework cases that FAIL on a0f8e43e (C1/C2/C3/E1/E2) and
//! pass after the contract fixes.

use super::adapt::{adapt_corpus_case, CaseDraft, CorpusPilotReport};
use super::fixtures::{
    chain_bundle, fixture_bundle_for_case, fixture_reader_for_case, pure_revert_pair,
    sample_issue_json, sample_pr_json,
};
use super::parse::{assemble_case_bundle, parse_pr_snapshot_from_gh_json, ProvenanceEventKindV1};
use super::pilot::{freeze_corpus_manifest, valid_20_cases, CorpusFreezeError, CORPUS_PILOT_SIZE};
use super::reader::{
    baseline_events_from_snapshots, fetch_case_bundle, refuse_github_mutation, MutationProbe,
    FORBIDDEN_GITHUB_MUTATIONS,
};
use tachi_params::{
    EvidenceRelationV1, ImmutableRevisionV1, LessonCandidateStatusV1, LessonEngineReceiptV1,
    SourceKindV1,
};

fn frozen_manifest() -> super::pilot::CorpusManifestV1 {
    freeze_corpus_manifest(valid_20_cases()).expect("valid_20_cases must freeze")
}

/// Case-0 is always Precedent in `valid_20_cases` and is used by chain tests.
fn chain_case_id() -> String {
    "corpus-case-0".to_string()
}

#[test]
fn provenance_connected_and_revision_bound() {
    let manifest = frozen_manifest();
    let bundle = chain_bundle(&chain_case_id(), true);
    let result = adapt_corpus_case("proj", &manifest, &bundle, None, None).expect("adapt");

    let kinds: Vec<_> = result.evidence_refs.iter().map(|r| r.target_kind).collect();
    assert!(
        kinds.contains(&SourceKindV1::Issue),
        "chain must include Issue evidence"
    );
    assert!(
        kinds.contains(&SourceKindV1::Comment),
        "chain must include Comment evidence"
    );
    assert!(
        kinds.contains(&SourceKindV1::Pr),
        "chain must include Pr evidence"
    );
    assert!(
        kinds.contains(&SourceKindV1::Commit),
        "chain must include Commit (merge/revert) evidence"
    );

    for r in &result.evidence_refs {
        match &r.immutable_revision {
            ImmutableRevisionV1::IssueSnapshotHash(h)
            | ImmutableRevisionV1::IssueBodyHash(h)
            | ImmutableRevisionV1::PrSnapshotHash(h)
            | ImmutableRevisionV1::PrHeadSha(h)
            | ImmutableRevisionV1::BlobSha(h)
            | ImmutableRevisionV1::MemoryRevision(h) => {
                assert!(!h.is_empty(), "revision hash must be non-empty");
            }
            ImmutableRevisionV1::Comment {
                comment_id,
                updated_at,
                body_hash,
            } => {
                assert!(!comment_id.is_empty());
                assert!(!updated_at.is_empty());
                assert!(!body_hash.is_empty());
            }
        }
    }

    let sections: Vec<_> = result
        .evidence_refs
        .iter()
        .filter_map(|r| r.section_or_span.as_deref())
        .collect();
    assert!(sections.contains(&"pr_merged") || sections.contains(&"issue_closed"));
    assert!(sections.contains(&"reopen") || sections.contains(&"revert"));
}

#[test]
fn edited_comment_invalidates_snapshot_idempotent_replay() {
    let manifest = frozen_manifest();
    let case_id = chain_case_id();
    let base_json = sample_issue_json(42, "OPEN", "body", "2026-07-13T00:00:00Z");
    let mut edited_json = base_json.clone();
    edited_json["comments"][0]["body"] =
        serde_json::Value::String("Spec-Ref: docs/other.md\nedited body".to_string());
    edited_json["comments"][0]["updatedAt"] =
        serde_json::Value::String("2026-07-13T03:00:00Z".to_string());

    let base = assemble_case_bundle(
        &case_id,
        "owner/repo",
        42,
        &base_json,
        None,
        vec![],
        "2026-07-13T04:00:00Z",
    )
    .expect("base assemble");
    let edited = assemble_case_bundle(
        &case_id,
        "owner/repo",
        42,
        &edited_json,
        None,
        vec![],
        "2026-07-13T04:00:00Z",
    )
    .expect("edited assemble");
    assert_ne!(
        base.issue.issue_snapshot_hash, edited.issue.issue_snapshot_hash,
        "edited comment body/updated_at must invalidate issue_snapshot_hash"
    );

    let a1 = adapt_corpus_case("proj", &manifest, &base, None, None).expect("adapt1");
    let a2 = adapt_corpus_case("proj", &manifest, &base, None, None).expect("adapt2");
    assert_eq!(a1.candidate.candidate_id, a2.candidate.candidate_id);
    assert_eq!(a1.evidence_refs, a2.evidence_refs);
    assert_eq!(a1, a2, "idempotent adapt on equal bundle");
}

#[test]
fn closed_merged_is_outcome_never_established() {
    let manifest = frozen_manifest();
    let bundle = chain_bundle(&chain_case_id(), false);
    let result = adapt_corpus_case("proj", &manifest, &bundle, None, None).expect("adapt");

    assert!(
        result.outcome_evidence,
        "closed/merged must set outcome_evidence"
    );
    assert_eq!(
        result.candidate.candidate_status,
        LessonCandidateStatusV1::Pending
    );
    assert!(!result.candidate.claims_establishment());
    let status_json = serde_json::to_string(&result.candidate.candidate_status).unwrap();
    assert_eq!(status_json, "\"pending\"");
}

#[test]
fn revert_reopen_appends_overturn_does_not_rewrite() {
    let manifest = frozen_manifest();
    let closed = chain_bundle(&chain_case_id(), false);
    let reopened = chain_bundle(&chain_case_id(), true);

    // C1 fixture contract: overturn must NOT flip issue state (still CLOSED).
    assert_eq!(closed.issue.state, "CLOSED");
    assert_eq!(reopened.issue.state, "CLOSED");
    assert_eq!(
        closed.issue.issue_snapshot_hash, reopened.issue.issue_snapshot_hash,
        "overturn fixture must keep issue snapshot unchanged"
    );
    assert_eq!(
        closed.pull_request.as_ref().map(|p| &p.pr_snapshot_hash),
        reopened.pull_request.as_ref().map(|p| &p.pr_snapshot_hash),
        "overturn fixture must keep PR snapshot unchanged"
    );

    let closed_result = adapt_corpus_case("proj", &manifest, &closed, None, None).expect("closed");
    let reopen_result =
        adapt_corpus_case("proj", &manifest, &reopened, None, None).expect("reopened");

    assert!(reopen_result.overturn_appended);
    assert!(reopen_result.evidence_refs.len() > closed_result.evidence_refs.len());

    for section in ["pr_merged", "issue_closed"] {
        let prior = closed_result
            .evidence_refs
            .iter()
            .find(|r| {
                r.relation == EvidenceRelationV1::Supports
                    && r.section_or_span.as_deref() == Some(section)
            })
            .unwrap_or_else(|| panic!("closed adapt missing {section}"));
        assert!(
            reopen_result.evidence_refs.iter().any(|r| {
                r.target_ref == prior.target_ref
                    && r.relation == prior.relation
                    && r.immutable_revision == prior.immutable_revision
                    && r.section_or_span == prior.section_or_span
            }),
            "prior {section} evidence ref must remain after overturn append"
        );
    }
    assert!(reopen_result.evidence_refs.iter().any(|r| {
        r.relation == EvidenceRelationV1::Contradicts
            && r.section_or_span.as_deref() == Some("reopen")
    }));
    assert!(reopen_result.evidence_refs.iter().any(|r| {
        r.relation == EvidenceRelationV1::Contradicts
            && r.section_or_span.as_deref() == Some("revert")
    }));
    // Old closed revision (real issue snapshot hash) is still present.
    let closed_hash = &closed.issue.issue_snapshot_hash;
    assert!(reopen_result.evidence_refs.iter().any(|r| {
        matches!(
            &r.immutable_revision,
            ImmutableRevisionV1::IssueSnapshotHash(h) if h == closed_hash
        )
    }));
}

/// C1 discrimination: pure revert with **unchanged** issue/PR snapshots must
/// yield a distinct `candidate_id` / `source_revision`. Fails on a0f8e43e
/// (source_revision bound only to snapshot hashes).
#[test]
fn pure_revert_unchanged_snapshots_yields_distinct_candidate_id() {
    let manifest = frozen_manifest();
    let (base, with_revert) = pure_revert_pair(&chain_case_id());

    assert_eq!(
        base.issue.issue_snapshot_hash,
        with_revert.issue.issue_snapshot_hash
    );
    assert_eq!(
        base.pull_request.as_ref().map(|p| &p.pr_snapshot_hash),
        with_revert
            .pull_request
            .as_ref()
            .map(|p| &p.pr_snapshot_hash)
    );
    assert_eq!(base.issue.state, "CLOSED");
    assert_eq!(with_revert.issue.state, "CLOSED");

    let r1 = adapt_corpus_case("proj", &manifest, &base, None, None).expect("base");
    let r2 = adapt_corpus_case("proj", &manifest, &with_revert, None, None).expect("revert");

    assert_ne!(
        r1.candidate.source_revision, r2.candidate.source_revision,
        "event-chain must enter source_revision so pure revert differs"
    );
    assert_ne!(
        r1.candidate.candidate_id, r2.candidate.candidate_id,
        "pure revert must not collide on candidate_id when snapshots are unchanged"
    );
    assert!(r2.overturn_appended);
    assert!(r2.evidence_refs.iter().any(|r| {
        r.relation == EvidenceRelationV1::Contradicts
            && r.section_or_span.as_deref() == Some("revert")
    }));
}

/// C2 discrimination: baseline events must carry real snapshot hashes, never
/// fabricated `open-{n}` / `pr-open-{n}` labels.
#[test]
fn baseline_events_use_real_snapshot_hashes_not_fabricated_labels() {
    let cases = valid_20_cases();
    let case = cases
        .iter()
        .find(|c| c.pr_number.is_some())
        .expect("need a case with PR");
    let reader = fixture_reader_for_case(case);
    let bundle = fetch_case_bundle(&reader, case, vec![], "2026-07-13T00:00:00Z")
        .expect("fetch with empty hints builds baseline from content");

    let issue_opened = bundle
        .events
        .iter()
        .find(|e| e.kind == ProvenanceEventKindV1::IssueOpened)
        .expect("baseline IssueOpened");
    let pr_opened = bundle
        .events
        .iter()
        .find(|e| e.kind == ProvenanceEventKindV1::PrOpened)
        .expect("baseline PrOpened");

    assert_eq!(
        issue_opened.revision_hash, bundle.issue.issue_snapshot_hash,
        "IssueOpened revision_hash must equal computed issue_snapshot_hash"
    );
    assert_eq!(
        pr_opened.revision_hash,
        bundle
            .pull_request
            .as_ref()
            .expect("PR present")
            .pr_snapshot_hash,
        "PrOpened revision_hash must equal computed pr_snapshot_hash"
    );
    assert_ne!(
        issue_opened.revision_hash,
        format!("open-{}", case.issue_number),
        "must not fabricate open-{{n}} as a snapshot hash"
    );
    assert_ne!(
        pr_opened.revision_hash,
        format!("pr-open-{}", case.pr_number.unwrap()),
        "must not fabricate pr-open-{{n}} as a snapshot hash"
    );
    // SHA-256 hex is 64 chars.
    assert_eq!(issue_opened.revision_hash.len(), 64);
    assert_eq!(pr_opened.revision_hash.len(), 64);

    // Helper itself binds real hashes.
    let baseline = baseline_events_from_snapshots(
        &bundle.issue,
        bundle.pull_request.as_ref(),
        "2026-07-13T00:00:00Z",
    );
    assert_eq!(baseline[0].revision_hash, bundle.issue.issue_snapshot_hash);
}

/// C3 discrimination: result must carry full Issue/PR snapshots, not only hashes.
#[test]
fn result_carries_full_issue_and_pr_snapshots() {
    let manifest = frozen_manifest();
    let bundle = chain_bundle(&chain_case_id(), false);
    let result = adapt_corpus_case("proj", &manifest, &bundle, None, None).expect("adapt");

    assert_eq!(result.issue, bundle.issue);
    assert_eq!(result.pull_request, bundle.pull_request);
    assert_eq!(result.issue_snapshot_hash, result.issue.issue_snapshot_hash);
    assert!(!result.issue.body.is_empty());
    assert!(!result.issue.updated_at.is_empty());
    let pr = result.pull_request.as_ref().expect("PR snapshot required");
    assert!(!pr.head_sha.is_empty());
    assert!(!pr.base_sha.is_empty());
    assert!(!pr.reviews.is_empty());
    assert!(!pr.checks.is_empty());
    assert_eq!(
        result.pr_snapshot_hash.as_deref(),
        Some(pr.pr_snapshot_hash.as_str())
    );
}

/// E2 discrimination: derived prose consumes only issue title/body, so
/// coverage must be partial when comments/PR bodies are counted.
#[test]
fn derived_draft_reports_partial_coverage_when_comments_and_pr_not_in_prose() {
    let manifest = frozen_manifest();
    let bundle = chain_bundle(&chain_case_id(), false);
    let result = adapt_corpus_case("proj", &manifest, &bundle, None, None).expect("adapt");

    assert!(
        !result.candidate.coverage.is_full(),
        "derived draft only consumes issue title/body; must not claim full coverage \
         over comments+PR (source_bytes={}, covered_bytes={})",
        result.candidate.coverage.source_bytes,
        result.candidate.coverage.covered_bytes
    );
    assert!(result.candidate.coverage.covered_bytes < result.candidate.coverage.source_bytes);
    let expected_covered = format!("{}\n{}", bundle.issue.title, bundle.issue.body).len();
    assert_eq!(result.candidate.coverage.covered_bytes, expected_covered);
}

/// E2 over-claim regression: a long CUSTOM situation whose byte length exceeds
/// the counted source but that never contains the selected comment / PR body
/// must report `partial`, not `full`. The old `len().min(source_bytes)`
/// heuristic scored this `full`; honest containment accounting must not.
#[test]
fn long_custom_situation_missing_comments_and_pr_reports_partial_not_full() {
    let manifest = frozen_manifest();
    let bundle = chain_bundle(&chain_case_id(), false);

    // Issue title/body IS reproduced, then padded far past the counted source —
    // but the selected comment body and PR title/body are absent from the prose.
    let issue_block = format!("{}\n{}", bundle.issue.title, bundle.issue.body);
    let situation = format!("{issue_block}\n{}", "z".repeat(4096));
    let draft = CaseDraft {
        situation,
        proposed_ruling: "ruling".to_string(),
        why: "why".to_string(),
        how_to_apply: "apply".to_string(),
    };

    let result = adapt_corpus_case("proj", &manifest, &bundle, Some(draft), None).expect("adapt");

    assert!(
        result.candidate.situation.len() > result.candidate.coverage.source_bytes,
        "situation is deliberately longer than the counted source"
    );
    assert!(
        !result.candidate.coverage.is_full(),
        "byte length alone must not buy full coverage (source_bytes={}, covered_bytes={})",
        result.candidate.coverage.source_bytes,
        result.candidate.coverage.covered_bytes,
    );
    assert!(result.candidate.coverage.covered_bytes < result.candidate.coverage.source_bytes);
    // Only the issue block is contained, so covered == its byte cost exactly.
    assert_eq!(result.candidate.coverage.covered_bytes, issue_block.len());
}

/// E2 empty-segment edge: an EMPTY PR body must not earn a `contains("")`
/// credit (nor inflate the denominator). Coverage is still `partial` here
/// because the selected comment body was never consumed; the empty PR body
/// contributes 0 to both covered_bytes and source_bytes.
#[test]
fn long_pr_body_empty_handles_containment_correctly() {
    let manifest = frozen_manifest();

    let issue_json = sample_issue_json(42, "CLOSED", "Pilot body text", "2026-07-13T00:00:00Z");
    let mut pr_json = sample_pr_json(77, "MERGED", true, "2026-07-13T01:00:00Z");
    pr_json["body"] = serde_json::Value::String(String::new());

    let bundle = assemble_case_bundle(
        &chain_case_id(),
        "owner/repo",
        42,
        &issue_json,
        Some((77, &pr_json)),
        vec![],
        "2026-07-14T00:00:00Z",
    )
    .expect("assemble with empty PR body");
    assert!(
        bundle.pull_request.as_ref().unwrap().body.is_empty(),
        "test precondition: PR body is empty"
    );

    // Situation reproduces the issue block AND the PR title, but the selected
    // comment is left out and the PR body is empty.
    let issue_block = format!("{}\n{}", bundle.issue.title, bundle.issue.body);
    let pr_title = bundle.pull_request.as_ref().unwrap().title.clone();
    let comment_body = bundle.issue.selected_comment_revisions[0].body.clone();
    assert!(!comment_body.is_empty(), "test needs a non-empty comment");
    let draft = CaseDraft {
        situation: format!("{issue_block}\n{pr_title}"),
        proposed_ruling: "ruling".to_string(),
        why: "why".to_string(),
        how_to_apply: "apply".to_string(),
    };

    let result = adapt_corpus_case("proj", &manifest, &bundle, Some(draft), None).expect("adapt");

    assert!(
        !result.candidate.coverage.is_full(),
        "empty PR body must not spuriously complete coverage (source_bytes={}, covered_bytes={})",
        result.candidate.coverage.source_bytes,
        result.candidate.coverage.covered_bytes,
    );
    // covered = issue block + PR title; source additionally counts the
    // (non-empty) selected comment. The empty PR body counts in NEITHER —
    // no free separator byte, no contains("") credit.
    let expected_covered = issue_block.len() + (1 + pr_title.len());
    let expected_source = expected_covered + (1 + comment_body.len());
    assert_eq!(result.candidate.coverage.covered_bytes, expected_covered);
    assert_eq!(result.candidate.coverage.source_bytes, expected_source);
}

/// Idempotency regression: the SAME issue/PR content read at two DIFFERENT
/// crawl times must yield the SAME candidate_id. Baseline events stamp
/// `occurred_at = captured_at`, so before the fix (crawl-time in the identity
/// string) the two adapts diverged. Identity binds content/event only.
#[test]
fn same_content_different_crawl_time_yields_identical_candidate_id() {
    let manifest = frozen_manifest();
    let cases = valid_20_cases();
    let case = cases
        .iter()
        .find(|c| c.pr_number.is_some())
        .expect("need a case with PR");
    let reader = fixture_reader_for_case(case);

    // Identical content, two crawl times. Baseline events carry occurred_at =
    // captured_at, so the ONLY difference between the bundles is crawl-time.
    let early = fetch_case_bundle(&reader, case, vec![], "2026-07-13T00:00:00Z").expect("early");
    let late = fetch_case_bundle(&reader, case, vec![], "2026-08-01T09:30:00Z").expect("late");

    assert_ne!(early.captured_at, late.captured_at);
    assert_ne!(
        early.events[0].occurred_at, late.events[0].occurred_at,
        "baseline events must actually carry the differing crawl time"
    );
    assert_eq!(
        early.issue.issue_snapshot_hash, late.issue.issue_snapshot_hash,
        "content (and thus snapshot hash) is identical across crawl times"
    );

    let a = adapt_corpus_case("proj", &manifest, &early, None, None).expect("early adapt");
    let b = adapt_corpus_case("proj", &manifest, &late, None, None).expect("late adapt");

    assert_eq!(
        a.candidate.source_revision, b.candidate.source_revision,
        "crawl-time must not enter source_revision"
    );
    assert_eq!(
        a.candidate.candidate_id, b.candidate.candidate_id,
        "same content at different crawl times must yield the same candidate_id"
    );
}

#[test]
fn unordered_api_collections_are_permutation_stable_but_comments_remain_ordered() {
    let manifest = frozen_manifest();
    let case_id = chain_case_id();
    let mut issue_json = sample_issue_json(42, "CLOSED", "Body", "2026-07-13T03:00:00Z");
    issue_json["labels"] = serde_json::json!([
        {"name": "priority:p1"},
        {"name": "type:bug"}
    ]);
    issue_json["comments"] = serde_json::json!([
        {
            "id": "comment-1",
            "body": "Spec-Ref: owner/repo:docs/a.md@commit/blob#a",
            "updatedAt": "2026-07-13T01:00:00Z",
            "author": {"login": "owner"}
        },
        {
            "id": "comment-2",
            "body": "Spec-Ref: owner/repo:docs/b.md@commit/blob#b",
            "updatedAt": "2026-07-13T02:00:00Z",
            "author": {"login": "owner"}
        }
    ]);
    let mut pr_json = sample_pr_json(77, "MERGED", true, "2026-07-13T03:00:00Z");
    pr_json["reviews"] = serde_json::json!([
        {
            "author": {"login": "bob"},
            "state": "CHANGES_REQUESTED",
            "submittedAt": "2026-07-13T02:00:00Z"
        },
        {
            "author": {"login": "alice"},
            "state": "APPROVED",
            "submittedAt": "2026-07-13T01:00:00Z"
        }
    ]);
    pr_json["statusCheckRollup"] = serde_json::json!([
        {"name": "lint", "conclusion": "SUCCESS", "status": "COMPLETED"},
        {"name": "test", "conclusion": "SUCCESS", "status": "COMPLETED"}
    ]);

    let assemble = |issue: &serde_json::Value, pr: &serde_json::Value| {
        assemble_case_bundle(
            &case_id,
            "owner/repo",
            42,
            issue,
            Some((77, pr)),
            vec![],
            "2026-07-13T04:00:00Z",
        )
        .expect("assemble")
    };
    let base = assemble(&issue_json, &pr_json);
    let base_candidate = adapt_corpus_case("proj", &manifest, &base, None, None)
        .expect("adapt base")
        .candidate;

    let mut labels_permuted = issue_json.clone();
    labels_permuted["labels"].as_array_mut().unwrap().reverse();
    let labels = assemble(&labels_permuted, &pr_json);
    assert_eq!(base.issue.labels, labels.issue.labels);
    assert_eq!(
        base.issue.issue_snapshot_hash,
        labels.issue.issue_snapshot_hash
    );
    assert_eq!(
        base_candidate.candidate_id,
        adapt_corpus_case("proj", &manifest, &labels, None, None)
            .expect("adapt labels")
            .candidate
            .candidate_id
    );

    let mut reviews_permuted = pr_json.clone();
    reviews_permuted["reviews"]
        .as_array_mut()
        .unwrap()
        .reverse();
    let reviews = assemble(&issue_json, &reviews_permuted);
    assert_eq!(
        base.pull_request.as_ref().unwrap().reviews,
        reviews.pull_request.as_ref().unwrap().reviews
    );
    assert_eq!(
        base.pull_request.as_ref().unwrap().pr_snapshot_hash,
        reviews.pull_request.as_ref().unwrap().pr_snapshot_hash
    );
    assert_eq!(
        base_candidate.candidate_id,
        adapt_corpus_case("proj", &manifest, &reviews, None, None)
            .expect("adapt reviews")
            .candidate
            .candidate_id
    );

    let mut checks_permuted = pr_json.clone();
    checks_permuted["statusCheckRollup"]
        .as_array_mut()
        .unwrap()
        .reverse();
    let checks = assemble(&issue_json, &checks_permuted);
    assert_eq!(
        base.pull_request.as_ref().unwrap().checks,
        checks.pull_request.as_ref().unwrap().checks
    );
    assert_eq!(
        base.pull_request.as_ref().unwrap().pr_snapshot_hash,
        checks.pull_request.as_ref().unwrap().pr_snapshot_hash
    );
    assert_eq!(
        base_candidate.candidate_id,
        adapt_corpus_case("proj", &manifest, &checks, None, None)
            .expect("adapt checks")
            .candidate
            .candidate_id
    );

    let mut comments_reordered = issue_json.clone();
    comments_reordered["comments"]
        .as_array_mut()
        .unwrap()
        .reverse();
    let comments = assemble(&comments_reordered, &pr_json);
    assert_ne!(
        base.issue.issue_snapshot_hash, comments.issue.issue_snapshot_hash,
        "comment revision sequence is semantically ordered"
    );
    assert_ne!(
        base_candidate.candidate_id,
        adapt_corpus_case("proj", &manifest, &comments, None, None)
            .expect("adapt comments")
            .candidate
            .candidate_id
    );
}

/// target_ref collision: two event chains that differ ONLY in one hop's
/// `target_ref` (same kind, revision_hash, snapshots, crawl-time) must yield
/// distinct candidate_ids. Before the fix, target_ref was omitted from the
/// identity string and the chains collided.
#[test]
fn chains_differing_only_in_target_ref_yield_distinct_candidate_ids() {
    let manifest = frozen_manifest();
    let base = chain_bundle(&chain_case_id(), false);
    let mut variant = base.clone();

    let idx = variant
        .events
        .iter()
        .position(|e| e.kind == ProvenanceEventKindV1::PrMerged)
        .expect("chain has PrMerged");
    assert_ne!(variant.events[idx].target_ref, "owner/repo#999");
    variant.events[idx].target_ref = "owner/repo#999".to_string();

    // Snapshots and every other event field stay identical — only target_ref moved.
    assert_eq!(
        base.issue.issue_snapshot_hash,
        variant.issue.issue_snapshot_hash
    );
    assert_eq!(
        base.pull_request.as_ref().map(|p| &p.pr_snapshot_hash),
        variant.pull_request.as_ref().map(|p| &p.pr_snapshot_hash)
    );
    for (a, b) in base.events.iter().zip(variant.events.iter()) {
        assert_eq!(a.kind, b.kind);
        assert_eq!(a.revision_hash, b.revision_hash);
        assert_eq!(a.occurred_at, b.occurred_at);
    }

    let r1 = adapt_corpus_case("proj", &manifest, &base, None, None).expect("base");
    let r2 = adapt_corpus_case("proj", &manifest, &variant, None, None).expect("variant");

    assert_ne!(
        r1.candidate.source_revision, r2.candidate.source_revision,
        "event target_ref must enter source_revision"
    );
    assert_ne!(
        r1.candidate.candidate_id, r2.candidate.candidate_id,
        "chains differing only in target_ref must not collide on candidate_id"
    );
}

#[test]
fn mutation_refusal_zero_github_writes() {
    for op in FORBIDDEN_GITHUB_MUTATIONS {
        let err = refuse_github_mutation(op).expect_err("mutation must refuse");
        assert!(err.contains("read-only") || err.contains("refusing"));
    }

    let cases = valid_20_cases();
    let case = &cases[0];
    let fixture = fixture_reader_for_case(case);
    let probe = MutationProbe::new(fixture);
    let _bundle = fetch_case_bundle(&probe, case, vec![], "2026-07-13T00:00:00Z")
        .expect("fetch via reads only");
    probe.assert_no_mutations();
    let calls = probe.recorded_calls();
    assert!(calls.iter().any(|c| c.starts_with("read_issue:")));
    if case.pr_number.is_some() {
        assert!(calls.iter().any(|c| c.starts_with("read_pr:")));
    }

    for op in FORBIDDEN_GITHUB_MUTATIONS {
        assert!(probe.attempt_mutation(op).is_err());
    }
}

#[test]
fn candidate_never_established_and_preview_identity() {
    let manifest = frozen_manifest();
    let bundle = chain_bundle(&chain_case_id(), false);

    let missing = adapt_corpus_case("proj", &manifest, &bundle, None, None).expect("missing");
    assert_eq!(missing.candidate.identity_status(), "preview_only");
    assert!(!missing.candidate.claims_establishment());

    let fallback = LessonEngineReceiptV1 {
        requested_role: "proposal".to_string(),
        effective_provider: Some("openai".to_string()),
        effective_model: Some("gpt".to_string()),
        effective_version: Some("1".to_string()),
        fallback_chain: vec!["backup".to_string()],
        degraded: false,
    };
    let with_fallback =
        adapt_corpus_case("proj", &manifest, &bundle, None, Some(fallback)).expect("fallback");
    assert_eq!(with_fallback.candidate.identity_status(), "preview_only");

    let degraded = LessonEngineReceiptV1 {
        requested_role: "proposal".to_string(),
        effective_provider: Some("openai".to_string()),
        effective_model: Some("gpt".to_string()),
        effective_version: Some("1".to_string()),
        fallback_chain: vec![],
        degraded: true,
    };
    let with_degraded =
        adapt_corpus_case("proj", &manifest, &bundle, None, Some(degraded)).expect("degraded");
    assert_eq!(with_degraded.candidate.identity_status(), "preview_only");
}

#[test]
fn no_portable_kernel_github_dependency() {
    let manifest = frozen_manifest();
    let bundle = chain_bundle(&chain_case_id(), false);
    let result = adapt_corpus_case("proj", &manifest, &bundle, None, None).expect("adapt");
    assert!(!result.candidate.claims_establishment());
    assert!(result
        .evidence_refs
        .iter()
        .any(|r| r.target_kind == SourceKindV1::Issue));
    assert!(result
        .evidence_refs
        .iter()
        .any(|r| r.target_kind == SourceKindV1::Pr));

    adapter_module_path_is_not_memcore();
}

#[cfg(test)]
fn adapter_module_path_is_not_memcore() {
    let path = module_path!();
    assert!(
        path.contains("github_corpus_ops"),
        "adapter tests must live under github_corpus_ops"
    );
    assert!(
        !path.contains("memcore"),
        "adapter must not live under memcore"
    );
}

#[test]
fn freeze_requires_exactly_20() {
    let mut cases = valid_20_cases();
    cases.pop();
    let errors = freeze_corpus_manifest(cases).expect_err("19 must fail");
    assert!(errors.contains(&CorpusFreezeError::WrongCaseCount {
        expected: CORPUS_PILOT_SIZE,
        actual: 19
    }));

    let mut cases = valid_20_cases();
    let extra = cases[0].clone();
    let mut extra = extra;
    extra.case_id = "extra".to_string();
    cases.push(extra);
    let errors = freeze_corpus_manifest(cases).expect_err("21 must fail");
    assert!(errors.contains(&CorpusFreezeError::WrongCaseCount {
        expected: CORPUS_PILOT_SIZE,
        actual: 21
    }));

    let mut cases = valid_20_cases();
    cases[0].selection_reason.clear();
    let errors = freeze_corpus_manifest(cases).expect_err("missing reason must fail");
    assert!(errors
        .iter()
        .any(|e| matches!(e, CorpusFreezeError::MissingSelectionReason { .. })));
}

#[test]
fn freeze_and_adapt_all_20_yields_pilot_report() {
    let cases = valid_20_cases();
    let manifest = freeze_corpus_manifest(cases.clone()).expect("freeze 20");
    let mut results = Vec::new();
    for case in &cases {
        let bundle = fixture_bundle_for_case(case);
        let result = adapt_corpus_case("proj", &manifest, &bundle, None, None)
            .unwrap_or_else(|e| panic!("adapt {} failed: {e}", case.case_id));
        assert_eq!(
            result.candidate.candidate_status,
            LessonCandidateStatusV1::Pending
        );
        assert!(!result.candidate.claims_establishment());
        results.push(result);
    }
    let report = CorpusPilotReport::from_results(&results);
    assert_eq!(report.cases, 20);
    assert_eq!(report.candidates_emitted, 20);
    assert!(report.outcome_evidence_count > 0);
}

#[test]
fn unselected_case_is_refused() {
    let manifest = frozen_manifest();
    let mut bundle = chain_bundle(&chain_case_id(), false);
    bundle.case_id = "not-in-manifest".to_string();
    let err = adapt_corpus_case("proj", &manifest, &bundle, None, None).expect_err("refuse");
    assert!(matches!(
        err,
        super::adapt::AdaptError::NotInFrozenManifest { .. }
    ));
}

/// E1 also exercised via under-fetched assemble path.
#[test]
fn assemble_refuses_pr_missing_head_sha() {
    let mut pr = sample_pr_json(1, "OPEN", false, "2026-07-13T00:00:00Z");
    pr.as_object_mut().unwrap().remove("headRefOid");
    let issue = sample_issue_json(1, "OPEN", "body", "2026-07-13T00:00:00Z");
    let err = assemble_case_bundle(
        "corpus-case-0",
        "owner/repo",
        1,
        &issue,
        Some((1, &pr)),
        vec![],
        "2026-07-13T00:00:00Z",
    )
    .expect_err("missing head_sha must refuse");
    assert!(err.to_string().contains("head_sha"));
    // Direct parser path too.
    assert!(parse_pr_snapshot_from_gh_json("owner/repo", 1, &pr).is_err());
}
