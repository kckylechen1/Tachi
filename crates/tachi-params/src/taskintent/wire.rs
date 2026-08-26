//! `TaskIntentV1` — the frozen host semantic wire (tachi#1840, zeroclaw #205
//! TB-3). This module owns the Tachi-side DECODER half of the
//! `task-intent.v1` golden; the ZeroClaw encoder half and the cross-repo
//! round trip belong to V2b (zeroclaw #234).
//!
//! Field freeze (exactly these seventeen top-level fields, TB-3):
//!
//! ```text
//! objective, capability_request, requester, parent_ref, supervisor_ref,
//! context_bundle_ref, source_refs, constraints, expected_artifacts,
//! evaluation_requirement, workspace_source, routing_preference,
//! approval_requirement, privacy_class, expiry, retry_of
//! ```
//! (plus `schema` — the version tag, see [`SCHEMA_TAG`]).
//!
//! **The schema admits no execution detail under any name or nesting**
//! (TB-1/TB-4): no `command`/`env`/`cwd`/`path`/`model`/`backend`-shaped
//! field exists at any depth. Every text-bearing value is a bounded,
//! content-scanned [`BoundedText`] (see [`super::admission`]); every
//! selector is a closed enum. `workspace_source` is a typed
//! repo/revision-selector, never a caller filesystem path.
//!
//! Digest: [`TaskIntentV1::canonical_digest`] reuses
//! `memcore::canonical_digest::canonical_json_digest_hex` — the workspace's
//! single canonical-JSON digest rule — over the wire serialization prefixed
//! with the schema tag. The same rule must be implemented by the V2b encoder
//! (the golden pins a sample digest).
//!
//! DECISION (OPEN) TB-5/A carrier record: [`Capability`] is a **closed Rust
//! enum** (option (a), the least-authority reading — contract rev 3 line 7:
//! open decisions "default closed/deny until picked"). A runtime-admitted
//! `CapabilityId` catalog (option (b)) remains available to the owner; the
//! golden pins whichever variant set ships, so an owner flip is a
//! deliberate golden-changing PR, never a silent drift.
//!
//! Owner override exercised once (2026-08-26, zeroclaw #234 rows 1–2): the
//! owner's ratified V-program text names `capability_request =
//! repository_implementation` as THE acceptance capability for the watershed
//! vertical, which is the surfaced owner override TB-5/A reserved — the
//! third variant below was added by that ratification. The carrier stays
//! the closed enum (option (b) remains unflipped); the golden example keeps
//! `reasoning_review`, so the cross-repo pinned digest did NOT move.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::refs::{ParentRunRef, RequesterRef, SubAgentRunRef, TaskRef};

/// Version tag carried on every `TaskIntentV1` wire payload.
pub const SCHEMA_TAG: &str = "task-intent.v1";

/// Hard cap for any single text-bearing wire value (TB-4: no unbounded
/// transcript can be represented on this wire at all).
pub const BOUNDED_TEXT_MAX: usize = 4_096;

/// A bounded, content-scanned text value. Construction validates length;
/// admission (TB-4) additionally scans content per forbidden category.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct BoundedText(String);

impl BoundedText {
    /// Construct a bounded text, rejecting oversize values.
    pub fn new(value: impl Into<String>) -> Result<Self, WireError> {
        let value = value.into();
        if value.len() > BOUNDED_TEXT_MAX {
            return Err(WireError::TextTooLong { len: value.len() });
        }
        Ok(Self(value))
    }

    /// The text content.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<BoundedText> for String {
    fn from(value: BoundedText) -> Self {
        value.0
    }
}

/// RFC3339 timestamp on the wire (e.g. task expiry).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Timestamp(DateTime<Utc>);

impl Timestamp {
    /// Parse an RFC3339 timestamp.
    pub fn parse(value: &str) -> Result<Self, WireError> {
        Ok(Self(
            DateTime::parse_from_rfc3339(value)
                .map_err(WireError::BadTimestamp)?
                .with_timezone(&Utc),
        ))
    }
}

impl From<Timestamp> for String {
    fn from(value: Timestamp) -> Self {
        value.0.to_rfc3339()
    }
}

