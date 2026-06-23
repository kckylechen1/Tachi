use super::*;

pub(crate) fn handle_route_simulation(
    server: &MemoryServer,
    limit: usize,
    focus_task: Option<&str>,
    risk_override: Option<&str>,
    file_paths: &[String],
) -> Result<String, String> {
    let rows = load_live_eval_rows(server, limit.max(1))?;
    let performance_matrix = aggregate_performance_matrix(&rows);
    let focus = focus_task
        .filter(|task| !task.trim().is_empty())
        .map(|task| classify_dispatch_risk(task, risk_override, file_paths));

    let summaries = ["current", "cost_sensitive", "quality_first"]
        .iter()
        .map(|policy| simulate_route_policy(policy, &performance_matrix, focus.as_ref()))
        .collect::<Vec<_>>();

    serde_json::to_string(&json!({
        "action": "route_simulate",
        "read_only": true,
        "source": "live_memory_eval",
        "row_count": rows.len(),
        "matrix_rows": performance_matrix.len(),
        "limit": limit.max(1),
        "focus": focus.as_ref().map(|risk| json!({
            "task_type": risk.task_type,
            "risk": risk.risk,
            "risk_reasons": risk.reasons,
            "required_profiles": risk.required_profiles,
            "blocked_profiles": risk.blocked_profiles,
        })),
        "policies": summaries,
        "caveats": route_simulation_caveats(&rows, &performance_matrix),
    }))
    .map_err(|e| format!("serialize route simulation: {e}"))
}

pub(crate) fn handle_route_policy_proposals(
    server: &MemoryServer,
    limit: usize,
    status_filter: Option<&str>,
) -> Result<String, String> {
    let rows = load_live_eval_rows(server, limit.max(1))?;
    let performance_matrix = aggregate_performance_matrix(&rows);
    let current = simulate_route_policy("current", &performance_matrix, None);
    let variants = ["cost_sensitive", "quality_first"]
        .iter()
        .map(|policy| simulate_route_policy(policy, &performance_matrix, None))
        .collect::<Vec<_>>();
    let mut proposals = build_route_policy_proposals(&current, &variants, rows.len(), limit.max(1));
    proposals.extend(build_loadout_evolution_proposals(
        server,
        &performance_matrix,
        limit.max(1),
    )?);

    server.with_global_store(|store| {
        for proposal in proposals {
            let id = proposal["proposal_id"]
                .as_str()
                .ok_or_else(|| "route policy proposal missing id".to_string())?
                .to_string();
            let mut next = proposal;
            if let Some((existing, _version)) = store
                .get_state_kv(DISPATCH_POLICY_PROPOSAL_NS, &id)
                .map_err(|e| format!("load route policy proposal: {e}"))?
            {
                if let Ok(existing_json) = serde_json::from_str::<Value>(&existing) {
                    let existing_status = existing_json
                        .get("status")
                        .and_then(|value| value.as_str())
                        .unwrap_or("pending");
                    if existing_status != "pending" {
                        next["status"] = json!(existing_status);
                    }
                    if let Some(review) = existing_json.get("review") {
                        next["review"] = review.clone();
                    }
                    if let Some(applied_at) = existing_json.get("applied_at") {
                        next["applied_at"] = applied_at.clone();
                    }
                }
            }
            let raw = serde_json::to_string(&next)
                .map_err(|e| format!("serialize route policy proposal: {e}"))?;
            store
                .set_state(DISPATCH_POLICY_PROPOSAL_NS, &id, &raw)
                .map_err(|e| format!("persist route policy proposal: {e}"))?;
        }
        Ok(())
    })?;

    let desired = status_filter
        .map(str::trim)
        .filter(|value| !value.is_empty() && *value != "all")
        .map(|value| value.to_ascii_lowercase());
    let mut records = server.with_global_store_read(|store| {
        store
            .list_state(DISPATCH_POLICY_PROPOSAL_NS)
            .map_err(|e| format!("list route policy proposals: {e}"))
    })?;
    records.truncate(limit.max(1).min(100));
    let mut out = Vec::new();
    for row in records {
        let mut value: Value = serde_json::from_str(&row.value_json)
            .unwrap_or_else(|_| json!({ "proposal_id": row.key, "raw": row.value_json }));
        value["state_version"] = json!(row.version);
        value["updated_at"] = json!(row.updated_at);
        let status = value
            .get("status")
            .and_then(|status| status.as_str())
            .unwrap_or("pending");
        if desired.as_deref().is_some_and(|wanted| wanted != status) {
            continue;
        }
        out.push(value);
    }

    serde_json::to_string(&json!({
        "action": "proposals",
        "kind": "dispatch_policy",
        "proposal_kinds": ["route_policy", "loadout_evolution"],
        "read_only": false,
        "requires_human_approval": true,
        "generated_from": {
            "source": "live_memory_eval",
            "row_count": rows.len(),
            "matrix_rows": performance_matrix.len(),
            "limit": limit.max(1),
        },
        "count": out.len(),
        "proposals": out,
        "next_actions": [
            "tachi_task(action='review_proposal', proposal_id=..., review_status='approved')",
            "tachi_task(action='apply_proposals', proposal_id=..., confirm=true) for approved route_policy rules",
            "tachi_task(action='apply_proposals', proposal_id=..., confirm=true) for approved loadout_evolution proposals to project reviewed profile/card loadout overlays"
        ],
    }))
    .map_err(|e| format!("serialize route policy proposals: {e}"))
}

pub(crate) fn handle_route_policy_review(
    server: &MemoryServer,
    proposal_id: &str,
    review_status: &str,
    note: Option<&str>,
) -> Result<String, String> {
    let proposal_id = proposal_id.trim();
    if proposal_id.is_empty() {
        return Err("proposal_id is required when action='review_proposal'".to_string());
    }
    let status = match review_status.trim().to_ascii_lowercase().as_str() {
        "approved" | "approve" => "approved",
        "rejected" | "reject" => "rejected",
        other => {
            return Err(format!(
                "Invalid review_status '{}'. Expected approved|rejected",
                other
            ))
        }
    };
    let reviewed_at = Utc::now().to_rfc3339();
    let updated = server.with_global_store(|store| {
        let (raw, _version) = store
            .get_state_kv(DISPATCH_POLICY_PROPOSAL_NS, proposal_id)
            .map_err(|e| format!("load route policy proposal: {e}"))?
            .ok_or_else(|| format!("route policy proposal not found: {proposal_id}"))?;
        let mut value: Value =
            serde_json::from_str(&raw).map_err(|e| format!("parse route policy proposal: {e}"))?;
        value["status"] = json!(status);
        value["review"] = json!({
            "status": status,
            "note": note,
            "reviewed_at": reviewed_at,
        });
        let next = serde_json::to_string(&value)
            .map_err(|e| format!("serialize route policy review: {e}"))?;
        store
            .set_state(DISPATCH_POLICY_PROPOSAL_NS, proposal_id, &next)
            .map_err(|e| format!("persist route policy review: {e}"))?;
        Ok(value)
    })?;

    serde_json::to_string(&json!({
        "action": "review_proposal",
        "proposal_id": proposal_id,
        "proposal": updated,
    }))
    .map_err(|e| format!("serialize route policy review response: {e}"))
}

