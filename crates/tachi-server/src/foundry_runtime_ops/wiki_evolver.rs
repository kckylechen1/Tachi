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

use std::collections::{HashMap, HashSet};

use chrono::Utc;
use serde_json::{json, Value};

use super::scrub_agent_noise;
use crate::server_state::MemoryServer;
use memcore::types::MemoryEntry;
use memcore::{InsertMemoryResult, MemoryError};
use tachi_llm::{CompletionStatusV1, Generated, LlmClient, PersistedModelInvocationReceiptV1};

#[cfg(test)]
static FAIL_REM_WIKI_COMPLETION_ONCE: std::sync::Mutex<Option<String>> =
    std::sync::Mutex::new(None);

#[cfg(test)]
fn take_rem_wiki_completion_failure(draft_id: &str) -> bool {
    let mut target = FAIL_REM_WIKI_COMPLETION_ONCE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if target.as_deref() == Some(draft_id) {
        target.take();
        true
    } else {
        false
    }
}

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
const REM_WIKI_SOURCE_CLAIM_CONTRACT: &str = "rem-wiki-source-claim-v1";
const REM_WIKI_PRODUCER_VERSION: &str = "weekly-wiki-evolver-v1";

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, serde::Deserialize, serde::Serialize)]
struct RemSourceStore {
    identity: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RemSourceRole {
    Global,
    Project,
}

#[derive(Clone, Debug)]
struct RuntimeRemSourceStores {
    global: RemSourceStore,
    project: Option<RemSourceStore>,
    shared_route: RemSourceRole,
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
    revision: i64,
}

type RemSourceRevision = (String, i64);
type RoutedRemSources = (Vec<RemSourceRevision>, Vec<RemSourceRevision>);

#[derive(serde::Serialize)]
struct RemSourceSetIdentityV1<'a> {
    contract: &'static str,
    producer_version: &'a str,
    sources: &'a [RemSourceRef],
}

#[derive(serde::Serialize)]
struct RemSourceClaimIdentityV1<'a> {
    contract: &'static str,
    source: &'a RemSourceRef,
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
    let recovery = recover_pending_rem_operations(server)?;
    let candidates = collect_pattern_memories(server)?;
    if candidates.is_empty() {
        return Ok(WikiEvolverReport {
            clusters_found: 0,
            drafts_written: 0,
            skipped: 0,
            errors: 0,
            recovered_completed: recovery.completed,
            recovered_aborted: recovery.aborted_stale,
            foreign_pending_skipped: recovery.foreign_pending_skipped,
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
            Ok(InsertMemoryResult::Inserted) => drafts_written += 1,
            Ok(InsertMemoryResult::Existing) => skipped += 1,
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
        recovered_completed: recovery.completed,
        recovered_aborted: recovery.aborted_stale,
        foreign_pending_skipped: recovery.foreign_pending_skipped,
    };
    eprintln!(
        "[wiki_evolver] weekly run {}: clusters={} drafts={} skipped={} errors={} recovered_completed={} recovered_aborted={} foreign_pending_skipped={}",
        if report.errors == 0 { "completed" } else { "completed_with_errors" },
        report.clusters_found,
        report.drafts_written,
        report.skipped,
        report.errors,
        report.recovered_completed,
        report.recovered_aborted,
        report.foreign_pending_skipped,
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
    pub recovered_completed: usize,
    pub recovered_aborted: usize,
    pub foreign_pending_skipped: usize,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct RemRecoveryReport {
    completed: usize,
    aborted_stale: usize,
    foreign_pending_skipped: usize,
}

// ─── Candidate collection ─────────────────────────────────────────────────────

fn rem_source_store_identities(
    global_path: &std::path::Path,
    project_path: Option<&std::path::Path>,
) -> Result<RuntimeRemSourceStores, String> {
    let mut paths = vec![global_path.to_path_buf()];
    if let Some(project_path) = project_path {
        paths.push(project_path.to_path_buf());
    }
    let bindings = crate::physical_db_identity::physical_db_bindings_for_paths(&paths)?;
    let global = RemSourceStore {
        identity: bindings[0].physical_id.clone(),
    };
    let project = project_path.map(|_| RemSourceStore {
        identity: bindings[1].physical_id.clone(),
    });
    let shared_route = if project.as_ref() == Some(&global)
        && bindings[1].is_primary_alias
        && !bindings[0].is_primary_alias
    {
        RemSourceRole::Project
    } else {
        RemSourceRole::Global
    };
    Ok(RuntimeRemSourceStores {
        global,
        project,
        shared_route,
    })
}

fn runtime_rem_source_stores(server: &MemoryServer) -> Result<RuntimeRemSourceStores, String> {
    let global_path = server.global_db_path_buf();
    let project_path = server.project_db_path_buf();
    let runtime_stores = rem_source_store_identities(&global_path, project_path.as_deref())?;
    let store_identity = |store: &mut memcore::MemoryStore, role: &str| {
        store
            .opened_physical_db_identity()
            .map(str::to_string)
            .ok_or_else(|| format!("REM {role} store has no opened physical identity"))
    };
    let global_write_identity =
        server.with_global_store(|store| store_identity(store, "global"))?;
    let global_read_identity =
        server.with_global_store_read(|store| store_identity(store, "global read-pool"))?;
    if global_write_identity != runtime_stores.global.identity
        || global_read_identity != runtime_stores.global.identity
    {
        return Err(format!(
            "REM global store path identity changed after open: opened write={global_write_identity}, read={global_read_identity}, current={}",
            runtime_stores.global.identity
        ));
    }
    if let Some(project_store) = runtime_stores.project.as_ref() {
        let project_write_identity =
            server.with_project_store(|store| store_identity(store, "project"))?;
        let project_read_identity =
            server.with_project_store_read(|store| store_identity(store, "project read-pool"))?;
        if project_write_identity != project_store.identity
            || project_read_identity != project_store.identity
        {
            return Err(format!(
                "REM project store path identity changed after open: opened write={project_write_identity}, read={project_read_identity}, current={}",
                project_store.identity
            ));
        }
    }
    Ok(runtime_stores)
}

fn run_checked_rem_source_action<T>(
    store: &mut memcore::MemoryStore,
    path: &std::path::Path,
    expected: &RemSourceStore,
    role: &str,
    action: impl FnOnce(&mut memcore::MemoryStore) -> Result<T, String>,
) -> Result<T, String> {
    let verify = |store: &memcore::MemoryStore, phase: &str| {
        store
            .verify_opened_physical_db_identity(path)
            .map_err(|error| format!("REM {role} store identity check failed {phase}: {error}"))?;
        let opened = store
            .opened_physical_db_identity()
            .ok_or_else(|| format!("REM {role} store has no opened physical identity"))?;
        if opened != expected.identity {
            return Err(format!(
                "REM {role} store does not match the operation source identity {phase}"
            ));
        }
        Ok(())
    };
    verify(store, "before operation")?;
    let result = action(store);
    let post = verify(store, "after operation");
    match (result, post) {
        (Ok(value), Ok(())) => Ok(value),
        (Ok(_), Err(error)) => Err(error),
        (Err(error), Ok(())) => Err(error),
        (Err(error), Err(identity_error)) => Err(format!(
            "{error}; additionally, the REM physical identity invariant failed after the operation: {identity_error}"
        )),
    }
}

fn with_rem_global_store<T>(
    server: &MemoryServer,
    expected: &RemSourceStore,
    action: impl FnOnce(&mut memcore::MemoryStore) -> Result<T, String>,
) -> Result<T, String> {
    let path = server.global_db_path_buf();
    server.with_global_store(|store| {
        run_checked_rem_source_action(store, &path, expected, "global", action)
    })
}

fn with_rem_global_store_read<T>(
    server: &MemoryServer,
    expected: &RemSourceStore,
    action: impl FnOnce(&mut memcore::MemoryStore) -> Result<T, String>,
) -> Result<T, String> {
    let path = server.global_db_path_buf();
    server.with_global_store_read(|store| {
        run_checked_rem_source_action(store, &path, expected, "global read", action)
    })
}

fn with_rem_project_store<T>(
    server: &MemoryServer,
    expected: &RemSourceStore,
    action: impl FnOnce(&mut memcore::MemoryStore) -> Result<T, String>,
) -> Result<T, String> {
    let path = server
        .project_db_path_buf()
        .ok_or_else(|| "REM project store is unavailable".to_string())?;
    server.with_project_store(|store| {
        run_checked_rem_source_action(store, &path, expected, "project", action)
    })
}

fn with_rem_project_store_read<T>(
    server: &MemoryServer,
    expected: &RemSourceStore,
    action: impl FnOnce(&mut memcore::MemoryStore) -> Result<T, String>,
) -> Result<T, String> {
    let path = server
        .project_db_path_buf()
        .ok_or_else(|| "REM project store is unavailable".to_string())?;
    server.with_project_store_read(|store| {
        run_checked_rem_source_action(store, &path, expected, "project read", action)
    })
}

fn collect_pattern_memories(server: &MemoryServer) -> Result<Vec<RemCandidate>, String> {
    let collect = |store: &mut memcore::MemoryStore| -> Result<Vec<MemoryEntry>, String> {
        store
            .unprocessed_pattern_memories()
            .map_err(|e| format!("query pattern memories: {e}"))
    };

    let runtime_stores = runtime_rem_source_stores(server)
        .map_err(|error| format!("REM runtime source store identity: {error}"))?;
    let shared_physical_store = runtime_stores.project.as_ref() == Some(&runtime_stores.global);
    let collect_global =
        !shared_physical_store || runtime_stores.shared_route == RemSourceRole::Global;
    let mut entries = if collect_global {
        with_rem_global_store_read(server, &runtime_stores.global, collect)
            .map_err(|e| format!("REM global candidate collection: {e}"))?
            .into_iter()
            .map(|entry| RemCandidate {
                store: runtime_stores.global.clone(),
                entry,
            })
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    if server.has_project_db() {
        let project_store = runtime_stores.project.clone().ok_or_else(|| {
            "REM project candidate collection: project store identity is missing".to_string()
        })?;
        if !shared_physical_store || runtime_stores.shared_route == RemSourceRole::Project {
            let project = with_rem_project_store_read(server, &project_store, collect)
                .map_err(|e| format!("REM project candidate collection: {e}"))?;
            entries.extend(project.into_iter().map(|entry| RemCandidate {
                store: project_store.clone(),
                entry,
            }));
        }
    }
    deduplicate_rem_candidates(&mut entries);
    Ok(entries)
}

fn deduplicate_rem_candidates(entries: &mut Vec<RemCandidate>) {
    let mut seen = HashSet::new();
    entries.retain(|candidate| {
        seen.insert((candidate.store.identity.clone(), candidate.entry.id.clone()))
    });
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
) -> Result<InsertMemoryResult, String> {
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
            revision: candidate.entry.revision,
        })
        .collect::<Vec<_>>();
    sources.sort();
    sources.dedup();
    sources
}

fn stable_rem_draft_id(sources: &[RemSourceRef]) -> String {
    stable_rem_draft_id_for_version(sources, REM_WIKI_PRODUCER_VERSION)
}

fn stable_rem_draft_id_for_version(sources: &[RemSourceRef], producer_version: &str) -> String {
    let payload = serde_json::to_vec(&RemSourceSetIdentityV1 {
        contract: REM_WIKI_SOURCE_SET_CONTRACT,
        producer_version,
        sources,
    })
    .expect("serializing the REM source-set identity cannot fail");
    format!(
        "wiki-rem:{}",
        uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_OID, &payload)
    )
}

fn stable_rem_source_claim(source: &RemSourceRef) -> (String, String) {
    let identity = serde_json::to_string(&RemSourceClaimIdentityV1 {
        contract: REM_WIKI_SOURCE_CLAIM_CONTRACT,
        source,
    })
    .expect("serializing a REM source claim cannot fail");
    let key = format!(
        "rem-source:{}",
        uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_OID, identity.as_bytes())
    );
    (key, identity)
}

