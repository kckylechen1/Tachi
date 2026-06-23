use super::super::*;

pub(in crate::dispatch_profile) fn load_route_policy_rule_loadout(
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

fn route_policy_rule_sample_count(value: &Value) -> u32 {
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

pub(in crate::dispatch_profile) fn apply_route_policy_rules_to_candidates(
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