pub(crate) fn handle_route_policy_apply(
    server: &MemoryServer,
    proposal_id: &str,
    confirm: bool,
) -> Result<String, String> {
    let proposal_id = proposal_id.trim();
    if proposal_id.is_empty() {
        return Err("proposal_id is required when action='apply_proposals'".to_string());
    }
    if !confirm {
        return Err(
            "apply_proposals requires confirm=true after human approval; no routing changes applied"
                .to_string(),
        );
    }
    let applied_at = Utc::now().to_rfc3339();
    let updated = server.with_global_store(|store| {
        let (raw, _version) = store
            .get_state_kv(DISPATCH_POLICY_PROPOSAL_NS, proposal_id)
            .map_err(|e| format!("load route policy proposal: {e}"))?
            .ok_or_else(|| format!("route policy proposal not found: {proposal_id}"))?;
        let mut value: Value =
            serde_json::from_str(&raw).map_err(|e| format!("parse route policy proposal: {e}"))?;
        let status = value
            .get("status")
            .and_then(|status| status.as_str())
            .unwrap_or("pending");
        if status != "approved" {
            return Err(format!(
                "route policy proposal {proposal_id} must be approved before apply; current status={status}"
            ));
        }
        let kind = value
            .get("kind")
            .and_then(Value::as_str)
            .unwrap_or("route_policy");
        match kind {
            "route_policy" => {
                value["status"] = json!("applied");
                value["applied_at"] = json!(applied_at);
                let next = serde_json::to_string(&value)
                    .map_err(|e| format!("serialize applied route policy proposal: {e}"))?;
                store
                    .set_state(DISPATCH_POLICY_PROPOSAL_NS, proposal_id, &next)
                    .map_err(|e| format!("persist applied route policy proposal: {e}"))?;
                store
                    .set_state(ROUTE_POLICY_RULE_NS, proposal_id, &next)
                    .map_err(|e| format!("persist route policy rule: {e}"))?;
            }
            "loadout_evolution" => {
                let profile_name = value
                    .get("profile")
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        format!("loadout_evolution proposal {proposal_id} missing profile")
                    })?;
                let profile = resolve_dispatch_profile(profile_name).ok_or_else(|| {
                    format!(
                        "loadout_evolution proposal {proposal_id} references unknown profile {profile_name}"
                    )
                })?;
                let operation = value
                    .get("operation")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if !matches!(
                    operation,
                    "promote_observed_skill_to_signature"
                        | "add_evidence_backed_passive_trait"
                        | "add_evidence_contract_required"
                        | "add_card_weakness"
                        | "mark_skill_demotion_target"
                ) {
                    return Err(format!(
                        "unsupported loadout_evolution operation for {proposal_id}: {operation}"
                    ));
                }

                let mut overlay = if let Some((raw, _version)) = store
                    .get_state_kv(PROFILE_CARD_OVERLAY_NS, profile.name)
                    .map_err(|e| format!("load profile/card overlay: {e}"))?
                {
                    serde_json::from_str::<Value>(&raw)
                        .map_err(|e| format!("parse profile/card overlay: {e}"))?
                } else {
                    json!({
                        "kind": "profile_card_loadout_overlay",
                        "profile": profile.name,
                        "add_signature_skills": [],
                        "source_proposal_ids": [],
                        "created_at": applied_at,
                    })
                };

                let mut overlay_skills =
                    profile_projected_signature_skills_from_overlay(profile, Some(&overlay));
                let mut overlay_traits =
                    profile_projected_passive_traits_from_overlay(profile, Some(&overlay));
                let mut overlay_evidence_required =
                    profile_projected_evidence_required_from_overlay(profile, Some(&overlay));
                let mut overlay_weak_against =
                    profile_projected_weak_against_from_overlay(profile, Some(&overlay));
                let mut overlay_demotion_targets =
                    profile_demotion_targets_from_overlay(profile, Some(&overlay));
                let mut added_signature_skills = Vec::new();
                let mut added_passive_traits = Vec::new();
                let mut added_evidence_required = Vec::new();
                let mut added_weak_against = Vec::new();
                let mut added_demotion_targets = Vec::new();
                let already_projected = match operation {
                    "promote_observed_skill_to_signature" => {
                        let skill_id = value
                            .get("skill_id")
                            .and_then(Value::as_str)
                            .map(str::trim)
                            .filter(|skill| !skill.is_empty())
                            .map(str::to_string)
                            .ok_or_else(|| {
                                format!("loadout_evolution proposal {proposal_id} missing skill_id")
                            })?;
                        if profile
                            .forbidden_skills
                            .iter()
                            .any(|skill| *skill == skill_id)
                        {
                            return Err(format!(
                                "loadout_evolution proposal {proposal_id} targets forbidden skill {skill_id}"
                            ));
                        }
                        let baseline_skills = profile_required_skill_ids(profile);
                        if baseline_skills.iter().any(|skill| skill == &skill_id) {
                            return Err(format!(
                                "loadout_evolution proposal {proposal_id} targets existing baseline skill {skill_id}"
                            ));
                        }
                        let already_projected =
                            overlay_skills.iter().any(|skill| skill == &skill_id);

                        if !already_projected {
                            overlay_skills.push(skill_id.clone());
                        }
                        added_signature_skills.push(skill_id);
                        already_projected
                    }
                    "add_evidence_backed_passive_trait" => {
                        let trait_id = value
                            .get("trait_id")
                            .and_then(Value::as_str)
                            .map(str::trim)
                            .filter(|trait_id| !trait_id.is_empty())
                            .map(str::to_string)
                            .or_else(|| {
                                value
                                    .get("proposed_patch")
                                    .and_then(|patch| patch.get("add_passive_traits"))
                                    .and_then(Value::as_array)
                                    .into_iter()
                                    .flatten()
                                    .filter_map(Value::as_str)
                                    .map(str::trim)
                                    .find(|trait_id| !trait_id.is_empty())
                                    .map(str::to_string)
                            })
                            .ok_or_else(|| {
                                format!(
                                    "loadout_evolution proposal {proposal_id} missing trait_id"
                                )
                            })?;
                        if profile.passive_traits.iter().any(|item| *item == trait_id) {
                            return Err(format!(
                                "loadout_evolution proposal {proposal_id} targets existing baseline passive trait {trait_id}"
                            ));
                        }
                        let already_projected =
                            overlay_traits.iter().any(|item| item == &trait_id);
                        if !already_projected {
                            overlay_traits.push(trait_id.clone());
                        }
                        added_passive_traits.push(trait_id);
                        already_projected
                    }
                    "add_evidence_contract_required" => {
                        let evidence_id = value
                            .get("evidence_id")
                            .and_then(Value::as_str)
                            .map(str::trim)
                            .filter(|evidence_id| !evidence_id.is_empty())
                            .map(str::to_string)
                            .or_else(|| {
                                value
                                    .get("proposed_patch")
                                    .and_then(|patch| patch.get("add_evidence_required"))
                                    .and_then(Value::as_array)
                                    .into_iter()
                                    .flatten()
                                    .filter_map(Value::as_str)
                                    .map(str::trim)
                                    .find(|evidence_id| !evidence_id.is_empty())
                                    .map(str::to_string)
                            })
                            .ok_or_else(|| {
                                format!(
                                    "loadout_evolution proposal {proposal_id} missing evidence_id"
                                )
                            })?;
                        if profile.evidence_required.iter().any(|item| *item == evidence_id) {
                            return Err(format!(
                                "loadout_evolution proposal {proposal_id} targets existing baseline evidence requirement {evidence_id}"
                            ));
                        }
                        let already_projected = overlay_evidence_required
                            .iter()
                            .any(|item| item == &evidence_id);
                        if !already_projected {
                            overlay_evidence_required.push(evidence_id.clone());
                        }
                        added_evidence_required.push(evidence_id);
                        already_projected
                    }
                    "add_card_weakness" => {
                        let weakness_id = value
                            .get("weakness_id")
                            .and_then(Value::as_str)
                            .map(str::trim)
                            .filter(|weakness_id| !weakness_id.is_empty())
                            .map(str::to_string)
                            .or_else(|| {
                                value
                                    .get("proposed_patch")
                                    .and_then(|patch| patch.get("add_weak_against"))
                                    .and_then(Value::as_array)
                                    .into_iter()
                                    .flatten()
                                    .filter_map(Value::as_str)
                                    .map(str::trim)
                                    .find(|weakness_id| !weakness_id.is_empty())
                                    .map(str::to_string)
                            })
                            .ok_or_else(|| {
                                format!(
                                    "loadout_evolution proposal {proposal_id} missing weakness_id"
                                )
                            })?;
                        if profile.weak_against.iter().any(|item| *item == weakness_id) {
                            return Err(format!(
                                "loadout_evolution proposal {proposal_id} targets existing baseline weakness {weakness_id}"
                            ));
                        }
                        let already_projected =
                            overlay_weak_against.iter().any(|item| item == &weakness_id);
                        if !already_projected {
                            overlay_weak_against.push(weakness_id.clone());
                        }
                        added_weak_against.push(weakness_id);
                        already_projected
                    }
                    "mark_skill_demotion_target" => {
                        let skill_id = value
                            .get("skill_id")
                            .and_then(Value::as_str)
                            .map(str::trim)
                            .filter(|skill_id| !skill_id.is_empty())
                            .map(str::to_string)
                            .or_else(|| {
                                value
                                    .get("proposed_patch")
                                    .and_then(|patch| patch.get("demotion_targets"))
                                    .and_then(Value::as_array)
                                    .into_iter()
                                    .flatten()
                                    .filter_map(Value::as_str)
                                    .map(str::trim)
                                    .find(|skill_id| !skill_id.is_empty())
                                    .map(str::to_string)
                            })
                            .ok_or_else(|| {
                                format!(
                                    "loadout_evolution proposal {proposal_id} missing skill_id"
                                )
                            })?;
                        let known_skill = profile_required_skill_ids(profile)
                            .into_iter()
                            .any(|skill| skill == skill_id)
                            || overlay_skills.iter().any(|skill| skill == &skill_id);
                        if !known_skill {
                            return Err(format!(
                                "loadout_evolution proposal {proposal_id} targets unknown loadout skill {skill_id}"
                            ));
                        }
                        let already_projected = overlay_demotion_targets
                            .iter()
                            .any(|item| item == &skill_id);
                        if !already_projected {
                            overlay_demotion_targets.push(skill_id.clone());
                        }
                        added_demotion_targets.push(skill_id);
                        already_projected
                    }
                    _ => unreachable!("unsupported operation checked above"),
                };
                crate::skill_policy::dedupe_preserve_order(&mut overlay_skills);
                crate::skill_policy::dedupe_preserve_order(&mut overlay_traits);
                crate::skill_policy::dedupe_preserve_order(&mut overlay_evidence_required);
                crate::skill_policy::dedupe_preserve_order(&mut overlay_weak_against);
                crate::skill_policy::dedupe_preserve_order(&mut overlay_demotion_targets);

                let mut source_proposals = overlay
                    .get("source_proposal_ids")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect::<Vec<_>>();
                source_proposals.push(proposal_id.to_string());
                crate::skill_policy::dedupe_preserve_order(&mut source_proposals);

                overlay["profile"] = json!(profile.name);
                overlay["kind"] = json!("profile_card_loadout_overlay");
                overlay["add_signature_skills"] = json!(overlay_skills);
                overlay["add_passive_traits"] = json!(overlay_traits);
                overlay["add_evidence_required"] = json!(overlay_evidence_required);
                overlay["add_weak_against"] = json!(overlay_weak_against);
                overlay["demotion_targets"] = json!(overlay_demotion_targets);
                overlay["source_proposal_ids"] = json!(source_proposals);
                overlay["updated_at"] = json!(applied_at);
                overlay["last_applied_proposal_id"] = json!(proposal_id);
                let overlay_raw = serde_json::to_string(&overlay)
                    .map_err(|e| format!("serialize profile/card overlay: {e}"))?;
                store
                    .set_state(PROFILE_CARD_OVERLAY_NS, profile.name, &overlay_raw)
                    .map_err(|e| format!("persist profile/card overlay: {e}"))?;

                value["status"] = json!("applied");
                value["applied_at"] = json!(applied_at);
                value["projection"] = json!({
                    "status": "applied_profile_card_overlay",
                    "namespace": PROFILE_CARD_OVERLAY_NS,
                    "key": profile.name,
                    "already_projected": already_projected,
                    "added_signature_skills": added_signature_skills,
                    "added_passive_traits": added_passive_traits,
                    "added_evidence_required": added_evidence_required,
                    "added_weak_against": added_weak_against,
                    "added_demotion_targets": added_demotion_targets,
                    "note": "Reviewed loadout evolution is projected as a durable profile/card overlay; built-in static definitions remain the baseline."
                });
                let next = serde_json::to_string(&value)
                    .map_err(|e| format!("serialize applied loadout proposal: {e}"))?;
                store
                    .set_state(DISPATCH_POLICY_PROPOSAL_NS, proposal_id, &next)
                    .map_err(|e| format!("persist applied loadout proposal: {e}"))?;
            }
            other => {
                return Err(format!(
                    "apply_proposals does not support proposal kind {other} for {proposal_id}"
                ));
            }
        }
        Ok(value)
    })?;
    let applied_kind = updated
        .get("kind")
        .and_then(Value::as_str)
        .unwrap_or("route_policy");

    serde_json::to_string(&json!({
        "action": "apply_proposals",
        "proposal_id": proposal_id,
        "applied": true,
        "routing_mutated": applied_kind == "route_policy",
        "profile_card_mutated": applied_kind == "loadout_evolution",
        "rule_namespace": if applied_kind == "route_policy" { Value::String(ROUTE_POLICY_RULE_NS.to_string()) } else { Value::Null },
        "projection_namespace": if applied_kind == "loadout_evolution" { Value::String(PROFILE_CARD_OVERLAY_NS.to_string()) } else { Value::Null },
        "proposal": updated,
        "note": if applied_kind == "route_policy" {
            "Approved route-policy rule was persisted and will be consumed by recommend() when task type, risk gates, and sample thresholds match."
        } else {
            "Approved loadout-evolution proposal was projected into the profile/card overlay and will be visible in profile, loadout, recommend, and dispatch prompt surfaces."
        },
    }))
    .map_err(|e| format!("serialize route policy apply response: {e}"))
}

