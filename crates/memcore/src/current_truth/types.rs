//! Typed vocabulary for the #1297 current-truth fold.
//!
//! Everything here is plain data with a deterministic canonical ordering.
//! See the module doc on `crate::current_truth` for the redlines these types
//! are shaped to hold.

use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};

use crate::types::AuthorityLevel;

/// `tachi_events.event_type` carried by every truth assertion.
///
/// Deliberately matches none of the prefixes in `tachi-server`'s
/// `continuity_ops::projection::entry::inferred_projection_from_event_type`
/// (`pattern.` / `timeline.` / `bonding.` / `affect.` / `emotion.` /
/// `world_book.` / `worldbook.` / `lorebook.` / `project_cycle.` /
/// `domain_profile.` / `outcome.` / `evidence_gate.` and the exact matches
/// `session.captured` / `wiki.saved` / `task.outcome` / `subagent.evaluated`
/// / `session.outcome`). Combined with the empty `projection_hints` that
/// [`super::build_truth_assertion_event`] enforces, that is what keeps
/// reducer input out of the `memories` table (redline 1).
pub const TRUTH_ASSERTION_EVENT_TYPE: &str = "truth.assertion.v1";

/// `tachi_events.domain` carried by every truth assertion, so a reader can
/// enumerate them with one indexed `(project, domain)` query.
pub const TRUTH_ASSERTION_DOMAIN: &str = "current_truth";

/// Payload discriminator. A payload without exactly this version string is
/// refused rather than best-effort parsed.
pub const TRUTH_ASSERTION_PAYLOAD_VERSION: &str = "truth_assertion_v1";

/// Version tag stamped on every [`CurrentTruthProjectionV1`].
pub const CURRENT_TRUTH_PROJECTION_VERSION: &str = "current_truth_projection_v1";

/// Canonical affirmative value for boolean-shaped predicates.
pub const VALUE_YES: &str = "yes";
/// Canonical negative value for boolean-shaped predicates.
pub const VALUE_NO: &str = "no";
/// Canonical `issue_state` value: the owning system reports the issue open.
pub const VALUE_ISSUE_OPEN: &str = "open";
/// Canonical `issue_state` value: the owning system reports the issue closed.
pub const VALUE_ISSUE_CLOSED: &str = "closed";

/// Errors this module returns instead of panicking or silently degrading.
///
/// Kept local rather than folded into [`crate::error::MemoryError`]: nothing
/// here touches storage, so a storage error type would be a lie about where
/// failures come from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CurrentTruthError {
    /// The `as_of` argument was not parseable RFC3339.
    ///
    /// Fail direction is deliberate: an unparseable `as_of` refuses the whole
    /// projection rather than silently falling back to "now" or to "no time
    /// filter". A reducer that quietly widened its own window would be the
    /// staleness bug this issue exists to kill.
    InvalidAsOf(String),
    /// An assertion could not be built or decoded. Carries the reason.
    InvalidAssertion(String),
    /// Canonical JSON rendering failed.
    Serialization(String),
}

impl std::fmt::Display for CurrentTruthError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidAsOf(raw) => {
                write!(f, "current-truth as_of is not RFC3339: {raw}")
            }
            Self::InvalidAssertion(reason) => write!(f, "invalid truth assertion: {reason}"),
            Self::Serialization(reason) => {
                write!(f, "current-truth serialization failed: {reason}")
            }
        }
    }
}

impl std::error::Error for CurrentTruthError {}

// ─── Predicates ─────────────────────────────────────────────────────────────

