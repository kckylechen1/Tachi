use super::capture::*;
use super::helpers::*;
use super::recall_cache::process_recall_rerank_cache_job;
use super::*;
use regex::Regex;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::Ordering;
use std::sync::OnceLock;

/// Outcome of a memory-distill job. Carries a structured skip reason so
/// foundry_jobs.metadata.skip_reason answers "why didn't this run?" instead
/// of the previous opaque "worker reported no-op".
pub(crate) enum DistillOutcome {
    Wrote(#[allow(dead_code)] String),
    Skipped(String),
}

/// All source memories filtered out (archived or already a distill output).
pub(crate) const SKIP_NO_SOURCE_ENTRIES: &str = "no_source_entries";
/// No coherent topic/entity bucket among the source memories.
pub(crate) const SKIP_NO_COHERENT_BUCKET: &str = "no_coherent_bucket";
/// LLM returned an empty payload (post-trim).
pub(crate) const SKIP_EMPTY_LLM_OUTPUT: &str = "empty_llm_output";

const GUIDE_TYPE_CONSTRAINT: &str = "constraint";
const GUIDE_TYPE_FIX_PATTERN: &str = "fix_pattern";
const GUIDE_TYPE_DECISION: &str = "decision";
const GUIDE_TYPE_RUNBOOK: &str = "runbook";

fn foundry_requested_by(server: &MemoryServer) -> Option<String> {
    server
        .agent_runtime_read()
        .agent_profile
        .as_ref()
        .map(|profile| profile.agent_id.clone())
}

fn foundry_job_lane(kind: &memory_core::FoundryJobKind) -> memory_core::FoundryModelLane {
    match kind {
        // PR #2 / Q3: `MemoryNeighborhood` is pure vector-math + DB work
        // and never calls a chat model. The legacy `Maintenance` lane has
        // been removed (deserialize-aliased to `Reasoning` for back-compat).
        memory_core::FoundryJobKind::MemoryNeighborhood => memory_core::FoundryModelLane::Reasoning,
        memory_core::FoundryJobKind::RecallRerankCache => memory_core::FoundryModelLane::Rerank,
        memory_core::FoundryJobKind::MemoryDistill => memory_core::FoundryModelLane::Distill,
        memory_core::FoundryJobKind::ForgetSweep => memory_core::FoundryModelLane::Distill,
        _ => memory_core::FoundryModelLane::Reasoning,
    }
}

fn foundry_worker_name(kind: &memory_core::FoundryJobKind) -> &'static str {
    match kind {
        memory_core::FoundryJobKind::MemoryNeighborhood => "foundry_neighborhood",
        memory_core::FoundryJobKind::RecallRerankCache => "foundry_recall_rerank_cache",
        memory_core::FoundryJobKind::MemoryDistill => "foundry_distill",
        memory_core::FoundryJobKind::ForgetSweep => "foundry_forget",
        _ => "foundry",
    }
}

pub(super) fn foundry_job_label(kind: &memory_core::FoundryJobKind) -> &'static str {
    match kind {
        memory_core::FoundryJobKind::MemoryNeighborhood => "memory_neighborhood",
        memory_core::FoundryJobKind::RecallRerankCache => "recall_rerank_cache",
        memory_core::FoundryJobKind::MemoryDistill => "memory_distill",
        memory_core::FoundryJobKind::ForgetSweep => "forget_sweep",
        memory_core::FoundryJobKind::SessionIngest => "session_ingest",
        memory_core::FoundryJobKind::MemoryEnrichment => "memory_enrichment",
        memory_core::FoundryJobKind::SkillEvolution => "skill_evolution",
        memory_core::FoundryJobKind::AgentEvolution => "agent_evolution",
        memory_core::FoundryJobKind::ProfileProjection => "profile_projection",
    }
}

fn build_foundry_maintenance_job(
    server: &MemoryServer,
    kind: memory_core::FoundryJobKind,
    agent_id: &str,
    path_prefix: &str,
    memory_ids: &[String],
    metadata: serde_json::Value,
) -> memory_core::FoundryJobSpec {
    let mut sorted_memory_ids = memory_ids.to_vec();
    sorted_memory_ids.sort();
    sorted_memory_ids.dedup();
    memory_core::FoundryJobSpec {
        id: format!("foundry-job:{}", uuid::Uuid::new_v4()),
        kind: kind.clone(),
        lane: foundry_job_lane(&kind),
        status: memory_core::FoundryJobStatus::Queued,
        target_agent_id: Some(agent_id.to_string()),
        requested_by: foundry_requested_by(server),
        created_at: Utc::now().to_rfc3339(),
        evidence_count: sorted_memory_ids.len(),
        goal_count: 1,
        metadata: json!({
            "path_prefix": path_prefix,
            "memory_ids": sorted_memory_ids,
            "job": metadata,
        }),
    }
}

pub(super) fn enqueue_capture_maintenance_jobs(
    server: &MemoryServer,
    target_db: DbScope,
    named_project: Option<String>,
    db_path: Option<std::path::PathBuf>,
    agent_id: &str,
    path_prefix: &str,
    memory_ids: &[String],
    merged_count: usize,
    duplicate_count: usize,
) -> Result<Vec<memory_core::FoundryJobSpec>, String> {
    if memory_ids.is_empty() {
        return Ok(Vec::new());
    }

    let specs = capture_maintenance_specs(
        server,
        agent_id,
        path_prefix,
        memory_ids,
        merged_count,
        duplicate_count,
    );

    for spec in &specs {
        server.enqueue_foundry_job(FoundryMaintenanceItem {
            job: spec.clone(),
            target_db,
            named_project: named_project.clone(),
            db_path: db_path.clone(),
            path_prefix: path_prefix.to_string(),
            memory_ids: memory_ids.to_vec(),
        })?;
    }

    Ok(specs)
}

