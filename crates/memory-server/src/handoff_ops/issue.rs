use crate::gh_ops::CliGhClient;
use crate::gh_safe_merge::GhClient;
use crate::server_state::{HandoffMemo, MemoryServer};
use crate::shell_ops::{append_github_event, merge_github_status, run_dir_for_flow_id};
use crate::tool_params::HandoffPromoteIssueParams;
use chrono::Utc;
use memory_core::{MemoryEntry, MemoryStore};
use serde_json::json;

use super::identity::resolve_from_agent;
use super::memo::memo_from_entry;

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

pub(super) fn existing_issue_url(entry: &MemoryEntry) -> Option<String> {
    entry
        .metadata
        .get("github")
        .and_then(|gh| gh.get("issue_url"))
        .and_then(|v| v.as_str())
        .filter(|url| !url.is_empty())
        .map(str::to_string)
}

pub(super) fn upsert_promoting_entry(
    store: &mut MemoryStore,
    mut entry: MemoryEntry,
) -> Result<(), String> {
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

pub(super) fn revert_promoting_entry(
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

pub(super) async fn promote_handoff_issue_with_client<C: GhClient>(
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
