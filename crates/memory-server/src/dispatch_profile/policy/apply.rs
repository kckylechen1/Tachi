use super::super::*;

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
