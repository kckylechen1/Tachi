//! #1002 acceptance criterion 4: the closed disposition vocabulary covers
//! the 7 named 2026-07-13 manual-cleanup failure classes plus the two
//! remaining closed outcomes and the default KEEP — replayed against both
//! synthetic per-class fixtures and 5 named historical issues.
//!
//! Historical-case honesty note: #979/#947/#987/#997 fixtures below are
//! reconstructed from facts verifiable in THIS repo's local `git log`
//! (real merge commits fixing each, found via `git log --all --oneline |
//! grep '#<n>'` at authoring time — no live GitHub call). #515 has no local
//! git-log corroboration in this repo's history, so it is represented as a
//! clearly-labeled SYNTHETIC placeholder for the scope-collision failure
//! class, not a factual reconstruction — flagged again in the final
//! delivery report, not just here.

use super::super::disposition::{
    propose_disposition, RefinerySignalsV1, RelatedIssueStateV1, RelatedSignalV1,
};
use super::super::fixtures::{gh_issue_json, minimal_evidence, FixtureDocResolver};
use super::super::build_refinery_packet;
use tachi_params::{DispositionV1, IssueRelationKindV1, RepoRevisionV1};

const CAPTURED_AT: &str = "2026-07-13T00:00:00Z";

fn repo_revision(commit_sha: &str) -> RepoRevisionV1 {
    RepoRevisionV1 {
        repo: "kckylechen1/tachi".to_string(),
        git_ref: "main".to_string(),
        commit_sha: commit_sha.to_string(),
        verified_at: CAPTURED_AT.to_string(),
    }
}

fn propose(
    issue_ref: &str,
    signals: RefinerySignalsV1,
) -> tachi_params::IssueDispositionProposalV1 {
    let evidence = minimal_evidence(issue_ref, "OPEN", &[]);
    propose_disposition(&evidence, &signals, Vec::new(), Vec::new(), &[], CAPTURED_AT)
        .expect("propose_disposition")
}

fn propose_with_labels(
    issue_ref: &str,
    labels: &[&str],
    signals: RefinerySignalsV1,
) -> tachi_params::IssueDispositionProposalV1 {
    let evidence = minimal_evidence(issue_ref, "OPEN", labels);
    propose_disposition(&evidence, &signals, Vec::new(), Vec::new(), &[], CAPTURED_AT)
        .expect("propose_disposition")
}

#[test]
fn happy_path_grounded_no_signals_is_keep() {
    let proposal = propose("owner/repo#8001", RefinerySignalsV1::default());
    assert_eq!(proposal.disposition, DispositionV1::Keep);
}

#[test]
fn failure_class_stale_body_is_historical() {
    let signals = RefinerySignalsV1 {
        stale_body_signal: true,
        ..Default::default()
    };
    let proposal = propose("owner/repo#8002", signals);
    assert_eq!(proposal.disposition, DispositionV1::Historical);
}

#[test]
fn failure_class_child_state_drift_is_narrow() {
    let signals = RefinerySignalsV1 {
        related: vec![
            RelatedSignalV1 {
                target_ref: "owner/repo#8010".to_string(),
                kind: IssueRelationKindV1::ParentOf,
                state: RelatedIssueStateV1::ClosedShipped,
            },
            RelatedSignalV1 {
                target_ref: "owner/repo#8011".to_string(),
                kind: IssueRelationKindV1::ParentOf,
                state: RelatedIssueStateV1::Open,
            },
        ],
        ..Default::default()
    };
    let proposal = propose("owner/repo#8003", signals);
    assert_eq!(proposal.disposition, DispositionV1::Narrow);
}

#[test]
fn failure_class_scope_collision_is_merge_candidate() {
    let signals = RefinerySignalsV1 {
        scope_collisions: vec!["owner/repo#8020".to_string(), "owner/repo#8021".to_string()],
        ..Default::default()
    };
    let proposal = propose("owner/repo#8004", signals);
    assert_eq!(proposal.disposition, DispositionV1::MergeCandidate);
}

