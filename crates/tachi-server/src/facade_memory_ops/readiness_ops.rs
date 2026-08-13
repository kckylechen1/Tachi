//! Alert, ask, and readiness handlers for `tachi_memory`.
//! Consolidate lifecycle lives in `consolidate_ops` (#775).

use super::current_work_anchor::{
    compute_anchor_gated_confidence, extract_exact_issue_anchors, overall_grounding_status,
    prepend_anchor_evidence_rows, resolve_current_work_anchors,
};
use super::evidence_format::{
    build_thinking_scaffold, evidence_rows, format_agent_status, json_string, sections_to_evidence,
    synthesis_markdown_text, wants_json,
};
use crate::agent_markdown;
use crate::facade_search_ops::collect_tachi_search_sections;
use crate::tool_params::*;
use crate::MemoryServer;
use serde_json::{json, Value};

/// #1071: synthesis is called with a bounded timeout so a stalled provider
/// degrades the response to `partial`, never hangs `ask`.
const ASK_SYNTHESIS_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

// ---------------------------------------------------------------------------
// Alerts
// ---------------------------------------------------------------------------

pub(crate) async fn handle_memory_alerts(
    server: &MemoryServer,
    params: &TachiMemoryParams,
) -> Result<String, String> {
    let warnings = crate::status_ops::collect_agent_warning_lines(server).await;
    let wiki_counts = crate::wiki_ops::wiki_hygiene_counts(server).await?;
    if wants_json(params.format.as_deref()) {
        return json_string(&json!({
            "status": "completed",
            "warnings": warnings,
            "wiki_counts": wiki_counts,
        }));
    }
    Ok(agent_markdown::format_alerts(&warnings, &wiki_counts))
}

// ---------------------------------------------------------------------------
// Ask
// ---------------------------------------------------------------------------

