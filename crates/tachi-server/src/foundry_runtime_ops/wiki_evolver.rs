//! REM Wiki Evolver — weekly synthesis of `tier=pattern` memories into
//! draft wiki entries.
//!
//! ## What this does
//! 1. Collects `tier=pattern` memories from the last 7 days across
//!    global + project DBs.
//! 2. Clusters them by topic/keyword overlap (lightweight; no embeddings).
//! 3. For each cluster with ≥ 2 members, calls an LLM to synthesize a
//!    structured draft wiki entry.
//! 4. Writes each draft to the wiki project DB under `/wiki/drafts/` with a
//!    deterministic source-set identity and `review_status=pending`.
//!
//! ## What this does NOT do
//! - Auto-activate: all output is `review_status=pending`. Humans (or a
//!   future gate step) must explicitly promote drafts to `/wiki/<slug>`.
//! - Modify existing wiki entries: drafts are always new entries.
//! - Run during daily pipeline: called once per week (Sunday 05:00 Shanghai).
//!
//! ## Safety
//! The LLM synthesis step uses low temperature and produces *pattern
//! summaries*, NOT causal inference claims. The module never writes to
//! the "active" wiki path space (`/wiki/` without `drafts/` prefix).

use std::collections::HashMap;

use chrono::Utc;
use serde_json::{json, Value};

use super::scrub_agent_noise;
use crate::server_state::MemoryServer;
use memcore::types::MemoryEntry;
use memcore::{InsertMemoryResult, MemoryError};
use tachi_llm::{CompletionStatusV1, Generated, LlmClient, PersistedModelInvocationReceiptV1};

// ─── Constants ────────────────────────────────────────────────────────────────

/// Minimum cluster size to attempt LLM synthesis.
const MIN_CLUSTER_SIZE: usize = 2;

/// Maximum number of clusters to synthesize per weekly run (budget cap).
const MAX_CLUSTERS_PER_RUN: usize = 20;

/// Maximum memory entries per cluster sent to the LLM (keep prompt small).
const MAX_ENTRIES_PER_CLUSTER: usize = 8;

/// Minimum text length after noise scrubbing to include in synthesis.
const MIN_TEXT_LEN: usize = 60;

/// Importance assigned to pending draft entries (below "active" wiki threshold).
const DRAFT_IMPORTANCE: f64 = 0.65;

const REM_WIKI_SOURCE_SET_CONTRACT: &str = "rem-wiki-source-set-v1";
const REM_WIKI_PRODUCER_VERSION: &str = "weekly-wiki-evolver-v1";

#[derive(
    Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, serde::Deserialize, serde::Serialize,
)]
#[serde(rename_all = "snake_case")]
enum RemSourceScope {
    Global,
    Project,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, serde::Deserialize, serde::Serialize)]
struct RemSourceStore {
    scope: RemSourceScope,
    identity: String,
}

#[derive(Clone, Debug)]
struct RemCandidate {
    store: RemSourceStore,
    entry: MemoryEntry,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, serde::Deserialize, serde::Serialize)]
struct RemSourceRef {
    store: RemSourceStore,
    id: String,
}

#[derive(serde::Serialize)]
struct RemSourceSetIdentityV1<'a> {
    contract: &'static str,
    producer_version: &'static str,
    sources: &'a [RemSourceRef],
}

/// System prompt for wiki synthesis.
const WIKI_SYNTHESIS_SYSTEM: &str = r#"You are synthesizing programming session patterns into a structured wiki reference entry.

## Your Task
Read the clustered memory entries below. Produce a single cohesive wiki entry that a future agent can use as a reference — not a session log, but a clean how-to / decision record.

## Output Format
Return ONLY a single valid JSON object (no markdown, no wrapping):
{
  "title": "<clear, noun-phrase title, ≤80 chars>",
  "body": "<structured Markdown content, 300–1200 chars; use headers if helpful>",
  "summary": "<one sentence TL;DR, ≤120 chars>",
  "keywords": ["<tag1>", "<tag2>", ...],
  "entities": ["<project>", "<tool>", ...],
  "domain": "<primary domain, e.g. rust, mcp, agent, memory, infra>"
}

