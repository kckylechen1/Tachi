//! Wiki reviewed-projection lifecycle, provenance, and closure-boundary
//! types (#1072).
//!
//! Frozen design authority: `docs/engineering/architecture/issue-refinery-memory-lanes.md`
//! §7 (wiki is a reviewed projection) and §7.1 (reference compatibility and
//! closure state). This module implements the subset of that document's
//! target contracts #1072 lands as callable runtime shapes:
//! `KnowledgeArtifactV1`'s closed lifecycle/authority vocabulary,
//! `WikiEvidenceRefV1` (the canonical typed evidence-ref write shape, with
//! legacy `metadata.source_refs: string[]` retained as a read fallback), and
//! `ClosureProposalV1` / `ClosureApprovalReceiptV1` (the
//! `closure_candidate → pending_approval → applied` lifecycle's
//! hash-invalidation core).
//!
//! Deliberately NOT implemented in this leaf (left for a later leaf, and
//! explicitly checklisted in the #1072 PR body rather than hidden):
//!
//! - a generic `EngineReceiptV1` — no live wiki-write path in this leaf runs
//!   an engine that needs one (mirrors #1002/#1071's same deferral);
//! - an MCP-exposed `propose_closure`/`approve_closure`/`apply_closure`
//!   `tachi_task` action surface — the type + hash-invalidation logic below
//!   is real and unit-tested; wiring a new `TachiTaskAction` touches ~7
//!   exhaustively-matched call sites this leaf's no-cargo-build
//!   hand-verification budget cannot safely cover in one pass;
//! - full external trusted-doc blob-SHA drift detection for semantic
//!   staleness — the `wiki_ops::lint` fix landed alongside this module
//!   covers the "supersedes/contradicts edges" trigger from canon doc §7's
//!   required-behavior list; the blob-SHA-drift trigger needs #1002's
//!   `CanonicalDocRefV1` resolver wired into wiki writes, a separate leaf's
//!   worth of work.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

use crate::{canonical_json_sha256, sha256_hex, SourceKindV1};

// ─── §7: KnowledgeArtifactV1 lifecycle/authority vocabulary ────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WikiArtifactKindV1 {
    Draft,
    Wiki,
    Guide,
}

impl WikiArtifactKindV1 {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::Wiki => "wiki",
            Self::Guide => "guide",
        }
    }
}

impl fmt::Display for WikiArtifactKindV1 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Canon doc §7's closed lifecycle vocabulary:
/// `candidate | pending_review | active | stale | superseded | rejected`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WikiLifecycleV1 {
    Candidate,
    PendingReview,
    Active,
    Stale,
    Superseded,
    Rejected,
}

impl WikiLifecycleV1 {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Candidate => "candidate",
            Self::PendingReview => "pending_review",
            Self::Active => "active",
            Self::Stale => "stale",
            Self::Superseded => "superseded",
            Self::Rejected => "rejected",
        }
    }

    /// The fail-closed truthful-retrieval gate this leaf's frozen contract
    /// requires ("an unreviewed projection is never served as reviewed" /
    /// canon doc §7 "default search returns `active` artifacts only").
    /// Only `Active` is retrievable through the default (no explicit scope)
    /// wiki search/browse/read path.
    pub fn is_default_retrievable(self) -> bool {
        matches!(self, Self::Active)
    }
}