#[test]
fn failure_class_superseded_but_not_shipped_is_dormant_with_contradiction() {
    let signals = RefinerySignalsV1 {
        related: vec![RelatedSignalV1 {
            target_ref: "owner/repo#8030".to_string(),
            kind: IssueRelationKindV1::Supersedes,
            state: RelatedIssueStateV1::Open,
        }],
        ..Default::default()
    };
    let proposal = propose("owner/repo#8005", signals);
    assert_eq!(proposal.disposition, DispositionV1::Dormant);
    assert!(
        !proposal.contradictions.is_empty(),
        "a not-yet-shipped supersession must surface a contradiction, not silently DORMANT"
    );
}

#[test]
fn failure_class_protected_router_wins_over_other_signals() {
    let signals = RefinerySignalsV1 {
        scope_collisions: vec!["owner/repo#8041".to_string()],
        ..Default::default()
    };
    let proposal = propose_with_labels("owner/repo#8006", &["router"], signals);
    assert_eq!(proposal.disposition, DispositionV1::Router);
}

#[test]
fn failure_class_missing_prerequisite_is_blocked() {
    let signals = RefinerySignalsV1 {
        related: vec![RelatedSignalV1 {
            target_ref: "owner/repo#8050".to_string(),
            kind: IssueRelationKindV1::DependsOn,
            state: RelatedIssueStateV1::Open,
        }],
        ..Default::default()
    };
    let proposal = propose("owner/repo#8007", signals);
    assert_eq!(proposal.disposition, DispositionV1::Blocked);
}

#[test]
fn failure_class_incomplete_dispatch_packet_is_decision_required() {
    let signals = RefinerySignalsV1 {
        dispatch_packet_complete: Some(false),
        ..Default::default()
    };
    let proposal = propose("owner/repo#8008", signals);
    assert_eq!(proposal.disposition, DispositionV1::DecisionRequired);
}

#[test]
fn close_fixed_when_shipped_evidence_present() {
    let signals = RefinerySignalsV1 {
        shipped_evidence: Some(repo_revision("eddd65d1")),
        ..Default::default()
    };
    let proposal = propose("owner/repo#8009", signals);
    assert_eq!(proposal.disposition, DispositionV1::CloseFixed);
}

#[test]
fn close_superseded_when_superseding_work_already_shipped() {
    let signals = RefinerySignalsV1 {
        related: vec![RelatedSignalV1 {
            target_ref: "owner/repo#8060".to_string(),
            kind: IssueRelationKindV1::Supersedes,
            state: RelatedIssueStateV1::ClosedShipped,
        }],
        ..Default::default()
    };
    let proposal = propose("owner/repo#8012", signals);
    assert_eq!(proposal.disposition, DispositionV1::CloseSuperseded);
}

