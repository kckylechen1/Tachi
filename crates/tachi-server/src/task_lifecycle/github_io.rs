use super::*;

/// #1693 compact status/intake: whitelist-shape one GitHub issue snapshot
/// for compact responses. Identifiers, state, labels, and reference paths
/// stay; `body` (and any comment content) is omitted WHOLE — never
/// truncated — because the caller already holds the ref and asked for a
/// compact read. `null` (no snapshot) stays `null` exactly and a missing
/// key stays missing; a non-object snapshot is rendered as a typed
/// unknown marker instead of a fake valid empty object. The explicit full
/// reads (non-compact status, intake `format=full`) retain the complete
/// snapshot. Shared by the status cycle receipt and the intake receipt.
pub(super) fn compact_issue_snapshot_value(snapshot: &Value) -> Value {
    compact_object_value(snapshot, |object| {
        Value::Object(whitelist_object_fields(
            object,
            &[
                "repo",
                "number",
                "title",
                "labels",
                "state",
                "url",
                "doc_paths",
                "spec_paths",
                "source",
            ],
        ))
    })
}

/// #1693 compact status: the PR-snapshot counterpart. PR snapshots carry
/// no body today; the whitelist keeps this honest if one ever appears.
pub(super) fn compact_pr_snapshot_value(snapshot: &Value) -> Value {
    compact_object_value(snapshot, |object| {
        Value::Object(whitelist_object_fields(
            object,
            &[
                "repo",
                "number",
                "title",
                "state",
                "url",
                "head_ref",
                "base_ref",
                "review_decision",
                "review",
                "mergeable",
                "merge_state",
                "source",
            ],
        ))
    })
}

/// #1693 compact status: whitelist the cached flow `status.json` GitHub
/// block (`github.cached`). The field set is the CLOSED vocabulary the
/// owning producers write (`task_lifecycle::github_flow_state`'s schema
/// block plus the intake / link_pr / safe_merge / ship writers): identity,
/// gate state, check/review state, reference paths — and the automation
/// plan's DECISION fields (risk classification, reason codes, gate
/// reasons, branch/pr identity). The risk rows keep
/// `reason`/`needle`/`source`/`confidence` presence assertions and omit
/// the free-form `evidence` snippet WHOLE — that snippet is derived from
/// the issue body, exactly the content a compact read must not replay.
/// Unknown fields at every level are omitted whole (closed whitelist — no
/// open arbitrary branches); `null` stays `null`; a non-object input is
/// a typed unknown marker.
pub(super) fn compact_cached_github_value(cached: &Value) -> Value {
    compact_object_value(cached, |object| {
        let mut compact = whitelist_object_fields(
            object,
            &[
                "repo",
                "issue_number",
                "issue_ref",
                "issue_url",
                "issue_title",
                "issue_state",
                "pr_number",
                "pr_ref",
                "pr_url",
                "pr_title",
                "pr_state",
                "head_ref",
                "base_ref",
                "merge_state",
                "mergeable",
                "head_sha",
                "policy",
                "requested_mode",
                "dry_run",
                "will_merge",
                "merge_attempted",
                "merge_executed",
                "labels",
                "doc_paths",
                "spec_paths",
                "updated_at",
            ],
        );
        for (key, fields) in [
            (
                "checks",
                &[
                    "state",
                    "status",
                    "conclusion",
                    "source",
                    "artifact",
                    "failed_checks_recorded_only",
                    "dry_run",
                    "updated_at",
                    "required",
                    "allow_missing",
                    "head_consistent",
                    "head_consistency_state",
                ][..],
            ),
            (
                "review",
                &["state", "updated_at", "required", "allow_missing_decision"][..],
            ),
            (
                "head_consistency",
                &[
                    "head_sha",
                    "checks_head_sha",
                    "review_decision_head_sha",
                    "head_consistent",
                    "state",
                    "requirement",
                    "source",
                ][..],
            ),
            (
                "flow",
                &[
                    "flow_id",
                    "linked_issue_refs",
                    "has_linked_issue",
                    "required",
                ][..],
            ),
            (
                "verification",
                &[
                    "flow_id",
                    "overall",
                    "required_total",
                    "current_head_sha",
                    "expected_head",
                    "observed_pr_head_sha",
                    "claim_id",
                    "claim_transition_version",
                    "passed",
                    "failed",
                    "pending",
                    "stale",
                    "waiting_on",
                    "reasons",
                    "ledger_updated_at",
                ][..],
            ),
            (
                "ship",
                &["status", "branch", "commit_sha", "files", "warnings"][..],
            ),
        ] {
            if let Some(nested) = object.get(key) {
                compact.insert(
                    (*key).to_string(),
                    compact_object_value(nested, |value| {
                        Value::Object(whitelist_object_fields(value, fields))
                    }),
                );
            }
        }
        if let Some(plan) = object
            .get("automation_plan")
            .filter(|value| value.is_object())
        {
            compact.insert(
                "automation_plan".to_string(),
                compact_automation_plan_value(plan.as_object().expect("checked object")),
            );
        }
        Value::Object(compact)
    })
}