/// The `(subject_ref, predicate)` space this fold reduces over.
///
/// Declaration order **is** the canonical output order — [`TruthPredicate`]
/// derives `Ord` from it and [`Self::ALL`] repeats it. Do not reorder without
/// accepting that every stored canonical projection JSON changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TruthPredicate {
    /// Lifecycle state the owning issue tracker reports: `open` | `closed`.
    ///
    /// Load-bearing negative: `issue_state = open` does **not** mean
    /// "implementation missing" — #1288/#1290 and #1289/#1292 both merged
    /// while their issues stayed open.
    IssueState,
    /// `yes` | `no` — whether the subject is owner-close-protected, i.e. an
    /// agent must never close it. Read off the owning system's labels, so it
    /// is a source observation, not a judgement.
    OwnerProtected,
    /// What implemented the subject: a PR ref or commit SHA, or [`VALUE_NO`].
    ImplementedBy,
    /// The merge commit SHA, or [`VALUE_NO`] when the source reports the
    /// subject as not merged. A revert is expressed as a *later observation*
    /// carrying a different value — never by rewriting the merge event.
    Merged,
    /// A named owner accepted the work: [`VALUE_YES`] | [`VALUE_NO`].
    Accepted,
    /// A named owner closed the subject: [`VALUE_YES`] | [`VALUE_NO`].
    ///
    /// `unknown` here is the point of the whole exercise: merged PR +
    /// owner-protected issue + no owner-close event ⇒ `owner_closed =
    /// unknown`, **not** "done".
    OwnerClosed,
    /// Host-scoped deployment. The host belongs in `subject_ref` (deployment
    /// truth is host-scoped: one adapter/binary/schema receipt cannot
    /// establish deployment on another machine). Receipt *construction* is
    /// out of leaf 1 — see the module doc.
    Deployed,
    /// The source revision a handoff pinned its evidence to. Compared against
    /// the subject's reconciled observations to detect a stale handoff.
    ///
    /// The handoff's prose is agent-authored and stays a candidate forever;
    /// this predicate carries only the typed pointer to the source revision
    /// the handoff cited, which is why it takes a source-snapshot issuer.
    HandoffEvidenceHead,
}

impl TruthPredicate {
    /// Canonical order. Every subject in a projection emits every predicate,
    /// in exactly this order, so `unknown` is always explicitly present
    /// rather than inferred from an absent key.
    pub const ALL: [Self; 8] = [
        Self::IssueState,
        Self::OwnerProtected,
        Self::ImplementedBy,
        Self::Merged,
        Self::Accepted,
        Self::OwnerClosed,
        Self::Deployed,
        Self::HandoffEvidenceHead,
    ];

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::IssueState => "issue_state",
            Self::OwnerProtected => "owner_protected",
            Self::ImplementedBy => "implemented_by",
            Self::Merged => "merged",
            Self::Accepted => "accepted",
            Self::OwnerClosed => "owner_closed",
            Self::Deployed => "deployed",
            Self::HandoffEvidenceHead => "handoff_evidence_head",
        }
    }

    /// Strict parse. Unknown predicate strings are **refused**, not mapped to
    /// a default — an unrecognised predicate is surfaced as a rejection with
    /// a reason rather than silently folded into some other predicate.
    pub fn from_str_opt(s: Option<&str>) -> Option<Self> {
        match s.unwrap_or("").trim() {
            "issue_state" => Some(Self::IssueState),
            "owner_protected" => Some(Self::OwnerProtected),
            "implemented_by" => Some(Self::ImplementedBy),
            "merged" => Some(Self::Merged),
            "accepted" => Some(Self::Accepted),
            "owner_closed" => Some(Self::OwnerClosed),
            "deployed" => Some(Self::Deployed),
            "handoff_evidence_head" => Some(Self::HandoffEvidenceHead),
            _ => None,
        }
    }

    /// Which issuer class may produce an *admitted* assertion of this
    /// predicate. Anything else is a candidate.
    pub fn required_issuer(&self) -> IssuerClass {
        match self {
            Self::IssueState
            | Self::OwnerProtected
            | Self::ImplementedBy
            | Self::Merged
            | Self::HandoffEvidenceHead => IssuerClass::SourceSnapshot,
            Self::Accepted | Self::OwnerClosed => IssuerClass::OwnerDecision,
            Self::Deployed => IssuerClass::DeploymentReceipt,
        }
    }

    /// Whether a later observation of this predicate *replaces* an earlier
    /// one (a transition) instead of conflicting with it.
    ///
    /// This is the one place recency is allowed to decide anything, and it is
    /// scoped on purpose:
    ///
    /// - **True** for predicates owned by an external system that emits no
    ///   supersession edge of its own. `open → closed → reopened` is a
    ///   transition, and a revert is a later `merged` observation. The system
    ///   of record has exactly one live answer; the latest observation of it
    ///   is that answer.
    /// - **False** for decision predicates ([`Self::Accepted`],
    ///   [`Self::OwnerClosed`]). A decision is revised by an *explicit*
    ///   [`AssertionRelationV1::Supersedes`] or
    ///   [`AssertionRelationV1::Retracts`], never by a newer decision quietly
    ///   out-ranking an older one. Two undirected decisions with different
    ///   values are a [`TruthStateV1::Conflicted`] — "conflict is an output
    ///   state, not a tiebreak to spend recency on".
    pub fn revises_by_observation_recency(&self) -> bool {
        match self {
            Self::IssueState
            | Self::OwnerProtected
            | Self::ImplementedBy
            | Self::Merged
            | Self::Deployed
            | Self::HandoffEvidenceHead => true,
            Self::Accepted | Self::OwnerClosed => false,
        }
    }

    /// Closed value vocabulary, where one exists. `None` means "any non-empty
    /// token" (SHAs, PR refs and revision ids have no closed vocabulary).
    pub fn allowed_values(&self) -> Option<&'static [&'static str]> {
        match self {
            Self::IssueState => Some(&[VALUE_ISSUE_OPEN, VALUE_ISSUE_CLOSED]),
            Self::OwnerProtected | Self::Accepted | Self::OwnerClosed => {
                Some(&[VALUE_YES, VALUE_NO])
            }
            Self::ImplementedBy | Self::Merged | Self::Deployed | Self::HandoffEvidenceHead => None,
        }
    }
}

