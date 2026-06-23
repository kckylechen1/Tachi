use super::*;

fn scoring_test_server() -> MemoryServer {
    let db_path = std::env::temp_dir().join(format!(
        "dispatch-scoring-test-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    MemoryServer::new(db_path, None).expect("test memory server")
}

fn failed_eval_row(profile: &str) -> EvalRow {
    EvalRow {
        agent: "custom".to_string(),
        profile: Some(profile.to_string()),
        model: None,
        mode: None,
        task_type: crate::agent_eval::TaskType::FixRequest,
        turns: 0,
        tool_calls: 0,
        verification_present: false,
        failure_mode: Some("blocked".to_string()),
        completion_status: CompletionStatus::Blocked,
        cost_usd: None,
        cost_tokens: None,
        quality_score: None,
        latency_ms: None,
        subagents: Vec::new(),
    }
}

fn verified_eval_row(profile: &str, task_type: crate::agent_eval::TaskType) -> EvalRow {
    EvalRow {
        completion_status: CompletionStatus::Completed,
        verification_present: true,
        failure_mode: None,
        task_type,
        ..failed_eval_row(profile)
    }
}

#[test]
fn score_profile_candidate_keeps_role_correct_profile_above_role_wrong_competitor() {
    let server = scoring_test_server();

    // A fix_request: the executor role is correct, the senior reviewer is not.
    // The lone signal (dispatch_refactor) is something the reviewer is
    // strong_against but the executor is neither strong nor weak against, so
    // it isolates the eval-failure penalty as the only differentiator.
    let risk = DispatchRisk {
        task_type: "fix_request".to_string(),
        risk: "low".to_string(),
        reasons: vec!["touched_area:dispatch_refactor".to_string()],
        required_profiles: Vec::new(),
        blocked_profiles: Vec::new(),
    };

    let executor = resolve_dispatch_profile("glm_51_impl").expect("executor profile");
    let competitor = resolve_dispatch_profile("codex_55_review").expect("reviewer profile");

    // Sparse failures (3) for the role-correct executor. Under the old bare
    // -8.0-per-row penalty these alone removed -24, sinking the role bonus
    // below the role-wrong competitor; the bounded/weighted path must not.
    let rows = vec![
        failed_eval_row(executor.name),
        failed_eval_row(executor.name),
        failed_eval_row(executor.name),
    ];

    let executor_candidate =
        score_profile_candidate(&server, executor, &risk, &rows, &[], &[]).expect("executor");
    let competitor_candidate =
        score_profile_candidate(&server, competitor, &risk, &[], &[], &[]).expect("competitor");

    assert_eq!(executor_candidate.failure_count, 3);
    assert!(
        executor_candidate.score > competitor_candidate.score,
        "role-correct executor ({}) must stay above role-wrong competitor ({})",
        executor_candidate.score,
        competitor_candidate.score
    );
}

#[test]
fn research_request_prefers_read_role_over_executor_even_with_better_eval() {
    let server = scoring_test_server();

    // A read-only research task (e.g. "list files and summarize each"): the
    // explore role fits; an executor's diff/tests/files_changed evidence
    // contract is unsatisfiable. Give the EXECUTOR the better live history
    // (two verified successes) and the explorer NONE, then assert the explorer
    // still wins on role/task fit — the routing-policy gap surfaced live where
    // glm_51_impl(executor)=23.8 beat deepseek_explore(explore)=-0.2.
    let risk = DispatchRisk {
        task_type: "research_request".to_string(),
        risk: "low".to_string(),
        reasons: Vec::new(),
        required_profiles: Vec::new(),
        blocked_profiles: Vec::new(),
    };

    let executor = resolve_dispatch_profile("glm_51_impl").expect("executor profile");
    let explorer = resolve_dispatch_profile("deepseek_explore").expect("explore profile");

    let executor_rows = vec![
        verified_eval_row(executor.name, crate::agent_eval::TaskType::ResearchRequest),
        verified_eval_row(executor.name, crate::agent_eval::TaskType::ResearchRequest),
    ];

    let executor_candidate =
        score_profile_candidate(&server, executor, &risk, &executor_rows, &[], &[])
            .expect("executor");
    let explorer_candidate =
        score_profile_candidate(&server, explorer, &risk, &[], &[], &[]).expect("explorer");

    assert!(
        explorer_candidate.score > executor_candidate.score,
        "read-only research must prefer the explore role ({}) over a write-executor \
         with better eval history ({})",
        explorer_candidate.score,
        executor_candidate.score
    );

    // explain_request shares the read-only treatment (same arm).
    let explain_risk = DispatchRisk {
        task_type: "explain_request".to_string(),
        ..risk.clone()
    };
    let explain_executor = score_profile_candidate(
        &server,
        resolve_dispatch_profile("glm_51_impl").expect("executor"),
        &explain_risk,
        &[],
        &[],
        &[],
    )
    .expect("explain executor");
    let explain_explorer = score_profile_candidate(
        &server,
        resolve_dispatch_profile("deepseek_explore").expect("explorer"),
        &explain_risk,
        &[],
        &[],
        &[],
    )
    .expect("explain explorer");
    assert!(
        explain_explorer.score > explain_executor.score,
        "explain_request must also prefer the explore role ({}) over an executor ({})",
        explain_explorer.score,
        explain_executor.score
    );
}
