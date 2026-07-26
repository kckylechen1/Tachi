//! Free-text adjudication to principle-level precedent candidates (#1076).
//!
//! `precedent_ops` (#950 slice 1) persists a caller-supplied ruling
//! *verbatim, one row per verdict*, at `/precedents/<project>/<shortid>`.
//! This module is a sibling decomposition step: it fans each already-captured
//! ruling out into *one pending candidate row per principle cited*, at
//! `/precedent_candidates/<project>/<shortid>`. Both paths run independently
//! off the same `rulings[]` intake (`TachiCompleteParams::rulings`) — this
//! module never re-derives or replaces that intake, per the issue's frozen
//! "do not reimplement `rulings[]` intake" boundary; it reuses
//! `precedent_ops`'s validation/normalization/scrub/dedup building blocks
//! (`normalize_ruling`, `scrub_ruling`, `project_segment`, `frame_field`,
//! `extract_persisted_id`) rather than duplicating them.
//!
//! Three hard boundaries, matching #1076's frozen contract:
//!
//! 1. **Candidates only, never established.** Every row this module writes
//!    carries `metadata.candidate_status = "pending"`, unconditionally — no
//!    code path here ever writes anything else. Promotion to an established
//!    precedent is #1077's gate, entirely outside this module.
//! 2. **Fan-out preserves, never harmonizes.** A ruling's `case`,
//!    `options_considered`, `ruling`, `outcome`, and `overturned_by` are
//!    copied *verbatim* onto every principle-candidate it produces — a
//!    losing alternative or an owner self-overturn can never be silently
//!    dropped or summarized away by the fan-out. Content fields are never
//!    truncated. Every candidate carries every source ref that survived
//!    per-kind immutable-revision validation (`normalize_source_ref`) — a
//!    ref that FAILED validation (missing the snapshot hash its
//!    `target_kind` requires) is dropped from `source_refs` but never
//!    silently: it is recorded in `metadata.source_ref_warnings`, and
//!    `metadata.coverage` degrades from `"full"` to `"partial"` to reflect
//!    it (#1183 fix-round finding 6 — an unconditional `"full"` marker
//!    would have hidden dropped evidence behind a claim of completeness).
//!    `metadata.authority_complete` degrades the same way: it requires an
//!    adjudicator, at least one immutably-pinned source ref, AND zero
//!    dropped refs (finding 5) — a flag #1077's establishment gate must be
//!    able to trust is *earned*, not defaulted true.
//! 3. **Fail-safe.** A malformed ruling, a ruling with no `principles_cited`
//!    (nothing principle-level to emit), or a malformed individual
//!    `source_ref` is skipped/warned and never fails the enclosing
//!    `complete` call.
//!
//! ## Candidate identity
//!
//! Two deterministic hashes are derived per (ruling, principle) pair, both
//! built the same length-prefix-framing way as `precedent_ops::frame_field`
//! (so no field's content can bleed into an adjacent one):
//!
//! - `candidate_group_id` hashes project + `issue_ref` + case + options +
//!   ruling + *this one* principle + outcome + `overturned_by` — the
//!   candidate's case/principle identity, independent of *which* evidence
//!   or *which* engine identity backs it.
//! - `candidate_short_id` (the row's path segment) additionally folds in the
//!   adjudicator, every normalized source ref's full identity tuple
//!   (relation/kind/ref/comment_id/updated_at/body_hash/commit_sha/span),
//!   and the engine receipt's identity fields (provider/model/version/
//!   fallback_chain/degraded) — see that function's doc for why the receipt
//!   is folded in too (#1183 fix-round finding 10).
//!
//! An exact replay (byte-identical ruling, same source refs, same engine
//! receipt) re-derives the same `candidate_short_id` and the same rendered
//! `text`, so `save_memory`'s exact-path+exact-text dedup gate collapses it
//! onto the existing row — idempotent, per #1076 RED case 3. An edited owner
//! comment (same `comment_id`, different `updated_at`/`body_hash`) or an
//! improved engine receipt (e.g. `preview_only` -> `known`) changes the
//! `candidate_short_id` seed, so it is NOT treated as the same replay: a new
//! row is appended at a new path, sharing the old row's `candidate_group_id`
//! (so a later reader can tell the two rows are revisions of the same
//! case/principle claim) while the old row is left untouched — this module
//! never edits or deletes a previously-written candidate.

use serde_json::{json, Map, Value};

use crate::memory_search_ops::{
    save_eval_memory_with_authorized_reference_mutations, scrub_secrets,
};
use crate::precedent_ops::{
    extract_persisted_id, frame_field, normalize_ruling, project_segment, scrub_ruling,
    summary_line_with_prefix, NormalizedRuling,
};
use crate::tool_params::{
    RulingEngineReceiptParams, RulingSourceRefParams, SaveMemoryParams, TachiCompleteParams,
};
use crate::MemoryServer;

