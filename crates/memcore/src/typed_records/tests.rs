use super::*;
use crate::{DbOpenContext, KernelPolicy, MemoryStore, StoreProfile};
use rusqlite::Connection;
use serde_json::json;

fn kind(name: &str) -> RecordKind {
    RecordKind {
        id: RecordKindId { name: name.into(), version: 1 },
        key_schema: json!({"type":"object","properties":{"subject":{"type":"string"}},
            "required":["subject"],"additionalProperties":false}),
        payload_schema: json!({"type":"object","properties":{"summary":{"type":"string"}},
            "required":["summary"],"additionalProperties":false}),
    }
}

fn registry() -> RecordRegistry {
    RecordRegistry::new(vec![kind("observation"), kind("annotation")]).unwrap()
}

fn writer(role: &str, name: &str) -> RecordWriter {
    RecordWriter::from_host_config("host.agent", role, vec![kind(name).id]).unwrap()
}

fn input(id: &str) -> TypedRecordInput {
    TypedRecordInput {
        id: id.into(), kind: kind("observation").id,
        keys: json!({"subject":"agent-a"}), payload: json!({"summary":id}), links: vec![],
    }
}

fn open(path: &std::path::Path, fresh: bool, role: &str, registry: RecordRegistry) -> MemoryStore {
    let ctx = if fresh { DbOpenContext::create_fresh() } else { DbOpenContext::open_existing_deny() }
        .with_exact_profile(StoreProfile::PortableKernel).with_exact_role(role);
    let policy = KernelPolicy { record_registry: Some(Arc::new(registry)), ..Default::default() };
    MemoryStore::open_with_context(path.to_str().unwrap(), &ctx).unwrap().with_kernel_policy(policy)
}

#[test]
fn append_supersede_replay_preserves_original_row_and_receipt() {
    let dir = tempfile::tempdir().unwrap(); let path = dir.path().join("records.db");
    let mut store = open(&path, true, "assistant", registry());
    let writer = writer("assistant", "observation");
    let first = store.append_typed_record(&writer, &input("r1"), None).unwrap();
    let second = store.append_typed_record(&writer, &input("r2"), Some("r1")).unwrap();
    assert_eq!(second.record.revision, 2);
    let source = store.get_typed_record("r1").unwrap().unwrap();
    assert_eq!(source.payload, first.record.payload);
    assert_eq!(source.content_sha256, first.record.content_sha256);
    assert_eq!(source.revision, 1);
    assert_eq!(source.superseded_by.as_deref(), Some("r2"));
    let receipt = second.receipt.unwrap();
    assert_eq!(receipt.source_revision_before, receipt.source_revision_after);
    assert_eq!(receipt.source_superseded_by_after.as_deref(), Some("r2"));
    assert!(receipt.durable);
    let replay = store.append_typed_record(&writer, &input("r2"), Some("r1")).unwrap();
    assert!(replay.prior_idempotent);
    assert_eq!(replay.receipt.as_ref(), Some(&receipt));
    assert!(store.append_typed_record(&writer, &input("r3"), Some("r1")).is_err());
    assert!(store.get_typed_record("r3").unwrap().is_none());
    // Link keys may be shared by separate observations, including revisions.
    assert!(store.append_typed_record(&writer, &input("independent"), None).is_ok());
    drop(store);
    let mut reopened = open(&path, false, "assistant", registry());
    reopened.register_record_kinds().unwrap();
    assert_eq!(reopened.get_typed_record("r1").unwrap().unwrap(), source);
}

#[test]
fn raw_sql_update_delete_and_replace_refuse_even_with_typed_authority() {
    let dir = tempfile::tempdir().unwrap(); let path = dir.path().join("immutable.db");
    let mut store = open(&path, true, "assistant", registry());
    store.append_typed_record(&writer("assistant","observation"), &input("r1"), None).unwrap();
    let raw = Connection::open(&path).unwrap();
    for conn in [&raw, &store.conn] {
        for sql in ["UPDATE typed_records SET payload_json='{}'", "DELETE FROM typed_records",
            "INSERT OR REPLACE INTO typed_records SELECT * FROM typed_records"] {
            assert!(conn.execute(sql, []).is_err(), "{sql}");
        }
    }
    let _authority = crate::db::authorize_reserved_reference_write(&store.reserved_reference_write).unwrap();
    for sql in ["UPDATE typed_records SET writer='forged'", "DELETE FROM typed_records",
        "INSERT OR REPLACE INTO typed_records SELECT * FROM typed_records"] {
        assert!(store.conn.execute(sql, []).is_err(), "{sql}");
    }
}

