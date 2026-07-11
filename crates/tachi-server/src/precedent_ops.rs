//! Precedent capture (#950 slice 1: capture only).
//!
//! Leader adjudications (BLOCK/FIX-FIRST verdicts, scope rulings, principle
//! rulings) evaporate into transcripts today. This slice persists
//! *already-structured* rulings supplied on a `tachi_complete` / `tachi_task
//! action=complete` call as `/precedents/<project>/<date>-<shortid>` memory
//! rows, so a later slice can decompose, recall, and harden them. Two hard
//! boundaries per the issue's frozen design:
//!
//! 1. **No decomposition here.** The verdict→principle-record decomposition is a
//!    later backend-model lane. This module stores what the caller hands over,
//!    faithfully, unmodified.
//! 2. **Fail-safe.** A malformed ruling is skipped with a warning and never
//!    fails the enclosing `complete` call — completion recording is the primary
//!    contract; precedent capture is best-effort alongside it.
//!
//! Rows go through the standard `save_eval_memory` pipeline, and the pipeline
//! itself only scrubs `text`/`summary` for secrets — `metadata` is opaque to
//! it. So every caller-supplied ruling string (`case`, `ruling`,
//! `options_considered`, `principles_cited`, `overturned_by`) is scrubbed
//! *before* it goes into the body, the summary, and the metadata payload
//! (mirroring the `complete_ops::scrub` convention `tachi_complete` itself
//! uses for the eval record). Provenance (`dispatch_id` / `flow_id` /
//! `issue_ref` / `pr_ref`) carried on the completion is linked into both.
//!
//! The capture gate (`memory-server-capture-gate`) can hard-reject a save
//! under `TACHI_CAPTURE_GATE=enforce` (path bucket, domain, min-chars,
//! markdown-dump heuristics); a rejected save still returns `Ok(..)` from
//! `save_eval_memory` with `saved:false` and no `id`. That response is
//! detected and routed to `skipped`, never counted as `recorded` — a rejected
//! precedent must never be silently reported as captured.
//!
//! Category decision (documented per issue #950): rows are stored as the
//! existing `decision` category with `metadata.kind = "precedent"` as the
//! discriminator, NOT a new `precedent` category. A dedicated category would be
//! least-invasive on paper, but the canonical `MemoryCategory` allowlist is
//! enforced by a SQLite `CHECK (category IN (...))` constraint baked into the
//! table DDL (`memcore db/schema.rs`) and into the shipped template DB the test
//! harness copies. Adding a value there needs a table-recreating migration plus
//! a template-DB regen — the opposite of least-invasive. The `/precedents/...`
//! path prefix plus `metadata.kind` give the later recall slice a clean
//! namespace + discriminator without touching the schema.

use serde_json::{json, Map, Value};

use crate::memory_search_ops::save_eval_memory;
use crate::tool_params::{RulingRecordParams, SaveMemoryParams, TachiCompleteParams};
use crate::MemoryServer;

/// The three recognized truth-maintenance states for a ruling. An omitted
/// outcome defaults to `pending`; an explicit value outside this set makes the
/// ruling malformed (skipped + warned).
const VALID_OUTCOMES: [&str; 3] = ["validated", "overturned", "pending"];

/// Reject values that can't safely form a path segment. We only need the
/// project label to be a stable, filesystem/URL-safe token; anything else is
/// normalized to `global`.
fn project_segment(params: &TachiCompleteParams) -> String {
    let raw = params
        .project
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("global");
    let slug: String = raw
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    let trimmed = slug.trim_matches('-').to_string();
    if trimmed.is_empty() {
        "global".to_string()
    } else {
        trimmed
    }
}

/// Validate + normalize one caller-supplied ruling. `Err(reason)` marks it
/// malformed so the caller can skip + warn without aborting completion.
fn normalize_ruling(ruling: &RulingRecordParams) -> Result<NormalizedRuling, String> {
    let case = ruling.case.trim();
    if case.is_empty() {
        return Err("ruling missing required `case`".to_string());
    }
    let verdict = ruling.ruling.trim();
    if verdict.is_empty() {
        return Err("ruling missing required `ruling`".to_string());
    }
    let outcome = match ruling.outcome.as_deref().map(str::trim) {
        None | Some("") => "pending".to_string(),
        Some(value) => {
            let lowered = value.to_ascii_lowercase();
            if !VALID_OUTCOMES.contains(&lowered.as_str()) {
                return Err(format!(
                    "ruling has invalid `outcome` {value:?} (expected one of {VALID_OUTCOMES:?})"
                ));
            }
            lowered
        }
    };
    let options_considered = ruling
        .options_considered
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let principles_cited: Vec<String> = ruling
        .principles_cited
        .iter()
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty())
        .collect();
    let overturned_by = ruling
        .overturned_by
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    Ok(NormalizedRuling {
        case: case.to_string(),
        options_considered,
        ruling: verdict.to_string(),
        principles_cited,
        outcome,
        overturned_by,
    })
}

