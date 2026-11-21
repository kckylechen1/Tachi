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
    let raw = rusqlite::Connection::open(&path).unwrap();
    assert!(raw.execute("DELETE FROM typed_records", []).is_err());
    drop(raw);
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
        "../../../../docs/engineering/architecture/hypermem-compatibility-gate.fixture.json"
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

fn host_kind(name: &str, version: u32) -> RecordKind {
    RecordKind {
        id: RecordKindId {
            name: name.into(),
            version,
        },
        key_schema: json!({"type":"object","properties":{"agent_id":{}, "ended_at":{}},
            "required":["agent_id"],"additionalProperties":false}),
        payload_schema: json!({"type":"string"}),
    }
}

fn append(
    store: &mut MemoryStore,
    writer: &RecordWriter,
    kind: &RecordKindId,
    id: &str,
    keys: serde_json::Value,
    predecessor: Option<&str>,
) -> portable_kernel::TypedRecordWrite {
    store
        .append_typed_record(
            writer,
            &TypedRecordInput {
                id: id.into(),
                kind: kind.clone(),
                keys,
                payload: json!("private payload"),
                links: vec![],
            },
            predecessor,
        )
        .unwrap()
}

#[test]
fn scoped_purge_removes_only_matching_rows_and_incident_edges_in_one_commit() {
    use std::collections::BTreeMap;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("purge.db");
    let old = host_kind("episode", 1);
    let new = host_kind("episode", 2);
    let writer =
        RecordWriter::from_host_config("host", "personal", vec![old.id.clone(), new.id.clone()])
            .unwrap();
    let mut store = open(&path, "personal", true, vec![old.clone(), new.clone()]);
    let a = json!({"agent_id":"a"});
    let b = json!({"agent_id":"b"});
    append(&mut store, &writer, &old.id, "b-first", b.clone(), None);
    append(
        &mut store,
        &writer,
        &old.id,
        "b-old",
        b.clone(),
        Some("b-first"),
    );
    append(
        &mut store,
        &writer,
        &new.id,
        "a-one",
        a.clone(),
        Some("b-old"),
    );
    let receipt = append(&mut store, &writer, &new.id, "a-two", a, Some("a-one"))
        .receipt
        .unwrap();
    let retained = append(&mut store, &writer, &new.id, "b-new", b, Some("a-two")).record;
    let raw = rusqlite::Connection::open(&path).unwrap();
    portable_kernel::db::ensure_reserved_reference_write_guard(&raw).unwrap();
    for (table, update, message) in [
        (
            "typed_records",
            "revision=revision",
            "typed record revisions are immutable",
        ),
        (
            "typed_record_supersessions",
            "target_id=target_id",
            "typed record lineage is immutable",
        ),
    ] {
        for sql in [
            format!("DELETE FROM {table}"),
            format!("UPDATE {table} SET {update}"),
        ] {
            let err = raw.execute(&sql, []).unwrap_err();
            assert_eq!(err.to_string(), message, "{sql}");
        }
    }
    let scope = BTreeMap::from([("agent_id".into(), "a".into())]);
    assert!(store
        .purge_typed_records(&writer, &BTreeMap::new())
        .is_err());
    assert!(store
        .purge_typed_records(&writer, &BTreeMap::from([("agent.id".into(), "a".into())]))
        .is_err());
    for denied in [
        RecordWriter::from_host_config("host", "other", vec![old.id.clone(), new.id.clone()])
            .unwrap(),
        RecordWriter::from_host_config("host", "personal", vec![new.id.clone()]).unwrap(),
    ] {
        assert!(store.purge_typed_records(&denied, &scope).is_err());
    }
    // Fail the record delete after lineage removal has run.
    raw.execute_batch(
        "CREATE TRIGGER stop_purge BEFORE DELETE ON typed_records
                       BEGIN SELECT RAISE(ABORT, 'test rollback'); END;",
    )
    .unwrap();
    assert!(store
        .purge_typed_records(&writer, &scope)
        .unwrap_err()
        .to_string()
        .contains("test rollback"));
    assert_eq!(
        store
            .get_typed_record("a-one")
            .unwrap()
            .unwrap()
            .superseded_by
            .as_deref(),
        Some("a-two")
    );
    raw.execute_batch("DROP TRIGGER stop_purge").unwrap();
    let result = store.purge_typed_records(&writer, &scope).unwrap();
    assert_eq!((result.erased_records, result.erased_lineage), (2, 3));
    for erased in ["a-one", "a-two"] {
        assert!(store.get_typed_record(erased).unwrap().is_none());
    }
    assert!(store
        .get_typed_record("b-old")
        .unwrap()
        .unwrap()
        .superseded_by
        .is_none());
    assert_eq!(
        store
            .get_typed_record("b-first")
            .unwrap()
            .unwrap()
            .superseded_by
            .as_deref(),
        Some("b-old")
    );
    assert_eq!(store.get_typed_record("b-new").unwrap().unwrap(), retained);
    assert_eq!(
        raw.query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |row| {
            row.get::<_, i64>(0)
        })
        .unwrap(),
        0
    );
    let evidence: String = raw.query_row("SELECT value_json FROM hard_state WHERE namespace='memory_supersession_receipts' AND key=?1",
        [&receipt.receipt_id], |row| row.get(0)).unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&evidence).unwrap(),
        serde_json::to_value(receipt).unwrap()
    );
    assert!(!evidence.contains("private payload"));
}