#[test]
fn payload_keys_kind_and_writer_refusals_leave_no_rows() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = open(&dir.path().join("refusals.db"), true, "assistant", registry());
    let allowed = writer("assistant","observation");
    let mut attempts = vec![];
    let mut bad = input("missing"); bad.payload=json!({}); attempts.push(bad);
    let mut bad = input("wrong-type"); bad.payload=json!({"summary":3}); attempts.push(bad);
    let mut bad = input("unknown-key"); bad.keys=json!({"subject":"a","owner":true}); attempts.push(bad);
    let mut bad = input("unknown-version"); bad.kind.version=2; attempts.push(bad);
    let mut bad = input("unknown-kind"); bad.kind.name="unknown".into(); attempts.push(bad);
    for bad in attempts {
        assert!(store.append_typed_record(&allowed,&bad,None).is_err());
        assert!(store.get_typed_record(&bad.id).unwrap().is_none());
    }
    for denied in [writer("assistant","annotation"),writer("different","observation")] {
        assert!(store.append_typed_record(&denied,&input("denied"),None).is_err());
    }
    assert!(store.get_typed_record("denied").unwrap().is_none());
    let count:i64=store.conn.query_row("SELECT COUNT(*) FROM typed_records",[],|r|r.get(0)).unwrap();
    assert_eq!(count,0);
}

#[test]
fn local_links_bind_the_exact_kind_keys_and_digest_and_ontology() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = open(&dir.path().join("links.db"), true, "assistant", registry());
    let allowed = writer("assistant","observation");
    let target=store.append_typed_record(&allowed,&input("target"),None).unwrap().record;
    let mut linked=input("linked");
    linked.links.push(TypedRecordLink { relation:"references".into(),target:TypedRecordTarget::Local {
        record_id:target.id.clone(), kind:target.kind.clone(), keys:target.keys.clone(),
        content_sha256:target.content_sha256.clone(),
    }});
    store.append_typed_record(&allowed,&linked,None).unwrap();
    let mut bad=linked.clone(); bad.id="bad-digest".into();
    if let TypedRecordTarget::Local { content_sha256,.. }=&mut bad.links[0].target { *content_sha256="0".repeat(64); }
    assert!(store.append_typed_record(&allowed,&bad,None).is_err());
    for relation in ["invented", "supersedes", " supersedes "] {
        let mut bad=linked.clone(); bad.id=relation.into(); bad.links[0].relation=relation.into();
        assert!(store.append_typed_record(&allowed,&bad,None).is_err());
    }
    let mut bad=linked; bad.id="unknown-link-key".into();
    bad.links[0].target=TypedRecordTarget::External { kind:kind("annotation").id,keys:json!({"undeclared":1}) };
    assert!(store.append_typed_record(&allowed,&bad,None).is_err());
}

#[test]
fn receipt_failure_rolls_back_new_revision_and_lineage() {
    let dir=tempfile::tempdir().unwrap(); let path=dir.path().join("rollback.db");
    let mut store=open(&path,true,"assistant",registry());
    let writer=writer("assistant","observation");
    let original=store.append_typed_record(&writer,&input("r1"),None).unwrap().record;
    let id=SupersessionReceipt::id_for("typed_record_revision_v1","typed_record_revision_v1","r1","r2");
    // An independent fixture connection seeds a conflicting receipt; no
    // production authorization or trigger inventory is relaxed for the test.
    let raw=Connection::open(&path).unwrap();
    raw.execute("INSERT INTO hard_state(namespace,key,value_json) VALUES(?1,?2,'{}')",
        rusqlite::params![crate::SUPERSESSION_RECEIPT_NAMESPACE,id]).unwrap();
    assert!(store.append_typed_record(&writer,&input("r2"),Some("r1")).is_err());
    assert!(store.get_typed_record("r2").unwrap().is_none());
    assert_eq!(store.get_typed_record("r1").unwrap().unwrap(),original);
    let count:i64=store.conn.query_row("SELECT COUNT(*) FROM typed_record_supersessions",[],|r|r.get(0)).unwrap();
    assert_eq!(count,0);
}

