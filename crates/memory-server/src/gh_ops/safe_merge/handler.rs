use super::*;

/// Orchestrate one `tachi_gh safe_merge` invocation:
/// 1. Pull PR + checks via the `GhClient`.
/// 2. Run the pure `evaluate_merge_gate`.
/// 3. If `Ready` and `!dry_run`, call `pr_merge` (squash by default).
/// 4. When `flow_id` is present, persist `merge_state` + reasons into
///    `status.json::github` and append the matching event to `events.jsonl`.
/// 5. After a successful non-dry-run merge, when `reclaim_worktree` is true
///    and `worktree` resolves to a local path, reclaim the worktree + branch +
///    target via `tachi-clean wt-remove` (best-effort) and record a reclamation
///    event (`github_safe_merge_reclaimed` on success, or
///    `github_safe_merge_reclaim_skipped` on any skip/failure).
/// 6. Return a JSON envelope the agent can render directly.
pub(crate) async fn handle_github_safe_merge<C: GhClient + ?Sized>(
    client: &C,
    repo: &str,
    pr_number: u64,
    strategy: MergeStrategy,
    dry_run: bool,
    flow_id: Option<&str>,
    tests_run: &[String],
    policy: MergeGatePolicy,
    worktree: Option<&str>,
    reclaim_worktree: bool,
) -> Result<String, String> {
    let flow_run_dir = match flow_id {
        Some(fid) => Some(run_dir_for_flow_id(fid)?),
        None => None,
    };
    let mut pr = client
        .pr_view(repo, pr_number)
        .await
        .map_err(|e| format!("pr_view failed: {e}"))?;
    let check_state_ingest = if dry_run {
        let check_runs = client
            .checks_list(repo, pr_number)
            .await
            .map_err(|e| format!("checks_list failed for check-state ingest: {e}"))?;
        let observed_at = chrono::Utc::now().to_rfc3339();
        let pr_ref = format!("{repo}#{pr_number}");
        Some(write_check_state_artifact(
            flow_id,
            &CheckStateArtifactInput {
                repo,
                pr_number,
                pr_ref: Some(pr_ref.as_str()),
                head_ref: pr.head_ref.as_deref(),
                observed_at: observed_at.as_str(),
                source: "safe_merge.dry_run",
                dry_run,
                checks: &check_runs,
            },
        )?)
    } else {
        None
    };
    let check_state_ingest = check_state_ingest
        .map(serde_json::to_value)
        .transpose()
        .map_err(|e| format!("serialize check_state_ingest: {e}"))?;
    if matches!(pr.state, PrLifecycleState::Merged) {
        return handle_already_merged_pr(
            repo,
            pr_number,
            &pr,
            dry_run,
            flow_id,
            policy,
            check_state_ingest,
        )
        .await;
    }
    let mut verification_gate = match evaluate_verification_gate(flow_id, &pr.head_sha) {
        Ok(gate) => gate,
        Err(err) if flow_id.is_some() => Some(json!({
            "flow_id": flow_id,
            "overall": "failed",
            "required_total": 0,
            "current_head_sha": pr.head_sha,
            "passed": [],
            "failed": [],
            "pending": [],
            "stale": [],
            "waiting_on": [],
            "reasons": ["verification:invalid"],
            "error": err,
        })),
        Err(err) => return Err(err),
    };
    if verification_gate.is_none() && !tests_run.is_empty() {
        if let Some(fid) = flow_id {
            record_tests_run_verification(fid, repo, pr_number, &pr.head_sha, tests_run)?;
            verification_gate = evaluate_verification_gate(flow_id, &pr.head_sha)?;
        }
    }
    if verification_gate.is_none()
        && flow_id.is_some()
        && !matches!(policy.mode, MergeGatePolicyMode::Permissive)
    {
        verification_gate = Some(json!({
            "flow_id": flow_id,
            "overall": "pending",
            "required_total": 0,
            "current_head_sha": pr.head_sha,
            "passed": [],
            "failed": [],
            "pending": [],
            "stale": [],
            "waiting_on": ["verification:missing"],
            "reasons": [],
        }));
    }
    if verification_satisfies_head_consistency(verification_gate.as_ref(), policy) {
        pr.head_consistent = Some(true);
    }
    let mut decision = evaluate_merge_gate_with_policy(&pr, policy);
    decision = apply_verification_gate_to_decision(decision, verification_gate.as_ref());
    let has_linked_issue = !pr.linked_issue_refs.is_empty();
    if policy.require_linked_issue_or_flow && flow_id.is_none() && !has_linked_issue {
        decision = match decision {
            MergeDecision::Ready => MergeDecision::Pending {
                waiting_on: vec!["flow_or_issue:missing".to_string()],
            },
            MergeDecision::Pending { mut waiting_on } => {
                waiting_on.push("flow_or_issue:missing".to_string());
                MergeDecision::Pending { waiting_on }
            }
            blocked => blocked,
        };
    }
    let merge_state = decision.merge_state_label();
    let will_merge = matches!(decision, MergeDecision::Ready) && !dry_run;
    let requested_mode = if dry_run {
        "preview"
    } else {
        "merge_requested"
    };

    let (event_kind, event_payload, merged_sha) = match &decision {
        MergeDecision::Ready => {
            if dry_run {
                (
                    "github_review_gate_passed",
                    json!({
                        "repo": repo,
                        "pr_number": pr_number,
                        "head_sha": pr.head_sha,
                        "requested_mode": requested_mode,
                        "merge_attempted": false,
                        "merge_executed": false,
                        "dry_run": true,
                    }),
                    None,
                )
            } else {
                let merge_res = client
                    .pr_merge(repo, pr_number, strategy, &pr.head_sha)
                    .await
                    .map_err(|e| format!("pr_merge failed: {e}"))?;
                (
                    "github_pr_merged",
                    json!({
                        "repo": repo,
                        "pr_number": pr_number,
                        "head_sha": pr.head_sha,
                        "merge_sha": merge_res.merge_sha,
                        "strategy": format!("{:?}", merge_res.strategy).to_lowercase(),
                        "requested_mode": requested_mode,
                        "merge_attempted": true,
                        "merge_executed": true,
                        "dry_run": false,
                    }),
                    Some(merge_res.merge_sha),
                )
            }
        }
        MergeDecision::Blocked { reasons } => (
            "github_merge_blocked",
            json!({
                "repo": repo,
                "pr_number": pr_number,
                "head_sha": pr.head_sha,
                "requested_mode": requested_mode,
                "merge_attempted": false,
                "merge_executed": false,
                "reasons": reasons,
            }),
            None,
        ),
        MergeDecision::Pending { waiting_on } => (
            "github_checks_polled",
            json!({
                "repo": repo,
                "pr_number": pr_number,
                "head_sha": pr.head_sha,
                "requested_mode": requested_mode,
                "merge_attempted": false,
                "merge_executed": false,
                "waiting_on": waiting_on,
            }),
            None,
        ),
    };

    // Effective merge_state: if we actually merged, surface "merged" rather
    // than "ready" so downstream consumers don't need to re-check.
    let effective_state = if merged_sha.is_some() {
        "merged"
    } else {
        merge_state
    };
    let pr_state = if merged_sha.is_some() {
        "MERGED"
    } else {
        pr_lifecycle_state_label(pr.state)
    };

    let status_patch = json!({
        "repo": repo,
        "pr_number": pr_number,
        "pr_state": pr_state,
        "merge_state": effective_state,
        "head_sha": pr.head_sha,
        "policy": policy.mode.as_str(),
        "dry_run": dry_run,
        "will_merge": will_merge,
        "requested_mode": requested_mode,
        "merge_attempted": merged_sha.is_some(),
        "merge_executed": merged_sha.is_some(),
        "head_consistency": {
            "head_sha": pr.head_sha,
            "checks_head_sha": null,
            "review_decision_head_sha": null,
            "head_consistent": verification_satisfies_head_consistency(verification_gate.as_ref(), policy),
            "state": if verification_satisfies_head_consistency(verification_gate.as_ref(), policy) {
                "verified_by_tachi_verification"
            } else {
                "unknown"
            },
            "requirement": policy.require_head_consistency,
            "source": if verification_satisfies_head_consistency(verification_gate.as_ref(), policy) {
                "verification_ledger"
            } else {
                "single_pr_snapshot"
            },
            "note": if verification_satisfies_head_consistency(verification_gate.as_ref(), policy) {
                "required verification ledger passed for the same PR head SHA; match-head-commit still pins the final merge command"
            } else {
                "gh_pr_checks does not expose independent head SHA data; match-head-commit still pins the final merge command"
            },
        },
        "checks": {
            "state": match pr.checks {
                ChecksState::None => "none",
                ChecksState::Pending => "pending",
                ChecksState::Skipped => "skipped",
                ChecksState::Success => "success",
                ChecksState::Failure => "failure",
            },
            "required": policy.require_checks,
            "allow_missing": policy.allow_missing_checks,
            "source": "gh_pr_checks",
            "head_consistent": false,
            "head_consistency_state": "unknown",
        },
        "review": {
            "state": match pr.review_decision {
                Some(ReviewDecision::Approved) => "approved",
                Some(ReviewDecision::ChangesRequested) => "changes_requested",
                Some(ReviewDecision::ReviewRequired) => "review_required",
                None if policy.require_review_approval => "unknown",
                None => "not_required",
            },
            "required": policy.require_review_approval,
            "allow_missing_decision": policy.allow_missing_review_decision,
        },
        "flow": {
            "flow_id": flow_id,
            "linked_issue_refs": pr.linked_issue_refs,
            "has_linked_issue": has_linked_issue,
            "required": policy.require_linked_issue_or_flow,
        },
        "verification": verification_gate,
    });

    let mut persisted = false;
    if let (Some(fid), Some(run_dir)) = (flow_id, flow_run_dir.as_ref()) {
        tokio::fs::create_dir_all(run_dir)
            .await
            .map_err(|e| format!("create run dir: {e}"))?;
        merge_github_status(run_dir, status_patch.clone())?;
        append_github_event(run_dir, fid, event_kind, event_payload.clone())?;
        persisted = true;
    }

    // Reclamation hook (disk governor, load-bearing slice for #484): after a
    // successful non-dry-run GitHub PR merge, reclaim the local worktree +
    // branch + target dir when the caller supplied a worktree path. This is
    // best-effort: a missing worktree (PR opened from a non-Tachi checkout)
    // logs a warning and never fails the merge. Mirrors the cleanup that
    // `tachi_task(action='merge')`/`approve_merge` already performs on the
    // local dispatch path.
    let reclamation = reclaim_worktree_after_merge(
        merged_sha.as_deref(),
        dry_run,
        worktree,
        reclaim_worktree,
        flow_id,
        flow_run_dir.as_deref(),
    )
    .await;

    serde_json::to_string(&json!({
        "tool": "tachi_gh_safe_merge",
        "repo": repo,
        "pr_number": pr_number,
        "decision": decision,
        "merge_state": effective_state,
        "mode": policy.mode.as_str(),
        "merged_sha": merged_sha,
        "dry_run": dry_run,
        "will_merge": will_merge,
        "requested_mode": requested_mode,
        "merge_attempted": merged_sha.is_some(),
        "merge_executed": merged_sha.is_some(),
        "flow_id": flow_id,
        "check_state_ingest": check_state_ingest,
        "persisted": persisted,
        "status_patch": status_patch,
        "event": {
            "kind": event_kind,
            "payload": event_payload,
        },
        "reclamation": reclamation,
    }))
    .map_err(|e| format!("serialize: {e}"))
}