/// Closed vocabulary for `RulingSourceRefParams::target_kind`, matching the
/// canon doc's `SourceKindV1` subset relevant to an adjudication's evidence
/// (`docs/engineering/architecture/issue-refinery-memory-lanes.md` §3),
/// spelled out explicitly here (rather than reusing that doc's enum type)
/// per this leaf's own wire-shape rationale — see `RulingSourceRefParams`'s
/// doc comment.
const VALID_TARGET_KINDS: [&str; 6] = [
    "issue",
    "comment",
    "pr",
    "commit",
    "canonical_doc",
    "verification",
];

/// Closed vocabulary for `RulingSourceRefParams::relation`, matching the
/// canon doc's `EvidenceRelationV1`.
const VALID_RELATIONS: [&str; 5] = [
    "derived_from",
    "supports",
    "contradicts",
    "supersedes",
    "applies_to",
];

/// A validated, scrubbed `RulingSourceRefParams`.
struct NormalizedSourceRef {
    relation: String,
    target_kind: String,
    target_ref: String,
    comment_id: Option<String>,
    updated_at: Option<String>,
    body_hash: Option<String>,
    commit_sha: Option<String>,
    section_or_span: Option<String>,
}

impl NormalizedSourceRef {
    fn validated_append(&self) -> Result<memcore::db::ValidatedReferenceMutation, String> {
        memcore::db::ValidatedReferenceMutation::precedent_source(
            self.relation.clone(),
            self.target_kind.clone(),
            self.target_ref.clone(),
            self.comment_id.clone(),
            self.updated_at.clone(),
            self.body_hash.clone(),
            self.commit_sha.clone(),
            self.section_or_span.clone(),
        )
        .map_err(|error| format!("validate precedent source ref append: {error}"))
    }
}

/// Validate + normalize one caller-supplied source ref. `Err(reason)` marks
/// it malformed so the caller can skip + warn without dropping the whole
/// candidate.
fn normalize_source_ref(raw: &RulingSourceRefParams) -> Result<NormalizedSourceRef, String> {
    let target_kind = raw.target_kind.trim().to_ascii_lowercase();
    if target_kind.is_empty() {
        return Err("source_ref missing required `target_kind`".to_string());
    }
    if !VALID_TARGET_KINDS.contains(&target_kind.as_str()) {
        return Err(format!(
            "source_ref has invalid `target_kind` {target_kind:?} (expected one of {VALID_TARGET_KINDS:?})"
        ));
    }
    let target_ref = raw.target_ref.trim().to_string();
    if target_ref.is_empty() {
        return Err("source_ref missing required `target_ref`".to_string());
    }
    let relation = raw
        .relation
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_ascii_lowercase)
        .unwrap_or_else(|| "supports".to_string());
    if !VALID_RELATIONS.contains(&relation.as_str()) {
        return Err(format!(
            "source_ref has invalid `relation` {relation:?} (expected one of {VALID_RELATIONS:?})"
        ));
    }
    let comment_id = trimmed_opt(&raw.comment_id);
    if target_kind == "comment" && comment_id.is_none() {
        return Err(
            "source_ref with target_kind \"comment\" requires `comment_id` to pin an immutable revision"
                .to_string(),
        );
    }
    let body_hash = trimmed_opt(&raw.body_hash);
    let commit_sha = trimmed_opt(&raw.commit_sha);
    // #1183 fix-round finding 5: the frozen contract requires resolving
    // "immutable issue/comment/PR/doc/verification refs, and source
    // snapshot hashes" *before* decomposition — a source_ref that names a
    // target but carries no snapshot hash for that kind isn't actually
    // pinned to an immutable revision, it's a bare pointer that can drift
    // out from under the ruling. Per `RulingSourceRefParams::body_hash`'s
    // own doc ("comment body hash, issue body hash, PR snapshot hash, blob
    // SHA, ... depending on target_kind"), every kind needs a snapshot hash;
    // `commit` uses `commit_sha` instead of `body_hash` for that hash.
    // Rejecting here (not just degrading a flag) means only refs that are
    // ACTUALLY immutably pinned ever reach `authority_complete`'s
    // `!source_refs.is_empty()` check — see `record_complete_precedent_candidates`.
    match target_kind.as_str() {
        "commit" if commit_sha.is_none() => {
            return Err(
                "source_ref with target_kind \"commit\" requires `commit_sha` to pin an immutable revision"
                    .to_string(),
            );
        }
        "comment" | "issue" | "pr" | "canonical_doc" | "verification" if body_hash.is_none() => {
            return Err(format!(
                "source_ref with target_kind {target_kind:?} requires `body_hash` (a source snapshot hash) \
                 to pin an immutable revision"
            ));
        }
        _ => {}
    }
    let updated_at = trimmed_opt(&raw.updated_at)
        .map(|value| {
            chrono::DateTime::parse_from_rfc3339(&value)
                .map(|timestamp| {
                    timestamp
                        .with_timezone(&chrono::Utc)
                        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
                })
                .map_err(|_| format!("source_ref has invalid `updated_at` timestamp {value:?}"))
        })
        .transpose()?;
    Ok(NormalizedSourceRef {
        relation,
        target_kind,
        target_ref,
        comment_id,
        updated_at,
        body_hash,
        commit_sha,
        section_or_span: trimmed_opt(&raw.section_or_span),
    })
}