## Rules
1. Title must be a noun phrase describing the pattern — NOT a session headline.
2. Body must be written as forward-looking reference: what to do, why, and known gotchas.
3. Remove session noise: timestamps, agent tool traces, JSON blobs, file-path lists.
4. Keep concrete details: command patterns, crate names, error signatures, config keys.
5. Output ONLY the JSON object — no prose before or after.
"#;

// ─── Public entry point ───────────────────────────────────────────────────────

/// Run the weekly REM wiki evolution pass.
///
/// Called once per week (Sunday 05:00 Asia/Shanghai) from the daemon
/// bootstrap loop. Safe to call multiple times: unfinished source-store marker
/// groups are recovered from the deterministic Wiki operation before new
/// candidates are collected.
pub(crate) async fn run_weekly_wiki_evolution(
    server: &MemoryServer,
) -> Result<WikiEvolverReport, String> {
    recover_pending_rem_operations(server)?;
    let candidates = collect_pattern_memories(server)?;
    if candidates.is_empty() {
        return Ok(WikiEvolverReport {
            clusters_found: 0,
            drafts_written: 0,
            skipped: 0,
            errors: 0,
        });
    }

    let clusters = cluster_by_topic(&candidates);
    let mut drafts_written = 0usize;
    let mut skipped = 0usize;
    let mut errors = 0usize;

    for (topic, members) in clusters.iter().take(MAX_CLUSTERS_PER_RUN) {
        if members.len() < MIN_CLUSTER_SIZE {
            skipped += 1;
            continue;
        }

        match synthesize_and_save(server, topic, members).await {
            Ok(()) => {
                drafts_written += 1;
            }
            Err(e) => {
                errors += 1;
                eprintln!("[wiki_evolver] error synthesizing cluster '{topic}': {e}");
            }
        }
    }

    let report = WikiEvolverReport {
        clusters_found: clusters.len(),
        drafts_written,
        skipped,
        errors,
    };
    eprintln!(
        "[wiki_evolver] weekly run complete: clusters={} drafts={} skipped={} errors={}",
        report.clusters_found, report.drafts_written, report.skipped, report.errors
    );
    Ok(report)
}

/// Summary returned to the caller (daily_pipeline / bootstrap loop).
#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct WikiEvolverReport {
    pub clusters_found: usize,
    pub drafts_written: usize,
    pub skipped: usize,
    pub errors: usize,
}

// ─── Candidate collection ─────────────────────────────────────────────────────

fn rem_source_store_identity(scope: RemSourceScope, db_path: &std::path::Path) -> RemSourceStore {
    let canonical = std::fs::canonicalize(db_path).unwrap_or_else(|_| db_path.to_path_buf());
    RemSourceStore {
        scope,
        identity: canonical.display().to_string(),
    }
}

fn collect_pattern_memories(server: &MemoryServer) -> Result<Vec<RemCandidate>, String> {
    let collect = |store: &mut memcore::MemoryStore| -> Result<Vec<MemoryEntry>, String> {
        store
            .unprocessed_pattern_memories()
            .map_err(|e| format!("query pattern memories: {e}"))
    };

    let global_store =
        rem_source_store_identity(RemSourceScope::Global, &server.global_db_path_buf());
    let mut entries = server
        .with_global_store_read(collect)
        .map_err(|e| format!("REM global candidate collection: {e}"))?
        .into_iter()
        .map(|entry| RemCandidate {
            store: global_store.clone(),
            entry,
        })
        .collect::<Vec<_>>();
    if server.has_project_db() {
        let project_path = server.project_db_path_buf().ok_or_else(|| {
            "REM project candidate collection: project path is missing".to_string()
        })?;
        let project_store = rem_source_store_identity(RemSourceScope::Project, &project_path);
        let project = server
            .with_project_store_read(collect)
            .map_err(|e| format!("REM project candidate collection: {e}"))?;
        entries.extend(project.into_iter().map(|entry| RemCandidate {
            store: project_store.clone(),
            entry,
        }));
    }
    Ok(entries)
}

// ─── Clustering ────────────────────────────────────────────────────────────────

