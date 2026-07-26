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
        let (raw, version) = store
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
                // Legacy route_policy proposals (pre-v3 schema) carry no
                // content-addressed binding, so what the human reviewed is no
                // guarantee of what apply will persist. Refuse loudly; the row
                // remains listable with `legacy_unbound_proposal: true` but
                // cannot be applied.
                if value.get("schema_version").and_then(Value::as_u64)
                    != Some(tachi_dispatch::policy::ROUTE_POLICY_PROPOSAL_SCHEMA_VERSION)
                {
                    return Err(format!(
                        "legacy_unbound_proposal: {proposal_id} predates the v3 content-addressed identity and cannot be applied; regenerate with action='proposals' to mint a fresh pending v3 proposal"
                    ));
                }
                // Overwrite the top-level `policy_rule` / `evidence` fields
                // with the digest-validated `identity_payload` copies
                // immediately before persisting. `routing.rs`'s live
                // consumer (`build_route_policy_rule_loadout`, run on every
                // routing decision, unmodified by this PR and also reading
                // legacy pre-v3 rows already applied before this change)
                // reads `policy_rule` and `evidence` from the top level, NOT
                // from `identity_payload` — so it cannot be repointed at the
                // bound copy without breaking those pre-existing legacy rows.
                // At this point the drift check above has already proven the
                // two copies are canonically equal, so this write is a
                // normalization (stable key order, no literal-vs-canonical
                // mismatch), never a content change — the refusal above is
                // what actually stops a drifted proposal; this is defense in
                // depth for what lands in the namespace every routing
                // decision reads.
                // Atomically write proposal + route rule in ONE SQLite
                // transaction with a hard_state version CAS on the proposal
                // row's approved -> applied transition. A late write failure
                // (the rule write, the commit, the CAS itself) rolls back both
                // rows: the apply either fully lands or leaves no trace.
                let tx = store
                    .connection_mut()
                    .transaction()
                    .map_err(|e| format!("open route policy apply tx: {e}"))?;
                let source_rows = memcore::db::list_state(&tx, ROUTE_POLICY_RULE_NS)
                    .map_err(|e| format!("list active route policy rules in apply tx: {e}"))?;
                let live_source_revision = super::handlers::route_policy_source_revision(&source_rows);
                let identity_payload = super::handlers::validate_route_policy_proposal(
                    proposal_id,
                    &value,
                    Some(&live_source_revision),
                )?;
                if let Some(bound_apply_payload) = identity_payload.get("apply_payload") {
                    value["policy_rule"] = bound_apply_payload.clone();
                }
                if let Some(bound_evidence) = identity_payload.get("evidence_review") {
                    value["evidence"] = bound_evidence.clone();
                }
                value["status"] = json!("applied");
                value["applied_at"] = json!(applied_at);
                let next = serde_json::to_string(&value)
                    .map_err(|e| format!("serialize applied route policy proposal: {e}"))?;
                let cas_ok = memcore::db::set_state_if_version(
                    &tx,
                    DISPATCH_POLICY_PROPOSAL_NS,
                    proposal_id,
                    &next,
                    version,
                )
                .map_err(|e| format!("CAS applied route policy proposal: {e}"))?;
                if !cas_ok {
                    return Err(format!(
                        "stale_state_version: route policy proposal {proposal_id} changed before apply; reload and retry"
                    ));
                }
                memcore::db::set_state(&tx, ROUTE_POLICY_RULE_NS, proposal_id, &next)
                    .map_err(|e| format!("persist route policy rule: {e}"))?;
                tx.commit()
                    .map_err(|e| format!("commit route policy apply tx: {e}"))?;
            }
            "loadout_evolution" => {
                let identity_payload = super::handlers::validate_loadout_evolution_proposal(
                    proposal_id,
                    &value,
                    None,
                )?;
                let apply_payload = identity_payload
                    .get("apply_payload")
                    .cloned()
                    .ok_or_else(|| {
                        format!(
                            "loadout_evolution proposal {proposal_id} missing digest-bound apply payload"
                        )
                    })?;
                let profile_name = apply_payload
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
                let operation = apply_payload
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

                // Reserve the write transaction before reading the effective
                // overlay source. The exact row version/content snapshot is
                // both revalidated against the proposal identity and retained
                // for the explicit overlay CAS below.
                let tx = store
                    .connection_mut()
                    .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
                    .map_err(|e| format!("open loadout evolution apply tx: {e}"))?;
                let overlay_snapshot =
                    memcore::db::get_state(&tx, PROFILE_CARD_OVERLAY_NS, profile.name)
                        .map_err(|e| format!("load profile/card overlay in apply tx: {e}"))?;
                let live_source_revision = super::handlers::loadout_evolution_source_revision(
                    profile,
                    overlay_snapshot.as_ref(),
                );
                super::handlers::validate_loadout_evolution_proposal(
                    proposal_id,
                    &value,
                    Some(&live_source_revision),
                )?;

                let mut overlay = if let Some((raw, _version)) = overlay_snapshot.as_ref() {
                    serde_json::from_str::<Value>(raw)
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
                        let skill_id = apply_payload
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
                        let trait_id = apply_payload
                            .get("trait_id")
                            .and_then(Value::as_str)
                            .map(str::trim)
                            .filter(|trait_id| !trait_id.is_empty())
                            .map(str::to_string)
                            .or_else(|| {
                                apply_payload
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
                        let evidence_id = apply_payload
                            .get("evidence_id")
                            .and_then(Value::as_str)
                            .map(str::trim)
                            .filter(|evidence_id| !evidence_id.is_empty())
                            .map(str::to_string)
                            .or_else(|| {
                                apply_payload
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
                        let weakness_id = apply_payload
                            .get("weakness_id")
                            .and_then(Value::as_str)
                            .map(str::trim)
                            .filter(|weakness_id| !weakness_id.is_empty())
                            .map(str::to_string)
                            .or_else(|| {
                                apply_payload
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
                        let skill_id = apply_payload
                            .get("skill_id")
                            .and_then(Value::as_str)
                            .map(str::trim)
                            .filter(|skill_id| !skill_id.is_empty())
                            .map(str::to_string)
                            .or_else(|| {
                                apply_payload
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
                // Proposal lifecycle and overlay projection are independent
                // per-row CAS operations in one transaction. Either stale row
                // or either write failure rolls back both rows.
                let cas_ok = memcore::db::set_state_if_version(
                    &tx,
                    DISPATCH_POLICY_PROPOSAL_NS,
                    proposal_id,
                    &next,
                    version,
                )
                .map_err(|e| format!("CAS applied loadout evolution proposal: {e}"))?;
                if !cas_ok {
                    return Err(format!(
                        "stale_state_version: loadout_evolution proposal {proposal_id} changed before apply; reload and retry"
                    ));
                }
                let overlay_cas_ok = match overlay_snapshot.as_ref() {
                    Some((_raw, expected_version)) => memcore::db::set_state_if_version(
                        &tx,
                        PROFILE_CARD_OVERLAY_NS,
                        profile.name,
                        &overlay_raw,
                        *expected_version,
                    ),
                    None => memcore::db::insert_state_if_absent(
                        &tx,
                        PROFILE_CARD_OVERLAY_NS,
                        profile.name,
                        &overlay_raw,
                    ),
                }
                .map_err(|e| format!("CAS profile/card overlay: {e}"))?;
                if !overlay_cas_ok {
                    return Err(format!(
                        "stale_overlay_version: profile/card overlay {} changed before loadout_evolution proposal {proposal_id} could apply; reload, regenerate, and re-review",
                        profile.name
                    ));
                }
                tx.commit()
                    .map_err(|e| format!("commit loadout evolution apply tx: {e}"))?;
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
