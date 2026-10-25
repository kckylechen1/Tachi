use crate::server_state::{DbScope, MemoryServer};
use chrono::Utc;
use memcore::{MemoryEntry, MemoryStore};
use serde_json::json;

pub(super) const DLQ_MAX_ENTRIES: usize = 200;
pub(super) const DLQ_TTL_SECS: u64 = 3600;

#[derive(Clone)]
pub(super) struct DeadLetter {
    pub(super) id: String,
    pub(super) tool_name: String,
    pub(super) arguments: Option<serde_json::Map<String, serde_json::Value>>,
    pub(super) error: String,
    pub(super) error_category: String,
    pub(super) timestamp: String,
    pub(super) retry_count: u32,
    pub(super) max_retries: u32,
    pub(super) status: String,
}

fn dead_letter_is_fresh(dl: &DeadLetter, now: chrono::DateTime<Utc>) -> bool {
    chrono::DateTime::parse_from_rfc3339(&dl.timestamp)
        .map(|ts| (now - ts.with_timezone(&Utc)).num_seconds() < DLQ_TTL_SECS as i64)
        .unwrap_or(false)
}

pub(super) fn prune_expired_dead_letters(
    dlq: &mut std::collections::VecDeque<DeadLetter>,
    now: chrono::DateTime<Utc>,
) {
    dlq.retain(|dl| dead_letter_is_fresh(dl, now));
}

pub(super) fn push_dead_letter_with_limits(
    dlq: &mut std::collections::VecDeque<DeadLetter>,
    dl: DeadLetter,
    now: chrono::DateTime<Utc>,
) {
    prune_expired_dead_letters(dlq, now);
    dlq.push_back(dl);
    while dlq.len() > DLQ_MAX_ENTRIES {
        dlq.pop_front();
    }
}

pub(super) fn categorize_error(error: &str) -> String {
    let lower = error.to_lowercase();
    if lower.contains("not found") || lower.contains("not_found") {
        "not_found".to_string()
    } else if lower.contains("timeout") || lower.contains("timed out") {
        "timeout".to_string()
    } else if lower.contains("invalid") || lower.contains("param") {
        "invalid_params".to_string()
    } else {
        "internal".to_string()
    }
}

pub(super) fn dlq_replay_is_explicitly_safe(
    tool_name: &str,
    arguments: Option<&serde_json::Map<String, serde_json::Value>>,
) -> bool {
    crate::action_effect::dlq_replay_is_explicitly_safe(tool_name, arguments)
}

pub(super) fn should_enqueue_dlq(
    tool_name: &str,
    arguments: Option<&serde_json::Map<String, serde_json::Value>>,
    is_native_route: bool,
) -> bool {
    if tool_name.starts_with("dlq_")
        || tool_name.starts_with("ghost_")
        || tool_name == "get_pipeline_status"
    {
        return false;
    }
    if is_native_route || !dlq_replay_is_explicitly_safe(tool_name, arguments) {
        return false;
    }
    true
}

pub(super) fn slim_entry(e: &MemoryEntry, db: DbScope) -> serde_json::Value {
    let mut obj = serde_json::Map::new();
    obj.insert("id".into(), json!(e.id));
    obj.insert("db".into(), json!(db.as_str()));
    obj.insert("text".into(), json!(e.text));
    if !e.summary.is_empty() {
        obj.insert("summary".into(), json!(e.summary));
    }
    obj.insert("path".into(), json!(e.path));
    if !e.topic.is_empty() {
        obj.insert("topic".into(), json!(e.topic));
    }
    if !e.keywords.is_empty() {
        obj.insert("keywords".into(), json!(e.keywords));
    }
    obj.insert("importance".into(), json!(e.importance));
    obj.insert("timestamp".into(), json!(e.timestamp));
    obj.insert("category".into(), json!(e.category));
    obj.insert("scope".into(), json!(e.scope));
    if !e.persons.is_empty() {
        obj.insert("persons".into(), json!(e.persons));
    }
    if !e.entities.is_empty() {
        obj.insert("entities".into(), json!(e.entities));
    }
    if !e.location.is_empty() {
        obj.insert("location".into(), json!(e.location));
    }
    if let Some(ref rp) = e.retention_policy {
        obj.insert("retention_policy".into(), json!(rp));
    }
    if let Some(ref domain) = e.domain {
        if !domain.is_empty() {
            obj.insert("domain".into(), json!(domain));
        }
    }
    if e.archived {
        obj.insert("archived".into(), json!(true));
    }
    if let serde_json::Value::Object(ref m) = e.metadata {
        if !m.is_empty() {
            obj.insert("metadata".into(), json!(m));
        }
    }
    serde_json::Value::Object(obj)
}