/// Lightweight topic clustering. Groups by:
///   1. Non-empty `topic` field (exact match).
///   2. First keyword that appears in ≥ 2 entries (frequency-based fallback).
///   3. Falls into `"_misc"` bucket (excluded from synthesis — too heterogeneous).
fn cluster_by_topic(entries: &[RemCandidate]) -> HashMap<String, Vec<&RemCandidate>> {
    let mut clusters: HashMap<String, Vec<&RemCandidate>> = HashMap::new();

    // Pass 1 — group by explicit topic
    let mut remaining: Vec<&RemCandidate> = Vec::new();
    for entry in entries {
        let topic = entry.entry.topic.trim().to_string();
        if !topic.is_empty() {
            clusters.entry(topic).or_default().push(entry);
        } else {
            remaining.push(entry);
        }
    }

    // Pass 2 — count keyword frequencies across remaining entries
    let mut kw_freq: HashMap<String, usize> = HashMap::new();
    for entry in &remaining {
        let seen_in_entry: std::collections::HashSet<&str> =
            entry.entry.keywords.iter().map(|k| k.as_str()).collect();
        for kw in seen_in_entry {
            *kw_freq.entry(kw.to_string()).or_insert(0) += 1;
        }
    }

    // Pass 3 — assign remaining entries to the highest-frequency keyword
    for entry in remaining {
        let best_kw = entry
            .entry
            .keywords
            .iter()
            .filter_map(|kw| kw_freq.get(kw.as_str()).map(|freq| (kw, *freq)))
            .filter(|(_, freq)| *freq >= 2)
            .max_by_key(|(_, freq)| *freq)
            .map(|(kw, _)| kw.clone());

        let cluster_key = best_kw.unwrap_or_else(|| "_misc".to_string());
        clusters.entry(cluster_key).or_default().push(entry);
    }

    // Remove the misc bucket — too heterogeneous for synthesis
    clusters.remove("_misc");

    clusters
}

// ─── Synthesis ────────────────────────────────────────────────────────────────

async fn synthesize_and_save(
    server: &MemoryServer,
    topic: &str,
    members: &[&RemCandidate],
) -> Result<(), String> {
    let draft = synthesize_wiki_draft(server, topic, members).await?;
    save_wiki_draft(server, topic, members, draft.value, &draft.invocation)
}

async fn synthesize_wiki_draft(
    server: &MemoryServer,
    topic: &str,
    members: &[&RemCandidate],
) -> Result<Generated<WikiDraft>, String> {
    let scrubbed = members
        .iter()
        .take(MAX_ENTRIES_PER_CLUSTER)
        .filter_map(|e| {
            let text = scrub_agent_noise(&e.entry.text);
            if text.trim().len() < MIN_TEXT_LEN {
                return None;
            }
            Some(json!({
                "summary": e.entry.summary,
                "text": text.chars().take(600).collect::<String>(),
                "importance": e.entry.importance,
                "keywords": e.entry.keywords,
            }))
        })
        .collect::<Vec<_>>();

    if scrubbed.is_empty() {
        return Err("all cluster entries too short after noise scrubbing".to_string());
    }

    let user_payload = serde_json::to_string_pretty(&json!({
        "cluster_topic": topic,
        "entry_count": scrubbed.len(),
        "entries": scrubbed,
    }))
    .unwrap_or_default();

    let raw = server
        .llm
        .call_distill_llm_with_receipt(WIKI_SYNTHESIS_SYSTEM, &user_payload, None, 0.2, 1200)
        .await
        .map_err(|e| format!("wiki synthesis LLM call: {e}"))?;
    if raw.invocation.completion_status() == CompletionStatusV1::Truncated {
        return Err("wiki synthesis LLM call: llm_output_truncated".to_string());
    }

    Ok(Generated {
        value: parse_wiki_draft(&raw.value, topic)?,
        invocation: raw.invocation,
    })
}

