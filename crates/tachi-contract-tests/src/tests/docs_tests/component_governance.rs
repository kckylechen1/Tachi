use serde_json::json;
use std::collections::BTreeSet;

const REQUIRED_RECORD_FIELDS: &[&str] = &[
    "component_id",
    "component_type",
    "owner_repo",
    "owner_path",
    "contract_summary",
    "source_refs",
    "upstream_prereqs",
    "allowed_variation",
    "forbidden_variation",
    "downstream_consumers",
    "known_drift",
    "backflow_candidates",
    "last_verified_at",
    "last_checked_ref",
];

fn fixture() -> serde_json::Value {
    serde_json::from_str(include_str!(
        "../../../../../docs/engineering/architecture/component-governance-v0.fixture.json"
    ))
    .expect("component governance fixture parses")
}

fn string_set(value: &serde_json::Value, field: &str) -> BTreeSet<String> {
    value[field]
        .as_array()
        .unwrap_or_else(|| panic!("{field} must be an array"))
        .iter()
        .map(|item| {
            item.as_str()
                .unwrap_or_else(|| panic!("{field} item must be a string"))
        })
        .map(str::to_string)
        .collect()
}

#[test]
fn component_governance_fixture_defines_v0_schema_and_seed_records() {
    let fixture = fixture();
    assert_eq!(fixture["schema_version"], json!("component_governance.v0"));
    assert_eq!(fixture["issue"], json!(795));

    let required_fields = string_set(&fixture, "required_record_fields");
    assert_eq!(
        required_fields,
        REQUIRED_RECORD_FIELDS
            .iter()
            .map(|field| field.to_string())
            .collect::<BTreeSet<_>>()
    );

    let records = fixture["records"].as_array().expect("records array");
    let component_ids = records
        .iter()
        .map(|record| {
            record["component_id"]
                .as_str()
                .expect("component_id string")
        })
        .collect::<BTreeSet<_>>();
    assert_eq!(
        component_ids,
        [
            "hypermemory-trading-adapter",
            "romanbath-frontend-app-shell",
            "tachi-event-projection-bridge",
            "tachi-memory-kernel",
            "zeroclaw-chat-memory-adapter",
        ]
        .into_iter()
        .collect::<BTreeSet<_>>()
    );

    for record in records {
        for field in REQUIRED_RECORD_FIELDS {
            assert!(
                record.get(*field).is_some(),
                "{} is missing required field {field}",
                record["component_id"]
            );
        }
        chrono::DateTime::parse_from_rfc3339(
            record["last_verified_at"]
                .as_str()
                .expect("last_verified_at string"),
        )
        .expect("last_verified_at is RFC3339");
        assert!(
            record["last_checked_ref"]
                .as_str()
                .expect("last_checked_ref string")
                .len()
                >= 7,
            "last_checked_ref should name a concrete ref"
        );
    }
}

#[test]
fn component_governance_fixture_enforces_enums_and_non_goals() {
    let fixture = fixture();
    let component_types = string_set(&fixture, "component_type_enum");
    let drift_classes = string_set(&fixture, "drift_classification_enum");

    assert_eq!(
        component_types,
        [
            "frontend_app_shell",
            "kernel",
            "runtime_adapter",
            "workflow_bridge",
        ]
        .into_iter()
        .map(str::to_string)
        .collect::<BTreeSet<_>>()
    );
    assert_eq!(
        drift_classes,
        [
            "accepted_local_policy",
            "backflow_candidate",
            "blocked_fork",
            "none",
            "retire_delete",
            "unknown",
        ]
        .into_iter()
        .map(str::to_string)
        .collect::<BTreeSet<_>>()
    );

    for record in fixture["records"].as_array().expect("records array") {
        let component_type = record["component_type"]
            .as_str()
            .expect("component_type string");
        assert!(
            component_types.contains(component_type),
            "unexpected component_type {component_type}"
        );

        for drift in record["known_drift"].as_array().expect("known_drift array") {
            let class = drift["classification"]
                .as_str()
                .expect("known_drift classification string");
            assert!(
                drift_classes.contains(class),
                "unexpected drift classification {class}"
            );
        }
    }

    let non_goals = string_set(&fixture, "non_goals");
    for forbidden in [
        "repo_scanning",
        "cli_or_mcp_surface",
        "cutover_planner",
        "auto_sync",
    ] {
        assert!(
            non_goals.contains(forbidden),
            "missing non-goal {forbidden}"
        );
    }
}