pub(crate) async fn handle_memory_ask(
    server: &MemoryServer,
    params: &TachiMemoryParams,
) -> Result<String, String> {
    let query = params
        .query
        .clone()
        .or_else(|| params.text.clone())
        .ok_or_else(|| "query or text is required when action='ask'".to_string())?;
    // #946: runtime DB paths are authoritative. Evidence may contain stale
    // Desktop/Sigil paths from old memories; never let synthesis invent them.
    let runtime_binding = runtime_db_binding(server);

    // #1071 fix-round checkpoint 1: exact-anchor extraction/resolution must
    // happen before EVERY confidence-return path, including the runtime-DB
    // fast path below — a query can simultaneously name an exact anchor
    // ("what is the current db path for owner/repo#1234?") and ask about
    // the runtime binding; the anchor-gating rule still applies to that
    // query's confidence even though the runtime-path answer itself is
    // authoritative and unaffected. See `current_work_anchor` module doc
    // for the frozen-contract basis and documented scope gaps (issue
    // anchors only, no bare `#N`).
    let exact_anchor_targets = extract_exact_issue_anchors(&query);
    let required_anchors = if exact_anchor_targets.is_empty() {
        Vec::new()
    } else {
        resolve_current_work_anchors(server, &exact_anchor_targets).await
    };
    let grounding_status = overall_grounding_status(&required_anchors);

    if is_runtime_db_path_query(&query) {
        return answer_runtime_db_path_query(
            &query,
            &runtime_binding,
            params.format.as_deref(),
            &required_anchors,
            grounding_status,
        );
    }

    let search_params = TachiSearchParams {
        query: query.clone(),
        scope: params.scope.clone().unwrap_or_else(|| "all".to_string()),
        top_k: params.top_k.max(3).min(10),
        path_prefix: params.path_prefix.clone(),
        project: params.project.clone(),
        domain: params.domain.clone(),
        file_context: params.file_context.clone(),
        error_context: params.error_context.clone(),
        context_symbols: Vec::new(),
        agent_role: params.agent_role.clone(),
        category: params.category.clone(),
        include_archived: params.include_archived,
        include_training: params.include_training,
        enable_rerank: params.enable_rerank,
        as_of: params.as_of.clone(),
    };
    let (sections, _, _) = collect_tachi_search_sections(server, &search_params).await;
    let evidence = sections_to_evidence(&sections)?;
    // Tag each evidence row with the store it came from, and surface a warning
    // when the user didn't pin `project` and evidence spans both global and
    // project stores. Without this, ask synthesis can blend unrelated context
    // (e.g. "what is the current health status?" returns another project's API).
    let has_project_db = evidence_rows(&evidence)
        .iter()
        .any(|row| row.get("db").and_then(Value::as_str) == Some("project"));
    let uses_global = evidence_rows(&evidence)
        .iter()
        .any(|row| row.get("db").and_then(Value::as_str) == Some("global"));
    let cross_store = params.project.is_none() && has_project_db && uses_global;
    let evidence = inject_project_tags(evidence);

    // #1071: `required_anchors`/`grounding_status` were already resolved
    // above (before the runtime-DB fast path) — see checkpoint 1's fix
    // comment there. Reused here, not re-extracted, so a live-resolved (or
    // live-failed) anchor overrides the generic evidence-volume confidence
    // heuristic — never the reverse.
    let evidence = prepend_anchor_evidence_rows(evidence, &required_anchors);

    let mut thinking = build_thinking_scaffold("ask", &query, &evidence);
    // #1209: `anchor_confidence` is `None` exactly when no exact anchor was
    // requested (mirrors the `required_anchors.is_empty()` gate on the
    // `confidence`/`grounding_status` metadata fields below and in the JSON
    // branch) — a plain semantic query gets no cap line in the synthesis
    // prompt either, matching the metadata it would otherwise contradict.
    let anchor_confidence = if required_anchors.is_empty() {
        None
    } else {
        Some(compute_anchor_gated_confidence(&required_anchors))
    };
    if let Some(confidence) = anchor_confidence {
        if let Some(obj) = thinking.as_object_mut() {
            obj.insert("confidence".to_string(), json!(confidence));
            if confidence == "low" {
                if let Some(gaps) = obj.get_mut("gaps").and_then(Value::as_array_mut) {
                    for anchor in required_anchors.iter().filter(|anchor| {
                        anchor.grounding_status == GroundingStatusV1::MissingAnchor
                    }) {
                        for reason in &anchor.contradictions {
                            gaps.push(json!(format!(
                                "current-work anchor {} unresolved: {}",
                                anchor.source_ref, reason.description
                            )));
                        }
                    }
                }
            }
        }
    }

    // #1209: previously `compute_anchor_gated_confidence`'s result only ever
    // reached `thinking`/`required_anchors` metadata — `synthesize_answer`'s
    // prompt had zero reference to it, so a `missing_anchor`/`low`-capped
    // query could still get back confidently-worded prose that contradicts
    // its own metadata. Thread the same cap into the prompt here.
    let anchor_cap = anchor_confidence.map(|confidence| AnchorConfidenceCap {
        grounding_status,
        confidence,
    });

    let synthesis = if params.synthesize {
        Some(
            synthesize_answer(
                server,
                &query,
                &evidence,
                &runtime_binding,
                params.model.as_deref(),
                anchor_cap,
            )
            .await,
        )
    } else {
        None
    };
    // #1071: "Provider timeout/fallback preserves evidence but returns
    // overall partial/preview-only." Evidence confidence (`thinking`) is
    // computed above, before synthesis runs, and is never touched here —
    // only the top-level response status reflects a degraded synthesis.
    // #1071 fix-round checkpoint 6: `synthesize_answer` now reports its own
    // `status: "partial"` when the provider truncated its response
    // (`finish_reason == "length"`), so that case folds into this same
    // "partial" rule rather than remaining `completed/high/gaps=[]`.
    let overall_status = match synthesis
        .as_ref()
        .and_then(|s| s.get("status"))
        .and_then(Value::as_str)
    {
        Some("timeout") | Some("failed") | Some("partial") => "partial",
        _ => "completed",
    };
    if wants_json(params.format.as_deref()) {
        return json_string(&json!({
            "status": overall_status,
            "query": query,
            "evidence": evidence,
            "thinking": thinking,
            "synthesis": synthesis,
            "runtime": runtime_binding,
            "cross_store": cross_store,
            "cross_store_hint": if cross_store {
                Some("evidence spans global and project stores; pass `project=...` to pin a library or restrict `scope` to one store".to_string())
            } else {
                None
            },
            "grounding_status": grounding_status.as_str(),
            "required_anchors": required_anchors,
        }));
    }
    let synthesis_text = synthesis.as_ref().and_then(synthesis_markdown_text);
    let mut fields: Vec<(&str, String)> = vec![
        ("status", overall_status.to_string()),
        ("query", query),
        (
            "evidence",
            format!("{} hit(s)", evidence_rows(&evidence).len()),
        ),
        (
            "confidence",
            thinking
                .get("confidence")
                .and_then(Value::as_str)
                .unwrap_or("none")
                .to_string(),
        ),
        (
            "project_db",
            runtime_binding
                .get("project_db")
                .and_then(Value::as_str)
                .unwrap_or("(none)")
                .to_string(),
        ),
        (
            "global_db",
            runtime_binding
                .get("global_db")
                .and_then(Value::as_str)
                .unwrap_or("?")
                .to_string(),
        ),
    ];
    if !required_anchors.is_empty() {
        fields.push(("grounding_status", grounding_status.as_str().to_string()));
    }
    if cross_store {
        fields.push((
            "cross_store",
            "evidence spans global and project stores; pin `project=...` to scope to one library"
                .to_string(),
        ));
    }
    Ok(format_agent_status(
        "Tachi ask",
        &fields,
        Some(&evidence),
        synthesis_text.as_deref(),
    ))
}