fn trimmed_opt(value: &Option<String>) -> Option<String> {
    value
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// Scrub a normalized source ref's free-text-shaped fields in place
/// (`target_ref` and `section_or_span` are the only fields that can carry
/// arbitrary prose rather than a bare identifier/hash).
fn scrub_source_ref(r: &mut NormalizedSourceRef) {
    let (target_ref, _) = scrub_secrets(&r.target_ref);
    r.target_ref = target_ref;
    if let Some(span) = &r.section_or_span {
        let (safe, _) = scrub_secrets(span);
        r.section_or_span = Some(safe);
    }
}

/// Dedup exact-string-identical principles within one ruling while
/// preserving first-seen order, so a caller that accidentally repeats a
/// principle string doesn't get two identical candidate rows for it.
fn dedup_principles(principles: &[String]) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    principles
        .iter()
        .filter(|p| seen.insert((*p).clone()))
        .cloned()
        .collect()
}

/// #1076 required behavior: "Record effective provider/model/version/
/// fallback/degraded receipt. Unknown/fallback identity is preview-only."
/// Collapsed to the two states the sentence actually distinguishes: a fully
/// known, non-fallback, non-degraded identity is `"known"`; everything else
/// (absent receipt, missing provider/model/**version**, a non-empty fallback
/// chain, or an explicitly degraded run) is `"preview_only"`. `version` is
/// one of the three fields the frozen contract names ("provider/model/
/// version") — #1183 fix-round finding 7: this previously ignored a `None`
/// `effective_version` and still returned `"known"` whenever provider+model
/// were present, mislabeling an incomplete engine identity as fully known.
fn identity_status(receipt: Option<&RulingEngineReceiptParams>) -> &'static str {
    let Some(receipt) = receipt else {
        return "preview_only";
    };
    let has_provider = receipt
        .effective_provider
        .as_deref()
        .is_some_and(|s| !s.trim().is_empty());
    let has_model = receipt
        .effective_model
        .as_deref()
        .is_some_and(|s| !s.trim().is_empty());
    let has_version = receipt
        .effective_version
        .as_deref()
        .is_some_and(|s| !s.trim().is_empty());
    if !has_provider
        || !has_model
        || !has_version
        || !receipt.fallback_chain.is_empty()
        || receipt.degraded
    {
        "preview_only"
    } else {
        "known"
    }
}

/// SHA1-based deterministic hash (same primitive as
/// `precedent_ops::precedent_short_id`), truncated to 16 hex chars.
fn hash16(seed: &str) -> String {
    let hashed = uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_OID, seed.as_bytes());
    hashed.simple().to_string()[..16].to_string()
}

/// The case/principle identity seed shared by `candidate_group_id` and
/// `candidate_short_id` — see module doc "Candidate identity".
fn group_seed(
    project: &str,
    issue_ref: Option<&str>,
    principle: &str,
    n: &NormalizedRuling,
) -> String {
    let mut seed = String::new();
    seed.push_str(&frame_field(project));
    seed.push_str(&frame_field(issue_ref.unwrap_or("")));
    seed.push_str(&frame_field(&n.case));
    seed.push_str(&frame_field(n.options_considered.as_deref().unwrap_or("")));
    seed.push_str(&frame_field(&n.ruling));
    seed.push_str(&frame_field(principle));
    seed.push_str(&frame_field(&n.outcome));
    seed.push_str(&frame_field(n.overturned_by.as_deref().unwrap_or("")));
    seed
}

fn candidate_group_id(
    project: &str,
    issue_ref: Option<&str>,
    principle: &str,
    n: &NormalizedRuling,
) -> String {
    hash16(&group_seed(project, issue_ref, principle, n))
}

