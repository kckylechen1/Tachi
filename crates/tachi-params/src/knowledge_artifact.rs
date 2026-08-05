//! Wiki reviewed-projection lifecycle and provenance types (#1072).
//!
//! Frozen design authority: `docs/engineering/architecture/issue-refinery-memory-lanes.md`
//! §7 (wiki is a reviewed projection) and §7.1 (reference compatibility and
//! closure state). This module implements the subset of that document's
//! target contracts #1072 lands as callable runtime shapes:
//! `KnowledgeArtifactV1`'s closed lifecycle/authority vocabulary and
//! `WikiEvidenceRefV1` (the canonical typed evidence-ref write shape, with
//! legacy `metadata.source_refs: string[]` retained as a read fallback).
//!
//! §7.1's `ClosureProposalV1` / `ClosureApprovalReceiptV1` types and their
//! `closure_candidate → pending_approval → applied` hash-invalidation core
//! also shipped here, ahead of the `propose_closure`/`approve_closure`/
//! `apply_closure` surface that would have called them. That surface was
//! never built, so the types sat with zero production callers and were
//! deleted in #1564. Canon §7.1 is unaffected: it still requires an
//! independent review and approval receipt before `close_loop`, active-wiki
//! writes, or GitHub writeback, and whoever builds that surface should
//! derive the payload from the canon doc against the code as it stands then.
//!
//! Deliberately NOT implemented in this leaf (left for a later leaf, and
//! explicitly checklisted in the #1072 PR body rather than hidden):
//!
//! - a generic `EngineReceiptV1` — no live wiki-write path in this leaf runs
//!   an engine that needs one (mirrors #1002/#1071's same deferral);
//! - an MCP-exposed `propose_closure`/`approve_closure`/`apply_closure`
//!   `tachi_task` action surface — wiring a new `TachiTaskAction` touches ~7
//!   exhaustively-matched call sites this leaf's no-cargo-build
//!   hand-verification budget cannot safely cover in one pass;
//! - full external trusted-doc blob-SHA drift detection for semantic
//!   staleness — the `wiki_ops::lint` fix landed alongside this module
//!   covers the "supersedes/contradicts edges" trigger from canon doc §7's
//!   required-behavior list; the blob-SHA-drift trigger needs #1002's
//!   `CanonicalDocRefV1` resolver wired into wiki writes, a separate leaf's
//!   worth of work.

use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fmt;
use std::path::Path;
use std::str::FromStr;

use crate::SourceKindV1;

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

impl FromStr for WikiArtifactKindV1 {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim() {
            "draft" => Ok(Self::Draft),
            "wiki" => Ok(Self::Wiki),
            "guide" => Ok(Self::Guide),
            other => Err(format!("invalid wiki artifact kind '{other}'")),
        }
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

/// Semantic applicability scope for a Wiki/guide artifact. This is
/// intentionally independent from the physical database selected by
/// `project=`. In particular, storage in the named `wiki` database does not
/// imply either `Project` or `Shared`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WikiKnowledgeScopeV1 {
    Project,
    Shared,
    Unspecified,
}

impl WikiKnowledgeScopeV1 {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Project => "project",
            Self::Shared => "shared",
            Self::Unspecified => "unspecified",
        }
    }
}

impl fmt::Display for WikiKnowledgeScopeV1 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for WikiKnowledgeScopeV1 {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim() {
            "project" => Ok(Self::Project),
            "shared" => Ok(Self::Shared),
            "unspecified" => Ok(Self::Unspecified),
            other => Err(format!("invalid wiki knowledge scope '{other}'")),
        }
    }
}

/// Closed, normalized applicability dimensions used by Wiki and guide
/// readers. The singular wire keys `task_type` and `stage` are retained for
/// compatibility with the existing guide metadata contract; every value is
/// normalized to a sorted, deduplicated string array.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WikiApplicabilityV1 {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub projects: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub repos: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub domains: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub task_type: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub profiles: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stage: Vec<String>,
}

