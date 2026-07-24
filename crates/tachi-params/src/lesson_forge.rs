//! `LessonCandidateV1` — the D2 precedent-shaped forge target (#1073).
//!
//! Frozen design authority: `docs/engineering/architecture/issue-refinery-memory-lanes.md`
//! §8 (distill campaign and precedent boundary). This is the SAME candidate
//! shape #1059's GitHub source adapter later emits ("Same candidate shape as
//! #1073"; #1059 issue body), so it lives here in `tachi-params` rather than
//! inside a single leaf's crate — a second forge leaf can construct it
//! without depending on `tachi-server::lesson_forge_ops`.
//!
//! Two hard boundaries, both enforced at the type level so no downstream
//! code path can accidentally violate them:
//!
//! 1. **Never established.** [`LessonCandidateStatusV1`] has exactly one
//!    constructible variant, `Pending`. There is no `Established` variant to
//!    reach for — establishment is #950/#1077's gate, entirely outside this
//!    leaf, and a type with only one inhabitant can't be misused to claim
//!    otherwise (the same "no bare `Verified`" discipline
//!    `refinery::ClaimVerificationV1` already uses for HEAD claims).
//! 2. **Identity is honest about itself.** [`LessonEngineReceiptV1::identity_status`]
//!    collapses every degraded/fallback/unknown-identity shape down to
//!    `"preview_only"` — the frozen contract's "Unknown or fallback identity
//!    is preview-only" — so a caller can't read a half-known receipt as a
//!    fully attributed one.

use serde::{Deserialize, Serialize};

use crate::refinery::EvidenceRefV1;

/// Closed vocabulary for what kind of lesson a candidate encodes. Mirrors
/// #1059's frozen output vocabulary exactly ("Pending `LessonCandidateV1` in
/// one of: `precedent | bug_class | verification_pattern | lane_evidence`")
/// so both leaves emit interchangeable rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LessonCandidateKindV1 {
    Precedent,
    BugClass,
    VerificationPattern,
    LaneEvidence,
}

impl LessonCandidateKindV1 {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Precedent => "precedent",
            Self::BugClass => "bug_class",
            Self::VerificationPattern => "verification_pattern",
            Self::LaneEvidence => "lane_evidence",
        }
    }
}

/// Single-inhabitant status enum — see module doc boundary 1. Every
/// `LessonCandidateV1` this leaf constructs carries `Pending`
/// unconditionally; there is no other value to assign.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LessonCandidateStatusV1 {
    #[default]
    Pending,
}

impl LessonCandidateStatusV1 {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
        }
    }
}

/// "Effective provider/model/version/fallback/degraded receipt is
/// mandatory" (frozen contract). A distinct type from
/// `refinery::EngineReceiptV1` because that type has no `effective_version`
/// field and this leaf's contract names one explicitly; reusing it would
/// either drop a required field or force an unrelated leaf's type to grow a
/// field it doesn't need.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LessonEngineReceiptV1 {
    pub requested_role: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effective_provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effective_model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effective_version: Option<String>,
    #[serde(default)]
    pub fallback_chain: Vec<String>,
    #[serde(default)]
    pub degraded: bool,
}

impl LessonEngineReceiptV1 {
    /// A fully known, non-fallback, non-degraded identity. Requires
    /// `effective_version` too — the frozen contract names all five fields
    /// ("Effective provider/model/version/fallback/degraded receipt is
    /// mandatory") as the identity a caller must attest, not just
    /// provider+model (cross-vendor review finding 8: this used to accept a
    /// receipt with `effective_version: None` as "known").
    pub fn has_known_identity(&self) -> bool {
        self.effective_provider
            .as_deref()
            .is_some_and(|s| !s.trim().is_empty())
            && self
                .effective_model
                .as_deref()
                .is_some_and(|s| !s.trim().is_empty())
            && self
                .effective_version
                .as_deref()
                .is_some_and(|s| !s.trim().is_empty())
            && self.fallback_chain.is_empty()
            && !self.degraded
    }

    /// "known" | "preview_only" — see module doc boundary 2.
    pub fn identity_status(&self) -> &'static str {
        if self.has_known_identity() {
            "known"
        } else {
            "preview_only"
        }
    }
}

