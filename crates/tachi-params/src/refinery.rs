//! Issue Refinery typed evidence/disposition envelope (#1002).
//!
//! Frozen design authority: `docs/engineering/architecture/issue-refinery-memory-lanes.md`
//! §3 (typed evidence envelope), §4 (Issue Refinery), §5 (issue-to-doc
//! contract). This module implements the subset of that document's V1 target
//! contracts that #1002 lands as callable runtime shapes: `SourceKindV1`,
//! the `EvidenceRefV1`/`RepoRevisionV1`/`CanonicalDocRefV1` reference types,
//! `IssueSnapshotV1`, `IssueEvidenceV1`, the closed `DispositionV1`
//! vocabulary, and `IssueDispositionProposalV1`.
//!
//! Deliberately NOT implemented in this leaf (out of #1002's 8-point
//! acceptance contract, left for later leaves per the canon doc's delivery
//! sequence): the generic `EvidenceEnvelopeV1<T>` wrapper,
//! `AuthorityClassV1`/`AdjudicationStatusV1`, `ApprovalReceiptV1`, and
//! `FreezeReceiptV1` (the approver-authority types named here were deleted
//! with that dormant leaf — see #1583). The proposal replay-staleness
//! *check* required by #1002 is implemented directly against
//! `IssueDispositionProposalV1`'s own pinned fields (see
//! [`check_proposal_replay`]) without a separate receipt type, since
//! #1002's contract explicitly excludes the apply boundary.

use ring::digest::{digest, SHA256};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

// ─── §3: source vocabulary + reference types ────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceKindV1 {
    Issue,
    Comment,
    Pr,
    Commit,
    CanonicalDoc,
    EpisodicMemory,
    Wiki,
    Guide,
    Precedent,
    Eval,
    Runtime,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceRelationV1 {
    DerivedFrom,
    Supports,
    Contradicts,
    Supersedes,
    AppliesTo,
}

/// The immutable-revision union carried by an `EvidenceRefV1`. Untagged
/// (F4 fidelity fix, build-seat REQUEST-CHANGES) to match canon doc §3's
/// literal wire shape: `issue_snapshot_hash | issue_body_hash | {
/// comment_id, updated_at, body_hash } | pr_snapshot_hash | pr_head_sha |
/// blob_sha | memory_revision` — the six hash-shaped alternatives serialize
/// as BARE strings, not `{"kind": ..., "value": ...}` wrapper objects; the
/// `EvidenceRefV1` this is attached to already carries
/// `target_kind: SourceKindV1`, so the pairing disambiguates which kind of
/// hash it is without a second discriminant on this type. The comment
/// alternative serializes as the exact named object canon shows.
///
/// KNOWN LIMITATION (not fully resolved, flagged rather than hidden):
/// `#[serde(untagged)]` deserialization is ambiguous across the six
/// bare-string variants — serde tries each declared variant in order and
/// the first string-shaped one always matches, so `Deserialize` cannot
/// currently distinguish e.g. a `BlobSha` string from a `PrHeadSha` string
/// round-tripped back in. This is inert today (evidence refs in this leaf
/// are only ever serialized outward, never parsed back via `Deserialize`)
/// but would need a real fix (e.g. a paired discriminant field elsewhere,
/// or accepting that only construction-time Rust type identity matters)
/// before anything relies on deserializing this union.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ImmutableRevisionV1 {
    Comment {
        comment_id: String,
        updated_at: String,
        body_hash: String,
    },
    IssueSnapshotHash(String),
    IssueBodyHash(String),
    PrSnapshotHash(String),
    PrHeadSha(String),
    BlobSha(String),
    MemoryRevision(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidenceRefV1 {
    pub relation: EvidenceRelationV1,
    pub target_kind: SourceKindV1,
    /// Corresponds to the canon doc's `ref` field; renamed on the Rust side
    /// because `ref` is a reserved keyword. Wire JSON still uses `"ref"`.
    #[serde(rename = "ref")]
    pub target_ref: String,
    pub immutable_revision: ImmutableRevisionV1,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub section_or_span: Option<String>,
    pub captured_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoRevisionV1 {
    pub repo: String,
    #[serde(rename = "ref")]
    pub git_ref: String,
    pub commit_sha: String,
    pub verified_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CanonicalDocRefV1 {
    pub repo: String,
    pub trusted_ref: String,
    pub commit_sha: String,
    pub path: String,
    pub blob_sha: String,
    pub section: String,
    /// F4 fidelity fix (build-seat REQUEST-CHANGES): canon doc §3 lists this
    /// as a required field, not `Option<String>` — a resolver that hasn't
    /// verified authority has no business constructing a `Resolved`
    /// `CanonicalDocRefV1` at all. A real, honest value describing HOW
    /// authority was verified (e.g. `"git:reachable-from:<trusted_ref>"` for
    /// the live resolver, `"fixture:pre-registered"` for test doubles) —
    /// never a fabricated placeholder posing as a real verification.
    pub authority_receipt: String,
    /// F4 fidelity fix: same rationale as `authority_receipt` — every
    /// existing construction site already always supplied a real timestamp
    /// (`Option` was never actually exercised as `None` outside one hand-
    /// written test fixture), so making it required drops no real
    /// information.
    pub verified_reachable_at: String,
}

// ─── §4.1: grounding + spans ────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GroundingStatusV1 {
    Grounded,
    MissingAnchor,
}

impl GroundingStatusV1 {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Grounded => "grounded",
            Self::MissingAnchor => "missing_anchor",
        }
    }
}