impl WikiApplicabilityV1 {
    pub fn is_empty(&self) -> bool {
        self.projects.is_empty()
            && self.repos.is_empty()
            && self.domains.is_empty()
            && self.task_type.is_empty()
            && self.profiles.is_empty()
            && self.stage.is_empty()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WikiApplicabilityStatusV1 {
    Bounded,
    Unspecified,
    Malformed,
}

impl WikiApplicabilityStatusV1 {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Bounded => "bounded",
            Self::Unspecified => "unspecified",
            Self::Malformed => "malformed",
        }
    }
}

impl FromStr for WikiApplicabilityStatusV1 {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim() {
            "bounded" => Ok(Self::Bounded),
            "unspecified" => Ok(Self::Unspecified),
            "malformed" => Ok(Self::Malformed),
            other => Err(format!("invalid wiki applicability status '{other}'")),
        }
    }
}

/// One read-time representation shared by Wiki and guide producers/readers.
/// `validation_issues` is deliberately explicit: malformed or legacy fields
/// are never silently interpreted as universal applicability.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EffectiveKnowledgeArtifactV1 {
    pub artifact_kind: WikiArtifactKindV1,
    pub knowledge_scope: WikiKnowledgeScopeV1,
    pub origin_projects: Vec<String>,
    pub applies_to: WikiApplicabilityV1,
    pub applicability_status: WikiApplicabilityStatusV1,
    pub known_exceptions: Vec<String>,
    pub lifecycle: WikiLifecycleV1,
    pub authority: WikiAuthorityV1,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub validation_issues: Vec<String>,
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
/// malformed* `metadata.lifecycle` value (garbage/typo/non-string, not absent)
/// used to fall through to every later check and could land on the `Active`
/// default — a corrupted/unrecognized lifecycle marker must never resolve to
/// the most-trusted state. It now resolves to `PendingReview` (not
/// default-retrievable) instead, regardless of path or `review_status`.
pub fn derive_wiki_lifecycle(metadata: &serde_json::Value, path: &str) -> WikiLifecycleV1 {
    if let Some(explicit) = metadata.get("lifecycle") {
        return explicit
            .as_str()
            .and_then(|value| value.parse::<WikiLifecycleV1>().ok())
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
    let receipt = metadata
        .get("review_receipt")
        .cloned()
        .and_then(|v| serde_json::from_value::<WikiReviewReceiptV1>(v).ok())?;
    if receipt.approver.trim().is_empty()
        || !matches!(
            receipt.decision.trim().to_ascii_lowercase().as_str(),
            "approved" | "rejected"
        )
        || chrono::DateTime::parse_from_rfc3339(receipt.decided_at.trim()).is_err()
    {
        return None;
    }
    Some(receipt)
}

fn artifact_kind_from_path(path: &str) -> WikiArtifactKindV1 {
    if path == "/guide" || path.starts_with("/guide/") {
        WikiArtifactKindV1::Guide
    } else if path == "/wiki/drafts" || path.starts_with("/wiki/drafts/") {
        WikiArtifactKindV1::Draft
    } else {
        WikiArtifactKindV1::Wiki
    }
}

fn normalize_string_array(value: &serde_json::Value) -> Result<Vec<String>, ()> {
    let values = match value {
        serde_json::Value::String(value) => vec![value.as_str()],
        serde_json::Value::Array(values) => values
            .iter()
            .map(|value| value.as_str().ok_or(()))
            .collect::<Result<Vec<_>, _>>()?,
        _ => return Err(()),
    };
    let normalized = values
        .into_iter()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    Ok(normalized)
}

fn parse_optional_string_array(
    metadata: &serde_json::Value,
    key: &str,
    validation_issues: &mut Vec<String>,
) -> Vec<String> {
    let Some(value) = metadata.get(key) else {
        return Vec::new();
    };
    match normalize_string_array(value) {
        Ok(values) => values,
        Err(()) => {
            validation_issues.push(format!("malformed_{key}"));
            Vec::new()
        }
    }
}

fn derive_legacy_origin_projects(metadata: &serde_json::Value) -> Vec<String> {
    for pointer in [
        "/provenance/project",
        "/provenance/context/project",
        "/source_project",
        "/repo",
    ] {
        if let Some(project) = metadata
            .pointer(pointer)
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            return vec![project.to_string()];
        }
    }
    let Some(db_path) = metadata
        .pointer("/provenance/db_path")
        .and_then(serde_json::Value::as_str)
    else {
        return Vec::new();
    };
    let path = Path::new(db_path);
    let project = if path
        .parent()
        .and_then(Path::file_name)
        .is_some_and(|name| name == ".tachi")
    {
        path.parent()
            .and_then(Path::parent)
            .and_then(Path::file_name)
    } else {
        path.parent().and_then(Path::file_name)
    };
    project
        .and_then(|name| name.to_str())
        .map(str::trim)
        .filter(|name| !name.is_empty() && *name != "wiki" && *name != ".tachi")
        .map(|name| vec![name.to_string()])
        .unwrap_or_default()
}

