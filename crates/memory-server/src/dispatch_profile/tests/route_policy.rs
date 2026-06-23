use super::*;

#[test]
fn route_policy_simulation_sinks_non_finite_scores() {
    let rows = vec![
        AgentPerformanceMatrixRow {
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
        AgentPerformanceMatrixRow {
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
