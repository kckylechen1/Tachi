use super::*;

#[test]
fn setup_report_detects_readiness_from_local_state() {
    let home = test_fixture_path(format!("tachi-setup-report-{}", uuid::Uuid::new_v4()));
    let app_home = home.join(".tachi");
    let global_db = app_home.join("global").join("memory.db");
    let project_db = home.join("repo").join(".tachi").join("memory.db");
    let git_root = home.join("repo");

    std::fs::create_dir_all(app_home.join("skills").join("review-skill"))
        .expect("create tachi skill");
    std::fs::write(
        app_home
            .join("skills")
            .join("review-skill")
            .join("SKILL.md"),
        "# review",
    )
    .expect("write tachi skill");
    std::fs::create_dir_all(home.join(".claude")).expect("create claude dir");
    std::fs::write(home.join(".claude").join(".mcp.json"), "{}").expect("write claude mcp");
    std::fs::create_dir_all(global_db.parent().unwrap()).expect("create global db dir");

    let env = HashMap::from([
        ("VOYAGE_API_KEY".to_string(), "voyage-test".to_string()),
        (
            "SILICONFLOW_API_KEY".to_string(),
            "siliconflow-test".to_string(),
        ),
        ("ENABLE_PIPELINE".to_string(), "true".to_string()),
    ]);

    let report = api::build_setup_report(
        &home,
        &app_home,
        &global_db,
        Some(&project_db),
        Some(&git_root),
        &env,
    )
    .expect("setup report should build");

    assert_eq!(report.items.len(), 6);
    assert!(report.items.iter().any(|item| item.id == "cli_binary"));
    assert_eq!(
        report
            .items
            .iter()
            .find(|item| item.id == "api_keys")
            .map(|item| item.status.as_str()),
        Some("ready")
    );
    assert_eq!(
        report
            .items
            .iter()
            .find(|item| item.id == "skills")
            .map(|item| item.status.as_str()),
        Some("ready")
    );
    assert_eq!(
        report
            .items
            .iter()
            .find(|item| item.id == "agents")
            .map(|item| item.status.as_str()),
        Some("ready")
    );
    assert_eq!(
        report
            .items
            .iter()
            .find(|item| item.id == "pipeline")
            .map(|item| item.status.as_str()),
        Some("ready")
    );
    assert!(
        report
            .items
            .iter()
            .find(|item| item.id == "vault")
            .expect("vault item")
            .details
            .iter()
            .any(|detail| detail.contains("vault: not initialized")),
        "expected vault to be reported as not initialized"
    );
    assert!(
        report
            .next_steps
            .iter()
            .any(|step| step.contains("encrypted vault")),
        "expected a vault-setup next step when the vault is not initialized"
    );

    let _ = std::fs::remove_dir_all(&home);
}