/// The automation plan's decision fields: status/gate/risk classification
/// and identity, with risk rows as presence assertions minus the
/// body-derived `evidence` snippet (and minus the fixed coaching prose —
/// `recommended_next_action` is not a decision field the compact receipt
/// needs; the cycle view carries its own marked `next_action`).
fn compact_automation_plan_value(plan: &serde_json::Map<String, Value>) -> Value {
    let mut compact = whitelist_object_fields(
        plan,
        &[
            "status",
            "dispatch_allowed",
            "requires_leader",
            "risk",
            "has_acceptance_criteria",
            "missing_acceptance_criteria",
            "high_risk_reasons",
            "leader_gate_reasons",
            "branch",
            "pr_title",
        ],
    );
    for key in ["risk_evidence", "risk_advisory"] {
        if let Some(rows) = plan.get(key).and_then(Value::as_array) {
            let compact_rows: Vec<Value> = rows
                .iter()
                .map(|row| {
                    compact_object_value(row, |object| {
                        Value::Object(whitelist_object_fields(
                            object,
                            &["reason", "needle", "source", "confidence"],
                        ))
                    })
                })
                .filter(|row| row.as_object().is_some_and(|object| !object.is_empty()))
                .collect();
            if !compact_rows.is_empty() {
                compact.insert((*key).to_string(), Value::Array(compact_rows));
            }
        }
    }
    Value::Object(compact)
}

/// Compact one snapshot-shaped value: `null` (no snapshot) is preserved
/// EXACTLY as null; a non-object shape is malformed and renders as a
/// typed unknown marker rather than a fake valid empty object; an object
/// is shaped by `shape`.
fn compact_object_value(
    value: &Value,
    shape: impl FnOnce(&serde_json::Map<String, Value>) -> Value,
) -> Value {
    match value {
        Value::Null => Value::Null,
        Value::Object(object) => shape(object),
        _ => json!({ "state": "unknown", "reason": "snapshot_not_object" }),
    }
}

/// Keep only the whitelisted fields, including explicit unknown/null values:
/// presence assertions over
/// identifiers/state/references — everything else (bodies, comments,
/// free-form content) is omitted whole rather than truncated.
fn whitelist_object_fields(
    object: &serde_json::Map<String, Value>,
    keys: &[&str],
) -> serde_json::Map<String, Value> {
    let mut compact = serde_json::Map::new();
    for key in keys {
        if let Some(field) = object.get(*key) {
            compact.insert((*key).to_string(), field.clone());
        }
    }
    compact
}