async fn handle_already_merged_pr(
    repo: &str,
    pr_number: u64,
    pr: &PrState,
    dry_run: bool,
    flow_id: Option<&str>,
    policy: MergeGatePolicy,
    check_state_ingest: Option<Value>,
) -> Result<String, String> {
    let flow_run_dir = match flow_id {
        Some(fid) => Some(run_dir_for_flow_id(fid)?),
        None => None,
    };
    let requested_mode = if dry_run {
        "preview"
    } else {
        "merge_requested"
    };
    let status_patch = json!({
        "repo": repo,
        "pr_number": pr_number,
        "pr_state": "MERGED",
        "merge_state": "merged",
        "head_sha": pr.head_sha,
        "policy": policy.mode.as_str(),
        "dry_run": dry_run,
        "will_merge": false,
        "requested_mode": requested_mode,
        "merge_attempted": false,
        "merge_executed": false,
        "already_merged": true,
        "head_consistency": {
            "head_sha": pr.head_sha,
            "checks_head_sha": null,
            "review_decision_head_sha": null,
            "head_consistent": true,
            "state": "not_required_for_merged_pr",
            "requirement": policy.require_head_consistency,
            "source": "github_pr_state",
            "note": "PR is already merged, so safe_merge records the terminal state without re-running pre-merge head consistency gates",
        },
        "checks": {
            "state": match pr.checks {
                ChecksState::None => "none",
                ChecksState::Pending => "pending",
                ChecksState::Skipped => "skipped",
                ChecksState::Success => "success",
                ChecksState::Failure => "failure",
            },
            "required": policy.require_checks,
            "allow_missing": policy.allow_missing_checks,
            "source": "gh_pr_checks",
            "head_consistent": true,
            "head_consistency_state": "not_required_for_merged_pr",
        },
        "review": {
            "state": match pr.review_decision {
                Some(ReviewDecision::Approved) => "approved",
                Some(ReviewDecision::ChangesRequested) => "changes_requested",
                Some(ReviewDecision::ReviewRequired) => "review_required",
                None if policy.require_review_approval => "unknown",
                None => "not_required",
            },
            "required": policy.require_review_approval,
            "allow_missing_decision": policy.allow_missing_review_decision,
        },
        "flow": {
            "flow_id": flow_id,
            "linked_issue_refs": pr.linked_issue_refs,
            "has_linked_issue": !pr.linked_issue_refs.is_empty(),
            "required": policy.require_linked_issue_or_flow,
        },
        "verification": null,
    });
    let event_payload = json!({
        "repo": repo,
        "pr_number": pr_number,
        "head_sha": pr.head_sha,
        "requested_mode": requested_mode,
        "merge_attempted": false,
        "merge_executed": false,
        "dry_run": dry_run,
        "already_merged": true,
    });

    let mut persisted = false;
    if let (Some(fid), Some(run_dir)) = (flow_id, flow_run_dir.as_ref()) {
        tokio::fs::create_dir_all(run_dir)
            .await
            .map_err(|e| format!("create run dir: {e}"))?;
        merge_github_status(run_dir, status_patch.clone())?;
        append_github_event(run_dir, fid, "github_pr_merged", event_payload.clone())?;
        persisted = true;
    }

    serde_json::to_string(&json!({
        "tool": "tachi_gh_safe_merge",
        "repo": repo,
        "pr_number": pr_number,
        "decision": {"decision": "already_merged"},
        "merge_state": "merged",
        "mode": policy.mode.as_str(),
        "merged_sha": null,
        "dry_run": dry_run,
        "will_merge": false,
        "requested_mode": requested_mode,
        "merge_attempted": false,
        "merge_executed": false,
        "already_merged": true,
        "flow_id": flow_id,
        "check_state_ingest": check_state_ingest,
        "persisted": persisted,
        "status_patch": status_patch,
        "event": {
            "kind": "github_pr_merged",
            "payload": event_payload,
        },
    }))
    .map_err(|e| format!("serialize: {e}"))
}

