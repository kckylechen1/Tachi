//! Pure adapt: [`CaseCorpusBundle`] → evidence + Pending `LessonCandidateV1`.
//!
//! No I/O. Never establishes a precedent. Closed/merged produces OUTCOME
//! evidence only; reopen/revert APPENDs Contradicts without rewriting prior
//! refs. Idempotent on equal bundles.
//!
//! `source_revision` (and thus `candidate_id`) binds the issue/PR snapshot
//! hashes **and** the ordered provenance event chain, so a pure revert with
//! unchanged snapshots still yields a distinct candidate.

use tachi_params::{
    EvidenceRefV1, EvidenceRelationV1, ImmutableRevisionV1, IssueSnapshotV1, LessonCandidateKindV1,
    LessonCandidateStatusV1, LessonCandidateV1, LessonCoverageV1, LessonEngineReceiptV1,
    PullRequestSnapshotV1, SourceKindV1,
};

use super::parse::{CaseCorpusBundle, ProvenanceEventKindV1, ProvenanceEventV1};
use super::pilot::CorpusManifestV1;

/// Optional prose override. When absent, fields are derived deterministically
/// from the frozen case + issue title/body (full, never truncated).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaseDraft {
    pub situation: String,
    pub proposed_ruling: String,
    pub why: String,
    pub how_to_apply: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdaptError {
    NotInFrozenManifest { case_id: String },
    EmptyDraftField(&'static str),
    NoEvidenceRefs,
}

impl std::fmt::Display for AdaptError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotInFrozenManifest { case_id } => write!(
                f,
                "corpus case {case_id} is not a member of the frozen corpus manifest \
                 — refusing to adapt an unselected case"
            ),
            Self::EmptyDraftField(field) => {
                write!(f, "case draft field `{field}` is empty")
            }
            Self::NoEvidenceRefs => write!(
                f,
                "adapt produced no evidence refs — a candidate cannot cite an empty chain"
            ),
        }
    }
}

/// Concrete per-case adapt result (generic `EvidenceEnvelopeV1<T>` deferred).
///
/// Carries the full issue/PR snapshots (not just hashes) so a consumer can
/// recover and verify the semantic fields behind the revision bindings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GithubCorpusCaseResult {
    pub case_id: String,
    pub evidence_refs: Vec<EvidenceRefV1>,
    /// Full issue snapshot (revision-bound contract C3).
    pub issue: IssueSnapshotV1,
    /// Full PR snapshot when present (revision-bound contract C3).
    pub pull_request: Option<PullRequestSnapshotV1>,
    pub issue_snapshot_hash: String,
    pub pr_snapshot_hash: Option<String>,
    /// True when closed/merged produced OUTCOME evidence (Supports), never
    /// establishment.
    pub outcome_evidence: bool,
    /// True when reopen/revert appended Contradicts without rewriting older refs.
    pub overturn_appended: bool,
    pub candidate: LessonCandidateV1,
}

/// Aggregate pilot report — numbers only; no threshold auto-decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CorpusPilotReport {
    pub cases: usize,
    pub candidates_emitted: usize,
    pub outcome_evidence_count: usize,
    pub overturn_count: usize,
}

impl CorpusPilotReport {
    pub fn from_results(results: &[GithubCorpusCaseResult]) -> Self {
        Self {
            cases: results.len(),
            candidates_emitted: results.len(),
            outcome_evidence_count: results.iter().filter(|r| r.outcome_evidence).count(),
            overturn_count: results.iter().filter(|r| r.overturn_appended).count(),
        }
    }
}

fn hash16(seed: &str) -> String {
    let hashed = uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_OID, seed.as_bytes());
    hashed.simple().to_string()[..16].to_string()
}

fn frame_field(value: &str) -> String {
    format!("{}:{}|", value.len(), value)
}

fn group_seed(
    project: &str,
    case_id: &str,
    kind: LessonCandidateKindV1,
    draft: &CaseDraft,
) -> String {
    let mut seed = String::new();
    seed.push_str(&frame_field(project));
    seed.push_str(&frame_field(case_id));
    seed.push_str(&frame_field(kind.as_str()));
    seed.push_str(&frame_field(&draft.situation));
    seed.push_str(&frame_field(&draft.proposed_ruling));
    seed.push_str(&frame_field(&draft.why));
    seed.push_str(&frame_field(&draft.how_to_apply));
    seed
}

