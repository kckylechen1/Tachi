use super::*;

#[test]
fn issue_automation_plan_blocks_missing_acceptance_and_high_risk() {
    let missing_acceptance = crate::task_lifecycle::IssueSnapshot {
        repo: "kckylechen1/tachi".to_string(),
        number: 380,
        title: "Automate issue dispatch".to_string(),
        body: Some("Let Tachi read an issue and do the work.".to_string()),
        labels: Vec::new(),
        state: Some("OPEN".to_string()),
        url: "https://github.com/kckylechen1/tachi/issues/380".to_string(),
        doc_paths: Vec::new(),
        spec_paths: Vec::new(),
    };
    let plan = crate::task_lifecycle::build_issue_automation_plan(&missing_acceptance, None);
    assert_eq!(plan["dispatch_allowed"], json!(false));
    assert_eq!(plan["requires_leader"], json!(true));
    assert!(plan["leader_gate_reasons"]
        .as_array()
        .expect("leader gate reasons")
        .contains(&json!("missing_acceptance_criteria")));

    let high_risk = crate::task_lifecycle::IssueSnapshot {
        repo: "kckylechen1/tachi".to_string(),
        number: 381,
        title: "Rotate vault token handling".to_string(),
        body: Some("## Acceptance criteria\n- Secrets stay redacted.".to_string()),
        labels: vec!["security".to_string()],
        state: Some("OPEN".to_string()),
        url: "https://github.com/kckylechen1/tachi/issues/381".to_string(),
        doc_paths: Vec::new(),
        spec_paths: Vec::new(),
    };
    let plan = crate::task_lifecycle::build_issue_automation_plan(&high_risk, None);
    assert_eq!(plan["dispatch_allowed"], json!(false));
    assert!(plan["high_risk_reasons"]
        .as_array()
        .expect("high risk reasons")
        .contains(&json!("touches_security")));
}