fn pr_lifecycle_state_label(state: PrLifecycleState) -> &'static str {
    match state {
        PrLifecycleState::Open => "OPEN",
        PrLifecycleState::Closed => "CLOSED",
        PrLifecycleState::Merged => "MERGED",
    }
}

/// Reclaim a local worktree after a successful GitHub PR merge. The reclamation
/// is gated on all of: a real merge occurred (`merge_sha.is_some()` +
/// `!dry_run`), `reclaim_worktree` is true, and the caller supplied a worktree
/// path. The cleaner is best-effort: a worktree that does not exist locally
/// (the PR was opened from a non-Tachi checkout) logs a warning and continues
/// — it never fails the merge.
///
/// Returns a JSON object describing the outcome so the handler can surface it
/// in the response envelope. When `run_dir` is present, also appends a
/// reclamation event to the flow ledger: `github_safe_merge_reclaimed` for a
/// genuine success (`reclaimed: true`), or `github_safe_merge_reclaim_skipped`
/// for any skip or failure (worktree_missing, cleaner error, `reclaimed: false`).
async fn reclaim_worktree_after_merge(
    merge_sha: Option<&str>,
    dry_run: bool,
    worktree: Option<&str>,
    reclaim_worktree: bool,
    flow_id: Option<&str>,
    run_dir: Option<&std::path::Path>,
) -> Value {
    // No merge happened (dry-run, blocked, pending, or already-merged): nothing
    // to reclaim, and dry-run MUST NOT reclaim.
    if dry_run || merge_sha.is_none() {
        return json!({
            "attempted": false,
            "reclaimed": false,
            "skipped": if dry_run { "dry_run" } else { "no_merge" },
        });
    }
    // Operator opted out, or no worktree was supplied (no PR→worktree mapping).
    if !reclaim_worktree {
        return json!({
            "attempted": false,
            "reclaimed": false,
            "skipped": "reclaim_disabled",
        });
    }
    let Some(worktree_path) = worktree.map(str::trim).filter(|s| !s.is_empty()) else {
        return json!({
            "attempted": false,
            "reclaimed": false,
            "skipped": "no_worktree_mapped",
        });
    };

    // Best-effort: a missing worktree is a warning, not an error. The PR may
    // have been opened from a non-Tachi checkout with no local worktree to
    // reclaim.
    let path = std::path::Path::new(worktree_path);
    if !path.exists() {
        tracing::warn!(
            worktree = worktree_path,
            "safe_merge reclamation skipped: worktree does not exist locally (best-effort)"
        );
        let detail = json!({
            "attempted": false,
            "reclaimed": false,
            "skipped": "worktree_missing",
            "worktree": worktree_path,
            "warning": "worktree does not exist locally; nothing to reclaim",
        });
        record_reclamation_event(flow_id, run_dir, &detail);
        return detail;
    }

    let attempt = crate::dispatch_ops::remove_worktree_with_cleaner(worktree_path).await;
    let detail = match attempt {
        Ok(report) => json!({
            "attempted": true,
            "reclaimed": report.removed,
            "worktree": worktree_path,
            "warnings": report.warnings,
            "errors": report.errors,
        }),
        Err(err) => {
            tracing::warn!(
                worktree = worktree_path,
                error = %err,
                "safe_merge reclamation failed (best-effort); merge already succeeded"
            );
            json!({
                "attempted": true,
                "reclaimed": false,
                "worktree": worktree_path,
                "error": err,
            })
        }
    };
    record_reclamation_event(flow_id, run_dir, &detail);
    detail
}

