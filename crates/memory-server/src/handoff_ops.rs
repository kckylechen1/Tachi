use super::*;
use crate::gh_ops::CliGhClient;
use crate::gh_safe_merge::GhClient;
use crate::shell_ops::{append_github_event, merge_github_status, run_dir_for_flow_id};

const HANDOFF_PATH: &str = "/handoff";
const HANDOFF_MEMORY_LIMIT: usize = 50;
const HANDOFF_DB_LIMIT: usize = 500;
fn non_empty_env(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn current_agent_id(server: &MemoryServer) -> Option<String> {
    let guard = server.agent_runtime_read();
    guard
        .agent_profile
        .as_ref()
        .map(|profile| profile.agent_id.trim().to_string())
        .filter(|agent_id| !agent_id.is_empty())
}

fn fallback_agent_id(registered_agent: Option<String>) -> String {
    registered_agent
        .or_else(|| non_empty_env("TACHI_PROFILE"))
        .unwrap_or_else(|| "unknown-agent".to_string())
}

fn resolve_from_agent(server: &MemoryServer) -> String {
    fallback_agent_id(current_agent_id(server))
}

fn memo_matches_agent(memo: &HandoffMemo, agent_id: Option<&str>) -> bool {
    if memo.acknowledged {
        return false;
    }

    match (agent_id, memo.target_agent.as_deref()) {
        (_, None) => true,
        (Some(my_id), Some(target)) => my_id == target,
        (None, Some(_)) => true,
    }
}

fn memo_to_memory_entry(server: &MemoryServer, memo: &HandoffMemo) -> MemoryEntry {
    let memo_id = memo.id.clone();
    let mut metadata = crate::provenance::inject_provenance(
        server,
        serde_json::json!({
            "handoff_memo_id": memo_id,
            "handoff": memo,
            "status": "pending",
        }),
        "handoff_leave",
        "handoff_memo",
        Some("general"),
        DbScope::Global,
        serde_json::json!({
            "from_agent": memo.from_agent.clone(),
            "target_agent": memo.target_agent.clone(),
            "next_steps_count": memo.next_steps.len(),
        }),
    );
    // Handoff lives in the global DB but uses a non-/global path prefix; opt
    // in to cross-project routing so path-routing validation lets it through.
    if let Some(obj) = metadata.as_object_mut() {
        obj.insert(
            "allow_cross_project".to_string(),
            serde_json::Value::Bool(true),
        );
    }

    let routed_path =
        memory_core::path_router::standardize_handoff_path(memo.target_agent.as_deref());

    MemoryEntry {
        id: format!("handoff:{}", memo_id),
        text: format!(
            "[Handoff from {}] {}\n\nNext steps:\n{}",
            memo.from_agent,
            memo.summary,
            memo.next_steps
                .iter()
                .enumerate()
                .map(|(i, s)| format!("{}. {}", i + 1, s))
                .collect::<Vec<_>>()
                .join("\n")
        ),
        category: "handoff".to_string(),
        importance: 0.75,
        summary: format!("Handoff from {}", memo.from_agent),
        path: routed_path,
        timestamp: memo.created_at.clone(),
        valid_from: String::new(),
        valid_until: None,
        topic: "agent-handoff".to_string(),
        keywords: vec!["handoff".to_string(), memo.from_agent.clone()],
        persons: vec![],
        entities: vec![memo.from_agent.clone()],
        location: String::new(),
        source: "extraction".to_string(),
        scope: "general".to_string(),
        archived: false,
        access_count: 0,
        last_access: None,
        revision: 1,
        vector: None,
        metadata,
        retention_policy: Some(memory_core::RetentionPolicy::Pinned.as_str().to_string()),
        domain: None,
        recall_count: 0,
        query_diversity: 0,
        tier: "raw".to_string(),
    }
}

fn memo_from_entry(entry: &MemoryEntry) -> HandoffMemo {
    if let Some(memo) = entry
        .metadata
        .get("handoff")
        .and_then(|value| serde_json::from_value::<HandoffMemo>(value.clone()).ok())
    {
        let acknowledged = handoff_acknowledged(entry, &memo);
        return HandoffMemo {
            acknowledged,
            ..memo
        };
    }

    let metadata = entry.metadata.as_object();
    let from_agent = metadata
        .and_then(|m| m.get("provenance"))
        .and_then(|p| p.get("context"))
        .and_then(|c| c.get("from_agent"))
        .and_then(|v| v.as_str())
        .or_else(|| entry.entities.first().map(String::as_str))
        .unwrap_or("unknown-agent")
        .to_string();
    let target_agent = metadata
        .and_then(|m| m.get("provenance"))
        .and_then(|p| p.get("context"))
        .and_then(|c| c.get("target_agent"))
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let memo_id = metadata
        .and_then(|m| m.get("handoff_memo_id"))
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .or_else(|| entry.id.strip_prefix("handoff:").map(str::to_string))
        .unwrap_or_else(|| entry.id.clone());

    HandoffMemo {
        id: memo_id,
        from_agent,
        target_agent,
        summary: legacy_summary_from_entry(entry),
        next_steps: legacy_next_steps_from_entry(entry),
        context: None,
        created_at: entry.timestamp.clone(),
        acknowledged: handoff_acknowledged(entry, &empty_handoff_memo()),
    }
}

fn legacy_summary_from_entry(entry: &MemoryEntry) -> String {
    entry
        .text
        .strip_prefix("[Handoff from ")
        .and_then(|rest| rest.split_once("] "))
        .map(|(_, summary_and_steps)| {
            summary_and_steps
                .split_once("\n\nNext steps:")
                .map(|(summary, _)| summary)
                .unwrap_or(summary_and_steps)
                .trim()
                .to_string()
        })
        .filter(|summary| !summary.is_empty())
        .unwrap_or_else(|| entry.summary.clone())
}

fn legacy_next_steps_from_entry(entry: &MemoryEntry) -> Vec<String> {
    let Some((_, raw_steps)) = entry.text.split_once("\n\nNext steps:") else {
        return Vec::new();
    };

    raw_steps
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(|line| {
            let Some((prefix, step)) = line.split_once(". ") else {
                return line.to_string();
            };
            if prefix.chars().all(|c| c.is_ascii_digit()) {
                step.trim().to_string()
            } else {
                line.to_string()
            }
        })
        .collect()
}

fn empty_handoff_memo() -> HandoffMemo {
    HandoffMemo {
        id: String::new(),
        from_agent: String::new(),
        target_agent: None,
        summary: String::new(),
        next_steps: Vec::new(),
        context: None,
        created_at: String::new(),
        acknowledged: false,
    }
}

fn handoff_acknowledged(entry: &MemoryEntry, memo: &HandoffMemo) -> bool {
    memo.acknowledged
        || entry.archived
        || entry
            .metadata
            .get("status")
            .and_then(|value| value.as_str())
            .is_some_and(|status| matches!(status, "acknowledged" | "promoted"))
        || entry
            .metadata
            .get("acknowledged")
            .and_then(|value| value.as_bool())
            .unwrap_or(false)
}

/// Pending cross-project handoff memos in global DB (session briefing feed).
pub(crate) fn list_pending_handoffs_for_briefing(
    server: &MemoryServer,
    limit: usize,
) -> Result<Vec<serde_json::Value>, String> {
    let limit = limit.max(1).min(12);
    let entries = server.with_global_store_read(pending_handoff_entries)?;
    let rows = entries
        .into_iter()
        .take(limit)
        .map(|entry| {
            let memo = memo_from_entry(&entry);
            serde_json::json!({
                "id": entry.id,
                "path": entry.path,
                "from_agent": memo.from_agent,
                "target_agent": memo.target_agent,
                "summary": memo.summary,
                "next_steps": memo.next_steps,
                "created_at": memo.created_at,
                "kind": "handoff",
            })
        })
        .collect();
    Ok(rows)
}

fn pending_handoff_entries(store: &mut MemoryStore) -> Result<Vec<MemoryEntry>, String> {
    let mut entries = store
        .list_by_path(HANDOFF_PATH, HANDOFF_DB_LIMIT, false)
        .map_err(|e| format!("Failed to list handoff memories: {e}"))?;
    entries.retain(|entry| entry.category == "handoff" && !memo_from_entry(entry).acknowledged);
    entries.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
    Ok(entries)
}

fn upsert_acknowledged_entry(
    store: &mut MemoryStore,
    mut entry: MemoryEntry,
    agent_id: Option<&str>,
) -> Result<(), String> {
    let acknowledged_at = Utc::now().to_rfc3339();
    let mut memo = memo_from_entry(&entry);
    memo.acknowledged = true;

    let metadata = entry
        .metadata
        .as_object_mut()
        .ok_or_else(|| "handoff metadata must be an object".to_string())?;
    metadata.insert("handoff".into(), json!(memo));
    metadata.insert("status".into(), json!("acknowledged"));
    metadata.insert("acknowledged".into(), json!(true));
    metadata.insert("acknowledged_at".into(), json!(acknowledged_at));
    if let Some(agent_id) = agent_id.filter(|value| !value.trim().is_empty()) {
        metadata.insert("acknowledged_by".into(), json!(agent_id));
    }

    entry.vector = None;
    store
        .upsert(&entry)
        .map_err(|e| format!("Failed to acknowledge handoff memory: {e}"))
}

fn normalize_handoff_memo_id(id: &str) -> String {
    id.strip_prefix("handoff:").unwrap_or(id).to_string()
}

fn issue_number_from_url(url: &str) -> Option<u64> {
    url.trim_end_matches('/')
        .rsplit('/')
        .next()
        .and_then(|value| value.parse::<u64>().ok())
}

fn handoff_issue_body(memo: &HandoffMemo) -> String {
    let steps = if memo.next_steps.is_empty() {
        "_No next steps supplied._".to_string()
    } else {
        memo.next_steps
            .iter()
            .map(|step| format!("- [ ] {step}"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let context = memo
        .context
        .as_ref()
        .map(|value| serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string()))
        .unwrap_or_else(|| "null".to_string());
    format!(
        "## Handoff summary\n{}\n\n## Next steps\n{}\n\n## Context\n```json\n{}\n```\n\nSource: `handoff:{}` from `{}` at `{}`.",
        memo.summary, steps, context, memo.id, memo.from_agent, memo.created_at
    )
}

fn upsert_promoted_entry(
    store: &mut MemoryStore,
    mut entry: MemoryEntry,
    issue: serde_json::Value,
) -> Result<(), String> {
    let mut memo = memo_from_entry(&entry);
    memo.acknowledged = true;
    let metadata = entry
        .metadata
        .as_object_mut()
        .ok_or_else(|| "handoff metadata must be an object".to_string())?;
    metadata.insert("status".into(), json!("promoted"));
    metadata.insert("github".into(), issue);
    metadata.insert("promoted".into(), json!(true));
    metadata.insert("acknowledged".into(), json!(true));
    metadata.insert("handoff".into(), json!(memo));
    entry.retention_policy = Some(memory_core::RetentionPolicy::Pinned.as_str().to_string());
    entry.vector = None;
    store
        .upsert(&entry)
        .map_err(|e| format!("Failed to update promoted handoff memory: {e}"))
}

fn supersede_pending_handoffs(
    store: &mut MemoryStore,
    memo: &HandoffMemo,
    entry: &MemoryEntry,
) -> Result<(), String> {
    let pending = pending_handoff_entries(store)?;
    for old_entry in pending {
        let old_memo = memo_from_entry(&old_entry);
        if old_memo.from_agent == memo.from_agent && old_memo.target_agent == memo.target_agent {
            let mut old_entry_mut = old_entry.clone();
            old_entry_mut.archived = true;
            old_entry_mut.vector = None;
            if let Some(obj) = old_entry_mut.metadata.as_object_mut() {
                obj.insert(
                    "status".to_string(),
                    serde_json::Value::String("superseded".to_string()),
                );
            }
            store.upsert(&old_entry_mut).map_err(|e| format!("{e}"))?;
            store
                .supersede_memory(&old_entry.id, &entry.id)
                .map_err(|e| format!("{e}"))?;
        }
    }
    Ok(())
}

pub(crate) async fn handle_handoff_leave(
    server: &MemoryServer,
    params: HandoffLeaveParams,
) -> Result<String, String> {
    let from_agent = resolve_from_agent(server);

    let memo = HandoffMemo {
        id: uuid::Uuid::new_v4().to_string(),
        from_agent: from_agent.clone(),
        target_agent: params.target_agent,
        summary: params.summary,
        next_steps: params.next_steps,
        context: params.context,
        created_at: Utc::now().to_rfc3339(),
        acknowledged: false,
    };

    let memo_id = memo.id.clone();
    let memo_json = serde_json::to_string(&memo).map_err(|e| format!("serialize: {e}"))?;
    let entry = memo_to_memory_entry(server, &memo);

    server.with_global_store(|store| {
        supersede_pending_handoffs(store, &memo, &entry)?;
        store.upsert(&entry).map_err(|e| format!("{e}"))
    })?;

    let mut memos = server.agent_runtime_write();
    memos.handoff_memos.push(memo);

    if memos.handoff_memos.len() > HANDOFF_MEMORY_LIMIT {
        let drain_count = memos.handoff_memos.len() - HANDOFF_MEMORY_LIMIT;
        memos.handoff_memos.drain(..drain_count);
    }

    serde_json::to_string(&json!({
        "status": "memo_left",
        "memo_id": memo_id,
        "from_agent": from_agent,
        "memo": memo_json,
    }))
    .map_err(|e| format!("serialize: {e}"))
}

pub(crate) async fn handle_handoff_check(
    server: &MemoryServer,
    params: HandoffCheckParams,
) -> Result<String, String> {
    let agent_id = params.agent_id.as_deref();
    let entries = server.with_global_store_read(pending_handoff_entries)?;
    let matching_entries: Vec<MemoryEntry> = entries
        .into_iter()
        .filter(|entry| memo_matches_agent(&memo_from_entry(entry), agent_id))
        .collect();
    let matching: Vec<HandoffMemo> = matching_entries.iter().map(memo_from_entry).collect();

    let result = serde_json::to_string(&json!({
        "pending_memos": matching.len(),
        "memos": matching,
    }))
    .map_err(|e| format!("serialize: {e}"))?;

    if params.acknowledge {
        for entry in matching_entries {
            server.with_global_store(|store| upsert_acknowledged_entry(store, entry, agent_id))?;
        }

        let mut rt = server.agent_runtime_write();
        for memo in rt.handoff_memos.iter_mut() {
            if memo_matches_agent(memo, agent_id) {
                memo.acknowledged = true;
            }
        }
    }

    Ok(result)
}

fn existing_issue_url(entry: &MemoryEntry) -> Option<String> {
    entry
        .metadata
        .get("github")
        .and_then(|gh| gh.get("issue_url"))
        .and_then(|v| v.as_str())
        .filter(|url| !url.is_empty())
        .map(str::to_string)
}

fn upsert_promoting_entry(store: &mut MemoryStore, mut entry: MemoryEntry) -> Result<(), String> {
    let metadata = entry
        .metadata
        .as_object_mut()
        .ok_or_else(|| "handoff metadata must be an object".to_string())?;
    if let Some(status) = metadata.get("status").and_then(|v| v.as_str()) {
        if matches!(status, "promoting" | "promoted") {
            return Err(format!("handoff already {status}"));
        }
    }
    metadata.insert("status".into(), json!("promoting"));
    metadata.insert("promoting_at".into(), json!(Utc::now().to_rfc3339()));
    entry.vector = None;
    store
        .upsert(&entry)
        .map_err(|e| format!("Failed to mark handoff as promoting: {e}"))
}

fn revert_promoting_entry(
    store: &mut MemoryStore,
    mut entry: MemoryEntry,
    previous_status: &str,
) -> Result<(), String> {
    let metadata = entry
        .metadata
        .as_object_mut()
        .ok_or_else(|| "handoff metadata must be an object".to_string())?;
    metadata.insert("status".into(), json!(previous_status));
    metadata.remove("promoting_at");
    entry.vector = None;
    store
        .upsert(&entry)
        .map_err(|e| format!("Failed to revert promoting status: {e}"))
}

fn repair_handoff_flow_artifact(
    flow_artifact: Option<&(String, std::path::PathBuf)>,
    entry: &MemoryEntry,
    entry_id: &str,
) -> Result<serde_json::Value, String> {
    let Some((flow_id, run_dir)) = flow_artifact else {
        return Ok(json!(null));
    };
    let Some(github) = entry.metadata.get("github").cloned() else {
        return Ok(json!({
            "status": "skipped",
            "reason": "missing_github_metadata",
        }));
    };

    let patch = json!({ "issue": github });
    let status_patch = merge_github_status(run_dir, patch)?;
    append_github_event(
        run_dir,
        flow_id,
        "github_issue_linked",
        json!({
            "repo": github["repo"],
            "issue_number": github["issue_number"],
            "issue_url": github["issue_url"],
            "source": github.get("source").cloned().unwrap_or_else(|| json!({
                "kind": "handoff",
                "entry_id": entry_id,
            })),
        }),
    )?;

    Ok(json!({
        "status": "repaired",
        "flow_id": flow_id,
        "status_patch": status_patch,
        "event_persisted": true,
    }))
}

async fn promote_handoff_issue_with_client<C: GhClient>(
    server: &MemoryServer,
    client: &C,
    params: HandoffPromoteIssueParams,
) -> Result<String, String> {
    let memo_id = normalize_handoff_memo_id(&params.memo_id);
    let entry_id = format!("handoff:{memo_id}");
    let flow_artifact = match params
        .flow_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        Some(flow_id) => Some((flow_id.to_string(), run_dir_for_flow_id(flow_id)?)),
        None => None,
    };
    let entry = server.with_global_store_read(|store| {
        store
            .get(&entry_id)
            .map_err(|e| format!("Failed to read handoff memory: {e}"))?
            .ok_or_else(|| format!("Handoff memo not found: {memo_id}"))
    })?;

    // ── Dedup: return existing issue link unless force=true ──────────
    if !params.force {
        if let Some(url) = existing_issue_url(&entry) {
            let issue_number = entry
                .metadata
                .get("github")
                .and_then(|gh| gh.get("issue_number"))
                .cloned();
            let repair = repair_handoff_flow_artifact(flow_artifact.as_ref(), &entry, &entry_id)?;
            return serde_json::to_string(&json!({
                "status": "already_promoted",
                "memo_id": memo_id,
                "entry_id": entry_id,
                "issue_url": url,
                "issue_number": issue_number,
                "artifact_repair": repair,
                "hint": "Pass force=true to create a new issue anyway.",
            }))
            .map_err(|e| format!("serialize: {e}"));
        }
    }

    // ── Mark promoting (intermediate state before GitHub call) ───────
    let previous_status = entry
        .metadata
        .get("status")
        .and_then(|v| v.as_str())
        .unwrap_or("pending")
        .to_string();
    if let Some((latest, url)) = server.with_global_store(|store| {
        let latest = store
            .get(&entry_id)
            .map_err(|e| format!("Failed to read handoff memory: {e}"))?
            .ok_or_else(|| format!("Handoff memo not found: {memo_id}"))?;
        if !params.force {
            if let Some(url) = existing_issue_url(&latest) {
                return Ok(Some((latest, url)));
            }
        }
        if params.force {
            let mut force_entry = latest;
            let metadata = force_entry
                .metadata
                .as_object_mut()
                .ok_or_else(|| "handoff metadata must be an object".to_string())?;
            metadata.insert("status".into(), json!("promoting"));
            metadata.insert("promoting_at".into(), json!(Utc::now().to_rfc3339()));
            force_entry.vector = None;
            store
                .upsert(&force_entry)
                .map_err(|e| format!("Failed to mark handoff as promoting: {e}"))?;
        } else {
            upsert_promoting_entry(store, latest)?;
        }
        Ok(None)
    })? {
        let issue_number = latest
            .metadata
            .get("github")
            .and_then(|gh| gh.get("issue_number"))
            .cloned();
        let repair = repair_handoff_flow_artifact(flow_artifact.as_ref(), &latest, &entry_id)?;
        return serde_json::to_string(&json!({
            "status": "already_promoted",
            "memo_id": memo_id,
            "entry_id": entry_id,
            "issue_url": url,
            "issue_number": issue_number,
            "artifact_repair": repair,
            "hint": "Pass force=true to create a new issue anyway.",
        }))
        .map_err(|e| format!("serialize: {e}"));
    }

    let memo = memo_from_entry(&entry);
    let title = params
        .title
        .clone()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| memo.summary.chars().take(120).collect());
    if title.trim().is_empty() {
        return Err("handoff summary or issue title must be non-empty".to_string());
    }
    let labels = if params.labels.is_empty() {
        vec!["handoff".to_string()]
    } else {
        params.labels.clone()
    };
    let body = handoff_issue_body(&memo);

    // ── Create GitHub issue ─────────────────────────────────────────
    let issue = match client
        .issue_create(&params.repo, &title, Some(&body), &labels)
        .await
    {
        Ok(issue) => issue,
        Err(gh_err) => {
            // Revert promoting state so the memo is not stuck
            let revert_entry = server
                .with_global_store_read(|store| {
                    store
                        .get(&entry_id)
                        .map_err(|e| e.to_string())?
                        .ok_or_else(|| "entry disappeared during promote".to_string())
                })
                .unwrap_or(entry.clone());
            if let Err(error) = server.with_global_store(|store| {
                revert_promoting_entry(store, revert_entry, &previous_status)
            }) {
                tracing::warn!(
                    memo_id = %params.memo_id,
                    error = %error,
                    "failed to revert handoff promoting state after GitHub issue creation failed"
                );
            }
            return Err(format!("GitHub issue creation failed: {gh_err}"));
        }
    };

    let issue_number = if issue.number == 0 {
        issue_number_from_url(&issue.url)
    } else {
        Some(issue.number)
    };
    let promoted_at = Utc::now().to_rfc3339();
    let promoted_by = resolve_from_agent(server);
    let github = json!({
        "repo": params.repo,
        "issue_number": issue_number,
        "issue_url": issue.url,
        "issue_title": issue.title,
        "issue_state": issue.state,
        "labels": labels,
        "promoted_at": promoted_at,
        "promoted_by": promoted_by,
        "source": {
            "kind": "handoff",
            "memo_id": memo.id,
            "entry_id": entry_id.clone(),
        },
    });

    // ── Persist promoted status back to memory ──────────────────────
    // Re-read entry (it was updated to "promoting" earlier)
    let promoted_entry = server
        .with_global_store_read(|store| {
            store
                .get(&entry_id)
                .map_err(|e| e.to_string())?
                .ok_or_else(|| "entry disappeared during promote".to_string())
        })
        .unwrap_or(entry.clone());
    if let Err(write_err) = server
        .with_global_store(|store| upsert_promoted_entry(store, promoted_entry, github.clone()))
    {
        // GitHub issue was created but memory write-back failed.
        // Return a recovery payload so the caller can retry or manually reconcile.
        return serde_json::to_string(&json!({
            "status": "partial_failure",
            "memo_id": memo_id,
            "entry_id": entry_id,
            "issue_url": issue.url,
            "issue_number": issue_number,
            "github": github,
            "error": format!("GitHub issue created but memory write-back failed: {write_err}"),
            "recovery": {
                "action": "promote_issue",
                "memo_id": memo_id,
                "force": true,
                "hint": "Re-run with force=true to overwrite the stale promoting state.",
            },
        }))
        .map_err(|e| format!("serialize: {e}"));
    }

    let mut status_patch = None;
    let mut event_persisted = false;
    if let Some((flow_id, run_dir)) = flow_artifact {
        let patch = json!({
            "issue": github,
        });
        status_patch = match merge_github_status(&run_dir, patch) {
            Ok(patch) => Some(patch),
            Err(write_err) => {
                return serde_json::to_string(&json!({
                    "status": "partial_failure",
                    "memo_id": memo_id,
                    "entry_id": entry_id,
                    "issue_url": github["issue_url"],
                    "issue_number": github["issue_number"],
                    "github": github,
                    "error": format!("GitHub issue created and memory updated, but status.json write failed: {write_err}"),
                    "recovery": {
                        "action": "promote_issue",
                        "memo_id": memo_id,
                        "flow_id": flow_id,
                        "hint": "Re-run promote_issue with the same flow_id to repair flow artifacts.",
                    },
                }))
                .map_err(|e| format!("serialize: {e}"));
            }
        };
        if let Err(write_err) = append_github_event(
            &run_dir,
            &flow_id,
            "github_issue_created",
            json!({
                "repo": github["repo"],
                "issue_number": github["issue_number"],
                "issue_url": github["issue_url"],
                "source": github["source"],
            }),
        ) {
            return serde_json::to_string(&json!({
                "status": "partial_failure",
                "memo_id": memo_id,
                "entry_id": entry_id,
                "issue_url": github["issue_url"],
                "issue_number": github["issue_number"],
                "github": github,
                "status_patch": status_patch,
                "error": format!("GitHub issue created and memory updated, but events.jsonl write failed: {write_err}"),
                "recovery": {
                    "action": "promote_issue",
                    "memo_id": memo_id,
                    "flow_id": flow_id,
                    "hint": "Re-run promote_issue with the same flow_id to repair flow artifacts.",
                },
            }))
            .map_err(|e| format!("serialize: {e}"));
        }
        event_persisted = true;
    }

    serde_json::to_string(&json!({
        "status": "promoted",
        "memo_id": memo_id,
        "entry_id": entry_id,
        "issue_url": github["issue_url"],
        "issue_number": github["issue_number"],
        "github": github,
        "status_patch": status_patch,
        "event_persisted": event_persisted,
    }))
    .map_err(|e| format!("serialize: {e}"))
}

pub(crate) async fn handle_handoff_promote_issue(
    server: &MemoryServer,
    params: HandoffPromoteIssueParams,
) -> Result<String, String> {
    let client = CliGhClient { server };
    promote_handoff_issue_with_client(server, &client, params).await
}

pub(crate) fn gc_expired_handoff_memories(
    store: &mut MemoryStore,
    max_age_days: u64,
) -> Result<usize, String> {
    let cutoff = chrono::Utc::now()
        - chrono::Duration::days(std::cmp::min(max_age_days, i64::MAX as u64) as i64);

    let mut stmt = store
        .connection()
        .prepare(
            "SELECT id, timestamp, metadata, archived
             FROM memories
             WHERE category = ?1 AND path LIKE ?2",
        )
        .map_err(|e| format!("prepare handoff GC query failed: {e}"))?;
    let rows = stmt
        .query_map(("handoff", format!("{HANDOFF_PATH}%")), |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, bool>(3)?,
            ))
        })
        .map_err(|e| format!("query expired handoff memories failed: {e}"))?;

    let mut ids_to_delete = Vec::new();
    for row in rows {
        let (id, timestamp, metadata_json, archived) =
            row.map_err(|e| format!("read expired handoff candidate failed: {e}"))?;
        let metadata: serde_json::Value = serde_json::from_str(&metadata_json)
            .map_err(|e| format!("parse handoff metadata for '{id}' failed: {e}"))?;

        let status = metadata
            .get("status")
            .and_then(|v| v.as_str())
            .unwrap_or("pending");
        if status != "acknowledged" && status != "promoted" && status != "superseded" && !archived {
            continue;
        }

        let timestamp = chrono::DateTime::parse_from_rfc3339(&timestamp)
            .map_err(|e| format!("parse handoff timestamp for '{id}' failed: {e}"))?
            .with_timezone(&chrono::Utc);
        if timestamp < cutoff {
            ids_to_delete.push(id);
        }
    }
    drop(stmt);

    let mut deleted = 0usize;
    for id in ids_to_delete {
        if store
            .delete(&id)
            .map_err(|e| format!("delete expired handoff '{id}' failed: {e}"))?
        {
            deleted += 1;
        }
    }

    Ok(deleted)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ensure_test_env() {
        static INIT: std::sync::Once = std::sync::Once::new();
        INIT.call_once(|| {
            std::env::set_var("VOYAGE_API_KEY", "test-voyage-key");
            std::env::set_var("SILICONFLOW_API_KEY", "test-siliconflow-key");
            std::env::set_var("SILICONFLOW_MODEL", "test-model");
            std::env::set_var("SUMMARY_MODEL", "test-summary-model");
        });
    }

    fn env_lock() -> &'static std::sync::Mutex<()> {
        crate::shell_ops::tachi_run_root_env_lock()
    }

    fn test_server(db_path: std::path::PathBuf) -> MemoryServer {
        ensure_test_env();
        MemoryServer::new(db_path, None).expect("test memory server")
    }

    fn test_store() -> MemoryStore {
        MemoryStore::open_in_memory().expect("test memory store")
    }

    fn test_entry(memo: HandoffMemo) -> MemoryEntry {
        MemoryEntry {
            id: format!("handoff:{}", memo.id),
            path: HANDOFF_PATH.to_string(),
            summary: memo.summary.clone(),
            text: memo.summary.clone(),
            importance: 0.9,
            timestamp: memo.created_at.clone(),
            valid_from: String::new(),
            valid_until: None,
            category: "handoff".to_string(),
            topic: "agent-handoff".to_string(),
            keywords: vec!["handoff".to_string()],
            persons: vec![],
            entities: vec![memo.from_agent.clone()],
            location: String::new(),
            source: "test".to_string(),
            scope: "general".to_string(),
            archived: false,
            access_count: 0,
            last_access: None,
            revision: 1,
            metadata: json!({
                "handoff_memo_id": memo.id,
                "handoff": memo,
                "status": "pending",
            }),
            vector: None,
            retention_policy: None,
            domain: None,
            recall_count: 0,
            query_diversity: 0,
            tier: "raw".to_string(),
        }
    }

    #[test]
    fn pending_handoff_entries_reads_persisted_memory() {
        let mut store = test_store();
        let memo = HandoffMemo {
            id: "memo-1".to_string(),
            from_agent: "agent-a".to_string(),
            target_agent: Some("agent-b".to_string()),
            summary: "persisted memo".to_string(),
            next_steps: vec!["continue".to_string()],
            context: None,
            created_at: Utc::now().to_rfc3339(),
            acknowledged: false,
        };
        store.upsert(&test_entry(memo)).expect("upsert memo");

        let entries = pending_handoff_entries(&mut store).expect("pending entries");
        assert_eq!(entries.len(), 1);
        let memo = memo_from_entry(&entries[0]);
        assert_eq!(memo.id, "memo-1");
        assert_eq!(memo.from_agent, "agent-a");
        assert_eq!(memo.target_agent.as_deref(), Some("agent-b"));
    }

    #[test]
    fn acknowledge_updates_persisted_handoff_metadata() {
        let mut store = test_store();
        let memo = HandoffMemo {
            id: "memo-ack".to_string(),
            from_agent: "agent-a".to_string(),
            target_agent: Some("agent-b".to_string()),
            summary: "needs ack".to_string(),
            next_steps: vec!["ack".to_string()],
            context: None,
            created_at: Utc::now().to_rfc3339(),
            acknowledged: false,
        };
        let entry = test_entry(memo);
        store.upsert(&entry).expect("upsert memo");

        upsert_acknowledged_entry(&mut store, entry, Some("agent-b")).expect("ack memo");

        let pending = pending_handoff_entries(&mut store).expect("pending entries");
        assert!(pending.is_empty());
        let stored = store
            .get("handoff:memo-ack")
            .expect("get memo")
            .expect("memo exists");
        assert_eq!(stored.metadata["status"], json!("acknowledged"));
        assert_eq!(stored.metadata["acknowledged_by"], json!("agent-b"));
        assert_eq!(stored.metadata["handoff"]["acknowledged"], json!(true));
    }

    #[tokio::test]
    async fn handoff_check_reads_and_acks_persisted_memos_after_restart() {
        let db_path = std::env::temp_dir().join(format!(
            "handoff-persistence-{}.sqlite",
            uuid::Uuid::new_v4()
        ));

        {
            let server = test_server(db_path.clone());
            server
                .agent_register(Parameters(AgentRegisterParams {
                    agent_id: "agent-a".to_string(),
                    display_name: None,
                    capabilities: vec![],
                    tool_filter: None,
                    rate_limit_rpm: None,
                    rate_limit_burst: None,
                }))
                .await
                .expect("register source agent");
            server
                .handoff_leave(Parameters(HandoffLeaveParams {
                    summary: "persist across restart".to_string(),
                    next_steps: vec!["resume from db".to_string()],
                    target_agent: Some("agent-b".to_string()),
                    context: Some(json!({"file": "src/lib.rs"})),
                }))
                .await
                .expect("leave handoff");
        }

        let server = test_server(db_path.clone());
        let check = server
            .handoff_check(Parameters(HandoffCheckParams {
                agent_id: Some("agent-b".to_string()),
                acknowledge: true,
            }))
            .await
            .expect("check persisted handoff");
        let check_json: serde_json::Value = serde_json::from_str(&check).expect("check json");
        assert_eq!(check_json["pending_memos"], json!(1));
        assert_eq!(check_json["memos"][0]["from_agent"], json!("agent-a"));
        assert_eq!(
            check_json["memos"][0]["next_steps"],
            json!(["resume from db"])
        );

        let after = server
            .handoff_check(Parameters(HandoffCheckParams {
                agent_id: Some("agent-b".to_string()),
                acknowledge: false,
            }))
            .await
            .expect("check after ack");
        let after_json: serde_json::Value = serde_json::from_str(&after).expect("after json");
        assert_eq!(after_json["pending_memos"], json!(0));

        let stored = server
            .with_global_store_read(|store| {
                let entries = store
                    .list_by_path(HANDOFF_PATH, 10, false)
                    .map_err(|e| e.to_string())?;
                entries
                    .into_iter()
                    .next()
                    .ok_or_else(|| "missing handoff memory".to_string())
            })
            .expect("read stored handoff");
        assert_eq!(stored.metadata["status"], json!("acknowledged"));
        assert_eq!(stored.metadata["acknowledged_by"], json!("agent-b"));

        let _ = std::fs::remove_file(db_path);
    }

    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn promote_handoff_issue_updates_memory_and_flow_artifacts() {
        let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
        let original_run_root = std::env::var_os("TACHI_RUN_ROOT");
        let db_path =
            std::env::temp_dir().join(format!("handoff-promote-{}.sqlite", uuid::Uuid::new_v4()));
        let run_root =
            std::env::temp_dir().join(format!("handoff-promote-runs-{}", uuid::Uuid::new_v4()));
        let flow_id = "flow_handoff-promote";
        std::fs::create_dir_all(run_root.join(flow_id)).expect("create run dir");
        std::env::set_var("TACHI_RUN_ROOT", &run_root);

        let server = test_server(db_path.clone());
        let left = server
            .handoff_leave(Parameters(HandoffLeaveParams {
                summary: "Promote this memo".to_string(),
                next_steps: vec!["Create a tracked GitHub issue".to_string()],
                target_agent: Some("next-agent".to_string()),
                context: Some(json!({"risk": "medium"})),
            }))
            .await
            .expect("leave handoff");
        let left_json: serde_json::Value = serde_json::from_str(&left).expect("leave json");
        let memo_id = left_json["memo_id"].as_str().expect("memo id").to_string();
        let client = crate::gh_safe_merge::MockGhClient::new();

        let promoted = promote_handoff_issue_with_client(
            &server,
            &client,
            HandoffPromoteIssueParams {
                memo_id: memo_id.clone(),
                repo: "owner/repo".to_string(),
                title: None,
                labels: vec!["task".to_string()],
                flow_id: Some(flow_id.to_string()),
                force: false,
            },
        )
        .await
        .expect("promote handoff");
        let promoted_json: serde_json::Value =
            serde_json::from_str(&promoted).expect("promote json");
        assert_eq!(promoted_json["issue_number"], json!(1000));
        assert_eq!(promoted_json["event_persisted"], json!(true));

        let stored = server
            .with_global_store_read(|store| {
                store
                    .get(&format!("handoff:{memo_id}"))
                    .map_err(|e| e.to_string())?
                    .ok_or_else(|| "missing promoted handoff".to_string())
            })
            .expect("read promoted handoff");
        assert_eq!(stored.metadata["status"], json!("promoted"));
        assert_eq!(stored.metadata["github"]["issue_number"], json!(1000));
        assert_eq!(stored.metadata["acknowledged"], json!(true));
        assert_eq!(stored.metadata["handoff"]["acknowledged"], json!(true));
        assert_eq!(
            stored.retention_policy.as_deref(),
            Some(memory_core::RetentionPolicy::Pinned.as_str())
        );

        let status = std::fs::read_to_string(run_root.join(flow_id).join("status.json"))
            .expect("read status");
        assert!(status.contains("https://github.com/owner/repo/issues/1000"));
        let events = std::fs::read_to_string(run_root.join(flow_id).join("events.jsonl"))
            .expect("read events");
        assert!(events.contains("github_issue_created"));

        if let Some(root) = original_run_root {
            std::env::set_var("TACHI_RUN_ROOT", root);
        } else {
            std::env::remove_var("TACHI_RUN_ROOT");
        }
        let _ = std::fs::remove_file(db_path);
        let _ = std::fs::remove_dir_all(run_root);
    }

    #[tokio::test]
    async fn promote_rejects_invalid_flow_id_before_issue_creation() {
        let db_path = std::env::temp_dir().join(format!(
            "handoff-invalid-flow-{}.sqlite",
            uuid::Uuid::new_v4()
        ));
        let server = test_server(db_path.clone());
        let left = server
            .handoff_leave(Parameters(HandoffLeaveParams {
                summary: "Invalid flow id test".to_string(),
                next_steps: vec![],
                target_agent: None,
                context: None,
            }))
            .await
            .expect("leave");
        let left_json: serde_json::Value = serde_json::from_str(&left).expect("json");
        let memo_id = left_json["memo_id"].as_str().expect("memo id").to_string();
        let client = crate::gh_safe_merge::MockGhClient::new();

        let err = promote_handoff_issue_with_client(
            &server,
            &client,
            HandoffPromoteIssueParams {
                memo_id,
                repo: "owner/repo".to_string(),
                title: None,
                labels: vec![],
                flow_id: Some("../escape".to_string()),
                force: false,
            },
        )
        .await
        .expect_err("invalid flow id should fail");
        assert!(err.contains("Invalid flow_id"));

        let _ = std::fs::remove_file(db_path);
    }

    #[test]
    fn resolve_from_agent_falls_back_to_profile_env_then_unknown() {
        let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
        let original_profile = std::env::var_os("TACHI_PROFILE");
        std::env::remove_var("TACHI_PROFILE");
        assert_eq!(fallback_agent_id(None), "unknown-agent");

        std::env::set_var("TACHI_PROFILE", "antigravity");
        assert_eq!(fallback_agent_id(None), "antigravity");
        assert_eq!(
            fallback_agent_id(Some("registered".to_string())),
            "registered"
        );

        if let Some(profile) = original_profile {
            std::env::set_var("TACHI_PROFILE", profile);
        } else {
            std::env::remove_var("TACHI_PROFILE");
        }
    }

    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn promote_dedup_returns_already_promoted_without_force() {
        let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
        let original_run_root = std::env::var_os("TACHI_RUN_ROOT");
        let db_path =
            std::env::temp_dir().join(format!("handoff-dedup-{}.sqlite", uuid::Uuid::new_v4()));
        let run_root =
            std::env::temp_dir().join(format!("handoff-dedup-runs-{}", uuid::Uuid::new_v4()));
        std::env::set_var("TACHI_RUN_ROOT", &run_root);

        let server = test_server(db_path.clone());
        let left = server
            .handoff_leave(Parameters(HandoffLeaveParams {
                summary: "Dedup test memo".to_string(),
                next_steps: vec!["step".to_string()],
                target_agent: None,
                context: None,
            }))
            .await
            .expect("leave");
        let left_json: serde_json::Value = serde_json::from_str(&left).expect("json");
        let memo_id = left_json["memo_id"].as_str().expect("memo id").to_string();
        let client = crate::gh_safe_merge::MockGhClient::new();

        // First promote succeeds
        let first = promote_handoff_issue_with_client(
            &server,
            &client,
            HandoffPromoteIssueParams {
                memo_id: memo_id.clone(),
                repo: "owner/repo".to_string(),
                title: None,
                labels: vec![],
                flow_id: None,
                force: false,
            },
        )
        .await
        .expect("first promote");
        let first_json: serde_json::Value = serde_json::from_str(&first).expect("json");
        assert_eq!(first_json["status"], json!("promoted"));

        // Second promote (no force) returns already_promoted
        let second = promote_handoff_issue_with_client(
            &server,
            &client,
            HandoffPromoteIssueParams {
                memo_id: memo_id.clone(),
                repo: "owner/repo".to_string(),
                title: None,
                labels: vec![],
                flow_id: None,
                force: false,
            },
        )
        .await
        .expect("second promote");
        let second_json: serde_json::Value = serde_json::from_str(&second).expect("json");
        assert_eq!(second_json["status"], json!("already_promoted"));
        assert!(second_json["issue_url"].as_str().is_some());
        assert!(second_json["hint"].as_str().unwrap().contains("force=true"));

        if let Some(root) = original_run_root {
            std::env::set_var("TACHI_RUN_ROOT", root);
        } else {
            std::env::remove_var("TACHI_RUN_ROOT");
        }
        let _ = std::fs::remove_file(db_path);
        let _ = std::fs::remove_dir_all(run_root);
    }

    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn promote_force_creates_new_issue_even_if_already_promoted() {
        let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
        let original_run_root = std::env::var_os("TACHI_RUN_ROOT");
        let db_path =
            std::env::temp_dir().join(format!("handoff-force-{}.sqlite", uuid::Uuid::new_v4()));
        let run_root =
            std::env::temp_dir().join(format!("handoff-force-runs-{}", uuid::Uuid::new_v4()));
        std::env::set_var("TACHI_RUN_ROOT", &run_root);

        let server = test_server(db_path.clone());
        let left = server
            .handoff_leave(Parameters(HandoffLeaveParams {
                summary: "Force re-promote test".to_string(),
                next_steps: vec![],
                target_agent: None,
                context: None,
            }))
            .await
            .expect("leave");
        let left_json: serde_json::Value = serde_json::from_str(&left).expect("json");
        let memo_id = left_json["memo_id"].as_str().expect("memo id").to_string();
        let client = crate::gh_safe_merge::MockGhClient::new();

        // First promote
        let first = promote_handoff_issue_with_client(
            &server,
            &client,
            HandoffPromoteIssueParams {
                memo_id: memo_id.clone(),
                repo: "owner/repo".to_string(),
                title: None,
                labels: vec![],
                flow_id: None,
                force: false,
            },
        )
        .await
        .expect("first promote");
        let first_json: serde_json::Value = serde_json::from_str(&first).expect("json");
        let first_issue_number = first_json["issue_number"].as_u64().expect("issue number");

        // Force re-promote creates a new issue with a different number
        let second = promote_handoff_issue_with_client(
            &server,
            &client,
            HandoffPromoteIssueParams {
                memo_id: memo_id.clone(),
                repo: "owner/repo".to_string(),
                title: None,
                labels: vec![],
                flow_id: None,
                force: true,
            },
        )
        .await
        .expect("force promote");
        let second_json: serde_json::Value = serde_json::from_str(&second).expect("json");
        assert_eq!(second_json["status"], json!("promoted"));
        let second_issue_number = second_json["issue_number"].as_u64().expect("issue number");
        assert_ne!(first_issue_number, second_issue_number);

        if let Some(root) = original_run_root {
            std::env::set_var("TACHI_RUN_ROOT", root);
        } else {
            std::env::remove_var("TACHI_RUN_ROOT");
        }
        let _ = std::fs::remove_file(db_path);
        let _ = std::fs::remove_dir_all(run_root);
    }

    #[test]
    fn promoting_intermediate_state_is_set_before_issue_creation() {
        let mut store = test_store();
        let memo = HandoffMemo {
            id: "memo-promoting".to_string(),
            from_agent: "agent-a".to_string(),
            target_agent: None,
            summary: "test promoting state".to_string(),
            next_steps: vec![],
            context: None,
            created_at: Utc::now().to_rfc3339(),
            acknowledged: false,
        };
        let entry = test_entry(memo);
        store.upsert(&entry).expect("upsert");

        upsert_promoting_entry(&mut store, entry.clone()).expect("mark promoting");
        let stored = store
            .get("handoff:memo-promoting")
            .expect("get")
            .expect("exists");
        assert_eq!(stored.metadata["status"], json!("promoting"));
        assert!(stored.metadata["promoting_at"].as_str().is_some());

        revert_promoting_entry(&mut store, stored, "pending").expect("revert");
        let reverted = store
            .get("handoff:memo-promoting")
            .expect("get")
            .expect("exists");
        assert_eq!(reverted.metadata["status"], json!("pending"));
        assert!(reverted.metadata.get("promoting_at").is_none());
    }

    #[test]
    fn promoted_status_is_not_pending() {
        let mut store = test_store();
        let memo = HandoffMemo {
            id: "memo-promoted".to_string(),
            from_agent: "agent-a".to_string(),
            target_agent: None,
            summary: "promoted memo".to_string(),
            next_steps: vec![],
            context: None,
            created_at: Utc::now().to_rfc3339(),
            acknowledged: false,
        };
        let mut entry = test_entry(memo);
        entry.metadata["status"] = json!("promoted");
        store.upsert(&entry).expect("upsert");

        let entries = pending_handoff_entries(&mut store).expect("pending entries");
        assert!(entries.is_empty());
    }

    #[test]
    fn existing_issue_url_extracts_from_metadata() {
        let mut entry = test_entry(HandoffMemo {
            id: "memo-url".to_string(),
            from_agent: "a".to_string(),
            target_agent: None,
            summary: "s".to_string(),
            next_steps: vec![],
            context: None,
            created_at: Utc::now().to_rfc3339(),
            acknowledged: false,
        });
        assert!(existing_issue_url(&entry).is_none());

        let md = entry.metadata.as_object_mut().expect("obj");
        md.insert(
            "github".into(),
            json!({"issue_url": "https://github.com/o/r/issues/42"}),
        );
        assert_eq!(
            existing_issue_url(&entry).as_deref(),
            Some("https://github.com/o/r/issues/42")
        );
    }

    #[test]
    fn test_gc_expired_handoff_memories() {
        let mut store = test_store();

        let old_ack = HandoffMemo {
            id: "old-ack".to_string(),
            from_agent: "agent-a".to_string(),
            target_agent: None,
            summary: "old ack memo".to_string(),
            next_steps: vec![],
            context: None,
            created_at: (chrono::Utc::now() - chrono::Duration::days(31)).to_rfc3339(),
            acknowledged: true,
        };
        let mut entry_old_ack = test_entry(old_ack);
        entry_old_ack.metadata["status"] = json!("acknowledged");
        store.upsert(&entry_old_ack).expect("upsert old ack");

        let new_ack = HandoffMemo {
            id: "new-ack".to_string(),
            from_agent: "agent-a".to_string(),
            target_agent: None,
            summary: "new ack memo".to_string(),
            next_steps: vec![],
            context: None,
            created_at: chrono::Utc::now().to_rfc3339(),
            acknowledged: true,
        };
        let mut entry_new_ack = test_entry(new_ack);
        entry_new_ack.metadata["status"] = json!("acknowledged");
        store.upsert(&entry_new_ack).expect("upsert new ack");

        let old_pending = HandoffMemo {
            id: "old-pending".to_string(),
            from_agent: "agent-a".to_string(),
            target_agent: None,
            summary: "old pending memo".to_string(),
            next_steps: vec![],
            context: None,
            created_at: (chrono::Utc::now() - chrono::Duration::days(31)).to_rfc3339(),
            acknowledged: false,
        };
        let entry_old_pending = test_entry(old_pending);
        store
            .upsert(&entry_old_pending)
            .expect("upsert old pending");

        let deleted = gc_expired_handoff_memories(&mut store, 30).expect("gc");
        assert_eq!(deleted, 1);

        assert!(store.get("handoff:old-ack").expect("get").is_none());
        assert!(store.get("handoff:new-ack").expect("get").is_some());
        assert!(store.get("handoff:old-pending").expect("get").is_some());
    }

    #[tokio::test]
    async fn test_handoff_leave_supersedes_pending_duplicate() {
        let db_path =
            std::env::temp_dir().join(format!("handoff-dup-test-{}.sqlite", uuid::Uuid::new_v4()));
        let server = test_server(db_path.clone());
        server
            .agent_register(Parameters(AgentRegisterParams {
                agent_id: "agent-a".to_string(),
                display_name: None,
                capabilities: vec![],
                tool_filter: None,
                rate_limit_rpm: None,
                rate_limit_burst: None,
            }))
            .await
            .expect("register agent");

        let first_resp = server
            .handoff_leave(Parameters(HandoffLeaveParams {
                summary: "first pending memo".to_string(),
                next_steps: vec![],
                target_agent: Some("agent-b".to_string()),
                context: None,
            }))
            .await
            .expect("leave first");
        let first_json: serde_json::Value = serde_json::from_str(&first_resp).expect("json");
        let first_id = first_json["memo_id"].as_str().expect("id").to_string();

        let second_resp = server
            .handoff_leave(Parameters(HandoffLeaveParams {
                summary: "second pending memo".to_string(),
                next_steps: vec![],
                target_agent: Some("agent-b".to_string()),
                context: None,
            }))
            .await
            .expect("leave second");
        let second_json: serde_json::Value = serde_json::from_str(&second_resp).expect("json");
        let second_id = second_json["memo_id"].as_str().expect("id").to_string();

        server
            .with_global_store_read(|store| {
                let entry1 = store
                    .get_with_options(&format!("handoff:{first_id}"), true)
                    .unwrap()
                    .unwrap();
                assert!(entry1.archived);
                assert_eq!(entry1.metadata["status"], "superseded");

                let entry2 = store.get(&format!("handoff:{second_id}")).unwrap().unwrap();
                assert!(!entry2.archived);
                assert_eq!(entry2.metadata["status"], "pending");
                Ok(())
            })
            .unwrap();

        let _ = std::fs::remove_file(db_path);
    }

    #[test]
    fn supersede_pending_handoffs_propagates_db_errors() {
        use rusqlite::Connection;

        let db_path = std::env::temp_dir().join(format!(
            "handoff-supersede-db-error-{}.sqlite",
            uuid::Uuid::new_v4()
        ));

        // Seed a pending handoff entry.
        {
            let mut store = MemoryStore::open(db_path.to_str().unwrap()).expect("open store");
            let memo = HandoffMemo {
                id: "memo-1".to_string(),
                from_agent: "agent-a".to_string(),
                target_agent: Some("agent-b".to_string()),
                summary: "pending".to_string(),
                next_steps: vec![],
                context: None,
                created_at: Utc::now().to_rfc3339(),
                acknowledged: false,
            };
            store.upsert(&test_entry(memo)).expect("seed pending");
        }

        // Reopen the store, then hold a RESERVED lock on the DB so the
        // supersede write fails instead of being swallowed.
        let mut store = MemoryStore::open(db_path.to_str().unwrap()).expect("reopen store");
        let lock_path = db_path.clone();
        let (acquired_tx, acquired_rx) = std::sync::mpsc::channel::<()>();
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let lock_handle = std::thread::spawn(move || {
            let conn = Connection::open(&lock_path).expect("open lock connection");
            conn.execute_batch("BEGIN IMMEDIATE;")
                .expect("begin immediate");
            let _ = acquired_tx.send(());
            let _ = release_rx.recv();
        });
        acquired_rx.recv().expect("lock acquired");

        let new_memo = HandoffMemo {
            id: "memo-2".to_string(),
            from_agent: "agent-a".to_string(),
            target_agent: Some("agent-b".to_string()),
            summary: "new".to_string(),
            next_steps: vec![],
            context: None,
            created_at: Utc::now().to_rfc3339(),
            acknowledged: false,
        };
        let new_entry = test_entry(new_memo.clone());

        let result = supersede_pending_handoffs(&mut store, &new_memo, &new_entry);
        assert!(
            result.is_err(),
            "expected supersede DB error to propagate, got {result:?}"
        );

        let _ = release_tx.send(());
        let _ = lock_handle.join();
        let _ = std::fs::remove_file(&db_path);
        let _ = std::fs::remove_file(db_path.with_extension("sqlite-wal"));
        let _ = std::fs::remove_file(db_path.with_extension("sqlite-shm"));
    }
}