/// `identity_status` for an `Option<&LessonEngineReceiptV1>` — an absent
/// receipt is unconditionally preview-only (there is nothing to attest).
pub fn lesson_identity_status(receipt: Option<&LessonEngineReceiptV1>) -> &'static str {
    receipt
        .map(LessonEngineReceiptV1::identity_status)
        .unwrap_or("preview_only")
}

/// Proof that a candidate's prose fields were built FROM the complete source
/// text handed to the forge, not a truncated slice of it. `source_bytes` and
/// `covered_bytes` are always equal in this leaf — the forge never slices
/// its input (contrast `tachi-foundry`'s daily-distill batch payload, which
/// caps each memory's `text` at `.chars().take(800)`; the frozen contract
/// explicitly forbids reusing that behavior here). A future leaf that
/// legitimately omits part of a source (e.g. binary/non-text spans) would
/// have a real reason for `covered_bytes < source_bytes` and must record it
/// here rather than silently drop the tail.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct LessonCoverageV1 {
    pub source_bytes: usize,
    pub covered_bytes: usize,
}

impl LessonCoverageV1 {
    pub fn full(source_bytes: usize) -> Self {
        Self {
            source_bytes,
            covered_bytes: source_bytes,
        }
    }

    pub fn is_full(&self) -> bool {
        self.covered_bytes >= self.source_bytes
    }
}

/// The D2 forge target: `situation -> proposed ruling -> why -> how to
/// apply -> refs` (frozen contract). Pending only — see module doc.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LessonCandidateV1 {
    /// Deterministic identity: same (project, source_row_id,
    /// source_revision, kind, situation, proposed_ruling, why,
    /// how_to_apply) always re-derives this id, so an exact replay dedupes
    /// instead of duplicating (same discipline as
    /// `precedent_ops::precedent_short_id` / #1076's `candidate_short_id`).
    pub candidate_id: String,
    /// Same seed minus `source_revision` — links revisions of the same
    /// case to each other the way #1076's `candidate_group_id` does.
    pub candidate_group_id: String,
    pub kind: LessonCandidateKindV1,
    pub situation: String,
    pub proposed_ruling: String,
    pub why: String,
    pub how_to_apply: String,
    /// Immutable source refs this candidate cites. Never empty — a
    /// candidate with no refs fails the frozen contract's discrimination
    /// criterion 4 ("cites its immutable source refs") and the forge
    /// refuses to construct one (see `tachi-server::lesson_forge_ops::forge`).
    pub refs: Vec<EvidenceRefV1>,
    /// Source key. Route-based pilot sources use the canonical
    /// `<route>:<stable-id>` form so provenance stays collision-free without
    /// adding a required public field to this shipped struct-literal surface.
    pub source_row_id: String,
    pub source_revision: String,
    pub coverage: LessonCoverageV1,
    #[serde(default)]
    pub candidate_status: LessonCandidateStatusV1,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub engine_receipt: Option<LessonEngineReceiptV1>,
}