/// Build the (Phase 1) per-capture maintenance specs. Pulled out of
/// [`enqueue_capture_maintenance_jobs`] so the kind-set can be asserted in
/// unit tests without spinning up a MemoryServer.
///
/// Phase 1 invariant: this list MUST NOT include
/// [`memory_core::FoundryJobKind::MemoryDistill`]. Distill is now handled
/// exclusively by the daily batch scheduler
/// (`run_daily_batch_distill`); per-capture distill jobs would defeat the
/// batching that keeps Claude CLI invocations cheap.
pub(super) fn capture_maintenance_specs(
    server: &MemoryServer,
    agent_id: &str,
    path_prefix: &str,
    memory_ids: &[String],
    merged_count: usize,
    duplicate_count: usize,
) -> Vec<memory_core::FoundryJobSpec> {
    vec![
        build_foundry_maintenance_job(
            server,
            memory_core::FoundryJobKind::MemoryNeighborhood,
            agent_id,
            path_prefix,
            memory_ids,
            json!({
                "kind": "memory_neighborhood",
                "neighbor_limit": FOUNDRY_RELATED_LIMIT,
                "merged_count": merged_count,
                "duplicate_count": duplicate_count,
            }),
        ),
        build_foundry_maintenance_job(
            server,
            memory_core::FoundryJobKind::RecallRerankCache,
            agent_id,
            path_prefix,
            memory_ids,
            json!({
                "kind": "recall_rerank_cache",
                "top_k": FOUNDRY_RECALL_RERANK_TOP_K,
                "candidate_multiplier": FOUNDRY_RECALL_RERANK_CANDIDATE_MULTIPLIER,
            }),
        ),
        // NOTE: Phase 1 — MemoryDistill is no longer enqueued from capture.
        // The daily batch distill (`run_daily_batch_distill`, invoked from
        // the bootstrap scheduler) replaces the per-capture distill job.
        // We still enqueue ForgetSweep so per-capture sweeps continue to
        // garbage-collect stale distill memories.
        build_foundry_maintenance_job(
            server,
            memory_core::FoundryJobKind::ForgetSweep,
            agent_id,
            path_prefix,
            memory_ids,
            json!({
                "kind": "forget_sweep",
                "keep_latest": FOUNDRY_DISTILL_KEEP,
            }),
        ),
    ]
}

pub(crate) fn enqueue_foundry_capture_maintenance(
    server: &MemoryServer,
    target_db: DbScope,
    named_project: Option<String>,
    db_path: Option<std::path::PathBuf>,
    agent_id: &str,
    path_prefix: &str,
    memory_ids: &[String],
) -> Result<Vec<memory_core::FoundryJobSpec>, String> {
    enqueue_capture_maintenance_jobs(
        server,
        target_db,
        named_project,
        db_path,
        agent_id,
        path_prefix,
        memory_ids,
        0,
        0,
    )
}

pub(super) fn scheduled_distill_path_prefix(path: &str) -> String {
    let segments = path
        .trim_matches('/')
        .split('/')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>();
    match segments.as_slice() {
        [] => "/".to_string(),
        ["project", second, ..] => format!("/project/{second}"),
        ["kanban", from, to, ..] => format!("/kanban/{from}/{to}"),
        ["kanban", from] => format!("/kanban/{from}"),
        ["wiki", kind, domain, ..] => format!("/wiki/{kind}/{domain}"),
        ["wiki", kind] => format!("/wiki/{kind}"),
        [first, ..] => format!("/{first}"),
    }
}

fn is_generic_distill_topic(topic: &str) -> bool {
    matches!(
        topic.trim().to_ascii_lowercase().as_str(),
        "" | "unknown"
            | "general"
            | "other"
            | "misc"
            | "architecture"
            | "bug fix"
            | "bug fixes"
            | "bugfix"
            | "testing"
            | "test"
            | "changelog"
            | "roadmap"
            | "todo"
    )
}

pub(super) fn coherence_bucket_key(topic: &str, entities: &[String]) -> Option<String> {
    let topic = topic.trim();
    if !topic.is_empty() && !is_generic_distill_topic(topic) {
        return Some(format!("topic:{topic}"));
    }

    entities
        .iter()
        .map(|entity| entity.trim())
        .find(|entity| !entity.is_empty())
        .map(|entity| format!("entity:{entity}"))
}

#[allow(dead_code)] // Phase 1: legacy scheduler helper.
pub(super) fn scheduled_distill_group_key(path: &str, coherence_key: &str) -> String {
    format!("{}#{coherence_key}", scheduled_distill_path_prefix(path))
}

#[allow(dead_code)] // Phase 1: legacy scheduler helper.
fn distill_metadata_is_trusted(metadata: &serde_json::Value) -> bool {
    let has_coherence_key = metadata
        .get("coherence_key")
        .and_then(|value| value.as_str())
        .is_some_and(|value| !value.trim().is_empty());
    if !has_coherence_key {
        return false;
    }

    let has_bad_flag = metadata
        .get("quality_flags")
        .and_then(|value| value.as_array())
        .into_iter()
        .flatten()
        .filter_map(|value| value.as_str())
        .any(|flag| {
            matches!(
                flag,
                "legacy" | "suspect" | "incoherent" | "legacy_incoherent_distill"
            )
        });
    !has_bad_flag
}

fn source_namespace_count(entries: &[MemoryEntry]) -> usize {
    entries
        .iter()
        .map(|entry| scheduled_distill_path_prefix(&entry.path))
        .collect::<HashSet<_>>()
        .len()
}

fn distill_quality_flags(entries: &[MemoryEntry]) -> Vec<String> {
    let mut flags = Vec::new();
    if entries.len() < FOUNDRY_DISTILL_MIN_BATCH {
        flags.push("min_batch_not_met".to_string());
    }
    if source_namespace_count(entries) > 1 {
        flags.push("mixed_namespace".to_string());
    }
    flags
}

