//! #1693 cross-crate consumer fixture: this crate (the GitHub-domain
//! runtime) is a downstream consumer of `tachi-params`. The test proves
//! the Unified Work Read Model boundary from **outside** the defining
//! crate: a consumer builds the projection from a CurrentTruth consumer
//! view plus typed work facts — importing view/projection types only, no
//! `AssertionV1`, no store row shapes, no reducer internals — and the
//! owner-ruled R6-2 transition-debt semantics hold end to end.

use tachi_params::current_truth::consumer::{self, CallerAuthorizationV1};
use tachi_params::current_truth::refresh::{
    mint_assertions, GithubRepositoryStateV1, SnapshotIssueStateV1, SnapshotIssueV1,
    SnapshotObservationKindV1, SnapshotObservationV1, SnapshotPrStateV1, SnapshotPrV1,
};
use tachi_params::current_truth::store::CurrentTruthSqliteStore;
use tachi_params::current_truth::types::VisibilityClassV1;
use tachi_params::taskintent::mapping::adjudication::CanonicalAdjudicationFact;
use tachi_params::taskintent::plan::LifecycleMode;
use tachi_params::work_read_model::{
    project, rebuild, AdjudicationFactV1, BlockerKindV1, ClaimModeV1, ClaimStateV1, DebtStateV1,
    ImplementationStatusV1, NextActionKindV1, ProjectionOptions, RunReceiptFactV1, SourceFacts,
    SourceKind, SourceSnapshot, WorkClaimFactV1,
};

const REPO: &str = "kckylechen1/tachi";
const ISSUE_TOKEN: &str = "kckylechen1/tachi#issue:42";

fn issue(number: u64, state: SnapshotIssueStateV1, at: &str, rev: &str) -> SnapshotIssueV1 {
    SnapshotIssueV1 {
        number,
        state,
        updated_at: at.to_string(),
        snapshot_revision: rev.to_string(),
        visibility: VisibilityClassV1::Public,
    }
}

fn pr(
    number: u64,
    state: SnapshotPrStateV1,
    sha: Option<&str>,
    at: &str,
    rev: &str,
    linked: Vec<u64>,
) -> SnapshotPrV1 {
    SnapshotPrV1 {
        number,
        state,
        merge_commit_sha: sha.map(str::to_string),
        updated_at: at.to_string(),
        snapshot_revision: rev.to_string(),
        linked_issues: linked,
        visibility: VisibilityClassV1::Public,
    }
}

fn observation(kind: SnapshotObservationKindV1, at: &str, rev: &str) -> SnapshotObservationV1 {
    SnapshotObservationV1 {
        kind,
        observed_at: at.to_string(),
        revision: rev.to_string(),
        visibility: VisibilityClassV1::Public,
    }
}