/// Like `slim_entry` but additionally surfaces enrichment status fields:
///   - `embedding_pending`: true when no vector has been written yet
///   - `summary_pending`:   true when no summary has been written yet
///   - `keywords_status` / `keywords_pending`: write-side synonym/bilingual
///     keyword enrichment (#921) — only present when metadata carries
///     `enrichment.keywords_status` (flag-off ⇒ field absent = zero change)
///   - `foundry_jobs`:      array of `{id, kind, status, created_at}` for any
///     foundry jobs currently touching this memory id (queued/running first,
///     omitted when empty)
///
/// Used by `get_memory` so agents can tell whether async post-write enrichment
/// is still in flight versus complete. Intentionally NOT used by `list_memories`
/// or search results to avoid an N+1 lookup against `foundry_jobs`.
pub(super) fn slim_entry_with_enrichment(
    server: &MemoryServer,
    e: &MemoryEntry,
    db: DbScope,
    named_project: Option<&str>,
) -> serde_json::Value {
    let mut obj = match slim_entry(e, db) {
        serde_json::Value::Object(m) => m,
        _ => serde_json::Map::new(),
    };

    obj.insert("embedding_pending".into(), json!(e.vector.is_none()));
    obj.insert("summary_pending".into(), json!(e.summary.is_empty()));

    if let Some(keywords_status) = e
        .metadata
        .get("enrichment")
        .and_then(|value| value.get("keywords_status"))
        .and_then(|value| value.as_str())
    {
        obj.insert("keywords_status".into(), json!(keywords_status));
        obj.insert(
            "keywords_pending".into(),
            json!(keywords_status == "pending"),
        );
    }

    // Foundry job lookup runs against the same store the entry came from. Any
    // failure (table missing, transient lock, …) is non-fatal — we simply omit
    // the field so callers can rely on the entry payload itself.
    let lookup = |store: &mut MemoryStore| -> Result<Vec<memcore::FoundryJobSummary>, String> {
        memcore::find_foundry_jobs_for_memory(store.connection(), &e.id)
            .map_err(|err| err.to_string())
    };

    let jobs_res = match (db, named_project) {
        (DbScope::Project, Some(name)) => server.with_named_project_store_read(name, lookup),
        (DbScope::Project, None) => server.with_project_store_read(lookup),
        (DbScope::Global, _) => server.with_global_store_read(lookup),
    };

    if let Ok(jobs) = jobs_res {
        if !jobs.is_empty() {
            obj.insert("foundry_jobs".into(), json!(jobs));
        }
    }

    serde_json::Value::Object(obj)
}

fn round_score(value: f64) -> f64 {
    (value * 1000.0).round() / 1000.0
}

fn text_excerpt(text: &str, max_chars: usize) -> Option<String> {
    let compact = crate::utils::compact_text_line(text, max_chars);
    if compact.is_empty() {
        return None;
    }
    Some(compact)
}

/// Short single-line preview for listing UIs (token-cheap).
const SEARCH_EXCERPT_PREVIEW_CHARS: usize = 480;

/// Usable evidence body for QA / agent answer paths (tachi#2114).
///
/// LongMemEval session memories are typically 11–16 KB with answer needles
/// deep in the transcript. A 480-char head excerpt yields session hit@10≈0.98
/// but F1≈0.05 because the answerer never sees the span. Competitors
/// (Graphiti/Zep/Hy-Memory/Mem0) put full episode/fact content into search
/// context (~4–10k tokens). Cap at 32 KiB so session-sized memories hydrate
/// without unbounded blowups; truncated rows set `text_truncated`.
const SEARCH_EVIDENCE_TEXT_MAX_CHARS: usize = 32_768;