/// #1183 fix-round finding 10: `candidate_short_id` originally excluded
/// `engine_receipt` entirely, so a replay with the SAME ruling/source_refs
/// but an IMPROVED engine receipt (e.g. `preview_only` -> `known`, once the
/// effective provider/model/version become resolvable) deduped onto the
/// stale row and the better receipt was silently discarded — a later reader
/// would see `identity_status="preview_only"` forever even though a fuller
/// identity had since been captured. Folding the receipt's fields into the
/// identity seed means an engine-identity change (like a source-ref
/// revision change) appends a new candidate revision instead of vanishing
/// into the dedup gate, consistent with this module's "never silently drop"
/// boundary (module doc boundary 2) — `candidate_group_id` still excludes it
/// so revisions of the same case/principle/receipt-state remain linkable.
fn candidate_short_id(
    project: &str,
    issue_ref: Option<&str>,
    principle: &str,
    n: &NormalizedRuling,
    adjudicator: Option<&str>,
    source_refs: &[NormalizedSourceRef],
    engine_receipt: Option<&RulingEngineReceiptParams>,
) -> String {
    let mut seed = group_seed(project, issue_ref, principle, n);
    seed.push_str(&frame_field(adjudicator.unwrap_or("")));
    seed.push_str(&frame_field(&source_refs.len().to_string()));
    for r in source_refs {
        seed.push_str(&frame_field(&r.relation));
        seed.push_str(&frame_field(&r.target_kind));
        seed.push_str(&frame_field(&r.target_ref));
        seed.push_str(&frame_field(r.comment_id.as_deref().unwrap_or("")));
        seed.push_str(&frame_field(r.updated_at.as_deref().unwrap_or("")));
        seed.push_str(&frame_field(r.body_hash.as_deref().unwrap_or("")));
        seed.push_str(&frame_field(r.commit_sha.as_deref().unwrap_or("")));
        seed.push_str(&frame_field(r.section_or_span.as_deref().unwrap_or("")));
    }
    if let Some(receipt) = engine_receipt {
        seed.push_str(&frame_field(
            receipt.requested_role.as_deref().unwrap_or(""),
        ));
        seed.push_str(&frame_field(
            receipt.effective_provider.as_deref().unwrap_or(""),
        ));
        seed.push_str(&frame_field(
            receipt.effective_model.as_deref().unwrap_or(""),
        ));
        seed.push_str(&frame_field(
            receipt.effective_version.as_deref().unwrap_or(""),
        ));
        // Each fallback entry individually framed (not comma-joined) so two
        // different chains can never collide onto the same seed bytes —
        // same discipline as `frame_field`'s own doc comment.
        seed.push_str(&frame_field(&receipt.fallback_chain.len().to_string()));
        for f in &receipt.fallback_chain {
            seed.push_str(&frame_field(f));
        }
        seed.push_str(&frame_field(&receipt.degraded.to_string()));
    } else {
        seed.push_str(&frame_field(""));
    }
    hash16(&seed)
}

/// Build the human-readable body rendered into the candidate's memory text
/// field. Deliberately excludes provenance (`dispatch_id`/`flow_id`/
/// `pr_ref`) for the same reason `precedent_ops::render_body` does — see
/// that function's doc; identity is content, not which completion captured
/// it.
fn render_candidate_body(
    n: &NormalizedRuling,
    principle: &str,
    principle_index: usize,
    principle_count: usize,
    source_refs: &[NormalizedSourceRef],
    adjudicator: Option<&str>,
    coverage: &str,
) -> String {
    let mut lines = vec![format!(
        "Precedent candidate ({principle_index}/{principle_count}) — Principle: {principle}"
    )];
    lines.push(format!("Case: {}", n.case));
    if let Some(options) = &n.options_considered {
        lines.push(format!("Options considered: {options}"));
    }
    lines.push(format!("Ruling: {}", n.ruling));
    lines.push(format!("Outcome: {}", n.outcome));
    if let Some(overturned_by) = &n.overturned_by {
        lines.push(format!("Overturned by: {overturned_by}"));
    }
    if let Some(adjudicator) = adjudicator {
        lines.push(format!("Adjudicator: {adjudicator}"));
    }
    if source_refs.is_empty() {
        lines.push("Source refs: none".to_string());
    } else {
        lines.push(format!("Source refs ({}):", source_refs.len()));
        for r in source_refs {
            lines.push(format!(
                "  - {} {} {}",
                r.relation, r.target_kind, r.target_ref
            ));
        }
    }
    if coverage == "full" {
        lines.push("Coverage: full (no truncation, no dropped source refs)".to_string());
    } else {
        lines.push(
            "Coverage: partial (no truncation, but one or more supplied source refs were \
             dropped as malformed -- see metadata.source_ref_warnings)"
                .to_string(),
        );
    }
    lines.push("Candidate status: pending".to_string());
    lines.join("\n")
}