/// Authoritative live DB paths from the running server (not memory evidence).
fn runtime_db_binding(server: &MemoryServer) -> Value {
    json!({
        "global_db": server.global_db_path_buf().display().to_string(),
        "project_db": server.project_db_path_buf().map(|p| p.display().to_string()),
        "single_db_mode": !server.has_project_db(),
        "source": "runtime_binding",
    })
}

/// True when the question is about which memory DB path is currently active.
fn is_runtime_db_path_query(query: &str) -> bool {
    let q = query.to_ascii_lowercase();
    let asks_path = q.contains("memory.db")
        || q.contains("db path")
        || q.contains("database path")
        || q.contains("which database")
        || q.contains("which db")
        || q.contains("current db")
        || q.contains("active db")
        || (q.contains("project db") && (q.contains("path") || q.contains("where")))
        || (q.contains("global db") && (q.contains("path") || q.contains("where")));
    if !asks_path {
        return false;
    }
    q.contains("where is")
        || q.contains("what is")
        || q.contains("which")
        || q.contains("current")
        || q.contains("active")
        || q.contains("runtime")
        || q.contains("using")
}

/// #1071 fix-round checkpoint 1: `required_anchors`/`grounding_status` are
/// resolved by the caller BEFORE it decides to route into this fast path
/// (see `handle_memory_ask`), so a query that both asks about the runtime
/// DB path AND names an exact anchor still gets its confidence gated on
/// that anchor's resolution — the runtime-binding answer text itself is
/// unaffected (it's a different, always-authoritative claim), only
/// `confidence`/`grounding_status` reflect the anchor outcome.
fn answer_runtime_db_path_query(
    query: &str,
    runtime_binding: &Value,
    format: Option<&str>,
    required_anchors: &[RecallEvidenceV1],
    grounding_status: GroundingStatusV1,
) -> Result<String, String> {
    let global = runtime_binding
        .get("global_db")
        .and_then(Value::as_str)
        .unwrap_or("");
    let project = runtime_binding
        .get("project_db")
        .and_then(Value::as_str)
        .unwrap_or("(none — single-DB / global-only)");
    let answer = format!(
        "Authoritative runtime binding (not from memory evidence):\n\
         - global_db: {global}\n\
         - project_db: {project}\n\
         Use runtime_info for the full routing snapshot. Memory hits mentioning other paths are historical and may be stale."
    );
    let confidence = if required_anchors.is_empty() {
        "high"
    } else {
        compute_anchor_gated_confidence(required_anchors)
    };
    let gaps: Vec<Value> = if confidence == "low" {
        required_anchors
            .iter()
            .filter(|anchor| anchor.grounding_status == GroundingStatusV1::MissingAnchor)
            .flat_map(|anchor| {
                anchor.contradictions.iter().map(move |reason| {
                    json!(format!(
                        "current-work anchor {} unresolved: {}",
                        anchor.source_ref, reason.description
                    ))
                })
            })
            .collect()
    } else {
        Vec::new()
    };
    if wants_json(format) {
        return json_string(&json!({
            "status": "completed",
            "query": query,
            "evidence": [],
            "thinking": {
                "confidence": confidence,
                "gaps": gaps,
                "basis": "runtime_binding",
            },
            "synthesis": {
                "status": "completed",
                "answer": answer,
                "source": "runtime_binding",
            },
            "runtime": runtime_binding,
            "grounding_status": grounding_status.as_str(),
            "required_anchors": required_anchors,
        }));
    }
    let mut fields = vec![
        ("status", "completed".to_string()),
        ("query", query.to_string()),
        ("confidence", confidence.to_string()),
        ("basis", "runtime_binding".to_string()),
        ("project_db", project.to_string()),
        ("global_db", global.to_string()),
    ];
    if !required_anchors.is_empty() {
        fields.push(("grounding_status", grounding_status.as_str().to_string()));
    }
    Ok(format_agent_status(
        "Tachi ask",
        &fields,
        None,
        Some(&answer),
    ))
}