/// DECISION (OPEN) TB-5/A — carrier picked as (a) closed enum, least
/// authority: no variant can name a vendor, CLI, model, or tool, so
/// vendor/CLI-shaped `capability_request`s fail admission **structurally**
/// (they cannot be represented). The requester-bounded law (TB-5: the
/// requested capability must already be permitted by the requester's own
/// admitted profile) is enforced at admission against
/// [`super::RequesterAuthorityPort`], never against guidance content.
///
/// Variant-set note: the third variant is the owner's surfaced TB-5/A
/// override (2026-08-26) — see the module docs. Adding a variant is a
/// deliberate, golden-visible extension, never silent drift.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    /// Bounded reasoning / review producing a structured report (the V1
    /// ReasoningSubAgent vertical's successor capability).
    ReasoningReview,
    /// Read-only investigation over admitted repos, docs, and issues. Cannot
    /// express write authority: write-class work is representable on this
    /// wire ONLY through the owner-ratified [`Capability::RepositoryImplementation`]
    /// variant.
    ReadOnlyInvestigation,
    /// Repository implementation: write-class work producing repository
    /// changes (source diffs, verification evidence) under the ordinary
    /// dispatch/eval/adjudication spines. Added by the owner's surfaced
    /// TB-5/A override — the ratified V-program text names
    /// `repository_implementation` as THE acceptance capability for the
    /// watershed vertical (zeroclaw #234 rows 1–2). Placement, credentials,
    /// and sandboxing remain Tachi admission territory; this variant still
    /// cannot name a vendor, CLI, model, or tool.
    RepositoryImplementation,
}

/// The capability an intent requests (TB-5). One capability per intent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapabilityRequest {
    /// The requested capability (closed enum, TB-5/A option (a)).
    pub capability: Capability,
}

/// Where a task's source material lives (TB-3 `source_refs`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceRef {
    /// Kind of source (closed set).
    pub kind: SourceKind,
    /// Locator text (e.g. `owner/repo#123`). Bounded and content-scanned;
    /// never a filesystem path or a command.
    pub locator: BoundedText,
}

/// Closed source-kind vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    /// A tracked issue.
    Issue,
    /// A pull request.
    PullRequest,
    /// A repository at large.
    Repository,
    /// An admitted document.
    Document,
}

/// A semantic constraint on the work (TB-3 `constraints`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskConstraint {
    /// Human-readable constraint statement. Content-scanned (TB-4).
    pub description: BoundedText,
}

/// What artifact the requester expects (TB-3 `expected_artifacts`; drives
/// the TB-13 "success without required artifact is not contract success"
/// check).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactExpectation {
    /// Closed artifact class, e.g. `report`, `diff`, `verification_log`.
    /// Deliberately not a path: naming an output path would be execution
    /// detail (TB-1).
    pub artifact_class: ArtifactClass,
    /// Bounded description of what satisfies this expectation.
    pub description: BoundedText,
    /// Whether absence of this artifact fails the evaluation contract.
    pub required: bool,
}

/// Closed artifact-class vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactClass {
    /// A written report (the ReasoningReview output shape).
    Report,
    /// A source diff.
    Diff,
    /// Evidence that verification ran (tests/checks).
    VerificationLog,
}

/// Evaluation independence requirement (TB-3 `evaluation_requirement`;
/// classes frozen by TB-17).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvaluationRequirement {
    /// Required independence class for the evaluation of this task's result.
    pub independence: IndependenceClass,
}

/// TB-17 independence classes. `SameSessionContinuation` can never satisfy
/// an independent-review requirement (see
/// [`super::mapping::adjudication`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IndependenceClass {
    /// Deterministic mechanical check.
    DeterministicCheck,
    /// Continuation inside the same session — never independent review.
    SameSessionContinuation,
    /// Fresh context, same harness.
    FreshContextSameHarness,
    /// Fresh context, different model, same vendor.
    FreshContextCrossModelSameVendor,
    /// Fresh context, different vendor.
    FreshContextCrossVendor,
    /// Human review.
    HumanReview,
}

/// Typed workspace selector (TB-3 `workspace_source`): a repo and revision
/// the requester points at. This is a **selector over Tachi-admitted
/// workspace truth** (tachi ExecEnv/Workspace plane owns placement); a
/// caller-selected worktree path as execution authority is forbidden wire
/// content (TB-4) and is not representable here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceSourceRef {
    /// Repository identity (e.g. `owner/name`). Bounded, content-scanned.
    pub repo: BoundedText,
    /// Optional git revision selector (branch, tag, or commit).
    pub git_ref: Option<BoundedText>,
}

/// Typed routing preference (TB-5: preference only — never grants placement,
/// credentials, data egress, safety exceptions, or lifecycle authority).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoutingPreference {
    /// No preference; Tachi staffing chooses freely (default posture).
    NoPreference,
    /// Prefer the Tachi-managed batch lane (tachi#1676).
    PreferTachiManaged,
    /// Prefer a harness-native attached session (tachi#1678).
    PreferHarnessNative,
}