pub(super) fn load_route_policy_rule_loadout(
    server: &MemoryServer,
    risk: &DispatchRisk,
) -> Result<RoutePolicyRuleLoadout, String> {
    let records = server.with_global_store_read(|store| {
        store
            .list_state(ROUTE_POLICY_RULE_NS)
            .map_err(|e| format!("list route policy rules: {e}"))
    })?;
    let mut applied = Vec::new();
    let mut skipped = Vec::new();

    for row in records {
        let proposal_id = row.key.clone();
        let value: Value = match serde_json::from_str(&row.value_json) {
            Ok(value) => value,
            Err(err) => {
                skipped.push(SkippedRoutePolicyRule {
                    proposal_id,
                    reason: format!("invalid_json:{err}"),
                    policy: None,
                    task_type: None,
                    prefer_profile: None,
                    sample_count: None,
                });
                continue;
            }
        };
        let status = value
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        let review_status = value
            .get("review")
            .and_then(|review| review.get("status"))
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        let policy_rule = value.get("policy_rule").unwrap_or(&Value::Null);
        let policy = policy_rule
            .get("policy")
            .and_then(Value::as_str)
            .or_else(|| value.get("policy").and_then(Value::as_str))
            .map(str::to_string);
        let task_type = policy_rule
            .get("when_task_type")
            .and_then(Value::as_str)
            .map(str::to_string);
        let prefer_profile = policy_rule
            .get("prefer_profile")
            .and_then(Value::as_str)
            .map(str::to_string);
        let sample_count = route_policy_rule_sample_count(&value);
        let score_delta = value.get("score_delta").and_then(Value::as_f64);

        let skip_reason = if status != "applied" {
            Some(format!("status_not_applied:{status}"))
        } else if review_status != "approved" {
            Some(format!("review_not_approved:{review_status}"))
        } else if task_type.as_deref() != Some(risk.task_type.as_str()) {
            Some(format!(
                "task_type_mismatch:{}",
                task_type.as_deref().unwrap_or("missing")
            ))
        } else if sample_count < MIN_ROUTE_POLICY_RULE_SAMPLES {
            Some(format!(
                "insufficient_samples:{sample_count}<{}",
                MIN_ROUTE_POLICY_RULE_SAMPLES
            ))
        } else if prefer_profile
            .as_deref()
            .is_none_or(|profile| resolve_dispatch_profile(profile).is_none())
        {
            Some(format!(
                "unknown_prefer_profile:{}",
                prefer_profile.as_deref().unwrap_or("missing")
            ))
        } else if prefer_profile.as_deref().is_some_and(|profile| {
            risk.blocked_profiles
                .iter()
                .any(|blocked| blocked == profile)
        }) {
            Some(format!(
                "blocked_by_risk_classifier:{}",
                prefer_profile.as_deref().unwrap_or("missing")
            ))
        } else {
            None
        };

        if let Some(reason) = skip_reason {
            skipped.push(SkippedRoutePolicyRule {
                proposal_id,
                reason,
                policy,
                task_type,
                prefer_profile,
                sample_count: Some(sample_count),
            });
            continue;
        }

        applied.push(AppliedRoutePolicyRule {
            proposal_id,
            policy: policy.unwrap_or_else(|| "unknown".to_string()),
            task_type: task_type.unwrap_or_else(|| risk.task_type.clone()),
            prefer_profile: prefer_profile.unwrap_or_default(),
            sample_count,
            score_delta,
            status: status.to_string(),
        });
    }

    Ok(RoutePolicyRuleLoadout {
        namespace: ROUTE_POLICY_RULE_NS,
        min_samples: MIN_ROUTE_POLICY_RULE_SAMPLES,
        applied,
        skipped,
    })
}