/// Newline-preserving truncate for search evidence (`text` field).
fn evidence_text(text: &str, max_chars: usize) -> Option<(String, bool)> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    let count = trimmed.chars().count();
    if count <= max_chars {
        Some((trimmed.to_string(), false))
    } else {
        let keep = max_chars.saturating_sub(3);
        let truncated: String = trimmed.chars().take(keep).collect();
        Some((format!("{truncated}..."), true))
    }
}

/// Token-efficient search hit: id/path/topic/summary plus scores only.
/// Full evidence body is also attached as `text` (capped; see
/// `SEARCH_EVIDENCE_TEXT_MAX_CHARS`) so QA paths do not need a follow-up
/// `get_memory` for session-sized memories (tachi#2114). Metadata remains
/// available via `get_memory` / `include_metadata`.
///
/// Pass `include_metadata = true` to also surface the full `metadata` map.
/// This is required for callers that need to read semantic fields like
/// kanban `a2a_state`; for everything else the default keeps token usage
/// tight by omitting the metadata blob.
pub(super) fn slim_search_result(
    result: &memcore::SearchResult,
    db: DbScope,
    include_metadata: bool,
) -> serde_json::Value {
    let entry = &result.entry;
    let mut obj = serde_json::Map::new();
    obj.insert("id".into(), json!(entry.id));
    obj.insert("db".into(), json!(db.as_str()));
    obj.insert("path".into(), json!(entry.path));
    obj.insert("timestamp".into(), json!(entry.timestamp));
    obj.insert("revision".into(), json!(entry.revision));
    if !entry.valid_from.trim().is_empty() {
        obj.insert("valid_from".into(), json!(entry.valid_from));
    }
    if let Some(valid_until) = entry
        .valid_until
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        obj.insert("valid_until".into(), json!(valid_until));
    }
    if entry.archived {
        obj.insert("archived".into(), json!(true));
    }
    if !entry.source.trim().is_empty() {
        obj.insert("source".into(), json!(entry.source));
    }
    if !entry.topic.is_empty() {
        obj.insert("topic".into(), json!(entry.topic));
    }
    if !entry.summary.is_empty() {
        obj.insert("summary".into(), json!(entry.summary));
    }
    if let Some(excerpt) = text_excerpt(&entry.text, SEARCH_EXCERPT_PREVIEW_CHARS) {
        obj.insert("excerpt".into(), json!(excerpt));
    }
    // tachi#2114: hydrate usable evidence for QA. Prefer `text` over the
    // short `excerpt` preview when answering from search hits.
    if let Some((body, truncated)) = evidence_text(&entry.text, SEARCH_EVIDENCE_TEXT_MAX_CHARS) {
        obj.insert("text".into(), json!(body));
        if truncated {
            obj.insert("text_truncated".into(), json!(true));
        }
    }
    if include_metadata {
        // Always include metadata when requested — consumers like the kanban
        // board rely on it to surface a2a_state, eval_ledger_id, agent, etc.
        obj.insert("metadata".into(), entry.metadata.clone());
    } else {
        // Surface referenced source files (metadata.files) inline so agents can
        // jump to the file without a follow-up get_memory. Only string entries
        // are kept.
        if let Some(serde_json::Value::Array(files)) = entry.metadata.get("files") {
            let paths: Vec<&str> = files.iter().filter_map(|v| v.as_str()).collect();
            if !paths.is_empty() {
                obj.insert("files".into(), json!(paths));
            }
        }
        if let Some(source_memory_ids) = entry
            .metadata
            .get("source_memory_ids")
            .and_then(serde_json::Value::as_array)
        {
            let source_memory_ids: Vec<&str> = source_memory_ids
                .iter()
                .filter_map(serde_json::Value::as_str)
                .take(8)
                .collect();
            if !source_memory_ids.is_empty() {
                obj.insert("source_memory_ids".into(), json!(source_memory_ids));
            }
        }
        if let Some(source_refs) = compact_source_refs(entry.metadata.get("source_refs")) {
            obj.insert("source_refs".into(), source_refs);
        }
        if let Some(superseded_by) = entry
            .metadata
            .get("superseded_by")
            .and_then(serde_json::Value::as_str)
        {
            if !superseded_by.trim().is_empty() {
                obj.insert("superseded_by".into(), json!(superseded_by));
            }
        }
    }
    obj.insert(
        "relevance".into(),
        json!(round_score(result.score.final_score)),
    );
    obj.insert(
        "score".into(),
        json!({
            "vector": round_score(result.score.vector),
            "fts": round_score(result.score.fts),
            "symbolic": round_score(result.score.symbolic),
            "decay": round_score(result.score.decay),
            "final": round_score(result.score.final_score),
        }),
    );
    // tachi#1647 2c: this is a hand-built field whitelist, not a
    // `serde_json::to_value(result)` passthrough, so `graph_injected` /
    // `graph_provenance` must be forwarded explicitly or an ordinary
    // consumer (search_memory, tachi_search, wiki search — every caller of
    // `slim_search_result`) would silently lose them. Only present on
    // graph-injected rows, so an expansion-off search adds zero new bytes
    // here (mirrors `SearchResult`'s own `skip_serializing_if`).
    if result.graph_injected {
        obj.insert("graph_injected".into(), json!(true));
    }
    if let Some(provenance) = &result.graph_provenance {
        obj.insert(
            "graph_provenance".into(),
            json!({
                "via_edge": provenance.via_edge,
                "from_id": provenance.from_id,
                "activation": round_score(provenance.activation as f64),
                "distance": provenance.distance,
            }),
        );
    }
    serde_json::Value::Object(obj)
}