impl fmt::Display for WikiLifecycleV1 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for WikiLifecycleV1 {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim() {
            "candidate" => Ok(Self::Candidate),
            "pending_review" => Ok(Self::PendingReview),
            "active" => Ok(Self::Active),
            "stale" => Ok(Self::Stale),
            "superseded" => Ok(Self::Superseded),
            "rejected" => Ok(Self::Rejected),
            other => Err(format!("invalid wiki lifecycle '{other}'")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WikiAuthorityV1 {
    Advisory,
    Playbook,
}

impl WikiAuthorityV1 {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Advisory => "advisory",
            Self::Playbook => "playbook",
        }
    }
}

impl fmt::Display for WikiAuthorityV1 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for WikiAuthorityV1 {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim() {
            "advisory" => Ok(Self::Advisory),
            "playbook" => Ok(Self::Playbook),
            other => Err(format!("invalid wiki authority '{other}'")),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WikiReviewReceiptV1 {
    pub approver: String,
    pub decision: String,
    pub decided_at: String,
}

/// Canon doc §7.1's canonical typed evidence-ref write shape. Legacy
/// `metadata.source_refs: string[]` remains a read fallback. Deliberately a
/// lean subset of #1002's full `EvidenceRefV1`
/// (relation / immutable_revision / section_or_span): a bare wiki
/// `references[]` string (a URL, an absolute path, or a GitHub shorthand —
/// see `wiki_ops::references::validate_reference_format`) carries no
/// resolved immutable revision, and fabricating one here would be dishonest
/// evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WikiEvidenceRefV1 {
    #[serde(rename = "ref")]
    pub target_ref: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_kind: Option<SourceKindV1>,
    pub captured_at: String,
}

/// Best-effort classification reusing the same closed reference-format
/// vocabulary `wiki_ops::references::validate_reference_format` already
/// validates against (GitHub shorthand `#N` / `repo#N` / `owner/repo#N`,
/// and repo-relative `docs/...` spec paths). Ambiguous shapes (a bare URL or
/// absolute path could be almost any `SourceKindV1`) deliberately return
/// `None` rather than guess — an unclassified typed ref still retains its raw
/// `ref` string intact.
pub fn classify_wiki_reference(raw: &str) -> Option<SourceKindV1> {
    let trimmed = raw.trim();
    if trimmed.starts_with("docs/") || trimmed.starts_with("docs\\") {
        return Some(SourceKindV1::CanonicalDoc);
    }
    if is_github_issue_shorthand(trimmed) {
        return Some(SourceKindV1::Issue);
    }
    None
}

/// Hand-rolled equivalent of `wiki_ops::references`' GitHub-shorthand regex
/// (`^(?:#\d+|[a-zA-Z0-9_.-]+#\d+|[a-zA-Z0-9_-]+/[a-zA-Z0-9_.-]+#\d+)$`),
/// kept dependency-free (this crate does not otherwise depend on `regex`).
/// Best-effort classification only — see `classify_wiki_reference`'s doc.
fn is_github_issue_shorthand(s: &str) -> bool {
    let Some((prefix, rest)) = s.rsplit_once('#') else {
        return false;
    };
    if rest.is_empty() || !rest.chars().all(|c| c.is_ascii_digit()) {
        return false;
    }
    prefix.is_empty()
        || prefix
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-' | '/'))
}

/// Canonical-write builder: turns validated `references[]` strings into typed
/// `evidence_refs_v1` entries at write time. Readers retain a fallback for
/// legacy `metadata.source_refs: string[]` entries.
pub fn build_evidence_refs_v1(references: &[String], captured_at: &str) -> Vec<WikiEvidenceRefV1> {
    references
        .iter()
        .map(|reference| WikiEvidenceRefV1 {
            target_ref: reference.clone(),
            target_kind: classify_wiki_reference(reference),
            captured_at: captured_at.to_string(),
        })
        .collect()
}

/// Canon doc §7's `KnowledgeArtifactV1` shape. `source_bundle_hash` is
/// deliberately NOT `Option` (unlike `valid_from`/`valid_until`/
/// `review_receipt`) — the canon doc's own wire shape lists it without a
/// `?`, and the frozen invariant this leaf's cross-vendor review restored
/// (`Active ⇒ validated sources + approval`) depends on every constructed
/// artifact carrying a real source-bundle hash, not an easily-omitted
/// optional field. `engine_receipt` remains out of scope for this leaf (see
/// module doc) — no live write path constructs one yet, so adding the field
/// here would be an unenforced, dishonest gesture rather than a real
/// invariant.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KnowledgeArtifactV1 {
    pub artifact_kind: WikiArtifactKindV1,
    pub authority: WikiAuthorityV1,
    pub lifecycle: WikiLifecycleV1,
    pub scope: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub valid_from: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub valid_until: Option<String>,
    pub source_bundle_hash: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_receipt: Option<WikiReviewReceiptV1>,
}

/// Derives the effective `WikiLifecycleV1` for an already-stored wiki
/// memory entry's `metadata` + `path`, honoring (in priority order): an
/// explicit `metadata.lifecycle` (the vocabulary this leaf's own writer
/// stamps going forward), the pre-existing `metadata.review_status ==
/// "pending"` marker the REM wiki evolver (`foundry_runtime_ops::wiki_evolver`)
/// already stamps on `/wiki/drafts/...` entries, and finally the
/// `/wiki/drafts/` path convention itself as defense-in-depth for entries
/// missing both metadata markers. Defaults to `Active` — this is the
/// pre-#1072 behavior for every entry not otherwise marked (no lifecycle
/// field at all), so existing non-draft wiki reads stay behavior-frozen.
///
/// Fail-closed correction (cross-vendor review, #1215): a *present but
/// malformed* `metadata.lifecycle` string (garbage/typo, not simply absent)
/// used to fall through to every later check and could land on the `Active`
/// default — a corrupted/unrecognized lifecycle marker must never resolve to
/// the most-trusted state. It now resolves to `PendingReview` (not
/// default-retrievable) instead, regardless of path or `review_status`.
pub fn derive_wiki_lifecycle(metadata: &serde_json::Value, path: &str) -> WikiLifecycleV1 {
    if let Some(explicit) = metadata.get("lifecycle").and_then(|v| v.as_str()) {
        return explicit
            .parse::<WikiLifecycleV1>()
            .unwrap_or(WikiLifecycleV1::PendingReview);
    }
    if metadata
        .get("review_status")
        .and_then(|v| v.as_str())
        .is_some_and(|status| status.eq_ignore_ascii_case("pending"))
    {
        return WikiLifecycleV1::PendingReview;
    }
    if path == "/wiki/drafts" || path.starts_with("/wiki/drafts/") {
        return WikiLifecycleV1::PendingReview;
    }
    WikiLifecycleV1::Active
}

/// Companion to `derive_wiki_lifecycle`: reads the `authority` string
/// `wiki_ops`'s writer already stamps (`"advisory"` for `/wiki/...`,
/// `"playbook"` for `/guide/...`). Defaults to `Advisory` — the safer,
/// lower-authority default — when absent or unparseable.
pub fn derive_wiki_authority(metadata: &serde_json::Value) -> WikiAuthorityV1 {
    metadata
        .get("authority")
        .and_then(|v| v.as_str())
        .and_then(|s| s.parse::<WikiAuthorityV1>().ok())
        .unwrap_or(WikiAuthorityV1::Advisory)
}

/// Reads an already-stored `metadata.review_receipt` object back into a
/// typed `WikiReviewReceiptV1`, when present and well-formed. No live write
/// path in this leaf constructs one yet (see module doc) — this reader
/// exists so a future writer's receipt is honestly surfaced without another
/// wire-shape change.
pub fn derive_wiki_review_receipt(metadata: &serde_json::Value) -> Option<WikiReviewReceiptV1> {
    metadata
        .get("review_receipt")
        .cloned()
        .and_then(|v| serde_json::from_value(v).ok())
}

// ─── §7.1 closure boundary: ClosureProposalV1 / ClosureApprovalReceiptV1 ──

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClosureLifecycleV1 {
    ClosureCandidate,
    PendingApproval,
    Applied,
}

impl ClosureLifecycleV1 {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ClosureCandidate => "closure_candidate",
            Self::PendingApproval => "pending_approval",
            Self::Applied => "applied",
        }
    }
}