impl std::fmt::Display for TruthPredicate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

// ─── Issuers ────────────────────────────────────────────────────────────────

/// Coarse issuer class used by the admission policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IssuerClass {
    SourceSnapshot,
    OwnerDecision,
    DeploymentReceipt,
    Agent,
}

impl IssuerClass {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::SourceSnapshot => "source_snapshot",
            Self::OwnerDecision => "owner_decision",
            Self::DeploymentReceipt => "deployment_receipt",
            Self::Agent => "agent",
        }
    }
}

impl std::fmt::Display for IssuerClass {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Who produced an assertion, with the typed evidence its class requires.
///
/// The variants are the *only* admissible origins. "A model agreed" and "the
/// text says so" are deliberately absent — the closest thing is
/// [`Self::Agent`], which can never be admitted.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "class", rename_all = "snake_case")]
pub enum TruthIssuerV1 {
    /// A typed snapshot read directly out of the owning system.
    SourceSnapshot {
        /// Owning system, e.g. `github`.
        system: String,
        /// Digest of the snapshot the assertion was read from.
        snapshot_digest: String,
    },
    /// A named human owner's decision.
    OwnerDecision { owner_login: String },
    /// A host-scoped deployment receipt. Receipt construction is out of leaf 1.
    DeploymentReceipt {
        host: String,
        service: String,
        receipt_hash: String,
    },
    /// Anything authored by an agent or model, however confident, however
    /// well cited. Never admitted; see redline 3.
    Agent { agent: String },
}

impl TruthIssuerV1 {
    pub fn class(&self) -> IssuerClass {
        match self {
            Self::SourceSnapshot { .. } => IssuerClass::SourceSnapshot,
            Self::OwnerDecision { .. } => IssuerClass::OwnerDecision,
            Self::DeploymentReceipt { .. } => IssuerClass::DeploymentReceipt,
            Self::Agent { .. } => IssuerClass::Agent,
        }
    }

