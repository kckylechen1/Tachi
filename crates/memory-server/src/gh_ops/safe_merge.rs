use super::*;

// ─── safe_merge: CliGhClient + orchestrator ─────────────────────────────────

pub(super) fn parse_merge_strategy(raw: Option<&str>) -> Result<MergeStrategy, String> {
    match raw.unwrap_or("squash").to_ascii_lowercase().as_str() {
        "squash" => Ok(MergeStrategy::Squash),
        "merge" => Ok(MergeStrategy::Merge),
        "rebase" => Ok(MergeStrategy::Rebase),
        other => Err(format!(
            "invalid merge_strategy '{}' (allowed: squash, merge, rebase)",
            other
        )),
    }
}

pub(super) fn effective_safe_merge_dry_run(confirm: bool, requested_dry_run: Option<bool>) -> bool {
    !confirm || requested_dry_run.unwrap_or(false)
}

pub(super) fn verification_satisfies_head_consistency(
    gate: Option<&Value>,
    policy: MergeGatePolicy,
) -> bool {
    policy.require_head_consistency
        && gate.and_then(|v| v.get("overall")).and_then(Value::as_str) == Some("passed")
}

pub(super) fn apply_verification_gate_to_decision(
    decision: MergeDecision,
    gate: Option<&Value>,
) -> MergeDecision {
    let Some(gate) = gate else {
        return decision;
    };
    let reasons: Vec<String> = gate
        .get("reasons")
        .and_then(Value::as_array)
        .map(|rows| {
            rows.iter()
                .filter_map(Value::as_str)
                .map(ToString::to_string)
                .collect()
        })
        .unwrap_or_default();
    if !reasons.is_empty() {
        return match decision {
            MergeDecision::Blocked {
                reasons: mut existing,
            } => {
                existing.extend(reasons);
                existing.sort();
                existing.dedup();
                MergeDecision::Blocked { reasons: existing }
            }
            _ => MergeDecision::Blocked { reasons },
        };
    }

    let waiting_on: Vec<String> = gate
        .get("waiting_on")
        .and_then(Value::as_array)
        .map(|rows| {
            rows.iter()
                .filter_map(Value::as_str)
                .map(ToString::to_string)
                .collect()
        })
        .unwrap_or_default();
    if waiting_on.is_empty() {
        return decision;
    }
    match decision {
        MergeDecision::Ready => MergeDecision::Pending { waiting_on },
        MergeDecision::Pending {
            waiting_on: mut existing,
        } => {
            existing.extend(waiting_on);
            existing.sort();
            existing.dedup();
            MergeDecision::Pending {
                waiting_on: existing,
            }
        }
        blocked => blocked,
    }
}

pub(super) fn parse_merge_gate_policy(raw: Option<&str>) -> Result<MergeGatePolicy, String> {
    let mode = match raw
        .unwrap_or("standard")
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "" | "standard" => MergeGatePolicyMode::Standard,
        "permissive" => MergeGatePolicyMode::Permissive,
        "strict" => MergeGatePolicyMode::Strict,
        other => {
            return Err(format!(
                "invalid merge_policy '{}' (allowed: permissive, standard, strict)",
                other
            ))
        }
    };
    Ok(MergeGatePolicy::from_mode(mode))
}

pub(super) fn merge_strategy_flag(s: MergeStrategy) -> &'static str {
    match s {
        MergeStrategy::Squash => "--squash",
        MergeStrategy::Merge => "--merge",
        MergeStrategy::Rebase => "--rebase",
    }
}

/// Map a `gh` CLI failure string into a typed `GhError`. The input is already
/// sanitized by `run_gh`. We classify by substring so callers can distinguish
/// "PR doesn't exist" (NotFound, terminal) from "API rate limit" (transient).
pub(super) fn classify_gh_error(raw: &str) -> GhError {
    let lower = raw.to_ascii_lowercase();
    if lower.contains("could not resolve") || lower.contains("not found") || lower.contains("404") {
        GhError::NotFound(raw.to_string())
    } else if lower.contains("rate limit") || lower.contains("403") && lower.contains("rate") {
        GhError::RateLimited(raw.to_string())
    } else {
        GhError::Sanitized(raw.to_string())
    }
}

pub(super) fn is_no_checks_reported(raw: &str) -> bool {
    raw.to_ascii_lowercase().contains("no checks reported")
}

/// Production `GhClient` that wraps the sanitized `gh` subprocess pipeline
/// already used by the rest of `tachi_gh`. Holds a borrowed `&MemoryServer`
/// so the vault token is read fresh per call.
pub(crate) struct CliGhClient<'a> {
    pub(crate) server: &'a MemoryServer,
}

impl<'a> CliGhClient<'a> {
    fn build(&self) -> Result<(Command, String), GhError> {
        build_gh_command(self.server).map_err(|e| GhError::Sanitized(e))
    }
}