/// Keep provenance navigable on compact recall surfaces without exposing an
/// arbitrary metadata blob. Capture rows use typed `{ref_type, ref_id,
/// revision}` references; old rows may have plain strings.
pub(super) fn compact_source_refs(value: Option<&serde_json::Value>) -> Option<serde_json::Value> {
    let refs = value?.as_array()?;
    let refs: Vec<serde_json::Value> = refs
        .iter()
        .filter_map(|reference| match reference {
            serde_json::Value::String(value) if !value.trim().is_empty() => Some(json!(value)),
            serde_json::Value::Object(object) => {
                let ref_type = object.get("ref_type").and_then(serde_json::Value::as_str)?;
                let ref_id = object.get("ref_id").and_then(serde_json::Value::as_str)?;
                if ref_type.trim().is_empty() || ref_id.trim().is_empty() {
                    return None;
                }
                let mut compact = serde_json::Map::new();
                compact.insert("ref_type".into(), json!(ref_type));
                compact.insert("ref_id".into(), json!(ref_id));
                if let Some(revision) = object.get("revision") {
                    if revision.is_string() || revision.is_number() {
                        compact.insert("revision".into(), revision.clone());
                    }
                }
                Some(serde_json::Value::Object(compact))
            }
            _ => None,
        })
        .take(8)
        .collect();
    (!refs.is_empty()).then_some(serde_json::Value::Array(refs))
}

pub(super) fn slim_l0_rule(rule: &MemoryEntry, db: DbScope) -> serde_json::Value {
    let summary = if !rule.summary.is_empty() {
        rule.summary.clone()
    } else {
        rule.text.chars().take(200).collect()
    };
    json!({
        "id": rule.id,
        "db": db.as_str(),
        "path": rule.path,
        "topic": if rule.topic.is_empty() { serde_json::Value::Null } else { json!(rule.topic) },
        "summary": summary,
        "l0_rule": true,
    })
}

// Agent-rules content predicates, shared with status readiness and the
// setup-wizard write path.
pub(crate) const HARNESS_MARKER_START: &str = "<!-- TACHI:HARNESS:START -->";
pub(crate) const HARNESS_MARKER_END: &str = "<!-- TACHI:HARNESS:END -->";
pub(crate) const AGENT_RULES_START: &str = "<!-- BEGIN TACHI MEMORY RULES -->";
pub(crate) const AGENT_RULES_END: &str = "<!-- END TACHI MEMORY RULES -->";

