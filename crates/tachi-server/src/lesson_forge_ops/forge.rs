//! The D2 forge: selected source bundle -> pending `LessonCandidateV1`.
//!
//! "forge selected, grounded memory source bundles into pending
//! lesson/precedent candidates shaped as `situation -> proposed ruling ->
//! why -> how to apply -> refs`" (#1073 frozen contract).
//!
//! What this module does NOT do: write text. Producing the four prose
//! fields from a raw source is model work (the frozen contract makes an
//! engine receipt mandatory precisely because a model wrote them), and this
//! leaf has no live model-call budget to spend inside a bounded,
//! non-interactive implementer session. [`ForgeDraft`] is the seam: a real
//! caller supplies the model-authored draft (via whatever dispatch/engine
//! call it wired up), and [`forge_lesson_candidate`] does the part that
//! IS this leaf's job and IS fully deterministic/testable — validate the
//! draft against the frozen contract's own hard requirements (non-empty
//! fields, at least one cited ref, the row is a member of an already-frozen
//! pilot manifest so nothing gets PERSISTED outside the 50-row spend gate),
//! assign deterministic identity, and mark the result unconditionally
//! pending.
//!
//! **What "no truncation" means here, precisely** (cross-vendor review
//! finding 7): [`LessonCoverageV1::full`] reports the byte length of
//! `SourceBundle.full_text` exactly as handed in — this module itself never
//! slices that string before computing coverage or before the (external,
//! out-of-scope) model call it seams to. It does NOT and cannot verify that
//! the model actually read/used the full text when authoring `ForgeDraft` —
//! that's the model's own behavior, attested (or not) via `engine_receipt`,
//! not something a deterministic leaf with no model call of its own can
//! independently observe.

use tachi_params::{
    EvidenceRefV1, LessonCandidateKindV1, LessonCandidateStatusV1, LessonCandidateV1,
    LessonCoverageV1, LessonEngineReceiptV1,
};

use super::pilot::PilotManifestV1;

/// The complete source this candidate is forged from. `full_text` is never
/// sliced by this module — see `LessonCoverageV1`'s doc and the
/// `coverage_reflects_the_true_source_length_never_a_fixed_cap` test below.
#[derive(Debug, Clone)]
pub struct SourceBundle {
    pub row_id: String,
    pub revision: i64,
    pub full_text: String,
    /// Immutable refs this source is grounded in (issue/PR/doc/memory
    /// revision). At least one is required — see `ForgeError::NoSourceRefs`.
    pub refs: Vec<EvidenceRefV1>,
}

/// The model-authored draft this leaf's forge validates and frames. Never
/// constructed by this module — see module doc.
#[derive(Debug, Clone)]
pub struct ForgeDraft {
    pub situation: String,
    pub proposed_ruling: String,
    pub why: String,
    pub how_to_apply: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ForgeError {
    /// The row/revision isn't a member of an already-frozen pilot
    /// manifest — the spend gate the frozen contract requires ("Record
    /// ids/revisions and selection reason before model spend").
    NotInFrozenManifest {
        row_id: String,
        revision: i64,
    },
    EmptyDraftField(&'static str),
    /// Criterion 4 of the frozen contract's blinded-discrimination pass
    /// bar: "the candidate cites its immutable source refs".
    NoSourceRefs,
}

impl std::fmt::Display for ForgeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotInFrozenManifest { row_id, revision } => write!(
                f,
                "source row {row_id}@{revision} is not a member of the frozen pilot manifest \
                 — refusing to spend a forge call on an unselected row"
            ),
            Self::EmptyDraftField(field) => {
                write!(f, "forge draft field `{field}` is empty")
            }
            Self::NoSourceRefs => write!(
                f,
                "source bundle has no immutable refs — a candidate cannot cite what its \
                 source never had"
            ),
        }
    }
}

