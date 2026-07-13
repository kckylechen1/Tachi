//! Proposal reasoner (canon doc §2.1 "Proposal reasoner" seat): a
//! deterministic, ordered rule engine mapping structured `RefinerySignalsV1`
//! onto the closed 10-word `DispositionV1` vocabulary (canon doc §4.2). This
//! is intentionally NOT a free-text classifier (canon doc §10 explicitly
//! abandons "free-text substring verdict classifiers") — every signal here
//! is expected to come from a structured parse (`refinery_ops::parse`) or an
//! explicit cross-reference lookup, never a keyword search over prose.
//!
//! Precedence is fixed and total (first match wins), covering: the 2026-07-13
//! manual-cleanup failure classes the canon doc names (stale body,
//! child-state drift, scope collision, superseded-but-not-shipped, protected
//! router, missing prerequisite, incomplete dispatch packet), plus the two
//! remaining closed-disposition outcomes (`CLOSE_FIXED`, `CLOSE_SUPERSEDED`)
//! and the default `KEEP`. `missing_anchor` grounding always wins first —
//! canon doc §4.1: "neither reasoning nor the number of advisory hits may
//! upgrade it to high confidence".

use tachi_params::{
    CanonicalDocRefV1, ContradictionV1, DispositionV1, EvidenceRefV1, EvidenceRelationV1,
    GroundingStatusV1, ImmutableRevisionV1, IssueDispositionProposalV1, IssueEvidenceV1,
    IssueRelationKindV1, RepoRevisionV1, SourceKindV1,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum IssueStateV1 {
    Open,
    Closed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RelatedIssueStateV1 {
    Open,
    ClosedShipped,
    ClosedUnshipped,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RelatedSignalV1 {
    pub(crate) target_ref: String,
    pub(crate) kind: IssueRelationKindV1,
    pub(crate) state: RelatedIssueStateV1,
}

/// Structured inputs to the disposition classifier. Every field here is
/// meant to come from a deterministic parse or an explicit cross-reference
/// lookup (never a free-text keyword search) — see module docs.
#[derive(Debug, Clone, Default)]
pub(crate) struct RefinerySignalsV1 {
    pub(crate) issue_state: Option<IssueStateV1>,
    pub(crate) is_protected_router: bool,
    pub(crate) related: Vec<RelatedSignalV1>,
    pub(crate) scope_collisions: Vec<String>,
    pub(crate) shipped_evidence: Option<RepoRevisionV1>,
    pub(crate) dispatch_packet_complete: Option<bool>,
    pub(crate) stale_body_signal: bool,
}

/// Conservative signal derivation using ONLY the already-compiled evidence
/// (issue state + labels) — no additional live GH/git cross-reference
/// lookups. This is what the live `refine_issues` orchestration wires today;
/// the fuller signal set (child-state drift via related-issue state lookup,
/// scope collision detection, shipped-evidence cross-check) is implemented
/// and fixture-tested in `propose_disposition` below but requires extra
/// live calls per relation that this leaf does not yet make (see
/// `refinery_ops::handle_refine_issues` doc comment for the tracked gap).
pub(crate) fn signals_from_evidence_only(evidence: &IssueEvidenceV1) -> RefinerySignalsV1 {
    let is_protected_router = evidence.issue_snapshot.labels.iter().any(|label| {
        let label = label.to_ascii_lowercase();
        label.contains("router") || label.contains("umbrella") || label.contains("no-close")
    });
    let issue_state = Some(
        if evidence.issue_snapshot.state.eq_ignore_ascii_case("closed") {
            IssueStateV1::Closed
        } else {
            IssueStateV1::Open
        },
    );
    RefinerySignalsV1 {
        issue_state,
        is_protected_router,
        ..Default::default()
    }
}

fn classify(
    evidence: &IssueEvidenceV1,
    signals: &RefinerySignalsV1,
) -> (DispositionV1, Vec<ContradictionV1>) {
    let contradictions = Vec::new();

    if evidence.grounding_status == GroundingStatusV1::MissingAnchor {
        return (DispositionV1::DecisionRequired, contradictions);
    }
    if signals.is_protected_router {
        return (DispositionV1::Router, contradictions);
    }
    if signals.dispatch_packet_complete == Some(false) {
        return (DispositionV1::DecisionRequired, contradictions);
    }

    for rel in &signals.related {
        if rel.kind == IssueRelationKindV1::Supersedes {
            match rel.state {
                RelatedIssueStateV1::ClosedShipped => {
                    return (DispositionV1::CloseSuperseded, contradictions);
                }
                RelatedIssueStateV1::Open | RelatedIssueStateV1::ClosedUnshipped => {
                    let mut contradictions = contradictions;
                    contradictions.push(ContradictionV1 {
                        description: format!(
                            "issue declares supersedes {} but the superseding work is not shipped yet",
                            rel.target_ref
                        ),
                        evidence_refs: Vec::new(),
                    });
                    return (DispositionV1::Dormant, contradictions);
                }
                RelatedIssueStateV1::Unknown => {}
            }
        }
    }

    for rel in &signals.related {
        if matches!(
            rel.kind,
            IssueRelationKindV1::Blocks | IssueRelationKindV1::DependsOn
        ) && matches!(rel.state, RelatedIssueStateV1::Open)
        {
            return (DispositionV1::Blocked, contradictions);
        }
    }

    let parent_of: Vec<&RelatedSignalV1> = signals
        .related
        .iter()
        .filter(|r| r.kind == IssueRelationKindV1::ParentOf)
        .collect();
    if !parent_of.is_empty() {
        let has_open = parent_of
            .iter()
            .any(|r| r.state == RelatedIssueStateV1::Open);
        let has_closed = parent_of.iter().any(|r| {
            matches!(
                r.state,
                RelatedIssueStateV1::ClosedShipped | RelatedIssueStateV1::ClosedUnshipped
            )
        });
        if has_open && has_closed {
            return (DispositionV1::Narrow, contradictions);
        }
    }

    if !signals.scope_collisions.is_empty() {
        return (DispositionV1::MergeCandidate, contradictions);
    }

    if signals.shipped_evidence.is_some() {
        return (DispositionV1::CloseFixed, contradictions);
    }

    if signals.stale_body_signal {
        return (DispositionV1::Historical, contradictions);
    }

    (DispositionV1::Keep, contradictions)
}

/// Build the full `IssueDispositionProposalV1`: classify, then compute the
/// deterministic `packet_id`/`proposal_hash`/`source_bundle_hash` from the
/// pinned evidence. `preview_only` is unconditionally `true` in this leaf —
/// #1002's delivery scope is "proposal-only apply boundary" (canon doc §10
/// item 1); there is no live path in this leaf that supplies a real engine
/// identity receipt.
pub(crate) fn propose_disposition(
    evidence: &IssueEvidenceV1,
    signals: &RefinerySignalsV1,
    based_on_repo_revisions: Vec<RepoRevisionV1>,
    based_on_doc_revisions: Vec<CanonicalDocRefV1>,
    captured_at: &str,
) -> Result<IssueDispositionProposalV1, String> {
    let (disposition, contradictions) = classify(evidence, signals);

    let mut evidence_refs: Vec<EvidenceRefV1> = vec![EvidenceRefV1 {
        relation: EvidenceRelationV1::DerivedFrom,
        target_kind: SourceKindV1::Issue,
        target_ref: evidence.issue_ref.clone(),
        immutable_revision: ImmutableRevisionV1::IssueSnapshotHash {
            value: evidence.issue_snapshot_hash.clone(),
        },
        section_or_span: None,
        captured_at: captured_at.to_string(),
    }];
    for relation in &evidence.relations {
        evidence_refs.extend(relation.evidence_refs.clone());
    }

    let proposed_labels: Vec<String> = Vec::new();
    let proposed_doc_deltas: Vec<tachi_params::DocDeltaProposalV1> = Vec::new();
    let proposed_comment: Option<String> = None;

    let hash_basis = serde_json::json!({
        "issue_ref": evidence.issue_ref,
        "based_on_issue_snapshot_hash": evidence.issue_snapshot_hash,
        "based_on_repo_revisions": based_on_repo_revisions,
        "based_on_doc_revisions": based_on_doc_revisions,
        "disposition": disposition,
        "contradictions": contradictions,
        "proposed_labels": proposed_labels,
        "proposed_doc_deltas": proposed_doc_deltas,
    });
    let proposal_hash = tachi_params::canonical_json_sha256(&hash_basis)?;

    let source_bundle_basis = serde_json::json!({
        "issue_snapshot_hash": evidence.issue_snapshot_hash,
        "doc_blob_shas": based_on_doc_revisions
            .iter()
            .map(|d| d.blob_sha.clone())
            .collect::<Vec<_>>(),
        "repo_commit_shas": based_on_repo_revisions
            .iter()
            .map(|r| r.commit_sha.clone())
            .collect::<Vec<_>>(),
    });
    let source_bundle_hash = tachi_params::canonical_json_sha256(&source_bundle_basis)?;

    let packet_id = format!(
        "issue-refinery/{}/{}",
        evidence.issue_ref, evidence.issue_snapshot_hash
    );

    Ok(IssueDispositionProposalV1 {
        packet_id,
        proposal_hash,
        source_bundle_hash,
        issue_ref: evidence.issue_ref.clone(),
        based_on_issue_snapshot_hash: evidence.issue_snapshot_hash.clone(),
        based_on_repo_revisions,
        based_on_doc_revisions,
        grounding_status: evidence.grounding_status,
        disposition,
        evidence_refs,
        contradictions,
        proposed_comment,
        proposed_labels,
        proposed_doc_deltas,
        preview_only: true,
        engine_receipt: None,
    })
}