    /// Every issuer class carries required evidence fields. An issuer that
    /// cannot name its evidence is refused outright rather than downgraded,
    /// because a downgrade would leave a shapeless claim in the ledger that
    /// later looks like a real observation.
    pub fn validate(&self) -> Result<(), String> {
        let missing = |field: &str| Err(format!("issuer {} missing {field}", self.class()));
        match self {
            Self::SourceSnapshot {
                system,
                snapshot_digest,
            } => {
                if system.trim().is_empty() {
                    return missing("system");
                }
                if snapshot_digest.trim().is_empty() {
                    return missing("snapshot_digest");
                }
                Ok(())
            }
            Self::OwnerDecision { owner_login } => {
                if owner_login.trim().is_empty() {
                    return missing("owner_login");
                }
                Ok(())
            }
            Self::DeploymentReceipt {
                host,
                service,
                receipt_hash,
            } => {
                if host.trim().is_empty() {
                    return missing("host");
                }
                if service.trim().is_empty() {
                    return missing("service");
                }
                if receipt_hash.trim().is_empty() {
                    return missing("receipt_hash");
                }
                Ok(())
            }
            Self::Agent { agent } => {
                if agent.trim().is_empty() {
                    return missing("agent");
                }
                Ok(())
            }
        }
    }
}

// ─── Values and relations ───────────────────────────────────────────────────

/// A canonicalised predicate value. Trimmed, never empty.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TruthValue(String);

impl TruthValue {
    pub fn new(raw: &str) -> Result<Self, String> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Err("truth value is empty".to_string());
        }
        Ok(Self(trimmed.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for TruthValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// How an assertion relates to an earlier one.
#[derive(Debug, Clone, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AssertionRelationV1 {
    /// Asserts a value on its own behalf.
    #[default]
    Standalone,
    /// Displaces the target and asserts a replacement value. The target
    /// contributes to [`PredicateTruthV1::superseded`]; it stays in the
    /// ledger untouched.
    Supersedes { target_assertion_id: String },
    /// Withdraws the target. The target leaves the working set entirely and
    /// contributes to [`PredicateTruthV1::retracted`]; history keeps it. A
    /// retraction carries no value of its own.
    Retracts { target_assertion_id: String },
}

impl AssertionRelationV1 {
    pub fn target_assertion_id(&self) -> Option<&str> {
        match self {
            Self::Standalone => None,
            Self::Supersedes {
                target_assertion_id,
            }
            | Self::Retracts {
                target_assertion_id,
            } => Some(target_assertion_id.as_str()),
        }
    }

    /// Stable token used in the append-if-absent identity key.
    pub fn key_token(&self) -> String {
        match self {
            Self::Standalone => "standalone".to_string(),
            Self::Supersedes {
                target_assertion_id,
            } => format!("supersedes:{target_assertion_id}"),
            Self::Retracts {
                target_assertion_id,
            } => format!("retracts:{target_assertion_id}"),
        }
    }
}

// ─── Assertions ─────────────────────────────────────────────────────────────

/// A decoded assertion: the event's typed payload plus the two fields that
/// come from the *event row* and never from the payload — `assertion_id`
/// (the ledger primary key) and `authority` (the `tachi_events.authority`
/// column). A payload cannot declare its own identity or its own authority.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TruthAssertionV1 {
    pub assertion_id: String,
    pub subject_ref: String,
    pub predicate: TruthPredicate,
    /// `None` only for [`AssertionRelationV1::Retracts`], which asserts no
    /// value. Enforced at decode.
    pub value: Option<TruthValue>,
    pub issuer: TruthIssuerV1,
    /// Opaque identity of the observed source revision.
    ///
    /// **Not** an ordering key. The only producer in this repo
    /// (`tachi-server`'s `compile_current_work_anchor`) sets it to a SHA-256
    /// of the semantic revision basis, so "highest source_revision" would be
    /// ordering on a content hash. Recency comes from [`Self::observed_at`].
    pub source_revision: String,
    /// The owning system's own revision clock for this observation (e.g. a
    /// GitHub `updatedAt`), normalised to UTC RFC3339 with nanosecond
    /// precision and a `Z` suffix.
    ///
    /// This must be the *source's* instant, never the fetch wall clock: the
    /// append-if-absent identity compares full event content, so a re-fetch
    /// of an unchanged object has to produce a byte-identical event.
    pub observed_at: String,
    /// Immutable pointer to what was observed. Required — an assertion that
    /// cannot name its evidence is exactly the unsupported claim this issue
    /// exists to refuse.
    pub evidence_ref: String,
    #[serde(default)]
    pub relation: AssertionRelationV1,
    /// Copied from `tachi_events.authority`.
    pub authority: AuthorityLevel,
}

impl TruthAssertionV1 {
    pub fn as_ref_v1(&self) -> AssertionRefV1 {
        AssertionRefV1 {
            assertion_id: self.assertion_id.clone(),
            value: self.value.clone(),
            observed_at: self.observed_at.clone(),
            source_revision: self.source_revision.clone(),
            evidence_ref: self.evidence_ref.clone(),
            issuer_class: self.issuer.class(),
            authority: self.authority,
        }
    }
}

/// The evidence head a consumer needs to re-verify a value itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssertionRefV1 {
    pub assertion_id: String,
    pub value: Option<TruthValue>,
    pub observed_at: String,
    pub source_revision: String,
    pub evidence_ref: String,
    pub issuer_class: IssuerClass,
    pub authority: AuthorityLevel,
}