#[test]
fn all_ten_dispositions_are_reachable_by_at_least_one_fixture_in_this_file() {
    // Completeness check for the closed vocabulary — every variant must be
    // producible, not just declared.
    use std::collections::HashSet;
    let reachable: HashSet<DispositionV1> = [
        propose("owner/repo#9101", RefinerySignalsV1::default()).disposition,
        propose(
            "owner/repo#9102",
            RefinerySignalsV1 {
                stale_body_signal: true,
                ..Default::default()
            },
        )
        .disposition,
        propose(
            "owner/repo#9103",
            RefinerySignalsV1 {
                related: vec![
                    RelatedSignalV1 {
                        target_ref: "owner/repo#1".to_string(),
                        kind: IssueRelationKindV1::ParentOf,
                        state: RelatedIssueStateV1::ClosedShipped,
                    },
                    RelatedSignalV1 {
                        target_ref: "owner/repo#2".to_string(),
                        kind: IssueRelationKindV1::ParentOf,
                        state: RelatedIssueStateV1::Open,
                    },
                ],
                ..Default::default()
            },
        )
        .disposition,
        propose(
            "owner/repo#9104",
            RefinerySignalsV1 {
                scope_collisions: vec!["owner/repo#3".to_string()],
                ..Default::default()
            },
        )
        .disposition,
        propose(
            "owner/repo#9105",
            RefinerySignalsV1 {
                related: vec![RelatedSignalV1 {
                    target_ref: "owner/repo#4".to_string(),
                    kind: IssueRelationKindV1::Supersedes,
                    state: RelatedIssueStateV1::Open,
                }],
                ..Default::default()
            },
        )
        .disposition,
        propose_with_labels("owner/repo#9106", &["router"], RefinerySignalsV1::default())
            .disposition,
        propose(
            "owner/repo#9107",
            RefinerySignalsV1 {
                related: vec![RelatedSignalV1 {
                    target_ref: "owner/repo#5".to_string(),
                    kind: IssueRelationKindV1::Blocks,
                    state: RelatedIssueStateV1::Open,
                }],
                ..Default::default()
            },
        )
        .disposition,
        propose(
            "owner/repo#9108",
            RefinerySignalsV1 {
                dispatch_packet_complete: Some(false),
                ..Default::default()
            },
        )
        .disposition,
        propose(
            "owner/repo#9109",
            RefinerySignalsV1 {
                shipped_evidence: Some(repo_revision("sha1")),
                ..Default::default()
            },
        )
        .disposition,
        propose(
            "owner/repo#9110",
            RefinerySignalsV1 {
                related: vec![RelatedSignalV1 {
                    target_ref: "owner/repo#6".to_string(),
                    kind: IssueRelationKindV1::Supersedes,
                    state: RelatedIssueStateV1::ClosedShipped,
                }],
                ..Default::default()
            },
        )
        .disposition,
    ]
    .into_iter()
    .collect();
    for &d in DispositionV1::ALL {
        assert!(
            reachable.contains(&d),
            "disposition {d} is declared in the closed vocabulary but unreachable from any fixture in this test suite"
        );
    }
}

// ─── real production wiring for `RelatedIssueStateV1` (BUG-2 regression) ───

/// End-to-end (through `build_refinery_packet`, the exact function the live
/// `refine_issues` action calls — not the `propose()` bypass helpers above):
/// a `Supersedes:` relation line's own `[closed_shipped]` annotation (see
/// `parse::parse_related_state_suffix`) really is parsed and really does
/// drive `classify()` to CLOSE_SUPERSEDED. This is the production
/// construction site for `RelatedIssueStateV1::ClosedShipped` (a build-seat
/// RED previously flagged it as dead code because only the `propose()`
/// bypass helpers constructed it directly).
#[test]
fn relation_line_state_annotation_drives_close_superseded_through_the_real_pipeline() {
    let body = "This work is superseded by the landed replacement.\n\n\
                Supersedes: owner/repo#7001 [closed_shipped]\n"
        .to_string();
    let gh_json = gh_issue_json(
        "Superseded-and-shipped fixture",
        &body,
        "OPEN",
        &[],
        None,
        CAPTURED_AT,
        &[],
    );
    let resolver = FixtureDocResolver::new();
    let (evidence, proposal) =
        build_refinery_packet("owner/repo", 8090, &gh_json, &resolver, CAPTURED_AT)
            .expect("build_refinery_packet");
    assert_eq!(evidence.relations.len(), 1);
    assert_eq!(proposal.disposition, DispositionV1::CloseSuperseded);
}

/// Same pipeline, `[closed_unshipped]` annotation -> DORMANT with a
/// contradiction — the production construction site for
/// `RelatedIssueStateV1::ClosedUnshipped`.
#[test]
fn relation_line_state_annotation_drives_dormant_through_the_real_pipeline() {
    let body = "This work is superseded but the replacement did not land.\n\n\
                Supersedes: owner/repo#7002 [closed_unshipped]\n"
        .to_string();
    let gh_json = gh_issue_json(
        "Superseded-but-unshipped fixture",
        &body,
        "OPEN",
        &[],
        None,
        CAPTURED_AT,
        &[],
    );
    let resolver = FixtureDocResolver::new();
    let (_evidence, proposal) =
        build_refinery_packet("owner/repo", 8091, &gh_json, &resolver, CAPTURED_AT)
            .expect("build_refinery_packet");
    assert_eq!(proposal.disposition, DispositionV1::Dormant);
    assert!(!proposal.contradictions.is_empty());
}

