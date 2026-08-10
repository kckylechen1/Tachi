use super::*;

pub(crate) async fn handle_tachi_gh(
    server: &MemoryServer,
    params: TachiGhParams,
) -> Result<String, String> {
    let action = params.action.clone();
    let raw = match action.as_str() {
        "repo_view" => {
            handle_gh_repo_view(
                server,
                GhRepoViewParams {
                    repo: required_repo(&params, "repo_view")?,
                },
            )
            .await
        }
        "issue_list" => {
            // gh CLI accepts `--label` multiple times or a single comma-separated
            // value. We normalize to comma-separated to preserve all labels the
            // caller passed; taking `.first()` silently dropped extras.
            let labels_csv = if params.labels.is_empty() {
                None
            } else {
                Some(params.labels.join(","))
            };
            handle_gh_issue_list(
                server,
                GhIssueListParams {
                    repo: required_repo(&params, "issue_list")?,
                    state: params.state.unwrap_or_else(|| "open".to_string()),
                    labels: labels_csv,
                    limit: params.limit.unwrap_or(30),
                },
            )
            .await
        }
        "issue_read" => {
            let number = params.number.ok_or("issue_read requires 'number' parameter")?;
            handle_gh_issue_read(
                server,
                GhIssueReadParams {
                    repo: required_repo(&params, "issue_read")?,
                    issue_number: number,
                },
            )
            .await
        }
        "issue_create" => {
            let repo = required_repo(&params, "issue_create")?;
            let title = params.title.ok_or("issue_create requires 'title' parameter")?;
            handle_gh_issue_create(
                server,
                GhIssueCreateParams {
                    repo,
                    title,
                    body: params.body,
                    labels: params.labels,
                },
            )
            .await
        }
        "issue_comment" => {
            let number = params
                .number
                .ok_or("issue_comment requires 'number' parameter")?;
            handle_gh_comment(
                server,
                "issue",
                GhCommentParams {
                    repo: required_repo(&params, "issue_comment")?,
                    number,
                    body: params.body,
                    dry_run: params.dry_run.unwrap_or(false),
                },
            )
            .await
        }
        "issue_label" => {
            let number = params
                .number
                .ok_or("issue_label requires 'number' parameter")?;
            handle_gh_label(
                server,
                GhLabelParams {
                    repo: required_repo(&params, "issue_label")?,
                    number,
                    labels: params.labels.clone(),
                    mode: params.label_mode.clone(),
                },
            )
            .await
        }
        "issue_freshness_scan" => handle_issue_freshness_scan(server, &params).await,
        "pr_comment" => {
            let target = resolve_tachi_gh_pr_target(&params, "pr_comment")?;
            handle_gh_comment(
                server,
                "pr",
                GhCommentParams {
                    repo: target.repo,
                    number: target.number,
                    body: params.body,
                    dry_run: params.dry_run.unwrap_or(false),
                },
            )
            .await
        }
        "pr_list" => {
            handle_gh_pr_list(
                server,
                GhPrListParams {
                    repo: required_repo(&params, "pr_list")?,
                    state: params.state.unwrap_or_else(|| "open".to_string()),
                    limit: params.limit.unwrap_or(30),
                },
            )
            .await
        }
        "pr_read" => {
            let target = resolve_tachi_gh_pr_target(&params, "pr_read")?;
            handle_gh_pr_read(
                server,
                GhPrReadParams {
                    repo: target.repo,
                    pr_number: target.number,
                },
            )
            .await
        }
        "pr_comments" => {
            let target = resolve_tachi_gh_pr_target(&params, "pr_comments")?;
            handle_gh_pr_comments(
                server,
                GhPrCommentsParams {
                    repo: target.repo,
                    pr_number: target.number,
                },
            )
            .await
        }
        "pr_review_digest" => {
            let target = resolve_tachi_gh_pr_target(&params, "pr_review_digest")?;
            handle_gh_pr_review_digest(
                server,
                GhPrCommentsParams {
                    repo: target.repo,
                    pr_number: target.number,
                },
                params.author_filter,
                params.write_digest.unwrap_or(true),
            )
            .await
        }
        "safe_merge" => {
            let target = resolve_tachi_gh_pr_target(&params, "safe_merge")?;
            let strategy = parse_merge_strategy(params.merge_strategy.as_deref())?;
            let policy = merge_gate_policy_from_params(
                params.merge_policy.as_deref(),
                params.allow_umbrella_close,
            )?;
            let client = gh_client_for_server(server)?;
            // Resolve the spelling before the cleaner can delete the path; after
            // deletion canonicalization is impossible, but lease reclaim must
            // still select the same canonical row used by holder evidence.
            let worktree_for_lease = params
                .worktree
                .as_deref()
                .map(crate::exec_env_ops::canonical_worktree_path)
                .transpose()?;
            let holder_gate = |worktree: &str| worktree_holder_gate(server, worktree);
            let out = handle_github_safe_merge_with_holder_gate(
                &client,
                &target.repo,
                target.number,
                strategy,
                effective_safe_merge_dry_run(params.confirm, params.dry_run),
                params.flow_id.as_deref(),
                &params.tests_run,
                policy,
                worktree_for_lease.as_deref(),
                params.reclaim_worktree.unwrap_or(true),
                &holder_gate,
            )
            .await?;
            // #894 S1: the exec_envs lease is the single owner of a managed env.
            // When safe_merge reclaimed the local worktree, flip the lease
            // through the one reclaim path. A failed transition is surfaced:
            // discarding it after physical deletion would falsely report a
            // complete reclaim while the durable lifecycle still says active.
            if let Some(worktree) = worktree_for_lease.as_deref() {
                if safe_merge_reclaimed_worktree(&out) {
                    server
                        .reclaim_exec_env_for_worktree(worktree, Some("safe_merge"))
                        .map_err(|err| format!("safe_merge cleaner removed {worktree}, but ExecEnv reclaim refused: {err}"))?;
                }
            }
            Ok(out)
        }
        "ship" => handle_github_ship(server, &params).await,
        "close_loop" => {
            let dry_run = params.dry_run.unwrap_or(false);
            let result = crate::workflow_closure::handle_close_loop(server, params.clone()).await?;
            // The closure handler has completed the wiki write (and its
            // pattern feedback/comment fan-out) before this marker is allowed
            // to advance the lifecycle. Preview and failed closure paths never
            // reach this branch.
            if !dry_run {
                if let Some(flow_id) = params
                    .flow_id
                    .as_deref()
                    .map(str::trim)
                    .filter(|id| !id.is_empty())
                {
                    crate::task_lifecycle::mark_task_close_loop(flow_id, &result)?;
                }
            }
            Ok(result)
        }
        "link_pr" => {
            let task_params = lifecycle_task_params(&params)?;
            Box::pin(crate::task_lifecycle::handle_task_link_pr(
                server,
                &task_params,
            ))
            .await
        }
        "pr_status" => {
            let task_params = lifecycle_task_params(&params)?;
            let target = crate::task_lifecycle::resolve_task_pr_target(&task_params).map_err(|_| {
                "pr_status requires either repo+number or pr_ref='owner/repo#123' / GitHub PR URL"
                    .to_string()
            })?;
            let policy = merge_gate_policy_from_params(
                task_params.merge_policy.as_deref(),
                task_params.allow_umbrella_close,
            )?;
            let client = gh_client_for_server(server)?;
            handle_github_safe_merge(
                &client,
                &target.repo,
                target.number,
                MergeStrategy::Squash,
                true,
                task_params.flow_id.as_deref(),
                &[],
                policy,
                // pr_status is a dry-run preview: reclamation never fires.
                None,
                false,
            )
            .await
        }
        "pr_handoff" => {
            let task_params = lifecycle_task_params(&params)?;
            crate::task_lifecycle::handle_task_pr_handoff(&task_params)
        }
        "release_note" => {
            let task_params = lifecycle_task_params(&params)?;
            Box::pin(crate::task_lifecycle::handle_task_release_note(
                server,
                &task_params,
            ))
            .await
        }
        "handoff_draft" => {
            let repo = required_repo(&params, "handoff_draft")?;
            handle_gh_handoff_draft(server, &params, repo)
        }
        "handoff_publish" => {
            let repo = required_repo(&params, "handoff_publish")?;
            Box::pin(handle_gh_handoff_publish(server, &params, repo)).await
        }
        "handoff_repair" => {
            let repo = required_repo(&params, "handoff_repair")?;
            let number = params
                .number
                .ok_or("handoff_repair requires 'number' parameter")?;
            Box::pin(handle_gh_handoff_repair(server, repo, number)).await
        }
        other => Err(format!(
            "Unknown action '{}'. Expected: repo_view, issue_list, issue_read, issue_create, issue_comment, issue_label, issue_freshness_scan, pr_list, pr_read, pr_comments, pr_comment, pr_review_digest, safe_merge, ship, close_loop, link_pr, pr_status, pr_handoff, release_note, handoff_draft, handoff_publish, handoff_repair",
            other
        )),
    }?;
    normalize_gh_response(&action, &raw, params.format.as_deref())
}

