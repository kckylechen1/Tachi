use super::*;
use std::collections::BTreeSet;
use std::path::PathBuf;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|path| path.parent())
        .expect("memory-server lives under crates/memory-server")
        .to_path_buf()
}

fn kernel_surface_doc() -> String {
    fs::read_to_string(repo_root().join("docs/engineering/architecture/kernel-surface-v1.md"))
        .expect("read kernel surface doc")
}

fn kernel_surface_fixture() -> serde_json::Value {
    serde_json::from_str(include_str!(
        "../../../../../docs/engineering/architecture/kernel-surface-v1.fixture.json"
    ))
    .expect("kernel surface fixture parses")
}

#[test]
fn kernel_surface_fixture_covers_portable_memory_contract() {
    let fixture = kernel_surface_fixture();
    assert_eq!(fixture["issue"], json!(790));

    let required_surfaces = fixture["portable_bundle"]["required_surfaces"]
        .as_array()
        .expect("required surfaces")
        .iter()
        .map(|item| item.as_str().expect("surface string"))
        .collect::<BTreeSet<_>>();
    for required in [
        "memory-core",
        "memory_db_schema_contracts",
        "recall_scorer_rrf",
        "vector_backfill",
        "event_ledger_projection",
        "library_identity_routing",
        "status_readiness",
    ] {
        assert!(
            required_surfaces.contains(required),
            "missing portable surface {required}"
        );
    }

    let classes = fixture["surface_classes"]
        .as_array()
        .expect("surface classes")
        .iter()
        .map(|item| item["class"].as_str().expect("class string"))
        .collect::<BTreeSet<_>>();
    for required in ["kernel", "runtime_adapter", "workflow", "admin", "product"] {
        assert!(classes.contains(required), "missing class {required}");
    }

    let operations = fixture["backend_boundary"]
        .as_array()
        .expect("backend boundary")
        .iter()
        .map(|item| item["operation"].as_str().expect("operation string"))
        .collect::<BTreeSet<_>>();
    for required in [
        "status_readiness",
        "search_recall",
        "save_retain",
        "developer_briefing_context",
        "readiness_diagnostics",
        "lifecycle_hooks",
    ] {
        assert!(
            operations.contains(required),
            "missing operation {required}"
        );
    }
    let lifecycle = fixture["backend_boundary"]
        .as_array()
        .expect("backend boundary")
        .iter()
        .find(|item| item["operation"] == json!("lifecycle_hooks"))
        .expect("lifecycle hooks boundary");
    assert_eq!(lifecycle["implementation_status"], json!("target_proposed"));
    assert_eq!(lifecycle["required"], json!(false));
    assert!(lifecycle["current_bindings"]
        .as_array()
        .expect("current bindings")
        .contains(&json!("memory.saved")));
    for proposed in [
        "retain_requested",
        "recall_requested",
        "recall_returned",
        "reflect_requested",
        "reflection_returned",
    ] {
        assert!(
            lifecycle["target_terms"]
                .as_array()
                .expect("target terms")
                .contains(&json!(proposed)),
            "missing explicit proposed lifecycle term {proposed}"
        );
    }

    let ontology = fixture["ontology"]
        .as_array()
        .expect("ontology")
        .iter()
        .map(|item| item["name"].as_str().expect("ontology name"))
        .collect::<BTreeSet<_>>();
    for required in [
        "world_fact",
        "experience",
        "opinion_preference",
        "observation_summary",
    ] {
        assert!(ontology.contains(required), "missing ontology {required}");
    }
}

#[test]
fn portable_kernel_manifest_does_not_require_product_surfaces() {
    let fixture = kernel_surface_fixture();
    let required_surfaces = fixture["portable_bundle"]["required_surfaces"]
        .as_array()
        .expect("required surfaces")
        .iter()
        .map(|item| item.as_str().expect("surface string"))
        .collect::<BTreeSet<_>>();

    for forbidden in fixture["portable_bundle"]["excluded_surfaces"]
        .as_array()
        .expect("excluded surfaces")
    {
        let forbidden = forbidden.as_str().expect("forbidden surface string");
        assert!(
            !required_surfaces.contains(forbidden),
            "portable kernel must not require product/operator surface {forbidden}"
        );
    }
}

#[test]
fn kernel_surface_doc_names_evidence_and_decision_rule() {
    let doc = kernel_surface_doc();

    assert!(doc.contains("### Portable Memory Kernel Manifest"));
    assert!(doc.contains("### Backend Boundary"));
    assert!(doc.contains("### Memory Ontology"));
    assert!(doc.contains("OMP's\n`MemoryBackend`"));
    assert!(doc.contains("Hindsight's four-network ontology"));
    assert!(doc.contains("It explicitly excludes GitHub, dispatch, ship, release"));
    assert!(doc.contains("Downstream\nagents should cite that fixture"));
    assert!(doc.contains("This is a target contract"));
    assert!(doc.contains("`memory.saved` exists today"));
}
