//! RED→GREEN discrimination tests for the #1059 GitHub corpus adapter.

use super::adapt::{adapt_corpus_case, CorpusPilotReport};
use super::fixtures::{
    chain_bundle, fixture_bundle_for_case, fixture_reader_for_case, sample_issue_json,
};
use super::parse::assemble_case_bundle;
use super::pilot::{freeze_corpus_manifest, valid_20_cases, CorpusFreezeError, CORPUS_PILOT_SIZE};
use super::reader::{
    fetch_case_bundle, refuse_github_mutation, MutationProbe, FORBIDDEN_GITHUB_MUTATIONS,
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

    // Connected hops: issue → comment → pr → merge → reopen/revert
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
    );
    let edited = assemble_case_bundle(
        &case_id,
        "owner/repo",
        42,
        &edited_json,
        None,
        vec![],
        "2026-07-13T04:00:00Z",
    );
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

    let closed_result = adapt_corpus_case("proj", &manifest, &closed, None, None).expect("closed");
    let reopen_result =
        adapt_corpus_case("proj", &manifest, &reopened, None, None).expect("reopened");

    assert!(reopen_result.overturn_appended);
    assert!(reopen_result.evidence_refs.len() > closed_result.evidence_refs.len());

    // Prior outcome Supports hops (merge/close) remain; overturn APPENDs.
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
    // Old closed revision hash from the event chain is still present.
    assert!(reopen_result.evidence_refs.iter().any(|r| {
        matches!(
            &r.immutable_revision,
            ImmutableRevisionV1::IssueSnapshotHash(h) if h == "rev-closed"
        )
    }));
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
    // Adapter types live in tachi-server / tachi-params — portable evidence
    // kinds (`SourceKindV1::Issue` / `Pr`), never memcore GitHub SDK types.
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

    // Structural: this module path is not memcore.
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
    // No threshold auto-decision fields exist on the report.
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
