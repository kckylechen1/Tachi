//! The same portable implementation accepts independent host vocabularies.
use std::sync::Arc;

use portable_kernel::{
    DbOpenContext, KernelPolicy, MemoryStore, RecordKind, RecordKindId, RecordRegistry,
    RecordWriter, StoreProfile, TypedRecordInput, ADMIN_SURFACE_ENABLED,
};
use serde_json::json;

fn open(path: &std::path::Path, role: &str, fresh: bool, kinds: Vec<RecordKind>) -> MemoryStore {
    let context = if fresh {
        DbOpenContext::create_fresh()
    } else {
        DbOpenContext::open_existing_deny()
    }
    .with_exact_profile(StoreProfile::PortableKernel)
    .with_exact_role(role);
    let mut store = MemoryStore::open_with_context(path.to_str().unwrap(), &context)
        .unwrap()
        .with_kernel_policy(KernelPolicy {
            record_registry: Some(Arc::new(RecordRegistry::new(kinds).unwrap())),
            ..Default::default()
        });
    store.register_record_kinds().unwrap();
    store
}

#[test]
fn a_personal_host_can_define_its_own_preference_without_product_semantics() {
    #[allow(clippy::assertions_on_constants)]
    {
        assert!(!ADMIN_SURFACE_ENABLED);
    }
    let kind = RecordKind {
        id: RecordKindId {
            name: "assistant.preference".into(),
            version: 1,
        },
        key_schema: json!({"type":"object","properties":{"subject":{"type":"string"}},
            "required":["subject"],"additionalProperties":false}),
        payload_schema: json!({"type":"object","properties":{"response_style":{"enum":["concise","detailed"]}},
            "required":["response_style"],"additionalProperties":false}),
    };
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("personal.db");
    let mut store = open(&path, "personal", true, vec![kind.clone()]);
    let writer =
        RecordWriter::from_host_config("profile-manager", "personal", vec![kind.id.clone()])
            .unwrap();
    let original = TypedRecordInput {
        id: "preference-1".into(),
        kind: kind.id.clone(),
        keys: json!({"subject":"user"}),
        payload: json!({"response_style":"concise"}),
        links: vec![],
    };
    let first = store.append_typed_record(&writer, &original, None).unwrap();
    let mut correction = original.clone();
    correction.id = "preference-2".into();
    correction.payload = json!({"response_style":"detailed"});
    let second = store
        .append_typed_record(&writer, &correction, Some(&original.id))
        .unwrap();
    assert_eq!(second.record.revision, 2);
    assert!(second.receipt.as_ref().unwrap().durable);
    assert!(
        store
            .append_typed_record(&writer, &correction, Some(&original.id))
            .unwrap()
            .prior_idempotent
    );
    assert!(store
        .connection()
        .execute("DELETE FROM typed_records", [])
        .is_err());
    drop(store);
    let reopened = open(&path, "personal", false, vec![kind]);
    let history = reopened.get_typed_record(&original.id).unwrap().unwrap();
    assert_eq!(history.payload, first.record.payload);
    assert_eq!(history.content_sha256, first.record.content_sha256);
    assert_eq!(history.superseded_by.as_deref(), Some("preference-2"));
}

#[test]
fn hypermem_gate_fixture_round_trips_through_the_real_portable_kernel() {
    let fixture: serde_json::Value = serde_json::from_str(include_str!(
        "../../../docs/engineering/architecture/hypermem-compatibility-gate.fixture.json"
    ))
    .unwrap();
    let fixture = &fixture["host_typed_records"];
    assert_eq!(fixture["kernel_issue"], 2105);
    assert_eq!(fixture["consumer_issue"], 3564);
    let kinds: Vec<RecordKind> = serde_json::from_value(fixture["kinds"].clone()).unwrap();
    let records: Vec<TypedRecordInput> =
        serde_json::from_value(fixture["records"].clone()).unwrap();
    assert_eq!(records.len(), 5);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("host.db");
    let role = fixture["role"].as_str().unwrap();
    let mut store = open(&path, role, true, kinds.clone());
    // This broad grant is a fixture host, never a production caller default.
    let fixture_writer = RecordWriter::from_host_config(
        "compatibility-fixture",
        role,
        records.iter().map(|record| record.kind.clone()).collect(),
    )
    .unwrap();
    for record in &records {
        let written = store
            .append_typed_record(&fixture_writer, record, None)
            .unwrap();
        assert_eq!(
            store.get_typed_record(&record.id).unwrap(),
            Some(written.record)
        );
    }
    let copilot = RecordWriter::from_host_config(
        "zeroclaw-copilot",
        role,
        vec![
            RecordKindId {
                name: "challenge".into(),
                version: 1,
            },
            RecordKindId {
                name: "decision".into(),
                version: 1,
            },
        ],
    )
    .unwrap();
    let mut forbidden = records
        .iter()
        .find(|record| record.kind.name == "outcome")
        .unwrap()
        .clone();
    forbidden.id = "forbidden-outcome".into();
    assert!(store
        .append_typed_record(&copilot, &forbidden, None)
        .is_err());
    assert!(store.get_typed_record(&forbidden.id).unwrap().is_none());
    let mut invalid = records
        .iter()
        .find(|record| record.kind.name == "challenge")
        .unwrap()
        .clone();
    invalid.id = "wrong-payload".into();
    invalid.payload = json!({"position_size":100});
    assert!(store.append_typed_record(&copilot, &invalid, None).is_err());
    drop(store);
    let reopened = open(&path, role, false, kinds);
    for record in records {
        let stored = reopened.get_typed_record(&record.id).unwrap().unwrap();
        assert_eq!(stored.kind, record.kind);
        assert_eq!(stored.keys, record.keys);
        assert_eq!(stored.payload, record.payload);
        assert_eq!(stored.links, record.links);
    }
}
