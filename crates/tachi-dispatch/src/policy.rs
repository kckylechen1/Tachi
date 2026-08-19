use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap, HashSet};

use crate::eval::AgentPerformanceMatrixRow;
use crate::{
    profile_evidence_contract_json, profile_json, profile_matches_agent, profile_role_matches,
    profile_skill_loadout_json, sanitize_policy_key, DispatchProfileDef, RouteSimulationSummary,
    DISPATCH_PROFILES, MIN_CARD_RISK_EVOLUTION_SAMPLES, MIN_LOADOUT_EVOLUTION_SAMPLES,
};

/// Policy-version tag bound into every v3 route-policy proposal identity. The
/// identity binds the *complete immutable apply payload + the evidence used
/// for review + this policy version + the apply target*, so any change to the
/// apply shape (a new field on `policy_rule`, a renamed target, or this const
/// itself) rotates every proposal id and forces re-review. Bumped only on a
/// breaking change to the proposal schema/apply payload.
pub const ROUTE_POLICY_PROPOSAL_SCHEMA_VERSION: u64 = 3;
pub const ROUTE_POLICY_PROPOSAL_POLICY_VERSION: &str = "2026-07-route-policy-v3";
pub const ROUTE_POLICY_PROPOSAL_KIND: &str = "route_policy";

/// Stable apply-target tag bound into the identity, so a proposal cannot be
/// replayed against a different namespace (route-rule vs profile overlay) by
/// swapping the target field alone.
pub const ROUTE_POLICY_PROPOSAL_TARGET: &str = "route_policy_rule";

/// Recursively re-serialize a [`Value`] into canonical form: every JSON object
/// becomes a sorted-key `BTreeMap`, every array is canonicalized element-wise,
/// and scalars pass through untouched. The output is deterministic regardless
/// of the input's insertion order, which is the property the v3 content-addressed
/// identity (see [`route_policy_v3_identity_payload`]) requires: two callers
/// that built the "same" proposal from different code paths must hash to the
/// same id, and any change to the apply payload, review evidence, policy
/// version, or target must produce a different id.
///
/// `pub` (not crate-private) so the server crate's display/bound drift checks
/// — comparing an unbound top-level display field (`policy_rule`, `evidence`,
/// `config_env`) against its digest-bound `identity_payload` counterpart —
/// normalize through the SAME canonicalization the identity hash itself uses,
/// rather than inventing a second comparison rule that could silently drift
/// from this one. See `canonical_json_eq`.
pub fn canonical_json(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut sorted = BTreeMap::new();
            for (key, value) in map {
                sorted.insert(key.clone(), canonical_json(value));
            }
            Value::Object(sorted.into_iter().collect())
        }
        Value::Array(items) => Value::Array(items.iter().map(canonical_json).collect()),
        scalar => scalar.clone(),
    }
}

/// `true` iff `a` and `b` are equal after canonicalization through the same
/// [`canonical_json`] the v3 identity hash uses. The single comparison rule
/// for every display-copy-vs-bound-copy drift check in the server crate
/// (route policy's `policy_rule`/`evidence`, recall's `config_env`) — never
/// duplicate this as a second normalization.
pub fn canonical_json_eq(a: &Value, b: &Value) -> bool {
    canonical_json(a) == canonical_json(b)
}

/// Build the canonical identity payload that the v3 proposal id hashes. The
/// returned value is a fully canonicalized [`Value`] (sorted keys top to
/// bottom) ready to be serialized and SHA-256 hashed by the caller — the
/// dispatch crate is intentionally free of a crypto dependency, so the hash
/// itself is computed in the server crate where `sha2` is already in scope.
///
/// What is bound (and thus what rotates the id when it changes):
/// * `policy_version` — the proposal-schema version ([`ROUTE_POLICY_PROPOSAL_POLICY_VERSION`]);
/// * `target` — the apply target tag ([`ROUTE_POLICY_PROPOSAL_TARGET`]);
/// * `apply_payload` — the complete immutable payload that apply will persist
///   (the `policy_rule` body, including `fallback_to_current_profile`);
/// * `evidence_review` — the evidence snapshot the human reviewed.
///
/// What is *not* bound (and thus must never rotate the id): `status`,
/// `created_or_refreshed_at`, `review`, `applied_at`, `requires_human_approval`.
pub fn route_policy_v3_identity_payload(
    apply_payload: &Value,
    evidence_review: &Value,
    policy_version: &str,
    target: &str,
    source_revision: &str,
) -> Value {
    let mut map: BTreeMap<String, Value> = BTreeMap::new();
    map.insert("kind".to_string(), json!(ROUTE_POLICY_PROPOSAL_KIND));
    map.insert("policy_version".to_string(), json!(policy_version));
    map.insert("source_revision".to_string(), json!(source_revision));
    map.insert("target".to_string(), json!(target));
    map.insert("apply_payload".to_string(), canonical_json(apply_payload));
    map.insert(
        "evidence_review".to_string(),
        canonical_json(evidence_review),
    );
    Value::Object(map.into_iter().collect())
}

