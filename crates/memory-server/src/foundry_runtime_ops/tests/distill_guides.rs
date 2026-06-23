use super::*;
fn distill_source_entry(id: &str, text: &str, category: &str) -> MemoryEntry {
    MemoryEntry {
        id: id.to_string(),
        path: "/project/tachi/crates/memory-server/src/tools.rs".to_string(),
        summary: text.chars().take(40).collect(),
        text: text.to_string(),
        importance: 0.7,
        timestamp: "2026-01-01T00:00:00Z".to_string(),
        valid_from: String::new(),
        valid_until: None,
        category: category.to_string(),
        topic: "guide-layer".to_string(),
        keywords: vec!["tachi".to_string()],
        persons: vec![],
        entities: vec!["Tachi".to_string()],
        location: String::new(),
        source: "manual".to_string(),
        scope: "project".to_string(),
        archived: false,
        access_count: 0,
        last_access: None,
        revision: 1,
        metadata: json!({
            "file_path": "crates/memory-server/src/tools.rs"
        }),
        vector: None,
        retention_policy: None,
        domain: None,
        recall_count: 0,
        query_diversity: 0,
        tier: "raw".to_string(),
    }
}

#[test]
fn distill_guide_classifier_emits_supported_guide_types() {
    let source = vec![distill_source_entry("src", "context", "fact")];
    assert_eq!(
        classify_distill_guide_type("Must not add a normal guide write tool.", &source),
        "constraint"
    );
    assert_eq!(
        classify_distill_guide_type("Fix linker error by rebuilding sqlite vec.", &source),
        "fix_pattern"
    );
    assert_eq!(
        classify_distill_guide_type(
            "Decision: choose metadata fields over schema changes.",
            &source
        ),
        "decision"
    );
    assert_eq!(
        classify_distill_guide_type("Runbook: 1. Inspect logs\n2. Re-run cargo test.", &source),
        "runbook"
    );
}

#[test]
fn distill_edges_include_causal_guide_relations() {
    let sources = vec![distill_source_entry(
        "source-1",
        "error: linker failed for sqlite vec",
        "fact",
    )];
    let guide = MemoryEntry {
        id: "guide-1".to_string(),
        path: "/guide/fix_pattern/codex/20260101T000000".to_string(),
        summary: "Fix linker errors".to_string(),
        text: "Fix linker error by rebuilding sqlite vec; avoid deleting migrations.".to_string(),
        importance: 0.75,
        timestamp: "2026-01-01T00:00:00Z".to_string(),
        valid_from: String::new(),
        valid_until: None,
        category: "guide".to_string(),
        topic: "fix_pattern".to_string(),
        keywords: vec!["guide".to_string(), "fix_pattern".to_string()],
        persons: vec![],
        entities: vec!["Tachi".to_string()],
        location: String::new(),
        source: FOUNDRY_DISTILL_SOURCE.to_string(),
        scope: "project".to_string(),
        archived: false,
        access_count: 0,
        last_access: None,
        revision: 1,
        metadata: json!({
            "guide": true,
            "guide_type": "fix_pattern",
        }),
        vector: None,
        retention_policy: None,
        domain: None,
        recall_count: 0,
        query_diversity: 0,
        tier: "raw".to_string(),
    };

    let relations = build_distill_edges(&guide, &sources, "fix_pattern", &guide.timestamp)
        .into_iter()
        .map(|edge| edge.relation)
        .collect::<std::collections::HashSet<_>>();
    assert!(relations.contains("distilled_from"));
    assert!(relations.contains("fixed_by"));
    assert!(relations.contains("rejected_because"));

    let constraint_relations =
        build_distill_edges(&guide, &sources, "constraint", &guide.timestamp)
            .into_iter()
            .map(|edge| edge.relation)
            .collect::<std::collections::HashSet<_>>();
    assert!(constraint_relations.contains("causes"));
}