/// Typed approval requirement asserted by the requester (TB-3). This is an
/// assertion of what approval the requester believes applies — actual
/// approval authority is resolved by Tachi admission against policy, never
/// granted by this field (TB-4 seam law: intent fields are not authority).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalRequirement {
    /// Requester asserts no explicit approval gate is required for this
    /// intent. Admission may still impose one from policy.
    NotRequired,
    /// Requester asserts explicit human approval is required before launch.
    RequireExplicitApproval,
}

/// Privacy class of the intent content (TB-3). Private-Dyad-labeled content
/// is forbidden on this wire entirely (TB-4); `Confidential` is the most
/// sensitive class that may be represented.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrivacyClass {
    /// Public-safe content.
    Public,
    /// Internal content.
    Internal,
    /// Confidential content; strictest visibility/redaction handling short
    /// of the forbidden Private-Dyad class.
    Confidential,
}

/// The frozen host semantic wire (TB-3). Exactly the fields below; see the
/// module docs for the freeze list and the golden test that pins it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskIntentV1 {
    /// Schema version tag (`task-intent.v1`).
    pub schema: String,
    /// What the requester wants accomplished. Bounded, content-scanned.
    pub objective: BoundedText,
    /// The capability being requested (closed enum; TB-5).
    pub capability_request: CapabilityRequest,
    /// Admitted requester identity (verified at admission).
    pub requester: RequesterRef,
    /// Optional parent run lineage.
    pub parent_ref: Option<ParentRunRef>,
    /// Optional supervising sub-agent run.
    pub supervisor_ref: Option<SubAgentRunRef>,
    /// Opaque reference to the admitted context bundle (content, never
    /// authority — TB-4 seam law).
    pub context_bundle_ref: BoundedText,
    /// Source material references.
    pub source_refs: Vec<SourceRef>,
    /// Semantic constraints.
    pub constraints: Vec<TaskConstraint>,
    /// Expected artifacts (drives TB-13 contract satisfaction).
    pub expected_artifacts: Vec<ArtifactExpectation>,
    /// Evaluation independence requirement.
    pub evaluation_requirement: EvaluationRequirement,
    /// Optional typed workspace selector.
    pub workspace_source: Option<WorkspaceSourceRef>,
    /// Optional typed routing preference.
    pub routing_preference: Option<RoutingPreference>,
    /// Requester-asserted approval requirement.
    pub approval_requirement: ApprovalRequirement,
    /// Privacy class of the intent content.
    pub privacy_class: PrivacyClass,
    /// Optional expiry timestamp.
    pub expiry: Option<Timestamp>,
    /// Explicit lineage for a deliberate retry of a prior task (TB-18):
    /// absent for a first submission; a retry is a NEW submission with this
    /// field set, never a rewrite of the prior attempt's facts.
    pub retry_of: Option<TaskRef>,
}

impl TaskIntentV1 {
    /// The canonical request digest used by TB-7 idempotency: SHA-256
    /// (lower hex) over the canonical JSON of `{"schema": tag, "intent":
    /// payload}` using the workspace-wide `memcore::canonical_digest` rule
    /// (keys sorted recursively, so serde field order cannot fork digests).
    /// The V2b encoder must implement the identical rule; the golden pins a
    /// sample digest.
    pub fn canonical_digest(&self) -> String {
        let value = serde_json::to_value(self).expect("TaskIntentV1 serializes");
        let composite = serde_json::json!({
            "schema": SCHEMA_TAG,
            "intent": value,
        });
        memcore::canonical_digest::canonical_json_digest_hex(&composite)
    }
}

impl TryFrom<String> for BoundedText {
    type Error = WireError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl TryFrom<String> for Timestamp {
    type Error = WireError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(&value)
    }
}