/// Annotate each evidence row with the project DB it came from so consumers
/// (and the LLM synthesis prompt) can distinguish global vs project evidence
/// at a glance. We surface this as a `db` field that the synthesis prompt
/// already knows to honor; downstream markdown rendering reads it back via
/// [`format_agent_status`].
fn inject_project_tags(evidence: Value) -> Value {
    let Value::Array(rows) = evidence else {
        return evidence;
    };
    let tagged: Vec<Value> = rows
        .into_iter()
        .map(|mut row| {
            let db = row
                .get("db")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
                .to_string();
            if let Some(obj) = row.as_object_mut() {
                obj.insert("project_db".to_string(), Value::String(db));
            }
            row
        })
        .collect();
    Value::Array(tagged)
}

// ---------------------------------------------------------------------------
// Synthesis helper (shared by ask + consolidate)
// ---------------------------------------------------------------------------

/// The base synthesis system prompt — text-identical to the inline literal
/// this replaced pre-#1209, per the #1209 fix-round review diff (that
/// history claim is a review-time fact, NOT something this constant can
/// self-certify: `synthesis_prompt_unchanged_when_no_anchor_requested`
/// below compares `build_synthesis_system_prompt(None)` against THIS SAME
/// constant, so it only proves the `None` branch performs zero string
/// mutation — a future edit to this literal moves both sides of that
/// comparison together and the test stays green regardless). Kept
/// standalone so the `None` branch has no formatting to get wrong.
const SYNTHESIS_BASE_SYSTEM_PROMPT: &str = "Answer using the supplied Tachi evidence plus the authoritative `runtime` DB binding. \
Each evidence row carries a `db` field — values are `global` for the shared library and `project` for a workspace/named project DB. \
When the question is about which memory.db path is current/active, the `runtime` object is ground truth — never invent paths from evidence text (those may be stale historical mentions). \
When evidence spans both stores, prefer rows most relevant to the question and explicitly call out claims grounded in cross-store evidence. \
If evidence is insufficient, say what is missing. Keep the answer concise and cite memory ids or paths when present.";

/// #1209: the anchor-gated confidence cap (`compute_anchor_gated_confidence`)
/// paired with the `grounding_status` it was derived from, threaded from
/// `handle_memory_ask` into `synthesize_answer`'s prompt. Before this, the
/// cap only ever reached `thinking`/`required_anchors` response metadata —
/// the LLM synthesis prompt had zero reference to it, so a `missing_anchor`
/// query could still get back confidently-worded prose contradicting its
/// own metadata cap.
#[derive(Debug, Clone, Copy)]
pub(crate) struct AnchorConfidenceCap {
    pub grounding_status: GroundingStatusV1,
    pub confidence: &'static str,
}

