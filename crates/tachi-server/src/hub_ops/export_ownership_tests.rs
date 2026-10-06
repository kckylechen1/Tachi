use super::*;

fn skill(id: &str) -> HubCapability {
    serde_json::from_value(json!({
        "id": id, "cap_type": "skill", "name": id, "version": 1,
        "description": "fixture", "definition": "{\"content\":\"new content\"}",
        "enabled": true, "uses": 0, "successes": 0, "failures": 0,
        "avg_rating": 0.0, "last_used": null, "created_at": "fixture", "updated_at": "fixture"
    }))
    .unwrap()
}

#[test]
fn clean_request_preserves_unrelated_skills_and_reports_no_pruning() {
    let temp = tempfile::tempdir_in(std::fs::canonicalize(std::env::temp_dir()).unwrap()).unwrap();
    let store = temp.path().join("store");
    let projection = temp.path().join("projection");
    let foreign = temp.path().join("foreign");
    std::fs::create_dir_all(projection.join("unrelated")).unwrap();
    std::fs::write(projection.join("unrelated/SKILL.md"), "unrelated content").unwrap();
    std::fs::create_dir(&foreign).unwrap();
    std::fs::write(foreign.join("SKILL.md"), "foreign content").unwrap();
    std::os::unix::fs::symlink(&foreign, projection.join("foreign-link")).unwrap();
    let params = ExportSkillsParams {
        agent: "claude".to_string(),
        skill_ids: None,
        visibility: "all".to_string(),
        output_dir: None,
        clean: true,
    };
    let result: serde_json::Value = serde_json::from_str(
        &export_for_claude_to_dirs(&[skill("skill:new")], &params, &store, &projection).unwrap(),
    )
    .unwrap();
    assert_eq!(
        std::fs::read_to_string(projection.join("unrelated/SKILL.md")).unwrap(),
        "unrelated content"
    );
    assert_eq!(
        std::fs::read_link(projection.join("foreign-link")).unwrap(),
        foreign
    );
    assert_eq!(
        std::fs::read_to_string(foreign.join("SKILL.md")).unwrap(),
        "foreign content"
    );
    assert_eq!(result["exported"], 1);
    assert_eq!(result["errors"].as_array().unwrap().len(), 0);
    assert_eq!(result["cleanup"]["requested"], true);
    assert_eq!(result["cleanup"]["performed"], false);
}

#[test]
fn conflicting_export_is_not_reported_as_success() {
    let temp = tempfile::tempdir_in(std::fs::canonicalize(std::env::temp_dir()).unwrap()).unwrap();
    let store = temp.path().join("store");
    let projection = temp.path().join("projection");
    std::fs::create_dir_all(projection.join("conflict")).unwrap();
    std::fs::write(projection.join("conflict/SKILL.md"), "original").unwrap();
    let params = ExportSkillsParams {
        agent: "claude".to_string(),
        skill_ids: None,
        visibility: "all".to_string(),
        output_dir: None,
        clean: false,
    };
    let result: serde_json::Value = serde_json::from_str(
        &export_for_claude_to_dirs(&[skill("skill:conflict")], &params, &store, &projection)
            .unwrap(),
    )
    .unwrap();
    assert_eq!(result["exported"], 0);
    assert_eq!(result["errors"].as_array().unwrap().len(), 1);
    assert_eq!(
        std::fs::read_to_string(projection.join("conflict/SKILL.md")).unwrap(),
        "original"
    );
    assert!(!store.join("conflict").exists());
}

#[test]
fn empty_export_reports_cleanup_without_initializing_roots() {
    let result = empty_export_result("claude", true);
    assert_eq!(result["exported"], 0);
    assert_eq!(result["cleanup"]["requested"], true);
    assert_eq!(result["cleanup"]["performed"], false);
    assert!(empty_export_result("generic", true)
        .get("cleanup")
        .is_none());
}

#[test]
fn root_initialization_failure_reports_possible_new_directories() {
    let temp = tempfile::tempdir_in(std::fs::canonicalize(std::env::temp_dir()).unwrap()).unwrap();
    let foreign = temp.path().join("foreign");
    std::fs::create_dir(&foreign).unwrap();
    let link = temp.path().join("link");
    std::os::unix::fs::symlink(&foreign, &link).unwrap();
    let store = temp.path().join("new-store");
    let params = ExportSkillsParams {
        agent: "claude".to_string(),
        skill_ids: None,
        visibility: "all".to_string(),
        output_dir: None,
        clean: true,
    };
    let error =
        export_for_claude_to_dirs(&[skill("skill:new")], &params, &store, &link).unwrap_err();
    assert!(
        error.contains("new export root directories may exist"),
        "{error}"
    );
    assert!(store.is_dir());
    assert_eq!(std::fs::read_dir(foreign).unwrap().count(), 0);
}