/// Bind source_revision to issue/PR snapshot hashes **and** the ordered
/// event chain. Each hop contributes ONLY content/event identity —
/// `kind + revision_hash + target_ref`. Crawl-time (`occurred_at` /
/// `captured_at`) is deliberately EXCLUDED: the same content read at two
/// different crawl times must yield the SAME candidate_id (idempotency),
/// while a distinct event (e.g. a Revert hop appended to the chain, with its
/// own kind + revision_hash + target_ref) still changes the identity — that
/// is why a pure revert over unchanged snapshots still produces a distinct
/// candidate. `revision_hash` and `target_ref` are length-framed so no pair
/// of chains that genuinely differ can collide through delimiter injection.
fn source_revision_string(
    issue_hash: &str,
    pr_hash: Option<&str>,
    events: &[ProvenanceEventV1],
) -> String {
    let mut s = match pr_hash {
        Some(pr) => format!("issue:{issue_hash}+pr:{pr}"),
        None => format!("issue:{issue_hash}"),
    };
    for event in events {
        s.push('|');
        s.push_str(event.kind.as_str());
        s.push(':');
        s.push_str(&frame_field(&event.revision_hash));
        s.push_str(&frame_field(&event.target_ref));
    }
    s
}

fn derived_draft(case: &super::pilot::CorpusCaseV1, bundle: &CaseCorpusBundle) -> CaseDraft {
    CaseDraft {
        situation: format!("{}\n{}", bundle.issue.title, bundle.issue.body),
        proposed_ruling: case.reference_decision.clone(),
        why: case.selection_reason.clone(),
        how_to_apply: case.cold_start_material_decision.clone(),
    }
}

/// Ordered counted-source segments, each paired with the byte cost it
/// contributes to `source_bytes`. The issue title/body block has no leading
/// separator; every later segment (selected comment, PR title, PR body) is
/// preceded by one `\n` in the joined source, so its cost is `1 + len`.
/// Empty segments (e.g. an absent PR body) are emitted with their raw cost
/// here — [`coverage_for_draft`] is responsible for dropping them so an empty
/// string never earns a `contains("")` credit.
fn counted_source_segments(bundle: &CaseCorpusBundle) -> Vec<(String, usize)> {
    let issue_block = format!("{}\n{}", bundle.issue.title, bundle.issue.body);
    let mut segments = vec![(issue_block.clone(), issue_block.len())];
    for c in &bundle.issue.selected_comment_revisions {
        segments.push((c.body.clone(), 1 + c.body.len()));
    }
    if let Some(pr) = &bundle.pull_request {
        segments.push((pr.title.clone(), 1 + pr.title.len()));
        segments.push((pr.body.clone(), 1 + pr.body.len()));
    }
    segments
}

/// Honest coverage: a counted source segment counts as covered ONLY when its
/// text is NON-EMPTY AND actually appears in the prose `situation`. `full`
/// therefore requires every non-empty segment (issue title/body, each selected
/// comment, PR title/body) to be consumed — a long custom situation whose byte
/// length happens to exceed the counted source but never contains the
/// comments/PR body reports `partial`, never `full`. Byte length alone never
/// buys coverage.
///
/// An EMPTY segment (e.g. an absent/empty PR body — a legitimate parse case)
/// consumed no source text, so it contributes 0 to BOTH sides: it is never
/// credited via the vacuously-true `contains("")`, and it never inflates the
/// denominator. `full` therefore does not spuriously depend on an empty body.
fn coverage_for_draft(draft: &CaseDraft, bundle: &CaseCorpusBundle) -> LessonCoverageV1 {
    let segments = counted_source_segments(bundle);
    let mut source_bytes = 0usize;
    let mut covered_bytes = 0usize;
    for (text, cost) in &segments {
        if text.is_empty() {
            continue;
        }
        source_bytes += *cost;
        if draft.situation.contains(text.as_str()) {
            covered_bytes += *cost;
        }
    }
    if covered_bytes >= source_bytes {
        LessonCoverageV1::full(source_bytes)
    } else {
        LessonCoverageV1 {
            source_bytes,
            covered_bytes,
        }
    }
}