#[test]
fn time_window_filters_and_verifies_the_selected_revisions() {
    use portable_kernel::TypedRecordQuery;
    use std::collections::BTreeMap;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("list.db");
    let kind = host_kind("episode", 1);
    let other = host_kind("episode", 2);
    let writer =
        RecordWriter::from_host_config("host", "personal", vec![kind.id.clone(), other.id.clone()])
            .unwrap();
    let mut store = open(&path, "personal", true, vec![kind.clone(), other.clone()]);
    let keys = |agent: &str, time: &str| json!({"agent_id":agent,"ended_at":time});
    let first = append(
        &mut store,
        &writer,
        &kind.id,
        "predecessor",
        keys("a", "2026-10-31T23:59:59.999999Z"),
        None,
    );
    for (id, agent, time, version, previous) in [
        (
            "latest",
            "a",
            "2026-11-01T01:00:00.000001+02:00",
            &kind.id,
            Some("predecessor"),
        ),
        ("day", "a", "2026-10-31", &kind.id, None),
        ("below", "a", "2026-10-30", &kind.id, None),
        ("wrong-agent", "b", "2026-11-01", &kind.id, None),
        ("wrong-version", "a", "2026-11-01", &other.id, None),
        ("above", "a", "2026-11-02", &kind.id, None),
    ] {
        append(
            &mut store,
            &writer,
            version,
            id,
            keys(agent, time),
            previous,
        );
    }
    let mut query = TypedRecordQuery {
        kind: Some(kind.id.clone()),
        key_filters: BTreeMap::from([("agent_id".into(), "a".into())]),
        time_key: Some("ended_at".into()),
        since: Some("2026-10-31".into()),
        until: Some("2026-11-01".into()),
        limit: 1,
        ..Default::default()
    };
    let list = |q: &TypedRecordQuery| store.list_typed_records(q).unwrap();
    assert_eq!(list(&query)[0].id, "latest");
    query.include_superseded = true;
    let rows = list(&query);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].superseded_by.as_deref(), Some("latest"));
    assert_eq!(rows[0].id, "predecessor");
    query.limit = 10;
    query.until = Some("2026-10-31T23:00:00.000000Z".into());
    assert_eq!(
        list(&query)
            .iter()
            .map(|r| r.id.as_str())
            .collect::<Vec<_>>(),
        ["day"]
    );
    query.time_key = None;
    query.since = Some(first.record.created_at.clone());
    query.until = query.since.clone();
    let rows = list(&query);
    assert!(rows.iter().any(|row| row.id == "predecessor"));
    assert!(rows
        .iter()
        .all(|row| row.created_at == first.record.created_at));

    for invalid in [
        TypedRecordQuery {
            time_key: Some("a.b".into()),
            ..Default::default()
        },
        TypedRecordQuery {
            since: Some("yesterday".into()),
            ..Default::default()
        },
        TypedRecordQuery {
            limit: 0,
            ..Default::default()
        },
        TypedRecordQuery {
            limit: usize::MAX,
            ..Default::default()
        },
    ] {
        assert!(store.list_typed_records(&invalid).is_err());
    }
    let raw = rusqlite::Connection::open(&path).unwrap();
    raw.execute_batch(
        "DROP TRIGGER typed_records_no_update;
                      UPDATE typed_records SET payload_json='null' WHERE id='day'",
    )
    .unwrap();
    let error = store
        .list_typed_records(&TypedRecordQuery::default())
        .unwrap_err();
    assert!(error.to_string().contains("content digest"));
}