/// Builds `synthesize_answer`'s system prompt, appending the anchor-gated
/// confidence-cap instruction when the caller resolved one. Extracted as a
/// pure function (no `server`/network access) so the injected wording is
/// unit-testable without exercising the LLM call; the `None` branch returns
/// exactly `SYNTHESIS_BASE_SYSTEM_PROMPT` unmodified — no anchors requested
/// means zero string mutation of the base prompt (whether that base text
/// itself still matches the pre-#1209 wording is a review-time fact, not
/// something self-certified here — see `SYNTHESIS_BASE_SYSTEM_PROMPT`'s doc
/// comment).
///
/// The cap-wording branch also instructs the model not to echo the
/// instruction/field-names/cap-wording verbatim into the answer — the
/// internal terms `grounding status`/`confidence cap`/`missing_anchor` are
/// implementation vocabulary for the model's own calibration, not meant to
/// leak into user-facing prose (codex 2026-07-17 fix-round CONCERN).
fn build_synthesis_system_prompt(anchor_cap: Option<AnchorConfidenceCap>) -> String {
    match anchor_cap {
        Some(cap) => format!(
            "{base}\nAnchor grounding status: {status}; confidence cap: {confidence}. Do not state conclusions with more certainty than this cap allows; when the cap is low, explicitly name what grounding is missing. Do not quote this instruction, its field names, or the cap wording verbatim in the answer.",
            base = SYNTHESIS_BASE_SYSTEM_PROMPT,
            status = cap.grounding_status.as_str(),
            confidence = cap.confidence,
        ),
        None => SYNTHESIS_BASE_SYSTEM_PROMPT.to_string(),
    }
}