pub(super) fn route_policy_rule_sample_count(value: &Value) -> u32 {
    value
        .get("evidence")
        .and_then(|evidence| evidence.get("proposed"))
        .and_then(|proposed| proposed.get("samples"))
        .and_then(Value::as_u64)
        .or_else(|| {
            value
                .get("evidence")
                .and_then(|evidence| evidence.get("row_count"))
                .and_then(Value::as_u64)
        })
        .unwrap_or(0)
        .min(u32::MAX as u64) as u32
}

pub(super) fn apply_route_policy_rules_to_candidates(
    candidates: &mut [ProfileCandidate],
    rules: &RoutePolicyRuleLoadout,
    risk: &DispatchRisk,
) {
    for rule in &rules.applied {
        if rule.task_type != risk.task_type {
            continue;
        }
        if let Some(candidate) = candidates
            .iter_mut()
            .find(|candidate| candidate.profile == rule.prefer_profile)
        {
            candidate.score = round2(candidate.score + ROUTE_POLICY_RULE_SCORE_BONUS);
            candidate.reasons.push(format!(
                "approved_route_policy_rule:{} policy={} samples={} score_delta={:.2}",
                rule.proposal_id,
                rule.policy,
                rule.sample_count,
                rule.score_delta.unwrap_or_default()
            ));
        }
    }
}

pub(super) fn simulate_route_policy(
    policy: &str,
    performance_matrix: &[AgentPerformanceMatrixRow],
    focus: Option<&DispatchRisk>,
) -> RouteSimulationSummary {
    let profile_names = DISPATCH_PROFILES
        .iter()
        .map(|profile| profile.name)
        .collect::<Vec<_>>();
    let mut by_task: HashMap<String, Vec<&AgentPerformanceMatrixRow>> = HashMap::new();
    for row in performance_matrix {
        if row.scope != "leader" {
            continue;
        }
        let Some(profile) = row.profile.as_deref() else {
            continue;
        };
        if !profile_names.contains(&profile) {
            continue;
        }
        if let Some(focus) = focus {
            if row.task_type != focus.task_type {
                continue;
            }
        }
        by_task.entry(row.task_type.clone()).or_default().push(row);
    }

    let mut choices = Vec::new();
    for (task_type, rows) in by_task {
        let mut scored = rows
            .into_iter()
            .filter_map(|row| {
                row.profile.as_deref()?;
                let score = route_policy_score(policy, row, focus);
                Some((row, score, route_policy_reasons(policy, row, focus)))
            })
            .collect::<Vec<_>>();
        scored.sort_by(|(a, a_score, _), (b, b_score, _)| {
            compare_scores_desc(*a_score, *b_score).then_with(|| {
                a.profile
                    .as_deref()
                    .unwrap_or("")
                    .cmp(b.profile.as_deref().unwrap_or(""))
            })
        });
        if let Some((row, score, reasons)) = scored.first() {
            choices.push(RouteSimulationChoice {
                task_type,
                profile: row.profile.clone().unwrap_or_default(),
                agent: row.agent.clone(),
                samples: row.samples,
                score: round2(*score),
                success_rate: row.success_rate,
                verification_rate: row.verification_rate,
                failure_count: row.failure_count,
                avg_latency_ms: row.avg_latency_ms,
                avg_cost_usd: row.avg_cost_usd,
                avg_retry_count: row.avg_retry_count,
                human_override_rate: row.human_override_rate,
                reasons: reasons.clone(),
            });
        }
    }
    choices.sort_by(|a, b| a.task_type.cmp(&b.task_type));

    summarize_route_simulation(policy, choices, focus)
}

pub(super) fn build_route_policy_proposals(
    current: &RouteSimulationSummary,
    variants: &[RouteSimulationSummary],
    row_count: usize,
    limit: usize,
) -> Vec<Value> {
    let current_by_task = current
        .route_choices
        .iter()
        .map(|choice| (choice.task_type.as_str(), choice))
        .collect::<HashMap<_, _>>();
    let mut out = Vec::new();
    for variant in variants {
        for choice in &variant.route_choices {
            let Some(current_choice) = current_by_task.get(choice.task_type.as_str()) else {
                continue;
            };
            if current_choice.profile == choice.profile {
                continue;
            }
            let id = format!(
                "route_policy:{}:{}:{}",
                sanitize_policy_key(&variant.policy),
                sanitize_policy_key(&choice.task_type),
                sanitize_policy_key(&choice.profile)
            );
            out.push(json!({
                "proposal_id": id,
                "kind": "route_policy",
                "status": "pending",
                "requires_human_approval": true,
                "created_or_refreshed_at": Utc::now().to_rfc3339(),
                "policy": variant.policy,
                "task_type": choice.task_type,
                "current_profile": current_choice.profile,
                "proposed_profile": choice.profile,
                "current_score": current_choice.score,
                "proposed_score": choice.score,
                "score_delta": round2(choice.score - current_choice.score),
                "policy_rule": {
                    "when_task_type": choice.task_type,
                    "prefer_profile": choice.profile,
                    "policy": variant.policy,
                    "fallback_to_current_profile": current_choice.profile,
                },
                "evidence": {
                    "source": "live_memory_eval",
                    "row_count": row_count,
                    "limit": limit,
                    "current": current_choice,
                    "proposed": choice,
                    "route_simulate_call": "tachi_task(action='route_simulate', limit=...)",
                },
                "rationale": format!(
                    "{} replay prefers {} over current {} for {}",
                    variant.policy, choice.profile, current_choice.profile, choice.task_type
                ),
            }));
        }
    }
    out
}