/// Read-only WorkClaim gate used immediately before safe-merge invokes the
/// external cleaner. Missing active leases are legacy/not-applicable; every
/// held or uncertain holder answer is a loud refusal.
pub(crate) fn worktree_holder_gate(
    server: &MemoryServer,
    worktree_path: &str,
) -> Result<(), String> {
    let canonical_path = crate::exec_env_ops::canonical_worktree_path(worktree_path)?;
    server.with_global_store_read(|store| {
        let Some(lease) =
            memcore::find_active_exec_env_by_path(store.connection(), &canonical_path)
            .map_err(|err| format!("holder evidence unavailable while locating ExecEnv: {err}"))?
        else {
            return Ok(());
        };
        match memcore::holder_evidence(store.connection(), &lease.env_id)
            .map_err(|err| format!("holder evidence unavailable for ExecEnv {}: {err}", lease.env_id))?
        {
            memcore::HolderEvidence::Clear | memcore::HolderEvidence::NotApplicable => Ok(()),
            evidence => Err(format!(
                "refusing external cleaner for {worktree_path}: persisted WorkClaim holder evidence is {evidence:?}"
            )),
        }
    })
}

/// #1000 issue-freshness scan: three independent detectors (zombie / stale
/// gate+anchor / same-surface churn), each writing its OWN candidate rowset
/// and reaping its own kind on every run.
///
/// Frozen posture (codex review finding 5, "圈候选不判决"): NONE of these are
/// verdicts. Zombie hits are "candidate: fixed-awaiting-closure" claims;
/// stale/churn hits are pure review-queue reasons. Closing an issue stays a
/// leader/owner action on GitHub itself — this handler never calls
/// `issue_close` or similar.
///
/// Error-arm honesty (codex review finding 6): the zombie arm is the primary
/// signal this action exists for — a `gh` failure there is surfaced as a hard
/// error (`?`), not swallowed. The stale/churn arms are best-effort
/// enhancements (they need a resolvable repo root / activity cutoff that may
/// not always be available) — a failure there degrades the scan to
/// zombies-only, but LOUDLY: the failure reason is captured in
/// `stale_scan_error`/`churn_scan_error` fields on the response, never
/// silently swallowed into an empty vec the caller can't distinguish from
/// "scanned, found nothing".
///
/// Reap honesty (round-3 codex review finding 3): a DB error while reaping a
/// kind's stale rows used to be swallowed by `.unwrap_or(0)` — indistinguish-
/// able from "nothing needed reaping" even though a closed zombie or fixed
/// stale/churn candidate's ghost row is still sitting in the briefing. Every
/// reap call now surfaces its error into `reap_errors` and marks the kind in
/// `reap_incomplete_kinds`, so a caller can tell "reaped 0 because clean" from
/// "reaped 0 because the DB call itself failed".
///
/// Run one kind's `reap_stale_kind_rows` call and record the outcome
/// consistently. Round-3 finding 3 wired this Ok/Err bookkeeping inline at
/// each of the three call sites (zombie/stale_candidate/churn_candidate);
/// that duplication is exactly how the zombie arm drifted out of sync with
/// the other two in the first place — its `Err` branch pushed to
/// `reap_errors` but the matching `reap_incomplete_kinds.push(KIND_ZOMBIE)`
/// was missing, so a caller checking only `reap_incomplete_kinds` (the
/// documented "which kind's ghost rows may still be showing" signal) saw an
/// empty list and treated a failed zombie reap as a clean one (PR #1004
/// round-4 codex review — the merge-blocker). Centralizing the bookkeeping
/// here means every kind gets the same treatment by construction, not by
/// three separately-maintained copy-pasted match arms. `reap_fn` is a
/// closure rather than the concrete `reap_stale_kind_rows` call so this is
/// unit-testable with an injected `Err` without touching a real DB (see
/// `record_reap_outcome_marks_kind_incomplete_on_error` below).
fn record_reap_outcome(
    kind: &'static str,
    reap_fn: impl FnOnce() -> Result<usize, String>,
    reap_errors: &mut Vec<String>,
    reap_incomplete_kinds: &mut Vec<&'static str>,
) -> usize {
    match reap_fn() {
        Ok(n) => n,
        Err(e) => {
            reap_errors.push(format!("{kind}: {e}"));
            reap_incomplete_kinds.push(kind);
            0
        }
    }
}