/// Total, deterministic order for every emitted assertion list.
/// `assertion_id` is the ledger primary key, so this is a total order even
/// when two observations share an instant.
///
/// The `observed_at` leg is a **string** compare, and that is only correct
/// because of an invariant established one layer down: `decode_truth_assertion`
/// runs every `observed_at` through [`parse_instant`] + [`normalize_instant`],
/// so every value reaching here is UTC, nanosecond precision, `Z`-suffixed —
/// one spelling per instant, where lexicographic order *is* chronological
/// order. Feeding this function refs that skipped decode would silently break
/// that. Anywhere the comparison must hold without the invariant (the action
/// queue's staleness test), the instants are re-parsed instead.
pub(super) fn sort_refs(refs: &mut [AssertionRefV1]) {
    refs.sort_by(|a, b| {
        a.observed_at
            .cmp(&b.observed_at)
            .then_with(|| a.assertion_id.cmp(&b.assertion_id))
    });
}

// ─── Projection ─────────────────────────────────────────────────────────────

/// Reduced state of one `(subject_ref, predicate)` pair.
///
/// Note what is **not** here: there is no fourth `Superseded` variant. The
/// adjudicated conflict rule makes supersession an *assertion*-level
/// disposition (it "contributes `superseded`") and leaves the predicate-level
/// remainder at exactly three outcomes — empty ⇒ `unknown`, one distinct
/// value ⇒ `current`, two or more ⇒ `conflicted`. The superseded assertions
/// are still emitted, on [`PredicateTruthV1::superseded`], so "we heard and
/// it was withdrawn" stays distinguishable from "we never heard".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum TruthStateV1 {
    /// Nothing admitted survives for this predicate. Load-bearing, not
    /// decoration — `unknown` must never render as "done".
    Unknown,
    Current {
        value: TruthValue,
        /// Every admitted assertion agreeing on `value`.
        evidence: Vec<AssertionRefV1>,
    },
    /// Two or more distinct values at equal admitted authority.
    ///
    /// There is deliberately **no** winner field on this variant: the shape
    /// of the type makes "the reducer picked one anyway" unrepresentable.
    Conflicted { members: Vec<AssertionRefV1> },
}

impl TruthStateV1 {
    pub fn is_unknown(&self) -> bool {
        matches!(self, Self::Unknown)
    }

