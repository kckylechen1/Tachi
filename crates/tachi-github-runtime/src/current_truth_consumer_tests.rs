//! #1693 consumer fixture over the CurrentTruth v1 read surface (#1696
//! discrimination 11).
//!
//! This crate is the GitHub-domain runtime and a downstream consumer of
//! `tachi-params`: the test proves the consumer boundary from **outside**
//! the defining crate — evidence heads and source revisions are readable
//! through `tachi_params::current_truth::consumer` view types alone, with
//! no `AssertionV1`, no store row shape, and no reducer internals imported.

use tachi_params::current_truth::consumer::{self, CallerAuthorizationV1};
use tachi_params::current_truth::refresh::{
    mint_assertions, GithubRepositoryStateV1, SnapshotIssueStateV1, SnapshotIssueV1,
    SnapshotPrStateV1, SnapshotPrV1,
};
use tachi_params::current_truth::store::CurrentTruthSqliteStore;
use tachi_params::current_truth::types::{PredicateV1, ReductionStatusV1, VisibilityClassV1};

const REPO: &str = "kckylechen1/tachi";

#[test]
fn consumer_reads_evidence_heads_and_revisions_across_the_crate_boundary() {
    let state = GithubRepositoryStateV1 {
        repo: REPO.to_string(),
        refresh_revision: "r1".to_string(),
        refreshed_at: "2026-08-26T11:00:00Z".to_string(),
        issues: vec![SnapshotIssueV1 {
            number: 42,
            state: SnapshotIssueStateV1::Open,
            updated_at: "2026-08-26T11:00:00Z".to_string(),
            snapshot_revision: "iss42-a1".to_string(),
            visibility: VisibilityClassV1::Public,
        }],
        pull_requests: vec![SnapshotPrV1 {
            number: 7,
            state: SnapshotPrStateV1::Merged,
            merge_commit_sha: Some("merge007".to_string()),
            updated_at: "2026-08-26T11:00:00Z".to_string(),
            snapshot_revision: "pr7-b2".to_string(),
            linked_issues: vec![42],
            visibility: VisibilityClassV1::Public,
        }],
        observations: vec![],
    };

    let store = CurrentTruthSqliteStore::open_in_memory().expect("open store");
    store.append_all(&mint_assertions(&state)).expect("append");
    store
        .record_refresh(
            REPO,
            true,
            Some("r1"),
            Some("2026-08-26T11:00:00Z"),
            "2026-08-26T11:00:05Z",
            None,
        )
        .expect("record posture");

    let view = consumer::read_view(
        &store,
        REPO,
        CallerAuthorizationV1 {
            sees_private: false,
        },
    )
    .expect("consumer view");

    assert!(view.posture.fresh);
    assert_eq!(view.posture.last_fresh_revision.as_deref(), Some("r1"));

    let issue_row = view
        .subjects
        .iter()
        .find(|subject| subject.subject_token == "kckylechen1/tachi#issue:42")
        .expect("issue row");
    let implementation = issue_row
        .predicates
        .iter()
        .find(|predicate| predicate.predicate == PredicateV1::ImplementationPresent)
        .expect("implementation_present row");
    assert_eq!(implementation.status, ReductionStatusV1::Current);
    let head = implementation
        .evidence_heads
        .first()
        .expect("evidence head");
    // Per-issue mint: presence carries the composite revision token over
    // the issue snapshot AND every linked PR's snapshot.
    assert!(head.source_revision.starts_with("composite-"));
    assert_eq!(head.source, "github-snapshot-adapter");

    // The merged PR row is visible with its merge SHA as the value token.
    let pr_row = view
        .subjects
        .iter()
        .find(|subject| subject.subject_token == "kckylechen1/tachi#pull_request:7")
        .expect("PR row");
    let merged = pr_row
        .predicates
        .iter()
        .find(|predicate| predicate.predicate == PredicateV1::PrMerged)
        .expect("pr_merged row");
    assert_eq!(merged.status, ReductionStatusV1::Current);
    assert_eq!(merged.value_token, "merge007");
}