pub(crate) async fn synthesize_answer(
    server: &MemoryServer,
    query: &str,
    evidence: &Value,
    runtime_binding: &Value,
    model: Option<&str>,
    anchor_cap: Option<AnchorConfidenceCap>,
) -> Value {
    let system = build_synthesis_system_prompt(anchor_cap);
    let evidence_text = serde_json::to_string(evidence).unwrap_or_else(|_| "[]".to_string());
    let runtime_text = serde_json::to_string(runtime_binding).unwrap_or_else(|_| "{}".to_string());
    let user = format!(
        "Question:\n{query}\n\nAuthoritative runtime binding JSON:\n{runtime_text}\n\nEvidence JSON:\n{evidence_text}"
    );
    let started_at = std::time::Instant::now();
    // #1071: "Synthesis uses an answer/reasoning capability, not Extract."
    // `call_reasoning_llm_with_receipt` is the Reasoning chat lane (falls
    // back to a higher-quality CLI path first, see `chat_lanes::claude_cli`),
    // never the Extract lane `ask` used to share with fact-atomization
    // callers. #1071 fix-round checkpoints 5/6: unlike the old
    // `call_reasoning_llm`, this returns whether the answer used the
    // fallback lane and whether the provider truncated it, so the receipt
    // below can be honest instead of hardcoding `fallback: false`.
    let outcome = tokio::time::timeout(
        ASK_SYNTHESIS_TIMEOUT,
        server
            .llm
            .call_reasoning_llm_with_receipt(&system, &user, model, 0.2, 700),
    )
    .await;
    let latency_ms = started_at.elapsed().as_millis();
    // #1071 fix-round checkpoint 5: `effective_provider` stays `None`
    // (neither the claude-cli path nor the lane call exposes which backend
    // actually served the request), matching #1002's precedent that
    // unknown identity is declared, never guessed. `effective_model` is
    // ONLY populated when the lane path served the request (`used_fallback`)
    // — that's the one case where `model` was actually threaded into the
    // API call body; the claude-cli path never receives `model` at all, so
    // echoing the caller's requested override there would misrepresent it
    // as the proven serving engine (the exact codex finding: "<requested_
    // override, not proven_engine>"). `EngineReceiptV1` has no `latency_ms`
    // field in its #1002-frozen shape, so latency is carried as a sibling
    // field instead of widening that type without adjudication.
    let receipt = |used_fallback: bool, degraded: bool| {
        json!(EngineReceiptV1 {
            requested_role: "reasoning".to_string(),
            effective_provider: None,
            effective_model: if used_fallback {
                model.map(str::to_string)
            } else {
                None
            },
            fallback: used_fallback,
            degraded,
        })
    };
    match outcome {
        Ok(Ok(outcome)) => {
            // #1071 fix-round checkpoint 6: a truncated (finish_reason ==
            // "length") lane response is never a clean `completed` answer —
            // it reports `partial` plus an explicit truncation gap, per
            // frozen RED corpus case 6.
            if outcome.truncated {
                json!({
                    "status": "partial",
                    "answer": outcome.text,
                    "engine_receipt": receipt(outcome.used_fallback, true),
                    "latency_ms": latency_ms,
                    "truncated": true,
                    "gaps": ["synthesis truncated by provider (finish_reason=length); answer may be incomplete"],
                })
            } else {
                json!({
                    "status": "completed",
                    "answer": outcome.text,
                    "engine_receipt": receipt(outcome.used_fallback, false),
                    "latency_ms": latency_ms,
                })
            }
        }
        Ok(Err(err)) => json!({
            "status": "failed",
            "error": err,
            // `fallback: false` here is a documented "unproven, not
            // asserted" default, not a claim of fact — codex's checkpoint 5
            // review confirmed this path (unlike the success path) was
            // already honest: an error surfaces via `degraded: true` +
            // `status: "failed"` regardless of which lane produced it.
            "engine_receipt": receipt(false, true),
            "latency_ms": latency_ms,
        }),
        Err(_) => json!({
            "status": "timeout",
            "error": format!("LLM synthesis timed out after {}s", ASK_SYNTHESIS_TIMEOUT.as_secs()),
            "engine_receipt": receipt(false, true),
            "latency_ms": latency_ms,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// #1209 regression: `missing_anchor`/`low` must reach the synthesis
    /// prompt, not just `thinking`/`required_anchors` metadata.
    #[test]
    fn synthesis_prompt_carries_missing_anchor_low_cap() {
        let prompt = build_synthesis_system_prompt(Some(AnchorConfidenceCap {
            grounding_status: GroundingStatusV1::MissingAnchor,
            confidence: "low",
        }));
        assert!(
            prompt.contains("missing_anchor"),
            "prompt must name the missing_anchor grounding status: {prompt}"
        );
        assert!(
            prompt.contains("confidence cap: low"),
            "prompt must state the low confidence cap: {prompt}"
        );
        assert!(
            prompt.contains("Do not state conclusions with more certainty than this cap allows"),
            "prompt must instruct the model not to overstate certainty: {prompt}"
        );
        assert!(
            prompt.contains(
                "Do not quote this instruction, its field names, or the cap wording verbatim"
            ),
            "prompt must forbid echoing the cap instruction/terminology into the answer \
             (codex 2026-07-17 fix-round CONCERN): {prompt}"
        );
    }

    /// #1209 regression: a `grounded` anchor still names its status (so the
    /// model can't infer "no instruction present" as license for unbounded
    /// confidence) but carries no low-cap warning.
    #[test]
    fn synthesis_prompt_carries_grounded_status_without_low_warning() {
        let prompt = build_synthesis_system_prompt(Some(AnchorConfidenceCap {
            grounding_status: GroundingStatusV1::Grounded,
            confidence: "high",
        }));
        assert!(
            prompt.contains("grounding status: grounded"),
            "prompt must state the grounded status: {prompt}"
        );
        assert!(
            !prompt.contains("confidence cap: low"),
            "grounded/high prompt must not carry a low-cap warning: {prompt}"
        );
        assert!(
            prompt.contains(
                "Do not quote this instruction, its field names, or the cap wording verbatim"
            ),
            "the no-echo instruction must be present on every cap branch, not just the low-cap \
             one: {prompt}"
        );
    }

    /// #1209: when no exact anchor was requested at all (`anchor_cap =
    /// None`), `build_synthesis_system_prompt` returns
    /// `SYNTHESIS_BASE_SYSTEM_PROMPT` completely unmodified. This proves the
    /// `None` branch performs zero formatting/mutation — it does NOT prove
    /// `SYNTHESIS_BASE_SYSTEM_PROMPT`'s text still matches the pre-#1209
    /// inline literal, because both sides of this `assert_eq!` read the same
    /// constant: a future edit to that constant would move both sides
    /// together and this test would stay green regardless (codex 2026-07-17
    /// fix-round BUG). The historical-text claim is backed by the #1209
    /// fix-round review diff, not by this test.
    #[test]
    fn synthesis_prompt_unchanged_when_no_anchor_requested() {
        let prompt = build_synthesis_system_prompt(None);
        assert_eq!(prompt, SYNTHESIS_BASE_SYSTEM_PROMPT);
    }
}