/// Append a reclamation event to the flow ledger when a flow run dir is
/// present. The event kind reflects the outcome so readers filtering by kind
/// are not misled:
///   - genuine success (`reclaimed: true`) → `github_safe_merge_reclaimed`
///   - skip (`worktree_missing`) OR cleaner-error OR `reclaimed: false` →
///     `github_safe_merge_reclaim_skipped`
///
/// Errors here are logged but never propagate — the merge already succeeded
/// and reclamation is best-effort.
fn record_reclamation_event(
    flow_id: Option<&str>,
    run_dir: Option<&std::path::Path>,
    detail: &Value,
) {
    let (Some(fid), Some(dir)) = (flow_id, run_dir) else {
        return;
    };
    let reclaimed = detail.get("reclaimed").and_then(Value::as_bool) == Some(true);
    let kind = if reclaimed {
        "github_safe_merge_reclaimed"
    } else {
        "github_safe_merge_reclaim_skipped"
    };
    if let Err(err) = append_github_event(dir, fid, kind, detail.clone()) {
        tracing::warn!(
            flow_id = fid,
            error = %err,
            "failed to append {kind} event (best-effort)"
        );
    }
}

fn record_tests_run_verification(
    flow_id: &str,
    repo: &str,
    pr_number: u64,
    head_sha: &str,
    tests_run: &[String],
) -> Result<(), String> {
    let params = TachiVerifyParams {
        action: "record".to_string(),
        format: Some("json".to_string()),
        flow_id: Some(flow_id.to_string()),
        pr_ref: Some(format!("{repo}#{pr_number}")),
        head_sha: Some(head_sha.to_string()),
        check_id: None,
        kind: None,
        command: None,
        commands: tests_run.to_vec(),
        status: Some("passed".to_string()),
        exit_code: Some(0),
        log_path: None,
        summary: Some("safe_merge recorded caller-supplied tests_run evidence".to_string()),
        cwd: None,
        required: Some(true),
        limit: None,
        checks: Vec::new(),
    };
    record_verification_items(&params, "passed")
        .map(|_| ())
        .map_err(|err| format!("record tests_run verification: {err}"))
}
