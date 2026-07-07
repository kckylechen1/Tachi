use super::*;
use std::collections::BTreeSet;

#[test]
fn hypermem_compatibility_gate_fixture_covers_issue_793_contract() {
    let fixture: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../../docs/engineering/architecture/hypermem-compatibility-gate.fixture.json"
    ))
    .expect("hypermem compatibility gate fixture parses");

    assert_eq!(fixture["issue"], json!(793));
    assert_eq!(fixture["direct_reader_decision"], json!("shim_then_retire"));

    let checklist = fixture["checklist"]
        .as_array()
        .expect("checklist is an array");
    let checklist_areas = checklist
        .iter()
        .map(|item| item["area"].as_str().expect("checklist area"))
        .collect::<BTreeSet<_>>();
    for required in [
        "hypermemory_aliases",
        "hapi_trading_harness_bridge",
        "direct_memories_table_reader",
        "trading_metadata_categories",
        "a_share_decay_policy",
    ] {
        assert!(
            checklist_areas.contains(required),
            "missing checklist area {required}"
        );
    }

    let allowed_classes = [
        "allowed_adapter_policy",
        "upstream_backflow_candidate",
        "delete_retire",
    ]
    .into_iter()
    .collect::<BTreeSet<_>>();
    for item in fixture["difference_classifications"]
        .as_array()
        .expect("difference classifications")
    {
        let class = item["classification"]
            .as_str()
            .expect("classification string");
        assert!(
            allowed_classes.contains(class),
            "unexpected classification {class}"
        );
    }

    let evidence = fixture["minimum_recall_quality_evidence"]
        .as_array()
        .expect("minimum evidence array")
        .iter()
        .map(|item| item.as_str().expect("evidence string"))
        .collect::<BTreeSet<_>>();
    for required in [
        "score_stability",
        "vector_fts_fallback_behavior",
        "domain_metadata_preservation",
        "trading_memory_provenance",
    ] {
        assert!(evidence.contains(required), "missing evidence {required}");
    }

    assert_eq!(
        fixture["extension_points"]["domain_scorer"]["forks_kernel"],
        json!(false)
    );
    assert_eq!(
        fixture["extension_points"]["decay_policy"]["forks_kernel"],
        json!(false)
    );
    assert!(
        fixture["extension_points"]["decay_policy"]["references_only"]
            .as_array()
            .expect("references_only")
            .iter()
            .any(|item| item == "omp_type_specific_decay")
    );
    assert!(
        fixture["extension_points"]["decay_policy"]["references_only"]
            .as_array()
            .expect("references_only")
            .iter()
            .any(|item| item == "hindsight_confidence_reinforcement")
    );
}

#[test]
fn hypermem_compatibility_gate_has_no_operator_surface_dependency() {
    let fixture: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../../docs/engineering/architecture/hypermem-compatibility-gate.fixture.json"
    ))
    .expect("hypermem compatibility gate fixture parses");

    let required_surfaces = fixture["required_surfaces"]
        .as_array()
        .expect("required surfaces")
        .iter()
        .map(|item| item.as_str().expect("surface string"))
        .collect::<BTreeSet<_>>();

    for forbidden in fixture["forbidden_required_surfaces"]
        .as_array()
        .expect("forbidden surfaces")
    {
        let forbidden = forbidden.as_str().expect("forbidden surface string");
        assert!(
            !required_surfaces.contains(forbidden),
            "gate must not require operator surface {forbidden}"
        );
    }

    assert_eq!(
        required_surfaces,
        ["fixture_json", "local_docs", "memory_kernel_api"]
            .into_iter()
            .collect::<BTreeSet<_>>()
    );
}