/// Same content-addressing helper for recall-config proposals. The dispatch
/// crate has no recall-specific types (recall lives in the server crate), so
/// this is a thin canonicalizer over a caller-supplied apply payload
/// (`config_env`) and review evidence. The server crate's
/// `recall_proposal_ops` is the only caller.
pub const RECALL_CONFIG_PROPOSAL_SCHEMA_VERSION: u64 = 3;
pub const RECALL_CONFIG_PROPOSAL_POLICY_VERSION: &str = "2026-07-recall-config-v3";
pub const RECALL_CONFIG_PROPOSAL_KIND: &str = "recall_config";
pub const RECALL_CONFIG_PROPOSAL_TARGET: &str = "recall_config_env";

pub fn recall_config_v3_identity_payload(
    config_env: &Value,
    evidence_review: &Value,
    policy_version: &str,
    target: &str,
    source_revision: &str,
) -> Value {
    let mut map: BTreeMap<String, Value> = BTreeMap::new();
    map.insert("kind".to_string(), json!(RECALL_CONFIG_PROPOSAL_KIND));
    map.insert("policy_version".to_string(), json!(policy_version));
    map.insert("source_revision".to_string(), json!(source_revision));
    map.insert("target".to_string(), json!(target));
    map.insert(
        "apply_payload".to_string(),
        json!({ "config_env": canonical_json(config_env) }),
    );
    map.insert(
        "evidence_review".to_string(),
        canonical_json(evidence_review),
    );
    Value::Object(map.into_iter().collect())
}

/// Content-addressed schema for profile/card loadout changes. The immutable
/// apply payload is deliberately separate from descriptive proposal fields so
/// a reviewer approves exactly what `apply_proposals` can mutate.
pub const LOADOUT_EVOLUTION_PROPOSAL_SCHEMA_VERSION: u64 = 3;
pub const LOADOUT_EVOLUTION_PROPOSAL_POLICY_VERSION: &str = "2026-07-loadout-evolution-v3";
pub const LOADOUT_EVOLUTION_PROPOSAL_KIND: &str = "loadout_evolution";
pub const LOADOUT_EVOLUTION_PROPOSAL_TARGET: &str = "profile_card_overlay";

/// Extract the complete payload consumed by the loadout apply operation. This
/// is also the display-copy shape checked by the server before review/apply;
/// do not add a field read by apply without binding it here.
pub fn loadout_evolution_v3_apply_payload(proposal: &Value) -> Value {
    json!({
        "profile": proposal.get("profile").cloned().unwrap_or(Value::Null),
        "operation": proposal.get("operation").cloned().unwrap_or(Value::Null),
        "skill_id": proposal.get("skill_id").cloned().unwrap_or(Value::Null),
        "trait_id": proposal.get("trait_id").cloned().unwrap_or(Value::Null),
        "evidence_id": proposal.get("evidence_id").cloned().unwrap_or(Value::Null),
        "weakness_id": proposal.get("weakness_id").cloned().unwrap_or(Value::Null),
        "proposed_patch": proposal.get("proposed_patch").cloned().unwrap_or(Value::Null),
        "current_loadout": proposal.get("current_loadout").cloned().unwrap_or(Value::Null),
        "current_evidence_contract": proposal.get("current_evidence_contract").cloned().unwrap_or(Value::Null),
        "current_card": proposal.get("current_card").cloned().unwrap_or(Value::Null),
    })
}

/// Canonical identity payload for a loadout-evolution proposal. The identity
/// binds every value apply can consume, the exact reviewer evidence, the
/// policy version, and the profile-card overlay target.
pub fn loadout_evolution_v3_identity_payload(
    apply_payload: &Value,
    evidence_review: &Value,
    policy_version: &str,
    target: &str,
    source_revision: &str,
) -> Value {
    let mut map: BTreeMap<String, Value> = BTreeMap::new();
    map.insert("kind".to_string(), json!(LOADOUT_EVOLUTION_PROPOSAL_KIND));
    map.insert("policy_version".to_string(), json!(policy_version));
    map.insert("target".to_string(), json!(target));
    map.insert("source_revision".to_string(), json!(source_revision));
    map.insert("apply_payload".to_string(), canonical_json(apply_payload));
    map.insert(
        "evidence_review".to_string(),
        canonical_json(evidence_review),
    );
    Value::Object(map.into_iter().collect())
}