fn parse_wiki_draft(raw: &str, fallback_topic: &str) -> Result<WikiDraft, String> {
    let stripped = LlmClient::strip_code_fence(raw);
    let start = stripped.find('{').unwrap_or(0);
    let end = stripped.rfind('}').map(|i| i + 1).unwrap_or(stripped.len());
    let obj: Value = serde_json::from_str(&stripped[start..end]).map_err(|e| {
        format!(
            "parse wiki draft JSON: {e} — raw={}",
            raw.chars().take(300).collect::<String>()
        )
    })?;

    let title = obj
        .get("title")
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .unwrap_or(fallback_topic)
        .to_string();
    let body = obj
        .get("body")
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .ok_or("missing 'body' field in wiki draft")?
        .to_string();
    let summary = obj
        .get("summary")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let keywords: Vec<String> = obj
        .get("keywords")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    let entities: Vec<String> = obj
        .get("entities")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    let domain = obj
        .get("domain")
        .and_then(Value::as_str)
        .unwrap_or("wiki")
        .to_string();

    Ok(WikiDraft {
        title,
        body,
        summary,
        keywords,
        entities,
        domain,
    })
}

// ─── Persistence ─────────────────────────────────────────────────────────────

fn normalized_rem_sources(members: &[&RemCandidate]) -> Vec<RemSourceRef> {
    let mut sources = members
        .iter()
        .map(|candidate| RemSourceRef {
            store: candidate.store.clone(),
            id: candidate.entry.id.clone(),
        })
        .collect::<Vec<_>>();
    sources.sort();
    sources.dedup();
    sources
}

fn stable_rem_draft_id(sources: &[RemSourceRef]) -> String {
    let payload = serde_json::to_vec(&RemSourceSetIdentityV1 {
        contract: REM_WIKI_SOURCE_SET_CONTRACT,
        producer_version: REM_WIKI_PRODUCER_VERSION,
        sources,
    })
    .expect("serializing the REM source-set identity cannot fail");
    format!(
        "wiki-rem:{}",
        uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_OID, &payload)
    )
}

fn rem_identity_conflict(draft_id: &str, reason: &str) -> MemoryError {
    MemoryError::InvalidArg(format!(
        "rem_wiki_identity_conflict: source-set identity collision for {draft_id}: {reason}"
    ))
}

fn stored_rem_sources(entry: &MemoryEntry) -> Result<Vec<RemSourceRef>, MemoryError> {
    serde_json::from_value(
        entry
            .metadata
            .pointer("/rem/sources")
            .cloned()
            .ok_or_else(|| rem_identity_conflict(&entry.id, "sources are missing"))?,
    )
    .map_err(|_| rem_identity_conflict(&entry.id, "sources are malformed"))
}

fn validate_existing_rem_draft(
    existing: &MemoryEntry,
    draft_id: &str,
    sources: &[RemSourceRef],
) -> Result<(), MemoryError> {
    let rem_string = |key: &str| {
        existing
            .metadata
            .pointer(&format!("/rem/{key}"))
            .and_then(Value::as_str)
    };
    let require = |condition: bool, reason: &str| {
        condition
            .then_some(())
            .ok_or_else(|| rem_identity_conflict(draft_id, reason))
    };
    require(existing.id == draft_id, "row id is not canonical")?;
    require(existing.source == "wiki", "row source is not wiki")?;
    require(
        existing.path.starts_with("/wiki/drafts/"),
        "row is outside the draft namespace",
    )?;
    require(
        existing
            .metadata
            .get("artifact_kind")
            .and_then(Value::as_str)
            == Some("draft"),
        "artifact_kind is not draft",
    )?;
    require(
        rem_string("producer") == Some("weekly_wiki_evolver"),
        "producer is mismatched",
    )?;
    require(
        rem_string("source_set_contract") == Some(REM_WIKI_SOURCE_SET_CONTRACT),
        "source-set contract is mismatched",
    )?;
    require(
        rem_string("producer_version") == Some(REM_WIKI_PRODUCER_VERSION),
        "producer version is mismatched",
    )?;
    require(
        rem_string("operation_id") == Some(draft_id),
        "operation id is mismatched",
    )?;
    require(
        matches!(
            rem_string("operation_status"),
            Some("pending_sources" | "complete")
        ),
        "operation status is invalid",
    )?;
    require(
        existing
            .metadata
            .get("review_status")
            .and_then(Value::as_str)
            == Some("pending"),
        "review status is not pending",
    )?;
    require(
        crate::provenance::trusted_existing_model_invocation(&existing.metadata).is_some(),
        "typed model invocation receipt is missing",
    )?;
    require(
        stored_rem_sources(existing)? == sources,
        "stored source set is mismatched",
    )?;
    require(
        stable_rem_draft_id(sources) == draft_id,
        "canonical fields do not recompute to the occupied id",
    )?;
    if existing.archived {
        return Err(rem_identity_conflict(draft_id, "occupied row is archived"));
    }
    Ok(())
}