fn parse_wiki_applicability(
    metadata: &serde_json::Value,
    validation_issues: &mut Vec<String>,
) -> (WikiApplicabilityV1, WikiApplicabilityStatusV1) {
    let Some(value) = metadata.get("applies_to") else {
        return (
            WikiApplicabilityV1::default(),
            WikiApplicabilityStatusV1::Unspecified,
        );
    };
    let Some(object) = value.as_object() else {
        validation_issues.push("malformed_applies_to".to_string());
        return (
            WikiApplicabilityV1::default(),
            WikiApplicabilityStatusV1::Malformed,
        );
    };
    const KEYS: [&str; 6] = [
        "projects",
        "repos",
        "domains",
        "task_type",
        "profiles",
        "stage",
    ];
    if object.keys().any(|key| !KEYS.contains(&key.as_str())) {
        validation_issues.push("malformed_applies_to".to_string());
        return (
            WikiApplicabilityV1::default(),
            WikiApplicabilityStatusV1::Malformed,
        );
    }
    let parse = |key: &str| -> Result<Vec<String>, ()> {
        object
            .get(key)
            .map(normalize_string_array)
            .transpose()
            .map(Option::unwrap_or_default)
    };
    let applicability = (|| {
        Ok::<_, ()>(WikiApplicabilityV1 {
            projects: parse("projects")?,
            repos: parse("repos")?,
            domains: parse("domains")?,
            task_type: parse("task_type")?,
            profiles: parse("profiles")?,
            stage: parse("stage")?,
        })
    })();
    match applicability {
        Ok(applicability) if applicability.is_empty() => {
            (applicability, WikiApplicabilityStatusV1::Unspecified)
        }
        Ok(applicability) => (applicability, WikiApplicabilityStatusV1::Bounded),
        Err(()) => {
            validation_issues.push("malformed_applies_to".to_string());
            (
                WikiApplicabilityV1::default(),
                WikiApplicabilityStatusV1::Malformed,
            )
        }
    }
}

