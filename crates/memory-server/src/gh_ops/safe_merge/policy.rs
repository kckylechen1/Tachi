use super::*;

pub(in crate::gh_ops) fn parse_merge_strategy(raw: Option<&str>) -> Result<MergeStrategy, String> {
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

pub(in crate::gh_ops) fn effective_safe_merge_dry_run(
    confirm: bool,
    requested_dry_run: Option<bool>,
) -> bool {
    !confirm || requested_dry_run.unwrap_or(false)
}

pub(in crate::gh_ops) fn verification_satisfies_head_consistency(
    gate: Option<&Value>,
    policy: MergeGatePolicy,
) -> bool {
    policy.require_head_consistency
        && gate.and_then(|v| v.get("overall")).and_then(Value::as_str) == Some("passed")
}

pub(in crate::gh_ops) fn apply_verification_gate_to_decision(
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

pub(in crate::gh_ops) fn parse_merge_gate_policy(
    raw: Option<&str>,
) -> Result<MergeGatePolicy, String> {
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

pub(in crate::gh_ops) fn merge_strategy_flag(s: MergeStrategy) -> &'static str {
    match s {
        MergeStrategy::Squash => "--squash",
        MergeStrategy::Merge => "--merge",
        MergeStrategy::Rebase => "--rebase",
    }
}