/// Scan for project memories that have not yet been included in a distill output
/// and enqueue distill jobs for sufficiently large coherent groups.
#[allow(dead_code)] // Phase 1: kept as manual fallback; see foundry_runtime_ops/mod.rs re-export.
pub(crate) async fn schedule_pending_distill_jobs(server: &MemoryServer) -> Result<usize, String> {
    struct ScheduledDistillGroup {
        path_prefix: String,
        coherence_key: String,
        memory_ids: Vec<String>,
    }

    if !server.has_project_db() {
        return Ok(0);
    }

    let (unprocessed_memories, pending_groups) = server.with_project_store_read(|store| {
        let conn = store.connection();

        let mut processed_ids = HashSet::new();
        let mut stmt = conn
            .prepare("SELECT metadata FROM memories WHERE archived = 0 AND source = ?1")
            .map_err(|e| format!("prepare distill metadata query: {e}"))?;
        let rows = stmt
            .query_map([FOUNDRY_DISTILL_SOURCE], |row| row.get::<_, String>(0))
            .map_err(|e| format!("query distill metadata rows: {e}"))?;
        for row in rows {
            let metadata_raw = row.map_err(|e| format!("read distill metadata row: {e}"))?;
            let metadata = serde_json::from_str::<serde_json::Value>(&metadata_raw)
                .unwrap_or_else(|_| json!({}));
            if distill_metadata_is_trusted(&metadata) {
                let source_ids = metadata
                    .get("source_memory_ids")
                    .and_then(|value| value.as_array())
                    .into_iter()
                    .flatten()
                    .filter_map(|value| value.as_str());
                processed_ids.extend(source_ids.map(ToOwned::to_owned));
            }
        }

        let mut pending_groups = HashSet::new();
        let mut stmt = conn
            .prepare(
                "SELECT path_prefix, metadata
                 FROM foundry_jobs
                 WHERE kind = 'memory_distill' AND status IN ('queued', 'running')",
            )
            .map_err(|e| format!("prepare pending distill job query: {e}"))?;
        let rows = stmt
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(|e| format!("query pending distill jobs: {e}"))?;
        for row in rows {
            let (path_prefix, metadata_raw) =
                row.map_err(|e| format!("read pending distill row: {e}"))?;
            let metadata = serde_json::from_str::<serde_json::Value>(&metadata_raw)
                .unwrap_or_else(|_| json!({}));
            let coherence_key = metadata
                .get("job")
                .and_then(|job| job.get("coherence_key"))
                .and_then(|value| value.as_str());
            if let Some(coherence_key) = coherence_key {
                pending_groups.insert(scheduled_distill_group_key(&path_prefix, coherence_key));
            } else {
                pending_groups.insert(path_prefix);
            }
        }

        let mut unprocessed = Vec::new();
        let mut stmt = conn
            .prepare(
                "SELECT id, path, topic, entities
                 FROM memories
                  WHERE archived = 0 AND source != ?1
                  ORDER BY timestamp ASC",
            )
            .map_err(|e| format!("prepare candidate memory query: {e}"))?;
        let rows = stmt
            .query_map([FOUNDRY_DISTILL_SOURCE], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                ))
            })
            .map_err(|e| format!("query candidate memories: {e}"))?;
        for row in rows {
            let (memory_id, path, topic, entities_raw) =
                row.map_err(|e| format!("read candidate memory row: {e}"))?;
            if !processed_ids.contains(&memory_id) {
                let entities =
                    serde_json::from_str::<Vec<String>>(&entities_raw).unwrap_or_default();
                unprocessed.push((memory_id, path, topic, entities));
            }
        }

        Ok((unprocessed, pending_groups))
    })?;

    if unprocessed_memories.is_empty() {
        return Ok(0);
    }

    let mut grouped_ids: HashMap<String, ScheduledDistillGroup> = HashMap::new();
    for (memory_id, path, topic, entities) in unprocessed_memories {
        let Some(coherence_key) = coherence_bucket_key(&topic, &entities) else {
            continue;
        };
        let path_prefix = scheduled_distill_path_prefix(&path);
        let group_key = scheduled_distill_group_key(&path_prefix, &coherence_key);
        if pending_groups.contains(&group_key) || pending_groups.contains(&path_prefix) {
            continue;
        }
        grouped_ids
            .entry(group_key)
            .or_insert_with(|| ScheduledDistillGroup {
                path_prefix: path_prefix.clone(),
                coherence_key: coherence_key.clone(),
                memory_ids: Vec::new(),
            })
            .memory_ids
            .push(memory_id);
    }

    let scheduler_agent_id =
        foundry_requested_by(server).unwrap_or_else(|| "tachi_scheduler".into());
    let mut jobs_scheduled = 0usize;

    for (_, group) in grouped_ids {
        if group.memory_ids.len() < FOUNDRY_DISTILL_MIN_BATCH {
            continue;
        }

        let job = build_foundry_maintenance_job(
            server,
            memory_core::FoundryJobKind::MemoryDistill,
            &scheduler_agent_id,
            &group.path_prefix,
            &group.memory_ids,
            json!({
                "kind": "memory_distill",
                "window": FOUNDRY_DISTILL_WINDOW,
                "coherence_key": group.coherence_key,
                "scheduler": "bootstrap_distill_interval",
            }),
        );

        server.enqueue_foundry_job(FoundryMaintenanceItem {
            job,
            target_db: DbScope::Project,
            named_project: None,
            db_path: None,
            path_prefix: group.path_prefix,
            memory_ids: group.memory_ids,
        })?;
        jobs_scheduled += 1;
    }

    Ok(jobs_scheduled)
}

pub(super) fn with_foundry_store<T>(
    server: &MemoryServer,
    item: &FoundryMaintenanceItem,
    f: impl FnOnce(&mut MemoryStore) -> Result<T, String>,
) -> Result<T, String> {
    if let Some(ref db_path) = item.db_path {
        server.with_path_store(db_path, f)
    } else if let Some(project_name) = item.named_project.as_deref() {
        server.with_named_project_store(project_name, f)
    } else {
        server.with_store_for_scope(item.target_db, f)
    }
}

pub(super) fn with_foundry_store_read<T>(
    server: &MemoryServer,
    item: &FoundryMaintenanceItem,
    f: impl FnOnce(&mut MemoryStore) -> Result<T, String>,
) -> Result<T, String> {
    if let Some(ref db_path) = item.db_path {
        server.with_path_store_read(db_path, f)
    } else if let Some(project_name) = item.named_project.as_deref() {
        server.with_named_project_store_read(project_name, f)
    } else {
        server.with_store_for_scope_read(item.target_db, f)
    }
}

pub(super) fn memory_claim_signature(entry: &MemoryEntry) -> String {
    format!(
        "{}:r{}:vec{}:arch{}",
        entry.id,
        entry.revision,
        if entry.vector.is_some() { 1 } else { 0 },
        if entry.archived { 1 } else { 0 }
    )
}

pub(super) fn build_foundry_event_hash(
    server: &MemoryServer,
    item: &FoundryMaintenanceItem,
) -> Result<String, String> {
    let mut signatures = Vec::with_capacity(item.memory_ids.len());
    for memory_id in &item.memory_ids {
        let maybe_entry = with_foundry_store_read(server, item, |store| {
            store
                .get(memory_id)
                .map_err(|e| format!("Failed to load memory {} for claim hash: {e}", memory_id))
        })?;
        match maybe_entry {
            Some(entry) => signatures.push(memory_claim_signature(&entry)),
            None => signatures.push(format!("{memory_id}:missing")),
        }
    }
    signatures.sort();
    let job_scope = match item.job.kind {
        memory_core::FoundryJobKind::RecallRerankCache => {
            stable_hash(&item.job.metadata.to_string())
        }
        _ => String::new(),
    };

    Ok(stable_hash(&format!(
        "{}:{}:{}:{}:{}",
        foundry_job_label(&item.job.kind),
        item.named_project.as_deref().unwrap_or("default"),
        item.path_prefix,
        job_scope,
        signatures.join(","),
    )))
}

pub(super) fn merge_foundry_metadata(
    existing: &serde_json::Value,
    patch: serde_json::Value,
) -> serde_json::Value {
    let mut root = match existing {
        serde_json::Value::Object(map) => map.clone(),
        _ => serde_json::Map::new(),
    };
    let mut foundry = root
        .get("foundry")
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default();
    if let Some(patch_obj) = patch.as_object() {
        for (key, value) in patch_obj {
            foundry.insert(key.clone(), value.clone());
        }
    }
    root.insert("foundry".into(), serde_json::Value::Object(foundry));
    serde_json::Value::Object(root)
}