impl fmt::Display for GroundingStatusV1 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Half-open `[start_byte, end_byte)` byte range into a source's exact UTF-8
/// bytes. Every offset produced by this crate's callers must land on a UTF-8
/// character boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceSpanV1 {
    pub start_byte: usize,
    pub end_byte: usize,
}

impl SourceSpanV1 {
    pub fn len(&self) -> usize {
        self.end_byte.saturating_sub(self.start_byte)
    }

    pub fn is_empty(&self) -> bool {
        self.end_byte <= self.start_byte
    }

    pub fn overlaps(&self, other: &SourceSpanV1) -> bool {
        self.start_byte < other.end_byte && other.start_byte < self.end_byte
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnchorKindV1 {
    CanonicalDoc,
    IssueRef,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnchorV1 {
    pub kind: AnchorKindV1,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub doc_ref: Option<CanonicalDocRefV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issue_ref: Option<String>,
}

/// A claim's verification state. `Verified` can only be constructed by
/// supplying a real [`RepoRevisionV1`] — there is no bare `Verified` unit
/// variant, so no code path lacking repo-tool evidence can produce it. This
/// is the type-level half of the #1002 acceptance requirement that
/// model-only reasoning can never mark a HEAD claim verified; the other half
/// is that the default evidence-compiler builder (see `tachi-server`'s
/// `refinery_ops::compiler`) never constructs the `Verified` arm itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ClaimVerificationV1 {
    ModelOnly,
    Verified { repo_revision: RepoRevisionV1 },
}

impl ClaimVerificationV1 {
    pub fn is_verified(&self) -> bool {
        matches!(self, ClaimVerificationV1::Verified { .. })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimV1 {
    pub claim_id: String,
    pub text: String,
    pub source_span: SourceSpanV1,
    pub anchors: Vec<AnchorV1>,
    pub verification: ClaimVerificationV1,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IssueRelationKindV1 {
    Blocks,
    DependsOn,
    DuplicateOf,
    Supersedes,
    ParentOf,
    Related,
}

impl IssueRelationKindV1 {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Blocks => "blocks",
            Self::DependsOn => "depends_on",
            Self::DuplicateOf => "duplicate_of",
            Self::Supersedes => "supersedes",
            Self::ParentOf => "parent_of",
            Self::Related => "related",
        }
    }
}

impl fmt::Display for IssueRelationKindV1 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IssueRelationV1 {
    pub kind: IssueRelationKindV1,
    pub target_ref: String,
    pub evidence_refs: Vec<EvidenceRefV1>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CoverageV1 {
    pub source_bytes: usize,
    pub covered_bytes: usize,
    pub omitted_spans: Vec<SourceSpanV1>,
}

// ─── §3: current-work snapshot (semantic state, not only prose) ────────────

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommentRevisionV1 {
    pub comment_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    pub updated_at: String,
    pub body: String,
    pub body_hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IssueSnapshotV1 {
    pub issue_ref: String,
    pub repo: String,
    pub number: u64,
    pub title: String,
    pub body: String,
    pub state: String,
    pub labels: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub milestone: Option<String>,
    pub dependency_refs: Vec<String>,
    pub selected_comment_revisions: Vec<CommentRevisionV1>,
    pub updated_at: String,
    pub issue_body_hash: String,
    pub issue_snapshot_hash: String,
}

impl IssueSnapshotV1 {
    /// Recompute `issue_snapshot_hash` per §3/§5: SHA-256 over canonical JSON
    /// of the issue's *semantic state* — body, state, labels, milestone,
    /// dependency_refs, selected_comment_revisions, updated_at. Identity
    /// fields (issue_ref/repo/number) and the hash fields themselves are
    /// deliberately excluded from the hash basis.
    pub fn compute_snapshot_hash(&self) -> Result<String, String> {
        let basis = serde_json::json!({
            "body": self.body,
            "state": self.state,
            "labels": self.labels,
            "milestone": self.milestone,
            "dependency_refs": self.dependency_refs,
            "selected_comment_revisions": self.selected_comment_revisions,
            "updated_at": self.updated_at,
        });
        canonical_json_sha256(&basis)
    }
}

// ─── §3: PullRequestSnapshotV1 (semantic PR state) ──────────────────────────

/// One review row contributing to a PR's semantic snapshot hash.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrReviewV1 {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    pub state: String,
    pub submitted_at: String,
}

/// One check-run / status-check row contributing to a PR's semantic snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrCheckV1 {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conclusion: Option<String>,
    pub status: String,
}

/// Current-work PR snapshot per §3: body, state, head/base SHAs, reviews,
/// checks, merge state, and `updated_at`. `pr_head_sha` (via `head_sha`)
/// remains a code revision; `pr_snapshot_hash` is the PR-*state* revision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PullRequestSnapshotV1 {
    pub pr_ref: String,
    pub repo: String,
    pub number: u64,
    pub title: String,
    pub body: String,
    pub state: String,
    pub head_sha: String,
    pub base_sha: String,
    pub reviews: Vec<PrReviewV1>,
    pub checks: Vec<PrCheckV1>,
    pub merged: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub merge_commit_sha: Option<String>,
    pub updated_at: String,
    pub pr_body_hash: String,
    pub pr_snapshot_hash: String,
}

impl PullRequestSnapshotV1 {
    /// Recompute `pr_snapshot_hash` per §3: SHA-256 over canonical JSON of
    /// the PR's *semantic state* — body, state, head/base SHAs, reviews,
    /// checks, merge fields, updated_at. Identity fields (pr_ref/repo/number)
    /// and the hash fields themselves are excluded from the hash basis
    /// (mirrors [`IssueSnapshotV1::compute_snapshot_hash`]).
    pub fn compute_snapshot_hash(&self) -> Result<String, String> {
        let basis = serde_json::json!({
            "body": self.body,
            "state": self.state,
            "head_sha": self.head_sha,
            "base_sha": self.base_sha,
            "reviews": self.reviews,
            "checks": self.checks,
            "merged": self.merged,
            "merge_commit_sha": self.merge_commit_sha,
            "updated_at": self.updated_at,
        });
        canonical_json_sha256(&basis)
    }
}

// ─── §4.2: Issue Refinery packet payloads ──────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IssueEvidenceV1 {
    pub issue_ref: String,
    pub issue_body_hash: String,
    pub issue_snapshot_hash: String,
    pub issue_updated_at: String,
    pub grounding_status: GroundingStatusV1,
    pub issue_snapshot: IssueSnapshotV1,
    pub claims: Vec<ClaimV1>,
    pub relations: Vec<IssueRelationV1>,
    pub linked_specs: Vec<CanonicalDocRefV1>,
    pub coverage: CoverageV1,
}

/// The closed 10-word disposition vocabulary (canon doc §4.2). Wire format
/// is `SCREAMING_SNAKE_CASE`, matching the frozen literal strings exactly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum DispositionV1 {
    Keep,
    Narrow,
    Router,
    Dormant,
    Blocked,
    MergeCandidate,
    CloseFixed,
    CloseSuperseded,
    Historical,
    DecisionRequired,
}