async fn handle_issue_freshness_scan(
    server: &MemoryServer,
    params: &TachiGhParams,
) -> Result<String, String> {
    let repo = required_repo(params, "issue_freshness_scan")?;
    let limit = params.scan_limit.unwrap_or(100);

    // Resolved once, up front, so both the zombie arm's merge-commit-message
    // lookup (round-3 finding 1) and the stale arm's file-anchor probes share
    // the same local-checkout notion of "repo root".
    let repo_root = params
        .cwd
        .as_deref()
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_default();

    // Zombie arm: hard-fails the whole action on a `gh` error — this is the
    // scan's primary signal, not a best-effort extra (finding 6).
    let zombies = crate::gh_ops::fetch_and_scan_zombies(server, &repo, limit, Some(&repo_root))?;

    let mut save_errors = Vec::new();
    let mut reap_errors = Vec::new();
    let mut zombie_refs = Vec::with_capacity(zombies.len());
    for hit in &zombies {
        let issue_ref = format!("{repo}#{}", hit.issue_number);
        zombie_refs.push(issue_ref.clone());
        let row = crate::gh_ops::FreshnessRow {
            issue_ref: issue_ref.clone(),
            kind: crate::gh_ops::KIND_ZOMBIE.to_string(),
            verified_at_sha: hit.merge_commit_sha.clone().unwrap_or_default(),
            evidence_refs: vec![format!("{repo}#{}", hit.pr_number)],
            checked_at: chrono::Utc::now().to_rfc3339(),
        };
        if let Err(e) = crate::gh_ops::save_freshness_row(server, crate::gh_ops::ZOMBIE_NS, &row) {
            save_errors.push(format!("{issue_ref}: {e}"));
        }
    }
    // Each scan is authoritative for its own kind (finding 7): any
    // previously-saved zombie row not reproduced by THIS scan (the leader
    // closed it, or it otherwise stopped being a zombie) is reaped so it
    // does not live forever as a ghost row in the briefing.
    //
    // Round-3 finding 3: a reap failure (DB list/delete error) used to be
    // swallowed by `.unwrap_or(0)` — a closed zombie whose row failed to
    // delete would silently stay `zombie_reaped == 0`, indistinguishable
    // from "nothing needed reaping", while the ghost row keeps surfacing in
    // the briefing with no error anywhere in the response. `record_reap_
    // outcome` (defined above `handle_issue_freshness_scan`) now surfaces
    // the error in `reap_errors` AND marks the kind in
    // `reap_incomplete_kinds` for all three arms uniformly.
    let mut reap_incomplete_kinds: Vec<&'static str> = Vec::new();
    let zombie_reaped = record_reap_outcome(
        crate::gh_ops::KIND_ZOMBIE,
        || {
            crate::gh_ops::reap_stale_kind_rows(
                server,
                crate::gh_ops::ZOMBIE_NS,
                crate::gh_ops::KIND_ZOMBIE,
                &zombie_refs,
            )
        },
        &mut reap_errors,
        &mut reap_incomplete_kinds,
    );

    // Stale gate+anchor arm: best-effort, degrades LOUDLY on failure (finding
    // 6) — a missing/invalid repo root or `gh` error is captured in
    // `stale_scan_error`, never silently collapsed into an empty vec.
    // `stale_scan_hard_error` distinguishes "the scan itself failed" (no
    // fresh authoritative hit set — must NOT reap) from
    // `stale_scan_warning` ("the scan ran, but some anchors couldn't be
    // verified" — the hit set IS still fresh/authoritative, reaping is safe).
    let mut stale_scan_hard_error: Option<String> = None;
    let mut stale_scan_warning: Option<String> = None;
    let stale_candidates =
        match crate::gh_ops::fetch_and_scan_stale_candidates(server, &repo, &repo_root, limit) {
            Ok((candidates, warnings)) => {
                if !warnings.is_empty() {
                    stale_scan_warning = Some(format!(
                        "{} anchor(s) could not be verified: {}",
                        warnings.len(),
                        warnings.join("; ")
                    ));
                }
                candidates
            }
            Err(e) => {
                stale_scan_hard_error = Some(e);
                Vec::new()
            }
        };
    let mut stale_refs = Vec::with_capacity(stale_candidates.len());
    for candidate in &stale_candidates {
        let issue_ref = format!("{repo}#{}", candidate.issue_number);
        stale_refs.push(issue_ref.clone());
        let row = crate::gh_ops::FreshnessRow {
            issue_ref: issue_ref.clone(),
            kind: crate::gh_ops::KIND_STALE_CANDIDATE.to_string(),
            verified_at_sha: String::new(),
            evidence_refs: candidate.evidence.clone(),
            checked_at: chrono::Utc::now().to_rfc3339(),
        };
        if let Err(e) =
            crate::gh_ops::save_freshness_row(server, crate::gh_ops::STALE_CANDIDATE_NS, &row)
        {
            save_errors.push(format!("{issue_ref}: {e}"));
        }
    }
    // Reap stale_candidate rows only when this scan actually produced a
    // fresh, authoritative hit set — a hard scan failure means `stale_refs`
    // is empty for the WRONG reason (I/O error, not "nothing stale found"),
    // and reaping on it would wrongly delete every real row.
    //
    // Round-3 finding 3: a reap DB error is no longer swallowed into 0 — it
    // surfaces in `reap_errors` and marks this kind unreaped-this-round via
    // the shared `record_reap_outcome` helper (`reap_incomplete_kinds` is
    // declared above, alongside the zombie arm, so all three arms share the
    // one accumulator).
    let stale_reaped = if stale_scan_hard_error.is_none() {
        record_reap_outcome(
            crate::gh_ops::KIND_STALE_CANDIDATE,
            || {
                crate::gh_ops::reap_stale_kind_rows(
                    server,
                    crate::gh_ops::STALE_CANDIDATE_NS,
                    crate::gh_ops::KIND_STALE_CANDIDATE,
                    &stale_refs,
                )
            },
            &mut reap_errors,
            &mut reap_incomplete_kinds,
        )
    } else {
        0
    };

    // Same-surface-churn arm (Scope item 2's third heuristic, codex review
    // finding 2) — best-effort, same loud-degrade posture as the stale arm.
    // Default policy: 30-day activity window, 3+ distinct touching PRs.
    //
    // Round-3 finding 6: `churn_threshold=0` used to mean "every inactive
    // issue with a non-empty file-surface flags as churn regardless of how
    // many PRs touched it" — `touching_pr_numbers.len() >= 0` is trivially
    // true even with ZERO touching PRs (`churn_threshold` is `u32`, so a
    // negative value is already rejected at param deserialization —only 0
    // is reachable here). Clamp to a minimum of 1 (the heuristic's whole
    // premise is "N *distinct touching* PRs" — 0 touching PRs is not
    // evidence of anything) and note the clamp in the response so a caller
    // who actually passed 0 sees why they got 1's behavior instead of a
    // silent surprise.
    let requested_churn_threshold = params.churn_threshold.unwrap_or(3);
    let churn_threshold_clamped = requested_churn_threshold < 1;
    let churn_threshold = requested_churn_threshold.max(1) as usize;
    let activity_since = params
        .churn_activity_since
        .clone()
        .unwrap_or_else(|| (chrono::Utc::now() - chrono::Duration::days(30)).to_rfc3339());
    let mut churn_scan_error: Option<String> = None;
    let churn_candidates = match crate::gh_ops::fetch_and_scan_same_surface_churn(
        server,
        &repo,
        limit,
        &activity_since,
        churn_threshold,
    ) {
        Ok(candidates) => candidates,
        Err(e) => {
            churn_scan_error = Some(e);
            Vec::new()
        }
    };
    let mut churn_refs = Vec::with_capacity(churn_candidates.len());
    for candidate in &churn_candidates {
        let issue_ref = format!("{repo}#{}", candidate.issue_number);
        churn_refs.push(issue_ref.clone());
        let row = crate::gh_ops::FreshnessRow {
            issue_ref: issue_ref.clone(),
            kind: crate::gh_ops::KIND_CHURN_CANDIDATE.to_string(),
            verified_at_sha: String::new(),
            evidence_refs: candidate
                .touching_pr_numbers
                .iter()
                .map(|n| format!("{repo}#{n}"))
                .collect(),
            checked_at: chrono::Utc::now().to_rfc3339(),
        };
        if let Err(e) =
            crate::gh_ops::save_freshness_row(server, crate::gh_ops::STALE_CANDIDATE_NS, &row)
        {
            save_errors.push(format!("{issue_ref}: {e}"));
        }
    }
    let churn_reaped = if churn_scan_error.is_none() {
        record_reap_outcome(
            crate::gh_ops::KIND_CHURN_CANDIDATE,
            || {
                crate::gh_ops::reap_stale_kind_rows(
                    server,
                    crate::gh_ops::STALE_CANDIDATE_NS,
                    crate::gh_ops::KIND_CHURN_CANDIDATE,
                    &churn_refs,
                )
            },
            &mut reap_errors,
            &mut reap_incomplete_kinds,
        )
    } else {
        0
    };

    serde_json::to_string(&json!({
        "tool": "tachi_gh_issue_freshness_scan",
        "repo": repo,
        "zombies": zombies
            .iter()
            .map(|h| json!({
                "issue_number": h.issue_number,
                "pr_number": h.pr_number,
                "pr_title": h.pr_title,
                "merge_commit_sha": h.merge_commit_sha,
            }))
            .collect::<Vec<_>>(),
        "stale_candidates": stale_candidates
            .iter()
            .map(|c| json!({
                "issue_number": c.issue_number,
                "reason": c.reason,
                "evidence": c.evidence,
            }))
            .collect::<Vec<_>>(),
        "churn_candidates": churn_candidates
            .iter()
            .map(|c| json!({
                "issue_number": c.issue_number,
                "touching_pr_count": c.touching_pr_count,
                "touching_pr_numbers": c.touching_pr_numbers,
            }))
            .collect::<Vec<_>>(),
        "zombie_count": zombies.len(),
        "stale_candidate_count": stale_candidates.len(),
        "churn_candidate_count": churn_candidates.len(),
        "rows_saved": zombies.len() + stale_candidates.len() + churn_candidates.len() - save_errors.len(),
        "rows_reaped": zombie_reaped + stale_reaped + churn_reaped,
        "save_errors": save_errors,
        "stale_scan_error": stale_scan_hard_error,
        "stale_scan_warning": stale_scan_warning,
        "churn_scan_error": churn_scan_error,
        // Round-3 finding 3: reap DB errors are surfaced here instead of
        // being swallowed into a misleadingly-clean `rows_reaped` count. A
        // non-empty `reap_errors`/`reap_incomplete_kinds` means at least one
        // kind's ghost rows (closed zombies, fixed stale/churn candidates)
        // were NOT cleared this round and may still be showing in the
        // briefing even though they no longer reproduce.
        "reap_errors": reap_errors,
        "reap_incomplete_kinds": reap_incomplete_kinds,
        "churn_threshold_requested": requested_churn_threshold,
        "churn_threshold_effective": churn_threshold,
        "churn_threshold_clamped": churn_threshold_clamped,
    }))
    .map_err(|e| format!("serialize: {e}"))
}

