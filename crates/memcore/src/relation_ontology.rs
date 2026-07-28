//! Relation ontology v1 (tachi#773 S1 — "定型+止血").
//!
//! Frozen design (issue #773 v4, "设计 v4(冻结稿)"): the legal edge-relation
//! vocabulary for **new writes** on the generic path is the existing scorer
//! weight table (`scorer::graph_relation_activation_weight`) plus `about`
//! (memory → anchor). `related_to` remains a legal *value* — historical rows
//! stay readable and keep scoring via the scorer's explicit
//! `"similar_to" | "related_to" | "merge_hint" => 0.55` arm — but it is no
//! longer accepted on new writes: tachi#773 item 2 retires the `auto_link`
//! emission path that used to be its only producer, so by the end of this
//! branch nothing in-tree writes it anymore.
//!
//! One extra grandfather, **caller-scoped** (Sol post-adjudication rework):
//! `component_governance_ops` (tachi#772, #796/#815) seeds `owns` /
//! `consumes` / `backflow_candidate` / `blocked_by` edges on every server
//! boot (`seed_component_records`, idempotent). #772 is explicitly gated out
//! of the #773 S1 scope ("component registry 行将来走同一 anchor 惯例,
//! S1 落地前不动" — v3 design comment) and is not migrated by this branch.
//!
//! Critically, the grandfathering is **not** an unconditional relation-name
//! allowance: the generic write path
//! ([`is_legal_new_relation`] / [`validate_relation_for_write`], reached by
//! every dynamic string caller — continuity projection, the NAPI `add_edge`
//! surface, etc.) **rejects** all four. Only `component_governance_ops`, via
//! the typed [`crate::db::add_component_governance_edge`] door that takes the
//! closed [`ComponentGovernanceRelation`] enum (never a string), may seed
//! them. This shuts the laundering paths (projection forwarding a dynamic
//! `relation="owns"`, NAPI whitewashing `"blocked_by"`) that a name-only
//! allow-list left open. Growing the set past four requires an explicit
//! #772/S2 ontology adjudication, tripwired below.
//!
//! Enforcement lives at memcore's edge-write choke point,
//! [`crate::db::add_edge`] — every write that mints an edge from *caller
//! input* funnels through it. It is not literally every `INSERT INTO
//! memory_edges` in the workspace: re-keying and repair statements write the
//! table with raw SQL and never reach this validator (re-verified 2026-07-26,
//! #1460) —
//!
//! - `crate::store::exact_dedupe::transfer_edges_to_winner` — re-points an
//!   existing row's endpoint, carrying its stored relation verbatim;
//! - `tachi-server`'s `repair::memory_hygiene` R12 rules — hard-coded
//!   `distilled_from` / `supersedes` literals;
//! - `tachi-server`'s `repair::plan_c::copy_common_rows` — column-wise merge
//!   of an attached alias DB.
//! - `scripts/migrate_antigravity_split.py` — an offline migration that copies
//!   the source tuple's stored relation and weight; it is not a runtime writer.
//!
//! None of the four takes a relation from a request/caller string — the
//! first two carry an already-stored value or an [`ONTOLOGY_V1`] literal
//! (`distilled_from`, `supersedes`), the third copies rows out of another
//! Tachi DB file — so the ontology stays closed against *newly minted* wild
//! strings. What they do mean is that nothing downstream may assume a stored
//! row passed through here. Wild strings already sitting in a DB from
//! before this branch stay readable (`get_edges` / `graph_expand` never filter
//! on legality) — only the write path is gated.

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

/// `related_to` is a legal stored *value* (grandfathered reads; the scorer
/// scores it at 0.55 via its explicit
/// `"similar_to" | "related_to" | "merge_hint"` arm, not the `_ => 0.50`
/// fallback) but is deprecated for new writes: its only producer
/// (`auto_link`) stops emitting it as of tachi#773 item 2.
pub const DEPRECATED_RELATION: &str = "related_to";

