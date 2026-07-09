//! Memory lifecycle consolidation loop (tachi#775 / #734-A1).
//!
//! Turns dry-run-only `tachi_memory(action="consolidate")` into a
//! **propose → review → apply** loop (mirror of `recall_proposals`):
//!
//! 1. **Propose** (default consolidate call): scan project (or global) scratch
//!    rows; emit durable proposals for `supersede` (same path, older→newer) and
//!    `archive` (stale low-value scratch). Always non-mutating until apply.
//! 2. **Review**: `proposal_id` + `review_status=approved|rejected`.
//! 3. **Apply**: `proposal_id` + `confirm=true` after approval; mutates via
//!    `supersede_memory` / `archive_memory` and keeps provenance.
//!
//! Protected rows (never auto-proposed for archive/supersede as *targets*):
//! permanent/pinned/durable retention, wiki paths/categories, pattern tier.

use super::evidence_format::{json_string, wants_json};
use crate::tool_params::TachiMemoryParams;
use crate::MemoryServer;
use chrono::{Duration, Utc};
use memory_core::MemoryEntry;
use serde_json::{json, Value};
use std::collections::HashMap;

const LIFECYCLE_PROPOSAL_NS: &str = "memory_lifecycle_proposals";
const SCRATCH_PREFIX: &str = "/scratch";
const STALE_DAYS_DEFAULT: i64 = 30;
const ARCHIVE_IMPORTANCE_MAX: f64 = 0.55;

pub(crate) async fn handle_memory_consolidate(
    server: &MemoryServer,
    params: &TachiMemoryParams,
) -> Result<String, String> {
    let has_proposal = params
        .proposal_id
        .as_deref()
        .is_some_and(|id| !id.trim().is_empty());
    if has_proposal {
        // Review path: proposal_id + review_status
        if params
            .review_status
            .as_deref()
            .is_some_and(|s| !s.trim().is_empty())
        {
            return handle_review(server, params);
        }
        // Apply path: proposal_id (+ confirm gate inside)
        return handle_apply(server, params);
    }
    // Default: propose + list (always dry_run for mutations)
    handle_propose(server, params).await
}

async fn handle_propose(
    server: &MemoryServer,
    params: &TachiMemoryParams,
) -> Result<String, String> {
    let path_prefix = params
        .path_prefix
        .clone()
        .unwrap_or_else(|| SCRATCH_PREFIX.to_string());
    let generated = generate_and_persist_proposals(server, params, &path_prefix)?;
    let proposals = list_proposals(server, params)?;
    let response = json!({
        "status": "dry_run",
        "action": "consolidate",
        "kind": "memory_lifecycle",
        "requires_human_approval": true,
        "path_prefix": path_prefix,
        "generated_count": generated.len(),
        "generated": generated,
        "count": proposals.len(),
        "proposals": proposals,
        "next_actions": [
            "tachi_memory(action='consolidate', proposal_id=..., review_status='approved')",
            "tachi_memory(action='consolidate', proposal_id=..., confirm=true) after approval"
        ],
        "note": "No rows mutated. Consolidation applies only after review + confirm=true.",
    });
    if wants_json(params.format.as_deref()) {
        return json_string(&response);
    }
    Ok(format!(
        "Tachi consolidate\nstatus: dry_run\ngenerated: {}\npending proposals: {}\nnext: review then confirm=true to apply",
        generated.len(),
        proposals
            .iter()
            .filter(|p| p.get("status").and_then(Value::as_str) == Some("pending"))
            .count()
    ))
}

fn handle_review(server: &MemoryServer, params: &TachiMemoryParams) -> Result<String, String> {
    let proposal_id = required_proposal_id(params)?;
    let status = match params
        .review_status
        .as_deref()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "approved" | "approve" => "approved",
        "rejected" | "reject" => "rejected",
        other => {
            return Err(format!(
                "Invalid review_status '{other}'. Expected approved|rejected"
            ))
        }
    };
    let reviewed_at = Utc::now().to_rfc3339();
    let updated = with_proposal_store(server, params, |store| {
        let (raw, _version) = store
            .get_state_kv(LIFECYCLE_PROPOSAL_NS, proposal_id)
            .map_err(|e| format!("load lifecycle proposal: {e}"))?
            .ok_or_else(|| format!("lifecycle proposal not found: {proposal_id}"))?;
        let mut value: Value =
            serde_json::from_str(&raw).map_err(|e| format!("parse lifecycle proposal: {e}"))?;
        value["status"] = json!(status);
        value["review"] = json!({
            "status": status,
            "note": params.notes.clone(),
            "reviewed_at": reviewed_at,
        });
        let next = serde_json::to_string(&value)
            .map_err(|e| format!("serialize lifecycle review: {e}"))?;
        store
            .set_state(LIFECYCLE_PROPOSAL_NS, proposal_id, &next)
            .map_err(|e| format!("persist lifecycle review: {e}"))?;
        Ok(value)
    })?;

    let response = json!({
        "status": "completed",
        "action": "consolidate",
        "sub_action": "review",
        "proposal_id": proposal_id,
        "proposal": updated,
    });
    if wants_json(params.format.as_deref()) {
        return json_string(&response);
    }
    Ok(format!(
        "Tachi consolidate review\nstatus: completed\nproposal_id: `{proposal_id}`\nreview_status: {status}"
    ))
}