/// Project one stored Wiki/guide row into the single effective runtime
/// representation. The physical store is intentionally not an input: it can
/// prove where a row lives, but cannot prove where its advice applies.
pub fn derive_effective_knowledge_artifact(
    metadata: &serde_json::Value,
    path: &str,
    legacy_entry_scope: &str,
) -> EffectiveKnowledgeArtifactV1 {
    let mut validation_issues = Vec::new();

    let artifact_kind = match metadata.get("artifact_kind") {
        None => artifact_kind_from_path(path),
        Some(value) => value
            .as_str()
            .and_then(|value| value.parse::<WikiArtifactKindV1>().ok())
            .unwrap_or_else(|| {
                validation_issues.push("malformed_artifact_kind".to_string());
                artifact_kind_from_path(path)
            }),
    };

    let mut lifecycle = derive_wiki_lifecycle(metadata, path);
    if metadata.get("lifecycle").is_some_and(|value| {
        value
            .as_str()
            .and_then(|value| value.parse::<WikiLifecycleV1>().ok())
            .is_none()
    }) {
        lifecycle = WikiLifecycleV1::PendingReview;
        validation_issues.push("malformed_lifecycle".to_string());
    }
    if metadata.get("lifecycle").is_none() && artifact_kind == WikiArtifactKindV1::Guide {
        lifecycle = match metadata.get("status") {
            None => WikiLifecycleV1::PendingReview,
            Some(value) => value
                .as_str()
                .and_then(|value| value.parse::<WikiLifecycleV1>().ok())
                .unwrap_or_else(|| {
                    validation_issues.push("malformed_lifecycle".to_string());
                    WikiLifecycleV1::PendingReview
                }),
        };
    }

    let authority = match metadata.get("authority") {
        None => WikiAuthorityV1::Advisory,
        Some(value) => value
            .as_str()
            .and_then(|value| value.parse::<WikiAuthorityV1>().ok())
            .unwrap_or_else(|| {
                validation_issues.push("malformed_authority".to_string());
                WikiAuthorityV1::Advisory
            }),
    };

    let knowledge_scope = match metadata.get("knowledge_scope") {
        Some(value) => value
            .as_str()
            .and_then(|value| value.parse::<WikiKnowledgeScopeV1>().ok())
            .unwrap_or_else(|| {
                validation_issues.push("malformed_knowledge_scope".to_string());
                WikiKnowledgeScopeV1::Unspecified
            }),
        None => {
            let legacy_scope = metadata
                .get("scope")
                .and_then(serde_json::Value::as_str)
                .unwrap_or(legacy_entry_scope);
            if legacy_scope.eq_ignore_ascii_case("project") {
                WikiKnowledgeScopeV1::Project
            } else {
                validation_issues.push("legacy_scope_unresolved".to_string());
                WikiKnowledgeScopeV1::Unspecified
            }
        }
    };

    let has_typed_origin_projects = metadata.get("origin_projects").is_some();
    let mut origin_projects =
        parse_optional_string_array(metadata, "origin_projects", &mut validation_issues);
    if metadata.get("origin_projects").is_none() && origin_projects.is_empty() {
        origin_projects = derive_legacy_origin_projects(metadata);
        if !origin_projects.is_empty() {
            validation_issues.push("legacy_origin_derived".to_string());
        }
    }
    let known_exceptions =
        parse_optional_string_array(metadata, "known_exceptions", &mut validation_issues);
    let (applies_to, mut applicability_status) =
        parse_wiki_applicability(metadata, &mut validation_issues);
    let review_receipt = derive_wiki_review_receipt(metadata);
    if metadata.get("review_receipt").is_some() && review_receipt.is_none() {
        validation_issues.push("malformed_review_receipt".to_string());
    }
    if let Some(declared_status) = metadata.get("applicability_status") {
        match declared_status
            .as_str()
            .and_then(|value| value.parse::<WikiApplicabilityStatusV1>().ok())
        {
            Some(WikiApplicabilityStatusV1::Malformed) => {
                applicability_status = WikiApplicabilityStatusV1::Malformed;
                validation_issues.push("malformed_applicability_status".to_string());
            }
            Some(_) => {}
            None => {
                applicability_status = WikiApplicabilityStatusV1::Malformed;
                validation_issues.push("malformed_applicability_status".to_string());
            }
        }
    }
    if validation_issues
        .iter()
        .any(|issue| issue.starts_with("malformed_"))
    {
        lifecycle = WikiLifecycleV1::PendingReview;
        applicability_status = WikiApplicabilityStatusV1::Malformed;
    }
    if knowledge_scope == WikiKnowledgeScopeV1::Shared
        && applicability_status != WikiApplicabilityStatusV1::Malformed
    {
        if !has_typed_origin_projects {
            validation_issues.push("shared_scope_missing_typed_origin".to_string());
        }
        if !has_typed_origin_projects
            || origin_projects.is_empty()
            || applicability_status == WikiApplicabilityStatusV1::Unspecified
        {
            applicability_status = WikiApplicabilityStatusV1::Unspecified;
            validation_issues.push("shared_scope_not_bounded".to_string());
        }
    }
    if knowledge_scope == WikiKnowledgeScopeV1::Shared && lifecycle == WikiLifecycleV1::Active {
        let bounded = applicability_status == WikiApplicabilityStatusV1::Bounded;
        let reviewed = metadata
            .get("source_bundle_hash")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|hash| !hash.trim().is_empty())
            && review_receipt
                .is_some_and(|receipt| receipt.decision.eq_ignore_ascii_case("approved"));
        if !bounded {
            validation_issues.push("shared_active_without_bounded_applicability".to_string());
        }
        if !reviewed {
            validation_issues.push("shared_active_without_review".to_string());
        }
        if !bounded || !reviewed {
            lifecycle = WikiLifecycleV1::PendingReview;
        }
    }
    validation_issues.sort();
    validation_issues.dedup();

    EffectiveKnowledgeArtifactV1 {
        artifact_kind,
        knowledge_scope,
        origin_projects,
        applies_to,
        applicability_status,
        known_exceptions,
        lifecycle,
        authority,
        validation_issues,
    }
}