/// Merged -> reverted -> steady resnapshot still reporting the original PR
/// merged: the projection stays repair-blocked across the crate boundary.
#[test]
fn cross_crate_work_read_model_keeps_r6_2_debt_behind_steady_state() {
    let merged = GithubRepositoryStateV1 {
        repo: REPO.to_string(),
        refresh_revision: "r1".to_string(),
        refreshed_at: "2026-08-26T09:00:00Z".to_string(),
        issues: vec![issue(
            42,
            SnapshotIssueStateV1::Open,
            "2026-08-26T09:00:00Z",
            "iss42-a1",
        )],
        pull_requests: vec![pr(
            7,
            SnapshotPrStateV1::Merged,
            Some("merge007"),
            "2026-08-26T09:00:00Z",
            "pr7-b1",
            vec![42],
        )],
        observations: vec![],
    };
    let reverted = GithubRepositoryStateV1 {
        repo: REPO.to_string(),
        refresh_revision: "r2".to_string(),
        refreshed_at: "2026-08-26T10:00:00Z".to_string(),
        issues: vec![issue(
            42,
            SnapshotIssueStateV1::Open,
            "2026-08-26T10:00:00Z",
            "iss42-a2",
        )],
        pull_requests: vec![pr(
            7,
            SnapshotPrStateV1::Merged,
            Some("merge007"),
            "2026-08-26T10:00:00Z",
            "pr7-b2",
            vec![42],
        )],
        observations: vec![observation(
            SnapshotObservationKindV1::MergeReverted {
                number: 7,
                revert_commit_sha: "revert009".to_string(),
                original_merge_sha: "merge007".to_string(),
            },
            "2026-08-26T10:30:00Z",
            "rev2-revert",
        )],
    };
    let steady = GithubRepositoryStateV1 {
        repo: REPO.to_string(),
        refresh_revision: "r3".to_string(),
        refreshed_at: "2026-08-26T11:00:00Z".to_string(),
        issues: vec![issue(
            42,
            SnapshotIssueStateV1::Open,
            "2026-08-26T11:00:00Z",
            "iss42-a3",
        )],
        pull_requests: vec![pr(
            7,
            SnapshotPrStateV1::Merged,
            Some("merge007"),
            "2026-08-26T11:00:00Z",
            "pr7-b3",
            vec![42],
        )],
        observations: vec![],
    };

    let store = CurrentTruthSqliteStore::open_in_memory().expect("open store");
    for state in [&merged, &reverted, &steady] {
        store.append_all(&mint_assertions(state)).expect("append");
    }
    store
        .record_refresh(
            REPO,
            true,
            Some("r3"),
            Some("2026-08-26T11:00:00Z"),
            "2026-08-26T11:00:05Z",
            None,
        )
        .expect("posture");
    let view = consumer::read_view(
        &store,
        REPO,
        CallerAuthorizationV1 {
            sees_private: false,
        },
    )
    .expect("consumer view");

    let snapshots = vec![
        SourceSnapshot::new(
            SourceKind::CurrentTruth {
                repo: REPO.to_string(),
            },
            "ct-1",
            "2026-08-27T12:00:00Z",
            SourceFacts::CurrentTruth(Box::new(view)),
        )
        .expect("ct snapshot"),
        SourceSnapshot::new(
            SourceKind::WorkClaims,
            "claims-1",
            "2026-08-27T12:00:00Z",
            SourceFacts::WorkClaims(vec![WorkClaimFactV1 {
                claim_id: "claim-1".to_string(),
                agent_identity_id: Some("agent-1".to_string()),
                session_client: Some("cli".to_string()),
                issue_ref: Some(format!("{REPO}#42")),
                dispatch_id: Some("dispatch-1".to_string()),
                branch: "feat/y".to_string(),
                worktree_path: Some("/wt/y".to_string()),
                role: Some("worker".to_string()),
                mode: ClaimModeV1::Writable,
                expected_head: Some("merge007".to_string()),
                lease_expires_at: Some("2026-08-27T13:00:00Z".to_string()),
                transition_version: 1,
                exec_env_id: None,
                state: ClaimStateV1::Active,
                heartbeat_at: "2026-08-27T10:00:00Z".to_string(),
                visibility: VisibilityClassV1::Public,
            }]),
        )
        .expect("claims snapshot"),
        SourceSnapshot::new(
            SourceKind::RunReceipts,
            "runs-1",
            "2026-08-27T12:00:00Z",
            SourceFacts::RunReceipts(vec![RunReceiptFactV1 {
                dispatch_id: "dispatch-1".to_string(),
                assignment_id: None,
                lifecycle_owner: LifecycleMode::TachiManagedBatch,
                state_token: "working".to_string(),
                started_at: Some("2026-08-27T10:05:00Z".to_string()),
                finished_at: None,
                exit_code: None,
                run_dir: "/runs/y".to_string(),
                visibility: VisibilityClassV1::Public,
            }]),
        )
        .expect("runs snapshot"),
        SourceSnapshot::new(
            SourceKind::Adjudication,
            "adj-1",
            "2026-08-27T12:00:00Z",
            SourceFacts::Adjudication(vec![AdjudicationFactV1 {
                dispatch_id: "dispatch-1".to_string(),
                fact: CanonicalAdjudicationFact::NotRequired {
                    reason: "read-model fixture".to_string(),
                },
            }]),
        )
        .expect("adjudication snapshot"),
    ];

    let options = ProjectionOptions::new("2026-08-27T12:00:00Z");
    let set = project(&rebuild(snapshots), &options);
    let model = set
        .items
        .iter()
        .find(|model| model.work_token() == ISSUE_TOKEN)
        .expect("work item");

    assert!(
        model
            .next_actions
            .iter()
            .any(|a| a.kind == NextActionKindV1::RepairRevertOrReopen),
        "the ruled transition debt surfaces as a repair action across the crate boundary"
    );
    assert!(model
        .blockers
        .iter()
        .any(|b| b.kind == BlockerKindV1::OutstandingTransitionDebt));
    assert!(!model.success_shaped);
    let github = match &model.github {
        tachi_params::work_read_model::SectionState::Available(section) => section,
        other => panic!("github section: {other:?}"),
    };
    assert!(matches!(
        &github.transition_debt.revert,
        DebtStateV1::Outstanding { .. }
    ));
    assert_eq!(
        github.implementation_status,
        ImplementationStatusV1::Reverted
    );
}