fn handle_apply(server: &MemoryServer, params: &TachiMemoryParams) -> Result<String, String> {
    let proposal_id = required_proposal_id(params)?;
    if !params.confirm {
        return Err(
            "apply consolidate requires confirm=true after human approval; no memory rows mutated"
                .to_string(),
        );
    }

    let raw = with_proposal_store_read(server, params, |store| {
        store
            .get_state_kv(LIFECYCLE_PROPOSAL_NS, proposal_id)
            .map_err(|e| format!("load lifecycle proposal: {e}"))?
            .map(|(raw, _)| raw)
            .ok_or_else(|| format!("lifecycle proposal not found: {proposal_id}"))
    })?;
    let mut proposal: Value =
        serde_json::from_str(&raw).map_err(|e| format!("parse lifecycle proposal: {e}"))?;
    let status = proposal
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("pending");
    if status != "approved" {
        return Err(format!(
            "lifecycle proposal {proposal_id} must be approved before apply; current status={status}"
        ));
    }

    let action = proposal
        .get("lifecycle_action")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let source_id = proposal
        .get("source_id")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let target_id = proposal
        .get("target_id")
        .and_then(Value::as_str)
        .map(|s| s.to_string());

    if source_id.is_empty() {
        return Err(format!(
            "lifecycle proposal {proposal_id} missing source_id"
        ));
    }

    let apply_result = apply_lifecycle_action(server, params, &action, &source_id, target_id.as_deref())?;
    let applied_at = Utc::now().to_rfc3339();
    proposal["status"] = json!("applied");
    proposal["applied_at"] = json!(applied_at);
    proposal["apply_result"] = apply_result.clone();
    let proposal_for_store = proposal.clone();
    with_proposal_store(server, params, |store| {
        let next = serde_json::to_string(&proposal_for_store)
            .map_err(|e| format!("serialize applied lifecycle proposal: {e}"))?;
        store
            .set_state(LIFECYCLE_PROPOSAL_NS, proposal_id, &next)
            .map_err(|e| format!("persist applied lifecycle proposal: {e}"))
    })?;

    let response = json!({
        "status": "completed",
        "action": "consolidate",
        "sub_action": "apply",
        "proposal_id": proposal_id,
        "apply_result": apply_result,
        "proposal": proposal,
    });
    if wants_json(params.format.as_deref()) {
        return json_string(&response);
    }
    Ok(format!(
        "Tachi consolidate apply\nstatus: completed\nproposal_id: `{proposal_id}`\nlifecycle_action: {action}"
    ))
}

fn apply_lifecycle_action(
    server: &MemoryServer,
    params: &TachiMemoryParams,
    action: &str,
    source_id: &str,
    target_id: Option<&str>,
) -> Result<Value, String> {
    match action {
        "supersede" => {
            let target = target_id.ok_or_else(|| {
                "supersede proposal requires target_id (canonical survivor)".to_string()
            })?;
            with_memory_store(server, params, |store| {
                // Refuse protected sources.
                if let Some(entry) = store
                    .get(source_id)
                    .map_err(|e| format!("load source: {e}"))?
                {
                    if is_protected(&entry) {
                        return Err(format!(
                            "refusing to supersede protected memory {source_id} (retention/wiki/pattern)"
                        ));
                    }
                }
                let changed = store
                    .supersede_memory(source_id, target)
                    .map_err(|e| format!("supersede_memory: {e}"))?;
                let archived = store
                    .archive_memory(source_id)
                    .map_err(|e| format!("archive after supersede: {e}"))?;
                Ok(json!({
                    "lifecycle_action": "supersede",
                    "source_id": source_id,
                    "target_id": target,
                    "superseded": changed,
                    "archived": archived,
                }))
            })
        }
        "archive" => with_memory_store(server, params, |store| {
            if let Some(entry) = store
                .get(source_id)
                .map_err(|e| format!("load source: {e}"))?
            {
                if is_protected(&entry) {
                    return Err(format!(
                        "refusing to archive protected memory {source_id} (retention/wiki/pattern)"
                    ));
                }
            }
            let archived = store
                .archive_memory(source_id)
                .map_err(|e| format!("archive_memory: {e}"))?;
            Ok(json!({
                "lifecycle_action": "archive",
                "source_id": source_id,
                "archived": archived,
            }))
        }),
        other => Err(format!(
            "unsupported lifecycle_action '{other}'; expected supersede|archive"
        )),
    }
}

