//! Issue Refinery (#1002): a manually triggered, read-only, proposal-only
//! semantic refinement of one GitHub issue. Orchestrates the existing GitHub
//! read facility (`tachi_gh(action='issue_read')`) and local git state into
//! typed evidence/disposition packets — see
//! `docs/engineering/architecture/issue-refinery-memory-lanes.md` §4/§5 for
//! the frozen design authority.
//!
//! Safety boundary (canon doc §4.3), unconditionally true of everything in
//! this module: never closes/reopens an issue, never edits an issue body or
//! canonical doc, never establishes a precedent, never treats an issue
//! attachment/external download as evidence, never invents an execution
//! backend, and is only reachable on demand (no cron/resident scheduling).
//! `IssueDispositionProposalV1::preview_only` is unconditionally `true` in
//! this leaf.
//!
//! [`build_refinery_packet`] is the pure core (parse → resolve anchors via
//! an injected [`doc_resolver::DocRefResolver`] → compile evidence →
//! propose disposition) used by BOTH the live `handle_refine_issues` action
//! (via `doc_resolver::GitRefResolver`, real git calls) and every fixture
//! test in `refinery_ops::tests` (via a fixture resolver, zero I/O) — the
//! same production code path is what the acceptance tests exercise, not a
//! parallel re-implementation.
//!
//! Known scope gap (tracked here, not hidden): related-issue state (canon
//! doc §4.1 input-order step 4) is wired ONLY through each relation line's
//! own optional `[state]` annotation (see `parse::parse_related_state_suffix`)
//! — a bounded, zero-extra-IO signal an issue author/dispatch tool can set
//! explicitly. It is NOT yet a live per-relation GitHub cross-reference
//! (fetching the target issue's real state/labels); that fuller version,
//! and scope-collision detection / shipped-evidence cross-check, remain a
//! follow-up slice. Router protection and grounding are fully live (derived
//! from the fetched issue's own labels / resolved Spec-Ref anchors).

mod compiler;
mod disposition;
mod doc_resolver;
mod parse;

#[cfg(test)]
mod fixtures;
#[cfg(test)]
mod tests;

use crate::gh_ops::handle_tachi_gh;
use crate::task_lifecycle::parse_issue_ref;
use crate::tool_params::{
    CanonicalDocRefV1, GroundingStatusV1, IssueDispositionProposalV1, IssueEvidenceV1,
    IssueRelationV1, RepoRevisionV1, SourceSpanV1, TachiGhParams, TachiTaskParams,
};
use crate::MemoryServer;
use doc_resolver::{DocRefResolver, DocResolution, GitRefResolver};

const TRUSTED_REF: &str = "origin/main";

pub(crate) async fn handle_refine_issues(
    server: &MemoryServer,
    params: &TachiTaskParams,
) -> Result<String, String> {
    let raw_ref = params
        .issue_ref
        .clone()
        .or_else(|| {
            let repo = params.repo.clone()?;
            let number = params.number?;
            Some(format!("{repo}#{number}"))
        })
        .ok_or_else(|| {
            "issue_ref (or repo+number) is required for action='refine_issues'".to_string()
        })?;
    let target = parse_issue_ref(&raw_ref, params.repo.as_deref())
        .ok_or_else(|| format!("could not parse issue_ref '{raw_ref}'"))?;

    let raw = handle_tachi_gh(
        server,
        TachiGhParams {
            action: "issue_read".to_string(),
            repo: Some(target.repo.clone()),
            number: Some(target.number),
            dry_run: Some(true),
            ..Default::default()
        },
    )
    .await?;
    let value: serde_json::Value =
        serde_json::from_str(&raw).map_err(|e| format!("parse issue_read: {e}"))?;
    let result = value
        .get("result")
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));

    let repo_root = std::env::current_dir().map_err(|e| format!("current_dir: {e}"))?;
    let resolver = GitRefResolver { repo_root };
    let captured_at = chrono::Utc::now().to_rfc3339();
    let (evidence, proposal) = build_refinery_packet(
        &target.repo,
        target.number,
        &result,
        &resolver,
        &captured_at,
    )?;

    serde_json::to_string(&serde_json::json!({
        "tool": "tachi_task_refine_issues",
        "issue_ref": evidence.issue_ref,
        "evidence": evidence,
        "proposal": proposal,
    }))
    .map_err(|e| format!("serialize refine_issues result: {e}"))
}