/// Match whole marker lines outside fenced examples. Byte ranges include the
/// marker lines' indentation and newline, so replacement preserves all bytes
/// outside the bounded block (including CRLF and surrounding whitespace).
pub(crate) fn ordered_marker_range(
    content: &str,
    start: &str,
    end: &str,
) -> Option<(usize, usize)> {
    let mut offset = 0;
    let mut started_at = None;
    let mut fence: Option<(u8, usize)> = None;
    for line in content.split_inclusive('\n') {
        let line_start = offset;
        offset += line.len();
        let text = line.trim();
        let first = text.as_bytes().first().copied();
        let fence_length = if matches!(first, Some(b'`' | b'~')) {
            text.bytes().take_while(|byte| Some(*byte) == first).count()
        } else {
            0
        };
        if let Some((delimiter, length)) = fence {
            if first == Some(delimiter)
                && fence_length >= length
                && text[fence_length..].trim().is_empty()
            {
                fence = None;
            }
            continue;
        }
        if fence_length >= 3 {
            fence = Some((first.expect("fence delimiter"), fence_length));
            continue;
        }
        if text == start {
            // A later opening marker must not grant authority to erase text
            // following an earlier orphan marker.
            started_at = Some(line_start);
        } else if text == end {
            if let Some(start) = started_at {
                return Some((start, offset));
            }
        }
    }
    None
}

pub(crate) fn is_managed_agent_rules(content: &str) -> bool {
    ordered_marker_range(content, HARNESS_MARKER_START, HARNESS_MARKER_END).is_some()
}

pub(crate) fn is_installed_agent_rules(content: &str) -> bool {
    is_managed_agent_rules(content)
        || ordered_marker_range(content, AGENT_RULES_START, AGENT_RULES_END).is_some()
}

#[cfg(test)]
mod slim_search_result_graph_tests {
    use super::*;
    use memcore::{GraphInjectionProvenance, HybridScore, SearchResult};
    use serde_json::json as jsonval;

    fn entry(id: &str) -> MemoryEntry {
        let now = Utc::now().to_rfc3339();
        MemoryEntry {
            id: id.to_string(),
            path: "/test".to_string(),
            summary: String::new(),
            text: String::new(),
            importance: 0.5,
            timestamp: now.clone(),
            valid_from: now,
            valid_until: None,
            category: "fact".to_string(),
            topic: String::new(),
            keywords: Vec::new(),
            persons: Vec::new(),
            entities: Vec::new(),
            location: String::new(),
            source: String::new(),
            scope: "user".to_string(),
            archived: false,
            access_count: 0,
            scored_count: 0,
            last_access: None,
            last_use_at: None,
            revision: 1,
            vector: None,
            retention_policy: None,
            domain: None,
            metadata: jsonval!({}),
            recall_count: 0,
            query_diversity: 0,
            tier: "raw".to_string(),
        }
    }

    /// tachi#1647 2c: `slim_search_result` is a hand-built field whitelist —
    /// without explicit forwarding, `graph_injected`/`graph_provenance`
    /// would silently vanish between `SearchResult` and every production
    /// row-shaping caller (search_memory, tachi_search, wiki search).
    #[test]
    fn forwards_graph_injected_and_provenance_when_present() {
        let result = SearchResult {
            entry: entry("hop2"),
            score: HybridScore {
                vector: 0.0,
                fts: 0.0,
                symbolic: 0.0,
                decay: 0.0,
                final_score: 0.4,
            },
            graph_injected: true,
            graph_provenance: Some(GraphInjectionProvenance {
                via_edge: "elaborates".to_string(),
                from_id: "seed".to_string(),
                activation: 0.42,
                distance: 2,
            }),
        };
        let row = slim_search_result(&result, DbScope::Project, false);
        assert_eq!(row["graph_injected"], jsonval!(true));
        assert_eq!(row["graph_provenance"]["via_edge"], jsonval!("elaborates"));
        assert_eq!(row["graph_provenance"]["from_id"], jsonval!("seed"));
        assert_eq!(row["graph_provenance"]["distance"], jsonval!(2));
    }

    /// Ordinary (non-injected) rows must carry neither key — expansion-off
    /// search adds zero new bytes to the response.
    #[test]
    fn omits_graph_fields_for_ordinary_rows() {
        let result = SearchResult {
            entry: entry("ordinary"),
            score: HybridScore {
                vector: 0.5,
                fts: 0.5,
                symbolic: 0.0,
                decay: 1.0,
                final_score: 0.5,
            },
            graph_injected: false,
            graph_provenance: None,
        };
        let row = slim_search_result(&result, DbScope::Project, false);
        assert!(row.get("graph_injected").is_none());
        assert!(row.get("graph_provenance").is_none());
    }