#[derive(Default)]
pub(super) struct LoadoutSkillEvidence {
    hits: u32,
    verified: u32,
    success: u32,
    quality_sum: f64,
    quality_count: u32,
    task_types: HashMap<String, u32>,
    eval_refs: Vec<String>,
}

pub(super) fn build_loadout_evolution_proposals(
    server: &MemoryServer,
    performance_matrix: &[AgentPerformanceMatrixRow],
    limit: usize,
) -> Result<Vec<Value>, String> {
    let entries = load_live_eval_entries(server, limit)?;
    let mut out = Vec::new();

    for profile in DISPATCH_PROFILES {
        let profile_rows = performance_matrix
            .iter()
            .filter(|row| row.profile.as_deref() == Some(profile.name))
            .cloned()
            .collect::<Vec<_>>();
        let profile_samples = sum_matrix_samples(&profile_rows);
        if profile_samples < MIN_LOADOUT_EVOLUTION_SAMPLES {
            out.extend(build_card_risk_evolution_proposals(
                profile,
                &profile_rows,
                &entries,
                profile_samples,
                &profile_weak_against_for_server(server, profile)?
                    .into_iter()
                    .collect::<HashSet<_>>(),
                &profile_demotion_targets(server, profile)?
                    .into_iter()
                    .collect::<HashSet<_>>(),
                &profile_required_skill_ids_for_server(server, profile)?,
                limit,
            ));
            continue;
        }
        let failure_count = sum_matrix_failures(&profile_rows);
        let human_override_rate =
            weighted_matrix_rate(&profile_rows, |row| Some(row.human_override_rate)).unwrap_or(0.0);
        let avg_retry_count =
            weighted_matrix_rate(&profile_rows, |row| Some(row.avg_retry_count)).unwrap_or(0.0);
        let success_rate =
            weighted_matrix_rate(&profile_rows, |row| row.success_rate).unwrap_or(0.0);
        let useful_rate = weighted_matrix_rate(&profile_rows, |row| row.useful_rate).unwrap_or(0.0);
        let positive_rate = success_rate.max(useful_rate);

        let existing_weak_against = profile_weak_against_for_server(server, profile)?
            .into_iter()
            .collect::<HashSet<_>>();
        let existing_demotion_targets = profile_demotion_targets(server, profile)?
            .into_iter()
            .collect::<HashSet<_>>();
        let profile_required_skills = profile_required_skill_ids_for_server(server, profile)?;
        out.extend(build_card_risk_evolution_proposals(
            profile,
            &profile_rows,
            &entries,
            profile_samples,
            &existing_weak_against,
            &existing_demotion_targets,
            &profile_required_skills,
            limit,
        ));

        if failure_count > 0
            || human_override_rate >= 0.10
            || avg_retry_count >= 1.0
            || positive_rate < 0.80
        {
            continue;
        }

        let existing_skills = profile_required_skill_ids_for_server(server, profile)?
            .into_iter()
            .chain(
                profile
                    .forbidden_skills
                    .iter()
                    .map(|skill| skill.to_string()),
            )
            .collect::<HashSet<_>>();
        let existing_passive_traits = {
            let loadout = profile_skill_loadout_json_for_server(server, profile)?;
            loadout
                .get("passive_traits")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect::<HashSet<_>>()
        };
        let existing_evidence_required = profile_evidence_required_for_server(server, profile)?
            .into_iter()
            .collect::<HashSet<_>>();
        let mut buckets: HashMap<String, LoadoutSkillEvidence> = HashMap::new();
        for entry in &entries {
            let Some(meta) = entry.metadata.as_object() else {
                continue;
            };
            if meta.get("profile").and_then(Value::as_str) != Some(profile.name) {
                continue;
            }
            let task_type = meta
                .get("task_type")
                .and_then(Value::as_str)
                .unwrap_or("other")
                .to_string();
            let outcome = meta
                .get("outcome")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_ascii_lowercase();
            let verified = meta
                .get("verification_present")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let quality = meta.get("quality_score").and_then(Value::as_f64);
            let skills = meta
                .get("skills_used")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|skill| !skill.is_empty())
                .map(str::to_string)
                .collect::<Vec<_>>();

            for skill in skills {
                if existing_skills.contains(&skill) {
                    continue;
                }
                let evidence = buckets.entry(skill).or_default();
                evidence.hits += 1;
                if verified {
                    evidence.verified += 1;
                }
                if matches!(outcome.as_str(), "success" | "completed") {
                    evidence.success += 1;
                }
                if let Some(quality) = quality {
                    evidence.quality_sum += quality;
                    evidence.quality_count += 1;
                }
                *evidence.task_types.entry(task_type.clone()).or_insert(0) += 1;
                if evidence.eval_refs.len() < 5 {
                    evidence.eval_refs.push(entry.path.clone());
                }
            }
        }

        let min_skill_hits = (profile_samples / 2).max(3);
        for proposal in build_passive_trait_evolution_proposals(
            profile,
            &profile_rows,
            profile_samples,
            &existing_passive_traits,
            limit,
            min_skill_hits,
        ) {
            out.push(proposal);
        }
        for proposal in build_evidence_contract_evolution_proposals(
            profile,
            &profile_rows,
            profile_samples,
            &existing_evidence_required,
            limit,
            min_skill_hits,
        ) {
            out.push(proposal);
        }
        for (skill, evidence) in buckets {
            if evidence.hits < min_skill_hits {
                continue;
            }
            let verified_rate = evidence.verified as f64 / evidence.hits as f64;
            let success_rate = evidence.success as f64 / evidence.hits as f64;
            if verified_rate < 0.50 || success_rate < 0.80 {
                continue;
            }
            let avg_quality = (evidence.quality_count > 0)
                .then(|| evidence.quality_sum / evidence.quality_count as f64);
            let id = format!(
                "loadout_evolution:{}:promote_signature:{}",
                sanitize_policy_key(profile.name),
                sanitize_policy_key(&skill)
            );
            out.push(json!({
                "proposal_id": id,
                "kind": "loadout_evolution",
                "status": "pending",
                "requires_human_approval": true,
                "created_or_refreshed_at": Utc::now().to_rfc3339(),
                "profile": profile.name,
                "operation": "promote_observed_skill_to_signature",
                "skill_id": skill.clone(),
                "current_loadout": profile_skill_loadout_json(profile),
                "proposed_patch": {
                    "add_signature_skills": [skill.clone()],
                    "preserve_common_skills": profile.common_skills,
                    "preserve_forbidden_skills": profile.forbidden_skills,
                },
                "evidence": {
                    "source": "live_memory_eval",
                    "limit": limit,
                    "profile_samples": profile_samples,
                    "min_samples_for_evolution": MIN_LOADOUT_EVOLUTION_SAMPLES,
                    "min_skill_hits": min_skill_hits,
                    "skill_hits": evidence.hits,
                    "verified_rate": round2(verified_rate),
                    "success_rate": round2(success_rate),
                    "avg_quality_score": avg_quality.map(round2),
                    "profile_summary": summarize_matrix_rows(&profile_rows),
                    "task_types": evidence.task_types,
                    "eval_refs": evidence.eval_refs,
                    "loadout_call": "tachi_skill(action='loadout', profile=..., limit=...)",
                },
                "rationale": format!(
                    "{} appeared in {}/{} verified successful {} runs and is not part of the current sparse loadout",
                    skill, evidence.hits, profile_samples, profile.name
                ),
                "projection": {
                    "status": "pending_profile_card_projection",
                    "note": "Human approval records the proposal; apply_proposals projects approved changes into the profile/card overlay."
                }
            }));
        }
    }

    Ok(out)
}