/// Build the structured metadata payload carried on the candidate row.
#[allow(clippy::too_many_arguments)]
fn build_candidate_metadata(
    n: &NormalizedRuling,
    principle: &str,
    principle_index: usize,
    principle_count: usize,
    source_refs: &[NormalizedSourceRef],
    source_ref_warnings: &[String],
    adjudicator: Option<&str>,
    authority_complete: bool,
    id_status: &str,
    coverage: &str,
    engine_receipt: Option<&RulingEngineReceiptParams>,
    group_id: &str,
    params: &TachiCompleteParams,
    redactions: usize,
) -> Value {
    let mut map = Map::new();
    map.insert("kind".into(), json!("precedent_candidate"));
    // Unconditional — see module doc boundary 1: this module never writes
    // anything other than "pending".
    map.insert("candidate_status".into(), json!("pending"));
    map.insert("case".into(), json!(n.case));
    if let Some(options) = &n.options_considered {
        map.insert("options_considered".into(), json!(options));
    }
    map.insert("ruling".into(), json!(n.ruling));
    map.insert("principle".into(), json!(principle));
    map.insert("principle_index".into(), json!(principle_index));
    map.insert("principle_count".into(), json!(principle_count));
    map.insert("outcome".into(), json!(n.outcome));
    if let Some(overturned_by) = &n.overturned_by {
        map.insert("overturned_by".into(), json!(overturned_by));
    }
    if let Some(adjudicator) = adjudicator {
        map.insert("adjudicator".into(), json!(adjudicator));
    }
    map.insert("authority_complete".into(), json!(authority_complete));
    map.insert("identity_status".into(), json!(id_status));
    if let Some(receipt) = engine_receipt {
        map.insert(
            "engine_receipt".into(),
            json!({
                "requested_role": receipt.requested_role,
                "effective_provider": receipt.effective_provider,
                "effective_model": receipt.effective_model,
                "effective_version": receipt.effective_version,
                "fallback_chain": receipt.fallback_chain,
                "degraded": receipt.degraded,
            }),
        );
    }
    map.insert("source_ref_count".into(), json!(source_refs.len()));
    if !source_ref_warnings.is_empty() {
        map.insert("source_ref_warnings".into(), json!(source_ref_warnings));
    }
    // No truncation, ever: every candidate carries the ruling's full
    // case/options/ruling text verbatim (see module doc boundary 2). But
    // "full" vs "partial" coverage is earned, not assumed — #1183 fix-round
    // finding 6: a supplied source_ref that failed per-kind immutable-
    // revision validation is evidence that was silently discarded from
    // `source_refs` before it ever reached this metadata; the caller
    // resolves that (via `source_ref_warnings.is_empty()`) into `coverage`
    // before calling in, so this always mirrors reality instead of a
    // hardcoded claim.
    map.insert("coverage".into(), json!(coverage));
    map.insert("candidate_group_id".into(), json!(group_id));
    if let Some(did) = params.dispatch_id.as_deref().filter(|s| !s.is_empty()) {
        map.insert("dispatch_id".into(), json!(did));
    }
    if let Some(fid) = params.flow_id.as_deref().filter(|s| !s.is_empty()) {
        map.insert("flow_id".into(), json!(fid));
    }
    if let Some(iref) = params.issue_ref.as_deref().filter(|s| !s.is_empty()) {
        map.insert("issue_ref".into(), json!(iref));
    }
    if let Some(pref) = params.pr_ref.as_deref().filter(|s| !s.is_empty()) {
        map.insert("pr_ref".into(), json!(pref));
    }
    if redactions > 0 {
        map.insert("secret_redactions".into(), json!(redactions));
        map.insert(
            "secret_redaction_warning".into(),
            json!("Potential secrets were redacted from this ruling before persistence."),
        );
    }
    Value::Object(map)
}

