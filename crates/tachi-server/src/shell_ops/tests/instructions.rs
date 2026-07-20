use super::*;

#[test]
fn slugify_basic() {
    assert_eq!(slugify("Hello, World!"), "hello-world");
    assert_eq!(slugify("   "), "flow");
    assert_eq!(slugify("已经-OK_test"), "ok-test");
}

#[test]
fn meta_skill_mapping_is_complete() {
    assert!(meta_skill_for_stage("dispatch").is_some());
    for action in ["brainstorm", "plan", "review", "ship", "kanban", "status"] {
        assert!(meta_skill_for_stage(action).is_none(), "action {action}");
    }
}

#[test]
fn superpowers_meta_skills_resolve_for_all_shell_stages() {
    // The resolver reads `TACHI_SKILLS_ROOT`; share the same env lock as its
    // neutral-module tests so their mutations can never pollute this read.
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let rel = meta_skill_for_stage("dispatch").expect("mapped dispatch action");
    let resolved = crate::skill_source_resolver::resolve_vendored_skill_path(rel)
        .unwrap_or_else(|| panic!("superpowers skill not found for dispatch at {rel}"));
    assert!(resolved.ends_with("SKILL.md"));
    let content = std::fs::read_to_string(&resolved).expect("read dispatch superpowers skill");
    assert!(content.contains("name:") || content.starts_with("# "));
}

#[test]
fn build_instruction_includes_required_sections() {
    let inj = InjectionResult {
        required: true,
        rel_path: Some("skill/x/SKILL.md".into()),
        source_path: Some("/abs/skill/x/SKILL.md".into()),
        injected_path: Some(".tachi/runs/flow_x/injected/superpowers-dispatch.md".into()),
        content_hash: Some("a".repeat(16)),
        loaded: true,
        warning: None,
        failure_class: None,
    };
    let s = build_instruction_md(
        "flow_x",
        "dispatch",
        "do the thing",
        &inj,
        Some("be careful"),
        &["cargo test".to_string()],
        &["crates/tachi-server/**".to_string()],
    );
    assert!(s.contains("flow_x"));
    assert!(s.contains("Stage: **dispatch**"));
    assert!(s.contains("do the thing"));
    assert!(s.contains("superpowers-dispatch.md"));
    assert!(s.contains("## Native Lifecycle Policy"));
    assert!(s.contains("skill:superpowers-executing-plans"));
    assert!(s.contains("max 6 concurrent workers"));
    assert!(s.contains("cargo test"));
    assert!(s.contains("crates/tachi-server/**"));
    assert!(s.contains("be careful"));
}