fn ensure_rem_draft_winner(
    server: &MemoryServer,
    draft_id: &str,
    sources: &[RemSourceRef],
) -> Result<(), String> {
    server.with_named_project_store("wiki", |store| {
        store
            .with_immutable_supersession_transaction(|operation| {
                let existing = operation
                    .get_memory(draft_id)?
                    .ok_or_else(|| rem_identity_conflict(draft_id, "operation row is missing"))?;
                validate_existing_rem_draft(&existing, draft_id, sources)?;
                if !operation.memory_is_active_unsuperseded(draft_id)? {
                    return Err(rem_identity_conflict(
                        draft_id,
                        "occupied row is archived or superseded",
                    ));
                }
                Ok(())
            })
            .map_err(|error| format!("validate REM draft operation: {error}"))
    })
}

fn persist_rem_draft_operation(
    server: &MemoryServer,
    entry: &MemoryEntry,
    sources: &[RemSourceRef],
) -> Result<InsertMemoryResult, String> {
    server.with_named_project_store("wiki", |store| {
        store
            .with_immutable_supersession_transaction(|operation| {
                let result = operation.insert_if_absent(entry)?;
                if result == InsertMemoryResult::Existing {
                    let existing = operation.get_memory(&entry.id)?.ok_or_else(|| {
                        rem_identity_conflict(&entry.id, "existing row could not be read")
                    })?;
                    validate_existing_rem_draft(&existing, &entry.id, sources)?;
                    if !operation.memory_is_active_unsuperseded(&entry.id)? {
                        return Err(rem_identity_conflict(
                            &entry.id,
                            "occupied row is archived or superseded",
                        ));
                    }
                }
                Ok(result)
            })
            .map_err(|error| format!("persist REM draft operation: {error}"))
    })
}

fn complete_rem_operation(
    server: &MemoryServer,
    draft_id: &str,
    sources: &[RemSourceRef],
) -> Result<(), String> {
    ensure_rem_draft_winner(server, draft_id, sources)?;
    let now = Utc::now().to_rfc3339();
    let global_store =
        rem_source_store_identity(RemSourceScope::Global, &server.global_db_path_buf());
    let global_ids = sources
        .iter()
        .filter(|source| source.store.scope == RemSourceScope::Global)
        .map(|source| {
            if source.store != global_store {
                return Err(format!(
                    "REM global source store identity mismatch for {draft_id}"
                ));
            }
            Ok(source.id.clone())
        })
        .collect::<Result<Vec<_>, _>>()?;
    let project_sources = sources
        .iter()
        .filter(|source| source.store.scope == RemSourceScope::Project)
        .collect::<Vec<_>>();
    let project_ids = if project_sources.is_empty() {
        Vec::new()
    } else {
        let project_path = server.project_db_path_buf().ok_or_else(|| {
            format!(
                "complete REM project source group: bound project store is unavailable for {draft_id}"
            )
        })?;
        let project_store = rem_source_store_identity(RemSourceScope::Project, &project_path);
        project_sources
            .into_iter()
            .map(|source| {
                if source.store != project_store {
                    return Err(format!(
                        "REM project source store identity mismatch for {draft_id}"
                    ));
                }
                Ok(source.id.clone())
            })
            .collect::<Result<Vec<_>, _>>()?
    };

    if !global_ids.is_empty() {
        server
            .with_global_store(|store| {
                store
                    .mark_rem_processed_for_draft(&global_ids, &now, Some(draft_id))
                    .map_err(|error| format!("mark global REM sources: {error}"))
            })
            .map_err(|error| format!("complete REM global source group: {error}"))?;
    }
    if !project_ids.is_empty() {
        if !server.has_project_db() {
            return Err(format!(
                "complete REM project source group: bound project store is unavailable for {draft_id}"
            ));
        }
        server
            .with_project_store(|store| {
                store
                    .mark_rem_processed_for_draft(&project_ids, &now, Some(draft_id))
                    .map_err(|error| format!("mark project REM sources: {error}"))
            })
            .map_err(|error| format!("complete REM project source group: {error}"))?;
    }
    server.with_named_project_store("wiki", |store| {
        store
            .complete_rem_wiki_operation(draft_id, &now)
            .map_err(|error| format!("complete REM draft receipt: {error}"))
    })
}