#[async_trait]
impl<'a> GhClient for CliGhClient<'a> {
    async fn pr_view(&self, repo: &str, number: u64) -> Result<PrState, GhError> {
        validate_repo(repo).map_err(GhError::Sanitized)?;
        let (mut cmd, token) = self.build()?;
        cmd.args(["pr", "view", &number.to_string()])
            .args(["--repo", repo])
            .args([
                "--json",
                "number,state,mergeable,reviewDecision,isDraft,headRefOid,closingIssuesReferences",
            ]);
        let raw = run_gh(cmd, &token).map_err(|e| classify_gh_error(&e))?;
        let v: serde_json::Value = serde_json::from_str(&raw)
            .map_err(|e| GhError::Sanitized(format!("pr_view parse: {e}")))?;
        let checks = self.checks_list(repo, number).await?;
        Ok(parse_pr_view_json(&v, checks)
            .map_err(|e| GhError::Sanitized(format!("pr_view shape: {e}")))?)
    }

    async fn pr_merge(
        &self,
        repo: &str,
        number: u64,
        strategy: MergeStrategy,
        expected_head_sha: &str,
    ) -> Result<MergeResult, GhError> {
        validate_repo(repo).map_err(GhError::Sanitized)?;
        let (mut cmd, token) = self.build()?;
        cmd.args(["pr", "merge", &number.to_string()])
            .args(["--repo", repo])
            .arg(merge_strategy_flag(strategy));
        if !expected_head_sha.trim().is_empty() {
            cmd.args(["--match-head-commit", expected_head_sha]);
        }
        let _out = run_gh(cmd, &token).map_err(|e| classify_gh_error(&e))?;
        // gh pr merge prints a status line, not JSON. Re-fetch the merged SHA.
        let (mut cmd2, token2) = self.build()?;
        cmd2.args(["pr", "view", &number.to_string()])
            .args(["--repo", repo])
            .args(["--json", "mergeCommit"]);
        let sha_raw = run_gh(cmd2, &token2).map_err(|e| classify_gh_error(&e))?;
        let merge_sha = serde_json::from_str::<serde_json::Value>(&sha_raw)
            .ok()
            .and_then(|v| {
                v.get("mergeCommit")
                    .and_then(|mc| mc.get("oid"))
                    .and_then(|o| o.as_str())
                    .map(|s| s.to_string())
            })
            .unwrap_or_default();
        Ok(MergeResult {
            pr_number: number,
            merge_sha,
            strategy,
        })
    }

    async fn issue_create(
        &self,
        repo: &str,
        title: &str,
        body: Option<&str>,
        labels: &[String],
    ) -> Result<crate::gh_safe_merge::IssueState, GhError> {
        validate_repo(repo).map_err(GhError::Sanitized)?;
        let (mut cmd, token) = self.build()?;
        cmd.args(["issue", "create"])
            .args(["--repo", repo])
            .args(["--title", title]);
        if let Some(b) = body {
            cmd.args(["--body", b]);
        }
        for l in labels {
            cmd.args(["--label", l]);
        }
        let url = run_gh(cmd, &token)
            .map_err(|e| classify_gh_error(&e))?
            .trim()
            .to_string();
        // `gh issue create` prints the issue URL; derive the number from the trailing path segment.
        let number = url
            .rsplit('/')
            .next()
            .and_then(|s| s.parse::<u64>().ok())
            .ok_or_else(|| {
                GhError::Sanitized(format!(
                    "gh issue create returned an unparseable issue URL: {url}"
                ))
            })?;
        Ok(crate::gh_safe_merge::IssueState {
            number,
            title: title.to_string(),
            state: "OPEN".to_string(),
            url,
        })
    }

    async fn checks_list(
        &self,
        repo: &str,
        pr_number: u64,
    ) -> Result<Vec<crate::gh_safe_merge::CheckRun>, GhError> {
        validate_repo(repo).map_err(GhError::Sanitized)?;
        // `gh pr checks` may exit non-zero when checks have failed; we still
        // want to parse the JSON. Run it directly and tolerate non-zero exit
        // when stdout looks like a JSON array.
        let (mut raw_cmd, token) = self.build()?;
        raw_cmd
            .args(["pr", "checks", &pr_number.to_string()])
            .args(["--repo", repo])
            .arg("--json")
            .arg("name,state,bucket");
        let output = raw_cmd
            .output()
            .map_err(|e| GhError::Sanitized(format!("gh exec: {e}")))?;
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        let sanitized = sanitize_output(&stdout, &token);
        let sanitized_stderr = sanitize_output(&stderr, &token);
        let trimmed = sanitized.trim();
        if trimmed.is_empty() || trimmed == "null" {
            if output.status.success() {
                return Ok(Vec::new());
            }
            if is_no_checks_reported(&sanitized_stderr) {
                return Ok(Vec::new());
            }
            return Err(classify_gh_error(&format!(
                "gh pr checks failed: {}",
                sanitized_stderr.trim()
            )));
        }
        if !trimmed.starts_with('[') {
            return Err(classify_gh_error(&format!(
                "gh pr checks returned non-json output: {} {}",
                trimmed,
                sanitized_stderr.trim()
            )));
        }
        let arr: Vec<serde_json::Value> = serde_json::from_str(trimmed)
            .map_err(|e| GhError::Sanitized(format!("checks_list parse: {e}")))?;
        Ok(arr
            .into_iter()
            .map(|v| {
                let name = v
                    .get("name")
                    .and_then(|n| n.as_str())
                    .unwrap_or_default()
                    .to_string();
                // `gh pr checks --json` exposes `bucket` ∈ pass/fail/pending/skipping/cancel
                // and `state` for the raw check status. We map bucket → conclusion
                // and synthesize a `completed`/`in_progress` status.
                let bucket = v
                    .get("bucket")
                    .and_then(|b| b.as_str())
                    .unwrap_or("")
                    .to_string();
                let (status, conclusion) = match bucket.as_str() {
                    "pass" => ("completed".to_string(), Some("success".to_string())),
                    "fail" => ("completed".to_string(), Some("failure".to_string())),
                    "cancel" => ("completed".to_string(), Some("cancelled".to_string())),
                    "skipping" => ("completed".to_string(), Some("skipped".to_string())),
                    "pending" | "" => ("in_progress".to_string(), None),
                    _ => ("completed".to_string(), Some(bucket.clone())),
                };
                crate::gh_safe_merge::CheckRun {
                    name,
                    conclusion,
                    status,
                }
            })
            .collect())
    }
}