pub(super) fn update_entry_metadata(
    store: &mut MemoryStore,
    entry: &MemoryEntry,
    metadata: &serde_json::Value,
) -> Result<bool, String> {
    store
        .update_with_revision(
            &entry.id,
            &entry.text,
            &entry.summary,
            &entry.source,
            metadata,
            entry.vector.as_deref(),
            entry.revision,
        )
        .map_err(|e| format!("Failed to update foundry metadata for {}: {e}", entry.id))
}

pub(super) fn infer_memory_insight(
    entry: &MemoryEntry,
    avg_importance: f64,
    contradiction_count: u32,
    same_topic_count: u32,
    related_count: usize,
) -> serde_json::Value {
    let surprise =
        memory_core::surprise_score(entry, avg_importance, contradiction_count, same_topic_count);
    let mut reasons = Vec::new();

    if contradiction_count > 0 {
        reasons.push("contradiction".to_string());
    }
    if same_topic_count <= 1 {
        reasons.push("novel_topic".to_string());
    }
    if entry.access_count == 0 && entry.importance > 0.7 {
        reasons.push("overlooked_high_importance".to_string());
    }
    if (entry.importance - avg_importance).abs() >= 0.25 {
        reasons.push("importance_outlier".to_string());
    }
    if related_count >= FOUNDRY_RELATED_LIMIT {
        reasons.push("dense_neighborhood".to_string());
    }

    json!({
        "kind": "memory_insight",
        "surprise": round3(surprise),
        "priority": if surprise >= 0.4 { "high" } else if surprise >= 0.2 { "medium" } else { "low" },
        "reasons": reasons,
        "signals": {
            "avg_importance": round3(avg_importance),
            "importance_delta": round3(entry.importance - avg_importance),
            "contradiction_count": contradiction_count,
            "same_topic_count": same_topic_count,
            "related_count": related_count,
        }
    })
}

pub(super) fn build_foundry_distill_root(agent_id: &str) -> String {
    format!("{}/distilled", build_foundry_agent_root(agent_id))
}

fn guide_text_fragments<'a>(
    distill_text: &'a str,
    source_entries: &'a [MemoryEntry],
) -> impl Iterator<Item = &'a str> {
    std::iter::once(distill_text).chain(source_entries.iter().flat_map(|entry| {
        std::iter::once(entry.summary.as_str())
            .chain(std::iter::once(entry.text.as_str()))
            .chain(std::iter::once(entry.topic.as_str()))
            .chain(entry.keywords.iter().map(String::as_str))
    }))
}

fn contains_ignore_ascii_case(haystack: &str, needle: &str) -> bool {
    let n = needle.as_bytes();
    if n.is_empty() {
        return true;
    }
    let h = haystack.as_bytes();
    if h.len() < n.len() {
        return false;
    }
    h.windows(n.len())
        .any(|window| window.eq_ignore_ascii_case(n))
}

fn fragments_contain_any<'a, I>(fragments: I, needles: &[&str]) -> bool
where
    I: IntoIterator<Item = &'a str>,
{
    for fragment in fragments {
        for needle in needles {
            if contains_ignore_ascii_case(fragment, needle) {
                return true;
            }
        }
    }
    false
}

fn contains_any(haystack: &str, needles: &[&str]) -> bool {
    needles
        .iter()
        .any(|needle| contains_ignore_ascii_case(haystack, needle))
}

fn has_numbered_steps(text: &str) -> bool {
    text.lines()
        .filter(|line| {
            let trimmed = line.trim_start();
            let mut chars = trimmed.chars();
            matches!(chars.next(), Some(ch) if ch.is_ascii_digit())
                && matches!(chars.next(), Some('.' | ')'))
        })
        .take(2)
        .count()
        >= 2
}

pub(super) fn classify_distill_guide_type(
    distill_text: &str,
    source_entries: &[MemoryEntry],
) -> &'static str {
    let fragments = || guide_text_fragments(distill_text, source_entries);

    if fragments_contain_any(
        fragments(),
        &[
            "fix",
            "fixed",
            "repair",
            "bug",
            "error",
            "failure",
            "failed",
            "panic",
            "exception",
            "regression",
            "linker",
            "修复",
            "报错",
            "错误",
            "失败",
        ],
    ) {
        return GUIDE_TYPE_FIX_PATTERN;
    }
    if fragments_contain_any(
        fragments(),
        &[
            "must",
            "must not",
            "never",
            "required",
            "constraint",
            "invariant",
            "policy",
            "do not",
            "don't",
            "不得",
            "必须",
            "禁止",
            "约束",
        ],
    ) {
        return GUIDE_TYPE_CONSTRAINT;
    }
    if fragments_contain_any(
        fragments(),
        &[
            "decided", "decision", "choose", "chosen", "accepted", "rejected", "tradeoff", "adr",
            "决定", "取舍", "拒绝",
        ],
    ) || source_entries
        .iter()
        .any(|entry| entry.category.eq_ignore_ascii_case("decision"))
    {
        return GUIDE_TYPE_DECISION;
    }
    if fragments_contain_any(
        fragments(),
        &[
            "runbook",
            "checklist",
            "procedure",
            "step",
            "steps",
            "playbook",
            "how to",
            "操作",
            "步骤",
            "流程",
        ],
    ) || has_numbered_steps(distill_text)
    {
        return GUIDE_TYPE_RUNBOOK;
    }
    GUIDE_TYPE_RUNBOOK
}

fn build_guide_distill_path(agent_id: &str, guide_type: &str, timestamp_segment: &str) -> String {
    format!(
        "/guide/{}/{}/{}",
        guide_type,
        sanitize_safe_path_name(agent_id),
        timestamp_segment
    )
}

fn trim_context_token(raw: &str) -> String {
    raw.trim_matches(|ch: char| {
        ch.is_whitespace()
            || matches!(
                ch,
                '`' | '"' | '\'' | ',' | ';' | ':' | '(' | ')' | '[' | ']' | '{' | '}'
            )
    })
    .trim_end_matches('.')
    .to_string()
}

fn looks_like_file_pattern(value: &str) -> bool {
    let value = value.trim();
    if value.is_empty() {
        return false;
    }
    if value.starts_with("http://") || value.starts_with("https://") {
        return false;
    }
    let has_path_separator = value.contains('/');
    let has_wildcard = value.contains('*');
    if !has_path_separator && !has_wildcard {
        match value.rfind('.') {
            Some(dot) if dot > 0 && dot < value.len() - 1 => {}
            _ => return false,
        }
    }
    has_wildcard
        || value.ends_with(".rs")
        || value.ends_with(".ts")
        || value.ends_with(".tsx")
        || value.ends_with(".js")
        || value.ends_with(".jsx")
        || value.ends_with(".py")
        || value.ends_with(".go")
        || value.ends_with(".java")
        || value.ends_with(".md")
        || value.ends_with(".toml")
        || value.ends_with(".json")
        || value.ends_with(".yaml")
        || value.ends_with(".yml")
}