impl fmt::Display for ClosureLifecycleV1 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Canon doc §7.1's closure-boundary payload. `build_closure_proposal`
/// (below) is the only constructor this leaf ships, and it is deliberately
/// pure (no `&MemoryServer`, no I/O, not `async`) — the frozen contract's
/// closure boundary ("wiki evolution and closure synthesis... do not call
/// current `close_loop`, write `close_loop.json`, mark the task closed,
/// write active wiki, or post GitHub") is enforced *structurally*: a
/// function with this signature cannot reach any of those side effects, not
/// merely by convention.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClosureProposalV1 {
    pub proposal_id: String,
    pub lifecycle: ClosureLifecycleV1,
    pub issue_ref: String,
    pub wiki_title: String,
    pub wiki_text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wiki_path: Option<String>,
    pub doc_paths: Vec<String>,
    pub related_issues: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flow_id: Option<String>,
    /// `"explicit" | "flow_result" | "notes"` — where `wiki_title`/`wiki_text`
    /// were drafted from, mirroring `workflow_closure::handle_workflow`'s
    /// existing `draft_source` vocabulary for `close_loop` so a future apply
    /// step can reuse the same source-resolution convention.
    pub source_kind: String,
    pub proposal_hash: String,
    pub source_bundle_hash: String,
    pub captured_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClosureApprovalReceiptV1 {
    pub proposal_id: String,
    pub proposal_hash: String,
    pub source_bundle_hash: String,
    pub approver: String,
    pub decision: String,
    pub decided_at: String,
}