pub(super) fn build_passive_trait_evolution_proposals(
    profile: &DispatchProfileDef,
    profile_rows: &[AgentPerformanceMatrixRow],
    profile_samples: u32,
    existing_passive_traits: &HashSet<String>,
    limit: usize,
    min_task_hits: u32,
) -> Vec<Value> {
    let mut out = Vec::new();
    let mut proposed_traits = HashSet::new();
    for row in profile_rows {
        if row.samples < min_task_hits {
            continue;
        }
        if row.failure_count > 0
            || row.verification_rate < 0.50
            || row.human_override_rate >= 0.10
            || row.avg_retry_count >= 1.0
            || row.success_rate.or(row.useful_rate).unwrap_or(0.0) < 0.80
        {
            continue;
        }
        let Some((trait_id, trait_label)) = passive_trait_for_task_type(&row.task_type) else {
            continue;
        };
        if existing_passive_traits.contains(trait_id) {
            continue;
        }
        if !proposed_traits.insert(trait_id.to_string()) {
            continue;
        }
        let id = format!(
            "loadout_evolution:{}:add_passive_trait:{}",
            sanitize_policy_key(profile.name),
            sanitize_policy_key(trait_id)
        );
        out.push(json!({
            "proposal_id": id,
            "kind": "loadout_evolution",
            "status": "pending",
            "requires_human_approval": true,
            "created_or_refreshed_at": Utc::now().to_rfc3339(),
            "profile": profile.name,
            "operation": "add_evidence_backed_passive_trait",
            "trait_id": trait_id,
            "trait_label": trait_label,
            "current_loadout": profile_skill_loadout_json(profile),
            "proposed_patch": {
                "add_passive_traits": [trait_id],
                "preserve_signature_skills": profile.signature_skills,
                "preserve_forbidden_skills": profile.forbidden_skills,
            },
            "evidence": {
                "source": "live_memory_eval",
                "limit": limit,
                "profile_samples": profile_samples,
                "min_samples_for_evolution": MIN_LOADOUT_EVOLUTION_SAMPLES,
                "min_task_hits": min_task_hits,
                "task_type": row.task_type,
                "task_samples": row.samples,
                "verification_rate": round2(row.verification_rate),
                "success_rate": row.success_rate.map(round2),
                "useful_rate": row.useful_rate.map(round2),
                "avg_retry_count": round2(row.avg_retry_count),
                "human_override_rate": round2(row.human_override_rate),
                "profile_summary": summarize_matrix_rows(profile_rows),
                "loadout_call": "tachi_skill(action='loadout', profile=..., limit=...)",
            },
            "rationale": format!(
                "{} has {} clean verified {} samples; add passive trait {}",
                profile.name, row.samples, row.task_type, trait_id
            ),
            "projection": {
                "status": "pending_profile_card_projection",
                "note": "Human approval records the proposal; apply_proposals projects approved passive traits into the profile/card overlay."
            }
        }));
    }
    out
}

pub(super) fn build_evidence_contract_evolution_proposals(
    profile: &DispatchProfileDef,
    profile_rows: &[AgentPerformanceMatrixRow],
    profile_samples: u32,
    existing_evidence_required: &HashSet<String>,
    limit: usize,
    min_task_hits: u32,
) -> Vec<Value> {
    let mut out = Vec::new();
    let mut proposed_evidence = HashSet::new();
    for row in profile_rows {
        if row.samples < min_task_hits {
            continue;
        }
        if row.failure_count > 0
            || row.verification_rate < 0.50
            || row.human_override_rate >= 0.10
            || row.avg_retry_count >= 1.0
            || row.success_rate.or(row.useful_rate).unwrap_or(0.0) < 0.80
        {
            continue;
        }
        let Some((evidence_id, evidence_label)) =
            evidence_contract_target_for_task_type(&row.task_type)
        else {
            continue;
        };
        if existing_evidence_required.contains(evidence_id) {
            continue;
        }
        if !proposed_evidence.insert(evidence_id.to_string()) {
            continue;
        }
        let id = format!(
            "loadout_evolution:{}:add_evidence_required:{}",
            sanitize_policy_key(profile.name),
            sanitize_policy_key(evidence_id)
        );
        out.push(json!({
            "proposal_id": id,
            "kind": "loadout_evolution",
            "status": "pending",
            "requires_human_approval": true,
            "created_or_refreshed_at": Utc::now().to_rfc3339(),
            "profile": profile.name,
            "operation": "add_evidence_contract_required",
            "evidence_id": evidence_id,
            "evidence_label": evidence_label,
            "current_evidence_contract": profile_evidence_contract_json(profile),
            "proposed_patch": {
                "add_evidence_required": [evidence_id],
                "preserve_baseline_required": profile.evidence_required,
            },
            "evidence": {
                "source": "live_memory_eval",
                "limit": limit,
                "profile_samples": profile_samples,
                "min_samples_for_evolution": MIN_LOADOUT_EVOLUTION_SAMPLES,
                "min_task_hits": min_task_hits,
                "task_type": row.task_type,
                "task_samples": row.samples,
                "verification_rate": round2(row.verification_rate),
                "success_rate": row.success_rate.map(round2),
                "useful_rate": row.useful_rate.map(round2),
                "avg_retry_count": round2(row.avg_retry_count),
                "human_override_rate": round2(row.human_override_rate),
                "profile_summary": summarize_matrix_rows(profile_rows),
                "loadout_call": "tachi_skill(action='loadout', profile=..., limit=...)",
            },
            "rationale": format!(
                "{} has {} clean verified {} samples; require evidence artifact {}",
                profile.name, row.samples, row.task_type, evidence_id
            ),
            "projection": {
                "status": "pending_profile_card_projection",
                "note": "Human approval records the proposal; apply_proposals projects approved evidence requirements into the profile/card overlay."
            }
        }));
    }
    out
}