#[derive(Debug, Clone)]
pub struct LoadoutEvalEntry {
    pub path: String,
    pub metadata: Value,
}

#[derive(Debug, Clone, Default)]
pub struct ProfileCardRiskInputs {
    pub existing_weak_against: HashSet<String>,
    pub existing_demotion_targets: HashSet<String>,
    pub profile_required_skills: Vec<String>,
}

#[derive(Debug, Clone, Default)]
pub struct ProfilePositiveEvolutionInputs {
    pub profile_required_skills: Vec<String>,
    pub existing_passive_traits: HashSet<String>,
    pub existing_evidence_required: HashSet<String>,
}

#[derive(Default)]
struct LoadoutSkillEvidence {
    hits: u32,
    verified: u32,
    success: u32,
    quality_sum: f64,
    quality_count: u32,
    task_types: HashMap<String, u32>,
    eval_refs: Vec<String>,
}

struct ProposalContext<'a> {
    limit: usize,
    created_or_refreshed_at: &'a str,
}

struct CardRiskInputs<'a> {
    entries: &'a [LoadoutEvalEntry],
    existing_weak_against: &'a HashSet<String>,
    existing_demotion_targets: &'a HashSet<String>,
    profile_required_skills: &'a [String],
}

pub fn sum_matrix_samples(rows: &[AgentPerformanceMatrixRow]) -> u32 {
    rows.iter().map(|row| row.samples).sum()
}

pub fn sum_matrix_failures(rows: &[AgentPerformanceMatrixRow]) -> u32 {
    rows.iter().map(|row| row.failure_count).sum()
}

pub fn weighted_matrix_rate<F>(rows: &[AgentPerformanceMatrixRow], value: F) -> Option<f64>
where
    F: Fn(&AgentPerformanceMatrixRow) -> Option<f64>,
{
    let mut numerator = 0.0;
    let mut denominator = 0u32;
    for row in rows {
        let Some(rate) = value(row) else {
            continue;
        };
        numerator += rate * row.samples as f64;
        denominator += row.samples;
    }
    (denominator > 0).then(|| round4(numerator / denominator as f64))
}

pub fn summarize_matrix_rows(rows: &[AgentPerformanceMatrixRow]) -> Value {
    json!({
        "samples": sum_matrix_samples(rows),
        "success_rate": weighted_matrix_rate(rows, |row| row.success_rate),
        "useful_rate": weighted_matrix_rate(rows, |row| row.useful_rate),
        "verification_rate": weighted_matrix_rate(rows, |row| Some(row.verification_rate)),
        "failure_count": sum_matrix_failures(rows),
        "human_override_rate": weighted_matrix_rate(rows, |row| Some(row.human_override_rate)),
        "avg_retry_count": weighted_matrix_rate(rows, |row| Some(row.avg_retry_count)),
        "avg_latency_ms": weighted_matrix_rate(rows, |row| row.avg_latency_ms),
        "avg_cost_usd": weighted_matrix_rate(rows, |row| row.avg_cost_usd),
        "avg_quality_score": weighted_matrix_rate(rows, |row| row.avg_quality_score),
    })
}