fn closure_proposal_hash_basis(
    issue_ref: &str,
    wiki_title: &str,
    wiki_text: &str,
    wiki_path: &Option<String>,
    doc_paths: &[String],
    related_issues: &[String],
) -> serde_json::Value {
    serde_json::json!({
        "issue_ref": issue_ref,
        "wiki_title": wiki_title,
        "wiki_text": wiki_text,
        "wiki_path": wiki_path,
        "doc_paths": doc_paths,
        "related_issues": related_issues,
    })
}

/// Pure builder for a `ClosureProposalV1`. See the struct doc for why this
/// signature (no I/O) is itself the RED-case-5 guarantee: "candidate closure
/// must leave flow state not closed and create no close-loop artifact/
/// comment." Starts at `PendingApproval` — this leaf ships no automatic
/// closure-synthesis producer that would need the raw `ClosureCandidate`
/// stage (see module doc); `propose_closure` IS the explicit "submit for
/// review" action.
#[allow(clippy::too_many_arguments)]
pub fn build_closure_proposal(
    proposal_id: String,
    issue_ref: String,
    wiki_title: String,
    wiki_text: String,
    wiki_path: Option<String>,
    doc_paths: Vec<String>,
    related_issues: Vec<String>,
    flow_id: Option<String>,
    source_kind: &str,
    source_bundle_content: &str,
    captured_at: String,
) -> Result<ClosureProposalV1, String> {
    let source_bundle_hash = sha256_hex(source_bundle_content.as_bytes());
    let basis = closure_proposal_hash_basis(
        &issue_ref,
        &wiki_title,
        &wiki_text,
        &wiki_path,
        &doc_paths,
        &related_issues,
    );
    let proposal_hash = canonical_json_sha256(&basis)?;
    Ok(ClosureProposalV1 {
        proposal_id,
        lifecycle: ClosureLifecycleV1::PendingApproval,
        issue_ref,
        wiki_title,
        wiki_text,
        wiki_path,
        doc_paths,
        related_issues,
        flow_id,
        source_kind: source_kind.to_string(),
        proposal_hash,
        source_bundle_hash,
        captured_at,
    })
}

fn recompute_proposal_hash(proposal: &ClosureProposalV1) -> Result<String, String> {
    let basis = closure_proposal_hash_basis(
        &proposal.issue_ref,
        &proposal.wiki_title,
        &proposal.wiki_text,
        &proposal.wiki_path,
        &proposal.doc_paths,
        &proposal.related_issues,
    );
    canonical_json_sha256(&basis)
}

