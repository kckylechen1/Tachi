//! #1002 acceptance criterion 4: the closed disposition vocabulary covers
//! the 7 named 2026-07-13 manual-cleanup failure classes plus the two
//! remaining closed outcomes and the default KEEP — replayed against both
//! synthetic per-class fixtures and 5 named historical issues.
//!
//! Historical-case honesty note: #979/#947/#987/#997 fixtures below are
//! reconstructed from facts verifiable in THIS repo's local `git log` (real
//! merge commits fixing each, found via `git log --all --oneline | grep
//! '#<n>'`). #515 (previously a synthetic scope-collision placeholder — no
//! corroborating evidence at authoring time) was rebuilt this round (R5-1,
//! sol arbitration codex-r8f4e, archived on issue #1002) from real primary
//! sources: `gh issue view 515 --repo kckylechen1/tachi --json ...`
//! (read-only research, not a mutation) plus `git log --all --grep=515`
//! cross-referenced against the linked PRs — see that test's own doc
//! comment for the full evidence chain, including an honestly-surfaced
//! post-close dispute this leaf does not paper over. Per this leaf's own
//! acceptance criteria, these are fixture-level (classifier) replays via
//! `propose_disposition` directly — NOT full-pipeline `build_refinery_packet`
//! runs (see `refinery_ops::mod`'s module doc for which fixtures are which).

use super::super::build_refinery_packet;
use super::super::disposition::{
    propose_disposition, RefinerySignalsV1, RelatedIssueStateV1, RelatedSignalV1,
};
use super::super::fixtures::{gh_issue_json, minimal_evidence, FixtureDocResolver};
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
    propose_disposition(
        &evidence,
        &signals,
        Vec::new(),
        Vec::new(),
        &[],
        CAPTURED_AT,
    )
    .expect("propose_disposition")
}

