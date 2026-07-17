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
//!    dropped or summarized away by the fan-out. Every candidate also
//!    carries the ruling's *complete* set of source refs (no partitioning,
//!    no drop-tail), plus an explicit `metadata.coverage = "full"` marker.
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
//!   backs it.
//! - `candidate_short_id` (the row's path segment) additionally folds in the
//!   adjudicator and every normalized source ref's full identity tuple
//!   (relation/kind/ref/comment_id/updated_at/body_hash/commit_sha/span).
//!
//! An exact replay (byte-identical ruling, same source refs) re-derives the
//! same `candidate_short_id` and the same rendered `text`, so
//! `save_memory`'s exact-path+exact-text dedup gate collapses it onto the
//! existing row — idempotent, per #1076 RED case 3. An edited owner comment
//! (same `comment_id`, different `updated_at`/`body_hash`) changes the
//! `candidate_short_id` seed, so it is NOT treated as the same replay: a new
//! row is appended at a new path, sharing the old row's `candidate_group_id`
//! (so a later reader can tell the two rows are revisions of the same
//! case/principle claim) while the old row is left untouched — this module
//! never edits or deletes a previously-written candidate.

use serde_json::{json, Map, Value};

use crate::memory_search_ops::{save_eval_memory, scrub_secrets};
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
    Ok(NormalizedSourceRef {
        relation,
        target_kind,
        target_ref,
        comment_id,
        updated_at: trimmed_opt(&raw.updated_at),
        body_hash: trimmed_opt(&raw.body_hash),
        commit_sha: trimmed_opt(&raw.commit_sha),
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
/// (absent receipt, missing provider/model, a non-empty fallback chain, or
/// an explicitly degraded run) is `"preview_only"`.
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
    if !has_provider || !has_model || !receipt.fallback_chain.is_empty() || receipt.degraded {
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

fn candidate_short_id(
    project: &str,
    issue_ref: Option<&str>,
    principle: &str,
    n: &NormalizedRuling,
    adjudicator: Option<&str>,
    source_refs: &[NormalizedSourceRef],
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
    lines.push("Coverage: full (no truncation, no dropped source refs)".to_string());
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
    map.insert(
        "source_refs".into(),
        json!(source_refs
            .iter()
            .map(|r| json!({
                "relation": r.relation,
                "target_kind": r.target_kind,
                "target_ref": r.target_ref,
                "comment_id": r.comment_id,
                "updated_at": r.updated_at,
                "body_hash": r.body_hash,
                "commit_sha": r.commit_sha,
                "section_or_span": r.section_or_span,
            }))
            .collect::<Vec<_>>()),
    );
    map.insert("source_ref_count".into(), json!(source_refs.len()));
    if !source_ref_warnings.is_empty() {
        map.insert("source_ref_warnings".into(), json!(source_ref_warnings));
    }
    // No truncation/drop-tail path: every candidate carries the ruling's
    // full case/options/ruling text and its complete source_refs set (see
    // module doc boundary 2), so this is always "full" — never partial.
    map.insert("coverage".into(), json!("full"));
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
        let authority_complete = adjudicator.is_some() && !source_refs.is_empty();
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
            );
            let path = format!("/precedent_candidates/{project}/{short_id}");

            let text = render_candidate_body(
                &normalized,
                principle,
                principle_index,
                principle_count,
                &source_refs,
                adjudicator.as_deref(),
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

            match save_eval_memory(server, mem_params).await {
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

        let mut unknown_model = receipt;
        unknown_model.effective_model = None;
        assert_eq!(identity_status(Some(&unknown_model)), "preview_only");
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

        let id_v1 = candidate_short_id("global", None, "a", &n, None, &refs_v1);
        let id_v2 = candidate_short_id("global", None, "a", &n, None, &refs_v2);
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