pub fn profile_eval_feedback_json(
    profile: &DispatchProfileDef,
    rows_len: usize,
    performance_matrix: &[AgentPerformanceMatrixRow],
    limit: usize,
) -> Value {
    let limit = limit.max(1);
    let profile_rows = performance_matrix
        .iter()
        .filter(|row| row.profile.as_deref() == Some(profile.name))
        .cloned()
        .collect::<Vec<_>>();
    let fallback_rows = performance_matrix
        .iter()
        .filter(|row| {
            row.profile.is_none()
                && (profile_matches_agent(profile, &row.agent)
                    || row
                        .role
                        .as_deref()
                        .is_some_and(|role| profile_role_matches(profile, role)))
        })
        .cloned()
        .collect::<Vec<_>>();

    let profile_samples = sum_matrix_samples(&profile_rows);
    let fallback_role_samples = sum_matrix_samples(&fallback_rows);
    let summary = summarize_matrix_rows(&profile_rows);
    let failure_count = sum_matrix_failures(&profile_rows);
    let human_override_rate =
        weighted_matrix_rate(&profile_rows, |row| Some(row.human_override_rate));
    let avg_retry_count = weighted_matrix_rate(&profile_rows, |row| Some(row.avg_retry_count));
    let verification_rate = weighted_matrix_rate(&profile_rows, |row| Some(row.verification_rate));
    let success_rate = weighted_matrix_rate(&profile_rows, |row| row.success_rate);
    let useful_rate = weighted_matrix_rate(&profile_rows, |row| row.useful_rate);

    let mut guidance = Vec::new();
    if profile_samples == 0 {
        guidance.push(
            "low_sample: no live /eval rows for this profile; keep deterministic baseline loadout"
                .to_string(),
        );
    } else if profile_samples < MIN_LOADOUT_EVOLUTION_SAMPLES {
        guidance.push(format!(
            "low_sample: {} profile samples below evolution threshold {}",
            profile_samples, MIN_LOADOUT_EVOLUTION_SAMPLES
        ));
    } else {
        guidance.push(
            "evidence_available: profile has enough live samples for loadout review".to_string(),
        );
    }
    if failure_count > 0 {
        guidance.push(format!(
            "caution: {} failures present before promoting signature skills",
            failure_count
        ));
    }
    if human_override_rate.unwrap_or(0.0) >= 0.10 {
        guidance.push("review_required: human overrides are elevated for this profile".to_string());
    }
    if avg_retry_count.unwrap_or(0.0) >= 1.0 {
        guidance
            .push("review_required: retry count suggests loadout or prompt friction".to_string());
    }
    if verification_rate.unwrap_or(0.0) < 0.50 && profile_samples > 0 {
        guidance.push("evidence_gap: completions need stronger verification evidence".to_string());
    }
    if profile_samples >= MIN_LOADOUT_EVOLUTION_SAMPLES
        && failure_count == 0
        && human_override_rate.unwrap_or(0.0) < 0.10
        && avg_retry_count.unwrap_or(0.0) < 1.0
        && success_rate.or(useful_rate).unwrap_or(0.0) >= 0.80
    {
        guidance.push(
            "promotion_candidate: stable profile feedback can seed a reviewed loadout proposal"
                .to_string(),
        );
    }

    json!({
        "source": "live_eval",
        "limit": limit,
        "row_count": rows_len,
        "matrix_rows": performance_matrix.len(),
        "profile_samples": profile_samples,
        "fallback_role_samples": fallback_role_samples,
        "min_samples_for_evolution": MIN_LOADOUT_EVOLUTION_SAMPLES,
        "summary": summary,
        "performance_by_task": profile_rows,
        "role_backend_fallback": fallback_rows,
        "guidance": guidance,
    })
}

pub fn build_route_policy_proposals(
    current: &RouteSimulationSummary,
    variants: &[RouteSimulationSummary],
    row_count: usize,
    limit: usize,
    created_or_refreshed_at: &str,
    source_revision: &str,
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
            let legacy_proposal_id = format!(
                "route_policy:{}:{}:{}",
                sanitize_policy_key(&variant.policy),
                sanitize_policy_key(&choice.task_type),
                sanitize_policy_key(&choice.profile)
            );
            let apply_payload = json!({
                "when_task_type": choice.task_type,
                "prefer_profile": choice.profile,
                "policy": variant.policy,
                "fallback_to_current_profile": current_choice.profile,
            });
            let evidence = json!({
                "source": "live_memory_eval",
                "row_count": row_count,
                "limit": limit,
                "current": current_choice,
                "proposed": choice,
                "route_simulate_call": "tachi_tune(action='route_simulate', limit=...)",
            });
            let identity_payload = route_policy_v3_identity_payload(
                &apply_payload,
                &evidence,
                ROUTE_POLICY_PROPOSAL_POLICY_VERSION,
                ROUTE_POLICY_PROPOSAL_TARGET,
                source_revision,
            );
            out.push(json!({
                "proposal_id": legacy_proposal_id,
                "legacy_proposal_id": legacy_proposal_id,
                "kind": ROUTE_POLICY_PROPOSAL_KIND,
                "schema_version": ROUTE_POLICY_PROPOSAL_SCHEMA_VERSION,
                "policy_version": ROUTE_POLICY_PROPOSAL_POLICY_VERSION,
                "target": ROUTE_POLICY_PROPOSAL_TARGET,
                "source_revision": source_revision,
                "identity_payload": identity_payload,
                "status": "pending",
                "requires_human_approval": true,
                "created_or_refreshed_at": created_or_refreshed_at,
                "policy": variant.policy,
                "task_type": choice.task_type,
                "current_profile": current_choice.profile,
                "proposed_profile": choice.profile,
                "current_score": current_choice.score,
                "proposed_score": choice.score,
                "score_delta": round2(choice.score - current_choice.score),
                "policy_rule": apply_payload,
                "evidence": evidence,
                "rationale": format!(
                    "{} replay prefers {} over current {} for {}",
                    variant.policy, choice.profile, current_choice.profile, choice.task_type
                ),
            }));
        }
    }
    out
}

