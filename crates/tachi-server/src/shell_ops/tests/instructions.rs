use super::*;

#[test]
fn slugify_basic() {
    assert_eq!(slugify("Hello, World!"), "hello-world");
    assert_eq!(slugify("   "), "flow");
    assert_eq!(slugify("已经-OK_test"), "ok-test");
}

#[test]
fn meta_skill_mapping_is_complete() {
    for stage in &["brainstorm", "plan", "dispatch", "review", "ship"] {
        assert!(meta_skill_for_stage(stage).is_some(), "stage {stage}");
    }
    assert!(meta_skill_for_stage("kanban").is_none());
    assert!(meta_skill_for_stage("status").is_none());
}

#[test]
fn superpowers_meta_skills_resolve_for_all_shell_stages() {
    for stage in STAGE_ACTIONS {
        let rel = meta_skill_for_stage(stage).expect("mapped stage");
        let resolved = resolve_meta_skill(rel)
            .unwrap_or_else(|| panic!("superpowers skill not found for stage {stage} at {rel}"));
        assert!(
            resolved.ends_with("SKILL.md"),
            "stage {stage} should resolve to SKILL.md, got {}",
            resolved.display()
        );
        let content = std::fs::read_to_string(&resolved)
            .unwrap_or_else(|e| panic!("read superpowers skill for {stage}: {e}"));
        assert!(
            content.contains("name:") || content.starts_with("# "),
            "stage {stage} skill should look like a SKILL.md front matter or heading"
        );
    }
}

#[test]
fn build_instruction_includes_required_sections() {
    let inj = InjectionResult {
        required: true,
        rel_path: Some("skill/x/SKILL.md".into()),
        source_path: Some("/abs/skill/x/SKILL.md".into()),
        injected_path: Some(".tachi/runs/flow_x/injected/superpowers-plan.md".into()),
        content_hash: Some("a".repeat(16)),
        loaded: true,
        warning: None,
    };
    let s = build_instruction_md(
        "flow_x",
        "plan",
        "do the thing",
        &inj,
        Some("be careful"),
        &["cargo test".to_string()],
        &["crates/tachi-server/**".to_string()],
    );
    assert!(s.contains("flow_x"));
    assert!(s.contains("Stage: **plan**"));
    assert!(s.contains("do the thing"));
    assert!(s.contains("superpowers-plan.md"));
    assert!(s.contains("## Native Lifecycle Policy"));
    assert!(s.contains("skill:superpowers-writing-plans"));
    assert!(s.contains("skill:waza-think"));
    assert!(s.contains("max 6 concurrent workers"));
    assert!(s.contains("cargo test"));
    assert!(s.contains("crates/tachi-server/**"));
    assert!(s.contains("be careful"));
}

#[test]
fn ship_instruction_includes_pr_first_release_flow() {
    let inj = InjectionResult {
        required: true,
        rel_path: Some("skill/x/SKILL.md".into()),
        source_path: None,
        injected_path: Some(".tachi/runs/flow_x/injected/superpowers-ship.md".into()),
        content_hash: Some("b".repeat(16)),
        loaded: true,
        warning: None,
    };
    let s = build_instruction_md("flow_x", "ship", "ship it", &inj, None, &[], &[]);
    assert!(s.contains("## Release Flow"));
    assert!(s.contains("Push the feature branch"));
    assert!(s.contains("Open a PR"));
    assert!(s.contains("Pass the PR gate"));
    assert!(s.contains("CI checks"));
    assert!(s.contains("Merge the PR"));
    assert!(!s.contains("direct push to the protected branch:\n\n1. Merge"));
}