/// Pure pipeline: `gh issue view --json ...` result → typed evidence +
/// disposition proposal. Takes an injected [`DocRefResolver`] so tests never
/// shell real git/GitHub (#1002 acceptance criterion 7).
pub(crate) fn build_refinery_packet(
    repo: &str,
    number: u64,
    gh_issue_result: &serde_json::Value,
    resolver: &dyn DocRefResolver,
    captured_at: &str,
) -> Result<(IssueEvidenceV1, IssueDispositionProposalV1), String> {
    let snapshot = parse::parse_issue_snapshot_from_gh_json(repo, number, gh_issue_result);

    let mut source_text = snapshot.body.clone();
    for c in &snapshot.selected_comment_revisions {
        source_text.push('\n');
        source_text.push_str(&c.body);
    }
    let spec_refs = parse::parse_spec_ref_lines(&source_text);
    let relation_lines = parse::parse_relation_lines(&source_text);

    let mut linked_specs: Vec<CanonicalDocRefV1> = Vec::new();
    let mut doc_anchors_by_span: Vec<(SourceSpanV1, CanonicalDocRefV1)> = Vec::new();
    let mut grounding_status = GroundingStatusV1::Grounded;
    let mut missing_anchor_reasons: Vec<String> = Vec::new();
    for spec_ref in &spec_refs {
        match resolver.resolve(
            &spec_ref.repo,
            &spec_ref.path,
            &spec_ref.commit_sha,
            &spec_ref.blob_sha,
            &spec_ref.section,
            TRUSTED_REF,
        ) {
            DocResolution::Resolved(doc_ref) => {
                doc_anchors_by_span.push((spec_ref.span, doc_ref.clone()));
                linked_specs.push(doc_ref);
            }
            DocResolution::Unresolved { reason } => {
                // Any requested anchor that fails to resolve degrades the
                // whole packet (canon doc §4.1) — specs that DID resolve are
                // still reported, but the packet as a whole is not "high
                // confidence". The reason is not discarded: it becomes a
                // real contradiction on the proposal (see
                // `disposition::propose_disposition`), not silently dropped.
                grounding_status = GroundingStatusV1::MissingAnchor;
                missing_anchor_reasons.push(reason);
            }
        }
    }

    let issue_ref_anchors_by_span: Vec<(SourceSpanV1, String)> = relation_lines
        .iter()
        .map(|r| (r.span, r.target_ref.clone()))
        .collect();

    // Real (non-live-lookup) related-issue-state signals: each relation
    // line's own optional `[state]` annotation (see
    // `parse::parse_related_state_suffix`), defaulting to `Unknown` when
    // absent — `classify` treats `Unknown` as a no-op (fail open).
    let related_signals: Vec<disposition::RelatedSignalV1> = relation_lines
        .iter()
        .map(|r| disposition::RelatedSignalV1 {
            target_ref: r.target_ref.clone(),
            kind: r.kind,
            state: r.state,
        })
        .collect();

    let relations: Vec<IssueRelationV1> = relation_lines
        .into_iter()
        .map(|r| IssueRelationV1 {
            kind: r.kind,
            target_ref: r.target_ref,
            evidence_refs: Vec::new(),
        })
        .collect();

    let evidence = compiler::build_issue_evidence(
        snapshot,
        linked_specs.clone(),
        relations,
        grounding_status,
        &doc_anchors_by_span,
        &issue_ref_anchors_by_span,
    );

    let signals = disposition::RefinerySignalsV1 {
        related: related_signals,
        ..Default::default()
    };
    let repo_revisions: Vec<RepoRevisionV1> = Vec::new();
    let doc_revisions: Vec<CanonicalDocRefV1> = linked_specs;
    let proposal = disposition::propose_disposition(
        &evidence,
        &signals,
        repo_revisions,
        doc_revisions,
        &missing_anchor_reasons,
        captured_at,
    )?;

    Ok((evidence, proposal))
}
