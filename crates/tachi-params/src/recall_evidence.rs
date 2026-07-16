//! Evidence-grounded `ask`/briefing current-work anchor types (#1071).
//!
//! Frozen design authority: `docs/engineering/architecture/issue-refinery-memory-lanes.md`
//! §3 (typed evidence envelope), §6 (recall and ask are evidence
//! composition). This module implements the subset of §6.2's
//! `RecallEvidenceV1` target contract that #1071 lands as a callable runtime
//! shape, scoped to exact issue/PR anchor grounding for `tachi_memory(action=
//! 'ask')`. It deliberately reuses [`crate::SourceKindV1`] and
//! [`crate::GroundingStatusV1`] from the #1002 `refinery` module instead of
//! redeclaring them — both leaves share the same one source-artifact
//! vocabulary and the same closed grounded/missing_anchor state (canon doc
//! §3/§4.1).
//!
//! Deliberately NOT implemented in this leaf (left for a later leaf per the
//! canon doc's delivery sequence): independent per-source-kind candidate
//! budgets across the full search/recall surface (§6.1 steps 2-4), a
//! `claim_coverage` computed from real claim-level source-span coverage
//! (this leaf's `claim_coverage` is a coarse 0.0/1.0 "was the anchor itself
//! resolved" signal, not per-claim span coverage), and the generic
//! `EvidenceEnvelopeV1<T>` wrapper. `authority` only ever takes the
//! `CurrentWork` or `Advisory` values here — the other four vocabulary
//! members exist for wire-shape completeness with the canon doc's closed
//! authority/adjudication table (§3) but no live path in this leaf
//! constructs them.

use serde::{Deserialize, Serialize};

use crate::{GroundingStatusV1, SourceKindV1};

/// Canon doc §3's closed authority vocabulary. Only `CurrentWork` and
/// `Advisory` are constructed by this leaf's live path (see module doc);
/// the rest are declared for wire-shape completeness so a later leaf that
/// DOES construct them is not a payload-shape change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthorityClassV1 {
    CurrentWork,
    Canonical,
    Verification,
    Advisory,
    Playbook,
    Precedent,
}

impl AuthorityClassV1 {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::CurrentWork => "current_work",
            Self::Canonical => "canonical",
            Self::Verification => "verification",
            Self::Advisory => "advisory",
            Self::Playbook => "playbook",
            Self::Precedent => "precedent",
        }
    }
}

impl std::fmt::Display for AuthorityClassV1 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Canon doc §6.2's `RecallEvidenceV1`, plus one extension field
/// (`grounding_status`) carried for the same reason #1002's
/// `IssueDispositionProposalV1` carries its own `grounding_status`
/// extension: a consumer of only this row (without re-deriving it from
/// `contradictions`) can still see whether the anchor resolved.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecallEvidenceV1 {
    pub kind: SourceKindV1,
    pub authority: AuthorityClassV1,
    /// Free-text lifecycle label (e.g. issue state `"open"`/`"closed"`, or
    /// `"unknown"` when the anchor did not resolve). Not the closed
    /// `candidate | pending_review | active | stale | superseded | rejected`
    /// wiki-lifecycle vocabulary from canon doc §7 — that vocabulary governs
    /// `KnowledgeArtifactV1`, a different (also-unimplemented-in-this-leaf)
    /// shape; kept as a plain `String` here rather than forcing a borrowed
    /// enum this leaf's only caller (a live GitHub issue's `state` field)
    /// cannot otherwise honestly populate.
    pub lifecycle: String,
    pub source_ref: String,
    pub source_revision: String,
    pub valid_at: String,
    pub retrieval_score: f64,
    /// Coarse 0.0/1.0 signal in this leaf (see module doc) — 1.0 when the
    /// anchor resolved and its claim (the issue's own current state) is
    /// therefore fully represented, 0.0 when it did not.
    pub claim_coverage: f64,
    pub contradictions: Vec<String>,
    pub grounding_status: GroundingStatusV1,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authority_class_wire_values() {
        assert_eq!(AuthorityClassV1::CurrentWork.as_str(), "current_work");
        assert_eq!(
            serde_json::to_string(&AuthorityClassV1::CurrentWork).unwrap(),
            "\"current_work\""
        );
        assert_eq!(AuthorityClassV1::Advisory.as_str(), "advisory");
    }

    #[test]
    fn recall_evidence_round_trips() {
        let row = RecallEvidenceV1 {
            kind: SourceKindV1::Issue,
            authority: AuthorityClassV1::CurrentWork,
            lifecycle: "open".to_string(),
            source_ref: "owner/repo#1".to_string(),
            source_revision: "deadbeef".to_string(),
            valid_at: "2026-07-16T00:00:00Z".to_string(),
            retrieval_score: 1.0,
            claim_coverage: 1.0,
            contradictions: vec![],
            grounding_status: GroundingStatusV1::Grounded,
        };
        let wire = serde_json::to_string(&row).expect("serialize");
        let back: RecallEvidenceV1 = serde_json::from_str(&wire).expect("deserialize");
        assert_eq!(back, row);
    }
}