fn file_pattern_token_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| Regex::new(r"[A-Za-z0-9_./*\-]+").expect("file pattern regex compiles"))
}

fn wildcard_for_file_path(path: &str) -> Option<String> {
    let slash = path.rfind('/')?;
    let dot = path.rfind('.')?;
    if dot <= slash {
        return None;
    }
    Some(format!("{}/*{}", &path[..slash], &path[dot..]))
}

fn collect_file_patterns_from_text(text: &str, out: &mut Vec<String>) {
    for mat in file_pattern_token_regex().find_iter(text) {
        let candidate = mat.as_str();
        let bytes = candidate.as_bytes();
        if !bytes.iter().any(|b| matches!(b, b'*' | b'.' | b'/')) {
            continue;
        }
        let token = trim_context_token(candidate);
        if looks_like_file_pattern(&token) {
            if let Some(wildcard) = wildcard_for_file_path(&token) {
                out.push(token);
                out.push(wildcard);
            } else {
                out.push(token);
            }
        }
    }
}

fn collect_metadata_file_patterns(value: &serde_json::Value, out: &mut Vec<String>) {
    match value {
        serde_json::Value::String(raw) => {
            let token = trim_context_token(raw);
            if looks_like_file_pattern(&token) {
                out.push(token.clone());
                if let Some(wildcard) = wildcard_for_file_path(&token) {
                    out.push(wildcard);
                }
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                collect_metadata_file_patterns(item, out);
            }
        }
        serde_json::Value::Object(map) => {
            for (key, value) in map {
                let key = key.to_ascii_lowercase();
                if key.contains("file") || key.contains("path") {
                    collect_metadata_file_patterns(value, out);
                }
            }
        }
        _ => {}
    }
}

fn infer_file_patterns(source_entries: &[MemoryEntry]) -> Vec<String> {
    let mut patterns = Vec::new();
    for entry in source_entries {
        for candidate in [&entry.path, &entry.location] {
            let token = trim_context_token(candidate);
            if looks_like_file_pattern(&token) {
                patterns.push(token.clone());
                if let Some(wildcard) = wildcard_for_file_path(&token) {
                    patterns.push(wildcard);
                }
            }
        }
        collect_metadata_file_patterns(&entry.metadata, &mut patterns);
        collect_file_patterns_from_text(&entry.text, &mut patterns);
    }
    dedup_strings(patterns).into_iter().take(12).collect()
}

fn looks_like_error_line(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    contains_any(
        &lower,
        &[
            "error",
            "failed",
            "failure",
            "panic",
            "exception",
            "could not",
            "cannot",
            "linker",
            "报错",
            "错误",
            "失败",
        ],
    )
}

fn infer_error_patterns(distill_text: &str, source_entries: &[MemoryEntry]) -> Vec<String> {
    let mut patterns = Vec::new();
    for text in std::iter::once(distill_text).chain(
        source_entries
            .iter()
            .flat_map(|entry| [entry.summary.as_str(), entry.text.as_str()]),
    ) {
        for line in text.lines() {
            if looks_like_error_line(line) {
                patterns.push(line.trim().chars().take(120).collect::<String>());
            }
        }
    }
    dedup_strings(patterns).into_iter().take(8).collect()
}

fn mentions_rejection(text: &str) -> bool {
    contains_any(
        &text.to_ascii_lowercase(),
        &[
            "reject",
            "rejected",
            "avoid",
            "do not",
            "don't",
            "never",
            "instead of",
            "rather than",
            "拒绝",
            "不要",
            "避免",
            "禁止",
        ],
    )
}

fn guide_edge_relations(guide_type: &str, distill_text: &str) -> Vec<&'static str> {
    let mut relations = vec!["distilled_from"];
    match guide_type {
        GUIDE_TYPE_FIX_PATTERN => relations.push("fixed_by"),
        _ => relations.push("causes"),
    }
    if mentions_rejection(distill_text) {
        relations.push("rejected_because");
    }
    relations
}

pub(super) fn build_distill_edges(
    distill_entry: &MemoryEntry,
    source_entries: &[MemoryEntry],
    guide_type: &str,
    created_at: &str,
) -> Vec<memory_core::MemoryEdge> {
    let mut edges = Vec::new();
    let mut seen = HashSet::new();
    for source in source_entries {
        for relation in guide_edge_relations(guide_type, &distill_entry.text) {
            let (source_id, target_id, weight) = match relation {
                "distilled_from" => (distill_entry.id.clone(), source.id.clone(), 1.0),
                "fixed_by" => (source.id.clone(), distill_entry.id.clone(), 0.9),
                "rejected_because" => (distill_entry.id.clone(), source.id.clone(), 0.75),
                _ => (source.id.clone(), distill_entry.id.clone(), 0.7),
            };
            if seen.insert((source_id.clone(), target_id.clone(), relation.to_string())) {
                edges.push(memory_core::MemoryEdge {
                    source_id,
                    target_id,
                    relation: relation.to_string(),
                    weight,
                    metadata: json!({
                        "source": "foundry_distill",
                        "guide_type": guide_type,
                    }),
                    created_at: created_at.to_string(),
                    valid_from: created_at.to_string(),
                    valid_to: None,
                });
            }
        }
    }
    edges
}