struct NormalizedRuling {
    case: String,
    options_considered: Option<String>,
    ruling: String,
    principles_cited: Vec<String>,
    outcome: String,
    overturned_by: Option<String>,
}

/// Scrub every free-text field of a normalized ruling for secrets, in place.
/// `save_eval_memory`'s pipeline only scrubs `text`/`summary` on the way in;
/// `metadata` is opaque to it, and the ruling's own body/summary are built
/// from these fields *before* that pipeline runs. Scrubbing here up front
/// (rather than relying on the pipeline) guarantees the metadata copy is
/// redacted identically to the body/summary. Returns the total redaction
/// count across all fields.
fn scrub_ruling(n: &mut NormalizedRuling) -> usize {
    let mut redactions = 0usize;
    let (case, c) = crate::memory_search_ops::scrub_secrets(&n.case);
    n.case = case;
    redactions += c;
    let (ruling, c) = crate::memory_search_ops::scrub_secrets(&n.ruling);
    n.ruling = ruling;
    redactions += c;
    if let Some(options) = &n.options_considered {
        let (safe, c) = crate::memory_search_ops::scrub_secrets(options);
        n.options_considered = Some(safe);
        redactions += c;
    }
    for principle in &mut n.principles_cited {
        let (safe, c) = crate::memory_search_ops::scrub_secrets(principle);
        *principle = safe;
        redactions += c;
    }
    if let Some(overturned_by) = &n.overturned_by {
        let (safe, c) = crate::memory_search_ops::scrub_secrets(overturned_by);
        n.overturned_by = Some(safe);
        redactions += c;
    }
    redactions
}

/// Truncate a single line to at most `max` chars for the ≤100-char summary
/// field, appending an ellipsis when cut.
fn summary_line(case: &str, max: usize) -> String {
    let one_line = case.replace(['\n', '\r'], " ");
    if one_line.chars().count() <= max {
        format!("[precedent] {one_line}")
    } else {
        let head: String = one_line.chars().take(max.saturating_sub(1)).collect();
        format!("[precedent] {head}…")
    }
}

/// Build the human-readable body rendered into the memory text field.
fn render_body(n: &NormalizedRuling, params: &TachiCompleteParams) -> String {
    let mut lines = vec![format!("Precedent: {}", n.case)];
    if let Some(options) = &n.options_considered {
        lines.push(format!("Options considered: {options}"));
    }
    lines.push(format!("Ruling: {}", n.ruling));
    if !n.principles_cited.is_empty() {
        lines.push(format!("Principles cited: {}", n.principles_cited.join(", ")));
    }
    lines.push(format!("Outcome: {}", n.outcome));
    if let Some(overturned_by) = &n.overturned_by {
        lines.push(format!("Overturned by: {overturned_by}"));
    }
    let mut provenance: Vec<String> = Vec::new();
    if let Some(did) = params.dispatch_id.as_deref().filter(|s| !s.is_empty()) {
        provenance.push(format!("dispatch {did}"));
    }
    if let Some(fid) = params.flow_id.as_deref().filter(|s| !s.is_empty()) {
        provenance.push(format!("flow {fid}"));
    }
    if let Some(iref) = params.issue_ref.as_deref().filter(|s| !s.is_empty()) {
        provenance.push(format!("issue {iref}"));
    }
    if let Some(pref) = params.pr_ref.as_deref().filter(|s| !s.is_empty()) {
        provenance.push(format!("pr {pref}"));
    }
    if !provenance.is_empty() {
        lines.push(format!("Provenance: {}", provenance.join(" / ")));
    }
    lines.join("\n")
}