/// Parse the `gh pr view --json number,state,mergeable,reviewDecision,isDraft,headRefOid,closingIssuesReferences`
/// payload into a `PrState`. Pulled out as a free function so unit tests can
/// exercise the JSON shape without spawning `gh`.
pub(super) fn parse_pr_view_json(
    v: &serde_json::Value,
    checks: Vec<crate::gh_safe_merge::CheckRun>,
) -> Result<PrState, String> {
    let number = v
        .get("number")
        .and_then(|n| n.as_u64())
        .ok_or("pr_view: missing number")?;
    let state_raw = v
        .get("state")
        .and_then(|s| s.as_str())
        .ok_or("pr_view: missing state")?;
    let state = match state_raw {
        "OPEN" => PrLifecycleState::Open,
        "CLOSED" => PrLifecycleState::Closed,
        "MERGED" => PrLifecycleState::Merged,
        other => return Err(format!("pr_view: unknown state '{}'", other)),
    };
    let mergeable = match v.get("mergeable").and_then(|m| m.as_str()).unwrap_or("") {
        "MERGEABLE" => Mergeable::Mergeable,
        "CONFLICTING" => Mergeable::Conflicting,
        _ => Mergeable::Unknown,
    };
    let review_decision = v
        .get("reviewDecision")
        .and_then(|r| r.as_str())
        .and_then(|s| match s {
            "APPROVED" => Some(ReviewDecision::Approved),
            "CHANGES_REQUESTED" => Some(ReviewDecision::ChangesRequested),
            "REVIEW_REQUIRED" => Some(ReviewDecision::ReviewRequired),
            "" => None,
            _ => Some(ReviewDecision::ReviewRequired),
        });
    let is_draft = v.get("isDraft").and_then(|d| d.as_bool()).unwrap_or(false);
    let head_sha = v
        .get("headRefOid")
        .and_then(|h| h.as_str())
        .unwrap_or_default()
        .to_string();
    let linked_issue_refs = v
        .get("closingIssuesReferences")
        .and_then(|refs| refs.as_array())
        .map(|refs| {
            refs.iter()
                .filter_map(|issue| {
                    if let Some(url) = issue.get("url").and_then(|u| u.as_str()) {
                        Some(url.to_string())
                    } else {
                        issue
                            .get("number")
                            .and_then(|n| n.as_u64())
                            .map(|n| format!("#{n}"))
                    }
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    Ok(PrState {
        number,
        state,
        mergeable,
        review_decision,
        checks: ChecksState::aggregate(&checks),
        is_draft,
        head_sha,
        linked_issue_refs,
        head_consistent: None,
    })
}

/// Orchestrate one `tachi_gh safe_merge` invocation:
/// 1. Pull PR + checks via the `GhClient`.
/// 2. Run the pure `evaluate_merge_gate`.
/// 3. If `Ready` and `!dry_run`, call `pr_merge` (squash by default).
/// 4. When `flow_id` is present, persist `merge_state` + reasons into
///    `status.json::github` and append the matching event to `events.jsonl`.
/// 5. Return a JSON envelope the agent can render directly.
pub(crate) async fn handle_github_safe_merge<C: GhClient + ?Sized>(
    client: &C,
    repo: &str,
    pr_number: u64,
    strategy: MergeStrategy,
    dry_run: bool,
    flow_id: Option<&str>,
    policy: MergeGatePolicy,
) -> Result<String, String> {
    let flow_run_dir = match flow_id {
        Some(fid) => Some(run_dir_for_flow_id(fid)?),
        None => None,
    };
    let mut pr = client
        .pr_view(repo, pr_number)
        .await
        .map_err(|e| format!("pr_view failed: {e}"))?;
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

    let status_patch = json!({
        "repo": repo,
        "pr_number": pr_number,
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
        "persisted": persisted,
        "status_patch": status_patch,
        "event": {
            "kind": event_kind,
            "payload": event_payload,
        },
    }))
    .map_err(|e| format!("serialize: {e}"))
}