/// Absent annotation -> `Unknown`, and `classify()`'s no-op for `Unknown`
/// means a bare `Supersedes:` line (no state known) does not force any
/// closed/dormant disposition by itself.
#[test]
fn relation_line_without_state_annotation_is_unknown_and_is_a_classify_no_op() {
    let body = "This work is related to prior art.\n\nSupersedes: owner/repo#7003\n".to_string();
    let gh_json = gh_issue_json(
        "No-annotation fixture",
        &body,
        "OPEN",
        &[],
        None,
        CAPTURED_AT,
        &[],
    );
    let resolver = FixtureDocResolver::new();
    let (_evidence, proposal) =
        build_refinery_packet("owner/repo", 8092, &gh_json, &resolver, CAPTURED_AT)
            .expect("build_refinery_packet");
    assert_eq!(proposal.disposition, DispositionV1::Keep);
}

// ─── 5 historical-case replays (2026-07-13 manual cleanup) ─────────────────

#[test]
fn historical_979_vault_daemon_enxio_masking_already_fixed_is_close_fixed() {
    // Real: fixed by commit eddd65d1 ("fix(vault): allow vault unlock/lock
    // under standard profile; stop masking daemon error behind FIFO ENXIO
    // (#979) (#980)"), verifiable in this repo's `git log --all --oneline`.
    let signals = RefinerySignalsV1 {
        shipped_evidence: Some(repo_revision("eddd65d1")),
        ..Default::default()
    };
    let proposal = propose("kckylechen1/tachi#979", signals);
    assert_eq!(proposal.disposition, DispositionV1::CloseFixed);
}

#[test]
fn historical_947_mcp_ssrf_pin_already_fixed_is_close_fixed() {
    // Real: fixed by commit 089797ae ("fix(mcp): per-server allow_proxy
    // opt-in; default no_proxy + SSRF pin for remote MCP clients (#947)
    // (#981)"), verifiable in this repo's `git log --all --oneline`.
    let signals = RefinerySignalsV1 {
        shipped_evidence: Some(repo_revision("089797ae")),
        ..Default::default()
    };
    let proposal = propose("kckylechen1/tachi#947", signals);
    assert_eq!(proposal.disposition, DispositionV1::CloseFixed);
}

#[test]
fn historical_987_deflake_already_fixed_is_close_fixed() {
    // Real: fixed by commit 71460133 ("fix(#987,#997): deflake stdio_proxy /
    // component_check / subprocess-reap families (#1006)").
    let signals = RefinerySignalsV1 {
        shipped_evidence: Some(repo_revision("71460133")),
        ..Default::default()
    };
    let proposal = propose("kckylechen1/tachi#987", signals);
    assert_eq!(proposal.disposition, DispositionV1::CloseFixed);
}

#[test]
fn historical_997_deflake_already_fixed_is_close_fixed() {
    // Real: same fixing commit as #987 (71460133), both named in the same
    // PR title.
    let signals = RefinerySignalsV1 {
        shipped_evidence: Some(repo_revision("71460133")),
        ..Default::default()
    };
    let proposal = propose("kckylechen1/tachi#997", signals);
    assert_eq!(proposal.disposition, DispositionV1::CloseFixed);
}

#[test]
fn historical_515_synthetic_scope_collision_placeholder_is_merge_candidate() {
    // SYNTHETIC: no corroborating commit for #515 was found in this repo's
    // local `git log` at authoring time. Represented here as a clearly
    // labeled placeholder for the scope-collision failure class, not a
    // factual reconstruction of the real issue #515.
    let signals = RefinerySignalsV1 {
        scope_collisions: vec![
            "kckylechen1/tachi#9201".to_string(),
            "kckylechen1/tachi#9202".to_string(),
        ],
        ..Default::default()
    };
    let proposal = propose("kckylechen1/tachi#515", signals);
    assert_eq!(proposal.disposition, DispositionV1::MergeCandidate);
}