/// SHA1-based deterministic hash (same primitive precedent_ops's
/// `precedent_short_id` and #1076's `candidate_short_id` use), truncated to
/// 16 hex chars. Duplicated locally rather than imported from
/// `precedent_ops` (private to that module, and this leaf's identity seed
/// has a different field set) — a small, self-contained helper, not a
/// second copy of that module's ruling-decomposition logic.
fn hash16(seed: &str) -> String {
    let hashed = uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_OID, seed.as_bytes());
    hashed.simple().to_string()[..16].to_string()
}

/// Length-prefix-frame one field so no field's content can bleed into an
/// adjacent one in the hash seed (same discipline as
/// `precedent_ops::frame_field` / #1076's NUL-byte-safety fix).
fn frame_field(value: &str) -> String {
    format!("{}:{}|", value.len(), value)
}

/// Group-identity seed: project + row_id + kind + draft content, deliberately
/// omitting ONLY `source_revision` — see `LessonCandidateV1::candidate_group_id`'s
/// doc ("Same seed minus `source_revision` — links revisions of the same
/// case to each other"). Cross-vendor review finding 6: this used to omit
/// `row_id` too, so identical drafts forged from two DIFFERENT, unrelated
/// source rows collapsed into the same group — `row_id` is included here
/// precisely so that never happens; only genuinely different REVISIONS of
/// the SAME row share a group.
fn group_seed(
    project: &str,
    row_id: &str,
    kind: LessonCandidateKindV1,
    draft: &ForgeDraft,
) -> String {
    let mut seed = String::new();
    seed.push_str(&frame_field(project));
    seed.push_str(&frame_field(row_id));
    seed.push_str(&frame_field(kind.as_str()));
    seed.push_str(&frame_field(&draft.situation));
    seed.push_str(&frame_field(&draft.proposed_ruling));
    seed.push_str(&frame_field(&draft.why));
    seed.push_str(&frame_field(&draft.how_to_apply));
    seed
}

fn candidate_group_id(
    project: &str,
    row_id: &str,
    kind: LessonCandidateKindV1,
    draft: &ForgeDraft,
) -> String {
    hash16(&group_seed(project, row_id, kind, draft))
}

fn candidate_id(
    project: &str,
    kind: LessonCandidateKindV1,
    draft: &ForgeDraft,
    bundle: &SourceBundle,
) -> String {
    let mut seed = group_seed(project, &bundle.row_id, kind, draft);
    seed.push_str(&frame_field(&bundle.revision.to_string()));
    hash16(&seed)
}