impl LessonCandidateV1 {
    pub fn identity_status(&self) -> &'static str {
        lesson_identity_status(self.engine_receipt.as_ref())
    }

    pub fn cites_source_refs(&self) -> bool {
        !self.refs.is_empty()
    }

    /// Always `false` — there is no code path in this type that can flip
    /// it, matching the frozen contract's "does not claim establishment".
    /// Kept as an explicit method (rather than a stored field a future
    /// editor could accidentally set `true`) so establishment can only ever
    /// happen through #950/#1077's separate gate, never by mutating this
    /// struct.
    pub fn claims_establishment(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::refinery::{EvidenceRelationV1, ImmutableRevisionV1, SourceKindV1};

    fn sample_ref() -> EvidenceRefV1 {
        EvidenceRefV1 {
            relation: EvidenceRelationV1::DerivedFrom,
            target_kind: SourceKindV1::EpisodicMemory,
            target_ref: "/scratch/row-1".to_string(),
            immutable_revision: ImmutableRevisionV1::MemoryRevision("7".to_string()),
            section_or_span: None,
            captured_at: "2026-07-17T00:00:00Z".to_string(),
        }
    }

    fn sample_candidate(receipt: Option<LessonEngineReceiptV1>) -> LessonCandidateV1 {
        LessonCandidateV1 {
            candidate_id: "id1".to_string(),
            candidate_group_id: "group1".to_string(),
            kind: LessonCandidateKindV1::Precedent,
            situation: "s".to_string(),
            proposed_ruling: "r".to_string(),
            why: "w".to_string(),
            how_to_apply: "h".to_string(),
            refs: vec![sample_ref()],
            source_row_id: "row-1".to_string(),
            source_revision: "7".to_string(),
            coverage: LessonCoverageV1::full(1200),
            candidate_status: LessonCandidateStatusV1::Pending,
            engine_receipt: receipt,
        }
    }

    #[test]
    fn kind_wire_values_match_1059_vocabulary() {
        assert_eq!(
            serde_json::to_string(&LessonCandidateKindV1::BugClass).unwrap(),
            "\"bug_class\""
        );
        assert_eq!(
            serde_json::to_string(&LessonCandidateKindV1::VerificationPattern).unwrap(),
            "\"verification_pattern\""
        );
        assert_eq!(
            serde_json::to_string(&LessonCandidateKindV1::LaneEvidence).unwrap(),
            "\"lane_evidence\""
        );
        assert_eq!(LessonCandidateKindV1::Precedent.as_str(), "precedent");
    }

    #[test]
    fn candidate_status_only_ever_serializes_pending() {
        let c = sample_candidate(None);
        assert_eq!(c.candidate_status.as_str(), "pending");
        assert_eq!(
            serde_json::to_string(&c.candidate_status).unwrap(),
            "\"pending\""
        );
    }

    #[test]
    fn claims_establishment_is_always_false() {
        assert!(!sample_candidate(None).claims_establishment());
    }

    #[test]
    fn missing_receipt_is_preview_only() {
        let c = sample_candidate(None);
        assert_eq!(c.identity_status(), "preview_only");
    }

    #[test]
    fn fallback_receipt_is_preview_only_even_with_provider_and_model() {
        let receipt = LessonEngineReceiptV1 {
            requested_role: "producer".to_string(),
            effective_provider: Some("anthropic".to_string()),
            effective_model: Some("claude".to_string()),
            effective_version: Some("v1".to_string()),
            fallback_chain: vec!["backup-provider".to_string()],
            degraded: false,
        };
        assert_eq!(lesson_identity_status(Some(&receipt)), "preview_only");
        assert!(!receipt.has_known_identity());
    }

    #[test]
    fn degraded_receipt_is_preview_only() {
        let receipt = LessonEngineReceiptV1 {
            requested_role: "producer".to_string(),
            effective_provider: Some("anthropic".to_string()),
            effective_model: Some("claude".to_string()),
            effective_version: Some("v1".to_string()),
            fallback_chain: Vec::new(),
            degraded: true,
        };
        assert_eq!(lesson_identity_status(Some(&receipt)), "preview_only");
    }

    #[test]
    fn missing_version_receipt_is_preview_only_even_with_provider_and_model() {
        let receipt = LessonEngineReceiptV1 {
            requested_role: "producer".to_string(),
            effective_provider: Some("anthropic".to_string()),
            effective_model: Some("claude".to_string()),
            effective_version: None,
            fallback_chain: Vec::new(),
            degraded: false,
        };
        assert_eq!(lesson_identity_status(Some(&receipt)), "preview_only");
        assert!(!receipt.has_known_identity());
    }

    #[test]
    fn fully_known_non_degraded_receipt_is_known() {
        let receipt = LessonEngineReceiptV1 {
            requested_role: "producer".to_string(),
            effective_provider: Some("anthropic".to_string()),
            effective_model: Some("claude".to_string()),
            effective_version: Some("v1".to_string()),
            fallback_chain: Vec::new(),
            degraded: false,
        };
        assert_eq!(lesson_identity_status(Some(&receipt)), "known");
        assert!(receipt.has_known_identity());
    }

    #[test]
    fn coverage_full_marks_source_bytes_equal_covered_bytes_regardless_of_length() {
        // Discriminates against the daily-distill `.chars().take(800)` cap
        // the frozen contract explicitly forbids reusing: a source well
        // over 800 bytes must still report full coverage, not a an 800 cap.
        let long = "x".repeat(5000);
        let coverage = LessonCoverageV1::full(long.len());
        assert_eq!(coverage.source_bytes, 5000);
        assert_eq!(coverage.covered_bytes, 5000);
        assert!(coverage.is_full());
    }

    #[test]
    fn cites_source_refs_false_when_refs_empty() {
        let mut c = sample_candidate(None);
        c.refs.clear();
        assert!(!c.cites_source_refs());
        assert!(sample_candidate(None).cites_source_refs());
    }
}