fn build_evidence_chain(bundle: &CaseCorpusBundle) -> (Vec<EvidenceRefV1>, bool, bool) {
    let mut refs = Vec::new();
    let mut outcome_evidence = false;
    let mut overturn_appended = false;
    let captured_at = bundle.captured_at.as_str();

    // Always start with the issue snapshot (DerivedFrom).
    refs.push(EvidenceRefV1 {
        relation: EvidenceRelationV1::DerivedFrom,
        target_kind: SourceKindV1::Issue,
        target_ref: bundle.issue.issue_ref.clone(),
        immutable_revision: ImmutableRevisionV1::IssueSnapshotHash(
            bundle.issue.issue_snapshot_hash.clone(),
        ),
        section_or_span: None,
        captured_at: captured_at.to_string(),
    });

    // Selected comment revisions from the issue snapshot.
    for comment in &bundle.issue.selected_comment_revisions {
        refs.push(EvidenceRefV1 {
            relation: EvidenceRelationV1::Supports,
            target_kind: SourceKindV1::Comment,
            target_ref: format!("{}@{}", bundle.issue.issue_ref, comment.comment_id),
            immutable_revision: ImmutableRevisionV1::Comment {
                comment_id: comment.comment_id.clone(),
                updated_at: comment.updated_at.clone(),
                body_hash: comment.body_hash.clone(),
            },
            section_or_span: None,
            captured_at: captured_at.to_string(),
        });
    }

    if let Some(pr) = &bundle.pull_request {
        refs.push(EvidenceRefV1 {
            relation: EvidenceRelationV1::Supports,
            target_kind: SourceKindV1::Pr,
            target_ref: pr.pr_ref.clone(),
            immutable_revision: ImmutableRevisionV1::PrSnapshotHash(pr.pr_snapshot_hash.clone()),
            section_or_span: None,
            captured_at: captured_at.to_string(),
        });
    }

    for event in &bundle.events {
        match event.kind {
            ProvenanceEventKindV1::IssueOpened | ProvenanceEventKindV1::PrOpened => {
                let (target_kind, revision) = if event.kind == ProvenanceEventKindV1::PrOpened {
                    (
                        SourceKindV1::Pr,
                        ImmutableRevisionV1::PrSnapshotHash(event.revision_hash.clone()),
                    )
                } else {
                    (
                        SourceKindV1::Issue,
                        ImmutableRevisionV1::IssueSnapshotHash(event.revision_hash.clone()),
                    )
                };
                refs.push(EvidenceRefV1 {
                    relation: EvidenceRelationV1::DerivedFrom,
                    target_kind,
                    target_ref: event.target_ref.clone(),
                    immutable_revision: revision,
                    section_or_span: Some(event.kind.as_str().to_string()),
                    captured_at: event.occurred_at.clone(),
                });
            }
            ProvenanceEventKindV1::CommentSelected => {
                refs.push(EvidenceRefV1 {
                    relation: EvidenceRelationV1::Supports,
                    target_kind: SourceKindV1::Comment,
                    target_ref: event.target_ref.clone(),
                    immutable_revision: ImmutableRevisionV1::Comment {
                        comment_id: event.target_ref.clone(),
                        updated_at: event.occurred_at.clone(),
                        body_hash: event.revision_hash.clone(),
                    },
                    section_or_span: None,
                    captured_at: event.occurred_at.clone(),
                });
            }
            ProvenanceEventKindV1::PrMerged | ProvenanceEventKindV1::IssueClosed => {
                outcome_evidence = true;
                let (kind, revision) = if event.kind == ProvenanceEventKindV1::PrMerged {
                    (
                        SourceKindV1::Commit,
                        ImmutableRevisionV1::PrHeadSha(event.revision_hash.clone()),
                    )
                } else {
                    (
                        SourceKindV1::Issue,
                        ImmutableRevisionV1::IssueSnapshotHash(event.revision_hash.clone()),
                    )
                };
                refs.push(EvidenceRefV1 {
                    relation: EvidenceRelationV1::Supports,
                    target_kind: kind,
                    target_ref: event.target_ref.clone(),
                    immutable_revision: revision,
                    section_or_span: Some(event.kind.as_str().to_string()),
                    captured_at: event.occurred_at.clone(),
                });
            }
            ProvenanceEventKindV1::Reopen | ProvenanceEventKindV1::Revert => {
                overturn_appended = true;
                let kind = if event.kind == ProvenanceEventKindV1::Revert {
                    SourceKindV1::Commit
                } else {
                    SourceKindV1::Issue
                };
                let revision = if event.kind == ProvenanceEventKindV1::Revert {
                    ImmutableRevisionV1::PrHeadSha(event.revision_hash.clone())
                } else {
                    ImmutableRevisionV1::IssueSnapshotHash(event.revision_hash.clone())
                };
                refs.push(EvidenceRefV1 {
                    relation: EvidenceRelationV1::Contradicts,
                    target_kind: kind,
                    target_ref: event.target_ref.clone(),
                    immutable_revision: revision,
                    section_or_span: Some(event.kind.as_str().to_string()),
                    captured_at: event.occurred_at.clone(),
                });
            }
        }
    }

    // Closed/merged state on the snapshots themselves also counts as outcome
    // even when the event list omitted an explicit close/merge hop.
    if bundle.issue.state.eq_ignore_ascii_case("CLOSED") {
        outcome_evidence = true;
        if !refs.iter().any(|r| {
            r.relation == EvidenceRelationV1::Supports
                && r.section_or_span.as_deref() == Some("issue_closed")
        }) {
            refs.push(EvidenceRefV1 {
                relation: EvidenceRelationV1::Supports,
                target_kind: SourceKindV1::Issue,
                target_ref: bundle.issue.issue_ref.clone(),
                immutable_revision: ImmutableRevisionV1::IssueSnapshotHash(
                    bundle.issue.issue_snapshot_hash.clone(),
                ),
                section_or_span: Some("issue_closed".to_string()),
                captured_at: captured_at.to_string(),
            });
        }
    }
    if let Some(pr) = &bundle.pull_request {
        if pr.merged || pr.state.eq_ignore_ascii_case("MERGED") {
            outcome_evidence = true;
            if !refs.iter().any(|r| {
                r.relation == EvidenceRelationV1::Supports
                    && r.section_or_span.as_deref() == Some("pr_merged")
            }) {
                let rev = pr
                    .merge_commit_sha
                    .clone()
                    .unwrap_or_else(|| pr.head_sha.clone());
                refs.push(EvidenceRefV1 {
                    relation: EvidenceRelationV1::Supports,
                    target_kind: SourceKindV1::Commit,
                    target_ref: pr.pr_ref.clone(),
                    immutable_revision: ImmutableRevisionV1::PrHeadSha(rev),
                    section_or_span: Some("pr_merged".to_string()),
                    captured_at: captured_at.to_string(),
                });
            }
        }
    }

    (refs, outcome_evidence, overturn_appended)
}