/// Forge one pending `LessonCandidateV1`. `manifest` must already contain
/// `(bundle.row_id, bundle.revision)` — the type-level spend gate.
pub fn forge_lesson_candidate(
    project: &str,
    manifest: &PilotManifestV1,
    bundle: &SourceBundle,
    kind: LessonCandidateKindV1,
    draft: &ForgeDraft,
    engine_receipt: Option<LessonEngineReceiptV1>,
) -> Result<LessonCandidateV1, ForgeError> {
    if !manifest.contains(&bundle.row_id, bundle.revision) {
        return Err(ForgeError::NotInFrozenManifest {
            row_id: bundle.row_id.clone(),
            revision: bundle.revision,
        });
    }

    for (name, value) in [
        ("situation", &draft.situation),
        ("proposed_ruling", &draft.proposed_ruling),
        ("why", &draft.why),
        ("how_to_apply", &draft.how_to_apply),
    ] {
        if value.trim().is_empty() {
            return Err(ForgeError::EmptyDraftField(name));
        }
    }

    if bundle.refs.is_empty() {
        return Err(ForgeError::NoSourceRefs);
    }

    let group_id = candidate_group_id(project, &bundle.row_id, kind, draft);
    let id = candidate_id(project, kind, draft, bundle);

    Ok(LessonCandidateV1 {
        candidate_id: id,
        candidate_group_id: group_id,
        kind,
        situation: draft.situation.clone(),
        proposed_ruling: draft.proposed_ruling.clone(),
        why: draft.why.clone(),
        how_to_apply: draft.how_to_apply.clone(),
        refs: bundle.refs.clone(),
        source_row_id: bundle.row_id.clone(),
        source_revision: bundle.revision.to_string(),
        coverage: LessonCoverageV1::full(bundle.full_text.len()),
        candidate_status: LessonCandidateStatusV1::Pending,
        engine_receipt,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lesson_forge_ops::pilot::{freeze_pilot_manifest, PilotRowKindV1, PilotRowV1};
    use tachi_params::{EvidenceRelationV1, ImmutableRevisionV1, SourceKindV1};

    fn sample_ref() -> EvidenceRefV1 {
        EvidenceRefV1 {
            relation: EvidenceRelationV1::DerivedFrom,
            target_kind: SourceKindV1::EpisodicMemory,
            target_ref: "/scratch/row-0".to_string(),
            immutable_revision: ImmutableRevisionV1::MemoryRevision("1".to_string()),
            section_or_span: None,
            captured_at: "2026-07-17T00:00:00Z".to_string(),
        }
    }

    fn valid_draft() -> ForgeDraft {
        ForgeDraft {
            situation: "Agent faced env-gated auth bypass".to_string(),
            proposed_ruling: "Never trust an env var alone for a security gate".to_string(),
            why: "Env vars are attacker-controllable in this deployment".to_string(),
            how_to_apply: "Require a signed capability token, not an env flag".to_string(),
        }
    }

    fn manifest_with_row(row_id: &str, revision: i64) -> PilotManifestV1 {
        let mut rows = Vec::new();
        for i in 0..49 {
            rows.push(PilotRowV1 {
                row_id: format!("filler-{i}"),
                revision: 1,
                kind: PilotRowKindV1::Narrative,
                selection_reason: "filler".to_string(),
                reference_decision: "filler decision".to_string(),
                target_kind: LessonCandidateKindV1::Precedent,
            });
        }
        rows.push(PilotRowV1 {
            row_id: row_id.to_string(),
            revision,
            kind: PilotRowKindV1::StructuredControl,
            selection_reason: "control row for forge test".to_string(),
            reference_decision: "reference decision".to_string(),
            target_kind: LessonCandidateKindV1::Precedent,
        });
        freeze_pilot_manifest(rows).expect("test manifest must freeze")
    }

    /// Like `manifest_with_row`, but freezes every `(row_id, revision)` pair
    /// given, padded to 50 with filler narrative rows — used by the
    /// candidate-grouping tests below, which need more than one real row in
    /// the same frozen manifest.
    fn manifest_with_rows(entries: &[(&str, i64)]) -> PilotManifestV1 {
        let mut rows = Vec::new();
        for i in 0..(50 - entries.len()) {
            rows.push(PilotRowV1 {
                row_id: format!("filler-{i}"),
                revision: 1,
                kind: PilotRowKindV1::Narrative,
                selection_reason: "filler".to_string(),
                reference_decision: "filler decision".to_string(),
                target_kind: LessonCandidateKindV1::Precedent,
            });
        }
        for (row_id, revision) in entries {
            rows.push(PilotRowV1 {
                row_id: row_id.to_string(),
                revision: *revision,
                kind: PilotRowKindV1::StructuredControl,
                selection_reason: "control row for forge test".to_string(),
                reference_decision: "reference decision".to_string(),
                target_kind: LessonCandidateKindV1::Precedent,
            });
        }
        freeze_pilot_manifest(rows).expect("test manifest must freeze")
    }

    fn valid_bundle() -> SourceBundle {
        SourceBundle {
            row_id: "row-0".to_string(),
            revision: 1,
            full_text: "x".repeat(5000),
            refs: vec![sample_ref()],
        }
    }

    #[test]
    fn forging_a_row_outside_the_frozen_manifest_is_refused() {
        let manifest = manifest_with_row("row-0", 1);
        let mut bundle = valid_bundle();
        bundle.row_id = "not-in-manifest".to_string();
        let err = forge_lesson_candidate(
            "proj",
            &manifest,
            &bundle,
            LessonCandidateKindV1::Precedent,
            &valid_draft(),
            None,
        )
        .expect_err("row not in manifest must be refused");
        assert!(matches!(err, ForgeError::NotInFrozenManifest { .. }));
    }

    #[test]
    fn forging_a_stale_revision_of_a_known_row_is_refused() {
        let manifest = manifest_with_row("row-0", 1);
        let mut bundle = valid_bundle();
        bundle.revision = 2; // manifest froze revision 1
        let err = forge_lesson_candidate(
            "proj",
            &manifest,
            &bundle,
            LessonCandidateKindV1::Precedent,
            &valid_draft(),
            None,
        )
        .expect_err("stale revision must be refused");
        assert!(matches!(err, ForgeError::NotInFrozenManifest { .. }));
    }

    #[test]
    fn empty_draft_field_is_refused() {
        let manifest = manifest_with_row("row-0", 1);
        let bundle = valid_bundle();
        let mut draft = valid_draft();
        draft.why = "   ".to_string();
        let err = forge_lesson_candidate(
            "proj",
            &manifest,
            &bundle,
            LessonCandidateKindV1::Precedent,
            &draft,
            None,
        )
        .expect_err("blank why must be refused");
        assert_eq!(err, ForgeError::EmptyDraftField("why"));
    }

    #[test]
    fn source_with_no_refs_is_refused() {
        let manifest = manifest_with_row("row-0", 1);
        let mut bundle = valid_bundle();
        bundle.refs.clear();
        let err = forge_lesson_candidate(
            "proj",
            &manifest,
            &bundle,
            LessonCandidateKindV1::Precedent,
            &valid_draft(),
            None,
        )
        .expect_err("no source refs must be refused");
        assert_eq!(err, ForgeError::NoSourceRefs);
    }

    #[test]
    fn a_valid_forge_produces_a_pending_candidate_that_cites_refs_and_never_establishes() {
        let manifest = manifest_with_row("row-0", 1);
        let bundle = valid_bundle();
        let candidate = forge_lesson_candidate(
            "proj",
            &manifest,
            &bundle,
            LessonCandidateKindV1::Precedent,
            &valid_draft(),
            None,
        )
        .expect("valid forge must succeed");
        assert_eq!(candidate.candidate_status.as_str(), "pending");
        assert!(candidate.cites_source_refs());
        assert!(!candidate.claims_establishment());
        assert_eq!(candidate.identity_status(), "preview_only");
    }

    #[test]
    fn coverage_reflects_the_true_source_length_never_a_fixed_cap() {
        // Discriminates against reusing daily-distill's `.chars().take(800)`
        // truncation — a >800-byte source must report full coverage at its
        // TRUE length, not a cap.
        let manifest = manifest_with_row("row-0", 1);
        let mut bundle = valid_bundle();
        bundle.full_text = "y".repeat(12_345);
        let candidate = forge_lesson_candidate(
            "proj",
            &manifest,
            &bundle,
            LessonCandidateKindV1::Precedent,
            &valid_draft(),
            None,
        )
        .expect("valid forge must succeed");
        assert_eq!(candidate.coverage.source_bytes, 12_345);
        assert_eq!(candidate.coverage.covered_bytes, 12_345);
        assert!(candidate.coverage.is_full());
    }

    #[test]
    fn exact_replay_re_derives_the_same_candidate_id() {
        let manifest = manifest_with_row("row-0", 1);
        let bundle = valid_bundle();
        let a = forge_lesson_candidate(
            "proj",
            &manifest,
            &bundle,
            LessonCandidateKindV1::Precedent,
            &valid_draft(),
            None,
        )
        .expect("first forge");
        let b = forge_lesson_candidate(
            "proj",
            &manifest,
            &bundle,
            LessonCandidateKindV1::Precedent,
            &valid_draft(),
            None,
        )
        .expect("replay forge");
        assert_eq!(a.candidate_id, b.candidate_id);
        assert_eq!(a.candidate_group_id, b.candidate_group_id);
    }

    #[test]
    fn a_different_draft_changes_candidate_identity() {
        let manifest = manifest_with_row("row-0", 1);
        let bundle = valid_bundle();
        let a = forge_lesson_candidate(
            "proj",
            &manifest,
            &bundle,
            LessonCandidateKindV1::Precedent,
            &valid_draft(),
            None,
        )
        .expect("first forge");
        let mut other_draft = valid_draft();
        other_draft.proposed_ruling = "A completely different ruling".to_string();
        let b = forge_lesson_candidate(
            "proj",
            &manifest,
            &bundle,
            LessonCandidateKindV1::Precedent,
            &other_draft,
            None,
        )
        .expect("second forge");
        assert_ne!(a.candidate_id, b.candidate_id);
    }

    #[test]
    fn identical_drafts_from_two_different_unrelated_rows_do_not_share_a_group() {
        // Cross-vendor review finding 6: `candidate_group_id` used to be
        // derived from project+kind+draft only, omitting `row_id` entirely
        // — so two DIFFERENT rows that happen to forge the identical draft
        // text collapsed into the same group, breaking the uniqueness a
        // discriminating-test fixture needs. `row_id` must now separate
        // them even though every draft field is byte-identical.
        let manifest = manifest_with_rows(&[("row-0", 1), ("row-1", 1)]);
        let mut bundle_a = valid_bundle();
        bundle_a.row_id = "row-0".to_string();
        let mut bundle_b = valid_bundle();
        bundle_b.row_id = "row-1".to_string();

        let a = forge_lesson_candidate(
            "proj",
            &manifest,
            &bundle_a,
            LessonCandidateKindV1::Precedent,
            &valid_draft(),
            None,
        )
        .expect("first forge");
        let b = forge_lesson_candidate(
            "proj",
            &manifest,
            &bundle_b,
            LessonCandidateKindV1::Precedent,
            &valid_draft(),
            None,
        )
        .expect("second forge");

        assert_ne!(
            a.candidate_group_id, b.candidate_group_id,
            "identical drafts from unrelated rows must not collapse into the same group"
        );
        assert_ne!(a.candidate_id, b.candidate_id);
    }

    #[test]
    fn two_revisions_of_the_same_row_share_a_group_but_not_an_id() {
        // The other half of the same guarantee: `candidate_group_id` omits
        // ONLY `source_revision` (per `LessonCandidateV1`'s doc) — two
        // revisions of the SAME row with the same draft SHOULD share a
        // group (that's the whole point of "links revisions of the same
        // case to each other"), while `candidate_id` itself still differs
        // because revision is part of that seed.
        let manifest = manifest_with_rows(&[("row-0", 1), ("row-0", 2)]);
        let mut bundle_rev1 = valid_bundle();
        bundle_rev1.row_id = "row-0".to_string();
        bundle_rev1.revision = 1;
        let mut bundle_rev2 = valid_bundle();
        bundle_rev2.row_id = "row-0".to_string();
        bundle_rev2.revision = 2;

        let a = forge_lesson_candidate(
            "proj",
            &manifest,
            &bundle_rev1,
            LessonCandidateKindV1::Precedent,
            &valid_draft(),
            None,
        )
        .expect("first forge");
        let b = forge_lesson_candidate(
            "proj",
            &manifest,
            &bundle_rev2,
            LessonCandidateKindV1::Precedent,
            &valid_draft(),
            None,
        )
        .expect("second forge");

        assert_eq!(
            a.candidate_group_id, b.candidate_group_id,
            "two revisions of the same row with the same draft must share a group"
        );
        assert_ne!(a.candidate_id, b.candidate_id);
    }

    #[test]
    fn known_engine_receipt_yields_known_identity_status() {
        let manifest = manifest_with_row("row-0", 1);
        let bundle = valid_bundle();
        let receipt = LessonEngineReceiptV1 {
            requested_role: "producer".to_string(),
            effective_provider: Some("anthropic".to_string()),
            effective_model: Some("claude".to_string()),
            effective_version: Some("v1".to_string()),
            fallback_chain: Vec::new(),
            degraded: false,
        };
        let candidate = forge_lesson_candidate(
            "proj",
            &manifest,
            &bundle,
            LessonCandidateKindV1::Precedent,
            &valid_draft(),
            Some(receipt),
        )
        .expect("valid forge must succeed");
        assert_eq!(candidate.identity_status(), "known");
    }
}