#[test]
fn two_concurrent_replacements_have_one_winner() {
    let dir=tempfile::tempdir().unwrap(); let path=dir.path().join("concurrent.db");
    let mut store=open(&path,true,"assistant",registry());
    store.append_typed_record(&writer("assistant","observation"),&input("r1"),None).unwrap();
    drop(store);
    let barrier=Arc::new(std::sync::Barrier::new(2));
    let joins:Vec<_>=["r2","r3"].into_iter().map(|id| {
        let path=path.clone(); let barrier=barrier.clone();
        std::thread::spawn(move || {
            let mut store=open(&path,false,"assistant",registry());
            barrier.wait();
            store.append_typed_record(&writer("assistant","observation"),&input(id),Some("r1")).is_ok()
        })
    }).collect();
    assert_eq!(joins.into_iter().map(|join| usize::from(join.join().unwrap())).sum::<usize>(),1);
    let store=open(&path,false,"assistant",registry());
    let count:i64=store.conn.query_row("SELECT COUNT(*) FROM typed_records",[],|r|r.get(0)).unwrap();
    assert_eq!(count,2);
}

#[test]
fn registry_schema_cannot_change_after_reopen_or_through_generic_state() {
    let dir=tempfile::tempdir().unwrap(); let path=dir.path().join("registry.db");
    let mut store=open(&path,true,"assistant",registry()); store.register_record_kinds().unwrap();
    assert!(store.set_state(RECORD_KINDS_NAMESPACE,"observation@1","{}").is_err());
    assert!(crate::db::delete_state(&store.conn,RECORD_KINDS_NAMESPACE,"observation@1").is_err());
    assert!(crate::db::insert_state_if_absent(&store.conn,RECORD_KINDS_NAMESPACE,"forged@1","{}").is_err());
    for sql in ["UPDATE hard_state SET value_json='{}' WHERE namespace='typed_record_kinds'",
        "DELETE FROM hard_state WHERE namespace='typed_record_kinds'",
        "INSERT OR REPLACE INTO hard_state SELECT * FROM hard_state WHERE namespace='typed_record_kinds'"] {
        assert!(store.conn.execute(sql,[]).is_err());
    }
    crate::db::reap_expired_state(&store.conn,"2100-01-01T00:00:00Z").unwrap();
    drop(store);
    let mut changed=kind("observation"); changed.payload_schema=json!({"type":"number"});
    let mut store=open(&path,false,"assistant",RecordRegistry::new(vec![changed]).unwrap());
    assert!(store.register_record_kinds().is_err());
    assert!(store.append_typed_record(&writer("assistant","observation"),&input("new"),None).is_err());
}

#[test]
fn schema_compiler_enforces_draft_and_refuses_external_resolution() {
    for schema in [json!({"$ref":"https://example.invalid/schema"}),
        json!({"$ref":"file:///etc/passwd"}), json!({"type":"not-a-json-type"}),
        json!({"$schema":"http://json-schema.org/draft-07/schema#","type":"object"})] {
        let mut definition=kind("example"); definition.payload_schema=schema;
        assert!(RecordRegistry::new(vec![definition]).is_err());
    }
}

#[test]
fn exact_role_rejects_wrong_missing_and_legacy_alias_stamps() {
    let dir=tempfile::tempdir().unwrap(); let path=dir.path().join("role.db");
    drop(open(&path,true,"project:assistant",registry()));
    for role in ["assistant","other"] {
        let ctx=DbOpenContext::open_existing_deny().with_exact_profile(StoreProfile::PortableKernel).with_exact_role(role);
        assert!(MemoryStore::open_with_context(path.to_str().unwrap(),&ctx).is_err());
    }
    let missing=dir.path().join("unstamped.db");
    let ctx=DbOpenContext::create_fresh().with_exact_profile(StoreProfile::PortableKernel);
    drop(MemoryStore::open_with_context(missing.to_str().unwrap(),&ctx).unwrap());
    let ctx=DbOpenContext::open_existing_deny().with_exact_profile(StoreProfile::PortableKernel).with_exact_role("assistant");
    assert!(MemoryStore::open_with_context(missing.to_str().unwrap(),&ctx).is_err());
}