async fn process_memory_neighborhood_job(
    server: &MemoryServer,
    item: &FoundryMaintenanceItem,
) -> Result<usize, String> {
    let mut updated = 0usize;

    let avg_importance = with_foundry_store_read(server, item, |store| {
        store
            .avg_importance()
            .map_err(|e| format!("Failed to compute average importance: {e}"))
    })?;

    for memory_id in &item.memory_ids {
        let Some(entry) = with_foundry_store_read(server, item, |store| {
            store
                .get(memory_id)
                .map_err(|e| format!("Failed to load memory {} for neighborhood: {e}", memory_id))
        })?
        else {
            continue;
        };

        let Some(vector) = entry.vector.clone() else {
            continue;
        };

        let neighbors = search_similar_capture_entries(
            server,
            item.target_db,
            item.named_project.as_deref(),
            item.db_path.as_ref(),
            &item.path_prefix,
            &vector,
            FOUNDRY_RELATED_LIMIT + 2,
        )?;

        let mut best_neighbor: Option<memory_core::SearchResult> = None;
        let related = neighbors
            .into_iter()
            .filter(|row| row.entry.id != entry.id)
            .inspect(|row| {
                if best_neighbor.is_none() {
                    best_neighbor = Some(row.clone());
                }
            })
            .take(FOUNDRY_RELATED_LIMIT)
            .map(|row| {
                json!({
                    "id": row.entry.id,
                    "topic": row.entry.topic,
                    "path": row.entry.path,
                    "score": round3(row.score.vector),
                })
            })
            .collect::<Vec<_>>();

        if let Some(similar) = best_neighbor {
            let similarity = similar.score.vector;
            if similarity >= CAPTURE_DEDUP_THRESHOLD {
                let changed = with_foundry_store(server, item, |store| {
                    store.archive_memory(&entry.id).map_err(|e| {
                        format!("Failed to archive duplicate memory {}: {e}", entry.id)
                    })
                })?;
                if changed {
                    updated += 1;
                }
                continue;
            }

            if similarity >= CAPTURE_MERGE_THRESHOLD {
                let merged = merge_capture_entries(&similar.entry, &entry, similarity);
                persist_capture_entry(
                    server,
                    item.target_db,
                    item.named_project.as_deref(),
                    item.db_path.as_ref(),
                    &merged,
                )?;
                let changed = with_foundry_store(server, item, |store| {
                    store
                        .archive_memory(&entry.id)
                        .map_err(|e| format!("Failed to archive merged memory {}: {e}", entry.id))
                })?;
                if changed {
                    updated += 1;
                }
                queue_capture_enrichment(
                    server,
                    item.target_db,
                    item.named_project.clone(),
                    item.db_path.clone(),
                    &merged,
                    true,
                    item.job.target_agent_id.as_deref(),
                    Some(&item.path_prefix),
                );
                continue;
            }
        }

        if related.is_empty() {
            continue;
        }

        let (contradiction_count, same_topic_count) =
            with_foundry_store_read(server, item, |store| {
                let contradiction_count = store
                    .get_contradiction_count(&entry.id)
                    .map_err(|e| format!("Failed to count contradictions for {}: {e}", entry.id))?;
                let topic = entry.topic.trim();
                let same_topic_count = if topic.is_empty() {
                    0u32
                } else {
                    store.count_same_topic(topic).map_err(|e| {
                        format!("Failed to count same-topic memories for {}: {e}", entry.id)
                    })?
                };
                Ok((contradiction_count, same_topic_count))
            })?;
        let insight = infer_memory_insight(
            &entry,
            avg_importance,
            contradiction_count,
            same_topic_count,
            related.len(),
        );

        let metadata = merge_foundry_metadata(
            &entry.metadata,
            json!({
                "last_neighborhood_at": Utc::now().to_rfc3339(),
                "neighborhood_job_id": item.job.id,
                "related_entries": related,
                "insight": insight,
            }),
        );

        let applied = with_foundry_store(server, item, |store| {
            update_entry_metadata(store, &entry, &metadata)
        })?;
        if applied {
            updated += 1;
        }
    }

    Ok(updated)
}

pub(super) fn job_metadata_value<'a>(
    metadata: &'a serde_json::Value,
    key: &str,
) -> Option<&'a serde_json::Value> {
    metadata
        .get("job")
        .and_then(|job| job.get(key))
        .or_else(|| metadata.get(key))
}

pub(super) fn job_metadata_usize(metadata: &serde_json::Value, key: &str, default: usize) -> usize {
    job_metadata_value(metadata, key)
        .and_then(|value| value.as_u64())
        .map(|value| value as usize)
        .unwrap_or(default)
}