pub(super) async fn read_issue_snapshot(
    server: &MemoryServer,
    target: &GithubTarget,
    params: &TachiTaskParams,
) -> Result<IssueSnapshot, String> {
    let raw = crate::gh_ops::handle_tachi_gh(
        server,
        TachiGhParams {
            action: "issue_read".to_string(),
            repo: Some(target.repo.clone()),
            number: Some(target.number),
            dry_run: Some(true),
            ..Default::default()
        },
    )
    .await?;
    let value: Value = serde_json::from_str(&raw).map_err(|e| format!("parse issue_read: {e}"))?;
    let result = value.get("result").cloned().unwrap_or_else(|| json!({}));
    let title = result
        .get("title")
        .and_then(Value::as_str)
        .unwrap_or("GitHub issue")
        .to_string();
    let body = result
        .get("body")
        .and_then(Value::as_str)
        .map(str::to_string);
    let labels = result
        .get("labels")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    item.get("name")
                        .and_then(Value::as_str)
                        .or_else(|| item.as_str())
                        .map(str::to_string)
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let comments = result
        .get("comments")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.get("body").and_then(Value::as_str).map(str::to_string))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let mut doc_paths = params.doc_paths.clone();
    doc_paths.extend(extract_markdown_paths(body.as_deref().unwrap_or("")));
    for comment in &comments {
        doc_paths.extend(extract_markdown_paths(comment));
    }
    let mut spec_paths = params.spec_paths.clone();
    spec_paths.extend(
        doc_paths
            .iter()
            .filter(|path| path.to_ascii_lowercase().contains("spec"))
            .cloned(),
    );
    dedupe_strings(&mut doc_paths);
    dedupe_strings(&mut spec_paths);
    Ok(IssueSnapshot {
        repo: target.repo.clone(),
        number: target.number,
        title,
        body,
        labels,
        state: result
            .get("state")
            .and_then(Value::as_str)
            .map(str::to_string),
        url: result
            .get("url")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| {
                format!(
                    "https://github.com/{}/issues/{}",
                    target.repo, target.number
                )
            }),
        doc_paths,
        spec_paths,
    })
}

pub(super) async fn read_pr_snapshot(
    server: &MemoryServer,
    target: &GithubTarget,
) -> Result<PrSnapshot, String> {
    let raw = crate::gh_ops::handle_tachi_gh(
        server,
        TachiGhParams {
            action: "pr_read".to_string(),
            repo: Some(target.repo.clone()),
            number: Some(target.number),
            dry_run: Some(true),
            ..Default::default()
        },
    )
    .await?;
    let value: Value = serde_json::from_str(&raw).map_err(|e| format!("parse pr_read: {e}"))?;
    let result = value.get("result").cloned().unwrap_or_else(|| json!({}));
    Ok(PrSnapshot {
        repo: target.repo.clone(),
        number: target.number,
        title: result
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or("GitHub PR")
            .to_string(),
        state: result
            .get("state")
            .and_then(Value::as_str)
            .map(str::to_string),
        url: result
            .get("url")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| {
                format!("https://github.com/{}/pull/{}", target.repo, target.number)
            }),
        head_ref: result
            .get("headRefName")
            .and_then(Value::as_str)
            .map(str::to_string),
        base_ref: result
            .get("baseRefName")
            .and_then(Value::as_str)
            .map(str::to_string),
        review_decision: result
            .get("reviewDecision")
            .and_then(Value::as_str)
            .map(str::to_string),
        mergeable: result
            .get("mergeable")
            .and_then(Value::as_str)
            .map(str::to_string),
    })
}