/// Canonical fields for an unreviewed producer write. `requested_scope` is
/// the semantic write request (`project`, `shared`, or the legacy alias
/// `global`); physical database placement is deliberately absent.
pub fn build_candidate_knowledge_artifact_fields(
    path: &str,
    requested_scope: &str,
    proposal_metadata: &serde_json::Value,
) -> serde_json::Value {
    let artifact_kind = artifact_kind_from_path(path);
    let authority = if artifact_kind == WikiArtifactKindV1::Guide {
        WikiAuthorityV1::Playbook
    } else {
        WikiAuthorityV1::Advisory
    };
    let knowledge_scope = match requested_scope.trim().to_ascii_lowercase().as_str() {
        "project" => WikiKnowledgeScopeV1::Project,
        "global" | "shared" => WikiKnowledgeScopeV1::Shared,
        _ => WikiKnowledgeScopeV1::Unspecified,
    };
    let mut validation_issues = Vec::new();
    let origin_projects =
        parse_optional_string_array(proposal_metadata, "origin_projects", &mut validation_issues);
    let known_exceptions = parse_optional_string_array(
        proposal_metadata,
        "known_exceptions",
        &mut validation_issues,
    );
    let (applies_to, applicability_status) =
        parse_wiki_applicability(proposal_metadata, &mut validation_issues);
    if knowledge_scope == WikiKnowledgeScopeV1::Shared
        && (origin_projects.is_empty()
            || applicability_status == WikiApplicabilityStatusV1::Unspecified)
    {
        validation_issues.push("shared_scope_not_bounded".to_string());
    }
    validation_issues.sort();
    validation_issues.dedup();
    serde_json::json!({
        "artifact_kind": artifact_kind.as_str(),
        "knowledge_scope": knowledge_scope.as_str(),
        "origin_projects": origin_projects,
        "applies_to": applies_to,
        "applicability_status": applicability_status.as_str(),
        "known_exceptions": known_exceptions,
        "lifecycle": WikiLifecycleV1::PendingReview.as_str(),
        "authority": authority.as_str(),
        "artifact_metadata_warnings": validation_issues,
    })
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

    #[test]
    fn effective_artifact_keeps_physical_wiki_placement_out_of_semantic_scope() {
        let metadata = build_candidate_knowledge_artifact_fields(
            "/wiki/engineering/review",
            "global",
            &serde_json::json!({
                "origin_projects": ["Sigil"],
                "applies_to": {"repos": ["Sigil", "Quant_Analyzer_2026"]},
            }),
        );
        let effective =
            derive_effective_knowledge_artifact(&metadata, "/wiki/engineering/review", "global");
        assert_eq!(effective.knowledge_scope, WikiKnowledgeScopeV1::Shared);
        assert_eq!(effective.origin_projects, vec!["Sigil"]);
        assert_eq!(
            effective.applies_to.repos,
            vec!["Quant_Analyzer_2026", "Sigil"]
        );
        assert_eq!(
            effective.applicability_status,
            WikiApplicabilityStatusV1::Bounded
        );
        assert_eq!(effective.lifecycle, WikiLifecycleV1::PendingReview);
    }

    #[test]
    fn legacy_global_scope_is_not_promoted_to_universal_shared_scope() {
        let effective = derive_effective_knowledge_artifact(
            &serde_json::json!({"scope": "global"}),
            "/wiki/legacy",
            "global",
        );
        assert_eq!(effective.knowledge_scope, WikiKnowledgeScopeV1::Unspecified);
        assert_eq!(
            effective.applicability_status,
            WikiApplicabilityStatusV1::Unspecified
        );
        assert!(effective
            .validation_issues
            .contains(&"legacy_scope_unresolved".to_string()));
    }

    #[test]
    fn legacy_origin_is_derived_from_provenance_without_widening_scope() {
        let effective = derive_effective_knowledge_artifact(
            &serde_json::json!({
                "scope": "project",
                "provenance": {
                    "db_path": "/work/Quant_Analyzer_2026/.tachi/memory.db"
                }
            }),
            "/wiki/quant/lesson",
            "project",
        );
        assert_eq!(effective.knowledge_scope, WikiKnowledgeScopeV1::Project);
        assert_eq!(effective.origin_projects, vec!["Quant_Analyzer_2026"]);
        assert!(effective
            .validation_issues
            .contains(&"legacy_origin_derived".to_string()));
    }

    #[test]
    fn malformed_applicability_fails_closed() {
        let effective = derive_effective_knowledge_artifact(
            &serde_json::json!({
                "knowledge_scope": "shared",
                "origin_projects": ["Sigil"],
                "applies_to": {"repos": ["Sigil", 42]},
            }),
            "/wiki/engineering/review",
            "global",
        );
        assert_eq!(
            effective.applicability_status,
            WikiApplicabilityStatusV1::Malformed
        );
        assert!(effective.applies_to.is_empty());
        assert!(effective
            .validation_issues
            .contains(&"malformed_applies_to".to_string()));

        let candidate = build_candidate_knowledge_artifact_fields(
            "/guide/shared",
            "shared",
            &serde_json::json!({
                "origin_projects": ["Sigil"],
                "applies_to": {"repos": ["Sigil", 42]},
            }),
        );
        let candidate_effective =
            derive_effective_knowledge_artifact(&candidate, "/guide/shared", "global");
        assert_eq!(
            candidate_effective.applicability_status,
            WikiApplicabilityStatusV1::Malformed
        );
    }

    #[test]
    fn malformed_typed_identity_fields_fail_closed() {
        for (field, value, expected_issue) in [
            (
                "artifact_kind",
                serde_json::json!(42),
                "malformed_artifact_kind",
            ),
            (
                "knowledge_scope",
                serde_json::json!("not-a-scope"),
                "malformed_knowledge_scope",
            ),
            (
                "authority",
                serde_json::json!({"forged": true}),
                "malformed_authority",
            ),
        ] {
            let mut metadata = serde_json::json!({
                "artifact_kind": "guide",
                "knowledge_scope": "project",
                "lifecycle": "active",
                "authority": "playbook",
                "applies_to": {"repos": ["kckylechen1/tachi"]},
            });
            metadata[field] = value;

            let effective =
                derive_effective_knowledge_artifact(&metadata, "/guide/review", "project");
            assert_eq!(
                effective.lifecycle,
                WikiLifecycleV1::PendingReview,
                "RED: malformed {field} remained default-retrievable"
            );
            assert_eq!(
                effective.applicability_status,
                WikiApplicabilityStatusV1::Malformed,
                "RED: malformed {field} retained applicable authority"
            );
            assert!(
                effective
                    .validation_issues
                    .contains(&expected_issue.to_string()),
                "missing validation issue for {field}: {:?}",
                effective.validation_issues
            );
        }
    }

    #[test]
    fn non_string_lifecycle_and_unreviewed_active_shared_fail_closed() {
        let malformed = derive_effective_knowledge_artifact(
            &serde_json::json!({"lifecycle": 42}),
            "/wiki/malformed-lifecycle",
            "project",
        );
        assert_eq!(malformed.lifecycle, WikiLifecycleV1::PendingReview);

        let shared = derive_effective_knowledge_artifact(
            &serde_json::json!({
                "knowledge_scope": "shared",
                "origin_projects": ["Sigil"],
                "applies_to": {"repos": ["Sigil", "Quant_Analyzer_2026"]},
                "lifecycle": "active",
                "authority": "advisory",
            }),
            "/wiki/shared-unreviewed",
            "global",
        );
        assert_eq!(shared.lifecycle, WikiLifecycleV1::PendingReview);
        assert!(shared
            .validation_issues
            .contains(&"shared_active_without_review".to_string()));
    }

    #[test]
    fn malformed_review_receipt_cannot_activate_shared_knowledge() {
        let shared = derive_effective_knowledge_artifact(
            &serde_json::json!({
                "knowledge_scope": "shared",
                "origin_projects": ["Sigil"],
                "applies_to": {"repos": ["Sigil", "Quant_Analyzer_2026"]},
                "lifecycle": "active",
                "authority": "advisory",
                "source_bundle_hash": "reviewed-source-bundle",
                "review_receipt": {
                    "approver": "",
                    "decision": "approved",
                    "decided_at": "not-a-date"
                }
            }),
            "/wiki/shared-malformed-review",
            "global",
        );
        assert_eq!(shared.lifecycle, WikiLifecycleV1::PendingReview);
        assert!(shared
            .validation_issues
            .contains(&"malformed_review_receipt".to_string()));
    }

    #[test]
    fn reviewed_but_unbounded_shared_knowledge_stays_pending() {
        let shared = derive_effective_knowledge_artifact(
            &serde_json::json!({
                "artifact_kind": "wiki",
                "knowledge_scope": "shared",
                "origin_projects": ["Sigil"],
                "lifecycle": "active",
                "authority": "advisory",
                "source_bundle_hash": "reviewed-source-bundle",
                "review_receipt": {
                    "approver": "owner",
                    "decision": "approved",
                    "decided_at": "2026-07-31T00:00:00Z"
                }
            }),
            "/wiki/shared-unbounded",
            "global",
        );
        assert_eq!(
            shared.lifecycle,
            WikiLifecycleV1::PendingReview,
            "RED: review approval activated shared knowledge without an applicability boundary"
        );
        assert_eq!(
            shared.applicability_status,
            WikiApplicabilityStatusV1::Unspecified
        );
        assert!(shared
            .validation_issues
            .contains(&"shared_active_without_bounded_applicability".to_string()));
    }

    #[test]
    fn declared_malformed_applicability_status_fails_closed() {
        let effective = derive_effective_knowledge_artifact(
            &serde_json::json!({
                "artifact_kind": "wiki",
                "knowledge_scope": "project",
                "applies_to": {"repos": ["kckylechen1/tachi"]},
                "applicability_status": "malformed",
                "lifecycle": "active",
                "authority": "advisory"
            }),
            "/wiki/project-malformed-status",
            "project",
        );
        assert_eq!(
            effective.lifecycle,
            WikiLifecycleV1::PendingReview,
            "RED: declared malformed applicability remained default-retrievable"
        );
        assert_eq!(
            effective.applicability_status,
            WikiApplicabilityStatusV1::Malformed
        );
        assert!(effective
            .validation_issues
            .contains(&"malformed_applicability_status".to_string()));
    }

    #[test]
    fn shared_active_requires_typed_origin_not_legacy_provenance() {
        let shared = derive_effective_knowledge_artifact(
            &serde_json::json!({
                "artifact_kind": "wiki",
                "knowledge_scope": "shared",
                "applies_to": {"repos": ["kckylechen1/tachi"]},
                "lifecycle": "active",
                "authority": "advisory",
                "source_bundle_hash": "reviewed-source-bundle",
                "review_receipt": {
                    "approver": "owner",
                    "decision": "approved",
                    "decided_at": "2026-07-31T00:00:00Z"
                },
                "provenance": {
                    "db_path": "/work/Sigil/.tachi/memory.db"
                }
            }),
            "/wiki/shared-derived-origin",
            "global",
        );
        assert_eq!(shared.origin_projects, vec!["Sigil"]);
        assert_eq!(
            shared.lifecycle,
            WikiLifecycleV1::PendingReview,
            "RED: legacy-derived origin activated typed shared knowledge"
        );
        assert_eq!(
            shared.applicability_status,
            WikiApplicabilityStatusV1::Unspecified
        );
        assert!(shared
            .validation_issues
            .contains(&"shared_scope_missing_typed_origin".to_string()));
    }

    #[test]
    fn legacy_guide_without_typed_lifecycle_is_pending_and_advisory() {
        let effective = derive_effective_knowledge_artifact(
            &serde_json::json!({}),
            "/guide/generated/review",
            "global",
        );
        assert_eq!(effective.artifact_kind, WikiArtifactKindV1::Guide);
        assert_eq!(effective.lifecycle, WikiLifecycleV1::PendingReview);
        assert_eq!(effective.authority, WikiAuthorityV1::Advisory);
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
        for explicit in [
            serde_json::json!("bogus-not-a-real-lifecycle"),
            serde_json::json!(123),
            serde_json::json!({"bad": true}),
        ] {
            let lifecycle = derive_wiki_lifecycle(
                &serde_json::json!({"lifecycle": explicit}),
                "/wiki/engineering/ordinary-path",
            );
            assert_eq!(
                lifecycle,
                WikiLifecycleV1::PendingReview,
                "malformed lifecycle must fail closed, not default to Active"
            );
            assert!(!lifecycle.is_default_retrievable());
        }
    }

    /// tachi#1561 residual: memcore's search/list surfaces cannot call this
    /// crate's `derive_wiki_lifecycle` directly — `tachi-params/Cargo.toml`
    /// depends on `memcore`, so the reverse edge would be circular — so
    /// `memcore::namespace::is_non_default_retrievable_wiki_row` is a second,
    /// independent implementation of the same retrievability boolean
    /// (`derive_wiki_lifecycle(...).is_default_retrievable()`, negated).
    /// This crate is the only one that can see both sides of the mirror
    /// (it already depends on memcore), so it is the pinning point: every
    /// fixture below must classify identically on both sides, or the mirror
    /// has drifted.
    ///
    /// Scoped to `/wiki`-rooted paths, matching
    /// `is_non_default_retrievable_wiki_row`'s own documented scope (a row
    /// outside `/wiki` never earned a Wiki lifecycle marker in the first
    /// place, so the two sides are not expected to agree there).
    #[test]
    fn memcore_wiki_lifecycle_gate_matches_derive_wiki_lifecycle() {
        fn wiki_entry(path: &str, metadata: serde_json::Value) -> memcore::MemoryEntry {
            serde_json::from_value(serde_json::json!({
                "id": "memcore-parity-pin",
                "text": "cross-crate lifecycle gate parity fixture",
                "timestamp": "2026-01-01T00:00:00Z",
                "path": path,
                "metadata": metadata,
            }))
            .expect("construct a minimal MemoryEntry for the parity fixture")
        }

        let fixtures: Vec<(&str, serde_json::Value)> = vec![
            ("/wiki/engineering/foo", serde_json::json!({})),
            ("/wiki/drafts/unmarked", serde_json::json!({})),
            (
                "/wiki/drafts/some-slug",
                serde_json::json!({"review_status": "pending"}),
            ),
            (
                "/wiki/drafts/some-slug",
                serde_json::json!({"review_status": "Pending"}),
            ),
            (
                "/wiki/engineering/foo",
                serde_json::json!({"lifecycle": "active"}),
            ),
            (
                // Explicit `active` wins over the drafts-path convention —
                // same priority order `derive_wiki_lifecycle` documents.
                "/wiki/drafts/promoted",
                serde_json::json!({"lifecycle": "active"}),
            ),
            (
                "/wiki/engineering/foo",
                serde_json::json!({"lifecycle": "stale"}),
            ),
            (
                "/wiki/engineering/foo",
                serde_json::json!({"lifecycle": "pending_review"}),
            ),
            (
                "/wiki/engineering/foo",
                serde_json::json!({"lifecycle": "candidate"}),
            ),
            (
                "/wiki/engineering/foo",
                serde_json::json!({"lifecycle": "superseded"}),
            ),
            (
                "/wiki/engineering/foo",
                serde_json::json!({"lifecycle": "rejected"}),
            ),
            (
                "/wiki/engineering/foo",
                serde_json::json!({"lifecycle": "bogus-not-a-real-lifecycle"}),
            ),
            (
                "/wiki/engineering/foo",
                serde_json::json!({"lifecycle": 123}),
            ),
            (
                "/wiki/engineering/foo",
                serde_json::json!({"lifecycle": {"bad": true}}),
            ),
            (
                "/wiki/engineering/foo",
                serde_json::json!({"lifecycle": "stale", "review_status": "pending"}),
            ),
        ];

        for (path, metadata) in fixtures {
            let entry = wiki_entry(path, metadata.clone());
            let tachi_params_excluded =
                !derive_wiki_lifecycle(&metadata, path).is_default_retrievable();
            let memcore_excluded = memcore::is_non_default_retrievable_wiki_row(&entry);
            assert_eq!(
                tachi_params_excluded, memcore_excluded,
                "memcore's gate and tachi-params' derive_wiki_lifecycle disagree for \
                 path={path:?} metadata={metadata:?}: tachi_params_excluded={tachi_params_excluded}, \
                 memcore_excluded={memcore_excluded}"
            );
        }
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
}