    /// tachi#2114: long session bodies must hydrate `text` past the 480-char
    /// excerpt preview so answer needles deep in the transcript stay visible.
    #[test]
    fn hydrates_full_text_beyond_excerpt_preview_for_long_entries() {
        let needle = "UNIQUE_ANSWER_NEEDLE_DEEP_IN_SESSION_42";
        let mut body = "SESSION preamble\n".to_string();
        body.push_str(&"x".repeat(2_000));
        body.push('\n');
        body.push_str(needle);
        body.push('\n');
        body.push_str(&"y".repeat(2_000));

        let mut e = entry("long-session");
        e.text = body.clone();
        e.summary = "session summary".into();
        let result = SearchResult {
            entry: e,
            score: HybridScore {
                vector: 0.9,
                fts: 0.1,
                symbolic: 0.0,
                decay: 1.0,
                final_score: 0.9,
            },
            graph_injected: false,
            graph_provenance: None,
        };
        let row = slim_search_result(&result, DbScope::Project, false);

        let excerpt = row["excerpt"].as_str().expect("excerpt");
        assert!(
            excerpt.chars().count() <= SEARCH_EXCERPT_PREVIEW_CHARS + 3,
            "excerpt should stay a short preview, got {} chars",
            excerpt.chars().count()
        );
        assert!(
            !excerpt.contains(needle),
            "needle sits past the preview window; excerpt must not be the sole evidence"
        );

        let text = row["text"].as_str().expect("text hydrate");
        assert!(
            text.contains(needle),
            "hydrated text must include deep answer needle"
        );
        assert_eq!(text, body.trim());
        assert!(row.get("text_truncated").is_none());
    }

    /// tachi#2114: oversized bodies truncate with a flag rather than unbounded.
    #[test]
    fn truncates_evidence_text_at_cap_and_flags() {
        let oversized: String = "あ".repeat(SEARCH_EVIDENCE_TEXT_MAX_CHARS + 50);
        let mut e = entry("oversized");
        e.text = oversized;
        let result = SearchResult {
            entry: e,
            score: HybridScore {
                vector: 0.1,
                fts: 0.1,
                symbolic: 0.0,
                decay: 1.0,
                final_score: 0.1,
            },
            graph_injected: false,
            graph_provenance: None,
        };
        let row = slim_search_result(&result, DbScope::Project, false);
        let text = row["text"].as_str().expect("text");
        assert!(text.ends_with("..."));
        assert!(text.chars().count() <= SEARCH_EVIDENCE_TEXT_MAX_CHARS);
        assert_eq!(row["text_truncated"], jsonval!(true));
    }
}

#[cfg(test)]
mod dlq_tests {
    use super::*;

    fn dead_letter(id: &str, timestamp: String) -> DeadLetter {
        DeadLetter {
            id: id.to_string(),
            tool_name: "test_tool".to_string(),
            arguments: None,
            error: "boom".to_string(),
            error_category: "internal".to_string(),
            timestamp,
            retry_count: 0,
            max_retries: 3,
            status: "pending".to_string(),
        }
    }

    #[test]
    fn push_dead_letter_prunes_expired_entries_before_enqueue() {
        let now = Utc::now();
        let stale = (now - chrono::Duration::seconds(DLQ_TTL_SECS as i64 + 1)).to_rfc3339();
        let fresh = (now - chrono::Duration::seconds(1)).to_rfc3339();
        let mut dlq = std::collections::VecDeque::from([
            dead_letter("stale", stale),
            dead_letter("fresh", fresh),
        ]);

        push_dead_letter_with_limits(&mut dlq, dead_letter("new", now.to_rfc3339()), now);

        let ids = dlq.iter().map(|dl| dl.id.as_str()).collect::<Vec<_>>();
        assert_eq!(ids, vec!["fresh", "new"]);
    }

