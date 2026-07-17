use serde_json::json;

use crate::tool_params::{SaveMemoryParams, TachiCompleteParams};

use super::scrub::{scrub_eval_json, scrub_eval_string, scrub_eval_strings};

pub(super) struct CompleteEvalRecord {
    pub(super) task_id: String,
    pub(super) path: String,
    pub(super) safe_task: String,
    pub(super) safe_agent: String,
    pub(super) safe_notes: Option<String>,
    pub(super) safe_skills_used: Vec<String>,
    pub(super) safe_evidence_refs: Vec<String>,
    pub(super) safe_tests_run: Vec<String>,
    pub(super) safe_trajectory: Option<serde_json::Value>,
    pub(super) safe_subagents: serde_json::Value,
    pub(super) safe_feedback_rules: Vec<String>,
    pub(super) outcome_norm: String,
    pub(super) diff_present: bool,
    pub(super) verification_present: bool,
    pub(super) secret_redactions: usize,
    pub(super) mem_params: SaveMemoryParams,
}

pub(super) fn build_complete_eval_record(
    params: &TachiCompleteParams,
    date: &str,
    ts: &str,
    // #1041 B7: the caller-computed, wire-accurate signal — NOT
    // `params.project.is_some()` (see the `project_explicit` field below for
    // why that inversion is unsafe for the `tachi_task(action='complete')`
    // bridge path). `handle_tachi_complete`'s two callers compute this
    // correctly for their own entry point (see that fn's doc).
    project_explicit: bool,
) -> CompleteEvalRecord {
    let task_id = params.task_id.clone().unwrap_or_else(|| {
        let agent_slug = params
            .agent
            .replace(|c: char| !c.is_ascii_alphanumeric(), "-")
            .to_ascii_lowercase();
        format!("{}-{}", ts, agent_slug)
    });

    let path = format!("/eval/{}/{}", date, task_id);
    let mut secret_redactions = 0usize;
    let safe_task = scrub_eval_string(&params.task, &mut secret_redactions);
    let safe_agent = scrub_eval_string(&params.agent, &mut secret_redactions);
    let safe_notes = params
        .notes
        .as_ref()
        .map(|notes| scrub_eval_string(notes, &mut secret_redactions));
    let safe_skills_used = scrub_eval_strings(&params.skills_used, &mut secret_redactions);
    let safe_evidence_refs = scrub_eval_strings(&params.evidence_refs, &mut secret_redactions);
    let safe_tests_run = scrub_eval_strings(&params.tests_run, &mut secret_redactions);
    let safe_trajectory = params.trajectory.as_ref().map(|trajectory| {
        scrub_eval_json(
            coerce_stringified_trajectory_array(trajectory.clone()),
            &mut secret_redactions,
        )
    });
    let safe_diff = params
        .diff
        .as_ref()
        .map(|diff| scrub_eval_string(diff, &mut secret_redactions));
    let safe_subagents = scrub_eval_json(json!(&params.subagents), &mut secret_redactions);
    let safe_feedback_rules =
        scrub_eval_strings(&params.feedback_rules_applied, &mut secret_redactions);

    let outcome_norm = params.outcome.to_ascii_lowercase();
    let outcome_emoji = match outcome_norm.as_str() {
        "success" => "✓",
        "failure" => "✗",
        "partial" => "~",
        "aborted" => "⊘",
        _ => "?",
    };

    let duration_display = params
        .duration_ms
        .map(|ms| {
            if ms < 1000 {
                format!("{}ms", ms)
            } else if ms < 60_000 {
                format!("{:.1}s", (ms as f64) / 1000.0)
            } else {
                format!("{:.1}min", (ms as f64) / 60_000.0)
            }
        })
        .unwrap_or_else(|| "?".to_string());

    let cost_display = match (params.cost_tokens, params.cost_usd) {
        (Some(t), Some(u)) => format!(" | {} tok | ${:.4}", t, u),
        (Some(t), None) => format!(" | {} tok", t),
        (None, Some(u)) => format!(" | ${:.4}", u),
        _ => String::new(),
    };

    let mut summary_lines = vec![format!(
        "[{}] {} completed task in {}{}",
        outcome_emoji, safe_agent, duration_display, cost_display
    )];
    summary_lines.push(format!("Task: {}", safe_task));
    if !safe_skills_used.is_empty() {
        summary_lines.push(format!("Skills: {}", safe_skills_used.join(", ")));
    }
    if let Some(profile) = params.profile.as_deref().filter(|s| !s.is_empty()) {
        summary_lines.push(format!("Profile: {}", profile));
    }
    if let Some(issue_ref) = params.issue_ref.as_deref().filter(|s| !s.is_empty()) {
        summary_lines.push(format!("Issue: {}", issue_ref));
    }
    if let Some(pr_ref) = params.pr_ref.as_deref().filter(|s| !s.is_empty()) {
        summary_lines.push(format!("PR: {}", pr_ref));
    }
    if !params.tests_run.is_empty() {
        summary_lines.push(format!("Tests: {}", params.tests_run.join("; ")));
    }
    if let Some(q) = params.quality_score {
        summary_lines.push(format!("Quality: {:.2}", q));
    }
    if let Some(notes) = &safe_notes {
        if !notes.is_empty() {
            summary_lines.push(format!("Notes: {}", notes));
        }
    }
    let text = summary_lines.join("\n");

    let mut keywords: Vec<String> = Vec::new();
    keywords.push(safe_agent.clone());
    keywords.push(outcome_norm.clone());
    keywords.push("eval".to_string());
    for skill in &safe_skills_used {
        keywords.push(skill.clone());
    }
    if let Some(profile) = params.profile.as_deref().filter(|s| !s.is_empty()) {
        keywords.push(profile.to_string());
        keywords.push("dispatch_profile".to_string());
    }
    if let Some(risk) = params.risk.as_deref().filter(|s| !s.is_empty()) {
        keywords.push(format!("risk:{risk}"));
    }
    let entities = safe_skills_used.clone();
    if !params.subagents.is_empty() {
        keywords.push("subagent_eval".to_string());
    }

    let mut metadata_map = serde_json::Map::new();
    metadata_map.insert("task_id".into(), serde_json::json!(task_id.clone()));
    metadata_map.insert("agent".into(), serde_json::json!(safe_agent.clone()));
    metadata_map.insert("outcome".into(), serde_json::json!(outcome_norm.clone()));
    if let Some(task_type) = &params.task_type {
        if !task_type.is_empty() {
            metadata_map.insert("task_type".into(), serde_json::json!(task_type));
        }
    }
    if let Some(profile) = &params.profile {
        if !profile.is_empty() {
            metadata_map.insert("profile".into(), serde_json::json!(profile));
        }
    }
    if let Some(risk) = &params.risk {
        if !risk.is_empty() {
            metadata_map.insert("risk".into(), serde_json::json!(risk));
        }
    }
    if let Some(ms) = params.duration_ms {
        metadata_map.insert("duration_ms".into(), serde_json::json!(ms));
    }
    if !safe_skills_used.is_empty() {
        metadata_map.insert(
            "skills_used".into(),
            serde_json::json!(safe_skills_used.clone()),
        );
    }
    if let Some(t) = params.cost_tokens {
        metadata_map.insert("cost_tokens".into(), serde_json::json!(t));
    }
    if let Some(u) = params.cost_usd {
        metadata_map.insert("cost_usd".into(), serde_json::json!(u));
    }
    if let Some(q) = params.quality_score {
        metadata_map.insert("quality_score".into(), serde_json::json!(q));
    }
    if let Some(traj) = &safe_trajectory {
        metadata_map.insert("trajectory".into(), traj.clone());
    }
    if let Some(diff) = &safe_diff {
        if !diff.is_empty() {
            metadata_map.insert("diff".into(), serde_json::json!(diff));
        }
    }
    let diff_present = params.diff_present.unwrap_or_else(|| {
        params
            .diff
            .as_deref()
            .is_some_and(|diff| !diff.trim().is_empty())
    });
    metadata_map.insert("diff_present".into(), serde_json::json!(diff_present));
    let verification_present =
        !params.tests_run.is_empty() || !params.evidence_refs.is_empty() || diff_present;
    metadata_map.insert(
        "verification_present".into(),
        serde_json::json!(verification_present),
    );
    if let Some(wt) = &params.worktree {
        metadata_map.insert(
            "worktree".into(),
            serde_json::json!(scrub_eval_string(wt, &mut secret_redactions)),
        );
    }
    if !params.subagents.is_empty() {
        let roles: Vec<String> = params.subagents.iter().map(|s| s.role.clone()).collect();
        let models: Vec<String> = params
            .subagents
            .iter()
            .filter_map(|s| s.model.clone())
            .filter(|s| !s.is_empty())
            .collect();
        metadata_map.insert("subagent_eval".into(), serde_json::json!(true));
        metadata_map.insert(
            "subagent_count".into(),
            serde_json::json!(params.subagents.len()),
        );
        metadata_map.insert("subagent_roles".into(), serde_json::json!(roles));
        if !models.is_empty() {
            metadata_map.insert("subagent_models".into(), serde_json::json!(models));
        }
        metadata_map.insert("subagents".into(), safe_subagents.clone());
    }
    if !safe_feedback_rules.is_empty() {
        metadata_map.insert(
            "feedback_rules_applied".into(),
            serde_json::json!(safe_feedback_rules.clone()),
        );
    }
    if let Some(did) = &params.dispatch_id {
        metadata_map.insert("dispatch_id".into(), serde_json::json!(did));
    }
    if let Some(flow_id) = &params.flow_id {
        if !flow_id.is_empty() {
            metadata_map.insert("flow_id".into(), serde_json::json!(flow_id));
        }
    }
    if let Some(issue_ref) = &params.issue_ref {
        if !issue_ref.is_empty() {
            metadata_map.insert("issue_ref".into(), serde_json::json!(issue_ref));
        }
    }
    if let Some(pr_ref) = &params.pr_ref {
        if !pr_ref.is_empty() {
            metadata_map.insert("pr_ref".into(), serde_json::json!(pr_ref));
        }
    }
    if !safe_evidence_refs.is_empty() {
        metadata_map.insert(
            "evidence_refs".into(),
            serde_json::json!(safe_evidence_refs.clone()),
        );
    }
    if !safe_tests_run.is_empty() {
        metadata_map.insert(
            "tests_run".into(),
            serde_json::json!(safe_tests_run.clone()),
        );
    }
    if secret_redactions > 0 {
        metadata_map.insert(
            "secret_redactions".into(),
            serde_json::json!(secret_redactions),
        );
        metadata_map.insert(
            "secret_redaction_warning".into(),
            serde_json::json!("Potential secrets were redacted before eval persistence."),
        );
    }

    let mem_params = SaveMemoryParams {
        text,
        summary: format!("[{}] {} / {}", outcome_emoji, safe_agent, safe_task),
        path: path.clone(),
        importance: match outcome_norm.as_str() {
            "success" => 0.55,
            "failure" => 0.75,
            "partial" => 0.6,
            "aborted" => 0.5,
            _ => 0.5,
        },
        category: "eval".to_string(),
        topic: safe_task.clone(),
        keywords,
        persons: Vec::new(),
        entities,
        location: String::new(),
        scope: params
            .scope
            .clone()
            .unwrap_or_else(|| "project".to_string()),
        vector: None,
        id: None,
        force: false,
        auto_link: true,
        project: params.project.clone(),
        // #1041 B7 (was F2's stale premise): `params.project.is_some()` is
        // NOT proof of deliberate intent here — `TachiCompleteParams.project`
        // can be a transport-injected default forwarded from the
        // `tachi_task(action='complete')` bridge (`task_router.rs`), whose
        // OWN caller may have omitted `project=` entirely on a bound
        // session. Use the signal the caller of `build_complete_eval_record`
        // resolved correctly for its own entry point instead.
        project_explicit,
        retention_policy: None,
        domain: Some("eval".to_string()),
        timestamp: None,
        valid_from: None,
        valid_until: None,
        metadata: Some(serde_json::Value::Object(metadata_map)),
        emit_continuity: false,
    };

    CompleteEvalRecord {
        task_id,
        path,
        safe_task,
        safe_agent,
        safe_notes,
        safe_skills_used,
        safe_evidence_refs,
        safe_tests_run,
        safe_trajectory,
        safe_subagents,
        safe_feedback_rules,
        outcome_norm,
        diff_present,
        verification_present,
        secret_redactions,
        mem_params,
    }
}

