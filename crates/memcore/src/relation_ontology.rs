//! Relation ontology v1 (tachi#773 S1 — "定型+止血").
//!
//! Frozen design (issue #773 v4, "设计 v4(冻结稿)"): the legal edge-relation
//! vocabulary for **new writes** is the existing scorer weight table
//! (`scorer::graph_relation_activation_weight`) plus `about`
//! (memory → anchor). `related_to` remains a legal *value* — historical rows
//! stay readable and keep scoring via the scorer's `_ => 0.50` fallback arm —
//! but it is no longer accepted on new writes: tachi#773 item 2 retires the
//! `auto_link` emission path that used to be its only producer, so by the
//! end of this branch nothing in-tree writes it anymore.
//!
//! One extra grandfather: `component_governance_ops` (tachi#772, #796/#815)
//! seeds `owns` / `consumes` / `backflow_candidate` / `blocked_by` edges on
//! every server boot (`seed_component_records`, idempotent). #772 is
//! explicitly gated out of the #773 S1 scope ("component registry 行将来走
//! 同一 anchor 惯例, S1 落地前不动" — v3 design comment) and is not migrated
//! by this branch. Rejecting those relations here would turn a live,
//! intentional write path into a boot-time failure, which is not a S1 goal.
//! They are listed separately below so the #772 migration can delete this
//! block without touching the v1 ontology proper.
//!
//! Enforcement lives at memcore's single edge-write choke point,
//! [`crate::db::add_edge`] — every `INSERT INTO memory_edges` in the
//! workspace funnels through it (verified via
//! `rg 'INSERT INTO memory_edges'`). Wild strings already sitting in a DB
//! from before this branch stay readable (`get_edges` / `graph_expand` never
//! filter on legality) — only the write path is gated.

use crate::error::MemoryError;

/// Ontology v1: exactly the scorer's named weight-table vocabulary, plus the
/// new `about` relation (memory → anchor, tachi#773 item 4).
///
/// Keep this list in lockstep with
/// `scorer::graph_relation_activation_weight`'s match arms — a relation that
/// scores via a named arm but isn't legal to write (or vice versa) is a bug.
pub const ONTOLOGY_V1: &[&str] = &[
    "about",
    "causes",
    "contradicts",
    "derived_from",
    "distilled_from",
    "elaborates",
    "fixed_by",
    "follows",
    "merge_hint",
    "references",
    "reinforces",
    "rejected_because",
    "similar_to",
    "supersedes",
    "supports",
];

/// `related_to` is a legal stored *value* (grandfathered reads, scorer
/// fallback still scores it at 0.55) but is deprecated for new writes: its
/// only producer (`auto_link`) stops emitting it as of tachi#773 item 2.
pub const DEPRECATED_RELATION: &str = "related_to";

/// #772 component-governance relations (`seed_component_records`), gated out
/// of the #773 S1 ontology migration. See module docs.
pub const COMPONENT_GOVERNANCE_GRANDFATHERED: &[&str] =
    &["owns", "consumes", "backflow_candidate", "blocked_by"];

/// Whether `relation` may be used on a *new* edge write.
///
/// Rejects: anything not in [`ONTOLOGY_V1`], the deprecated
/// [`DEPRECATED_RELATION`], empty/whitespace-only strings. Accepts the #772
/// grandfathered set unconditionally (see module docs).
pub fn is_legal_new_relation(relation: &str) -> bool {
    let trimmed = relation.trim();
    if trimmed.is_empty() {
        return false;
    }
    if trimmed == DEPRECATED_RELATION {
        return false;
    }
    ONTOLOGY_V1.contains(&trimmed) || COMPONENT_GOVERNANCE_GRANDFATHERED.contains(&trimmed)
}

/// Validate a relation for a new edge write, returning a
/// [`MemoryError::InvalidArg`] listing the legal set on rejection.
pub fn validate_relation_for_write(relation: &str) -> Result<(), MemoryError> {
    if is_legal_new_relation(relation) {
        return Ok(());
    }
    Err(MemoryError::InvalidArg(format!(
        "illegal edge relation '{relation}': legal set is [{}] (deprecated: '{}' no longer accepted on new writes)",
        ONTOLOGY_V1.join(", "),
        DEPRECATED_RELATION,
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ontology_v1_matches_scorer_named_arms() {
        // Every named (non-wildcard, non-deprecated) scorer arm must be legal.
        for relation in [
            "supports",
            "elaborates",
            "causes",
            "fixed_by",
            "reinforces",
            "follows",
            "references",
            "distilled_from",
            "derived_from",
            "similar_to",
            "merge_hint",
            "supersedes",
            "contradicts",
            "rejected_because",
        ] {
            assert!(
                is_legal_new_relation(relation),
                "scorer arm '{relation}' must be a legal new-write relation"
            );
        }
    }

    #[test]
    fn about_is_legal() {
        assert!(is_legal_new_relation("about"));
    }

    #[test]
    fn related_to_is_deprecated_not_legal_for_new_writes() {
        assert!(!is_legal_new_relation("related_to"));
        let err = validate_relation_for_write("related_to").unwrap_err();
        assert!(err.to_string().contains("related_to"));
    }

    #[test]
    fn unknown_relation_rejected_with_legal_set_in_message() {
        assert!(!is_legal_new_relation("shares_entities"));
        let err = validate_relation_for_write("shares_entities").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("shares_entities"));
        assert!(msg.contains("causes"));
        assert!(msg.contains("about"));
    }

    #[test]
    fn empty_or_whitespace_relation_rejected() {
        assert!(!is_legal_new_relation(""));
        assert!(!is_legal_new_relation("   "));
        assert!(validate_relation_for_write("").is_err());
    }

    #[test]
    fn component_governance_grandfathered_relations_remain_legal() {
        for relation in COMPONENT_GOVERNANCE_GRANDFATHERED {
            assert!(
                is_legal_new_relation(relation),
                "#772 grandfathered relation '{relation}' must stay legal until its own migration"
            );
        }
    }
}
