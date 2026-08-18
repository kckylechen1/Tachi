use serde::Serialize;
use serde_json::{json, Value};

use crate::eval::{AgentPerformanceMatrixRow, CompletionStatus, EvalRow, SubagentTaskScore};
use crate::{
    fallback_chain, profile_matches_agent, profile_resolved_model, recommendation_identity_receipt,
    resolve_dispatch_profile, DispatchProfileDef, DISPATCH_PROFILES, MIN_ROUTE_POLICY_RULE_SAMPLES,
    ROUTE_POLICY_RULE_NS, ROUTE_POLICY_RULE_SCORE_BONUS,
};

#[derive(Debug, Clone, Serialize)]
pub struct DispatchRisk {
    pub task_type: String,
    pub risk: String,
    pub reasons: Vec<String>,
    pub required_profiles: Vec<String>,
    pub blocked_profiles: Vec<String>,
}

/// The `/eval/YYYY-MM-DD` memory entries the recommendation scorer read until
/// tachi#1675 PR4. Retired as a ROUTING evidence base by that cutover; the
/// entries themselves remain readable human notes (`tachi_agent_eval`'s
/// `aggregate_live`/`telemetry` still serve them).
pub const ROUTE_EVIDENCE_SOURCE_LIVE_EVAL_MEMORY: &str = "live_eval_memory";

/// The tachi#1675 decision-fact ledger (`route_recommendations` /
/// `route_decisions` / `eval_rubric_scores` joined onto the canonical outcome
/// and adjudication spines). The evidence base `recommend` sources from after
/// PR4's flip, and the one `tachi_agent_eval(action='route_projection')`
/// already declares.
pub const ROUTE_EVIDENCE_SOURCE_DECISION_FACT_LEDGER: &str = "decision_fact_ledger";

/// Prefix of the skip reason for a route-policy rule whose EVIDENCE comes from
/// an evidence base tachi#1675 PR4 retired as a routing input.
///
/// The rule row records the base its proposal was mined from (`evidence.source`
/// — `build_route_policy_proposals` writes `live_memory_eval` there). After the
/// flip, only a rule that declares the decision-fact ledger may move a routing
/// score; every other declaration — and an ABSENT one — is refused. Allowlist,
/// not blocklist: an undeclared rule is exactly the case a blocklist would let
/// through, and the retired base must not re-enter routing through a row whose
/// provenance nobody wrote down.
pub const RETIRED_EVIDENCE_SOURCE_SKIP_REASON: &str = "retired_evidence_source";

/// The zero-signal fallback reason of the legacy `/eval`-memory scorer. Kept
/// ONLY for that path: the #1202 owner ruling
/// (`docs/engineering/architecture/dispatch-lifecycle.md` §4.2) demoted MBIT to
/// derived evidence, so a routing answer whose only stated ground is "the MBIT
/// card fits" is exactly what design D7 forbids on the ledger path.
pub const BASELINE_MBIT_FIT_REASON: &str = "baseline_mbit_fit";

/// A candidate the ledger has no usable row about. Deliberately NOT a fit
/// claim: it names the absence, and the projection's answer for the response
/// as a whole is `abstain` (design D7).
pub const NO_LEDGER_EVIDENCE_REASON: &str = "no_ledger_evidence";

/// Which evidence base fed a scoring run (tachi#1675 PR4, design D6 phase 2).
///
/// Threaded rather than inferred: the evidence flip must be declared in the
/// response and must change the zero-signal fallback reason, and both of those
/// are decisions a caller makes, not something the scorer can guess from the
/// rows it was handed (an empty row slice looks the same either way).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouteEvidenceSource {
    /// Pre-PR4: `/eval/YYYY-MM-DD` memory entries.
    LiveEvalMemory,
    /// Post-PR4: the decision-fact ledger.
    DecisionFactLedger,
}