/// Build the structured metadata payload carried on the precedent row.
/// `n` must already be scrubbed (see `scrub_ruling`); `redactions` is
/// surfaced so a scrubbed row is visibly marked, matching the eval-record
/// convention (`complete_ops::eval_record`).
fn build_metadata(n: &NormalizedRuling, params: &TachiCompleteParams, redactions: usize) -> Value {
    let mut map = Map::new();
    map.insert("kind".into(), json!("precedent"));
    map.insert("case".into(), json!(n.case));
    if let Some(options) = &n.options_considered {
        map.insert("options_considered".into(), json!(options));
    }
    map.insert("ruling".into(), json!(n.ruling));
    if !n.principles_cited.is_empty() {
        map.insert("principles_cited".into(), json!(n.principles_cited));
    }
    map.insert("outcome".into(), json!(n.outcome));
    if let Some(overturned_by) = &n.overturned_by {
        map.insert("overturned_by".into(), json!(overturned_by));
    }
    // Provenance links back to the completion that carried this ruling.
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

/// Detect whether a `save_eval_memory` response actually persisted a row.
/// The capture gate (and the noise filter) can hard-reject a save under
/// `TACHI_CAPTURE_GATE=enforce`; the rejection response is still `Ok(..)` at
/// the `save_eval_memory` layer (it's a structured "not saved" JSON body, not
/// an `Err`), so the only reliable signal is a present, non-empty `id`. A
/// legitimate exact-duplicate response also carries a real `id` (the existing
/// row's), so this correctly counts that as recorded rather than skipped.
fn extract_persisted_id(saved: &Value) -> Result<String, String> {
    match saved.get("id").and_then(Value::as_str).filter(|s| !s.is_empty()) {
        Some(id) => Ok(id.to_string()),
        None => {
            if let Some(reason) = saved.get("reason").and_then(Value::as_str) {
                Err(reason.to_string())
            } else if let Some(rejected_by) = saved.get("rejected_by").and_then(Value::as_str) {
                let violations = saved
                    .get("violations")
                    .map(|v| v.to_string())
                    .unwrap_or_default();
                Err(format!("rejected_by={rejected_by} violations={violations}"))
            } else {
                Err("save response carried no id (rejected by capture gate or noise filter)"
                    .to_string())
            }
        }
    }
}

/// Persist every caller-supplied ruling on a `complete` call as a `/precedents`
/// memory row. Returns a pipeline-status value; recording failures are reported
/// per-ruling and are never fatal to the enclosing completion.
pub(crate) async fn record_complete_rulings(
    server: &MemoryServer,
    params: &TachiCompleteParams,
    date: &str,
) -> Value {
    if params.rulings.is_empty() {
        return json!("skipped (no rulings)");
    }
    let project = project_segment(params);
    let scope = params
        .scope
        .clone()
        .unwrap_or_else(|| "project".to_string());
    let mut recorded: Vec<Value> = Vec::new();
    let mut skipped: Vec<Value> = Vec::new();

    for ruling in &params.rulings {
        let mut normalized = match normalize_ruling(ruling) {
            Ok(n) => n,
            Err(reason) => {
                tracing::warn!(reason = %reason, "skipping malformed precedent ruling");
                skipped.push(json!({ "case": ruling.case, "reason": reason }));
                continue;
            }
        };
        let redactions = scrub_ruling(&mut normalized);
        let normalized = normalized;

        let short_id = uuid::Uuid::new_v4().simple().to_string()[..8].to_string();
        let path = format!("/precedents/{project}/{date}-{short_id}");

        let mut keywords = vec!["precedent".to_string(), normalized.outcome.clone()];
        keywords.extend(normalized.principles_cited.iter().cloned());
        if let Some(iref) = params.issue_ref.as_deref().filter(|s| !s.is_empty()) {
            keywords.push(iref.to_string());
        }

        let mem_params = SaveMemoryParams {
            text: render_body(&normalized, params),
            summary: summary_line(&normalized.case, 80),
            path: path.clone(),
            importance: match normalized.outcome.as_str() {
                "validated" => 0.8,
                "overturned" => 0.7,
                _ => 0.65,
            },
            // Stored under the existing `decision` category (see module docs);
            // `metadata.kind = "precedent"` is the discriminator.
            category: "decision".to_string(),
            topic: normalized.case.clone(),
            keywords,
            persons: Vec::new(),
            entities: normalized.principles_cited.clone(),
            location: String::new(),
            scope: scope.clone(),
            vector: None,
            id: None,
            force: false,
            auto_link: true,
            project: params.project.clone(),
            retention_policy: None,
            domain: Some("precedent".to_string()),
            timestamp: None,
            valid_from: None,
            valid_until: None,
            metadata: Some(build_metadata(&normalized, params, redactions)),
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
                            "outcome": normalized.outcome,
                            "path": path,
                            "id": id,
                        }));
                    }
                    Err(reason) => {
                        tracing::warn!(
                            reason = %reason,
                            "precedent ruling rejected by capture gate/noise filter"
                        );
                        skipped.push(json!({ "case": normalized.case, "reason": reason }));
                    }
                }
            }
            Err(err) => {
                tracing::warn!(error = %err, "failed to persist precedent ruling");
                skipped.push(json!({ "case": normalized.case, "reason": err }));
            }
        }
    }

    json!({ "recorded": recorded, "skipped": skipped })
}