/// tachi_gh lifecycle actions — the only ones `format="markdown"` applies to
/// (`TachiGhParams::format` docs it as "response shape for lifecycle
/// actions"). GitHub primitive actions (issue_list, pr_read, …) return their
/// own JSON shape untouched, same as before this fix.
fn is_lifecycle_action(action: &str) -> bool {
    matches!(
        action,
        "link_pr" | "pr_status" | "pr_handoff" | "release_note" | "close_loop"
    )
}

/// True iff a safe_merge response envelope reports that it genuinely reclaimed
/// the local worktree (`reclamation.reclaimed == true`). Used to gate the
/// exec_envs lease flip (#894 S1) on an actual fs reclamation, not a skip.
fn safe_merge_reclaimed_worktree(envelope: &str) -> bool {
    serde_json::from_str::<Value>(envelope)
        .ok()
        .and_then(|v| {
            v.get("reclamation")
                .and_then(|r| r.get("reclaimed"))
                .and_then(Value::as_bool)
        })
        .unwrap_or(false)
}

fn required_repo(params: &TachiGhParams, action: &str) -> Result<String, String> {
    params
        .repo
        .as_deref()
        .map(str::trim)
        .filter(|repo| !repo.is_empty())
        .map(str::to_string)
        .ok_or_else(|| format!("{action} requires 'repo' in owner/repo format"))
}