    #[test]
    fn dlq_replay_authority_rejects_write_tools_and_hub_call() {
        assert!(!dlq_replay_is_explicitly_safe("save_memory", None));
        assert!(!dlq_replay_is_explicitly_safe("hub_call", None));
        assert!(!dlq_replay_is_explicitly_safe("remote__save_memory", None));
        assert!(!dlq_replay_is_explicitly_safe(
            "tachi_memory",
            Some(&serde_json::Map::from_iter([(
                "action".to_string(),
                json!("save")
            )]))
        ));
        assert!(!dlq_replay_is_explicitly_safe(
            "tachi_memory",
            Some(&serde_json::Map::from_iter([(
                "action".to_string(),
                json!("search")
            )]))
        ));
        assert!(!dlq_replay_is_explicitly_safe(
            "tachi_event",
            Some(&serde_json::Map::from_iter([(
                "action".to_string(),
                json!("emit")
            )]))
        ));
        assert!(dlq_replay_is_explicitly_safe(
            "tachi_event",
            Some(&serde_json::Map::from_iter([(
                "action".to_string(),
                json!("metrics")
            )]))
        ));
    }

    #[test]
    fn should_enqueue_dlq_requires_an_explicit_safe_effect() {
        assert!(!should_enqueue_dlq("save_memory", None, true));
        assert!(!should_enqueue_dlq("hub_call", None, false));
        assert!(!should_enqueue_dlq("remote__echo", None, false));
        assert!(!should_enqueue_dlq("remote__unknown", None, false));
    }

    /// #1098: the owner's adjudication comment named this exact call shape —
    /// `remote__tachi_memory(action='save')` failing through a non-native
    /// (proxied) route used to pass the facade name match (raw name vs.
    /// canonical) and enter the DLQ, where it could be auto-replayed and
    /// duplicate the write. All proxy-qualified names now fail closed because
    /// the server has no per-remote-tool replay authority.
    #[test]
    fn f1098_should_enqueue_dlq_closes_remote_facade_mutation_bypass() {
        for action in ["save", "gc", "claim", "release"] {
            let args = serde_json::Map::from_iter([("action".to_string(), json!(action))]);
            assert!(
                !should_enqueue_dlq("remote__tachi_memory", Some(&args), false),
                "remote__tachi_memory(action='{action}') must not enter the DLQ"
            );
        }
        for action in ["emit", "project", "promote"] {
            let args = serde_json::Map::from_iter([("action".to_string(), json!(action))]);
            assert!(
                !should_enqueue_dlq("remote__tachi_event", Some(&args), false),
                "remote__tachi_event(action='{action}') must not enter the DLQ"
            );
        }
        let aliased_read_args =
            serde_json::Map::from_iter([("action".to_string(), json!("metrics"))]);
        assert!(!should_enqueue_dlq(
            "remote__tachi_event",
            Some(&aliased_read_args),
            false
        ));

        // Local read-only classification remains available to callers that
        // have independently established a non-native replay boundary.
        let local_read_args =
            serde_json::Map::from_iter([("action".to_string(), json!("metrics"))]);
        assert!(should_enqueue_dlq(
            "tachi_event",
            Some(&local_read_args),
            false
        ));
    }
}

/// Closed verification-status vocabulary shared by the write-side gate
/// (`verify_ops::storage::normalize_status`) and the read/render side.
pub(crate) fn valid_status(status: &str) -> bool {
    matches!(
        status,
        "pending" | "running" | "passed" | "failed" | "skipped" | "stale"
    )
}

/// #1454 H2: normalize a CALLER-AUTHORIZED status string at the READ/render
/// boundary. Ledger `overall` and item `status` are caller-authored (direct
/// file writes bypass the write-side `normalize_status` gate); anything
/// outside the closed vocabulary renders as the fixed `invalid` marker and is
/// NEVER interpolated raw into board/briefing/markup (a crafted value like
/// `]\n- [passed] ...` must not mint a new board line).
pub(crate) fn markup_status(value: &str) -> String {
    let normalized = value.trim().to_ascii_lowercase();
    if valid_status(&normalized) {
        normalized
    } else {
        "invalid".to_string()
    }
}

/// Default lease TTL: a claim whose heartbeat is older than this is presented
/// as expired by readers (briefing/collision-check), without a second write —
/// same lazy-expiry idiom as the exec_envs sweep backstop, just read-side.
pub(crate) const CLAIM_TTL_SECONDS: i64 = 30 * 60;