pub fn build_loadout_evolution_proposals<C, P>(
    performance_matrix: &[AgentPerformanceMatrixRow],
    entries: &[LoadoutEvalEntry],
    limit: usize,
    created_or_refreshed_at: &str,
    mut card_inputs: C,
    mut positive_inputs: P,
) -> Result<Vec<Value>, String>
where
    C: FnMut(&DispatchProfileDef) -> Result<ProfileCardRiskInputs, String>,
    P: FnMut(&DispatchProfileDef) -> Result<ProfilePositiveEvolutionInputs, String>,
{
    let mut out = Vec::new();

    for profile in DISPATCH_PROFILES {
        let profile_rows = performance_matrix
            .iter()
            .filter(|row| row.profile.as_deref() == Some(profile.name))
            .cloned()
            .collect::<Vec<_>>();
        let profile_samples = sum_matrix_samples(&profile_rows);
        let card_inputs = card_inputs(profile)?;
        let ctx = ProposalContext {
            limit,
            created_or_refreshed_at,
        };
        if profile_samples < MIN_LOADOUT_EVOLUTION_SAMPLES {
            out.extend(build_card_risk_evolution_proposals(
                profile,
                &profile_rows,
                profile_samples,
                CardRiskInputs {
                    entries,
                    existing_weak_against: &card_inputs.existing_weak_against,
                    existing_demotion_targets: &card_inputs.existing_demotion_targets,
                    profile_required_skills: &card_inputs.profile_required_skills,
                },
                ctx,
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
        out.extend(build_card_risk_evolution_proposals(
            profile,
            &profile_rows,
            profile_samples,
            CardRiskInputs {
                entries,
                existing_weak_against: &card_inputs.existing_weak_against,
                existing_demotion_targets: &card_inputs.existing_demotion_targets,
                profile_required_skills: &card_inputs.profile_required_skills,
            },
            ctx,
        ));

        if failure_count > 0
            || human_override_rate >= 0.10
            || avg_retry_count >= 1.0
            || positive_rate < 0.80
        {
            continue;
        }

        let positive_inputs = positive_inputs(profile)?;
        let existing_skills = positive_inputs
            .profile_required_skills
            .iter()
            .cloned()
            .chain(
                profile
                    .forbidden_skills
                    .iter()
                    .map(|skill| skill.to_string()),
            )
            .collect::<HashSet<_>>();
        let mut buckets: HashMap<String, LoadoutSkillEvidence> = HashMap::new();
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
        out.extend(build_passive_trait_evolution_proposals(
            profile,
            &profile_rows,
            profile_samples,
            &positive_inputs.existing_passive_traits,
            limit,
            min_skill_hits,
            created_or_refreshed_at,
        ));
        out.extend(build_evidence_contract_evolution_proposals(
            profile,
            &profile_rows,
            profile_samples,
            &positive_inputs.existing_evidence_required,
            limit,
            min_skill_hits,
            created_or_refreshed_at,
        ));
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
                "created_or_refreshed_at": created_or_refreshed_at,
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
                    "review_command": "tachi_tune(action='route_review', proposal_id=...)",
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

fn build_evidence_contract_evolution_proposals(
    profile: &DispatchProfileDef,
    profile_rows: &[AgentPerformanceMatrixRow],
    profile_samples: u32,
    existing_evidence_required: &HashSet<String>,
    limit: usize,
    min_task_hits: u32,
    created_or_refreshed_at: &str,
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
            "created_or_refreshed_at": created_or_refreshed_at,
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
                "review_command": "tachi_tune(action='route_review', proposal_id=...)",
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

fn build_passive_trait_evolution_proposals(
    profile: &DispatchProfileDef,
    profile_rows: &[AgentPerformanceMatrixRow],
    profile_samples: u32,
    existing_passive_traits: &HashSet<String>,
    limit: usize,
    min_task_hits: u32,
    created_or_refreshed_at: &str,
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
            "created_or_refreshed_at": created_or_refreshed_at,
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
                "review_command": "tachi_tune(action='route_review', proposal_id=...)",
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

fn build_card_risk_evolution_proposals(
    profile: &DispatchProfileDef,
    profile_rows: &[AgentPerformanceMatrixRow],
    profile_samples: u32,
    inputs: CardRiskInputs<'_>,
    ctx: ProposalContext<'_>,
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
        if !inputs.existing_weak_against.contains(&weakness_id)
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
                "created_or_refreshed_at": ctx.created_or_refreshed_at,
                "profile": profile.name,
                "operation": "add_card_weakness",
                "weakness_id": weakness_id,
                "weakness_label": format!("Repeated friction on {}", row.task_type),
                "current_card": profile_json(profile),
                "proposed_patch": {
                    "add_weak_against": [row.task_type],
                    "preserve_baseline_weak_against": profile.weak_against,
                },
                "evidence": {
                    "source": "live_memory_eval",
                    "limit": ctx.limit,
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
                    "note": "Human approval records the proposal; apply_proposals projects approved weakness markers into the profile card overlay."
                }
            }));
        }
    }

    if bad_task_types.is_empty() {
        return out;
    }

    let current_skills = inputs
        .profile_required_skills
        .iter()
        .collect::<HashSet<_>>();
    let mut skill_hits: HashMap<String, u32> = HashMap::new();
    for entry in inputs.entries {
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
        if hits < min_skill_hits || inputs.existing_demotion_targets.contains(&skill) {
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
            "created_or_refreshed_at": ctx.created_or_refreshed_at,
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
                "limit": ctx.limit,
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
                "note": "Human approval records the proposal; apply_proposals projects approved demotion targets into the profile card overlay without mutating baseline skills."
            }
        }));
    }

    out
}