/// Decompose every caller-supplied ruling on a `complete` call into
/// principle-level `/precedent_candidates` rows. Returns a pipeline-status
/// value; failures are reported per-candidate and are never fatal to the
/// enclosing completion (module doc boundary 3).
pub(crate) async fn record_complete_precedent_candidates(
    server: &MemoryServer,
    params: &TachiCompleteParams,
    project_explicit: bool,
) -> Value {
    if params.rulings.is_empty() {
        return json!("skipped (no rulings)");
    }
    let project = project_segment(params);
    let scope = params
        .scope
        .clone()
        .unwrap_or_else(|| "project".to_string());
    let issue_ref = params.issue_ref.as_deref().filter(|s| !s.is_empty());

    let mut recorded: Vec<Value> = Vec::new();
    let mut skipped: Vec<Value> = Vec::new();

    for ruling in &params.rulings {
        let ruling_case_label = ruling.case.clone();
        let mut normalized: NormalizedRuling = match normalize_ruling(ruling) {
            Ok(n) => n,
            Err(reason) => {
                tracing::warn!(
                    reason = %reason,
                    "skipping malformed ruling for precedent-candidate decomposition"
                );
                skipped.push(json!({
                    "case": ruling_case_label,
                    "reason": reason,
                    "stage": "ruling",
                }));
                continue;
            }
        };
        let redactions = scrub_ruling(&mut normalized);
        let normalized = normalized;

        let principles = dedup_principles(&normalized.principles_cited);
        if principles.is_empty() {
            tracing::warn!(
                case = %normalized.case,
                "ruling has no principles_cited; nothing to decompose into principle-level candidates"
            );
            skipped.push(json!({
                "case": normalized.case,
                "reason": "no principles_cited; #1076 decomposition requires at least one \
                           principle to emit a principle-level candidate",
                "stage": "decomposition",
            }));
            continue;
        }

        let mut source_ref_warnings: Vec<String> = Vec::new();
        let mut source_refs: Vec<NormalizedSourceRef> = Vec::new();
        for raw_ref in &ruling.source_refs {
            match normalize_source_ref(raw_ref) {
                Ok(mut normalized_ref) => {
                    scrub_source_ref(&mut normalized_ref);
                    source_refs.push(normalized_ref);
                }
                Err(reason) => {
                    tracing::warn!(reason = %reason, "skipping malformed ruling source_ref");
                    source_ref_warnings.push(reason);
                }
            }
        }

        let (adjudicator, adjudicator_redactions) = match trimmed_opt(&ruling.adjudicator) {
            Some(raw) => {
                let (safe, count) = scrub_secrets(&raw);
                (Some(safe), count)
            }
            None => (None, 0),
        };
        let total_redactions = redactions + adjudicator_redactions;

        // #1076 RED case 4: missing adjudicator or missing source authority
        // means this ruling's candidates can never claim complete authority
        // — they still get recorded (pending is the whole point of this
        // module), but `authority_complete=false` follows them everywhere,
        // and `candidate_status` never becomes anything but "pending"
        // regardless (see module doc boundary 1).
        //
        // #1183 fix-round finding 5/6: `authority_complete` must also be
        // earned, not defaulted true, when evidence was DROPPED as
        // malformed (`source_ref_warnings` non-empty) — a bare issue_ref +
        // adjudicator with the ref's own snapshot hash missing must not
        // read as authority-complete just because SOME source_ref survived
        // validation. Every ref that reaches `source_refs` is now already
        // per-kind immutably pinned (`normalize_source_ref`), so the
        // remaining gap this closes is "evidence was supplied but rejected"
        // — that must degrade the flag, never silently vanish.
        let authority_complete =
            adjudicator.is_some() && !source_refs.is_empty() && source_ref_warnings.is_empty();
        let reference_appends = match source_refs
            .iter()
            .map(NormalizedSourceRef::validated_append)
            .collect::<Result<Vec<_>, _>>()
        {
            Ok(appends) => appends,
            Err(reason) => {
                tracing::warn!(reason = %reason, "skipping precedent candidate with invalid append shape");
                skipped.push(json!({
                    "case": normalized.case,
                    "reason": reason,
                    "stage": "reference_append",
                }));
                continue;
            }
        };
        // #1183 fix-round finding 6: "full" coverage is only true when
        // every supplied source_ref survived validation — a malformed ref
        // that got dropped is evidence that was silently discarded from the
        // candidate's authority, and the frozen "no drop-tail" contract
        // means that droppage must be reflected, not hidden behind an
        // unconditional "full" marker.
        let coverage = if source_ref_warnings.is_empty() {
            "full"
        } else {
            "partial"
        };
        let id_status = identity_status(ruling.engine_receipt.as_ref());
        let principle_count = principles.len();

        for (zero_idx, principle) in principles.iter().enumerate() {
            let principle_index = zero_idx + 1;
            let group_id = candidate_group_id(&project, issue_ref, principle, &normalized);
            let short_id = candidate_short_id(
                &project,
                issue_ref,
                principle,
                &normalized,
                adjudicator.as_deref(),
                &source_refs,
                ruling.engine_receipt.as_ref(),
            );
            let path = format!("/precedent_candidates/{project}/{short_id}");

            let text = render_candidate_body(
                &normalized,
                principle,
                principle_index,
                principle_count,
                &source_refs,
                adjudicator.as_deref(),
                coverage,
            );
            let summary = summary_line_with_prefix("[precedent-candidate]", &normalized.case, 80);

            let mut keywords = vec![
                "precedent_candidate".to_string(),
                "pending".to_string(),
                normalized.outcome.clone(),
                principle.clone(),
            ];
            if let Some(iref) = issue_ref {
                keywords.push(iref.to_string());
            }
            if let Some(did) = params.dispatch_id.as_deref().filter(|s| !s.is_empty()) {
                keywords.push(did.to_string());
            }
            if let Some(fid) = params.flow_id.as_deref().filter(|s| !s.is_empty()) {
                keywords.push(fid.to_string());
            }
            if let Some(pref) = params.pr_ref.as_deref().filter(|s| !s.is_empty()) {
                keywords.push(pref.to_string());
            }

            let metadata = build_candidate_metadata(
                &normalized,
                principle,
                principle_index,
                principle_count,
                &source_refs,
                &source_ref_warnings,
                adjudicator.as_deref(),
                authority_complete,
                id_status,
                coverage,
                ruling.engine_receipt.as_ref(),
                &group_id,
                params,
                total_redactions,
            );

            let mem_params = SaveMemoryParams {
                text,
                summary,
                path: path.clone(),
                importance: match normalized.outcome.as_str() {
                    "validated" => 0.75,
                    "overturned" => 0.65,
                    _ => 0.6,
                },
                category: "decision".to_string(),
                topic: normalized.case.clone(),
                keywords,
                persons: Vec::new(),
                entities: vec![principle.clone()],
                location: String::new(),
                scope: scope.clone(),
                vector: None,
                id: None,
                force: false,
                auto_link: true,
                project: params.project.clone(),
                project_explicit,
                retention_policy: None,
                domain: Some("precedent_candidate".to_string()),
                timestamp: None,
                valid_from: None,
                valid_until: None,
                metadata: Some(metadata),
                emit_continuity: false,
            };

            match save_eval_memory_with_authorized_reference_mutations(
                server,
                mem_params,
                reference_appends.clone(),
            )
            .await
            {
                Ok(raw) => {
                    let saved: Value =
                        serde_json::from_str(&raw).unwrap_or_else(|_| json!({ "raw": raw }));
                    match extract_persisted_id(&saved) {
                        Ok(id) => {
                            recorded.push(json!({
                                "case": normalized.case,
                                "principle": principle,
                                "principle_index": principle_index,
                                "principle_count": principle_count,
                                "outcome": normalized.outcome,
                                "candidate_status": "pending",
                                "path": path,
                                "id": id,
                                "candidate_group_id": group_id,
                                "authority_complete": authority_complete,
                                "identity_status": id_status,
                            }));
                        }
                        Err(reason) => {
                            tracing::warn!(
                                reason = %reason,
                                "precedent candidate rejected by capture gate/noise filter"
                            );
                            skipped.push(json!({
                                "case": normalized.case,
                                "principle": principle,
                                "reason": reason,
                                "stage": "persist",
                            }));
                        }
                    }
                }
                Err(err) => {
                    tracing::warn!(error = %err, "failed to persist precedent candidate");
                    skipped.push(json!({
                        "case": normalized.case,
                        "principle": principle,
                        "reason": err,
                        "stage": "persist",
                    }));
                }
            }
        }

        if !source_ref_warnings.is_empty() {
            skipped.push(json!({
                "case": normalized.case,
                "reason": format!(
                    "{} source_ref(s) skipped: {}",
                    source_ref_warnings.len(),
                    source_ref_warnings.join("; ")
                ),
                "stage": "source_ref",
            }));
        }
    }

    json!({ "recorded": recorded, "skipped": skipped })
}