/// Adapt one frozen corpus case into typed evidence + a Pending candidate.
///
/// `engine_receipt` is used as-is; unknown/fallback/missing → preview_only
/// via [`LessonCandidateV1::identity_status`]. External URLs in bodies stay as text.
pub fn adapt_corpus_case(
    project: &str,
    manifest: &CorpusManifestV1,
    bundle: &CaseCorpusBundle,
    draft: Option<CaseDraft>,
    engine_receipt: Option<LessonEngineReceiptV1>,
) -> Result<GithubCorpusCaseResult, AdaptError> {
    let case = manifest
        .find(&bundle.case_id)
        .ok_or_else(|| AdaptError::NotInFrozenManifest {
            case_id: bundle.case_id.clone(),
        })?;

    let draft = draft.unwrap_or_else(|| derived_draft(case, bundle));
    for (name, value) in [
        ("situation", &draft.situation),
        ("proposed_ruling", &draft.proposed_ruling),
        ("why", &draft.why),
        ("how_to_apply", &draft.how_to_apply),
    ] {
        if value.trim().is_empty() {
            return Err(AdaptError::EmptyDraftField(name));
        }
    }

    let (evidence_refs, outcome_evidence, overturn_appended) = build_evidence_chain(bundle);
    if evidence_refs.is_empty() {
        return Err(AdaptError::NoEvidenceRefs);
    }

    let issue_snapshot_hash = bundle.issue.issue_snapshot_hash.clone();
    let pr_snapshot_hash = bundle
        .pull_request
        .as_ref()
        .map(|p| p.pr_snapshot_hash.clone());
    let source_revision = source_revision_string(
        &issue_snapshot_hash,
        pr_snapshot_hash.as_deref(),
        &bundle.events,
    );

    let coverage = coverage_for_draft(&draft, bundle);

    let group_id = hash16(&group_seed(
        project,
        &bundle.case_id,
        case.target_kind,
        &draft,
    ));
    let mut id_seed = group_seed(project, &bundle.case_id, case.target_kind, &draft);
    id_seed.push_str(&frame_field(&source_revision));
    let candidate_id = hash16(&id_seed);

    let candidate = LessonCandidateV1 {
        candidate_id,
        candidate_group_id: group_id,
        kind: case.target_kind,
        situation: draft.situation,
        proposed_ruling: draft.proposed_ruling,
        why: draft.why,
        how_to_apply: draft.how_to_apply,
        refs: evidence_refs.clone(),
        source_row_id: bundle.case_id.clone(),
        source_revision,
        coverage,
        candidate_status: LessonCandidateStatusV1::Pending,
        engine_receipt,
    };

    debug_assert!(!candidate.claims_establishment());
    debug_assert_eq!(candidate.candidate_status, LessonCandidateStatusV1::Pending);

    Ok(GithubCorpusCaseResult {
        case_id: bundle.case_id.clone(),
        evidence_refs,
        issue: bundle.issue.clone(),
        pull_request: bundle.pull_request.clone(),
        issue_snapshot_hash,
        pr_snapshot_hash,
        outcome_evidence,
        overturn_appended,
        candidate,
    })
}