impl DispositionV1 {
    pub const ALL: &'static [Self] = &[
        Self::Keep,
        Self::Narrow,
        Self::Router,
        Self::Dormant,
        Self::Blocked,
        Self::MergeCandidate,
        Self::CloseFixed,
        Self::CloseSuperseded,
        Self::Historical,
        Self::DecisionRequired,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Keep => "KEEP",
            Self::Narrow => "NARROW",
            Self::Router => "ROUTER",
            Self::Dormant => "DORMANT",
            Self::Blocked => "BLOCKED",
            Self::MergeCandidate => "MERGE_CANDIDATE",
            Self::CloseFixed => "CLOSE_FIXED",
            Self::CloseSuperseded => "CLOSE_SUPERSEDED",
            Self::Historical => "HISTORICAL",
            Self::DecisionRequired => "DECISION_REQUIRED",
        }
    }
}

impl fmt::Display for DispositionV1 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for DispositionV1 {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim() {
            "KEEP" => Ok(Self::Keep),
            "NARROW" => Ok(Self::Narrow),
            "ROUTER" => Ok(Self::Router),
            "DORMANT" => Ok(Self::Dormant),
            "BLOCKED" => Ok(Self::Blocked),
            "MERGE_CANDIDATE" => Ok(Self::MergeCandidate),
            "CLOSE_FIXED" => Ok(Self::CloseFixed),
            "CLOSE_SUPERSEDED" => Ok(Self::CloseSuperseded),
            "HISTORICAL" => Ok(Self::Historical),
            "DECISION_REQUIRED" => Ok(Self::DecisionRequired),
            other => Err(format!("Invalid issue refinery disposition '{other}'")),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContradictionV1 {
    pub description: String,
    pub evidence_refs: Vec<EvidenceRefV1>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocDeltaProposalV1 {
    pub path: String,
    pub blob_sha: String,
    pub section: String,
    pub reason: String,
    pub summary: String,
}

/// Minimal engine-identity receipt. #1002's V1 has no live path that
/// captures a real calling-model identity from the MCP transport, so
/// `IssueDispositionProposalV1::preview_only` is unconditionally `true` in
/// this leaf's orchestration — see `tachi-server`'s `refinery_ops` module.
/// The type exists so a later leaf that DOES have an identity receipt can
/// plug it in without a payload-shape change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EngineReceiptV1 {
    pub requested_role: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effective_provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effective_model: Option<String>,
    pub fallback: bool,
    pub degraded: bool,
}

impl EngineReceiptV1 {
    pub fn has_known_identity(&self) -> bool {
        self.effective_provider.is_some()
            && self.effective_model.is_some()
            && !self.fallback
            && !self.degraded
    }
}

/// F4 fidelity note (build-seat REQUEST-CHANGES): canon doc §4.2's literal
/// snippet lists 9 fields (`issue_ref`, `based_on_repo_revisions`,
/// `based_on_issue_snapshot_hash`, `disposition`, `evidence_refs`,
/// `contradictions`, `proposed_comment`, `proposed_labels`,
/// `proposed_doc_deltas`). The 7 fields below that snippet does not show
/// are each kept for a specific, cited reason — none are unjustified
/// scope-creep — see each field's own doc comment for its basis. A full
/// field→basis table also ships in this leaf's delivery report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IssueDispositionProposalV1 {
    /// Extension. Not in the §4.2 snippet; required by #1002's own dispatch
    /// instructions ("proposal 绑 packet id/proposal hash/source bundle
    /// hash/..."), which are a frozen input to this leaf distinct from (but
    /// not contradicting) the design-doc snippet. Deterministic:
    /// `issue-refinery/<issue_ref>/<issue_snapshot_hash>`.
    pub packet_id: String,
    /// Extension, same #1002 dispatch-instruction basis as `packet_id`.
    /// SHA-256 over the proposal's own decision-relevant fields (§5 hashing
    /// contract) — the reproducibility anchor `check_proposal_replay`-style
    /// consumers can use to prove a proposal wasn't tampered with in transit.
    pub proposal_hash: String,
    /// Extension, same #1002 dispatch-instruction basis. SHA-256 fingerprint
    /// of the evidence bundle (issue snapshot hash + doc/repo revisions)
    /// this proposal was built from — lets a caller cheaply check "is this
    /// proposal even talking about the bundle I have" before running the
    /// full `check_proposal_replay`.
    pub source_bundle_hash: String,
    pub issue_ref: String,
    pub based_on_issue_snapshot_hash: String,
    pub based_on_repo_revisions: Vec<RepoRevisionV1>,
    /// Extension beyond the literal §4.2 snippet (which shows
    /// `based_on_repo_revisions` but not a doc-revision sibling) — required
    /// by #1002's own dispatch instructions ("doc 修订/repo 修订") and by
    /// §5's replay contract itself, which is explicitly about detecting
    /// drift in "a changed blob SHA" — that requires pinning WHICH doc
    /// revisions the proposal was based on in the first place.
    pub based_on_doc_revisions: Vec<CanonicalDocRefV1>,
    /// Extension. Canon doc §4.1 defines the `grounding_status` concept
    /// (`grounding_status=missing_anchor`) as a property of the evidence
    /// packet; carrying it onto the disposition proposal too lets a
    /// consumer of ONLY the proposal (without the full evidence packet)
    /// still see why a `DECISION_REQUIRED` disposition was forced.
    pub grounding_status: GroundingStatusV1,
    pub disposition: DispositionV1,
    pub evidence_refs: Vec<EvidenceRefV1>,
    pub contradictions: Vec<ContradictionV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proposed_comment: Option<String>,
    pub proposed_labels: Vec<String>,
    pub proposed_doc_deltas: Vec<DocDeltaProposalV1>,
    /// Extension. Canon doc §3/§4.3 establishes the preview-only concept
    /// ("Unknown identity ... makes the result preview-only"; "#1002 ...
    /// proposal-only apply boundary") without listing it as a named
    /// `IssueDispositionProposalV1` field in the §4.2 snippet — carrying it
    /// explicitly (rather than leaving it implicit/undocumented) makes the
    /// invariant machine-checkable instead of a convention a consumer has
    /// to already know. Unconditionally `true` in this leaf (see
    /// `refinery_ops::mod` doc comments).
    pub preview_only: bool,
    /// Extension, paired with `preview_only` — canon doc §3 defines
    /// `EngineReceiptV1` and the rule "Unknown identity, an undeclared
    /// fallback, provider timeout, or missing tool access makes the result
    /// preview-only"; carrying the receipt itself lets a reviewer see WHY
    /// `preview_only` is true, not just that it is. Always `None` in this
    /// leaf (no live path captures a real engine identity yet).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub engine_receipt: Option<EngineReceiptV1>,
}

impl IssueDispositionProposalV1 {
    /// SHA-256 over the canonical JSON of this ENTIRE proposal, excluding
    /// `proposal_hash` itself (R4-4, build-seat REQUEST-CHANGES: the old
    /// hash basis hand-picked 8 fields and silently missed
    /// `evidence_refs`/`proposed_comment`/`grounding_status`/
    /// `preview_only`/`engine_receipt` — a full-struct serialization can't
    /// drift out of sync with the type's own field list the way a
    /// hand-picked list can, because adding a new field to this struct
    /// automatically becomes part of the hash basis with zero extra code).
    /// Callers construct the proposal with a placeholder `proposal_hash`
    /// (e.g. `String::new()`), call this once, then overwrite the field
    /// with the result.
    pub fn compute_proposal_hash(&self) -> Result<String, String> {
        let mut value = serde_json::to_value(self)
            .map_err(|e| format!("serialize proposal for hashing: {e}"))?;
        if let Some(obj) = value.as_object_mut() {
            obj.remove("proposal_hash");
        }
        canonical_json_sha256(&value)
    }
}

// ─── §4.2/§5: anti-replay guard (types + check; apply itself is out of scope) ─

/// The freshly recomputed grounding state a proposal is checked against.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CurrentGroundStateV1 {
    pub issue_snapshot_hash: String,
    pub repo_revisions: Vec<RepoRevisionV1>,
    pub doc_revisions: Vec<CanonicalDocRefV1>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum StalenessReasonV1 {
    IssueSnapshotChanged {
        expected: String,
        actual: String,
    },
    RepoRevisionChanged {
        repo: String,
        expected_commit_sha: String,
        actual_commit_sha: Option<String>,
    },
    DocRevisionChanged {
        path: String,
        expected_blob_sha: String,
        actual_blob_sha: Option<String>,
    },
    /// R4-2 (build-seat REQUEST-CHANGES): a proposal that never established
    /// ANY repo-revision pin at all (empty `based_on_repo_revisions`) must
    /// not vacuously "pass" replay on that axis — an empty axis means
    /// nobody ever verified the repo's state, not that it's unchanged.
    RepoRevisionUnavailable {
        detail: String,
    },
}

/// A stale issue snapshot hash, repo revision, or doc revision makes an
/// existing proposal non-replayable (canon doc §4.2/§5). Returns `Ok(())`
/// when every pinned fact still matches; otherwise every mismatch found
/// (not just the first) so a caller can report the full staleness reason.
pub fn check_proposal_replay(
    proposal: &IssueDispositionProposalV1,
    current: &CurrentGroundStateV1,
) -> Result<(), Vec<StalenessReasonV1>> {
    let mut reasons = Vec::new();

    if proposal.based_on_issue_snapshot_hash != current.issue_snapshot_hash {
        reasons.push(StalenessReasonV1::IssueSnapshotChanged {
            expected: proposal.based_on_issue_snapshot_hash.clone(),
            actual: current.issue_snapshot_hash.clone(),
        });
    }

    if proposal.based_on_repo_revisions.is_empty() {
        reasons.push(StalenessReasonV1::RepoRevisionUnavailable {
            detail: "proposal has no pinned repo revision at all — replay safety on the \
                     repo-HEAD axis was never established, not confirmed unchanged"
                .to_string(),
        });
    }
    for repo_rev in &proposal.based_on_repo_revisions {
        // Match by (repo, ref) — not repo alone — so a proposal pinned
        // against one ref can't be silently checked against a different
        // ref's commit for the same repo.
        let actual = current
            .repo_revisions
            .iter()
            .find(|r| r.repo == repo_rev.repo && r.git_ref == repo_rev.git_ref);
        let actual_sha = actual.map(|r| r.commit_sha.clone());
        if actual_sha.as_deref() != Some(repo_rev.commit_sha.as_str()) {
            reasons.push(StalenessReasonV1::RepoRevisionChanged {
                repo: repo_rev.repo.clone(),
                expected_commit_sha: repo_rev.commit_sha.clone(),
                actual_commit_sha: actual_sha,
            });
        }
    }

    for doc_rev in &proposal.based_on_doc_revisions {
        let actual = current.doc_revisions.iter().find(|d| {
            d.repo == doc_rev.repo
                && d.trusted_ref == doc_rev.trusted_ref
                && d.path == doc_rev.path
                && d.section == doc_rev.section
        });
        // The complete canonical identity must match — repo, trusted_ref,
        // commit_sha, path, blob_sha, AND section. An identical blob can
        // legitimately back more than one section, but authority over one
        // section does not authorize replay against another.
        // Canon doc §5: "a SHA alone does not grant canonical authority" —
        // an identical blob_sha at a DIFFERENT commit/trusted_ref must still
        // be treated as stale (the original approval was scoped to the
        // pinned commit's full context, not just this one file's content).
        let matches = actual
            .map(|d| {
                d.commit_sha == doc_rev.commit_sha
                    && d.trusted_ref == doc_rev.trusted_ref
                    && d.blob_sha == doc_rev.blob_sha
            })
            .unwrap_or(false);
        if !matches {
            reasons.push(StalenessReasonV1::DocRevisionChanged {
                path: doc_rev.path.clone(),
                expected_blob_sha: doc_rev.blob_sha.clone(),
                actual_blob_sha: actual.map(|d| d.blob_sha.clone()),
            });
        }
    }

    if reasons.is_empty() {
        Ok(())
    } else {
        Err(reasons)
    }
}

// ─── §5: canonical hashing primitives ──────────────────────────────────────

/// Normalize CRLF/CR to LF. Used everywhere the canon doc requires "line
/// endings normalized to LF" (issue body hash basis, canonical JSON string
/// values).
pub fn normalize_line_endings(input: &str) -> String {
    input.replace("\r\n", "\n").replace('\r', "\n")
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    let d = digest(&SHA256, bytes);
    d.as_ref().iter().map(|b| format!("{b:02x}")).collect()
}

/// SHA-256 over the exact UTF-8 issue body after CRLF/CR normalized to LF
/// and any line beginning exactly `Freeze-Receipt:` removed, preserving all
/// other bytes (canon doc §5 — "This makes the hash reproducible without
/// making the body self-referential").
pub fn compute_issue_body_hash(raw_body: &str) -> String {
    let normalized = normalize_line_endings(raw_body);
    let mut filtered = String::with_capacity(normalized.len());
    for line_with_ending in normalized.split_inclusive('\n') {
        let line = line_with_ending
            .strip_suffix('\n')
            .unwrap_or(line_with_ending);
        if !line.starts_with("Freeze-Receipt:") {
            filtered.push_str(line_with_ending);
        }
    }
    sha256_hex(filtered.as_bytes())
}

/// Canonical JSON per §5: object keys sorted lexicographically (already
/// guaranteed by `serde_json::Map`'s `BTreeMap` backing in this workspace,
/// since the `preserve_order` feature is not enabled anywhere in the
/// dependency graph — this function's sort is defensive, not load-bearing),
/// array order preserved, and every string value's line endings normalized
/// to LF.
pub fn canonicalize_json(value: &serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            let mut sorted = serde_json::Map::new();
            for key in keys {
                sorted.insert(key.clone(), canonicalize_json(&map[key]));
            }
            serde_json::Value::Object(sorted)
        }
        serde_json::Value::Array(items) => {
            serde_json::Value::Array(items.iter().map(canonicalize_json).collect())
        }
        serde_json::Value::String(s) => serde_json::Value::String(normalize_line_endings(s)),
        other => other.clone(),
    }
}