fn resolve_tachi_gh_pr_target(
    params: &TachiGhParams,
    action: &str,
) -> Result<crate::task_lifecycle::GithubTarget, String> {
    if let (Some(repo), Some(number)) = (
        params
            .repo
            .as_deref()
            .map(str::trim)
            .filter(|repo| !repo.is_empty()),
        params.number,
    ) {
        return Ok(crate::task_lifecycle::GithubTarget {
            repo: repo.to_string(),
            number,
        });
    }
    if let Some(pr_ref) = params.pr_ref.as_deref() {
        if let Some(target) = crate::task_lifecycle::parse_pr_ref(pr_ref) {
            return Ok(target);
        }
    }
    Err(format!(
        "{action} requires either repo+number or pr_ref='owner/repo#123' / GitHub PR URL"
    ))
}

fn lifecycle_task_params(
    params: &TachiGhParams,
) -> Result<crate::tool_params::TachiTaskParams, String> {
    // Handlers ignore `action`; force a valid tachi_task primary so GH lifecycle
    // action strings (link_pr/pr_status/…) do not fail TachiTaskAction decode
    // after #757 removed those variants from tachi_task.
    //
    // INVARIANT: lifecycle handlers must not read params.action; if one starts
    // to, this bridge must be replaced. Every callee downstream of this
    // function — `task_lifecycle::handle_task_link_pr`, `resolve_task_pr_target`
    // (pr_status), `handle_task_pr_handoff`, `handle_task_release_note` — must
    // keep ignoring `params.action` on the `TachiTaskParams` it receives here,
    // because that field is always overwritten to the literal `"status"` a few
    // lines below regardless of which of the four tachi_gh lifecycle actions
    // actually triggered this call. If one of those handlers starts branching
    // on `params.action`, it will silently misroute (every lifecycle action
    // behaves as `status`) instead of erroring.
    // `lifecycle_task_params_bridge_routes_all_four_gh_lifecycle_actions` in
    // `mod tests` below only proves the conversion itself is lossless for the
    // fields the handlers *do* key off (pr_ref/flow_id/repo/number) — it does
    // not, and cannot, catch a handler that starts reading `action`. Follow-up:
    // #757 tracks replacing this bridge with a dedicated lifecycle params type
    // instead of reusing `TachiTaskParams`.
    let mut value = serde_json::to_value(params)
        .map_err(|err| format!("serialize tachi_gh lifecycle params: {err}"))?;
    if let Some(obj) = value.as_object_mut() {
        obj.insert(
            "action".to_string(),
            serde_json::Value::String("status".to_string()),
        );
    }
    serde_json::from_value(value)
        .map_err(|err| format!("convert tachi_gh lifecycle params to tachi_task params: {err}"))
}