    pub fn is_conflicted(&self) -> bool {
        matches!(self, Self::Conflicted { .. })
    }

    pub fn current_value(&self) -> Option<&TruthValue> {
        match self {
            Self::Current { value, .. } => Some(value),
            _ => None,
        }
    }

    /// Latest `observed_at` among the assertions backing this state, if any.
    ///
    /// Returns the normalised text, and takes the max lexicographically —
    /// correct only under the same one-spelling-per-instant invariant
    /// [`sort_refs`] documents. Callers that need to *compare* two instants
    /// should parse rather than lean on this.
    pub fn latest_observed_at(&self) -> Option<&str> {
        let refs = match self {
            Self::Unknown => return None,
            Self::Current { evidence, .. } => evidence,
            Self::Conflicted { members } => members,
        };
        refs.iter().map(|r| r.observed_at.as_str()).max()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PredicateTruthV1 {
    pub predicate: TruthPredicate,
    pub state: TruthStateV1,
    /// Admitted assertions displaced by a targeted
    /// [`AssertionRelationV1::Supersedes`]. History, not truth.
    pub superseded: Vec<AssertionRefV1>,
    /// Admitted assertions withdrawn by a [`AssertionRelationV1::Retracts`].
    /// History, not truth.
    pub retracted: Vec<AssertionRefV1>,
    /// Admitted assertions that lost the recency stratum of a
    /// [`TruthPredicate::revises_by_observation_recency`] predicate — e.g.
    /// the merge observation that a later revert observation replaced. The
    /// ledger event behind each of these is untouched.
    pub displaced: Vec<AssertionRefV1>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubjectTruthV1 {
    pub subject_ref: String,
    /// Always all of [`TruthPredicate::ALL`], in canonical order, so
    /// `unknown` is explicit rather than inferred from a missing key.
    pub predicates: Vec<PredicateTruthV1>,
}

impl SubjectTruthV1 {
    pub fn predicate(&self, predicate: TruthPredicate) -> Option<&PredicateTruthV1> {
        self.predicates.iter().find(|p| p.predicate == predicate)
    }

    pub fn state(&self, predicate: TruthPredicate) -> Option<&TruthStateV1> {
        self.predicate(predicate).map(|p| &p.state)
    }
}

/// Why a well-formed assertion did not enter reduction.
///
/// A candidate is not an error. It is a decodable, evidence-carrying claim
/// that the admission policy will not let decide anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CandidateReasonV1 {
    /// Agent- or model-authored. Checked first and unconditionally — no
    /// authority level, citation, or predicate can lift it (redline 3).
    AgentAuthored,
    /// The issuer class is not the one this predicate's policy requires,
    /// e.g. an owner decision claiming a merge SHA.
    IssuerNotAuthoritativeForPredicate,
    /// `tachi_events.authority` is not decision-eligible per
    /// [`AuthorityLevel::is_decision_eligible`].
    AuthorityNotDecisionEligible,
    /// `observed_at` is later than the projection's `as_of`. A future
    /// observation is real, just not yet true at the requested instant.
    ObservedAfterAsOf,
    /// Defensive: `observed_at` did not re-parse at projection time. Decode
    /// normalises it, so reaching this means the fold state was hand-built or
    /// deserialized from something that skipped decode.
    UnparsableObservedAt,
}

impl CandidateReasonV1 {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::AgentAuthored => "agent_authored",
            Self::IssuerNotAuthoritativeForPredicate => "issuer_not_authoritative_for_predicate",
            Self::AuthorityNotDecisionEligible => "authority_not_decision_eligible",
            Self::ObservedAfterAsOf => "observed_after_as_of",
            Self::UnparsableObservedAt => "unparsable_observed_at",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CandidateRecordV1 {
    pub assertion: TruthAssertionV1,
    pub reason: CandidateReasonV1,
}

/// A ledger event of the right type that could not be decoded at all.
///
/// Refusals are emitted, never swallowed: a silent drop here would read
/// downstream as "there was nothing to say about this subject".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RejectedAssertionV1 {
    pub event_id: String,
    pub reason: String,
}

/// Everything the fold saw but did not let decide anything.
///
/// Structurally separate from [`CurrentTruthProjectionV1::subjects`]: an
/// agent-authored event can appear here and *only* here, so it cannot change
/// a value and cannot introduce a subject into the truth surface.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CurrentTruthDiagnosticsV1 {
    pub candidates: Vec<CandidateRecordV1>,
    pub rejected: Vec<RejectedAssertionV1>,
    /// Ledger events of some other type (the global ledger is shared with
    /// `wiki.saved`, `task.outcome`, `smoke.test`, `memory.saved`, …).
    pub ignored_event_ids: Vec<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CurrentTruthStatsV1 {
    pub subjects: usize,
    pub admitted_assertions: usize,
    pub candidate_assertions: usize,
    pub rejected_assertions: usize,
    pub ignored_events: usize,
    pub conflicted_predicates: usize,
    /// The number #1297 asks the reducer to report: how many subjects are
    /// `unknown` on `owner_closed`.
    pub unknown_owner_closed_subjects: usize,
}

/// A rebuildable read model. Never persisted, never written back as memory
/// rows, never consulted by ranking, save policy, or GC.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CurrentTruthProjectionV1 {
    pub projection_version: String,
    /// Normalised UTC RFC3339 of the instant this projection is true at.
    pub as_of: String,
    /// Truth surface. Sorted by `subject_ref`; every entry carries every
    /// predicate. Built only from admitted assertions.
    pub subjects: Vec<SubjectTruthV1>,
    pub diagnostics: CurrentTruthDiagnosticsV1,
    pub stats: CurrentTruthStatsV1,
}

impl CurrentTruthProjectionV1 {
    pub fn subject(&self, subject_ref: &str) -> Option<&SubjectTruthV1> {
        self.subjects.iter().find(|s| s.subject_ref == subject_ref)
    }