impl RouteEvidenceSource {
    pub const fn as_str(self) -> &'static str {
        match self {
            RouteEvidenceSource::LiveEvalMemory => ROUTE_EVIDENCE_SOURCE_LIVE_EVAL_MEMORY,
            RouteEvidenceSource::DecisionFactLedger => ROUTE_EVIDENCE_SOURCE_DECISION_FACT_LEDGER,
        }
    }

    /// The reason recorded for a candidate that accumulated NO scoring signal
    /// at all.
    ///
    /// This is the whole of design D7's in-scope half: on the ledger path the
    /// no-evidence branch names the absence (`no_ledger_evidence`) and the
    /// decision abstains; `baseline_mbit_fit` survives only on the legacy
    /// `/eval`-memory path.
    pub const fn no_signal_reason(self) -> &'static str {
        match self {
            RouteEvidenceSource::LiveEvalMemory => BASELINE_MBIT_FIT_REASON,
            RouteEvidenceSource::DecisionFactLedger => NO_LEDGER_EVIDENCE_REASON,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ProfileCandidate {
    pub profile: String,
    pub agent: String,
    pub role: String,
    pub model: Option<String>,
    pub score: f64,
    pub reasons: Vec<String>,
    pub live_samples: u32,
    pub useful_rate: Option<f64>,
    pub failure_count: u32,
    pub performance_samples: u32,
    pub human_override_rate: Option<f64>,
    pub avg_retry_count: Option<f64>,
    pub avg_latency_ms: Option<f64>,
    pub avg_cost_usd: Option<f64>,
}

#[derive(Debug, Clone, Default)]
pub struct RouteEvalRow {
    pub profile: Option<String>,
    pub completed: bool,
    pub verification_present: bool,
}

#[derive(Debug, Clone, Default)]
pub struct RouteSubagentScore {
    pub role: String,
    pub agent: String,
    pub task_type: String,
    pub samples: u32,
    pub useful_rate: f64,
    pub failure_count: u32,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct RoutePerformanceRow {
    pub scope: String,
    pub profile: Option<String>,
    pub role: Option<String>,
    pub agent: String,
    pub task_type: String,
    pub samples: u32,
    pub success_rate: Option<f64>,
    pub useful_rate: Option<f64>,
    pub verification_rate: f64,
    pub failure_count: u32,
    pub human_override_rate: f64,
    pub avg_retry_count: f64,
    pub avg_latency_ms: Option<f64>,
    pub avg_cost_usd: Option<f64>,
    pub avg_quality_score: Option<f64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AppliedRoutePolicyRule {
    pub proposal_id: String,
    pub policy: String,
    pub task_type: String,
    pub prefer_profile: String,
    pub sample_count: u32,
    pub score_delta: Option<f64>,
    pub status: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct SkippedRoutePolicyRule {
    pub proposal_id: String,
    pub reason: String,
    pub policy: Option<String>,
    pub task_type: Option<String>,
    pub prefer_profile: Option<String>,
    pub sample_count: Option<u32>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RoutePolicyRuleLoadout {
    pub namespace: &'static str,
    pub min_samples: u32,
    pub applied: Vec<AppliedRoutePolicyRule>,
    pub skipped: Vec<SkippedRoutePolicyRule>,
}

#[derive(Debug, Clone)]
pub struct RoutePolicyRuleRecord {
    pub proposal_id: String,
    pub value_json: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct RouteSimulationSummary {
    pub policy: String,
    pub selected_route_count: u32,
    pub sample_count: u32,
    pub estimated_success_rate: Option<f64>,
    pub estimated_verification_rate: Option<f64>,
    pub failure_count: u32,
    pub avg_retry_count: Option<f64>,
    pub avg_human_override_rate: Option<f64>,
    pub avg_latency_ms: Option<f64>,
    pub avg_cost_usd: Option<f64>,
    pub total_cost_usd: Option<f64>,
    pub score: f64,
    pub route_choices: Vec<RouteSimulationChoice>,
    pub caveats: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RouteSimulationChoice {
    pub task_type: String,
    pub profile: String,
    pub agent: String,
    pub samples: u32,
    pub score: f64,
    pub success_rate: Option<f64>,
    pub verification_rate: f64,
    pub failure_count: u32,
    pub avg_latency_ms: Option<f64>,
    pub avg_cost_usd: Option<f64>,
    pub avg_retry_count: f64,
    pub human_override_rate: f64,
    pub reasons: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct RecommendationProfilePayload {
    pub recommended_transport: String,
    pub transport_readiness: Value,
    pub evidence_required: Value,
    pub evidence_contract: Value,
    pub resolved_skills: Vec<String>,
    pub resolved_skill_loadout: Value,
    pub profile_card: Value,
}

pub fn route_eval_rows(rows: &[EvalRow]) -> Vec<RouteEvalRow> {
    rows.iter()
        .map(|row| RouteEvalRow {
            profile: canonical_profile_for_row(row.profile.as_deref()),
            completed: row.completion_status == CompletionStatus::Completed,
            verification_present: row.verification_present,
        })
        .collect()
}

pub fn route_subagent_scores(rows: &[SubagentTaskScore]) -> Vec<RouteSubagentScore> {
    rows.iter()
        .map(|row| RouteSubagentScore {
            role: row.role.clone(),
            agent: row.agent.clone(),
            task_type: row.task_type.clone(),
            samples: row.samples,
            useful_rate: row.useful_rate,
            failure_count: row.failure_count,
        })
        .collect()
}

pub fn route_performance_rows(rows: &[AgentPerformanceMatrixRow]) -> Vec<RoutePerformanceRow> {
    rows.iter()
        .map(|row| RoutePerformanceRow {
            scope: row.scope.clone(),
            profile: canonical_profile_for_row(row.profile.as_deref()),
            role: row.role.clone(),
            agent: row.agent.clone(),
            task_type: row.task_type.clone(),
            samples: row.samples,
            success_rate: row.success_rate,
            useful_rate: row.useful_rate,
            verification_rate: row.verification_rate,
            failure_count: row.failure_count,
            human_override_rate: row.human_override_rate,
            avg_retry_count: row.avg_retry_count,
            avg_latency_ms: row.avg_latency_ms,
            avg_cost_usd: row.avg_cost_usd,
            avg_quality_score: row.avg_quality_score,
        })
        .collect()
}

fn canonical_profile_for_row(profile: Option<&str>) -> Option<String> {
    profile.map(|raw| {
        resolve_dispatch_profile(raw)
            .map(|profile| profile.name.to_string())
            .unwrap_or_else(|| raw.to_string())
    })
}

pub fn classify_dispatch_risk(
    task: &str,
    task_type: &str,
    risk_override: Option<&str>,
    file_paths: &[String],
) -> DispatchRisk {
    let lower = task.to_ascii_lowercase();
    let lower_paths = file_paths
        .iter()
        .map(|path| path.to_ascii_lowercase())
        .collect::<Vec<_>>();
    let mut reasons = Vec::new();
    let mut risk = "medium".to_string();

    let mut push_reason = |reason: &str| {
        if !reasons.iter().any(|existing| existing == reason) {
            reasons.push(reason.to_string());
        }
    };

    for (needle, reason) in dispatch_risk_needles() {
        if lower.contains(needle) {
            push_reason(reason);
        }
    }
    for lower_path in &lower_paths {
        for (needle, reason) in dispatch_risk_needles() {
            if lower_path.contains(needle) {
                push_reason(reason);
            }
        }
    }
    if matches!(
        task_type,
        "migration_request" | "refactor_request" | "review_request"
    ) {
        push_reason(&format!("task_type={task_type}"));
    }
    let missing_verification = indicates_missing_verification(&lower);
    if missing_verification {
        push_reason("missing_verification_signal");
    }
    if indicates_prior_failure(&lower) {
        push_reason("prior_failure_or_regression_hint");
    }
    if !reasons.is_empty()
        && (contains_high_risk_surface(&lower)
            || lower_paths
                .iter()
                .any(|path| contains_high_risk_surface(path))
            || missing_verification
            || reasons
                .iter()
                .any(|reason| reason == "prior_failure_or_regression_hint"))
    {
        risk = "high".to_string();
    } else if matches!(task_type, "explain_request" | "research_request") {
        risk = "low".to_string();
    }
    if let Some(override_risk) = risk_override.filter(|s| !s.trim().is_empty()) {
        risk = override_risk.trim().to_ascii_lowercase();
        reasons.push(format!("user_override={risk}"));
    }
    if reasons.is_empty() {
        reasons.push("default deterministic route classification".to_string());
    }

    let required_profiles = match risk.as_str() {
        "high" | "critical" => vec!["claude_plan".to_string(), "codex_55_review".to_string()],
        "low" if task_type == "review_request" => vec!["codex_53_fast".to_string()],
        _ if task_type == "plan_request" => vec!["claude_plan".to_string()],
        _ if task_type == "review_request" => vec!["codex_55_review".to_string()],
        _ => Vec::new(),
    };
    let blocked_profiles = if matches!(risk.as_str(), "high" | "critical") {
        vec!["codex_53_fast".to_string()]
    } else {
        Vec::new()
    };

    DispatchRisk {
        task_type: task_type.to_string(),
        risk,
        reasons,
        required_profiles,
        blocked_profiles,
    }
}

pub fn recommend_dispatch_profile_candidates<W>(
    risk: &DispatchRisk,
    rows: &[RouteEvalRow],
    subagent_scores: &[RouteSubagentScore],
    performance_matrix: &[RoutePerformanceRow],
    route_policy_rules: &RoutePolicyRuleLoadout,
    evidence_source: RouteEvidenceSource,
    mut weak_against_for_profile: W,
) -> Result<Vec<ProfileCandidate>, String>
where
    W: FnMut(&DispatchProfileDef) -> Result<Vec<String>, String>,
{
    let mut candidates = DISPATCH_PROFILES
        .iter()
        .map(|profile| {
            let weak_against = weak_against_for_profile(profile)?;
            Ok(score_profile_candidate(
                profile,
                risk,
                rows,
                subagent_scores,
                performance_matrix,
                &weak_against,
                evidence_source,
            ))
        })
        .collect::<Result<Vec<_>, String>>()?;
    apply_route_policy_rules_to_candidates(&mut candidates, route_policy_rules, risk);
    candidates
        .sort_by(|a, b| compare_scores_desc(a.score, b.score).then(a.profile.cmp(&b.profile)));
    Ok(candidates)
}

fn score_profile_candidate(
    profile: &DispatchProfileDef,
    risk: &DispatchRisk,
    rows: &[RouteEvalRow],
    subagent_scores: &[RouteSubagentScore],
    performance_matrix: &[RoutePerformanceRow],
    weak_against: &[String],
    evidence_source: RouteEvidenceSource,
) -> ProfileCandidate {
    let mut score = 0.0;
    let mut reasons = Vec::new();

    if risk.required_profiles.iter().any(|p| p == profile.name) {
        score += 45.0;
        reasons.push("required_by_risk_classifier".to_string());
    }
    if risk.blocked_profiles.iter().any(|p| p == profile.name) {
        score -= 40.0;
        reasons.push("blocked_or_deprioritized_by_risk_classifier".to_string());
    }

    match risk.task_type.as_str() {
        "review_request" if profile.role.contains("review") => {
            score += 30.0;
            reasons.push("role_matches_review_request".to_string());
        }
        "plan_request" if matches!(profile.role, "planner" | "architect") => {
            score += 30.0;
            reasons.push("role_matches_plan_request".to_string());
        }
        "fix_request" | "refactor_request" | "migration_request" if profile.role == "executor" => {
            score += 25.0;
            reasons.push("role_matches_execution_request".to_string());
        }
        "test_request" if profile.role.contains("checker") || profile.role.contains("reviewer") => {
            score += 20.0;
            reasons.push("role_matches_verification_request".to_string());
        }
        "research_request" | "explain_request"
            if matches!(
                profile.role,
                "explore" | "planner" | "architect" | "ux_researcher"
            ) =>
        {
            score += 30.0;
            reasons.push("role_matches_read_only_request".to_string());
        }
        "research_request" | "explain_request" if profile.role == "executor" => {
            score -= 20.0;
            reasons.push("executor_deprioritized_for_read_only_request".to_string());
        }
        _ => {}
    }

    for signal in &risk.reasons {
        if profile
            .strong_against
            .iter()
            .any(|s| signal.contains(s) || s.contains("dispatch") && signal.contains("dispatch"))
        {
            score += 15.0;
            reasons.push(format!("strong_against_signal:{signal}"));
        }
    }
    if profile.role == "ux_researcher"
        && risk.reasons.iter().any(|reason| {
            reason.contains("agent_experience")
                || reason.contains("workflow_ux")
                || reason.contains("tool_surface_friction")
        })
    {
        score += 30.0;
        reasons.push("role_matches_agent_facing_ux".to_string());
    }
    for weakness in weak_against {
        if weakness == &risk.task_type
            || risk
                .reasons
                .iter()
                .any(|signal| signal.contains(weakness) || weakness.contains(signal))
        {
            score -= 12.0;
            reasons.push(format!("weak_against_signal:{weakness}"));
        }
    }
    if matches!(risk.risk.as_str(), "high" | "critical") && profile.role == "fast_checker" {
        score -= 20.0;
        reasons.push("fast_checker_deprioritized_for_high_risk".to_string());
    }

    let mut live_samples = 0u32;
    let mut useful_sum = 0.0;
    let mut useful_count = 0u32;
    let mut failure_count = 0u32;
    let mut performance_samples = 0u32;
    let mut human_override_sum = 0.0;
    let mut retry_sum = 0.0;
    let mut latency_sum = 0.0;
    let mut latency_count = 0u32;
    let mut cost_sum = 0.0;
    let mut cost_count = 0u32;
    let has_perf_leader_row = performance_matrix.iter().any(|perf| {
        perf.scope == "leader"
            && perf.profile.as_deref() == Some(profile.name)
            && perf.task_type == risk.task_type
    });
    let mut raw_failure_count = 0u32;
    for row in rows {
        if row.profile.as_deref() == Some(profile.name) {
            live_samples += 1;
            if row.completed && row.verification_present {
                score += 12.0;
                useful_sum += 1.0;
            } else if row.completed {
                score += 5.0;
                useful_sum += 0.6;
            } else {
                raw_failure_count += 1;
                failure_count += 1;
            }
            useful_count += 1;
        }
    }
    if !has_perf_leader_row && raw_failure_count > 0 {
        let weight = (raw_failure_count as f64).min(20.0) / 20.0;
        let raw_failure_penalty = (raw_failure_count as f64 * 4.0).min(16.0) * weight;
        score -= raw_failure_penalty;
        reasons.push(format!("live_eval_failures={raw_failure_count}"));
    }
    for sub in subagent_scores {
        let role_match = profile_role_matches(profile, &sub.role);
        let agent_match = profile_matches_agent(profile, &sub.agent);
        let task_match = sub.task_type == risk.task_type;
        if role_match && agent_match && task_match {
            live_samples += sub.samples;
            useful_sum += sub.useful_rate * sub.samples as f64;
            useful_count += sub.samples;
            failure_count += sub.failure_count;
            score += sub.useful_rate * 20.0;
            score -= sub.failure_count as f64 * 5.0;
            reasons.push(format!(
                "live_subagent_evidence:{}:{} samples={} useful_rate={:.2}",
                sub.agent, sub.task_type, sub.samples, sub.useful_rate
            ));
        }
    }

    for perf in performance_matrix {
        let profile_match = perf.profile.as_deref() == Some(profile.name);
        let task_match = perf.task_type == risk.task_type;
        if !profile_match || !task_match {
            continue;
        }
        let role_match = perf.scope == "leader"
            || perf
                .role
                .as_deref()
                .is_some_and(|role| profile_role_matches(profile, role));
        if !role_match {
            continue;
        }

        performance_samples += perf.samples;
        let weight = (perf.samples as f64).min(20.0) / 20.0;

        if perf.failure_count > 0 {
            failure_count += perf.failure_count;
            let penalty = (perf.failure_count as f64 * 4.0).min(16.0) * weight;
            score -= penalty;
            reasons.push(format!(
                "perf_failure_count:{}:{} failures={}",
                perf.scope, perf.task_type, perf.failure_count
            ));
        }
        if perf.human_override_rate > 0.0 {
            human_override_sum += perf.human_override_rate * perf.samples as f64;
            let penalty = perf.human_override_rate * 18.0 * weight;
            score -= penalty;
            reasons.push(format!(
                "perf_human_override_rate={:.2}",
                perf.human_override_rate
            ));
        }
        if perf.avg_retry_count > 0.0 {
            retry_sum += perf.avg_retry_count * perf.samples as f64;
            let penalty = (perf.avg_retry_count * 3.0).min(15.0) * weight;
            score -= penalty;
            reasons.push(format!("perf_avg_retry_count={:.2}", perf.avg_retry_count));
        }
        if let Some(latency) = perf.avg_latency_ms {
            latency_sum += latency * perf.samples as f64;
            latency_count += perf.samples;
            if latency > 600_000.0 {
                score -= 4.0 * weight;
                reasons.push(format!("perf_slow_avg_latency_ms={latency:.0}"));
            }
        }
        if let Some(cost) = perf.avg_cost_usd {
            cost_sum += cost * perf.samples as f64;
            cost_count += perf.samples;
            if cost > 1.0 {
                score -= 4.0 * weight;
                reasons.push(format!("perf_high_avg_cost_usd={cost:.4}"));
            } else if perf.avg_quality_score.unwrap_or(0.0) >= 0.8 && cost <= 0.10 {
                score += 3.0 * weight;
                reasons.push(format!("perf_cost_efficient_usd={cost:.4}"));
            }
        }
    }

    let useful_rate = (useful_count > 0).then(|| useful_sum / useful_count as f64);
    if let Some(rate) = useful_rate {
        reasons.push(format!("live_useful_rate={rate:.2}"));
    }
    // tachi#1675 PR4 (design D7): the no-signal fallback REASON belongs to the
    // evidence base, not to the scorer. On the ledger path it must name the
    // absence of usable rows — never a baseline MBIT fit, which the #1202
    // ruling demoted to derived evidence and which the projection answers with
    // `abstain` instead.
    if reasons.is_empty() {
        reasons.push(evidence_source.no_signal_reason().to_string());
    }

    ProfileCandidate {
        profile: profile.name.to_string(),
        agent: profile.backend.to_string(),
        role: profile.role.to_string(),
        model: profile_resolved_model(profile),
        score: round2(score),
        reasons,
        live_samples,
        useful_rate,
        failure_count,
        performance_samples,
        human_override_rate: (performance_samples > 0)
            .then(|| human_override_sum / performance_samples as f64),
        avg_retry_count: (performance_samples > 0).then(|| retry_sum / performance_samples as f64),
        avg_latency_ms: (latency_count > 0).then(|| latency_sum / latency_count as f64),
        avg_cost_usd: (cost_count > 0).then(|| cost_sum / cost_count as f64),
    }
}

/// The fallback chain for a recommendation: who to try if the primary is
/// unavailable.
///
/// Every entry comes from `candidates`. tachi#1675 PR4 (BUG-2): the
/// agent-affinity fill used to scan all of `DISPATCH_PROFILES`, so at high risk
/// it could name a profile the risk classifier had blocked or excluded — an
/// actionable output naming an inadmissible profile is the same hard-gate leak
/// as naming one in `recommended_profile`, one field over. The caller hands in
/// the gated candidate set; this function never widens it.
pub fn build_profile_fallback_chain(
    primary: &DispatchProfileDef,
    candidates: &[ProfileCandidate],
) -> Vec<String> {
    let mut out = vec![primary.name.to_string()];
    for candidate in candidates.iter().skip(1) {
        if out.len() >= 4 {
            break;
        }
        if !out.iter().any(|p| p == &candidate.profile) {
            out.push(candidate.profile.clone());
        }
    }
    for agent in fallback_chain(primary.backend) {
        if out.len() >= 5 {
            break;
        }
        if let Some(profile) = DISPATCH_PROFILES.iter().find(|profile| {
            profile_matches_agent(profile, agent)
                && candidates
                    .iter()
                    .any(|candidate| candidate.profile == profile.name)
                && !out.iter().any(|p| p == profile.name)
        }) {
            out.push(profile.name.to_string());
        }
    }
    out
}

/// Build the `recommend` response.
///
/// `evidence_source` is threaded (tachi#1675 PR4) rather than assumed: it is
/// declared verbatim in the payload as `evidence_source`, and it decides the
/// wording of `evidence_note` — a note that says "used matching /eval
/// evidence" after the ledger cutover would be the response lying about where
/// its evidence came from.
#[allow(clippy::too_many_arguments)]
pub fn build_dispatch_recommendation_response(
    task: &str,
    risk: &DispatchRisk,
    best_profile: &DispatchProfileDef,
    candidates: &[ProfileCandidate],
    route_policy_rules: &RoutePolicyRuleLoadout,
    row_count: usize,
    evidence_source: RouteEvidenceSource,
    profile_payload: RecommendationProfilePayload,
) -> Result<Value, String> {
    let best = candidates
        .first()
        .ok_or_else(|| "no dispatch profiles configured".to_string())?;
    let fallback = build_profile_fallback_chain(best_profile, candidates);
    let live_matched_samples = candidates
        .iter()
        .map(|candidate| candidate.live_samples)
        .sum::<u32>();
    let performance_matrix_hits = candidates
        .iter()
        .map(|candidate| candidate.performance_samples)
        .sum::<u32>();
    let evidence_note = match (evidence_source, route_policy_rules.applied.is_empty(), live_matched_samples) {
        (RouteEvidenceSource::LiveEvalMemory, false, _) => {
            "route_policy_weighted: recommendation used matching /eval evidence plus approved route-policy rules."
        }
        (RouteEvidenceSource::LiveEvalMemory, true, 0) => {
            "low_sample_fallback: no matching live /eval profile/subagent evidence; deterministic MBIT/risk fit dominated."
        }
        (RouteEvidenceSource::LiveEvalMemory, true, _) => {
            "live_eval_weighted: recommendation used matching /eval profile/subagent evidence."
        }
        (RouteEvidenceSource::DecisionFactLedger, false, _) => {
            "route_policy_weighted: recommendation used usable decision-fact-ledger rows plus approved route-policy rules."
        }
        (RouteEvidenceSource::DecisionFactLedger, true, 0) => {
            "no_ledger_evidence: no usable decision-fact-ledger row in window; `decision` abstains and the reported profile is deterministic admission/role fit only."
        }
        (RouteEvidenceSource::DecisionFactLedger, true, _) => {
            "ledger_weighted: recommendation used usable decision-fact-ledger rows (terminal-reconciled, rubric-scored, independently adjudicated)."
        }
    };

    Ok(json!({
        "task": task,
        "task_type": &risk.task_type,
        "risk": &risk.risk,
        "risk_reasons": &risk.reasons,
        "required_profiles": &risk.required_profiles,
        "blocked_profiles": &risk.blocked_profiles,
        "recommended_profile": &best.profile,
        "recommended_agent": &best.agent,
        "recommended_model": profile_resolved_model(best_profile),
        "identity_receipt": recommendation_identity_receipt(best_profile),
        "recommended_transport": profile_payload.recommended_transport,
        "transport_readiness": profile_payload.transport_readiness,
        "role": &best.role,
        "tool_profile": best_profile.tool_profile,
        "evidence_required": profile_payload.evidence_required,
        "evidence_contract": profile_payload.evidence_contract,
        "resolved_skills": profile_payload.resolved_skills,
        "resolved_skill_loadout": profile_payload.resolved_skill_loadout,
        "fallback_chain": fallback,
        "reason": &best.reasons,
        "route_explanation": &best.reasons,
        "evidence_note": evidence_note,
        // tachi#1675 PR4: which evidence base answered. Declared on EVERY
        // response, the same vocabulary
        // `tachi_agent_eval(action='route_projection')` publishes, so a
        // consumer never has to infer the cutover from the note prose.
        "evidence_source": evidence_source.as_str(),
        // Kept under its pre-cutover key for consumer compatibility. After the
        // flip these are ledger counts: `row_count` is the ledger rows read in
        // window and `matched_samples` the usable rows attributed to a
        // candidate. `performance_matrix_hits` is structurally 0 on the ledger
        // path — the ledger carries no performance-matrix aggregate.
        "live_eval": {
            "row_count": row_count,
            "matched_samples": live_matched_samples,
            "performance_matrix_hits": performance_matrix_hits,
        },
        "route_policy_rules": route_policy_rules,
        "profile_card": profile_payload.profile_card,
        "candidates": candidates,
    }))
}

pub fn build_route_policy_rule_loadout(
    records: &[RoutePolicyRuleRecord],
    risk: &DispatchRisk,
) -> RoutePolicyRuleLoadout {
    let mut applied = Vec::new();
    let mut skipped = Vec::new();

    for row in records {
        let proposal_id = row.proposal_id.clone();
        let value: serde_json::Value = match serde_json::from_str(&row.value_json) {
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
            .and_then(serde_json::Value::as_str)
            .unwrap_or("unknown");
        let review_status = value
            .get("review")
            .and_then(|review| review.get("status"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or("unknown");
        let policy_rule = value.get("policy_rule").unwrap_or(&serde_json::Value::Null);
        let policy = policy_rule
            .get("policy")
            .and_then(serde_json::Value::as_str)
            .or_else(|| value.get("policy").and_then(serde_json::Value::as_str))
            .map(str::to_string);
        let task_type = policy_rule
            .get("when_task_type")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string);
        let prefer_profile = policy_rule
            .get("prefer_profile")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string);
        let sample_count = route_policy_rule_sample_count(&value);
        let score_delta = value.get("score_delta").and_then(serde_json::Value::as_f64);
        // Which evidence base this rule was mined from, as the rule row itself
        // declares it (`build_route_policy_proposals` stamps
        // `evidence.source`). `None` means the row declares nothing, which the
        // gate below treats exactly like a retired declaration.
        let evidence_source = value
            .get("evidence")
            .and_then(|evidence| evidence.get("evidence_source"))
            .or_else(|| value.get("evidence").and_then(|e| e.get("source")))
            .or_else(|| value.get("evidence_source"))
            .and_then(serde_json::Value::as_str);

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
            .is_none_or(|profile| crate::resolve_dispatch_profile(profile).is_none())
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
        } else if !risk.required_profiles.is_empty()
            && prefer_profile.as_deref().is_some_and(|profile| {
                !risk
                    .required_profiles
                    .iter()
                    .any(|required| required == profile)
            })
        {
            // tachi#1675 PR4, BUG-2 (codex review finding 2): a `required`
            // classification RESTRICTS the admissible set — at high/critical
            // risk the classifier is naming who may run at all, not who gets a
            // bonus. Skipping only `blocked` profiles here let a rule spend its
            // +35 on a profile the hard gate had already removed, which is how
            // an excluded profile could out-score a required one. Same reason
            // string the projection's gate uses
            // (`agent_eval::projection::REASON_NOT_REQUIRED`), so both halves
            // of the gate speak one vocabulary.
            Some(format!(
                "not_required_for_risk_class:{}",
                prefer_profile.as_deref().unwrap_or("missing")
            ))
        } else if evidence_source != Some(ROUTE_EVIDENCE_SOURCE_DECISION_FACT_LEDGER) {
            // tachi#1675 PR4, BUG-1: the LAST surviving `/eval` -> routing
            // policy path. `tachi_tune(action='route_proposals')` still mines
            // `/eval` memory, and an approved proposal still lands in
            // `ROUTE_POLICY_RULE_NS` via `route_apply` — so without this gate
            // the retired evidence base kept steering the flipped recommend
            // surface through a +35 score bonus, one human approval removed.
            // The refusal is here, at the moment policy enters the DECISION,
            // rather than at proposal mint/apply: mint and apply keep their
            // reviewable human-notes value (and their identity/CAS/drift
            // discipline), while nothing they produce can score a route again
            // until it is mined from the ledger.
            //
            // Evaluated last so a rule that also fails a structural filter
            // still reports the more specific reason; a rule that clears every
            // structural filter cannot apply on retired evidence.
            Some(format!(
                "{RETIRED_EVIDENCE_SOURCE_SKIP_REASON}:{}",
                evidence_source.unwrap_or("undeclared")
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

    RoutePolicyRuleLoadout {
        namespace: ROUTE_POLICY_RULE_NS,
        min_samples: MIN_ROUTE_POLICY_RULE_SAMPLES,
        applied,
        skipped,
    }
}

fn route_policy_rule_sample_count(value: &serde_json::Value) -> u32 {
    value
        .get("evidence")
        .and_then(|evidence| evidence.get("proposed"))
        .and_then(|proposed| proposed.get("samples"))
        .and_then(serde_json::Value::as_u64)
        .or_else(|| {
            value
                .get("evidence")
                .and_then(|evidence| evidence.get("row_count"))
                .and_then(serde_json::Value::as_u64)
        })
        .unwrap_or(0)
        .min(u32::MAX as u64) as u32
}

fn apply_route_policy_rules_to_candidates(
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

pub fn simulate_route_policy(
    policy: &str,
    performance_matrix: &[RoutePerformanceRow],
    focus: Option<&DispatchRisk>,
) -> RouteSimulationSummary {
    let profile_names = DISPATCH_PROFILES
        .iter()
        .map(|profile| profile.name)
        .collect::<Vec<_>>();
    let mut by_task: std::collections::HashMap<String, Vec<&RoutePerformanceRow>> =
        std::collections::HashMap::new();
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

pub fn sanitize_policy_key(value: &str) -> String {
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

fn route_policy_score(
    policy: &str,
    row: &RoutePerformanceRow,
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

fn compare_scores_desc(left: f64, right: f64) -> std::cmp::Ordering {
    score_sort_key(right).total_cmp(&score_sort_key(left))
}

fn score_sort_key(score: f64) -> f64 {
    if score.is_finite() {
        score
    } else {
        f64::NEG_INFINITY
    }
}

fn route_policy_reasons(
    policy: &str,
    row: &RoutePerformanceRow,
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

fn summarize_route_simulation(
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

pub fn route_simulation_caveats(
    row_count: usize,
    performance_matrix: &[RoutePerformanceRow],
) -> Vec<String> {
    let mut caveats = vec![
        "simulation is replay-only and does not mutate routing policy".to_string(),
        "raw child transcripts are not loaded; only compact /eval evidence is used".to_string(),
    ];
    if row_count == 0 {
        caveats.push(
            "no /eval rows found; recommendations must fall back to deterministic MBIT/risk fit"
                .to_string(),
        );
    }
    let leader_profile_rows = performance_matrix
        .iter()
        .filter(|row| row.scope == "leader" && row.profile.is_some())
        .count();
    if leader_profile_rows == 0 && row_count > 0 {
        caveats.push("live eval rows exist but none have leader profile ids; record profile during completion for policy replay".to_string());
    }
    caveats
}

pub fn profile_role_matches(profile: &DispatchProfileDef, role: &str) -> bool {
    role == profile.role
        || (profile.role == "planner" && role == "architect")
        || (profile.role == "architect" && role == "critic")
        || (profile.role == "executor" && role == "implementer")
        || (profile.role.contains("review") && role.contains("review"))
}

fn round2(value: f64) -> f64 {
    (value * 100.0).round() / 100.0
}

fn round4(value: f64) -> f64 {
    (value * 10_000.0).round() / 10_000.0
}

fn dispatch_risk_needles() -> &'static [(&'static str, &'static str)] {
    &[
        ("dispatch_profile.rs", "touched_area:dispatch_refactor"),
        ("dispatch_ops", "touched_area:dispatch_refactor"),
        ("dispatch", "touches dispatch routing"),
        ("agent_eval.rs", "touched_area:eval_ledger_changes"),
        ("complete_ops.rs", "touched_area:eval_ledger_changes"),
        ("tachi_complete", "touched_area:eval_ledger_changes"),
        ("aggregate_live", "touched_area:eval_ledger_changes"),
        ("performance_matrix", "touched_area:eval_ledger_changes"),
        ("eval", "touches eval/routing evidence"),
        ("ux_matrix", "touches workflow_ux"),
        ("agent-facing ux", "touches agent_experience"),
        ("agent facing ux", "touches agent_experience"),
        ("user experience", "touches agent_experience"),
        ("tool surface", "touches tool_surface_friction"),
        ("体验", "touches agent_experience"),
        ("safe_merge", "touches GitHub merge gate"),
        ("gh_safe_merge.rs", "touches GitHub merge gate"),
        ("merge", "touches merge/release gate"),
        ("schema", "touches schema boundary"),
        ("migration", "touches migration behavior"),
        ("vault_ops.rs", "touches vault/secrets boundary"),
        ("credential_profile", "touches vault/secrets boundary"),
        ("vault", "touches vault/secrets boundary"),
        ("secret", "touches vault/secrets boundary"),
        ("api key", "touches vault/secrets boundary"),
        ("sandbox", "touches sandbox boundary"),
        ("profiles.rs", "touches tool surface/profile visibility"),
        ("tool profile", "touches tool surface/profile visibility"),
        ("mcp", "touches MCP/tool boundary"),
    ]
}

fn contains_high_risk_surface(lower: &str) -> bool {
    [
        "dispatch",
        "eval",
        "merge",
        "schema",
        "migration",
        "vault",
        "secret",
        "api key",
        "sandbox",
        "mcp",
        "profiles.rs",
        "tool profile",
        "dispatch profile",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

fn indicates_missing_verification(lower: &str) -> bool {
    [
        "without tests",
        "without verification",
        "no tests",
        "not tested",
        "untested",
        "skip tests",
        "skipped tests",
        "tests not run",
        "did not run tests",
        "没跑测试",
        "没有测试",
        "没验证",
        "未验证",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

fn indicates_prior_failure(lower: &str) -> bool {
    [
        "regression",
        "failed before",
        "retry loop",
        "flaky",
        "human override",
        "still failing",
        "keeps failing",
        "still broken",
        "failed again",
        "blocked by failure",
        "stuck in",
        "又坏",
        "回归",
        "失败过",
        "卡住",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resolve_dispatch_profile;

    fn failed_eval_row(profile: &str) -> RouteEvalRow {
        RouteEvalRow {
            profile: Some(profile.to_string()),
            completed: false,
            verification_present: false,
        }
    }

    fn verified_eval_row(profile: &str) -> RouteEvalRow {
        RouteEvalRow {
            profile: Some(profile.to_string()),
            completed: true,
            verification_present: true,
        }
    }

    #[test]
    fn score_profile_candidate_keeps_role_correct_profile_above_role_wrong_competitor() {
        let risk = DispatchRisk {
            task_type: "fix_request".to_string(),
            risk: "low".to_string(),
            reasons: vec!["touched_area:dispatch_refactor".to_string()],
            required_profiles: Vec::new(),
            blocked_profiles: Vec::new(),
        };

        let executor = resolve_dispatch_profile("glm_impl").expect("executor profile");
        let competitor = resolve_dispatch_profile("codex_55_review").expect("reviewer profile");
        let rows = vec![
            failed_eval_row(executor.name),
            failed_eval_row(executor.name),
            failed_eval_row(executor.name),
        ];

        let executor_candidate = score_profile_candidate(
            executor,
            &risk,
            &rows,
            &[],
            &[],
            &[],
            RouteEvidenceSource::LiveEvalMemory,
        );
        let competitor_candidate = score_profile_candidate(
            competitor,
            &risk,
            &[],
            &[],
            &[],
            &[],
            RouteEvidenceSource::LiveEvalMemory,
        );

        assert_eq!(executor_candidate.failure_count, 3);
        assert!(
            executor_candidate.score > competitor_candidate.score,
            "role-correct executor ({}) must stay above role-wrong competitor ({})",
            executor_candidate.score,
            competitor_candidate.score
        );
    }

    /// tachi#1675 PR4 / design D7: on the ledger path a candidate with no
    /// evidence says exactly that (`no_ledger_evidence`) and NEVER
    /// `baseline_mbit_fit` — the #1202 ruling demoted MBIT to derived evidence,
    /// so "the card fits" is not a routing ground. The legacy `/eval`-memory
    /// path keeps its own fallback, which is why the source is threaded rather
    /// than the string simply deleted.
    ///
    /// A risk with NO reasons and NO role-matching task type is the point: it
    /// makes EVERY profile a zero-signal candidate, so the fallback is what is
    /// under test rather than an incidental scoring path.
    #[test]
    fn the_ledger_path_never_falls_back_to_a_baseline_mbit_fit() {
        let risk = DispatchRisk {
            task_type: "unknown_request".to_string(),
            risk: "medium".to_string(),
            reasons: Vec::new(),
            required_profiles: Vec::new(),
            blocked_profiles: Vec::new(),
        };
        let loadout = RoutePolicyRuleLoadout {
            namespace: ROUTE_POLICY_RULE_NS,
            min_samples: MIN_ROUTE_POLICY_RULE_SAMPLES,
            applied: Vec::new(),
            skipped: Vec::new(),
        };

        let ledger = recommend_dispatch_profile_candidates(
            &risk,
            &[],
            &[],
            &[],
            &loadout,
            RouteEvidenceSource::DecisionFactLedger,
            |_| Ok(Vec::new()),
        )
        .expect("ledger-sourced candidates");
        assert!(
            !ledger.is_empty(),
            "the profile set must not be empty or this proves nothing"
        );
        for candidate in &ledger {
            assert!(
                !candidate
                    .reasons
                    .iter()
                    .any(|reason| reason == BASELINE_MBIT_FIT_REASON),
                "{} reported a baseline MBIT fit on the ledger path: {:?}",
                candidate.profile,
                candidate.reasons
            );
            assert!(
                candidate
                    .reasons
                    .iter()
                    .any(|reason| reason == NO_LEDGER_EVIDENCE_REASON),
                "{} must name the absence of ledger evidence: {:?}",
                candidate.profile,
                candidate.reasons
            );
        }

        // The legacy path is unchanged — the flip moved which evidence is
        // read, it did not silently rewrite the old path's vocabulary.
        let legacy = recommend_dispatch_profile_candidates(
            &risk,
            &[],
            &[],
            &[],
            &loadout,
            RouteEvidenceSource::LiveEvalMemory,
            |_| Ok(Vec::new()),
        )
        .expect("memory-sourced candidates");
        assert!(legacy.iter().all(|candidate| candidate
            .reasons
            .iter()
            .any(|reason| reason == BASELINE_MBIT_FIT_REASON)));
    }

    /// The declared source is one vocabulary, not two spellings.
    #[test]
    fn evidence_sources_declare_stable_names() {
        assert_eq!(
            RouteEvidenceSource::DecisionFactLedger.as_str(),
            "decision_fact_ledger"
        );
        assert_eq!(
            RouteEvidenceSource::LiveEvalMemory.as_str(),
            "live_eval_memory"
        );
    }

    #[test]
    fn research_request_prefers_read_role_over_executor_even_with_better_eval() {
        let risk = DispatchRisk {
            task_type: "research_request".to_string(),
            risk: "low".to_string(),
            reasons: Vec::new(),
            required_profiles: Vec::new(),
            blocked_profiles: Vec::new(),
        };

        let executor = resolve_dispatch_profile("glm_impl").expect("executor profile");
        let explorer = resolve_dispatch_profile("deepseek_explore").expect("explore profile");
        let executor_rows = vec![
            verified_eval_row(executor.name),
            verified_eval_row(executor.name),
        ];

        let executor_candidate = score_profile_candidate(
            executor,
            &risk,
            &executor_rows,
            &[],
            &[],
            &[],
            RouteEvidenceSource::LiveEvalMemory,
        );
        let explorer_candidate = score_profile_candidate(
            explorer,
            &risk,
            &[],
            &[],
            &[],
            &[],
            RouteEvidenceSource::LiveEvalMemory,
        );

        assert!(
            explorer_candidate.score > executor_candidate.score,
            "read-only research must prefer the explore role ({}) over a write-executor \
             with better eval history ({})",
            explorer_candidate.score,
            executor_candidate.score
        );
    }

    #[test]
    fn route_policy_simulation_sinks_non_finite_scores() {
        let rows = vec![
            RoutePerformanceRow {
                scope: "leader".to_string(),
                profile: Some("claude_plan".to_string()),
                role: Some("planner".to_string()),
                agent: "claude".to_string(),
                task_type: "review_request".to_string(),
                samples: 10,
                success_rate: Some(0.95),
                verification_rate: 1.0,
                avg_quality_score: Some(f64::NAN),
                ..Default::default()
            },
            RoutePerformanceRow {
                scope: "leader".to_string(),
                profile: Some("codex_55_review".to_string()),
                role: Some("reviewer".to_string()),
                agent: "codex".to_string(),
                task_type: "review_request".to_string(),
                samples: 10,
                success_rate: Some(0.90),
                verification_rate: 1.0,
                avg_quality_score: Some(0.90),
                ..Default::default()
            },
        ];

        let summary = simulate_route_policy("quality_first", &rows, None);

        assert_eq!(summary.route_choices.len(), 1);
        assert_eq!(summary.route_choices[0].profile, "codex_55_review");
        assert!(summary.route_choices[0].score.is_finite());
    }

    #[test]
    fn compare_scores_desc_keeps_non_finite_scores_last() {
        let mut scores = [
            ("nan", f64::NAN),
            ("best", 42.0),
            ("worst_finite", -1.0),
            ("positive_inf", f64::INFINITY),
            ("negative_inf", f64::NEG_INFINITY),
        ];

        scores.sort_by(|a, b| compare_scores_desc(a.1, b.1).then(a.0.cmp(b.0)));

        assert_eq!(scores[0], ("best", 42.0));
        assert_eq!(scores[1], ("worst_finite", -1.0));
        assert!(scores[2..].iter().all(|(_, score)| !score.is_finite()));
    }
}