fn propose_with_labels(
    issue_ref: &str,
    labels: &[&str],
    signals: RefinerySignalsV1,
) -> tachi_params::IssueDispositionProposalV1 {
    let evidence = minimal_evidence(issue_ref, "OPEN", labels);
    propose_disposition(
        &evidence,
        &signals,
        Vec::new(),
        Vec::new(),
        &[],
        CAPTURED_AT,
    )
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

// ─── prose relation-state annotations are advisory, v1 no-live-signal path ──
//
// #1105 landed the live per-relation cross-reference (see
// `refinery_ops::live_signals`); `build_refinery_packet` (used by every test
// below) is now the v1 BACK-COMPAT wrapper — equivalent to calling
// `build_refinery_packet_with_live_signals` with `LiveRelationSignals::default()`,
// i.e. "no live lookup was ever supplied for this target_ref". These tests
// therefore still hold exactly as written: with zero live signals, a prose
// `[state]` annotation is still advisory-only and cannot manufacture a
// closed/blocked/dormant disposition by itself. Live-signal-populated
// end-to-end coverage (a `Duplicate-Of:`/`Supersedes:` target whose live
// state genuinely resolves) lives in `tests::live_relation_signals`.
//
// PR #1191 final-gate note (fix-round, sonnet): the two tests immediately
// below were originally named
// `relation_line_state_annotation_cannot_close_superseded_before_1105` and
// `relation_line_state_annotation_cannot_drive_dormant_before_1105`, and
// asserted the literal contradiction strings a pre-#1105 STOPGAP emitted
// ("unverified related issue state annotation" / "advisory only until
// #1105"). #1105 (this PR) is the real verification mechanism the stopgap
// was always meant to be replaced by, so those exact strings are gone by
// design — `build_refinery_packet_with_live_signals` (`mod.rs`) now folds
// every relation's prose `[state]` against `live_signals.related_states`
// and feeds `classify()` (`disposition.rs`) the LIVE state only, never the
// prose one; a target_ref with no live signal (this back-compat wrapper's
// case) falls back to `Unknown`, and `classify()` is a no-op for `Unknown`.
// Disagreement between prose and live state is still surfaced, just under
// new wording: `"prose [state] annotation for {target} ({prose:?}) disagrees
// with the live-verified state ({live:?})"`. Leader ruling (2026-07-17,
// PR #1191 final gate): this is a legitimate replacement of the stopgap by
// the real mechanism it stood in for, not a frozen-assertion weakening —
// the safety PROPERTY under test (an unverified/prose-only relation-state
// annotation must never by itself close-superseded or drive dormant) is
// re-asserted below against the new mechanism, same fixtures, renamed off
// `_before_1105` to describe what they now check.

/// End-to-end (through `build_refinery_packet`, the exact function the live
/// `refine_issues` action calls — not the `propose()` bypass helpers above):
/// a `Supersedes:` relation line's own `[closed_shipped]` prose annotation,
/// with no live-verified state behind it (v1 back-compat path — see module
/// note above), is degraded to `Unknown` before it ever reaches `classify()`
/// and therefore cannot manufacture CLOSE_SUPERSEDED; the disagreement
/// between the prose annotation and the (unavailable) live state is still
/// surfaced as an advisory contradiction referencing the target.
/// Formerly `relation_line_state_annotation_cannot_close_superseded_before_1105`
/// (renamed — see module note above for why the old assertion string no
/// longer exists and what replaced it).
#[test]
fn relation_line_state_annotation_without_live_verification_cannot_close_superseded() {
    let body = "This work is superseded by the landed replacement.\n\n\
                Supersedes: owner/repo#7001 [closed_shipped]\n"
        .to_string();
    let gh_json = gh_issue_json(
        8090,
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
    assert_eq!(proposal.disposition, DispositionV1::Keep);
    assert!(proposal.contradictions.iter().any(|c| {
        c.description
            .contains("disagrees with the live-verified state")
            && c.description.contains("owner/repo#7001")
    }));
}

/// The same fail-safe applies to `[closed_unshipped]`: with no live state
/// behind the prose annotation, the target degrades to `Unknown` and cannot
/// manufacture DORMANT — only a genuinely live-verified `ClosedUnshipped`
/// (see `tests::live_relation_signals`) may do that.
/// Formerly `relation_line_state_annotation_cannot_drive_dormant_before_1105`
/// (renamed — see module note above for why the old assertion string no
/// longer exists and what replaced it).
#[test]
fn relation_line_state_annotation_without_live_verification_cannot_drive_dormant() {
    let body = "This work is superseded but the replacement did not land.\n\n\
                Supersedes: owner/repo#7002 [closed_unshipped]\n"
        .to_string();
    let gh_json = gh_issue_json(
        8091,
        "Superseded-but-unshipped fixture",
        &body,
        "OPEN",
        &[],
        None,
        CAPTURED_AT,
        &[],
    );
    let resolver = FixtureDocResolver::new();
    let (evidence, proposal) =
        build_refinery_packet("owner/repo", 8091, &gh_json, &resolver, CAPTURED_AT)
            .expect("build_refinery_packet");
    assert_eq!(evidence.relations.len(), 1);
    assert_eq!(proposal.disposition, DispositionV1::Keep);
    assert!(proposal.contradictions.iter().any(|c| {
        c.description
            .contains("disagrees with the live-verified state")
            && c.description.contains("owner/repo#7002")
    }));
}

#[test]
fn relation_line_state_annotation_cannot_drive_blocked_before_1105() {
    let body = "Dependency state is only asserted in prose.\n\n\
                Depends-On: owner/repo#7004 [open]\n"
        .to_string();
    let gh_json = gh_issue_json(
        8093,
        "Unverified dependency fixture",
        &body,
        "OPEN",
        &[],
        None,
        CAPTURED_AT,
        &[],
    );
    let resolver = FixtureDocResolver::new();
    let (evidence, proposal) =
        build_refinery_packet("owner/repo", 8093, &gh_json, &resolver, CAPTURED_AT)
            .expect("build_refinery_packet");
    assert_eq!(
        evidence.relations.len(),
        1,
        "relation evidence is preserved"
    );
    assert_eq!(proposal.disposition, DispositionV1::Keep);
    assert!(proposal
        .contradictions
        .iter()
        .any(|c| c.description.contains("owner/repo#7004")));
}

/// Absent annotation -> `Unknown`, and `classify()`'s no-op for `Unknown`
/// means a bare `Supersedes:` line (no state known) does not force any
/// closed/dormant disposition by itself.
#[test]
fn relation_line_without_state_annotation_is_unknown_and_is_a_classify_no_op() {
    let body = "This work is related to prior art.\n\nSupersedes: owner/repo#7003\n".to_string();
    let gh_json = gh_issue_json(
        8092,
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

/// R5-1 (sol arbitration, codex-r8f4e, archived on issue #1002): the prior
/// round's #515 fixture was a synthetic scope-collision placeholder with no
/// corroborating evidence — sol ruled that doesn't satisfy AC4's "real
/// historical evidence basis" bar the other four historical cases meet.
/// This is a real fact-based rebuild from primary sources fetched
/// 2026-07-14 via `gh issue view 515 --repo kckylechen1/tachi --json
/// number,title,state,body,labels,createdAt,updatedAt,closedAt,comments,url`
/// (read-only research, not a mutation) and `git log --all --grep=515`
/// cross-referenced against the linked PRs:
///
/// - Issue: <https://github.com/kckylechen1/tachi/issues/515> — "tachi mcp
///   add: one-command MCP registration to converge per-agent MCP config
///   into Tachi". Opened 2026-07-05, state=CLOSED, closedAt
///   2026-07-11T14:04:25Z.
/// - Shipping commits (real, in this repo's `git log --all --oneline`):
///   `55e07fc3` "Add safe MCP registration front door (#766)" (merged
///   2026-07-07) and `ba24a9c9` "Reserve seat policy fields for MCP add
///   (#781)" (merged 2026-07-07).
/// - The issue's own final CLOSING comment (2026-07-11T14:04:19Z, verbatim):
///   "已由 #766 的安全 MCP 注册入口完成，并已包含在 v1.9.0。后续 MCP surface
///   的演进统一在 #745 / #757 跟踪；本叶关闭。" ("Completed by #766's secure
///   MCP registration entry point, included in v1.9.0. Follow-up MCP
///   surface evolution tracked under #745/#757; this leaf closed.") — this
///   is the operative CLOSE_FIXED rationale this fixture models, backed by
///   the real `shipped_evidence` commit.
///
/// HONEST CAVEAT (not hidden — this is exactly the "doesn't cleanly match"
/// case the dispatch instructions asked to be reported, not forced): a
/// LATER comment on the SAME issue, ~49 minutes AFTER the close
/// (2026-07-11T14:53:47Z), is a leader-verified snapshot titled "不可关"
/// ("must not be closed") that lists 4 concrete residual gaps against the
/// issue's own acceptance criteria (discovered-tool-name printing not
/// implemented; the native per-agent context7 config was never actually
/// converged/removed; the secret-header allowlist stayed hardcoded at 3
/// entries; `--key <value>` argv exposure was never resolved). The issue
/// was never reopened after that comment — its real, final, still-current
/// GitHub state is CLOSED. A maximally rigorous refinery run at that exact
/// moment could arguably have produced DECISION_REQUIRED (the closure
/// itself was actively disputed in-thread) rather than a confident
/// CLOSE_FIXED. This fixture models the historically-operative outcome
/// (closed, backed by a real shipped commit) while surfacing — not
/// concealing — that a real, dated, in-thread dispute about that very
/// closure exists. `related` intentionally does not encode the #745/#757
/// handoff as a `Related:` signal: that would need this leaf's live
/// per-relation cross-reference (canon doc §4.1 step 4), which is an
/// explicitly deferred follow-up slice, not something to fake here.
#[test]
fn historical_515_mcp_registration_front_door_shipped_via_766_is_close_fixed() {
    let signals = RefinerySignalsV1 {
        shipped_evidence: Some(repo_revision("55e07fc3")),
        ..Default::default()
    };
    let proposal = propose("kckylechen1/tachi#515", signals);
    assert_eq!(proposal.disposition, DispositionV1::CloseFixed);
}