fn coerce_stringified_trajectory_array(value: serde_json::Value) -> serde_json::Value {
    let serde_json::Value::String(raw) = &value else {
        return value;
    };
    match serde_json::from_str::<serde_json::Value>(raw) {
        Ok(parsed) if parsed.is_array() => parsed,
        _ => value,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_params() -> TachiCompleteParams {
        TachiCompleteParams {
            task_id: None,
            task: "fix the thing".to_string(),
            agent: "codex".to_string(),
            outcome: "success".to_string(),
            task_type: Some("fix_request".to_string()),
            profile: None,
            risk: None,
            duration_ms: None,
            skills_used: Vec::new(),
            cost_tokens: Some(500),
            cost_usd: Some(0.02),
            quality_score: None,
            notes: None,
            trajectory: None,
            diff: None,
            worktree: None,
            subagents: Vec::new(),
            feedback_rules_applied: Vec::new(),
            dispatch_id: Some("dispatch-abc".to_string()),
            flow_id: Some("flow-1".to_string()),
            issue_ref: Some("kckylechen1/tachi#773".to_string()),
            pr_ref: None,
            evidence_refs: Vec::new(),
            tests_run: Vec::new(),
            diff_present: None,
            scope: Some("global".to_string()),
            // Mimics a transport-injected default forwarded through the
            // `tachi_task(action='complete')` bridge — `project` present,
            // but the ORIGINAL caller never wrote `project=` at all.
            project: Some("quant".to_string()),
            format: None,
            signatures: Vec::new(),
            rulings: Vec::new(),
            adjudication: None,
            eval_run_ids: Vec::new(),
        }
    }

    /// #1041 B7 core regression: `build_complete_eval_record` must use the
    /// CALLER-resolved `project_explicit` argument, never re-derive it from
    /// `params.project.is_some()` — that inversion would treat a
    /// transport-injected default (present `project`, but NOT a caller
    /// decision) as deliberate placement authority, letting eval rows for
    /// mismatched-domain content skip the #1041 S1 write-affinity gate
    /// entirely (the gate's `explicit_project_override` check trusts this
    /// exact flag).
    #[test]
    fn project_explicit_follows_the_argument_not_project_presence() {
        let params = base_params();
        assert!(
            params.project.is_some(),
            "fixture must have `project` present to prove the inversion"
        );

        let with_true = build_complete_eval_record(&params, "2026-01-01", "20260101T000000Z", true);
        assert!(
            with_true.mem_params.project_explicit,
            "an actually-explicit caller decision must come through as true"
        );

        let with_false =
            build_complete_eval_record(&params, "2026-01-01", "20260101T000000Z", false);
        assert!(
            !with_false.mem_params.project_explicit,
            "project.is_some() alone must NOT force project_explicit=true — \
             that was exactly the B7 inversion bug"
        );
    }
}
