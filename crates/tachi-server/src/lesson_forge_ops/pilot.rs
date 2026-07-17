//! The frozen 50-row pilot manifest (#1073).
//!
//! "Use exactly the frozen 50-row pilot selected by #1043: narrative
//! candidates plus structured controls. Record ids/revisions and selection
//! reason before model spend." — frozen contract, "Inputs and output".
//!
//! `#1043` is the campaign umbrella, not a pre-existing row list (verified:
//! no such artifact exists in this repo or in the campaign's own
//! comments — #1059's sibling leaf uses the identical imperative pattern,
//! "Freeze exactly 20 owner-controlled cases", instructing ITS implementer
//! to do the freezing). [`freeze_pilot_manifest`] is the validation/freezing
//! act: [`crate::lesson_forge_ops::forge::forge_lesson_candidate`] refuses
//! to construct a `LessonCandidateV1` for any row that isn't a member of an
//! already-frozen manifest.
//!
//! **Scope of this gate, precisely** (cross-vendor review finding 7): this
//! refuses to PERSIST a candidate built from an unselected row. It does not
//! — cannot, from inside this leaf — prevent a caller from spending an
//! actual live-model call on an unselected row BEFORE constructing the
//! `ForgeDraft` it hands to `forge_lesson_candidate`; controlling that
//! requires the caller to check `PilotManifestV1::contains` before ever
//! invoking the model, which is exactly the harness-runner seam
//! `forge.rs`'s module doc describes (`ForgeDraft` is never constructed by
//! this leaf). This module is also a purely in-memory validated `Vec` — it
//! has no durable record, hash, or timestamp of its own; a harness runner
//! that needs the freeze to survive across separate process invocations
//! must persist it (e.g. through `lesson_forge_ops::storage`'s save path),
//! which is a harness-runner/persistence concern this leaf doesn't invent.

use tachi_params::LessonCandidateKindV1;

/// The two source shapes the frozen contract names: "narrative candidates
/// plus structured controls".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PilotRowKindV1 {
    Narrative,
    StructuredControl,
}

/// The exact pilot size the frozen contract names ("50-row pilot",
/// "50-row cold-start A/B" in the issue title). Not a threshold subject to
/// tuning — the contract calls it "exactly the frozen 50-row pilot".
pub const PILOT_SIZE: usize = 50;

/// One frozen pilot row: identity, selection reason, and the predeclared
/// target decision the blinded discrimination run scores against
/// ("For each source row, predeclare the material target decision and
/// reference ruling" — frozen contract, "Blinded discrimination").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PilotRowV1 {
    pub row_id: String,
    pub revision: i64,
    pub kind: PilotRowKindV1,
    /// Why this row was selected, recorded before any model spend.
    pub selection_reason: String,
    /// The material target decision / reference ruling this row's
    /// candidate is scored against — predeclared, never inferred after the
    /// fact from a run's own output.
    pub reference_decision: String,
    /// Which lesson-candidate shape this row is expected to forge into.
    pub target_kind: LessonCandidateKindV1,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PilotFreezeError {
    WrongRowCount { expected: usize, actual: usize },
    DuplicateRow { row_id: String, revision: i64 },
    MissingSelectionReason { row_id: String },
    MissingReferenceDecision { row_id: String },
    NoNarrativeRows,
    NoStructuredControlRows,
}

impl std::fmt::Display for PilotFreezeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::WrongRowCount { expected, actual } => write!(
                f,
                "pilot manifest must have exactly {expected} rows, got {actual}"
            ),
            Self::DuplicateRow { row_id, revision } => {
                write!(f, "duplicate pilot row {row_id}@{revision}")
            }
            Self::MissingSelectionReason { row_id } => {
                write!(f, "pilot row {row_id} is missing a selection_reason")
            }
            Self::MissingReferenceDecision { row_id } => write!(
                f,
                "pilot row {row_id} is missing a predeclared reference_decision"
            ),
            Self::NoNarrativeRows => {
                write!(f, "pilot manifest has no narrative-candidate rows")
            }
            Self::NoStructuredControlRows => {
                write!(f, "pilot manifest has no structured-control rows")
            }
        }
    }
}

/// A frozen, spend-gating pilot manifest. Construct only via
/// [`freeze_pilot_manifest`] — there is no public constructor that skips
/// validation, so any [`PilotManifestV1`] a caller holds already passed the
/// exactly-50 / no-duplicate / recorded-reason / predeclared-decision /
/// narrative-plus-control checks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PilotManifestV1 {
    rows: Vec<PilotRowV1>,
}

impl PilotManifestV1 {
    pub fn rows(&self) -> &[PilotRowV1] {
        &self.rows
    }

    pub fn contains(&self, row_id: &str, revision: i64) -> bool {
        self.rows
            .iter()
            .any(|r| r.row_id == row_id && r.revision == revision)
    }

    pub fn find(&self, row_id: &str, revision: i64) -> Option<&PilotRowV1> {
        self.rows
            .iter()
            .find(|r| r.row_id == row_id && r.revision == revision)
    }
}

