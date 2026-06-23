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