fn rem_sources_belong_to_runtime_stores(
    sources: &[RemSourceRef],
    global_store: &RemSourceStore,
    project_store: Option<&RemSourceStore>,
) -> bool {
    !sources.is_empty()
        && sources.iter().all(|source| match source.store.scope {
            RemSourceScope::Global => &source.store == global_store,
            RemSourceScope::Project => project_store == Some(&source.store),
        })
}

fn recover_pending_rem_operations(server: &MemoryServer) -> Result<(), String> {
    let wiki_path = MemoryServer::resolve_existing_named_project_db_path_in_home(
        "wiki",
        &server.tachi_home_dir(),
    )
    .map_err(|error| format!("resolve REM Wiki operation store: {error}"))?;
    if wiki_path.is_none() {
        return Ok(());
    }
    let global_store =
        rem_source_store_identity(RemSourceScope::Global, &server.global_db_path_buf());
    let project_store = server
        .project_db_path_buf()
        .map(|path| rem_source_store_identity(RemSourceScope::Project, &path));
    let mut after: Option<(String, String)> = None;
    const PAGE_SIZE: usize = 500;
    loop {
        let pending = server.with_named_project_store_read("wiki", |store| {
            store
                .pending_rem_wiki_operations_after(
                    after
                        .as_ref()
                        .map(|(timestamp, id)| (timestamp.as_str(), id.as_str())),
                    PAGE_SIZE,
                )
                .map_err(|error| format!("list pending REM draft operations: {error}"))
        })?;
        if pending.is_empty() {
            return Ok(());
        }
        let page_len = pending.len();
        let last = pending
            .last()
            .map(|entry| (entry.timestamp.clone(), entry.id.clone()))
            .expect("a non-empty REM operation page has a final row");
        for entry in pending {
            let sources = match stored_rem_sources(&entry) {
                Ok(sources) => sources,
                Err(error) => {
                    eprintln!(
                        "[wiki_evolver] warn: pending REM operation {} has no routable source set: {error}",
                        entry.id
                    );
                    continue;
                }
            };
            if !rem_sources_belong_to_runtime_stores(
                &sources,
                &global_store,
                project_store.as_ref(),
            ) {
                continue;
            }
            validate_existing_rem_draft(&entry, &entry.id, &sources)
                .map_err(|error| format!("recover REM draft {}: {error}", entry.id))?;
            complete_rem_operation(server, &entry.id, &sources)?;
        }
        if page_len < PAGE_SIZE {
            return Ok(());
        }
        after = Some(last);
    }
}