#[cfg(test)]
mod unit_tests {
    use super::*;
    use crate::tool_params::RulingRecordParams;

    fn base_ruling() -> RulingRecordParams {
        RulingRecordParams {
            case: "shared case".to_string(),
            options_considered: None,
            ruling: "shared ruling".to_string(),
            principles_cited: vec!["a".to_string(), "a".to_string(), "b".to_string()],
            outcome: None,
            overturned_by: None,
            adjudicator: None,
            source_refs: Vec::new(),
            engine_receipt: None,
        }
    }

    #[test]
    fn dedup_principles_preserves_order_and_drops_exact_duplicates() {
        let n = base_ruling();
        let deduped = dedup_principles(&n.principles_cited);
        assert_eq!(deduped, vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn identity_status_unknown_without_receipt() {
        assert_eq!(identity_status(None), "preview_only");
    }

    #[test]
    fn identity_status_known_requires_full_non_degraded_identity() {
        let receipt = RulingEngineReceiptParams {
            requested_role: Some("leader".to_string()),
            effective_provider: Some("anthropic".to_string()),
            effective_model: Some("claude-sonnet-5".to_string()),
            effective_version: Some("2026-07".to_string()),
            fallback_chain: Vec::new(),
            degraded: false,
        };
        assert_eq!(identity_status(Some(&receipt)), "known");

        let mut degraded = receipt.clone();
        degraded.degraded = true;
        assert_eq!(identity_status(Some(&degraded)), "preview_only");

        let mut fallback = receipt.clone();
        fallback.fallback_chain = vec!["gpt-fallback".to_string()];
        assert_eq!(identity_status(Some(&fallback)), "preview_only");

        let mut unknown_model = receipt.clone();
        unknown_model.effective_model = None;
        assert_eq!(identity_status(Some(&unknown_model)), "preview_only");

        // #1183 fix-round finding 7: a receipt with provider+model present
        // but `effective_version=None` must NOT read as "known" -- the
        // frozen contract names "provider/model/version" together.
        let mut unknown_version = receipt;
        unknown_version.effective_version = None;
        assert_eq!(
            identity_status(Some(&unknown_version)),
            "preview_only",
            "missing effective_version must not be mislabeled as a known identity"
        );
    }

    #[test]
    fn candidate_short_id_changes_when_engine_receipt_changes() {
        // #1183 fix-round finding 10: an identical ruling/source_refs replay
        // with an IMPROVED engine receipt (preview_only -> known) must not
        // dedupe onto the stale row -- the better receipt would otherwise be
        // silently discarded.
        let n = normalize_ruling(&base_ruling()).expect("valid ruling normalizes");
        let receipt_known = RulingEngineReceiptParams {
            requested_role: Some("leader".to_string()),
            effective_provider: Some("anthropic".to_string()),
            effective_model: Some("claude-sonnet-5".to_string()),
            effective_version: Some("2026-07".to_string()),
            fallback_chain: Vec::new(),
            degraded: false,
        };

        let id_no_receipt = candidate_short_id("global", None, "a", &n, None, &[], None);
        let id_with_receipt =
            candidate_short_id("global", None, "a", &n, None, &[], Some(&receipt_known));
        assert_ne!(
            id_no_receipt, id_with_receipt,
            "a later capture of the same ruling with a newly-resolved engine receipt must not \
             collapse onto the receipt-less row"
        );

        let mut receipt_fallback = receipt_known.clone();
        receipt_fallback.fallback_chain = vec!["gpt-fallback".to_string()];
        let id_fallback =
            candidate_short_id("global", None, "a", &n, None, &[], Some(&receipt_fallback));
        assert_ne!(
            id_with_receipt, id_fallback,
            "a changed fallback_chain must derive a different candidate id"
        );

        let group_a = candidate_group_id("global", None, "a", &n);
        let group_b = candidate_group_id("global", None, "a", &n);
        assert_eq!(
            group_a, group_b,
            "candidate_group_id is independent of the engine receipt"
        );
    }

    #[test]
    fn candidate_short_id_changes_when_source_ref_revision_changes() {
        let n = normalize_ruling(&base_ruling()).expect("valid ruling normalizes");
        let refs_v1 = vec![NormalizedSourceRef {
            relation: "supports".to_string(),
            target_kind: "comment".to_string(),
            target_ref: "kckylechen1/tachi#530#issuecomment-1".to_string(),
            comment_id: Some("1".to_string()),
            updated_at: Some("2026-07-01T00:00:00Z".to_string()),
            body_hash: Some("hash-v1".to_string()),
            commit_sha: None,
            section_or_span: None,
        }];
        let mut refs_v2 = Vec::new();
        for r in &refs_v1 {
            refs_v2.push(NormalizedSourceRef {
                relation: r.relation.clone(),
                target_kind: r.target_kind.clone(),
                target_ref: r.target_ref.clone(),
                comment_id: r.comment_id.clone(),
                updated_at: Some("2026-07-02T00:00:00Z".to_string()),
                body_hash: Some("hash-v2".to_string()),
                commit_sha: r.commit_sha.clone(),
                section_or_span: r.section_or_span.clone(),
            });
        }

        let id_v1 = candidate_short_id("global", None, "a", &n, None, &refs_v1, None);
        let id_v2 = candidate_short_id("global", None, "a", &n, None, &refs_v2, None);
        assert_ne!(
            id_v1, id_v2,
            "an edited comment revision must not derive the same candidate id"
        );

        let group_v1 = candidate_group_id("global", None, "a", &n);
        let group_v2 = candidate_group_id("global", None, "a", &n);
        assert_eq!(
            group_v1, group_v2,
            "candidate_group_id is independent of source-ref revisions"
        );
    }
}