/// #772 component-governance relations (`seed_component_records`), gated out
/// of the #773 S1 ontology migration. See module docs.
///
/// This list is **documentation + tripwire anchor only** — it is deliberately
/// NOT consulted by [`is_legal_new_relation`], so the generic write path
/// rejects these names. The only writer is the typed
/// [`ComponentGovernanceRelation`] path. Keep the two in lockstep: every enum
/// variant's [`ComponentGovernanceRelation::as_str`] must appear here, and the
/// set is size-locked at four by the module's tripwire test.
pub const COMPONENT_GOVERNANCE_GRANDFATHERED: &[&str] =
    &["owns", "consumes", "backflow_candidate", "blocked_by"];

/// Closed set of #772 component-governance relations, the *only* type that can
/// reach [`crate::db::add_component_governance_edge`]. Being an enum (not a
/// string) is the caller-scoping mechanism: a dynamic string caller physically
/// cannot construct one, so the four grandfathered relations enter the graph
/// through exactly one typed door. Adding a variant is the deliberate #772/S2
/// decision point (and trips the size-lock test until the tripwire is updated).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComponentGovernanceRelation {
    Owns,
    Consumes,
    BackflowCandidate,
    BlockedBy,
}

impl ComponentGovernanceRelation {
    /// The stored `relation` string for this grandfathered relation. This is
    /// the authoritative value written to `memory_edges.relation`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Owns => "owns",
            Self::Consumes => "consumes",
            Self::BackflowCandidate => "backflow_candidate",
            Self::BlockedBy => "blocked_by",
        }
    }
}

/// Whether `relation` may be used on a *new* edge write via the **generic**
/// path ([`crate::db::add_edge`]).
///
/// Accepts exactly [`ONTOLOGY_V1`]. Rejects: anything else, the deprecated
/// [`DEPRECATED_RELATION`], empty/whitespace-only strings, **and the #772
/// [`COMPONENT_GOVERNANCE_GRANDFATHERED`] set** — those are caller-scoped to
/// the typed [`crate::db::add_component_governance_edge`] door and must not be
/// admissible from dynamic string callers (see module docs).
pub fn is_legal_new_relation(relation: &str) -> bool {
    let trimmed = relation.trim();
    if trimmed.is_empty() {
        return false;
    }
    if trimmed == DEPRECATED_RELATION {
        return false;
    }
    ONTOLOGY_V1.contains(&trimmed)
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
    fn component_governance_grandfathered_rejected_on_generic_path() {
        // Sol post-adjudication rework: the grandfathering is caller-scoped.
        // The generic path must REJECT all four — only the typed
        // add_component_governance_edge door may write them.
        for relation in COMPONENT_GOVERNANCE_GRANDFATHERED {
            assert!(
                !is_legal_new_relation(relation),
                "#772 grandfathered relation '{relation}' must be rejected on the generic write path"
            );
            assert!(
                validate_relation_for_write(relation).is_err(),
                "generic validate must reject grandfathered relation '{relation}'"
            );
        }
    }

    /// Size- and content-lock tripwire: the grandfathered exemption is a
    /// temporary #772 concession, not a growable side-vocabulary. This asserts
    /// the *exact* four-element set, so adding a fifth (or renaming one) turns
    /// this test red — forcing an explicit #772/S2 adjudication rather than a
    /// silent creep into a permanent second ontology. (The old test merely
    /// iterated the constant, so a longer list would still pass.)
    #[test]
    fn component_governance_grandfathered_set_is_size_and_content_locked() {
        assert_eq!(
            COMPONENT_GOVERNANCE_GRANDFATHERED.len(),
            4,
            "grandfathered set size changed — a new exemption needs #772/S2 sign-off"
        );
        assert_eq!(
            COMPONENT_GOVERNANCE_GRANDFATHERED,
            &["owns", "consumes", "backflow_candidate", "blocked_by"][..],
            "grandfathered set contents changed — a new exemption needs #772/S2 sign-off"
        );
    }

    /// Lockstep: every typed enum variant must map to a name in the
    /// documented grandfathered set (and only those names).
    #[test]
    fn component_governance_enum_matches_grandfathered_set() {
        use ComponentGovernanceRelation::*;
        for variant in [Owns, Consumes, BackflowCandidate, BlockedBy] {
            assert!(
                COMPONENT_GOVERNANCE_GRANDFATHERED.contains(&variant.as_str()),
                "enum variant {variant:?} ({}) missing from grandfathered set",
                variant.as_str()
            );
        }
    }
}