/// Validate + freeze a candidate pilot row set. Returns every violation
/// found (not just the first), matching this codebase's usual "report every
/// mismatch" discipline for spend/authority gates.
pub fn freeze_pilot_manifest(
    rows: Vec<PilotRowV1>,
) -> Result<PilotManifestV1, Vec<PilotFreezeError>> {
    let mut errors = Vec::new();

    if rows.len() != PILOT_SIZE {
        errors.push(PilotFreezeError::WrongRowCount {
            expected: PILOT_SIZE,
            actual: rows.len(),
        });
    }

    let mut seen: std::collections::HashSet<(String, i64)> = std::collections::HashSet::new();
    for row in &rows {
        if !seen.insert((row.row_id.clone(), row.revision)) {
            errors.push(PilotFreezeError::DuplicateRow {
                row_id: row.row_id.clone(),
                revision: row.revision,
            });
        }
        if row.selection_reason.trim().is_empty() {
            errors.push(PilotFreezeError::MissingSelectionReason {
                row_id: row.row_id.clone(),
            });
        }
        if row.reference_decision.trim().is_empty() {
            errors.push(PilotFreezeError::MissingReferenceDecision {
                row_id: row.row_id.clone(),
            });
        }
    }

    if !rows.iter().any(|r| r.kind == PilotRowKindV1::Narrative) {
        errors.push(PilotFreezeError::NoNarrativeRows);
    }
    if !rows
        .iter()
        .any(|r| r.kind == PilotRowKindV1::StructuredControl)
    {
        errors.push(PilotFreezeError::NoStructuredControlRows);
    }

    if errors.is_empty() {
        Ok(PilotManifestV1 { rows })
    } else {
        Err(errors)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: &str, rev: i64, kind: PilotRowKindV1) -> PilotRowV1 {
        PilotRowV1 {
            row_id: id.to_string(),
            revision: rev,
            kind,
            selection_reason: format!("selected {id} for coverage"),
            reference_decision: format!("reference decision for {id}"),
            target_kind: LessonCandidateKindV1::Precedent,
        }
    }

    fn valid_50_rows() -> Vec<PilotRowV1> {
        let mut rows = Vec::new();
        for i in 0..49 {
            rows.push(row(&format!("row-{i}"), 1, PilotRowKindV1::Narrative));
        }
        rows.push(row("row-control-0", 1, PilotRowKindV1::StructuredControl));
        rows
    }

    #[test]
    fn exactly_50_narrative_plus_control_rows_freeze_cleanly() {
        let rows = valid_50_rows();
        let manifest = freeze_pilot_manifest(rows).expect("50 valid rows must freeze");
        assert_eq!(manifest.rows().len(), 50);
        assert!(manifest.contains("row-0", 1));
        assert!(!manifest.contains("row-0", 2));
    }

    #[test]
    fn forty_nine_rows_is_red_wrong_count() {
        let mut rows = valid_50_rows();
        rows.pop();
        let errors = freeze_pilot_manifest(rows).expect_err("49 rows must not freeze");
        assert!(errors.contains(&PilotFreezeError::WrongRowCount {
            expected: 50,
            actual: 49
        }));
    }

    #[test]
    fn fifty_one_rows_is_red_wrong_count() {
        let mut rows = valid_50_rows();
        rows.push(row("row-extra", 1, PilotRowKindV1::Narrative));
        let errors = freeze_pilot_manifest(rows).expect_err("51 rows must not freeze");
        assert!(errors.contains(&PilotFreezeError::WrongRowCount {
            expected: 50,
            actual: 51
        }));
    }

    #[test]
    fn duplicate_row_id_and_revision_is_rejected() {
        let mut rows = valid_50_rows();
        rows[1] = rows[0].clone();
        let errors = freeze_pilot_manifest(rows).expect_err("duplicate row must not freeze");
        assert!(errors
            .iter()
            .any(|e| matches!(e, PilotFreezeError::DuplicateRow { .. })));
    }

    #[test]
    fn missing_selection_reason_is_rejected() {
        let mut rows = valid_50_rows();
        rows[0].selection_reason = "   ".to_string();
        let errors = freeze_pilot_manifest(rows).expect_err("blank reason must not freeze");
        assert!(errors
            .iter()
            .any(|e| matches!(e, PilotFreezeError::MissingSelectionReason { .. })));
    }

    #[test]
    fn missing_reference_decision_is_rejected() {
        let mut rows = valid_50_rows();
        rows[0].reference_decision = String::new();
        let errors = freeze_pilot_manifest(rows).expect_err("blank reference must not freeze");
        assert!(errors
            .iter()
            .any(|e| matches!(e, PilotFreezeError::MissingReferenceDecision { .. })));
    }

    #[test]
    fn all_narrative_no_control_rows_is_rejected() {
        let mut rows = Vec::new();
        for i in 0..50 {
            rows.push(row(&format!("row-{i}"), 1, PilotRowKindV1::Narrative));
        }
        let errors = freeze_pilot_manifest(rows).expect_err("must include structured controls");
        assert!(errors.contains(&PilotFreezeError::NoStructuredControlRows));
    }

    #[test]
    fn all_control_no_narrative_rows_is_rejected() {
        let mut rows = Vec::new();
        for i in 0..50 {
            rows.push(row(
                &format!("row-{i}"),
                1,
                PilotRowKindV1::StructuredControl,
            ));
        }
        let errors = freeze_pilot_manifest(rows).expect_err("must include narrative candidates");
        assert!(errors.contains(&PilotFreezeError::NoNarrativeRows));
    }

    #[test]
    fn find_returns_the_matching_row() {
        let rows = valid_50_rows();
        let manifest = freeze_pilot_manifest(rows).expect("50 valid rows must freeze");
        let found = manifest.find("row-0", 1).expect("row-0@1 must be found");
        assert_eq!(found.row_id, "row-0");
    }
}