fn generate_and_persist_proposals(
    server: &MemoryServer,
    params: &TachiMemoryParams,
    path_prefix: &str,
) -> Result<Vec<Value>, String> {
    let entries = with_memory_store_read(server, params, |store| {
        store
            .list_by_path(path_prefix, 500, false)
            .map_err(|e| format!("list_by_path: {e}"))
    })?;

    let mut proposals = Vec::new();
    proposals.extend(propose_same_path_supersedes(&entries, path_prefix));
    proposals.extend(propose_stale_archives(&entries, path_prefix));

    if proposals.is_empty() {
        return Ok(proposals);
    }

    with_proposal_store(server, params, |store| {
        for proposal in &proposals {
            let id = proposal["proposal_id"]
                .as_str()
                .ok_or_else(|| "proposal missing proposal_id".to_string())?;
            // Do not clobber approved/applied/rejected.
            if let Some((existing, _)) = store
                .get_state_kv(LIFECYCLE_PROPOSAL_NS, id)
                .map_err(|e| format!("load existing proposal: {e}"))?
            {
                if let Ok(existing_json) = serde_json::from_str::<Value>(&existing) {
                    let status = existing_json
                        .get("status")
                        .and_then(Value::as_str)
                        .unwrap_or("pending");
                    if status != "pending" {
                        continue;
                    }
                }
            }
            let raw = serde_json::to_string(proposal)
                .map_err(|e| format!("serialize proposal: {e}"))?;
            store
                .set_state(LIFECYCLE_PROPOSAL_NS, id, &raw)
                .map_err(|e| format!("persist proposal: {e}"))?;
        }
        Ok(())
    })?;
    Ok(proposals)
}

fn propose_same_path_supersedes(entries: &[MemoryEntry], path_prefix: &str) -> Vec<Value> {
    let mut by_path: HashMap<String, Vec<&MemoryEntry>> = HashMap::new();
    for entry in entries {
        if is_protected(entry) {
            continue;
        }
        if !entry.path.starts_with(path_prefix) {
            continue;
        }
        by_path.entry(entry.path.clone()).or_default().push(entry);
    }

    let mut out = Vec::new();
    for (path, mut group) in by_path {
        if group.len() < 2 {
            continue;
        }
        // Prefer parsed timestamps so Z vs +00:00 offsets do not invert order.
        group.sort_by(|a, b| {
            cmp_entry_timestamp_desc(a, b).then_with(|| a.id.cmp(&b.id))
        });
        let survivor = group[0];
        for older in group.iter().skip(1) {
            if older.id == survivor.id {
                continue;
            }
            let id = format!(
                "lifecycle:supersede:{}:{}",
                id_prefix(&older.id),
                id_prefix(&survivor.id)
            );
            out.push(json!({
                "proposal_id": id,
                "kind": "memory_lifecycle",
                "lifecycle_action": "supersede",
                "status": "pending",
                "requires_human_approval": true,
                "created_or_refreshed_at": Utc::now().to_rfc3339(),
                "source_id": older.id,
                "target_id": survivor.id,
                "path": path,
                "rationale": format!(
                    "Same path `{}` has multiple active rows; supersede older `{}` with newer `{}`.",
                    path, older.id, survivor.id
                ),
                "evidence": {
                    "source_timestamp": older.timestamp,
                    "target_timestamp": survivor.timestamp,
                    "source_summary": older.summary,
                    "target_summary": survivor.summary,
                    "source_access_count": older.access_count,
                    "target_access_count": survivor.access_count,
                },
            }));
        }
    }
    out
}

fn propose_stale_archives(entries: &[MemoryEntry], path_prefix: &str) -> Vec<Value> {
    let cutoff = Utc::now() - Duration::days(STALE_DAYS_DEFAULT);
    let mut out = Vec::new();
    for entry in entries {
        if is_protected(entry) {
            continue;
        }
        if !entry.path.starts_with(path_prefix) {
            continue;
        }
        if entry.importance > ARCHIVE_IMPORTANCE_MAX {
            continue;
        }
        if entry.access_count > 0 || entry.recall_count > 0 {
            continue;
        }
        if !entry_timestamp_before(entry, cutoff) {
            continue;
        }
        let id = format!("lifecycle:archive:{}", id_prefix(&entry.id));
        out.push(json!({
            "proposal_id": id,
            "kind": "memory_lifecycle",
            "lifecycle_action": "archive",
            "status": "pending",
            "requires_human_approval": true,
            "created_or_refreshed_at": Utc::now().to_rfc3339(),
            "source_id": entry.id,
            "target_id": Value::Null,
            "path": entry.path,
            "rationale": format!(
                "Stale low-value row `{}` (importance={}, access=0, older than {STALE_DAYS_DEFAULT}d).",
                entry.id, entry.importance
            ),
            "evidence": {
                "timestamp": entry.timestamp,
                "importance": entry.importance,
                "access_count": entry.access_count,
                "recall_count": entry.recall_count,
                "summary": entry.summary,
                "retention_policy": entry.retention_policy,
            },
        }));
    }
    out
}