    /// Canonical JSON.
    ///
    /// Deterministic by construction: struct fields serialise in declaration
    /// order, every emitted `Vec` is sorted by a total key, and `serde_json`
    /// is built without `preserve_order` so any map it does build is a
    /// `BTreeMap`. Byte-identical for identical input in any arrival order.
    pub fn canonical_json(&self) -> Result<String, CurrentTruthError> {
        serde_json::to_string(self).map_err(|e| CurrentTruthError::Serialization(e.to_string()))
    }

    /// Canonical JSON of the truth surface alone, excluding diagnostics and
    /// stats. This is the surface a candidate assertion must never move.
    pub fn canonical_subjects_json(&self) -> Result<String, CurrentTruthError> {
        serde_json::to_string(&self.subjects)
            .map_err(|e| CurrentTruthError::Serialization(e.to_string()))
    }
}

// ─── Time ───────────────────────────────────────────────────────────────────

/// Parse an RFC3339 instant.
///
/// Timestamps are compared as **instants**, never as strings: lexicographic
/// comparison of RFC3339 text is only correct when offset and sub-second
/// precision happen to match, and this ledger's producers do not guarantee
/// either. Fail direction is explicit at every call site — decode rejects,
/// projection demotes to a candidate, and the action queue withholds the item.
pub(super) fn parse_instant(raw: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(raw.trim())
        .ok()
        .map(|dt| dt.with_timezone(&Utc))
}

/// One spelling for one instant: UTC, nanosecond precision, `Z` suffix.
///
/// Normalising on the way in is what lets a re-fetch of an unchanged object
/// produce a byte-identical event, and what lets canonical JSON be stable
/// regardless of how a producer spelled its offset.
pub(super) fn normalize_instant(instant: DateTime<Utc>) -> String {
    instant.to_rfc3339_opts(SecondsFormat::Nanos, true)
}