fn rem_identity_conflict(draft_id: &str, reason: &str) -> MemoryError {
    MemoryError::InvalidArg(format!(
        "rem_wiki_identity_conflict: source-set identity collision for {draft_id}: {reason}"
    ))
}

fn stored_rem_sources(entry: &MemoryEntry) -> Result<Vec<RemSourceRef>, MemoryError> {
    let sources: Vec<RemSourceRef> = serde_json::from_value(
        entry
            .metadata
            .pointer("/rem/sources")
            .cloned()
            .ok_or_else(|| rem_identity_conflict(&entry.id, "sources are missing"))?,
    )
    .map_err(|_| rem_identity_conflict(&entry.id, "sources are malformed"))?;
    if sources.is_empty() {
        return Err(rem_identity_conflict(&entry.id, "source set is empty"));
    }
    Ok(sources)
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
    let producer_version = rem_string("producer_version")
        .filter(|version| !version.trim().is_empty() && version.len() <= 128)
        .ok_or_else(|| rem_identity_conflict(draft_id, "producer version is invalid"))?;
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
        stable_rem_draft_id_for_version(sources, producer_version) == draft_id,
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
    let expected_claims = sources
        .iter()
        .map(stable_rem_source_claim)
        .collect::<Vec<_>>();
    server.with_named_project_store_identity_checked("wiki", |store| {
        store
            .with_immutable_supersession_transaction(|operation| {
                let existing = operation
                    .get_memory(draft_id)?
                    .ok_or_else(|| rem_identity_conflict(draft_id, "operation row is missing"))?;
                validate_existing_rem_draft(&existing, draft_id, sources)?;
                operation.validate_rem_source_claims_for_draft(draft_id, &expected_claims)?;
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
    server.with_named_project_store_identity_checked("wiki", |store| {
        store
            .with_immutable_supersession_transaction(|operation| {
                let claimed_at = Utc::now().to_rfc3339();
                for source in sources {
                    let (source_key, source_identity) = stable_rem_source_claim(source);
                    operation.claim_rem_source(
                        &source_key,
                        &source_identity,
                        &entry.id,
                        &claimed_at,
                    )?;
                }
                let result = operation.insert_rem_operation_if_absent(entry)?;
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
    let runtime_stores = runtime_rem_source_stores(server)?;
    let (global_sources, project_sources) = route_rem_sources(sources, &runtime_stores, draft_id)?;

    if !project_sources.is_empty() && !server.has_project_db() {
        return Err(format!(
            "complete REM project source group: bound project store is unavailable for {draft_id}"
        ));
    }

    if !global_sources.is_empty() {
        if let Err(error) = with_rem_global_store(server, &runtime_stores.global, |store| {
            store
                .mark_rem_processed_for_draft_at_revisions(&global_sources, &now, draft_id)
                .map_err(|error| format!("mark global REM sources: {error}"))
        })
        .map_err(|error| format!("complete REM global source group: {error}"))
        {
            return fail_after_rem_source_completion(
                server,
                &runtime_stores,
                draft_id,
                &global_sources,
                &project_sources,
                error,
            );
        }
    }
    if !project_sources.is_empty() {
        let project_store = runtime_stores.project.as_ref().ok_or_else(|| {
            format!("complete REM project source group: project identity missing for {draft_id}")
        })?;
        if let Err(error) = with_rem_project_store(server, project_store, |store| {
            store
                .mark_rem_processed_for_draft_at_revisions(&project_sources, &now, draft_id)
                .map_err(|error| format!("mark project REM sources: {error}"))
        })
        .map_err(|error| format!("complete REM project source group: {error}"))
        {
            return fail_after_rem_source_completion(
                server,
                &runtime_stores,
                draft_id,
                &global_sources,
                &project_sources,
                error,
            );
        }
    }
    #[cfg(test)]
    if take_rem_wiki_completion_failure(draft_id) {
        return fail_after_rem_source_completion(
            server,
            &runtime_stores,
            draft_id,
            &global_sources,
            &project_sources,
            "injected REM Wiki completion failure".to_string(),
        );
    }
    if let Err(error) = server.with_named_project_store_identity_checked("wiki", |store| {
        store
            .complete_rem_wiki_operation(draft_id, &now)
            .map_err(|error| format!("complete REM draft receipt: {error}"))
    }) {
        return fail_after_rem_source_completion(
            server,
            &runtime_stores,
            draft_id,
            &global_sources,
            &project_sources,
            error,
        );
    }
    Ok(())
}

fn fail_after_rem_source_completion(
    server: &MemoryServer,
    runtime_stores: &RuntimeRemSourceStores,
    draft_id: &str,
    global_sources: &[RemSourceRevision],
    project_sources: &[RemSourceRevision],
    primary_error: String,
) -> Result<(), String> {
    match compensate_rem_source_markers(
        server,
        runtime_stores,
        draft_id,
        global_sources,
        project_sources,
    ) {
        Ok(()) => Err(format!(
            "{primary_error}; REM source markers were compensated for pending operation {draft_id}"
        )),
        Err(error) => Err(format!(
            "{primary_error}; REM source marker compensation failed for pending operation {draft_id}: {error}"
        )),
    }
}

fn compensate_rem_source_markers(
    server: &MemoryServer,
    runtime_stores: &RuntimeRemSourceStores,
    draft_id: &str,
    global_sources: &[RemSourceRevision],
    project_sources: &[RemSourceRevision],
) -> Result<(), String> {
    let mut rollback_errors = Vec::new();
    if !project_sources.is_empty() {
        let rollback = runtime_stores
            .project
            .as_ref()
            .ok_or_else(|| "REM project identity is unavailable during compensation".to_string())
            .and_then(|project_store| {
                with_rem_project_store(server, project_store, |store| {
                    store
                        .rollback_rem_processed_for_draft_at_revisions(project_sources, draft_id)
                        .map_err(|error| format!("rollback project REM sources: {error}"))
                })
            })
            .map_err(|error| format!("compensate REM project source group: {error}"));
        if let Err(error) = rollback {
            rollback_errors.push(error);
        }
    }
    if !global_sources.is_empty() {
        if let Err(error) = with_rem_global_store(server, &runtime_stores.global, |store| {
            store
                .rollback_rem_processed_for_draft_at_revisions(global_sources, draft_id)
                .map_err(|error| format!("rollback global REM sources: {error}"))
        })
        .map_err(|error| format!("compensate REM global source group: {error}"))
        {
            rollback_errors.push(error);
        }
    }
    if rollback_errors.is_empty() {
        Ok(())
    } else {
        Err(rollback_errors.join("; "))
    }
}

fn rem_sources_belong_to_runtime_stores(
    sources: &[RemSourceRef],
    runtime_stores: &RuntimeRemSourceStores,
) -> bool {
    !sources.is_empty()
        && sources.iter().all(|source| {
            source.store == runtime_stores.global
                || runtime_stores.project.as_ref() == Some(&source.store)
        })
}

fn rem_sources_owned_by_runtime_count(
    sources: &[RemSourceRef],
    runtime_stores: &RuntimeRemSourceStores,
) -> usize {
    sources
        .iter()
        .filter(|source| {
            source.store == runtime_stores.global
                || runtime_stores.project.as_ref() == Some(&source.store)
        })
        .count()
}

fn route_rem_sources(
    sources: &[RemSourceRef],
    runtime_stores: &RuntimeRemSourceStores,
    draft_id: &str,
) -> Result<RoutedRemSources, String> {
    let mut global_sources = Vec::new();
    let mut project_sources = Vec::new();
    let shared_physical_store = runtime_stores.project.as_ref() == Some(&runtime_stores.global);
    for source in sources {
        let marker = (source.id.clone(), source.revision);
        if source.store == runtime_stores.global {
            if shared_physical_store && runtime_stores.shared_route == RemSourceRole::Project {
                project_sources.push(marker);
            } else {
                global_sources.push(marker);
            }
        } else if runtime_stores.project.as_ref() == Some(&source.store) {
            project_sources.push(marker);
        } else {
            return Err(format!(
                "REM source store identity mismatch for {draft_id}: {}",
                source.store.identity
            ));
        }
    }
    Ok((global_sources, project_sources))
}

fn rem_source_revisions_are_current(
    server: &MemoryServer,
    runtime_stores: &RuntimeRemSourceStores,
    global_sources: &[RemSourceRevision],
    project_sources: &[RemSourceRevision],
) -> Result<bool, String> {
    let group_is_current = |store: &mut memcore::MemoryStore,
                            sources: &[RemSourceRevision],
                            role: &str|
     -> Result<bool, String> {
        for (id, expected_revision) in sources {
            let Some(source) = store
                .get(id)
                .map_err(|error| format!("read {role} REM source {id}: {error}"))?
            else {
                return Ok(false);
            };
            if source.archived || source.revision != *expected_revision {
                return Ok(false);
            }
        }
        Ok(true)
    };
    if !global_sources.is_empty()
        && !with_rem_global_store_read(server, &runtime_stores.global, |store| {
            group_is_current(store, global_sources, "global")
        })?
    {
        return Ok(false);
    }
    if !project_sources.is_empty() {
        let project_store = runtime_stores
            .project
            .as_ref()
            .ok_or_else(|| "REM project source identity is unavailable".to_string())?;
        if !with_rem_project_store_read(server, project_store, |store| {
            group_is_current(store, project_sources, "project")
        })? {
            return Ok(false);
        }
    }
    Ok(true)
}

fn recover_pending_rem_operations(server: &MemoryServer) -> Result<RemRecoveryReport, String> {
    let wiki_path = MemoryServer::resolve_existing_named_project_db_path_in_home(
        "wiki",
        &server.tachi_home_dir(),
    )
    .map_err(|error| format!("resolve REM Wiki operation store: {error}"))?;
    if wiki_path.is_none() {
        return Ok(RemRecoveryReport::default());
    }
    let runtime_stores = runtime_rem_source_stores(server)?;
    let mut report = RemRecoveryReport::default();
    let mut after: Option<(String, String)> = None;
    const PAGE_SIZE: usize = 500;
    loop {
        let pending = server.with_named_project_store_read_identity_checked("wiki", |store| {
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
            return Ok(report);
        }
        let page_len = pending.len();
        let last = pending
            .last()
            .map(|entry| (entry.timestamp.clone(), entry.id.clone()))
            .expect("a non-empty REM operation page has a final row");
        for entry in pending {
            let sources = stored_rem_sources(&entry)
                .map_err(|error| format!("recover REM draft {} source set: {error}", entry.id))?;
            if !rem_sources_belong_to_runtime_stores(&sources, &runtime_stores) {
                let owned_sources = rem_sources_owned_by_runtime_count(&sources, &runtime_stores);
                if owned_sources == 0 {
                    report.foreign_pending_skipped += 1;
                    continue;
                }
                return Err(format!(
                    "recover REM draft {}: source set mixes current and foreign physical stores",
                    entry.id
                ));
            }
            validate_existing_rem_draft(&entry, &entry.id, &sources)
                .map_err(|error| format!("recover REM draft {}: {error}", entry.id))?;
            let (global_sources, project_sources) =
                route_rem_sources(&sources, &runtime_stores, &entry.id)?;
            if !rem_source_revisions_are_current(
                server,
                &runtime_stores,
                &global_sources,
                &project_sources,
            )? {
                compensate_rem_source_markers(
                    server,
                    &runtime_stores,
                    &entry.id,
                    &global_sources,
                    &project_sources,
                )
                .map_err(|error| {
                    format!(
                        "recover REM draft {}: stale source marker compensation failed; keeping operation and source claims pending: {error}",
                        entry.id
                    )
                })?;
                let aborted_at = Utc::now().to_rfc3339();
                server.with_named_project_store_identity_checked("wiki", |store| {
                    store
                        .abort_stale_rem_wiki_operation(&entry.id, &aborted_at)
                        .map_err(|error| {
                            format!("abort stale REM draft operation {}: {error}", entry.id)
                        })
                })?;
                eprintln!(
                    "[wiki_evolver] warn: archived stale pending REM operation {} and released its source claims",
                    entry.id
                );
                report.aborted_stale += 1;
                continue;
            }
            complete_rem_operation(server, &entry.id, &sources)?;
            report.completed += 1;
        }
        if page_len < PAGE_SIZE {
            return Ok(report);
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
) -> Result<InsertMemoryResult, String> {
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

    let result = persist_rem_draft_operation(server, &entry, &sources)?;
    complete_rem_operation(server, &draft_id, &sources)?;
    Ok(result)
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

    fn test_store(identity: &str) -> RemSourceStore {
        RemSourceStore {
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

    fn source_entry(id: &str) -> MemoryEntry {
        let mut entry = occupied_entry(id, &[]);
        entry.path = format!("/patterns/{id}");
        entry.source = "test".to_string();
        entry.topic = id.to_string();
        entry.metadata = json!({});
        entry.retention_policy = None;
        entry.domain = None;
        entry.tier = "pattern".to_string();
        entry
    }

    #[test]
    fn rem_identity_is_sorted_and_store_aware() {
        let sources = vec![
            RemSourceRef {
                store: test_store("project-db"),
                id: "same".to_string(),
                revision: 1,
            },
            RemSourceRef {
                store: test_store("global-db"),
                id: "same".to_string(),
                revision: 1,
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
                store: test_store("project-db"),
                id: "same".to_string(),
                revision: 1,
            }])
        );
        let mut newer_revision = sorted.clone();
        newer_revision[0].revision += 1;
        assert_ne!(
            stable_rem_draft_id(&sorted),
            stable_rem_draft_id(&newer_revision),
            "a changed source revision must produce a different draft operation"
        );
    }

    #[cfg(unix)]
    #[test]
    fn rem_source_store_identity_collapses_hardlink_aliases() {
        let dir = tempfile::tempdir().unwrap();
        let first = dir.path().join("project-a.db");
        let second = dir.path().join("project-b.db");
        std::fs::write(&first, b"REM physical identity fixture").unwrap();
        std::fs::hard_link(&first, &second).unwrap();

        let runtime_stores = rem_source_store_identities(&first, Some(&second)).unwrap();
        let first_store = runtime_stores.global;
        let second_store = runtime_stores.project.expect("project alias identity");
        assert_eq!(first_store, second_store);

        let first_id = stable_rem_draft_id(&[RemSourceRef {
            store: first_store,
            id: "same-source".to_string(),
            revision: 1,
        }]);
        let second_id = stable_rem_draft_id(&[RemSourceRef {
            store: second_store,
            id: "same-source".to_string(),
            revision: 1,
        }]);
        assert_eq!(first_id, second_id);
    }

    #[cfg(unix)]
    #[test]
    fn rem_source_store_identity_rejects_distinct_live_wal_aliases() {
        let dir = tempfile::tempdir().unwrap();
        let first = dir.path().join("global.db");
        let second = dir.path().join("project.db");
        std::fs::write(&first, b"REM physical identity fixture").unwrap();
        std::fs::hard_link(&first, &second).unwrap();
        std::fs::write(format!("{}-wal", first.display()), b"global WAL").unwrap();
        std::fs::write(format!("{}-wal", second.display()), b"project WAL").unwrap();

        let error = rem_source_store_identities(&first, Some(&second))
            .expect_err("dual live WAL aliases must fail closed");
        assert!(error.contains("multiple live WAL/SHM owners"), "{error}");
    }

    #[cfg(unix)]
    #[test]
    fn rem_source_store_identity_routes_shared_store_through_live_project_alias() {
        let dir = tempfile::tempdir().unwrap();
        let global = dir.path().join("global.db");
        let project = dir.path().join("project.db");
        std::fs::write(&global, b"REM physical identity fixture").unwrap();
        std::fs::hard_link(&global, &project).unwrap();
        std::fs::write(format!("{}-wal", project.display()), b"project WAL").unwrap();

        let runtime_stores = rem_source_store_identities(&global, Some(&project)).unwrap();
        assert_eq!(
            runtime_stores.project.as_ref(),
            Some(&runtime_stores.global)
        );
        assert_eq!(runtime_stores.shared_route, RemSourceRole::Project);

        let sources = vec![RemSourceRef {
            store: runtime_stores.global.clone(),
            id: "source".to_string(),
            revision: 7,
        }];
        let (global_sources, project_sources) =
            route_rem_sources(&sources, &runtime_stores, "draft").unwrap();
        assert!(global_sources.is_empty());
        assert_eq!(project_sources, vec![("source".to_string(), 7)]);
    }

    #[cfg(unix)]
    #[test]
    fn runtime_rem_source_stores_rejects_a_path_replaced_after_open() {
        let dir = tempfile::tempdir().unwrap();
        let global = dir.path().join("global.db");
        let replacement = dir.path().join("replacement.db");
        let server = MemoryServer::new(global.clone(), None).expect("open runtime server");
        std::fs::write(&replacement, b"replacement physical database identity")
            .expect("create replacement inode");
        std::fs::rename(&replacement, &global).expect("replace configured database path");

        let error = runtime_rem_source_stores(&server)
            .expect_err("a long-lived connection must not be rebound by path alone");
        assert!(error.contains("identity changed after open"), "{error}");
    }

    #[cfg(unix)]
    #[test]
    fn rem_persistence_rejects_a_named_wiki_path_replaced_after_open() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let wiki_db = home.join("projects/wiki").join(memcore::MEMORY_DB_FILENAME);
        std::fs::create_dir_all(wiki_db.parent().unwrap()).expect("create Wiki store directory");
        drop(
            memcore::MemoryStore::open(wiki_db.to_str().unwrap())
                .expect("initialize named Wiki store"),
        );
        let server = MemoryServer::new_with_home_for_test(home.join("global.db"), None, home)
            .expect("open runtime server");
        server
            .with_named_project_store("wiki", |_| Ok(()))
            .expect("cache named Wiki store");

        let replacement = dir.path().join("replacement.db");
        std::fs::write(&replacement, b"replacement Wiki database identity")
            .expect("create replacement inode");
        std::fs::rename(&replacement, &wiki_db).expect("replace named Wiki database path");
        let sources = vec![RemSourceRef {
            store: test_store("source-store"),
            id: "source".to_string(),
            revision: 1,
        }];
        let draft_id = stable_rem_draft_id(&sources);
        let error =
            persist_rem_draft_operation(&server, &occupied_entry(&draft_id, &sources), &sources)
                .expect_err("REM must not write claims into a detached Wiki connection");
        assert!(error.contains("physical identity check failed"), "{error}");
    }

    #[test]
    fn recovery_fails_loudly_for_a_pending_operation_without_sources() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let wiki_db = home.join("projects/wiki").join(memcore::MEMORY_DB_FILENAME);
        std::fs::create_dir_all(wiki_db.parent().unwrap()).expect("create Wiki store directory");
        drop(
            memcore::MemoryStore::open(wiki_db.to_str().unwrap())
                .expect("initialize named Wiki store"),
        );
        let server = MemoryServer::new_with_home_for_test(home.join("global.db"), None, home)
            .expect("open runtime server");
        let draft_id = stable_rem_draft_id(&[]);
        persist_rem_draft_operation(&server, &occupied_entry(&draft_id, &[]), &[])
            .expect("seed malformed legacy pending operation");

        let error = recover_pending_rem_operations(&server)
            .expect_err("missing REM sources must not look recovered");
        assert!(error.contains(&draft_id), "{error}");
        assert!(error.contains("source set"), "{error}");
    }

    #[test]
    fn wiki_completion_failure_compensates_source_markers_and_keeps_pending_operation() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let wiki_db = home.join("projects/wiki").join(memcore::MEMORY_DB_FILENAME);
        std::fs::create_dir_all(wiki_db.parent().unwrap()).expect("create Wiki store directory");
        drop(
            memcore::MemoryStore::open(wiki_db.to_str().unwrap())
                .expect("initialize named Wiki store"),
        );
        let server = MemoryServer::new_with_home_for_test(home.join("global.db"), None, home)
            .expect("open runtime server");
        server
            .with_global_store(|store| {
                store
                    .upsert(&source_entry("global-source"))
                    .map_err(|error| error.to_string())
            })
            .expect("seed global source");
        let runtime = runtime_rem_source_stores(&server).expect("bind runtime source stores");
        let revision = server
            .with_global_store_read(|store| {
                store
                    .get("global-source")
                    .map_err(|error| error.to_string())?
                    .map(|entry| entry.revision)
                    .ok_or_else(|| "global source missing".to_string())
            })
            .expect("read global source revision");
        let sources = vec![RemSourceRef {
            store: runtime.global,
            id: "global-source".to_string(),
            revision,
        }];
        let draft_id = stable_rem_draft_id(&sources);
        persist_rem_draft_operation(&server, &occupied_entry(&draft_id, &sources), &sources)
            .expect("persist pending REM operation");

        *FAIL_REM_WIKI_COMPLETION_ONCE
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(draft_id.clone());
        let error = complete_rem_operation(&server, &draft_id, &sources)
            .expect_err("Wiki completion failure must compensate source markers");
        assert!(
            error.contains("injected REM Wiki completion failure"),
            "{error}"
        );
        assert!(error.contains("markers were compensated"), "{error}");

        let source = server
            .with_global_store_read(|store| {
                store
                    .get("global-source")
                    .map_err(|error| error.to_string())?
                    .ok_or_else(|| "global source missing".to_string())
            })
            .expect("read compensated source");
        assert!(source.metadata["rem"]["processed"].is_null());
        let pending = server
            .with_named_project_store_read("wiki", |store| {
                store
                    .get(&draft_id)
                    .map_err(|error| error.to_string())?
                    .ok_or_else(|| "REM operation missing".to_string())
            })
            .expect("read pending operation");
        assert_eq!(
            pending.metadata["rem"]["operation_status"],
            "pending_sources"
        );

        let recovery = recover_pending_rem_operations(&server)
            .expect("recovery must finish the compensated pending operation");
        assert_eq!(recovery.completed, 1);
        assert_eq!(recovery.aborted_stale, 0);
        assert_eq!(recovery.foreign_pending_skipped, 0);
    }

    #[test]
    fn rem_completion_refuses_a_missing_source_claim_before_marking_sources() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let wiki_db = home.join("projects/wiki").join(memcore::MEMORY_DB_FILENAME);
        std::fs::create_dir_all(wiki_db.parent().unwrap()).expect("create Wiki store directory");
        drop(memcore::MemoryStore::open(wiki_db.to_str().unwrap()).expect("initialize Wiki store"));
        let server = MemoryServer::new_with_home_for_test(home.join("global.db"), None, home)
            .expect("open runtime server");
        server
            .with_global_store(|store| {
                store
                    .upsert(&source_entry("claimed-source"))
                    .map_err(|error| error.to_string())
            })
            .expect("seed source");
        let runtime = runtime_rem_source_stores(&server).expect("bind runtime stores");
        let revision = server
            .with_global_store_read(|store| {
                store
                    .get("claimed-source")
                    .map_err(|error| error.to_string())?
                    .map(|entry| entry.revision)
                    .ok_or_else(|| "source missing".to_string())
            })
            .expect("read source revision");
        let sources = vec![RemSourceRef {
            store: runtime.global,
            id: "claimed-source".to_string(),
            revision,
        }];
        let draft_id = stable_rem_draft_id(&sources);
        persist_rem_draft_operation(&server, &occupied_entry(&draft_id, &sources), &sources)
            .expect("persist REM operation");
        server
            .with_named_project_store("wiki", |store| {
                store
                    .connection()
                    .execute(
                        "DELETE FROM rem_source_claims WHERE draft_id = ?1",
                        [&draft_id],
                    )
                    .map_err(|error| error.to_string())?;
                Ok(())
            })
            .expect("simulate incomplete legacy claim ledger");

        let error = complete_rem_operation(&server, &draft_id, &sources)
            .expect_err("missing claim must fail before source marker writes");
        assert!(error.contains("source claim ledger mismatch"), "{error}");
        let source = server
            .with_global_store_read(|store| {
                store
                    .get("claimed-source")
                    .map_err(|error| error.to_string())?
                    .ok_or_else(|| "source missing".to_string())
            })
            .expect("read unmarked source");
        assert!(source.metadata["rem"]["processed"].is_null());
    }

    #[test]
    fn project_marker_failure_compensates_the_committed_global_marker() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let wiki_db = home.join("projects/wiki").join(memcore::MEMORY_DB_FILENAME);
        std::fs::create_dir_all(wiki_db.parent().unwrap()).expect("create Wiki store directory");
        drop(
            memcore::MemoryStore::open(wiki_db.to_str().unwrap())
                .expect("initialize named Wiki store"),
        );
        let global_db = home.join("global.db");
        let project_db = home.join("project.db");
        let server =
            MemoryServer::new_with_home_for_test(global_db, Some(project_db), home.clone())
                .expect("open runtime server");
        server
            .with_global_store(|store| {
                store
                    .upsert(&source_entry("global-source"))
                    .map_err(|error| error.to_string())
            })
            .expect("seed global source");
        server
            .with_project_store(|store| {
                store
                    .upsert(&source_entry("project-source"))
                    .map_err(|error| error.to_string())
            })
            .expect("seed project source");

        let runtime = runtime_rem_source_stores(&server).expect("bind runtime source stores");
        let global_revision = server
            .with_global_store_read(|store| {
                store
                    .get("global-source")
                    .map_err(|error| error.to_string())?
                    .map(|entry| entry.revision)
                    .ok_or_else(|| "global source missing".to_string())
            })
            .expect("read global revision");
        let project_revision = server
            .with_project_store_read(|store| {
                store
                    .get("project-source")
                    .map_err(|error| error.to_string())?
                    .map(|entry| entry.revision)
                    .ok_or_else(|| "project source missing".to_string())
            })
            .expect("read project revision");
        let mut sources = vec![
            RemSourceRef {
                store: runtime.global,
                id: "global-source".to_string(),
                revision: global_revision,
            },
            RemSourceRef {
                store: runtime.project.expect("project store identity"),
                id: "project-source".to_string(),
                revision: project_revision,
            },
        ];
        sources.sort();
        let draft_id = stable_rem_draft_id(&sources);
        let draft = occupied_entry(&draft_id, &sources);
        persist_rem_draft_operation(&server, &draft, &sources).expect("persist REM operation");

        server
            .with_project_store(|store| {
                let mut changed = store
                    .get("project-source")
                    .map_err(|error| error.to_string())?
                    .ok_or_else(|| "project source missing".to_string())?;
                changed.text = "project source changed after REM synthesis".to_string();
                store.upsert(&changed).map_err(|error| error.to_string())
            })
            .expect("change project source revision");

        let error = complete_rem_operation(&server, &draft_id, &sources)
            .expect_err("project revision failure must keep the operation pending");
        assert!(error.contains("source revision changed"), "{error}");
        assert!(error.contains("markers were compensated"), "{error}");
        let global = server
            .with_global_store_read(|store| {
                store
                    .get("global-source")
                    .map_err(|error| error.to_string())?
                    .ok_or_else(|| "global source missing".to_string())
            })
            .expect("read compensated global source");
        assert!(global.metadata["rem"]["processed"].is_null());
        server
            .with_named_project_store("wiki", |store| {
                let pending = store
                    .get(&draft_id)
                    .map_err(|error| error.to_string())?
                    .ok_or_else(|| "REM operation missing".to_string())?;
                assert_eq!(
                    pending.metadata["rem"]["operation_status"],
                    "pending_sources"
                );
                Ok(())
            })
            .expect("verify pending operation");

        let recovery = recover_pending_rem_operations(&server)
            .expect("second-run recovery must retire the stale operation");
        assert_eq!(recovery.completed, 0);
        assert_eq!(recovery.aborted_stale, 1);
        server
            .with_named_project_store("wiki", |store| {
                let aborted = store
                    .get_with_options(&draft_id, true)
                    .map_err(|error| error.to_string())?
                    .ok_or_else(|| "aborted REM operation missing".to_string())?;
                assert!(aborted.archived);
                assert_eq!(
                    aborted.metadata["rem"]["operation_status"],
                    "aborted_stale_sources"
                );
                Ok(())
            })
            .expect("verify stale operation retirement");

        let next_project_revision = server
            .with_project_store_read(|store| {
                store
                    .get("project-source")
                    .map_err(|error| error.to_string())?
                    .map(|entry| entry.revision)
                    .ok_or_else(|| "project source missing".to_string())
            })
            .expect("read revised project source");
        let mut next_sources = vec![
            sources
                .iter()
                .find(|source| source.id == "global-source")
                .expect("global source identity")
                .clone(),
            RemSourceRef {
                store: sources
                    .iter()
                    .find(|source| source.id == "project-source")
                    .expect("project source identity")
                    .store
                    .clone(),
                id: "project-source".to_string(),
                revision: next_project_revision,
            },
        ];
        next_sources.sort();
        let next_draft_id = stable_rem_draft_id(&next_sources);
        assert_ne!(next_draft_id, draft_id);
        persist_rem_draft_operation(
            &server,
            &occupied_entry(&next_draft_id, &next_sources),
            &next_sources,
        )
        .expect("released claims must admit the revised source-set operation");
    }

    #[test]
    fn stale_recovery_compensates_crash_left_partial_marker_before_releasing_claims() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let wiki_db = home.join("projects/wiki").join(memcore::MEMORY_DB_FILENAME);
        std::fs::create_dir_all(wiki_db.parent().unwrap()).expect("create Wiki store directory");
        drop(memcore::MemoryStore::open(wiki_db.to_str().unwrap()).expect("initialize Wiki store"));
        let global_db = home.join("global.db");
        let project_db = home.join("project.db");
        let server =
            MemoryServer::new_with_home_for_test(global_db, Some(project_db), home.clone())
                .expect("open runtime server");
        server
            .with_global_store(|store| {
                store
                    .upsert(&source_entry("global-source"))
                    .map_err(|error| error.to_string())
            })
            .expect("seed global source");
        server
            .with_project_store(|store| {
                store
                    .upsert(&source_entry("project-source"))
                    .map_err(|error| error.to_string())
            })
            .expect("seed project source");

        let runtime = runtime_rem_source_stores(&server).expect("bind runtime source stores");
        let global_revision = server
            .with_global_store_read(|store| {
                store
                    .get("global-source")
                    .map_err(|error| error.to_string())?
                    .map(|entry| entry.revision)
                    .ok_or_else(|| "global source missing".to_string())
            })
            .expect("read global revision");
        let project_revision = server
            .with_project_store_read(|store| {
                store
                    .get("project-source")
                    .map_err(|error| error.to_string())?
                    .map(|entry| entry.revision)
                    .ok_or_else(|| "project source missing".to_string())
            })
            .expect("read project revision");
        let mut sources = vec![
            RemSourceRef {
                store: runtime.global,
                id: "global-source".to_string(),
                revision: global_revision,
            },
            RemSourceRef {
                store: runtime.project.expect("project store identity"),
                id: "project-source".to_string(),
                revision: project_revision,
            },
        ];
        sources.sort();
        let draft_id = stable_rem_draft_id(&sources);
        persist_rem_draft_operation(&server, &occupied_entry(&draft_id, &sources), &sources)
            .expect("persist REM operation");
        server
            .with_global_store(|store| {
                store
                    .mark_rem_processed_for_draft_at_revisions(
                        &[("global-source".to_string(), global_revision)],
                        "2026-07-31T00:00:01Z",
                        &draft_id,
                    )
                    .map_err(|error| error.to_string())
            })
            .expect("simulate crash after global marker commit");
        server
            .with_project_store(|store| {
                let mut changed = store
                    .get("project-source")
                    .map_err(|error| error.to_string())?
                    .ok_or_else(|| "project source missing".to_string())?;
                changed.text = "project source changed after crash".to_string();
                store.upsert(&changed).map_err(|error| error.to_string())
            })
            .expect("change unmarked project source revision");

        let recovery = recover_pending_rem_operations(&server)
            .expect("stale recovery must compensate markers before aborting");
        assert_eq!(recovery.completed, 0);
        assert_eq!(recovery.aborted_stale, 1);
        assert_eq!(recovery.foreign_pending_skipped, 0);
        let global = server
            .with_global_store_read(|store| {
                store
                    .get("global-source")
                    .map_err(|error| error.to_string())?
                    .ok_or_else(|| "global source missing".to_string())
            })
            .expect("read compensated global source");
        assert!(global.metadata["rem"]["processed"].is_null());
        server
            .with_named_project_store("wiki", |store| {
                let aborted = store
                    .get_with_options(&draft_id, true)
                    .map_err(|error| error.to_string())?
                    .ok_or_else(|| "aborted REM operation missing".to_string())?;
                assert!(aborted.archived);
                assert_eq!(
                    aborted.metadata["rem"]["operation_status"],
                    "aborted_stale_sources"
                );
                Ok(())
            })
            .expect("verify stale operation retirement");

        let next_project_revision = server
            .with_project_store_read(|store| {
                store
                    .get("project-source")
                    .map_err(|error| error.to_string())?
                    .map(|entry| entry.revision)
                    .ok_or_else(|| "project source missing".to_string())
            })
            .expect("read revised project source");
        let mut next_sources = vec![
            sources
                .iter()
                .find(|source| source.id == "global-source")
                .expect("global source identity")
                .clone(),
            RemSourceRef {
                store: sources
                    .iter()
                    .find(|source| source.id == "project-source")
                    .expect("project source identity")
                    .store
                    .clone(),
                id: "project-source".to_string(),
                revision: next_project_revision,
            },
        ];
        next_sources.sort();
        let next_draft_id = stable_rem_draft_id(&next_sources);
        assert_ne!(next_draft_id, draft_id);
        persist_rem_draft_operation(
            &server,
            &occupied_entry(&next_draft_id, &next_sources),
            &next_sources,
        )
        .expect("compensated marker and released claims must admit revised source-set operation");
    }

    #[test]
    fn stale_recovery_keeps_operation_pending_when_marker_compensation_fails() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let wiki_db = home.join("projects/wiki").join(memcore::MEMORY_DB_FILENAME);
        std::fs::create_dir_all(wiki_db.parent().unwrap()).expect("create Wiki store directory");
        drop(memcore::MemoryStore::open(wiki_db.to_str().unwrap()).expect("initialize Wiki store"));
        let global_db = home.join("global.db");
        let project_db = home.join("project.db");
        let server =
            MemoryServer::new_with_home_for_test(global_db, Some(project_db), home.clone())
                .expect("open runtime server");
        server
            .with_global_store(|store| {
                store
                    .upsert(&source_entry("global-source"))
                    .map_err(|error| error.to_string())
            })
            .expect("seed global source");
        server
            .with_project_store(|store| {
                store
                    .upsert(&source_entry("project-source"))
                    .map_err(|error| error.to_string())
            })
            .expect("seed project source");

        let runtime = runtime_rem_source_stores(&server).expect("bind runtime source stores");
        let global_revision = server
            .with_global_store_read(|store| {
                store
                    .get("global-source")
                    .map_err(|error| error.to_string())?
                    .map(|entry| entry.revision)
                    .ok_or_else(|| "global source missing".to_string())
            })
            .expect("read global revision");
        let project_revision = server
            .with_project_store_read(|store| {
                store
                    .get("project-source")
                    .map_err(|error| error.to_string())?
                    .map(|entry| entry.revision)
                    .ok_or_else(|| "project source missing".to_string())
            })
            .expect("read project revision");
        let mut sources = vec![
            RemSourceRef {
                store: runtime.global,
                id: "global-source".to_string(),
                revision: global_revision,
            },
            RemSourceRef {
                store: runtime.project.expect("project store identity"),
                id: "project-source".to_string(),
                revision: project_revision,
            },
        ];
        sources.sort();
        let draft_id = stable_rem_draft_id(&sources);
        persist_rem_draft_operation(&server, &occupied_entry(&draft_id, &sources), &sources)
            .expect("persist REM operation");
        server
            .with_global_store(|store| {
                store
                    .mark_rem_processed_for_draft_at_revisions(
                        &[("global-source".to_string(), global_revision)],
                        "2026-07-31T00:00:01Z",
                        &draft_id,
                    )
                    .map_err(|error| error.to_string())?;
                store
                    .delete("global-source")
                    .map_err(|error| error.to_string())?;
                Ok(())
            })
            .expect("simulate uncompensatable disappeared source");
        server
            .with_project_store(|store| {
                let mut changed = store
                    .get("project-source")
                    .map_err(|error| error.to_string())?
                    .ok_or_else(|| "project source missing".to_string())?;
                changed.text = "project source changed after crash".to_string();
                store.upsert(&changed).map_err(|error| error.to_string())
            })
            .expect("change project source revision");

        let error = recover_pending_rem_operations(&server)
            .expect_err("compensation failure must fail closed before stale abort");
        assert!(
            error.contains("stale source marker compensation failed"),
            "{error}"
        );
        assert!(error.contains("keeping operation and source claims pending"));
        assert!(error.contains("disappeared"), "{error}");
        let global = server
            .with_global_store_read(|store| {
                store
                    .get("global-source")
                    .map_err(|error| error.to_string())?
                    .map(|entry| entry.id)
                    .ok_or_else(|| "global source missing".to_string())
            })
            .expect_err("global source must still be missing");
        assert_eq!(global, "global source missing");
        server
            .with_named_project_store("wiki", |store| {
                let pending = store
                    .get(&draft_id)
                    .map_err(|error| error.to_string())?
                    .ok_or_else(|| "REM operation missing".to_string())?;
                assert!(!pending.archived);
                assert_eq!(
                    pending.metadata["rem"]["operation_status"],
                    "pending_sources"
                );
                Ok(())
            })
            .expect("verify stale operation remained pending");
        ensure_rem_draft_winner(&server, &draft_id, &sources)
            .expect("source claims must remain owned by the pending draft");
    }

    #[test]
    fn candidate_dedup_uses_physical_store_and_source_id() {
        let store = test_store("same-physical-db");
        let source = occupied_entry("same-source", &[]);
        let mut candidates = vec![
            RemCandidate {
                store: store.clone(),
                entry: source.clone(),
            },
            RemCandidate {
                store,
                entry: source,
            },
        ];
        deduplicate_rem_candidates(&mut candidates);
        assert_eq!(candidates.len(), 1);
    }

    #[test]
    fn pending_recovery_accepts_only_the_current_runtime_source_stores() {
        let global = test_store("/home/global.db");
        let project_a = test_store("/home/projects/a.db");
        let project_b = test_store("/home/projects/b.db");
        let owned = vec![
            RemSourceRef {
                store: global.clone(),
                id: "global-source".to_string(),
                revision: 1,
            },
            RemSourceRef {
                store: project_a.clone(),
                id: "project-source".to_string(),
                revision: 1,
            },
        ];

        let runtime_a = RuntimeRemSourceStores {
            global: global.clone(),
            project: Some(project_a),
            shared_route: RemSourceRole::Global,
        };
        assert!(rem_sources_belong_to_runtime_stores(&owned, &runtime_a));
        let runtime_b = RuntimeRemSourceStores {
            global: global.clone(),
            project: Some(project_b),
            shared_route: RemSourceRole::Global,
        };
        assert!(!rem_sources_belong_to_runtime_stores(&owned, &runtime_b));
        let global_only = RuntimeRemSourceStores {
            global,
            project: None,
            shared_route: RemSourceRole::Global,
        };
        assert!(!rem_sources_belong_to_runtime_stores(&owned, &global_only));
    }

    #[test]
    fn pending_recovery_reports_foreign_rows_and_rejects_mixed_store_ownership() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let wiki_db = home.join("projects/wiki").join(memcore::MEMORY_DB_FILENAME);
        std::fs::create_dir_all(wiki_db.parent().unwrap()).expect("create Wiki store directory");
        drop(memcore::MemoryStore::open(wiki_db.to_str().unwrap()).expect("initialize Wiki store"));
        let server = MemoryServer::new_with_home_for_test(home.join("global.db"), None, home)
            .expect("open runtime server");
        let runtime = runtime_rem_source_stores(&server).expect("bind runtime stores");

        let foreign_sources = vec![RemSourceRef {
            store: test_store("foreign-physical-store"),
            id: "foreign-source".to_string(),
            revision: 1,
        }];
        let foreign_id = stable_rem_draft_id(&foreign_sources);
        persist_rem_draft_operation(
            &server,
            &occupied_entry(&foreign_id, &foreign_sources),
            &foreign_sources,
        )
        .expect("persist foreign pending operation");
        let report = recover_pending_rem_operations(&server).expect("skip foreign operation");
        assert_eq!(report.foreign_pending_skipped, 1);
        assert_eq!(report.completed, 0);

        let mut mixed_sources = vec![
            RemSourceRef {
                store: runtime.global,
                id: "current-source".to_string(),
                revision: 1,
            },
            RemSourceRef {
                store: test_store("another-foreign-physical-store"),
                id: "foreign-source-2".to_string(),
                revision: 1,
            },
        ];
        mixed_sources.sort();
        let mixed_id = stable_rem_draft_id(&mixed_sources);
        persist_rem_draft_operation(
            &server,
            &occupied_entry(&mixed_id, &mixed_sources),
            &mixed_sources,
        )
        .expect("persist mixed pending operation");
        let error =
            recover_pending_rem_operations(&server).expect_err("mixed ownership must fail closed");
        assert!(error.contains(&mixed_id), "{error}");
        assert!(error.contains("mixes current and foreign"), "{error}");
    }

    #[test]
    fn source_routing_marks_a_dual_role_physical_store_once() {
        let shared = test_store("same-physical-db");
        let sources = vec![RemSourceRef {
            store: shared.clone(),
            id: "source".to_string(),
            revision: 1,
        }];
        let runtime_stores = RuntimeRemSourceStores {
            global: shared.clone(),
            project: Some(shared),
            shared_route: RemSourceRole::Global,
        };
        let (global, project) = route_rem_sources(&sources, &runtime_stores, "draft").unwrap();
        assert_eq!(global, vec![("source".to_string(), 1)]);
        assert!(project.is_empty());
    }

    #[test]
    fn deterministic_id_occupant_must_match_exact_source_set() {
        let sources = vec![RemSourceRef {
            store: test_store("project-db"),
            id: "source-a".to_string(),
            revision: 1,
        }];
        let id = stable_rem_draft_id(&sources);
        let valid = occupied_entry(&id, &sources);
        validate_existing_rem_draft(&valid, &id, &sources).expect("canonical occupant");

        let collision_sources = vec![RemSourceRef {
            store: test_store("project-db"),
            id: "source-b".to_string(),
            revision: 1,
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

    #[test]
    fn pending_operation_from_an_older_producer_version_still_validates() {
        let sources = vec![RemSourceRef {
            store: test_store("project-db"),
            id: "source-a".to_string(),
            revision: 1,
        }];
        let old_version = "weekly-wiki-evolver-v0";
        let id = stable_rem_draft_id_for_version(&sources, old_version);
        let mut existing = occupied_entry(&id, &sources);
        existing.metadata["rem"]["producer_version"] = json!(old_version);

        validate_existing_rem_draft(&existing, &id, &sources)
            .expect("recovery must honor the operation's persisted producer version");
    }
}