/// Safe 8-char prefix for proposal ids (char-based; never panics on non-ASCII ids).
fn id_prefix(id: &str) -> String {
    id.chars().take(8).collect()
}

fn cmp_entry_timestamp_desc(a: &MemoryEntry, b: &MemoryEntry) -> std::cmp::Ordering {
    match (parse_entry_utc(&a.timestamp), parse_entry_utc(&b.timestamp)) {
        (Some(ta), Some(tb)) => tb.cmp(&ta),
        _ => b.timestamp.cmp(&a.timestamp),
    }
}

fn entry_timestamp_before(entry: &MemoryEntry, cutoff: chrono::DateTime<Utc>) -> bool {
    match parse_entry_utc(&entry.timestamp) {
        Some(ts) => ts < cutoff,
        // Fall back to lexicographic RFC3339 when parse fails (legacy/odd rows).
        None => entry.timestamp < cutoff.to_rfc3339(),
    }
}

fn parse_entry_utc(raw: &str) -> Option<chrono::DateTime<Utc>> {
    chrono::DateTime::parse_from_rfc3339(raw)
        .ok()
        .map(|dt| dt.with_timezone(&Utc))
}

fn is_protected(entry: &MemoryEntry) -> bool {
    if entry.archived {
        return true;
    }
    if entry.tier.eq_ignore_ascii_case("pattern") {
        return true;
    }
    if entry.is_wiki() || entry.path.starts_with("/wiki") {
        return true;
    }
    matches!(
        entry
            .retention_policy
            .as_deref()
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some("permanent" | "pinned" | "durable")
    )
}

fn list_proposals(server: &MemoryServer, params: &TachiMemoryParams) -> Result<Vec<Value>, String> {
    // Same store resolution as propose/review/apply so named `project=` pins
    // see the proposals they just generated (Gemini #904 review).
    let rows = with_proposal_store_read(server, params, |store| {
        store
            .list_state(LIFECYCLE_PROPOSAL_NS)
            .map_err(|e| format!("list lifecycle proposals: {e}"))
    })?;

    let filter = params
        .state_filter
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_ascii_lowercase());
    let mut out = Vec::new();
    for row in rows {
        let Ok(value) = serde_json::from_str::<Value>(&row.value_json) else {
            continue;
        };
        if let Some(ref want) = filter {
            let status = value
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or("pending")
                .to_ascii_lowercase();
            if &status != want {
                continue;
            }
        }
        out.push(value);
    }
    Ok(out)
}

fn required_proposal_id(params: &TachiMemoryParams) -> Result<&str, String> {
    params
        .proposal_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "proposal_id is required".to_string())
}

fn with_proposal_store<T>(
    server: &MemoryServer,
    params: &TachiMemoryParams,
    f: impl FnOnce(&mut memory_core::MemoryStore) -> Result<T, String>,
) -> Result<T, String> {
    // Named project pin first; else project DB; else global.
    if let Some(name) = params.project.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        return server.with_named_project_store(name, f);
    }
    if server.has_project_db() {
        return server.with_project_store(f);
    }
    server.with_global_store(f)
}

fn with_proposal_store_read<T>(
    server: &MemoryServer,
    params: &TachiMemoryParams,
    f: impl FnOnce(&mut memory_core::MemoryStore) -> Result<T, String>,
) -> Result<T, String> {
    // *_store_read still takes &mut MemoryStore (shared lock style).
    if let Some(name) = params.project.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        return server.with_named_project_store_read(name, f);
    }
    if server.has_project_db() {
        return server.with_project_store_read(f);
    }
    server.with_global_store_read(f)
}

fn with_memory_store<T>(
    server: &MemoryServer,
    params: &TachiMemoryParams,
    f: impl FnOnce(&mut memory_core::MemoryStore) -> Result<T, String>,
) -> Result<T, String> {
    with_proposal_store(server, params, f)
}

fn with_memory_store_read<T>(
    server: &MemoryServer,
    params: &TachiMemoryParams,
    f: impl FnOnce(&mut memory_core::MemoryStore) -> Result<T, String>,
) -> Result<T, String> {
    with_proposal_store_read(server, params, f)
}