pub(super) fn job_metadata_string(metadata: &serde_json::Value, key: &str) -> Option<String> {
    job_metadata_value(metadata, key)
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

/// Minimum coherent batch size — fewer than this and a topic/entity bucket is
/// considered too thin to justify an LLM round-trip (avoids stitched hallucinations).
const FOUNDRY_DISTILL_MIN_BATCH: usize = 3;

/// Group memories by a coherence key (topic when available, otherwise the most
/// frequent shared entity). Buckets smaller than [`FOUNDRY_DISTILL_MIN_BATCH`]
/// are dropped — they would otherwise force the LLM to stitch unrelated facts
/// into a single false summary (the v0.15.x "缝合怪" regression).
pub(super) fn coherent_distill_buckets(
    entries: Vec<MemoryEntry>,
) -> Vec<(String, Vec<MemoryEntry>)> {
    let mut buckets: HashMap<String, Vec<MemoryEntry>> = HashMap::new();
    for entry in entries {
        let Some(key) = coherence_bucket_key(&entry.topic, &entry.entities) else {
            // No coherence signal — skip rather than risk a stitched distill.
            continue;
        };
        let namespace = scheduled_distill_path_prefix(&entry.path);
        buckets
            .entry(format!("{namespace}#{key}"))
            .or_default()
            .push(entry);
    }
    // PR #50 review: `coherent_distill_buckets` already drops buckets
    // whose `distill_quality_flags` are non-empty, so by construction
    // the selected bucket has clean flags. We re-derive them here for
    // two reasons:
    //   (1) they are stored in the distill memory's metadata for audit;
    //   (2) defense in depth: if the filter contract drifts in future
    //       refactors, we still surface a structured skip reason instead
    //       of silently writing a low-quality distill.
    buckets
        .into_iter()
        .filter(|(_, group)| distill_quality_flags(group).is_empty())
        .collect()
}

async fn process_memory_distill_job(
    server: &MemoryServer,
    item: &FoundryMaintenanceItem,
) -> Result<DistillOutcome, String> {
    let source_entries = with_foundry_store_read(server, item, |store| {
        let mut entries = Vec::new();
        for memory_id in &item.memory_ids {
            let maybe_entry = store
                .get(memory_id)
                .map_err(|e| format!("Failed to load memory {memory_id} for distill: {e}"))?;
            if let Some(entry) = maybe_entry {
                entries.push(entry);
            }
        }
        Ok(entries)
    })?;

    let raw_entries = source_entries
        .into_iter()
        .filter(|entry| !entry.archived && entry.source != FOUNDRY_DISTILL_SOURCE)
        .collect::<Vec<_>>();
    if raw_entries.is_empty() {
        return Ok(DistillOutcome::Skipped(SKIP_NO_SOURCE_ENTRIES.to_string()));
    }

    let preferred_coherence_key = item
        .job
        .metadata
        .get("job")
        .and_then(|job| job.get("coherence_key"))
        .and_then(|value| value.as_str())
        .map(str::to_string);

    // Coherence guard: only distil memories that share a topic or entity.
    // Pick the largest coherent bucket per job to keep behaviour 1:1 with the
    // legacy contract (one distill output per job).
    let mut buckets = coherent_distill_buckets(raw_entries);
    if buckets.is_empty() {
        return Ok(DistillOutcome::Skipped(SKIP_NO_COHERENT_BUCKET.to_string()));
    }
    let (bucket_key, source_entries) = if let Some(preferred_key) = preferred_coherence_key {
        let preferred_bucket_key = format!("{}#{preferred_key}", item.path_prefix);
        if let Some(index) = buckets
            .iter()
            .position(|(key, _)| key == &preferred_bucket_key || key == &preferred_key)
        {
            buckets.swap_remove(index)
        } else {
            // Preferred key was requested but no bucket matched. Log so this
            // doesn't silently diverge from the scheduler's intent.
            tracing::debug!(
                "[foundry/distill] preferred_coherence_key={preferred_key:?} not found in buckets ({} buckets); falling back to largest",
                buckets.len()
            );
            buckets.sort_by(|a, b| b.1.len().cmp(&a.1.len()));
            match buckets.into_iter().next() {
                Some(bucket) => bucket,
                None => return Ok(DistillOutcome::Skipped(SKIP_NO_COHERENT_BUCKET.to_string())),
            }
        }
    } else {
        buckets.sort_by(|a, b| b.1.len().cmp(&a.1.len()));
        match buckets.into_iter().next() {
            Some(bucket) => bucket,
            None => return Ok(DistillOutcome::Skipped(SKIP_NO_COHERENT_BUCKET.to_string())),
        }
    };

    let quality_flags = distill_quality_flags(&source_entries);
    if !quality_flags.is_empty() {
        // Defense in depth: by construction `coherent_distill_buckets`
        // already drops buckets with non-empty flags, so this branch is
        // currently unreachable. Kept (and converted to a structured
        // Skipped reason rather than a panic) so a future contract drift
        // in `coherent_distill_buckets` produces an observable skip code
        // ("quality_flags:<csv>") instead of silently emitting a low
        // quality distill memory. PR #50 review.
        return Ok(DistillOutcome::Skipped(format!(
            "quality_flags:{}",
            quality_flags.join(",")
        )));
    }

    let (namespace_key, coherence_key) = bucket_key
        .split_once('#')
        .map(|(namespace, coherence)| (namespace.to_string(), coherence.to_string()))
        .unwrap_or_else(|| (item.path_prefix.clone(), bucket_key.clone()));

    let distill_text = server
        .llm
        .generate_distill(&build_distill_input(&source_entries))
        .await
        .map_err(|e| format!("Foundry distill summary failed: {e}"))?;
    if distill_text.trim().is_empty() {
        return Ok(DistillOutcome::Skipped(SKIP_EMPTY_LLM_OUTPUT.to_string()));
    }

    let agent_id = item
        .job
        .target_agent_id
        .as_deref()
        .unwrap_or("unknown-agent");
    let now = Utc::now();
    let timestamp = now.to_rfc3339();
    let timestamp_segment = now.format("%Y%m%dT%H%M%S").to_string();
    let memory_id = uuid::Uuid::new_v4().to_string();
    let guide_type = classify_distill_guide_type(&distill_text, &source_entries);
    let file_patterns = infer_file_patterns(&source_entries);
    let error_patterns = infer_error_patterns(&distill_text, &source_entries);
    let legacy_distill_root = build_foundry_distill_root(agent_id);
    let mut metadata = crate::provenance::inject_provenance(
        server,
        json!({
            "guide": true,
            "guide_type": guide_type,
            "guide_layer": "guide",
            "file_patterns": file_patterns,
            "error_patterns": error_patterns,
            "source_memory_ids": source_entries.iter().map(|entry| entry.id.clone()).collect::<Vec<_>>(),
            "source_path_prefix": item.path_prefix,
            "namespace_key": namespace_key,
            "coherence_key": coherence_key,
            "bucket_key": bucket_key,
            "quality_flags": quality_flags,
            "job_id": item.job.id,
            "legacy_distill_root": legacy_distill_root,
        }),
        "foundry_worker",
        "memory_distill",
        Some(if item.target_db == DbScope::Project {
            "project"
        } else {
            "global"
        }),
        item.target_db,
        json!({
            "agent_id": agent_id,
            "path_prefix": item.path_prefix,
        }),
    );
    if let Some(db_path) = item.db_path.as_ref() {
        metadata = crate::provenance::restamp_provenance_for_destination(
            metadata,
            db_path,
            item.target_db,
        );
    }

    let distill_entry = MemoryEntry {
        id: memory_id.clone(),
        path: build_guide_distill_path(agent_id, guide_type, &timestamp_segment),
        summary: distill_text.chars().take(100).collect(),
        text: distill_text,
        importance: 0.75,
        timestamp,
        valid_from: String::new(),
        valid_until: None,
        category: "guide".to_string(),
        topic: guide_type.to_string(),
        keywords: dedup_strings({
            let mut kws = vec![
                "foundry".to_string(),
                "distill".to_string(),
                "guide".to_string(),
                guide_type.to_string(),
            ];
            for entry in &source_entries {
                kws.extend(entry.keywords.iter().cloned());
            }
            kws
        }),
        persons: Vec::new(),
        entities: dedup_strings(
            source_entries
                .iter()
                .flat_map(|entry| entry.entities.clone())
                .collect::<Vec<_>>(),
        ),
        location: item.path_prefix.clone(),
        source: FOUNDRY_DISTILL_SOURCE.to_string(),
        scope: if item.target_db == DbScope::Project {
            "project".to_string()
        } else {
            "global".to_string()
        },
        archived: false,
        access_count: 0,
        last_access: None,
        revision: 1,
        metadata,
        vector: None,
        retention_policy: None,
        domain: None,
    };

    with_foundry_store(server, item, |store| {
        store
            .upsert(&distill_entry)
            .map_err(|e| format!("Failed to save foundry distill memory: {e}"))
    })?;
    let edges = build_distill_edges(
        &distill_entry,
        &source_entries,
        guide_type,
        &distill_entry.timestamp,
    );
    with_foundry_store(server, item, |store| {
        for edge in &edges {
            store
                .add_edge(edge)
                .map_err(|e| format!("Failed to save foundry distill edge: {e}"))?;
        }
        Ok(())
    })?;
    queue_capture_enrichment(
        server,
        item.target_db,
        item.named_project.clone(),
        item.db_path.clone(),
        &distill_entry,
        false,
        None,
        None,
    );

    Ok(DistillOutcome::Wrote(memory_id))
}

fn process_forget_sweep_job(
    server: &MemoryServer,
    item: &FoundryMaintenanceItem,
) -> Result<usize, String> {
    let agent_id = item
        .job
        .target_agent_id
        .as_deref()
        .unwrap_or("unknown-agent");
    let distill_root = build_foundry_distill_root(agent_id);
    let distill_entries = with_foundry_store_read(server, item, |store| {
        store
            .list_by_path(&distill_root, FOUNDRY_DISTILL_KEEP + 12, false)
            .map_err(|e| format!("Failed to list foundry distill memories: {e}"))
    })?;

    let mut distill_entries = distill_entries
        .into_iter()
        .filter(|entry| entry.source == FOUNDRY_DISTILL_SOURCE)
        .collect::<Vec<_>>();
    distill_entries.sort_by(|a, b| {
        b.timestamp
            .cmp(&a.timestamp)
            .then_with(|| b.path.cmp(&a.path))
            .then_with(|| b.id.cmp(&a.id))
    });

    let stale_ids = distill_entries
        .into_iter()
        .skip(FOUNDRY_DISTILL_KEEP)
        .map(|entry| entry.id)
        .collect::<Vec<_>>();

    let mut archived = 0usize;
    for stale_id in stale_ids {
        let changed = with_foundry_store(server, item, |store| {
            store
                .archive_memory(&stale_id)
                .map_err(|e| format!("Failed to archive stale foundry distill {}: {e}", stale_id))
        })?;
        if changed {
            archived += 1;
        }
    }

    Ok(archived)
}

async fn handle_foundry_maintenance_item(
    server: &MemoryServer,
    item: &FoundryMaintenanceItem,
) -> Result<(memory_core::FoundryJobStatus, Option<String>), String> {
    let worker = foundry_worker_name(&item.job.kind);
    let event_hash = build_foundry_event_hash(server, item)?;
    let claimed = with_foundry_store(server, item, |store| {
        store
            .try_claim_event(&event_hash, &item.job.id, worker)
            .map_err(|e| format!("Failed to claim foundry job {}: {e}", item.job.id))
    })?;

    if !claimed {
        return Ok((
            memory_core::FoundryJobStatus::Skipped,
            Some("event_already_claimed".to_string()),
        ));
    }

    let result = match item.job.kind {
        memory_core::FoundryJobKind::MemoryNeighborhood => {
            process_memory_neighborhood_job(server, item)
                .await
                .map(|_| (memory_core::FoundryJobStatus::Completed, None))
        }
        memory_core::FoundryJobKind::RecallRerankCache => {
            process_recall_rerank_cache_job(server, item)
                .await
                .map(|_| (memory_core::FoundryJobStatus::Completed, None))
        }
        memory_core::FoundryJobKind::MemoryDistill => process_memory_distill_job(server, item)
            .await
            .map(|outcome| match outcome {
                DistillOutcome::Wrote(_) => (memory_core::FoundryJobStatus::Completed, None),
                DistillOutcome::Skipped(reason) => {
                    (memory_core::FoundryJobStatus::Skipped, Some(reason))
                }
            }),
        memory_core::FoundryJobKind::ForgetSweep => process_forget_sweep_job(server, item)
            .map(|_| (memory_core::FoundryJobStatus::Completed, None)),
        _ => Ok((
            memory_core::FoundryJobStatus::Skipped,
            Some("unknown_job_kind".to_string()),
        )),
    };

    if let Err(err) = &result {
        let _ = with_foundry_store(server, item, |store| {
            store
                .release_event_claim(&event_hash, worker)
                .map_err(|e| format!("Failed to release foundry job claim {}: {e}", item.job.id))
        });
        return Err(err.clone());
    }

    result
}

pub(crate) async fn run_foundry_maintenance_worker(
    server: MemoryServer,
    mut rx: mpsc::Receiver<FoundryMaintenanceItem>,
) {
    while let Some(item) = rx.recv().await {
        server
            .foundry_lock()
            .foundry_stats
            .queued
            .fetch_sub(1, Ordering::Relaxed);
        server
            .foundry_lock()
            .foundry_stats
            .running
            .fetch_add(1, Ordering::Relaxed);

        let result = handle_foundry_maintenance_item(&server, &item).await;

        server
            .foundry_lock()
            .foundry_stats
            .running
            .fetch_sub(1, Ordering::Relaxed);

        // Branch #5 + PR-C: capture a structured reason for non-completed
        // terminal transitions so `tachi doctor --jobs` and post-mortems
        // can surface *why* a job skipped/failed instead of just the bare
        // status. The reason is now returned in-band by
        // `handle_foundry_maintenance_item` (no global stash, no draining).
        let (status_str, reason): (&str, Option<String>) =
            match &result {
                Ok((memory_core::FoundryJobStatus::Skipped, reason)) => (
                    "skipped",
                    Some(reason.clone().unwrap_or_else(|| {
                        "worker reported no-op (no qualifying inputs)".to_string()
                    })),
                ),
                Ok((_, _)) => ("completed", None),
                Err(e) => ("failed", Some(e.clone())),
            };
        let _ = with_foundry_store(&server, &item, |store| {
            memory_core::update_foundry_job_status_with_reason(
                store.connection(),
                &item.job.id,
                status_str,
                reason.as_deref(),
            )
            .map_err(|e| format!("update foundry job status: {e}"))
        });

        match result {
            Ok((memory_core::FoundryJobStatus::Skipped, _)) => {
                server
                    .foundry_lock()
                    .foundry_stats
                    .skipped
                    .fetch_add(1, Ordering::Relaxed);
            }
            Ok(_) => {
                server
                    .foundry_lock()
                    .foundry_stats
                    .completed
                    .fetch_add(1, Ordering::Relaxed);
            }
            Err(err) => {
                tracing::warn!("[foundry-worker] job {} failed: {err}", item.job.id);
                server
                    .foundry_lock()
                    .foundry_stats
                    .failed
                    .fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    tracing::debug!("[foundry-worker] channel closed, worker exiting");
}

fn build_distill_input(entries: &[MemoryEntry]) -> String {
    entries
        .iter()
        .enumerate()
        .map(|(idx, entry)| {
            let summary = if entry.summary.trim().is_empty() {
                entry.text.chars().take(180).collect::<String>()
            } else {
                entry.summary.clone()
            };
            format!(
                "Memory {} | topic={} | importance={:.2}\nSummary: {}\nText: {}",
                idx + 1,
                if entry.topic.is_empty() {
                    "unknown"
                } else {
                    &entry.topic
                },
                entry.importance,
                summary,
                entry.text.chars().take(320).collect::<String>()
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

#[cfg(test)]
mod phase1_tests {
    use super::*;
    use tempfile::tempdir;

    /// Phase 1 regression: per-capture maintenance must not enqueue
    /// `MemoryDistill`. Distill now runs only via the daily batch
    /// scheduler (`run_daily_batch_distill`).
    #[tokio::test]
    async fn capture_specs_exclude_memory_distill() {
        let tmp = tempdir().expect("tempdir");
        let db_path = tmp.path().join("global.db");
        let server = crate::MemoryServer::new(db_path, None).expect("server");

        let memory_ids = vec!["m1".to_string(), "m2".to_string()];
        let specs = capture_maintenance_specs(&server, "agent", "/a/b", &memory_ids, 0, 0);

        let kinds: Vec<memory_core::FoundryJobKind> =
            specs.iter().map(|s| s.kind.clone()).collect();
        assert!(
            !kinds.contains(&memory_core::FoundryJobKind::MemoryDistill),
            "Phase 1: capture must not enqueue MemoryDistill (got {kinds:?})"
        );
        assert!(kinds.contains(&memory_core::FoundryJobKind::ForgetSweep));
        assert!(kinds.contains(&memory_core::FoundryJobKind::MemoryNeighborhood));
        assert!(kinds.contains(&memory_core::FoundryJobKind::RecallRerankCache));
    }
}