/// Canon doc §7.1's replay-staleness guarantee ("a changed proposal or
/// source snapshot invalidates approval; apply cannot replay it" — RED case
/// 6): recomputes both hashes from whatever the caller currently has in
/// hand — the STORED proposal object (tamper/mutation check) and, when
/// re-resolvable, the CURRENT source content (drift check, e.g. a flow's
/// `result.md` overwritten by a later run after approval) — and refuses to
/// authorize apply unless both still match the approval receipt's pinned
/// hashes. `current_source_bundle_content: None` means the source cannot be
/// re-resolved at apply time (e.g. an explicit-text proposal with no
/// `flow_id`) — the drift check is then skipped, since there is nothing
/// mutable to have drifted; the tamper check on the proposal itself still
/// runs unconditionally.
pub fn check_closure_apply_preconditions(
    proposal: &ClosureProposalV1,
    receipt: &ClosureApprovalReceiptV1,
    current_source_bundle_content: Option<&str>,
) -> Result<(), String> {
    if receipt.proposal_id != proposal.proposal_id {
        return Err(format!(
            "approval receipt proposal_id '{}' does not match proposal '{}'",
            receipt.proposal_id, proposal.proposal_id
        ));
    }
    if !receipt.decision.eq_ignore_ascii_case("approved") {
        return Err(format!(
            "proposal '{}' was not approved (decision={}) — apply refused",
            proposal.proposal_id, receipt.decision
        ));
    }
    let recomputed_proposal_hash = recompute_proposal_hash(proposal)?;
    if recomputed_proposal_hash != proposal.proposal_hash {
        return Err(format!(
            "proposal '{}' has a corrupted proposal_hash field — apply refused",
            proposal.proposal_id
        ));
    }
    if recomputed_proposal_hash != receipt.proposal_hash {
        return Err(format!(
            "proposal '{}' content changed since approval (proposal_hash mismatch) — apply refused, approval cannot be replayed",
            proposal.proposal_id
        ));
    }
    if let Some(current_source) = current_source_bundle_content {
        let current_source_bundle_hash = sha256_hex(current_source.as_bytes());
        if current_source_bundle_hash != receipt.source_bundle_hash {
            return Err(format!(
                "proposal '{}' source snapshot changed since approval (source_bundle_hash mismatch) — apply refused, approval cannot be replayed",
                proposal.proposal_id
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // ─── lifecycle/authority vocabulary ────────────────────────────────

    #[test]
    fn wiki_lifecycle_wire_values_and_retrievability() {
        assert_eq!(WikiLifecycleV1::Active.as_str(), "active");
        assert_eq!(WikiLifecycleV1::PendingReview.as_str(), "pending_review");
        assert!(WikiLifecycleV1::Active.is_default_retrievable());
        for non_active in [
            WikiLifecycleV1::Candidate,
            WikiLifecycleV1::PendingReview,
            WikiLifecycleV1::Stale,
            WikiLifecycleV1::Superseded,
            WikiLifecycleV1::Rejected,
        ] {
            assert!(
                !non_active.is_default_retrievable(),
                "{non_active} must not be default-retrievable"
            );
        }
    }

    #[test]
    fn wiki_lifecycle_round_trips_through_from_str() {
        for lifecycle in [
            WikiLifecycleV1::Candidate,
            WikiLifecycleV1::PendingReview,
            WikiLifecycleV1::Active,
            WikiLifecycleV1::Stale,
            WikiLifecycleV1::Superseded,
            WikiLifecycleV1::Rejected,
        ] {
            let parsed: WikiLifecycleV1 = lifecycle.as_str().parse().expect("parse");
            assert_eq!(parsed, lifecycle);
        }
        assert!("bogus".parse::<WikiLifecycleV1>().is_err());
    }

    #[test]
    fn knowledge_artifact_round_trips() {
        let artifact = KnowledgeArtifactV1 {
            artifact_kind: WikiArtifactKindV1::Wiki,
            authority: WikiAuthorityV1::Advisory,
            lifecycle: WikiLifecycleV1::Active,
            scope: "global".to_string(),
            valid_from: None,
            valid_until: None,
            source_bundle_hash: "deadbeef".to_string(),
            review_receipt: None,
        };
        let wire = serde_json::to_string(&artifact).expect("serialize");
        let back: KnowledgeArtifactV1 = serde_json::from_str(&wire).expect("deserialize");
        assert_eq!(back, artifact);
        assert!(wire.contains("\"lifecycle\":\"active\""));
    }

    // ─── derive_wiki_lifecycle: truthful-retrieval gate (RED case 2) ──────

    #[test]
    fn derive_wiki_lifecycle_defaults_active_for_ordinary_wiki_write() {
        let lifecycle = derive_wiki_lifecycle(&serde_json::json!({}), "/wiki/engineering/foo");
        assert_eq!(lifecycle, WikiLifecycleV1::Active);
    }

    #[test]
    fn derive_wiki_lifecycle_honors_rem_evolver_review_status_pending() {
        let lifecycle = derive_wiki_lifecycle(
            &serde_json::json!({"review_status": "pending"}),
            "/wiki/drafts/some-slug",
        );
        assert_eq!(lifecycle, WikiLifecycleV1::PendingReview);
    }

    #[test]
    fn derive_wiki_lifecycle_falls_back_to_drafts_path_without_metadata_marker() {
        // Defense-in-depth: even without an explicit metadata marker, the
        // `/wiki/drafts/` path convention itself must not resolve to Active.
        let lifecycle = derive_wiki_lifecycle(&serde_json::json!({}), "/wiki/drafts/unmarked");
        assert_eq!(lifecycle, WikiLifecycleV1::PendingReview);
    }

    #[test]
    fn derive_wiki_lifecycle_prefers_explicit_lifecycle_field() {
        let lifecycle = derive_wiki_lifecycle(
            &serde_json::json!({"lifecycle": "stale", "review_status": "pending"}),
            "/wiki/engineering/foo",
        );
        assert_eq!(lifecycle, WikiLifecycleV1::Stale);
    }

    /// Cross-vendor review (#1215, BUG 1): "Malformed lifecycle values
    /// silently default to Active (knowledge_artifact.rs:254–270)." A
    /// present-but-garbage `metadata.lifecycle` string must fail CLOSED
    /// (`PendingReview`, not default-retrievable) — never fall through to
    /// the most-trusted `Active` default. This is the RED that the old
    /// `if let Ok(parsed) = ... { return parsed }` — with no `else` —
    /// allowed: parse failure silently continued past the explicit-field
    /// check into the drafts-path/default-Active fallback below.
    #[test]
    fn derive_wiki_lifecycle_malformed_explicit_value_fails_closed_to_pending_review() {
        let lifecycle = derive_wiki_lifecycle(
            &serde_json::json!({"lifecycle": "bogus-not-a-real-lifecycle"}),
            "/wiki/engineering/ordinary-path",
        );
        assert_eq!(
            lifecycle,
            WikiLifecycleV1::PendingReview,
            "malformed lifecycle must fail closed, not default to Active"
        );
        assert!(!lifecycle.is_default_retrievable());
    }

    #[test]
    fn derive_wiki_authority_defaults_advisory() {
        assert_eq!(
            derive_wiki_authority(&serde_json::json!({})),
            WikiAuthorityV1::Advisory
        );
        assert_eq!(
            derive_wiki_authority(&serde_json::json!({"authority": "playbook"})),
            WikiAuthorityV1::Playbook
        );
    }

    // ─── evidence_refs_v1 canonical typed refs (RED case 3/7) ──────────

    #[test]
    fn classify_wiki_reference_recognizes_github_shorthand_and_docs_paths() {
        assert_eq!(
            classify_wiki_reference("kckylechen1/tachi#1072"),
            Some(SourceKindV1::Issue)
        );
        assert_eq!(classify_wiki_reference("#1072"), Some(SourceKindV1::Issue));
        assert_eq!(
            classify_wiki_reference("docs/engineering/architecture/foo.md"),
            Some(SourceKindV1::CanonicalDoc)
        );
        assert_eq!(
            classify_wiki_reference("https://example.com/README.md"),
            None,
            "ambiguous URL shape must not be guessed"
        );
    }

    #[test]
    fn build_evidence_refs_v1_preserves_raw_ref_and_dual_writes_alongside_strings() {
        let refs = build_evidence_refs_v1(
            &[
                "kckylechen1/tachi#1072".to_string(),
                "https://example.com".to_string(),
            ],
            "2026-07-17T00:00:00Z",
        );
        assert_eq!(refs.len(), 2);
        assert_eq!(refs[0].target_ref, "kckylechen1/tachi#1072");
        assert_eq!(refs[0].target_kind, Some(SourceKindV1::Issue));
        assert_eq!(refs[1].target_ref, "https://example.com");
        assert_eq!(refs[1].target_kind, None);
        let wire = serde_json::to_value(&refs[0]).expect("serialize");
        assert_eq!(wire["ref"], serde_json::json!("kckylechen1/tachi#1072"));
    }

    // ─── closure boundary (RED case 5/6) ───────────────────────────────

    fn sample_proposal() -> ClosureProposalV1 {
        build_closure_proposal(
            "proposal-1".to_string(),
            "kckylechen1/tachi#1072".to_string(),
            "Wiki lifecycle lesson".to_string(),
            "Body text of the durable lesson.".to_string(),
            Some("/wiki/engineering/closure-boundary".to_string()),
            vec!["docs/engineering/architecture/issue-refinery-memory-lanes.md".to_string()],
            vec![],
            Some("flow-abc".to_string()),
            "flow_result",
            "original result.md content",
            "2026-07-17T00:00:00Z".to_string(),
        )
        .expect("build proposal")
    }

    #[test]
    fn build_closure_proposal_is_pure_and_starts_pending_approval() {
        let proposal = sample_proposal();
        assert_eq!(proposal.lifecycle, ClosureLifecycleV1::PendingApproval);
        assert!(!proposal.proposal_hash.is_empty());
        assert!(!proposal.source_bundle_hash.is_empty());
        // Determinism: same inputs -> same hash, so a later re-derivation
        // (e.g. `apply`'s tamper check) can recompute and compare.
        let proposal_again = sample_proposal();
        assert_eq!(proposal.proposal_hash, proposal_again.proposal_hash);
    }

    fn approve(proposal: &ClosureProposalV1) -> ClosureApprovalReceiptV1 {
        ClosureApprovalReceiptV1 {
            proposal_id: proposal.proposal_id.clone(),
            proposal_hash: proposal.proposal_hash.clone(),
            source_bundle_hash: proposal.source_bundle_hash.clone(),
            approver: "owner".to_string(),
            decision: "approved".to_string(),
            decided_at: "2026-07-17T00:05:00Z".to_string(),
        }
    }

    #[test]
    fn apply_preconditions_pass_when_nothing_changed() {
        let proposal = sample_proposal();
        let receipt = approve(&proposal);
        // RED (naive apply): a check that never re-hashes the current source
        // would also return Ok here, so this alone doesn't discriminate —
        // paired with the two failure tests below, it proves the checker
        // distinguishes "unchanged" from "changed" rather than always
        // passing.
        assert!(check_closure_apply_preconditions(
            &proposal,
            &receipt,
            Some("original result.md content"),
        )
        .is_ok());
    }

    #[test]
    fn apply_preconditions_reject_changed_proposal_content_red_case_6a() {
        let proposal = sample_proposal();
        let receipt = approve(&proposal);
        let mut tampered = proposal.clone();
        tampered.wiki_text = "A different body written after approval.".to_string();
        // Re-derive `proposal_hash` from the tampered content so `tampered`
        // is internally self-consistent (as it would be after a real
        // edit-and-resubmit, e.g. another `build_closure_proposal` call) —
        // this exercises the receipt/proposal MISMATCH check (case 6a's
        // actual target). Leaving the stale `proposal_hash` field on the
        // clone instead trips the *earlier* internal-corruption guard
        // (`recomputed_proposal_hash != proposal.proposal_hash`, "has a
        // corrupted proposal_hash field") before the receipt comparison is
        // ever reached — a different failure mode than this RED case
        // documents, and not what "cannot be replayed" describes.
        tampered.proposal_hash =
            recompute_proposal_hash(&tampered).expect("recompute tampered proposal hash");
        // RED: a naive apply that only checks `receipt.decision == "approved"`
        // and never recomputes the proposal hash would let this replay
        // silently. GREEN: the mismatch is caught and apply is refused.
        let err = check_closure_apply_preconditions(&tampered, &receipt, None)
            .expect_err("tampered proposal content must fail preconditions");
        assert!(err.contains("cannot be replayed"), "err: {err}");
    }

    #[test]
    fn apply_preconditions_reject_changed_source_snapshot_red_case_6b() {
        let proposal = sample_proposal();
        let receipt = approve(&proposal);
        // RED: a naive apply that never re-reads/re-hashes the current
        // source (e.g. a flow's result.md overwritten by a later run after
        // approval) would replay the stale approval. GREEN: the drift is
        // caught and apply is refused.
        let err = check_closure_apply_preconditions(
            &proposal,
            &receipt,
            Some("a DIFFERENT result.md content written after approval"),
        )
        .expect_err("changed source snapshot must fail preconditions");
        assert!(err.contains("cannot be replayed"), "err: {err}");
    }

    #[test]
    fn apply_preconditions_reject_unapproved_or_mismatched_receipt() {
        let proposal = sample_proposal();
        let mut rejected = approve(&proposal);
        rejected.decision = "rejected".to_string();
        assert!(check_closure_apply_preconditions(&proposal, &rejected, None).is_err());

        let mut wrong_id = approve(&proposal);
        wrong_id.proposal_id = "some-other-proposal".to_string();
        assert!(check_closure_apply_preconditions(&proposal, &wrong_id, None).is_err());
    }

    #[test]
    fn apply_preconditions_skip_source_drift_check_when_source_unresolvable() {
        // An explicit-text proposal with no flow_id has nothing mutable to
        // re-read at apply time; `current_source_bundle_content: None` must
        // not be treated as a mismatch.
        let proposal = build_closure_proposal(
            "proposal-2".to_string(),
            "kckylechen1/tachi#1072".to_string(),
            "Explicit lesson".to_string(),
            "Explicit body, no flow_id.".to_string(),
            None,
            vec![],
            vec![],
            None,
            "explicit",
            "explicit body, no flow_id.",
            "2026-07-17T00:00:00Z".to_string(),
        )
        .expect("build proposal");
        let receipt = approve(&proposal);
        assert!(check_closure_apply_preconditions(&proposal, &receipt, None).is_ok());
    }
}