pub(super) async fn seed_intake_orchestrator(
    server: &MemoryServer,
    flow_id: &str,
    objective: &str,
    issue: &IssueSnapshot,
    automation_plan: &Value,
) -> Result<(), String> {
    let issue_ref = format!("{}#{}", issue.repo, issue.number);
    let refs = std::iter::once(issue_ref.clone())
        .chain(issue.doc_paths.iter().cloned())
        .chain(issue.spec_paths.iter().cloned())
        .collect::<Vec<_>>();
    let todo_status = if issue.doc_paths.is_empty() && issue.spec_paths.is_empty() {
        "pending"
    } else {
        "done"
    };
    let dispatch_allowed = automation_plan
        .get("dispatch_allowed")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let leader_gate_reasons = string_array_field(automation_plan, "leader_gate_reasons");
    let _ = crate::orchestrator_ops::handle_orchestrator(
        server,
        TachiOrchestratorParams {
            action: "todo_update".to_string(),
            task_id: Some(flow_id.to_string()),
            todo_id: Some("canonical-docs".to_string()),
            todo_content: Some(
                "Attach or confirm canonical docs/specs for this feature.".to_string(),
            ),
            todo_status: Some(todo_status.to_string()),
            parent_todo_id: None,
            agent: Some("tachi_task_intake".to_string()),
            issue_ref: Some(issue_ref.clone()),
            blocked_reason: None,
            verification: if todo_status == "done" {
                Some("Issue intake found canonical doc/spec references.".to_string())
            } else {
                None
            },
            references: refs.clone(),
            objective: None,
            current_state: None,
            completed_steps: Vec::new(),
            remaining_steps: Vec::new(),
            files_touched: Vec::new(),
            commands_run: Vec::new(),
            tests_run: Vec::new(),
            known_blockers: Vec::new(),
            next_action: None,
            newest_user_instruction: None,
        },
    )
    .await?;
    let _ = crate::orchestrator_ops::handle_orchestrator(
        server,
        TachiOrchestratorParams {
            action: "todo_update".to_string(),
            task_id: Some(flow_id.to_string()),
            todo_id: Some("dispatch-or-plan".to_string()),
            todo_content: Some(
                "Read status (the cycle view when flow_id/issue_ref/pr_ref is supplied), then follow the next lifecycle step for brief, harness-native execution, or PR handoff."
                    .to_string(),
            ),
            todo_status: Some(
                if dispatch_allowed {
                    "pending"
                } else {
                    "blocked"
                }
                .to_string(),
            ),
            parent_todo_id: None,
            agent: Some("tachi_task_intake".to_string()),
            issue_ref: Some(issue_ref.clone()),
            blocked_reason: if dispatch_allowed {
                None
            } else {
                Some(leader_gate_reasons.join(", "))
            },
            verification: None,
            references: refs.clone(),
            objective: None,
            current_state: None,
            completed_steps: Vec::new(),
            remaining_steps: Vec::new(),
            files_touched: Vec::new(),
            commands_run: Vec::new(),
            tests_run: Vec::new(),
            known_blockers: Vec::new(),
            next_action: None,
            newest_user_instruction: None,
        },
    )
    .await?;
    let _ = crate::orchestrator_ops::handle_orchestrator(
        server,
        TachiOrchestratorParams {
            action: "handoff_write".to_string(),
            task_id: Some(flow_id.to_string()),
            todo_id: None,
            todo_content: None,
            todo_status: None,
            parent_todo_id: None,
            agent: Some("tachi_task_intake".to_string()),
            issue_ref: Some(issue_ref),
            blocked_reason: None,
            verification: None,
            references: refs,
            objective: Some(objective.to_string()),
            current_state: Some("GitHub issue has been bound to a Tachi flow.".to_string()),
            completed_steps: vec!["Read issue and wrote intake flow artifacts.".to_string()],
            remaining_steps: vec![
                "Confirm canonical docs/specs.".to_string(),
                "Read status before selecting the next lifecycle step.".to_string(),
                if dispatch_allowed {
                    "Dispatch bounded worker slice.".to_string()
                } else {
                    "Get leader confirmation before dispatch.".to_string()
                },
                "Link PR and run pr_status gate before merge.".to_string(),
            ],
            files_touched: Vec::new(),
            commands_run: Vec::new(),
            tests_run: Vec::new(),
            known_blockers: leader_gate_reasons,
            next_action: Some(
                automation_plan
                    .get("recommended_next_action")
                    .and_then(Value::as_str)
                    .unwrap_or("Call tachi_task(action='status', flow_id=...) and inspect the cycle lifecycle state.")
                    .to_string(),
            ),
            newest_user_instruction: None,
        },
    )
    .await?;
    Ok(())
}