fn passive_trait_for_task_type(task_type: &str) -> Option<(&'static str, &'static str)> {
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

fn evidence_contract_target_for_task_type(task_type: &str) -> Option<(&'static str, &'static str)> {
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

fn round2(value: f64) -> f64 {
    (value * 100.0).round() / 100.0
}

fn round4(value: f64) -> f64 {
    (value * 10_000.0).round() / 10_000.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profiles::DISPATCH_PROFILES;

    #[test]
    fn profile_feedback_marks_low_sample_profiles() {
        let profile = &DISPATCH_PROFILES[0];
        let feedback = profile_eval_feedback_json(profile, 0, &[], 50);

        assert_eq!(feedback["source"], json!("live_eval"));
        assert_eq!(feedback["profile_samples"], json!(0));
        assert!(feedback["guidance"]
            .as_array()
            .expect("guidance")
            .iter()
            .any(|item| item.as_str().unwrap_or_default().starts_with("low_sample:")));
    }

    #[test]
    fn route_policy_proposals_preserve_evidence_payload() {
        let current = RouteSimulationSummary {
            policy: "current".to_string(),
            selected_route_count: 1,
            sample_count: 4,
            estimated_success_rate: Some(0.5),
            estimated_verification_rate: Some(0.5),
            failure_count: 1,
            avg_retry_count: Some(0.5),
            avg_human_override_rate: Some(0.0),
            avg_latency_ms: Some(10.0),
            avg_cost_usd: Some(0.02),
            total_cost_usd: Some(0.08),
            score: 10.0,
            route_choices: vec![crate::RouteSimulationChoice {
                task_type: "fix_request".to_string(),
                profile: "claude_plan".to_string(),
                agent: "claude".to_string(),
                samples: 4,
                score: 10.0,
                success_rate: Some(0.5),
                verification_rate: 0.5,
                failure_count: 1,
                avg_latency_ms: Some(10.0),
                avg_cost_usd: Some(0.02),
                avg_retry_count: 0.5,
                human_override_rate: 0.0,
                reasons: vec!["current".to_string()],
            }],
            caveats: Vec::new(),
        };
        let variant = RouteSimulationSummary {
            route_choices: vec![crate::RouteSimulationChoice {
                profile: "opencode_builder".to_string(),
                score: 12.5,
                reasons: vec!["cheaper".to_string()],
                ..current.route_choices[0].clone()
            }],
            policy: "cost_sensitive".to_string(),
            score: 12.5,
            ..current.clone()
        };

        let proposals = build_route_policy_proposals(&current, &[variant], 4, 50, "now", "source");

        assert_eq!(proposals.len(), 1);
        assert_eq!(
            proposals[0]["proposal_id"],
            json!("route_policy:cost_sensitive:fix_request:opencode_builder")
        );
        assert_eq!(proposals[0]["created_or_refreshed_at"], json!("now"));
        assert_eq!(proposals[0]["score_delta"], json!(2.5));
    }

    /// v3 schema: every route-policy proposal carries a content-addressed
    /// identity input covering policy_version + target + apply_payload +
    /// evidence_review. The dispatch layer does not hash (no sha2 dep — by
    /// design); it just produces the canonical input the server layer hashes.
    /// This pins the schema so an accidental field drop/rename is caught here.
    #[test]
    fn route_policy_proposals_carry_v3_identity_payload() {
        let current = RouteSimulationSummary {
            policy: "current".to_string(),
            selected_route_count: 1,
            sample_count: 4,
            estimated_success_rate: Some(0.5),
            estimated_verification_rate: Some(0.5),
            failure_count: 1,
            avg_retry_count: Some(0.5),
            avg_human_override_rate: Some(0.0),
            avg_latency_ms: Some(10.0),
            avg_cost_usd: Some(0.02),
            total_cost_usd: Some(0.08),
            score: 10.0,
            route_choices: vec![crate::RouteSimulationChoice {
                task_type: "fix_request".to_string(),
                profile: "claude_plan".to_string(),
                agent: "claude".to_string(),
                samples: 4,
                score: 10.0,
                success_rate: Some(0.5),
                verification_rate: 0.5,
                failure_count: 1,
                avg_latency_ms: Some(10.0),
                avg_cost_usd: Some(0.02),
                avg_retry_count: 0.5,
                human_override_rate: 0.0,
                reasons: vec!["current".to_string()],
            }],
            caveats: Vec::new(),
        };
        let variant = RouteSimulationSummary {
            route_choices: vec![crate::RouteSimulationChoice {
                profile: "opencode_builder".to_string(),
                score: 12.5,
                reasons: vec!["cheaper".to_string()],
                ..current.route_choices[0].clone()
            }],
            policy: "cost_sensitive".to_string(),
            score: 12.5,
            ..current.clone()
        };

        let proposals = build_route_policy_proposals(&current, &[variant], 4, 50, "now", "source");
        assert_eq!(proposals.len(), 1);
        let proposal = &proposals[0];

        // Schema marker present and on the v3 baseline.
        assert_eq!(
            proposal["schema_version"],
            json!(ROUTE_POLICY_PROPOSAL_SCHEMA_VERSION)
        );
        assert_eq!(
            proposal["policy_version"],
            json!(ROUTE_POLICY_PROPOSAL_POLICY_VERSION)
        );
        assert_eq!(proposal["target"], json!(ROUTE_POLICY_PROPOSAL_TARGET));

        // The apply payload is bound to the identity, including the fallback
        // profile — which is the field that flips when the current profile
        // changes (the "regeneration with changed fallback" case).
        let identity = proposal["identity_payload"].clone();
        assert_eq!(
            identity["apply_payload"]["fallback_to_current_profile"],
            json!("claude_plan")
        );
        assert_eq!(
            identity["apply_payload"]["prefer_profile"],
            json!("opencode_builder")
        );
        // Evidence used for review is bound too — changing it must rotate the
        // canonical payload (and therefore the server-side id).
        assert_eq!(identity["evidence_review"]["row_count"], json!(4));
        assert_eq!(identity["evidence_review"]["limit"], json!(50));

        // Volatile fields are NOT in the identity payload by construction.
        assert!(identity.get("status").is_none());
        assert!(identity.get("created_or_refreshed_at").is_none());
        assert!(identity.get("review").is_none());
    }

    /// The identity payload is the discriminating input for the SHA-256 id the
    /// server layer hashes. Changing the fallback profile (i.e. changing what
    /// the proposal would actually persist on apply) MUST change the canonical
    /// payload — otherwise a regenerated proposal could inherit an old approval
    /// across changed content. This pins that property at the unit level; the
    /// integration test (`route_regen_with_changed_fallback_gets_new_pending_id`)
    /// exercises the same property end-to-end through the server's SHA-256 id.
    #[test]
    fn route_policy_identity_payload_rotates_when_fallback_changes() {
        // `RouteSimulationSummary` does not derive `Default`, so the test builds
        // the full struct explicitly and only varies the fallback profile.
        let mk_summary = |fallback_profile: &str| {
            let current_choice = crate::RouteSimulationChoice {
                task_type: "fix_request".to_string(),
                profile: fallback_profile.to_string(),
                agent: "claude".to_string(),
                samples: 4,
                score: 10.0,
                success_rate: Some(0.5),
                verification_rate: 0.5,
                failure_count: 1,
                avg_latency_ms: Some(10.0),
                avg_cost_usd: Some(0.02),
                avg_retry_count: 0.5,
                human_override_rate: 0.0,
                reasons: vec!["current".to_string()],
            };
            let current = RouteSimulationSummary {
                policy: "current".to_string(),
                selected_route_count: 1,
                sample_count: 4,
                estimated_success_rate: Some(0.5),
                estimated_verification_rate: Some(0.5),
                failure_count: 1,
                avg_retry_count: Some(0.5),
                avg_human_override_rate: Some(0.0),
                avg_latency_ms: Some(10.0),
                avg_cost_usd: Some(0.02),
                total_cost_usd: Some(0.08),
                score: 10.0,
                route_choices: vec![current_choice.clone()],
                caveats: Vec::new(),
            };
            let variant_choice = crate::RouteSimulationChoice {
                profile: "opencode_builder".to_string(),
                score: 12.5,
                reasons: vec!["cheaper".to_string()],
                ..current_choice
            };
            let variant = RouteSimulationSummary {
                policy: "cost_sensitive".to_string(),
                score: 12.5,
                route_choices: vec![variant_choice],
                ..current.clone()
            };
            (current, variant)
        };
        let mk_identity = |fallback_profile: &str| {
            let (current, variant) = mk_summary(fallback_profile);
            let proposals =
                build_route_policy_proposals(&current, &[variant], 4, 50, "now", "source");
            serde_json::to_string(&proposals[0]["identity_payload"]).unwrap()
        };

        let base = mk_identity("claude_plan");
        let flipped = mk_identity("glm_impl");
        assert_ne!(
            base, flipped,
            "identity payload must change when fallback_to_current_profile changes"
        );
    }

    /// Changing the reviewed evidence (row_count / limit) MUST rotate the
    /// canonical payload — a proposal the human reviewed against evidence A
    /// cannot inherit its approval when regenerated against evidence B even if
    /// the apply payload is otherwise identical.
    #[test]
    fn route_policy_identity_payload_rotates_when_evidence_changes() {
        let mk_summary = || {
            let current_choice = crate::RouteSimulationChoice {
                task_type: "fix_request".to_string(),
                profile: "claude_plan".to_string(),
                agent: "claude".to_string(),
                samples: 4,
                score: 10.0,
                success_rate: Some(0.5),
                verification_rate: 0.5,
                failure_count: 1,
                avg_latency_ms: Some(10.0),
                avg_cost_usd: Some(0.02),
                avg_retry_count: 0.5,
                human_override_rate: 0.0,
                reasons: vec!["current".to_string()],
            };
            let current = RouteSimulationSummary {
                policy: "current".to_string(),
                selected_route_count: 1,
                sample_count: 4,
                estimated_success_rate: Some(0.5),
                estimated_verification_rate: Some(0.5),
                failure_count: 1,
                avg_retry_count: Some(0.5),
                avg_human_override_rate: Some(0.0),
                avg_latency_ms: Some(10.0),
                avg_cost_usd: Some(0.02),
                total_cost_usd: Some(0.08),
                score: 10.0,
                route_choices: vec![current_choice.clone()],
                caveats: Vec::new(),
            };
            let variant_choice = crate::RouteSimulationChoice {
                profile: "opencode_builder".to_string(),
                score: 12.5,
                reasons: vec!["cheaper".to_string()],
                ..current_choice
            };
            let variant = RouteSimulationSummary {
                policy: "cost_sensitive".to_string(),
                score: 12.5,
                route_choices: vec![variant_choice],
                ..current.clone()
            };
            (current, variant)
        };
        let mk_identity = |row_count: usize, limit: usize| {
            let (current, variant) = mk_summary();
            let proposals = build_route_policy_proposals(
                &current,
                &[variant],
                row_count,
                limit,
                "now",
                "source",
            );
            serde_json::to_string(&proposals[0]["identity_payload"]).unwrap()
        };

        let a = mk_identity(4, 50);
        let b = mk_identity(8, 50);
        let c = mk_identity(4, 100);
        assert_ne!(a, b, "identity must change when row_count changes");
        assert_ne!(a, c, "identity must change when limit changes");
    }

    #[test]
    fn loadout_evolution_does_not_read_positive_inputs_for_low_sample_profiles() {
        let rows = vec![AgentPerformanceMatrixRow {
            profile: Some("claude_plan".to_string()),
            task_type: "plan_request".to_string(),
            samples: 1,
            verification_rate: 1.0,
            ..AgentPerformanceMatrixRow::default()
        }];

        let proposals = build_loadout_evolution_proposals(
            &rows,
            &[],
            50,
            "now",
            |_profile| Ok(ProfileCardRiskInputs::default()),
            |_profile| Err("positive inputs should stay lazy".to_string()),
        )
        .expect("low-sample profiles only need card-risk inputs");

        assert!(proposals.is_empty());
    }
}