/// Wire-level construction/validation failure.
#[derive(Debug, thiserror::Error)]
pub enum WireError {
    /// A text-bearing value exceeded the bounded-text cap.
    #[error("text value exceeds the {BOUNDED_TEXT_MAX}-byte wire cap (len {len})")]
    TextTooLong {
        /// The offending length.
        len: usize,
    },
    /// A timestamp was not RFC3339.
    #[error("timestamp is not valid RFC3339")]
    BadTimestamp(#[source] chrono::ParseError),
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;

    #[test]
    fn bounded_text_caps_unbounded_transcripts() {
        assert!(BoundedText::new("x".repeat(BOUNDED_TEXT_MAX)).is_ok());
        assert!(BoundedText::new("x".repeat(BOUNDED_TEXT_MAX + 1)).is_err());
    }

    #[test]
    fn capability_wire_forms_round_trip_and_the_enum_stays_closed() {
        // TB-5/A: exactly the ratified variants admit on the wire, each
        // under its snake_case wire form; the closed enum fails closed on
        // anything else (the zeroclaw #234 live-receipt blocker class).
        let ratified: [(Capability, &str); 3] = [
            (Capability::ReasoningReview, "reasoning_review"),
            (Capability::ReadOnlyInvestigation, "read_only_investigation"),
            (
                Capability::RepositoryImplementation,
                "repository_implementation",
            ),
        ];
        for (variant, wire) in ratified {
            assert_eq!(
                serde_json::to_value(variant).expect("serializes"),
                serde_json::Value::String(wire.to_string()),
                "wire form for {variant:?}"
            );
            let decoded: Capability =
                serde_json::from_value(serde_json::Value::String(wire.to_string()))
                    .expect("decodes");
            assert_eq!(decoded, variant, "round-trip for {wire}");
        }
        // The V2b acceptance capability decodes through the full
        // CapabilityRequest envelope (the exact live shape zeroclaw #234
        // submitted when tachi rejected it).
        let request: CapabilityRequest = serde_json::from_value(serde_json::json!({
            "capability": "repository_implementation"
        }))
        .expect("V2b acceptance capability decodes");
        assert_eq!(request.capability, Capability::RepositoryImplementation);
        // Fail-closed: an unknown variant is a decode error naming the
        // expected set — never a silent default. Vendor/CLI-shaped
        // capability tokens stay structurally unrepresentable.
        let error = serde_json::from_value::<Capability>(serde_json::Value::String(
            "vendor_cli_run".to_string(),
        ))
        .expect_err("unknown variant must fail decode");
        let message = error.to_string();
        assert!(message.contains("unknown variant"), "{message}");
        assert!(message.contains("reasoning_review"), "{message}");
        assert!(message.contains("read_only_investigation"), "{message}");
        assert!(message.contains("repository_implementation"), "{message}");
    }

    #[test]
    fn digest_is_stable_and_content_sensitive() {
        let a = BoundedText::new("ship the vertical").expect("bounded");
        let mut intent = sample_intent(a.clone());
        let first = intent.canonical_digest();
        // Same semantic content ⇒ same digest.
        intent.objective = a;
        assert_eq!(intent.canonical_digest(), first);
        // Different content ⇒ different digest.
        intent.objective = BoundedText::new("ship the vertical now").expect("bounded");
        assert_ne!(intent.canonical_digest(), first);
    }

    /// Minimal well-formed intent used by module tests; the GOLDEN sample
    /// (checked-in JSON) is the cross-repo pin and lives in `golden/`.
    pub(crate) fn sample_intent(objective: BoundedText) -> TaskIntentV1 {
        TaskIntentV1 {
            schema: SCHEMA_TAG.to_string(),
            objective,
            capability_request: CapabilityRequest {
                capability: Capability::ReasoningReview,
            },
            requester: RequesterRef::claim("zeroclaw-host-alpha").expect("bounded"),
            parent_ref: None,
            supervisor_ref: None,
            context_bundle_ref: BoundedText::new("bundle-7f3a").expect("bounded"),
            source_refs: vec![SourceRef {
                kind: SourceKind::Issue,
                locator: BoundedText::new("kckylechen1/zeroclaw#205").expect("bounded"),
            }],
            constraints: vec![TaskConstraint {
                description: BoundedText::new("no new ledgers").expect("bounded"),
            }],
            expected_artifacts: vec![ArtifactExpectation {
                artifact_class: ArtifactClass::Report,
                description: BoundedText::new("scorecard report").expect("bounded"),
                required: true,
            }],
            evaluation_requirement: EvaluationRequirement {
                independence: IndependenceClass::FreshContextCrossVendor,
            },
            workspace_source: Some(WorkspaceSourceRef {
                repo: BoundedText::new("kckylechen1/zeroclaw").expect("bounded"),
                git_ref: Some(BoundedText::new("master").expect("bounded")),
            }),
            routing_preference: Some(RoutingPreference::PreferTachiManaged),
            approval_requirement: ApprovalRequirement::NotRequired,
            privacy_class: PrivacyClass::Internal,
            expiry: Some(Timestamp::parse("2026-12-01T00:00:00Z").expect("rfc3339")),
            retry_of: None,
        }
    }
}