pub(super) fn build_card_risk_evolution_proposals(
    profile: &DispatchProfileDef,
    profile_rows: &[AgentPerformanceMatrixRow],
    entries: &[memory_core::MemoryEntry],
    profile_samples: u32,
    existing_weak_against: &HashSet<String>,
    existing_demotion_targets: &HashSet<String>,
    profile_required_skills: &[String],
    limit: usize,
) -> Vec<Value> {
    let mut out = Vec::new();
    let mut proposed_weaknesses = HashSet::new();
    let mut bad_task_types = HashSet::new();
    for row in profile_rows {
        if row.samples < MIN_CARD_RISK_EVOLUTION_SAMPLES {
            continue;
        }
        let risk_signal =
            row.failure_count >= 2 || row.human_override_rate >= 0.25 || row.avg_retry_count >= 1.5;
        if !risk_signal {
            continue;
        }
        bad_task_types.insert(row.task_type.clone());
        let weakness_id = row.task_type.clone();
        if !existing_weak_against.contains(&weakness_id)
            && proposed_weaknesses.insert(weakness_id.clone())
        {
            let id = format!(
                "loadout_evolution:{}:add_card_weakness:{}",
                sanitize_policy_key(profile.name),
                sanitize_policy_key(&weakness_id)
            );
            out.push(json!({
                "proposal_id": id,
                "kind": "loadout_evolution",
                "status": "pending",
                "requires_human_approval": true,
                "created_or_refreshed_at": Utc::now().to_rfc3339(),
                "profile": profile.name,
                "operation": "add_card_weakness",
                "weakness_id": weakness_id,
                "weakness_label": format!("Repeated friction on {}", row.task_type),
                "current_card": profile_json(profile).get("mbit_card").cloned().unwrap_or(Value::Null),
                "proposed_patch": {
                    "add_weak_against": [row.task_type],
                    "preserve_baseline_weak_against": profile.weak_against,
                },
                "evidence": {
                    "source": "live_memory_eval",
                    "limit": limit,
                    "profile_samples": profile_samples,
                    "min_samples_for_card_risk_evolution": MIN_CARD_RISK_EVOLUTION_SAMPLES,
                    "task_type": row.task_type,
                    "task_samples": row.samples,
                    "failure_count": row.failure_count,
                    "human_override_rate": round2(row.human_override_rate),
                    "avg_retry_count": round2(row.avg_retry_count),
                    "profile_summary": summarize_matrix_rows(profile_rows),
                },
                "rationale": format!(
                    "{} has repeated friction on {}; add it to weak_against so recommendation can explain/deprioritize the match",
                    profile.name, row.task_type
                ),
                "projection": {
                    "status": "pending_profile_card_projection",
                    "note": "Human approval records the proposal; apply_proposals projects approved weakness markers into the MBIT/profile card overlay."
                }
            }));
        }
    }

    if bad_task_types.is_empty() {
        return out;
    }

    let current_skills = profile_required_skills.iter().collect::<HashSet<_>>();
    let mut skill_hits: HashMap<String, u32> = HashMap::new();
    for entry in entries {
        let Some(meta) = entry.metadata.as_object() else {
            continue;
        };
        if meta.get("profile").and_then(Value::as_str) != Some(profile.name) {
            continue;
        }
        let task_type = meta
            .get("task_type")
            .and_then(Value::as_str)
            .unwrap_or("other");
        if !bad_task_types.contains(task_type) {
            continue;
        }
        let outcome = meta
            .get("outcome")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_ascii_lowercase();
        let risky = matches!(outcome.as_str(), "failure" | "failed" | "partial")
            || meta
                .get("human_override")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            || meta.get("retry_count").and_then(Value::as_u64).unwrap_or(0) >= 2;
        if !risky {
            continue;
        }
        let skills = meta
            .get("skills_used")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::trim)
            .filter(|skill| !skill.is_empty())
            .filter(|skill| current_skills.contains(&skill.to_string()))
            .map(str::to_string)
            .collect::<Vec<_>>();
        for skill in skills {
            *skill_hits.entry(skill).or_insert(0) += 1;
        }
    }

    let min_skill_hits = MIN_CARD_RISK_EVOLUTION_SAMPLES;
    for (skill, hits) in skill_hits {
        if hits < min_skill_hits || existing_demotion_targets.contains(&skill) {
            continue;
        }
        let id = format!(
            "loadout_evolution:{}:demote_skill:{}",
            sanitize_policy_key(profile.name),
            sanitize_policy_key(&skill)
        );
        out.push(json!({
            "proposal_id": id,
            "kind": "loadout_evolution",
            "status": "pending",
            "requires_human_approval": true,
            "created_or_refreshed_at": Utc::now().to_rfc3339(),
            "profile": profile.name,
            "operation": "mark_skill_demotion_target",
            "skill_id": skill,
            "current_loadout": profile_skill_loadout_json(profile),
            "proposed_patch": {
                "demotion_targets": [skill],
                "preserve_signature_skills": profile.signature_skills,
            },
            "evidence": {
                "source": "live_memory_eval",
                "limit": limit,
                "profile_samples": profile_samples,
                "min_samples_for_card_risk_evolution": MIN_CARD_RISK_EVOLUTION_SAMPLES,
                "skill_hits": hits,
                "bad_task_types": bad_task_types,
                "profile_summary": summarize_matrix_rows(profile_rows),
            },
            "rationale": format!(
                "{} repeatedly appeared in failed/overridden/retried {} runs; mark as a demotion target for human review",
                skill, profile.name
            ),
            "projection": {
                "status": "pending_profile_card_projection",
                "note": "Human approval records the proposal; apply_proposals projects approved demotion targets into the MBIT/profile card overlay without mutating baseline skills."
            }
        }));
    }

    out
}

pub(super) fn passive_trait_for_task_type(task_type: &str) -> Option<(&'static str, &'static str)> {
    match task_type {
        "plan_request" => Some((
            "evidence_backed_planning",
            "Repeated verified planning success; keep plan-first behavior prominent.",
        )),
        "review_request" => Some((
            "evidence_backed_review_gate",
            "Repeated verified review success; keep blocker/evidence review behavior prominent.",
        )),
        "fix_request" | "refactor_request" | "migration_request" => Some((
            "evidence_backed_change_control",
            "Repeated verified change work; keep bounded-diff and regression-control behavior prominent.",
        )),
        "test_request" => Some((
            "evidence_backed_verification",
            "Repeated verified test work; keep verification-first behavior prominent.",
        )),
        _ => None,
    }
}

pub(super) fn evidence_contract_target_for_task_type(
    task_type: &str,
) -> Option<(&'static str, &'static str)> {
    match task_type {
        "plan_request" => Some((
            "acceptance_criteria",
            "Repeated verified planning success; require explicit acceptance criteria in handoffs.",
        )),
        "review_request" => Some((
            "severity_rationale",
            "Repeated verified review success; require severity rationale with findings.",
        )),
        "fix_request" | "refactor_request" | "migration_request" => Some((
            "regression_tests",
            "Repeated verified change work; require regression-test evidence with diffs.",
        )),
        "test_request" => Some((
            "test_evidence",
            "Repeated verified test work; require concrete test evidence and gaps.",
        )),
        _ => None,
    }
}

pub(super) fn load_live_eval_entries(
    server: &MemoryServer,
    limit: usize,
) -> Result<Vec<memory_core::MemoryEntry>, String> {
    let limit = limit.max(1);
    let mut entries = server.with_global_store_read(|store| {
        store
            .list_by_path("/eval", limit, false)
            .map_err(|e| format!("list global eval entries: {e}"))
    })?;
    if server.has_project_db() {
        let mut project_entries = server.with_project_store_read(|store| {
            store
                .list_by_path("/eval", limit, false)
                .map_err(|e| format!("list project eval entries: {e}"))
        })?;
        entries.append(&mut project_entries);
    }
    entries.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
    entries.truncate(limit);
    Ok(entries)
}

pub(super) fn sanitize_policy_key(value: &str) -> String {
    let sanitized = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>();
    sanitized.trim_matches('-').to_string()
}