fn save_wiki_draft(
    server: &MemoryServer,
    topic: &str,
    members: &[&RemCandidate],
    draft: WikiDraft,
    invocation: &PersistedModelInvocationReceiptV1,
) -> Result<(), String> {
    server.prepare_named_project_store_for_write("wiki")?;
    let sources = normalized_rem_sources(members);
    let draft_id = stable_rem_draft_id(&sources);
    // Slugify: lowercase, spaces → hyphens, strip non-alphanumeric
    let slug = draft
        .title
        .to_ascii_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    let slug = if slug.is_empty() {
        topic
            .chars()
            .map(|c| if c.is_alphanumeric() { c } else { '-' })
            .collect::<String>()
    } else {
        slug
    };

    let short_id = draft_id
        .strip_prefix("wiki-rem:")
        .and_then(|value| value.get(..8))
        .unwrap_or("identity");
    let path = format!("/wiki/drafts/{slug}-{short_id}");

    // Build the text with a pending-review header so readers know the status.
    let full_text = format!(
        "<!-- review_status: pending | generated: {} -->\n\n{}",
        Utc::now().format("%Y-%m-%d"),
        draft.body,
    );
    let full_text = crate::memory_search_ops::scrub_generated_memory_text(&full_text);

    let mut kw = draft.keywords;
    kw.push("rem-draft".to_string());
    kw.push("pending-review".to_string());
    kw.sort();
    kw.dedup();
    let generated_at = Utc::now().to_rfc3339();
    let proposal = json!({});
    let mut metadata =
        crate::tool_params::build_candidate_knowledge_artifact_fields(&path, "global", &proposal);
    if let Some(object) = metadata.as_object_mut() {
        object.extend(
            json!({
                "wiki": true,
                "wiki_title": draft.title,
                "allow_cross_project": true,
                "layer": "wiki",
                "scope": "global",
                "status": "pending_review",
                "review_status": "pending",
                "rem": {
                    "producer": "weekly_wiki_evolver",
                    "source_set_contract": REM_WIKI_SOURCE_SET_CONTRACT,
                    "producer_version": REM_WIKI_PRODUCER_VERSION,
                    "operation_id": draft_id.clone(),
                    "operation_status": "pending_sources",
                    "sources": sources.clone(),
                    "generated_at": generated_at.clone(),
                }
            })
            .as_object()
            .cloned()
            .unwrap_or_default(),
        );
    }
    let metadata = crate::provenance::inject_provenance(
        server,
        metadata,
        "wiki_evolver",
        "weekly_wiki_evolver",
        Some("global"),
        crate::server_state::DbScope::Project,
        json!({"operation_id": draft_id.clone()}),
    );
    let named_wiki_path = server
        .resolve_server_named_project_db_path("wiki")
        .map_err(|error| format!("resolve REM Wiki provenance path: {error}"))?;
    let metadata =
        crate::provenance::correct_provenance_db_path_for_named_project(metadata, &named_wiki_path);
    let metadata = crate::provenance::attach_model_invocation(metadata, invocation)
        .map_err(|error| format!("attach REM model invocation: {error}"))?;
    let entry = MemoryEntry {
        id: draft_id.clone(),
        path,
        summary: draft.summary,
        text: full_text,
        importance: DRAFT_IMPORTANCE,
        timestamp: generated_at.clone(),
        valid_from: generated_at,
        valid_until: None,
        category: "experience".to_string(),
        topic: topic.to_string(),
        keywords: kw,
        persons: Vec::new(),
        entities: draft.entities,
        location: String::new(),
        source: "wiki".to_string(),
        scope: "global".to_string(),
        archived: false,
        access_count: 0,
        scored_count: 0,
        last_access: None,
        last_use_at: None,
        revision: 1,
        metadata,
        vector: None,
        retention_policy: Some("durable".to_string()),
        domain: Some(draft.domain),
        recall_count: 0,
        query_diversity: 0,
        tier: "raw".to_string(),
    };

    persist_rem_draft_operation(server, &entry, &sources)?;
    complete_rem_operation(server, &draft_id, &sources)
}

// ─── Internal types ───────────────────────────────────────────────────────────

struct WikiDraft {
    title: String,
    body: String,
    summary: String,
    keywords: Vec<String>,
    entities: Vec<String>,
    domain: String,
}

#[cfg(test)]
mod rem_identity_tests {
    use super::*;

    fn test_store(scope: RemSourceScope, identity: &str) -> RemSourceStore {
        RemSourceStore {
            scope,
            identity: identity.to_string(),
        }
    }