/// SHA-256 over the canonical JSON serialization of `value`.
pub fn canonical_json_sha256<T: Serialize>(value: &T) -> Result<String, String> {
    let raw =
        serde_json::to_value(value).map_err(|e| format!("canonical_json_sha256 serialize: {e}"))?;
    let canonical = canonicalize_json(&raw);
    let text = serde_json::to_string(&canonical)
        .map_err(|e| format!("canonical_json_sha256 stringify: {e}"))?;
    Ok(sha256_hex(text.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disposition_roundtrip_all_variants() {
        for &d in DispositionV1::ALL {
            let s = d.as_str();
            let parsed: DispositionV1 = s.parse().expect("disposition parse");
            assert_eq!(parsed, d);
            let wire = serde_json::to_string(&d).unwrap();
            assert_eq!(wire, format!("\"{s}\""));
            let back: DispositionV1 = serde_json::from_str(&wire).unwrap();
            assert_eq!(back, d);
        }
        assert!("NOPE".parse::<DispositionV1>().is_err());
    }

    #[test]
    fn grounding_status_wire_values() {
        assert_eq!(GroundingStatusV1::Grounded.as_str(), "grounded");
        assert_eq!(GroundingStatusV1::MissingAnchor.as_str(), "missing_anchor");
        assert_eq!(
            serde_json::to_string(&GroundingStatusV1::MissingAnchor).unwrap(),
            "\"missing_anchor\""
        );
    }

    #[test]
    fn issue_relation_kind_wire_values() {
        assert_eq!(IssueRelationKindV1::DependsOn.as_str(), "depends_on");
        assert_eq!(
            serde_json::to_string(&IssueRelationKindV1::DuplicateOf).unwrap(),
            "\"duplicate_of\""
        );
    }

    #[test]
    fn compute_issue_body_hash_strips_freeze_receipt_line_and_normalizes_crlf() {
        let with_crlf_and_receipt =
            "Title line\r\nBody detail.\r\nFreeze-Receipt: abc123\r\nTrailer.\r\n";
        let without_receipt_lf = "Title line\nBody detail.\nTrailer.\n";
        assert_eq!(
            compute_issue_body_hash(with_crlf_and_receipt),
            sha256_hex(without_receipt_lf.as_bytes())
        );
    }

    #[test]
    fn compute_issue_body_hash_preserves_trailing_lf_and_only_removes_exact_receipt_lines() {
        assert_ne!(
            compute_issue_body_hash("same body"),
            compute_issue_body_hash("same body\n"),
            "the issue body's trailing LF is part of its byte identity"
        );
        assert_eq!(
            compute_issue_body_hash("body\n Freeze-Receipt: advisory\n"),
            sha256_hex(b"body\n Freeze-Receipt: advisory\n"),
            "an indented prose mention is not an exact Freeze-Receipt line"
        );
    }

    #[test]
    fn compute_issue_body_hash_is_reproducible() {
        let a = compute_issue_body_hash("same body\ntext");
        let b = compute_issue_body_hash("same body\ntext");
        assert_eq!(a, b);
        assert_ne!(a, compute_issue_body_hash("different body"));
    }

    #[test]
    fn canonical_json_sha256_is_order_independent_for_object_keys() {
        let a = serde_json::json!({"b": 1, "a": 2});
        let b = serde_json::json!({"a": 2, "b": 1});
        assert_eq!(
            canonical_json_sha256(&a).unwrap(),
            canonical_json_sha256(&b).unwrap()
        );
    }

    #[test]
    fn canonical_json_sha256_normalizes_string_line_endings() {
        let a = serde_json::json!({"body": "line1\r\nline2"});
        let b = serde_json::json!({"body": "line1\nline2"});
        assert_eq!(
            canonical_json_sha256(&a).unwrap(),
            canonical_json_sha256(&b).unwrap()
        );
    }

    #[test]
    fn snapshot_hash_ignores_identity_fields_but_covers_semantic_state() {
        let base = IssueSnapshotV1 {
            issue_ref: "owner/repo#1".to_string(),
            repo: "owner/repo".to_string(),
            number: 1,
            title: "t".to_string(),
            body: "same body".to_string(),
            state: "OPEN".to_string(),
            labels: vec!["bug".to_string()],
            milestone: None,
            dependency_refs: vec![],
            selected_comment_revisions: vec![],
            updated_at: "2026-07-13T00:00:00Z".to_string(),
            issue_body_hash: String::new(),
            issue_snapshot_hash: String::new(),
        };
        let mut renumbered = base.clone();
        renumbered.issue_ref = "owner/repo#2".to_string();
        renumbered.number = 2;
        assert_eq!(
            base.compute_snapshot_hash().unwrap(),
            renumbered.compute_snapshot_hash().unwrap(),
            "identity fields must not affect the semantic-state snapshot hash"
        );

        let mut changed_body = base.clone();
        changed_body.body = "different body".to_string();
        assert_ne!(
            base.compute_snapshot_hash().unwrap(),
            changed_body.compute_snapshot_hash().unwrap(),
            "a body change must change the semantic-state snapshot hash"
        );
    }

    fn sample_pull_request_snapshot() -> PullRequestSnapshotV1 {
        PullRequestSnapshotV1 {
            pr_ref: "owner/repo#10".to_string(),
            repo: "owner/repo".to_string(),
            number: 10,
            title: "t".to_string(),
            body: "same body".to_string(),
            state: "OPEN".to_string(),
            head_sha: "aaa111".to_string(),
            base_sha: "bbb222".to_string(),
            reviews: vec![PrReviewV1 {
                author: Some("alice".to_string()),
                state: "APPROVED".to_string(),
                submitted_at: "2026-07-13T01:00:00Z".to_string(),
            }],
            checks: vec![PrCheckV1 {
                name: "ci".to_string(),
                conclusion: Some("SUCCESS".to_string()),
                status: "COMPLETED".to_string(),
            }],
            merged: false,
            merge_commit_sha: None,
            updated_at: "2026-07-13T00:00:00Z".to_string(),
            pr_body_hash: String::new(),
            pr_snapshot_hash: String::new(),
        }
    }

    #[test]
    fn pull_request_snapshot_hash_ignores_identity_fields_but_covers_semantic_state() {
        let base = sample_pull_request_snapshot();
        let mut renumbered = base.clone();
        renumbered.pr_ref = "owner/repo#99".to_string();
        renumbered.number = 99;
        assert_eq!(
            base.compute_snapshot_hash().unwrap(),
            renumbered.compute_snapshot_hash().unwrap(),
            "identity fields must not affect the PR semantic-state snapshot hash"
        );

        let mut changed_body = base.clone();
        changed_body.body = "different body".to_string();
        assert_ne!(
            base.compute_snapshot_hash().unwrap(),
            changed_body.compute_snapshot_hash().unwrap(),
            "a one-byte body change must invalidate the PR snapshot hash"
        );
    }

    #[test]
    fn pull_request_snapshot_hash_is_stable_across_recomputation() {
        let mut snap = sample_pull_request_snapshot();
        let h1 = snap.compute_snapshot_hash().unwrap();
        snap.pr_snapshot_hash = h1.clone();
        let h2 = snap.compute_snapshot_hash().unwrap();
        assert_eq!(
            h1, h2,
            "recomputing over the same semantic state must be stable"
        );
    }

    #[test]
    fn claim_verification_default_path_is_never_verified_without_repo_revision() {
        // There is no bare `Verified` unit variant to construct — every
        // `Verified` value must carry a real `RepoRevisionV1`. This proves
        // the only zero-evidence construction available is `ModelOnly`.
        let model_only = ClaimVerificationV1::ModelOnly;
        assert!(!model_only.is_verified());

        let verified = ClaimVerificationV1::Verified {
            repo_revision: RepoRevisionV1 {
                repo: "owner/repo".to_string(),
                git_ref: "main".to_string(),
                commit_sha: "deadbeef".to_string(),
                verified_at: "2026-07-13T00:00:00Z".to_string(),
            },
        };
        assert!(verified.is_verified());
    }

    #[test]
    fn check_proposal_replay_fresh_when_all_pins_match() {
        let proposal = sample_proposal();
        let current = CurrentGroundStateV1 {
            issue_snapshot_hash: proposal.based_on_issue_snapshot_hash.clone(),
            repo_revisions: proposal.based_on_repo_revisions.clone(),
            doc_revisions: proposal.based_on_doc_revisions.clone(),
        };
        assert!(check_proposal_replay(&proposal, &current).is_ok());
    }

    #[test]
    fn check_proposal_replay_rejects_stale_issue_snapshot() {
        let proposal = sample_proposal();
        let mut current = CurrentGroundStateV1 {
            issue_snapshot_hash: proposal.based_on_issue_snapshot_hash.clone(),
            repo_revisions: proposal.based_on_repo_revisions.clone(),
            doc_revisions: proposal.based_on_doc_revisions.clone(),
        };
        current.issue_snapshot_hash = "different-hash".to_string();
        let err = check_proposal_replay(&proposal, &current).expect_err("must be stale");
        assert!(matches!(
            err[0],
            StalenessReasonV1::IssueSnapshotChanged { .. }
        ));
    }

    #[test]
    fn check_proposal_replay_rejects_stale_doc_revision() {
        let proposal = sample_proposal();
        let mut current = CurrentGroundStateV1 {
            issue_snapshot_hash: proposal.based_on_issue_snapshot_hash.clone(),
            repo_revisions: proposal.based_on_repo_revisions.clone(),
            doc_revisions: proposal.based_on_doc_revisions.clone(),
        };
        current.doc_revisions[0].blob_sha = "newblob".to_string();
        let err = check_proposal_replay(&proposal, &current).expect_err("must be stale");
        assert!(err
            .iter()
            .any(|r| matches!(r, StalenessReasonV1::DocRevisionChanged { .. })));
    }

    /// F2 (build-seat REQUEST-CHANGES): identical blob content at a
    /// DIFFERENT commit must still be treated as stale — "a SHA alone does
    /// not grant canonical authority" (canon doc §5). A byte-identical file
    /// re-verified at a different commit was not the commit the original
    /// approval was scoped to.
    #[test]
    fn check_proposal_replay_rejects_same_blob_at_a_different_commit() {
        let proposal = sample_proposal();
        let mut current = CurrentGroundStateV1 {
            issue_snapshot_hash: proposal.based_on_issue_snapshot_hash.clone(),
            repo_revisions: proposal.based_on_repo_revisions.clone(),
            doc_revisions: proposal.based_on_doc_revisions.clone(),
        };
        assert_eq!(
            current.doc_revisions[0].blob_sha, proposal.based_on_doc_revisions[0].blob_sha,
            "test setup: blob_sha must start identical"
        );
        current.doc_revisions[0].commit_sha = "a-different-commit-sha".to_string();
        let err = check_proposal_replay(&proposal, &current)
            .expect_err("same blob at a different commit must still be stale");
        assert!(err
            .iter()
            .any(|r| matches!(r, StalenessReasonV1::DocRevisionChanged { .. })));
    }

    #[test]
    fn check_proposal_replay_rejects_same_blob_at_a_different_section() {
        let proposal = sample_proposal();
        let mut current = CurrentGroundStateV1 {
            issue_snapshot_hash: proposal.based_on_issue_snapshot_hash.clone(),
            repo_revisions: proposal.based_on_repo_revisions.clone(),
            doc_revisions: proposal.based_on_doc_revisions.clone(),
        };
        assert_eq!(
            current.doc_revisions[0].blob_sha, proposal.based_on_doc_revisions[0].blob_sha,
            "test setup: blob_sha must start identical"
        );
        current.doc_revisions[0].section = "a-different-section".to_string();
        let err = check_proposal_replay(&proposal, &current)
            .expect_err("same blob at a different section must still be stale");
        assert!(err
            .iter()
            .any(|r| matches!(r, StalenessReasonV1::DocRevisionChanged { .. })));
    }

    /// F2: repo HEAD moving (commit_sha drift on the SAME repo+ref) must
    /// reject replay — this is the axis `based_on_repo_revisions` exists to
    /// guard, previously untested because the live path never populated it.
    #[test]
    fn check_proposal_replay_rejects_repo_head_drift() {
        let proposal = sample_proposal();
        let mut current = CurrentGroundStateV1 {
            issue_snapshot_hash: proposal.based_on_issue_snapshot_hash.clone(),
            repo_revisions: proposal.based_on_repo_revisions.clone(),
            doc_revisions: proposal.based_on_doc_revisions.clone(),
        };
        current.repo_revisions[0].commit_sha = "a-new-head-sha".to_string();
        let err = check_proposal_replay(&proposal, &current)
            .expect_err("repo HEAD drift must reject replay");
        assert!(err
            .iter()
            .any(|r| matches!(r, StalenessReasonV1::RepoRevisionChanged { .. })));
    }

    /// R4-2 (build-seat REQUEST-CHANGES): an empty `based_on_repo_revisions`
    /// axis must not vacuously pass replay — it means the repo pin was
    /// never established, not that it's confirmed unchanged.
    #[test]
    fn check_proposal_replay_rejects_a_proposal_with_no_repo_revision_pin_at_all() {
        let mut proposal = sample_proposal();
        proposal.based_on_repo_revisions = Vec::new();
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

    /// R4-4 (build-seat REQUEST-CHANGES): the hash basis is the WHOLE
    /// proposal struct (minus `proposal_hash` itself), so changing ANY
    /// field — including ones the old hand-picked field list silently
    /// missed, like `proposed_comment` — must change the hash.
    #[test]
    fn compute_proposal_hash_changes_when_proposed_comment_changes() {
        let mut proposal = sample_proposal();
        proposal.proposed_comment = Some("first comment".to_string());
        let hash1 = proposal
            .compute_proposal_hash()
            .expect("compute_proposal_hash");
        proposal.proposed_comment = Some("a completely different comment".to_string());
        let hash2 = proposal
            .compute_proposal_hash()
            .expect("compute_proposal_hash");
        assert_ne!(hash1, hash2);
    }

    fn sample_proposal() -> IssueDispositionProposalV1 {
        IssueDispositionProposalV1 {
            packet_id: "issue-refinery/owner/repo#1/snap1".to_string(),
            proposal_hash: "proposalhash".to_string(),
            source_bundle_hash: "bundlehash".to_string(),
            issue_ref: "owner/repo#1".to_string(),
            based_on_issue_snapshot_hash: "snap1".to_string(),
            based_on_repo_revisions: vec![RepoRevisionV1 {
                repo: "owner/repo".to_string(),
                git_ref: "main".to_string(),
                commit_sha: "sha1".to_string(),
                verified_at: "2026-07-13T00:00:00Z".to_string(),
            }],
            based_on_doc_revisions: vec![CanonicalDocRefV1 {
                repo: "owner/repo".to_string(),
                trusted_ref: "origin/main".to_string(),
                commit_sha: "sha1".to_string(),
                path: "docs/x.md".to_string(),
                blob_sha: "blob1".to_string(),
                section: "1".to_string(),
                authority_receipt: "fixture:sample".to_string(),
                verified_reachable_at: "2026-07-13T00:00:00Z".to_string(),
            }],
            grounding_status: GroundingStatusV1::Grounded,
            disposition: DispositionV1::Keep,
            evidence_refs: vec![],
            contradictions: vec![],
            proposed_comment: None,
            proposed_labels: vec![],
            proposed_doc_deltas: vec![],
            preview_only: true,
            engine_receipt: None,
        }
    }
}