pub(super) fn route_policy_score(
    policy: &str,
    row: &AgentPerformanceMatrixRow,
    focus: Option<&DispatchRisk>,
) -> f64 {
    let success = row.success_rate.unwrap_or(0.0);
    let quality = row.avg_quality_score.unwrap_or(success);
    let verification = row.verification_rate;
    let cost = row.avg_cost_usd.unwrap_or(0.0);
    let latency_minutes = row.avg_latency_ms.unwrap_or(0.0) / 60_000.0;
    let retry = row.avg_retry_count;
    let override_rate = row.human_override_rate;
    let failure_rate = if row.samples > 0 {
        row.failure_count as f64 / row.samples as f64
    } else {
        0.0
    };
    let mut score = match policy {
        "cost_sensitive" => {
            success * 45.0 + verification * 15.0 + quality * 10.0
                - cost * 35.0
                - latency_minutes * 1.5
                - retry * 10.0
                - override_rate * 20.0
                - failure_rate * 30.0
        }
        "quality_first" => {
            success * 55.0 + quality * 35.0 + verification * 20.0
                - failure_rate * 45.0
                - override_rate * 12.0
                - retry * 6.0
                - cost * 6.0
                - latency_minutes * 0.5
        }
        _ => {
            success * 45.0 + quality * 25.0 + verification * 18.0
                - failure_rate * 35.0
                - override_rate * 18.0
                - retry * 8.0
                - cost * 10.0
                - latency_minutes
        }
    };

    if let (Some(focus), Some(profile)) = (focus, row.profile.as_deref()) {
        if focus.required_profiles.iter().any(|p| p == profile) {
            score += 20.0;
        }
        if focus.blocked_profiles.iter().any(|p| p == profile) {
            score -= 30.0;
        }
    }
    score
}

pub(super) fn compare_scores_desc(left: f64, right: f64) -> std::cmp::Ordering {
    score_sort_key(right).total_cmp(&score_sort_key(left))
}

pub(super) fn score_sort_key(score: f64) -> f64 {
    if score.is_finite() {
        score
    } else {
        f64::NEG_INFINITY
    }
}

pub(super) fn route_policy_reasons(
    policy: &str,
    row: &AgentPerformanceMatrixRow,
    focus: Option<&DispatchRisk>,
) -> Vec<String> {
    let mut reasons = vec![
        format!(
            "{} samples success={:.2}",
            row.samples,
            row.success_rate.unwrap_or(0.0)
        ),
        format!("verification={:.2}", row.verification_rate),
    ];
    if row.failure_count > 0 {
        reasons.push(format!("failures={}", row.failure_count));
    }
    if row.avg_cost_usd.is_some() {
        reasons.push(format!(
            "avg_cost_usd={:.4}",
            row.avg_cost_usd.unwrap_or_default()
        ));
    }
    if row.avg_latency_ms.is_some() {
        reasons.push(format!(
            "avg_latency_ms={:.0}",
            row.avg_latency_ms.unwrap_or_default()
        ));
    }
    match policy {
        "cost_sensitive" => reasons.push("policy_prioritizes_cost_and_latency".to_string()),
        "quality_first" => {
            reasons.push("policy_prioritizes_success_quality_verification".to_string())
        }
        _ => reasons.push("policy_balances_quality_cost_and_failures".to_string()),
    }
    if let (Some(focus), Some(profile)) = (focus, row.profile.as_deref()) {
        if focus.required_profiles.iter().any(|p| p == profile) {
            reasons.push("focus_task_required_profile_bonus".to_string());
        }
        if focus.blocked_profiles.iter().any(|p| p == profile) {
            reasons.push("focus_task_blocked_profile_penalty".to_string());
        }
    }
    reasons
}

pub(super) fn summarize_route_simulation(
    policy: &str,
    choices: Vec<RouteSimulationChoice>,
    focus: Option<&DispatchRisk>,
) -> RouteSimulationSummary {
    let sample_count = choices.iter().map(|choice| choice.samples).sum::<u32>();
    let sample_count_f = sample_count as f64;
    let mut success_sum = 0.0;
    let mut success_samples = 0u32;
    let mut verification_sum = 0.0;
    let mut failures = 0u32;
    let mut retry_sum = 0.0;
    let mut override_sum = 0.0;
    let mut latency_sum = 0.0;
    let mut latency_samples = 0u32;
    let mut cost_sum = 0.0;
    let mut cost_samples = 0u32;
    let mut score_sum = 0.0;

    for choice in &choices {
        if let Some(success) = choice.success_rate {
            success_sum += success * choice.samples as f64;
            success_samples += choice.samples;
        }
        verification_sum += choice.verification_rate * choice.samples as f64;
        failures += choice.failure_count;
        if let Some(latency) = choice.avg_latency_ms {
            latency_sum += latency * choice.samples as f64;
            latency_samples += choice.samples;
        }
        if let Some(cost) = choice.avg_cost_usd {
            cost_sum += cost * choice.samples as f64;
            cost_samples += choice.samples;
        }
        retry_sum += choice.avg_retry_count * choice.samples as f64;
        override_sum += choice.human_override_rate * choice.samples as f64;
        score_sum += choice.score * choice.samples as f64;
    }

    let mut caveats = Vec::new();
    if sample_count == 0 {
        caveats.push(
            "no matching leader/profile eval rows; policy comparison is evidence-empty".to_string(),
        );
    }
    if focus.is_some() && choices.is_empty() {
        caveats.push("focus task type has no matching live eval rows".to_string());
    }
    if sample_count < 10 && sample_count > 0 {
        caveats.push("low sample count; treat as directional, not learned policy".to_string());
    }

    RouteSimulationSummary {
        policy: policy.to_string(),
        selected_route_count: choices.len() as u32,
        sample_count,
        estimated_success_rate: (success_samples > 0)
            .then(|| round4(success_sum / success_samples as f64)),
        estimated_verification_rate: (sample_count > 0)
            .then(|| round4(verification_sum / sample_count_f)),
        failure_count: failures,
        avg_retry_count: (sample_count > 0).then(|| round4(retry_sum / sample_count_f)),
        avg_human_override_rate: (sample_count > 0).then(|| round4(override_sum / sample_count_f)),
        avg_latency_ms: (latency_samples > 0).then(|| round2(latency_sum / latency_samples as f64)),
        avg_cost_usd: (cost_samples > 0).then(|| round4(cost_sum / cost_samples as f64)),
        total_cost_usd: (cost_samples > 0).then(|| round4(cost_sum)),
        score: if sample_count > 0 {
            round2(score_sum / sample_count_f)
        } else {
            0.0
        },
        route_choices: choices,
        caveats,
    }
}

pub(super) fn route_simulation_caveats(
    rows: &[EvalRow],
    performance_matrix: &[AgentPerformanceMatrixRow],
) -> Vec<String> {
    let mut caveats = vec![
        "simulation is replay-only and does not mutate routing policy".to_string(),
        "raw child transcripts are not loaded; only compact /eval evidence is used".to_string(),
    ];
    if rows.is_empty() {
        caveats.push(
            "no /eval rows found; recommendations must fall back to deterministic MBIT/risk fit"
                .to_string(),
        );
    }
    let leader_profile_rows = performance_matrix
        .iter()
        .filter(|row| row.scope == "leader" && row.profile.is_some())
        .count();
    if leader_profile_rows == 0 && !rows.is_empty() {
        caveats.push("live eval rows exist but none have leader profile ids; record profile during completion for policy replay".to_string());
    }
    caveats
}