    fn occupied_entry(id: &str, sources: &[RemSourceRef]) -> MemoryEntry {
        MemoryEntry {
            id: id.to_string(),
            path: "/wiki/drafts/test-identity".to_string(),
            summary: "REM identity".to_string(),
            text: "REM identity fixture".to_string(),
            importance: 0.65,
            timestamp: "2026-07-31T00:00:00Z".to_string(),
            valid_from: "2026-07-31T00:00:00Z".to_string(),
            valid_until: None,
            category: "experience".to_string(),
            topic: "identity".to_string(),
            keywords: Vec::new(),
            persons: Vec::new(),
            entities: Vec::new(),
            location: String::new(),
            source: "wiki".to_string(),
            scope: "global".to_string(),
            archived: false,
            access_count: 0,
            scored_count: 0,
            last_access: None,
            last_use_at: None,
            revision: 1,
            metadata: json!({
                "artifact_kind": "draft",
                "review_status": "pending",
                "provenance": {
                    "model_invocation": {"schema": tachi_llm::MODEL_INVOCATION_SCHEMA_V1}
                },
                "rem": {
                    "producer": "weekly_wiki_evolver",
                    "source_set_contract": REM_WIKI_SOURCE_SET_CONTRACT,
                    "producer_version": REM_WIKI_PRODUCER_VERSION,
                    "operation_id": id,
                    "operation_status": "pending_sources",
                    "sources": sources,
                }
            }),
            vector: None,
            retention_policy: Some("durable".to_string()),
            domain: Some("wiki".to_string()),
            recall_count: 0,
            query_diversity: 0,
            tier: "raw".to_string(),
        }
    }

    #[test]
    fn rem_identity_is_sorted_and_store_aware() {
        let sources = vec![
            RemSourceRef {
                store: test_store(RemSourceScope::Project, "project-db"),
                id: "same".to_string(),
            },
            RemSourceRef {
                store: test_store(RemSourceScope::Global, "global-db"),
                id: "same".to_string(),
            },
        ];
        let mut reversed = sources.clone();
        reversed.reverse();
        reversed.sort();
        let mut sorted = sources.clone();
        sorted.sort();
        assert_eq!(stable_rem_draft_id(&sorted), stable_rem_draft_id(&reversed));
        assert_ne!(
            stable_rem_draft_id(&sorted),
            stable_rem_draft_id(&[RemSourceRef {
                store: test_store(RemSourceScope::Project, "project-db"),
                id: "same".to_string(),
            }])
        );
    }

    #[test]
    fn pending_recovery_accepts_only_the_current_runtime_source_stores() {
        let global = test_store(RemSourceScope::Global, "/home/global.db");
        let project_a = test_store(RemSourceScope::Project, "/home/projects/a.db");
        let project_b = test_store(RemSourceScope::Project, "/home/projects/b.db");
        let owned = vec![
            RemSourceRef {
                store: global.clone(),
                id: "global-source".to_string(),
            },
            RemSourceRef {
                store: project_a.clone(),
                id: "project-source".to_string(),
            },
        ];

        assert!(rem_sources_belong_to_runtime_stores(
            &owned,
            &global,
            Some(&project_a)
        ));
        assert!(!rem_sources_belong_to_runtime_stores(
            &owned,
            &global,
            Some(&project_b)
        ));
        assert!(!rem_sources_belong_to_runtime_stores(&owned, &global, None));
    }

    #[test]
    fn deterministic_id_occupant_must_match_exact_source_set() {
        let sources = vec![RemSourceRef {
            store: test_store(RemSourceScope::Project, "project-db"),
            id: "source-a".to_string(),
        }];
        let id = stable_rem_draft_id(&sources);
        let valid = occupied_entry(&id, &sources);
        validate_existing_rem_draft(&valid, &id, &sources).expect("canonical occupant");

        let collision_sources = vec![RemSourceRef {
            store: test_store(RemSourceScope::Project, "project-db"),
            id: "source-b".to_string(),
        }];
        let collision = occupied_entry(&id, &collision_sources);
        let error = validate_existing_rem_draft(&collision, &id, &sources)
            .expect_err("mismatched occupant must fail");
        assert!(error.to_string().contains("rem_wiki_identity_conflict"));

        let mut archived = valid;
        archived.archived = true;
        let error = validate_existing_rem_draft(&archived, &id, &sources)
            .expect_err("archived deterministic winner must fail");
        assert!(error.to_string().contains("occupied row is archived"));
    }
}