fn normalize_gh_response(action: &str, raw: &str, format: Option<&str>) -> Result<String, String> {
    let Ok(mut value) = serde_json::from_str::<Value>(raw) else {
        return Ok(raw.to_string());
    };
    if let Some(obj) = value.as_object_mut() {
        obj.entry("status".to_string())
            .or_insert_with(|| Value::String("completed".to_string()));
        obj.entry("action".to_string())
            .or_insert_with(|| Value::String(action.to_string()));
    }
    if is_lifecycle_action(action)
        && format
            .map(str::trim)
            .is_some_and(|format| format.eq_ignore_ascii_case("markdown"))
    {
        return Ok(render_lifecycle_markdown(action, &value));
    }
    serde_json::to_string(&value).map_err(|err| format!("serialize normalized gh response: {err}"))
}

/// Render a tachi_gh lifecycle response (`link_pr`/`pr_status`/`pr_handoff`/
/// `release_note`/`close_loop`) as markdown. `TachiGhParams::format` has advertised
/// `format="markdown"` for lifecycle actions since the field was added, but
/// this path was never wired up — every response was JSON regardless of
/// `format`. Field list matches what each lifecycle handler actually emits
/// (see `task_lifecycle::issue_flow`/`release_ux::release_note`).
fn render_lifecycle_markdown(action: &str, value: &Value) -> String {
    let mut lines = vec![
        format!("## Tachi GH {action}"),
        format!("action: `{action}`"),
    ];
    for field in [
        "flow_id",
        "issue_ref",
        "pr_ref",
        "branch",
        "pr_title",
        "safe_to_open",
        "verification_overall",
        "release_note_path",
        "pr_handoff_path",
        "run_dir",
    ] {
        let Some(field_value) = value.get(field) else {
            continue;
        };
        match field_value {
            Value::String(text) if !text.is_empty() => lines.push(format!("{field}: `{text}`")),
            Value::Bool(flag) => lines.push(format!("{field}: `{flag}`")),
            _ => {}
        }
    }
    if let Some(blockers) = value.get("blocked_reasons").and_then(Value::as_array) {
        let blockers = blockers
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>()
            .join(", ");
        if !blockers.is_empty() {
            lines.push(format!("blocked_reasons: {blockers}"));
        }
    }
    for body_field in ["pr_body", "release_note"] {
        if let Some(body) = value.get(body_field).and_then(Value::as_str) {
            if !body.is_empty() {
                lines.push(String::new());
                lines.push(body.to_string());
            }
        }
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(value: Value) -> TachiGhParams {
        serde_json::from_value(value).expect("params")
    }

    /// PR #1004 round-4 codex review — the merge-blocker: prove the
    /// zombie-kind reap error path actually marks `KIND_ZOMBIE` incomplete,
    /// with a plain injected `Err` (no DB needed — `record_reap_outcome`
    /// takes the reap call as a closure precisely so this doesn't need one).
    /// Before this fix, only the stale_candidate/churn_candidate arms pushed
    /// their kind into `reap_incomplete_kinds` on error; the zombie arm's
    /// `Err` branch recorded `reap_errors` but never `reap_incomplete_kinds`,
    /// so a caller checking only the latter (the documented signal for
    /// "which kind's ghost rows may still be showing") would see a clean
    /// empty list even though the zombie reap had actually failed.
    #[test]
    fn record_reap_outcome_marks_kind_incomplete_on_error() {
        let mut reap_errors = Vec::new();
        let mut reap_incomplete_kinds = Vec::new();

        let n = record_reap_outcome(
            crate::gh_ops::KIND_ZOMBIE,
            || Err::<usize, String>("simulated DB failure".to_string()),
            &mut reap_errors,
            &mut reap_incomplete_kinds,
        );

        assert_eq!(n, 0, "a failed reap must report 0 reaped, not swallow it");
        assert_eq!(
            reap_errors,
            vec!["zombie: simulated DB failure".to_string()],
            "the error must be recorded verbatim, kind-prefixed"
        );
        assert_eq!(
            reap_incomplete_kinds,
            vec![crate::gh_ops::KIND_ZOMBIE],
            "KIND_ZOMBIE must land in reap_incomplete_kinds on a reap error — \
             this is the exact bug: the zombie arm's Err branch used to skip \
             this push while stale_candidate/churn_candidate did it correctly"
        );
    }

    /// Companion happy-path guard: a successful reap must NOT mark the kind
    /// incomplete and must NOT record an error — otherwise a trivial "always
    /// mark incomplete" fix would also make this test pass, which would be
    /// dishonest in the other direction (permanently flagging every scan as
    /// incomplete even when nothing failed).
    #[test]
    fn record_reap_outcome_leaves_kind_untouched_on_success() {
        let mut reap_errors = Vec::new();
        let mut reap_incomplete_kinds = Vec::new();

        let n = record_reap_outcome(
            crate::gh_ops::KIND_ZOMBIE,
            || Ok(3usize),
            &mut reap_errors,
            &mut reap_incomplete_kinds,
        );

        assert_eq!(n, 3);
        assert!(reap_errors.is_empty());
        assert!(reap_incomplete_kinds.is_empty());
    }

    /// All three kinds must be treated identically by the same helper — a
    /// regression that special-cased one kind again (the original bug) would
    /// show up here as an incomplete `reap_incomplete_kinds` list.
    #[test]
    fn record_reap_outcome_marks_every_kind_on_error_not_just_some() {
        let mut reap_errors = Vec::new();
        let mut reap_incomplete_kinds = Vec::new();

        for kind in [
            crate::gh_ops::KIND_ZOMBIE,
            crate::gh_ops::KIND_STALE_CANDIDATE,
            crate::gh_ops::KIND_CHURN_CANDIDATE,
        ] {
            record_reap_outcome(
                kind,
                || Err::<usize, String>("boom".to_string()),
                &mut reap_errors,
                &mut reap_incomplete_kinds,
            );
        }

        assert_eq!(
            reap_incomplete_kinds,
            vec![
                crate::gh_ops::KIND_ZOMBIE,
                crate::gh_ops::KIND_STALE_CANDIDATE,
                crate::gh_ops::KIND_CHURN_CANDIDATE,
            ],
            "every kind that hits a reap error must be marked incomplete, zombie included"
        );
        assert_eq!(reap_errors.len(), 3);
    }

    #[test]
    fn pr_target_accepts_pr_ref_without_repo_or_number() {
        let params = params(json!({
            "action": "safe_merge",
            "pr_ref": "owner/repo#42"
        }));
        let target = resolve_tachi_gh_pr_target(&params, "safe_merge").expect("target");
        assert_eq!(target.repo, "owner/repo");
        assert_eq!(target.number, 42);
    }

    #[test]
    fn pr_target_keeps_repo_number_alias() {
        let params = params(json!({
            "action": "safe_merge",
            "repo": "owner/repo",
            "number": 42
        }));
        let target = resolve_tachi_gh_pr_target(&params, "safe_merge").expect("target");
        assert_eq!(target.repo, "owner/repo");
        assert_eq!(target.number, 42);
    }

    /// Guard for the `lifecycle_task_params` bridge (see the invariant comment
    /// at its definition): drives all four tachi_gh lifecycle actions
    /// (link_pr/pr_status/pr_handoff/release_note) THROUGH the bridge — not
    /// through the handlers — and asserts each converts without error, with
    /// `action` forced to `status` (the bridge's whole reason to exist), and
    /// with the fields the handlers actually key off (pr_ref/flow_id/
    /// doc_paths/spec_paths/notes) preserved untouched per action. A
    /// per-action-distinct pr_ref/flow_id pair means a field mix-up between
    /// actions fails loudly instead of silently misrouting. doc_paths/
    /// spec_paths/notes cover the terra-review finding that
    /// `TachiGhParams` dropped these fields silently (release_note reads
    /// doc_paths/spec_paths, pr_handoff reads notes as a title override).
    #[test]
    fn lifecycle_task_params_bridge_routes_all_four_gh_lifecycle_actions() {
        for action in ["link_pr", "pr_status", "pr_handoff", "release_note"] {
            let pr_ref = format!("owner/repo#{}", action.len());
            let flow_id = format!("flow-{action}");
            let doc_path = format!("docs/{action}.md");
            let spec_path = format!("specs/{action}.md");
            let notes = format!("notes-{action}");
            let gh_params = params(json!({
                "action": action,
                "pr_ref": pr_ref,
                "flow_id": flow_id,
                "doc_paths": [doc_path],
                "spec_paths": [spec_path],
                "notes": notes,
            }));

            let task_params = lifecycle_task_params(&gh_params)
                .unwrap_or_else(|err| panic!("{action}: bridge conversion failed: {err}"));

            assert_eq!(
                task_params.action,
                crate::tool_params::TachiTaskAction::Status,
                "{action}: bridge must force action=status regardless of the source tachi_gh action"
            );
            assert_eq!(
                task_params.pr_ref.as_deref(),
                Some(pr_ref.as_str()),
                "{action}: pr_ref must round-trip through the bridge unchanged"
            );
            assert_eq!(
                task_params.flow_id.as_deref(),
                Some(flow_id.as_str()),
                "{action}: flow_id must round-trip through the bridge unchanged"
            );
            assert_eq!(
                task_params.doc_paths,
                vec![doc_path],
                "{action}: doc_paths must round-trip through the bridge unchanged"
            );
            assert_eq!(
                task_params.spec_paths,
                vec![spec_path],
                "{action}: spec_paths must round-trip through the bridge unchanged"
            );
            assert_eq!(
                task_params.notes.as_deref(),
                Some(notes.as_str()),
                "{action}: notes must round-trip through the bridge unchanged"
            );

            // resolve_task_pr_target is action-agnostic (reads repo/number/pr_ref
            // only) — confirm routing to the right PR target still works through
            // the bridge for the actions that rely on it (link_pr, pr_status).
            let target = crate::task_lifecycle::resolve_task_pr_target(&task_params)
                .unwrap_or_else(|err| panic!("{action}: resolve_task_pr_target failed: {err}"));
            assert_eq!(target.repo, "owner/repo");
            assert_eq!(target.number, action.len() as u64);
        }
    }

    /// `TachiGhParams::format` advertises "json (default) or markdown" for
    /// lifecycle actions (gh.rs doc comment). Guard that `normalize_gh_response`
    /// actually produces markdown for a lifecycle action instead of silently
    /// ignoring `format` and always returning JSON (the terra-review finding).
    #[test]
    fn normalize_gh_response_honors_markdown_format_for_lifecycle_actions() {
        let raw = json!({
            "ok": true,
            "action": "pr_handoff",
            "flow_id": "flow-1",
            "pr_title": "Fix the thing",
            "safe_to_open": true,
            "pr_body": "## Summary\nDetails here.",
        })
        .to_string();

        let markdown = normalize_gh_response("pr_handoff", &raw, Some("markdown"))
            .expect("markdown normalize");
        assert!(
            markdown.contains("pr_title: `Fix the thing`"),
            "expected rendered field in markdown output, got: {markdown}"
        );
        assert!(
            markdown.contains("## Summary\nDetails here."),
            "expected pr_body content inlined in markdown output, got: {markdown}"
        );
        assert!(
            serde_json::from_str::<Value>(&markdown).is_err(),
            "markdown output should not itself be a JSON document, got: {markdown}"
        );

        let json_default =
            normalize_gh_response("pr_handoff", &raw, None).expect("default normalize");
        assert!(
            serde_json::from_str::<Value>(&json_default).is_ok(),
            "default (no format) response must stay JSON, got: {json_default}"
        );

        // GitHub primitive actions are unaffected by format=markdown — only the
        // four lifecycle actions render markdown (gh.rs: "response shape for
        // lifecycle actions").
        let primitive_raw = json!({ "ok": true, "number": 42 }).to_string();
        let primitive_with_markdown_format =
            normalize_gh_response("pr_read", &primitive_raw, Some("markdown"))
                .expect("primitive normalize");
        assert!(
            serde_json::from_str::<Value>(&primitive_with_markdown_format).is_ok(),
            "non-lifecycle action must stay JSON even when format=markdown, got: {primitive_with_markdown_format}"
        );
    }
}
