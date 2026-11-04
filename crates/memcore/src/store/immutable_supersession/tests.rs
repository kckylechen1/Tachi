use super::*;
use rusqlite::types::Value as SqlValue;
use serde_json::json;

fn fixture_entry(id: &str) -> MemoryEntry {
    MemoryEntry {
        id: id.to_string(),
        path: "/".to_string(),
        summary: String::new(),
        text: format!("body for {id}"),
        importance: 0.7,
        timestamp: "2026-08-04T00:00:00Z".to_string(),
        valid_from: String::new(),
        valid_until: None,
        category: "fact".to_string(),
        topic: String::new(),
        keywords: vec![],
        persons: vec![],
        entities: vec![],
        location: String::new(),
        source: "fixture".to_string(),
        scope: "general".to_string(),
        archived: false,
        access_count: 0,
        scored_count: 0,
        last_access: None,
        last_use_at: None,
        revision: 1,
        vector: None,
        retention_policy: None,
        domain: None,
        metadata: json!({}),
        recall_count: 0,
        query_diversity: 0,
        tier: "raw".to_string(),
    }
}

/// Capture every user table as a comparable value snapshot. A retirement
/// refusal must not merely leave the target row looking unchanged: it
/// must leave all SQLite bytes represented by the ordinary store tables
/// unchanged, including FTS/vector projections and metadata tables.
fn database_snapshot(store: &MemoryStore) -> Vec<(String, Vec<Vec<String>>)> {
    let conn = store.connection();
    let table_names = conn
        .prepare(
            "SELECT name FROM sqlite_master WHERE type = 'table' \
             AND name NOT LIKE 'sqlite_%' ORDER BY name",
        )
        .expect("prepare table snapshot")
        .query_map([], |row| row.get::<_, String>(0))
        .expect("list snapshot tables")
        .collect::<Result<Vec<_>, _>>()
        .expect("read snapshot tables");

    table_names
        .into_iter()
        .map(|table| {
            let quoted = format!("\"{}\"", table.replace('"', "\"\""));
            let mut stmt = conn
                .prepare(&format!("SELECT * FROM {quoted}"))
                .expect("prepare table contents snapshot");
            let column_count = stmt.column_count();
            let rows = stmt
                .query_map([], |row| {
                    (0..column_count)
                        .map(|column| {
                            row.get::<_, SqlValue>(column)
                                .map(|value| format!("{value:?}"))
                        })
                        .collect::<Result<Vec<_>, _>>()
                })
                .expect("read table contents snapshot")
                .collect::<Result<Vec<_>, _>>()
                .expect("collect table contents snapshot");
            (table, rows)
        })
        .collect()
}

fn retired_entry(path: &str, category: &str, id: &str) -> MemoryEntry {
    let mut entry = fixture_entry(id);
    entry.path = path.to_string();
    entry.category = category.to_string();
    entry
}

fn retire_existing_fixture(store: &MemoryStore, id: &str) {
    let _authorization =
        crate::db::authorize_reserved_reference_write(&store.reserved_reference_write)
            .expect("authorize raw legacy sticky fixture");
    store
        .connection()
        .execute(
            "UPDATE memories SET path='/sticky/legacy', category='sticky' WHERE id=?1",
            [id],
        )
        .expect("turn ordinary seed into raw legacy sticky fixture");
}

fn assert_transaction_refuses_without_writes<F>(label: &str, entry: MemoryEntry, operation: F)
where
    F: Fn(&mut ImmutableSupersessionTransaction<'_>, &MemoryEntry) -> Result<(), MemoryError>,
{
    let mut store = MemoryStore::open_in_memory().expect("open memory store");
    let before = database_snapshot(&store);
    let before_changes = store.connection().total_changes();
    let error = store
        .with_immutable_supersession_transaction(|tx| operation(tx, &entry))
        .expect_err(label);
    assert!(
        error.to_string().contains("tachi_a2a"),
        "{label} must name its successor: {error}"
    );
    assert_eq!(
        store.connection().total_changes(),
        before_changes,
        "{label} must execute zero SQLite changes"
    );
    assert_eq!(
        database_snapshot(&store),
        before,
        "{label} refusal must leave every ordinary store table byte-identical"
    );
}

fn assert_transaction_family_refuses<F>(label: &str, operation: F)
where
    F: Copy
        + Fn(&mut ImmutableSupersessionTransaction<'_>, &MemoryEntry) -> Result<(), MemoryError>,
{
    for (variant, entry) in [
        (
            "retired path",
            retired_entry("//STICKY///legacy/", "fact", "retired-path"),
        ),
        (
            "retired category",
            retired_entry("/ordinary", " Sticky ", "retired-category"),
        ),
    ] {
        assert_transaction_refuses_without_writes(
            &format!("{label} must reject {variant}"),
            entry,
            operation,
        );
    }
}

#[test]
fn immutable_transaction_upsert_rejects_retired_path_and_category() {
    assert_transaction_family_refuses("transaction upsert", |tx, entry| tx.upsert(entry));
}

#[test]
fn immutable_transaction_insert_if_absent_rejects_retired_path_and_category() {
    assert_transaction_family_refuses("transaction insert_if_absent", |tx, entry| {
        tx.insert_if_absent(entry).map(|_| ())
    });
}

#[test]
fn immutable_transaction_insert_rem_rejects_retired_path_and_category() {
    assert_transaction_family_refuses("transaction insert_rem_operation_if_absent", |tx, entry| {
        tx.insert_rem_operation_if_absent(entry).map(|_| ())
    });
}

#[test]
fn immutable_transaction_validated_reference_upsert_rejects_retired_path_and_category() {
    assert_transaction_family_refuses("transaction validated reference upsert", |tx, entry| {
        tx.upsert_with_validated_reference_mutations(entry, &Map::new(), &[])
    });
}

#[test]
fn immutable_transaction_validated_reference_upsert_with_removals_rejects_retired_path_and_category(
) {
    assert_transaction_family_refuses(
        "transaction validated reference upsert with removals",
        |tx, entry| {
            tx.upsert_with_validated_reference_mutations_and_metadata_removals(
                entry,
                None,
                &Map::new(),
                &[],
                &[],
                db::NearDuplicatePolicy::NonSemantic,
            )
            .map(|_| ())
        },
    );
}

#[test]
fn immutable_transaction_memory_writers_accept_normal_path_and_category() {
    let normal_entry = || fixture_entry("normal-transaction-writer");

    let mut store = MemoryStore::open_in_memory().expect("open memory store");
    store
        .with_immutable_supersession_transaction(|tx| tx.upsert(&normal_entry()))
        .expect("normal transaction upsert");

    let mut store = MemoryStore::open_in_memory().expect("open memory store");
    store
        .with_immutable_supersession_transaction(|tx| {
            tx.insert_if_absent(&normal_entry()).map(|_| ())
        })
        .expect("normal transaction insert_if_absent");

    let mut rem = normal_entry();
    rem.id = "wiki-rem:normal".to_string();
    rem.source = "wiki".to_string();
    rem.path = "/wiki/drafts/normal".to_string();
    rem.metadata = json!({
        "rem": {
            "producer": "weekly_wiki_evolver",
            "operation_id": "wiki-rem:normal",
            "operation_status": "pending_sources"
        }
    });
    let mut store = MemoryStore::open_in_memory().expect("open memory store");
    store
        .with_immutable_supersession_transaction(|tx| {
            tx.insert_rem_operation_if_absent(&rem).map(|_| ())
        })
        .expect("normal transaction insert_rem_operation_if_absent");

    let mut store = MemoryStore::open_in_memory().expect("open memory store");
    store
        .with_immutable_supersession_transaction(|tx| {
            tx.upsert_with_validated_reference_mutations(&normal_entry(), &Map::new(), &[])
        })
        .expect("normal validated reference upsert");

    let mut store = MemoryStore::open_in_memory().expect("open memory store");
    store
        .with_immutable_supersession_transaction(|tx| {
            tx.upsert_with_validated_reference_mutations_and_metadata_removals(
                &normal_entry(),
                None,
                &Map::new(),
                &[],
                &[],
                db::NearDuplicatePolicy::NonSemantic,
            )
            .map(|_| ())
        })
        .expect("normal validated reference upsert with removals");
}

#[test]
fn immutable_supersession_rejects_retired_source_or_target_before_any_side_effect() {
    for retired_id in ["source", "target"] {
        let mut store = MemoryStore::open_in_memory().expect("open memory store");
        store
            .insert_if_absent(&fixture_entry("source"))
            .expect("seed source");
        store
            .insert_if_absent(&fixture_entry("target"))
            .expect("seed target");
        retire_existing_fixture(&store, retired_id);
        let before = database_snapshot(&store);
        let before_changes = store.connection().total_changes();

        let error = store
            .with_immutable_supersession_transaction(|operation| {
                operation.claim_immutable_supersession("source", "target")?;
                operation.archive_claimed_source("source")
            })
            .expect_err("retired source or target must refuse the whole transaction");
        assert!(error.to_string().contains("tachi_a2a"), "{error}");
        assert_eq!(store.connection().total_changes(), before_changes);
        assert_eq!(
            database_snapshot(&store),
            before,
            "{retired_id} retirement refusal must precede row, edge, and projection writes"
        );
    }
}

/// tachi#1635 (#1632 conformance, item 1): the transaction wrapper's
/// `claim_immutable_supersession` must refuse source == target the same
/// way the test-support raw CAS does — but here a self-edge becomes a
/// typed refusal rather than a silent `Ok(false)`.
#[test]
fn claim_immutable_supersession_refuses_self_supersession() {
    let mut store = MemoryStore::open_in_memory().expect("open memory store");
    store
        .insert_if_absent(&fixture_entry("self-loop"))
        .expect("seed self-loop candidate");

    let error = store
        .with_immutable_supersession_transaction(|operation| {
            operation.claim_immutable_supersession("self-loop", "self-loop")
        })
        .expect_err("source == target must refuse");
    assert!(
        error.to_string().contains("self_supersession"),
        "unexpected error: {error}"
    );

    let unsuperseded = store
        .with_immutable_supersession_transaction(|operation| {
            operation.memory_is_active_unsuperseded("self-loop")
        })
        .expect("read back after refused self-supersession");
    assert!(
        unsuperseded,
        "a refused self-supersession must leave the row active and unsuperseded"
    );
}

/// tachi#1671: applying the identical edge again returns an explicit
/// prior-idempotent result backed by the first transaction's durable
/// receipt. A competing target remains a typed refusal.
#[test]
fn claim_immutable_supersession_same_edge_replay_returns_prior_result() {
    let mut store = MemoryStore::open_in_memory().expect("open memory store");
    store
        .insert_if_absent(&fixture_entry("replay-source"))
        .expect("seed source");
    store
        .insert_if_absent(&fixture_entry("replay-target"))
        .expect("seed target");

    store
        .with_immutable_supersession_transaction(|operation| {
            operation.claim_immutable_supersession("replay-source", "replay-target")
        })
        .expect("first claim must install the edge");

    let result = store
        .with_immutable_supersession_transaction(|operation| {
            operation.claim_immutable_supersession("replay-source", "replay-target")
        })
        .expect("identical replay must return an explicit prior result");
    assert_eq!(result, SupersessionCommitResult::PriorIdempotent);
}

#[test]
fn receipt_namespace_refuses_ttl_backfill_and_reaping() {
    let mut store = MemoryStore::open_in_memory().expect("open memory store");
    store
        .insert_if_absent(&fixture_entry("ttl-source"))
        .expect("seed source");
    store
        .insert_if_absent(&fixture_entry("ttl-target"))
        .expect("seed target");
    store
        .with_immutable_supersession_transaction(|operation| {
            operation.claim_immutable_supersession("ttl-source", "ttl-target")
        })
        .expect("commit supersession receipt");

    assert_eq!(
        store
            .backfill_missing_expires_at(
                SUPERSESSION_RECEIPT_NAMESPACE,
                "2000-01-01T00:00:00Z",
                None,
            )
            .expect("protected namespace backfill is a no-op"),
        0
    );
    assert_eq!(
        store
            .reap_expired_state("2100-01-01T00:00:00Z")
            .expect("reap hard state"),
        0
    );
    assert_eq!(
        store
            .with_immutable_supersession_transaction(|operation| {
                operation.claim_immutable_supersession("ttl-source", "ttl-target")
            })
            .expect("receipt survives TTL maintenance"),
        SupersessionCommitResult::PriorIdempotent
    );
}

#[test]
fn transaction_refuses_to_archive_a_source_it_did_not_claim() {
    let mut store = MemoryStore::open_in_memory().expect("open memory store");
    for id in ["claimed-source", "claimed-target", "unrelated-source"] {
        store
            .insert_if_absent(&fixture_entry(id))
            .expect("seed transaction fixture");
    }
    let error = store
        .with_immutable_supersession_transaction(|operation| {
            operation.claim_immutable_supersession("claimed-source", "claimed-target")?;
            operation.archive_claimed_source("unrelated-source")
        })
        .expect_err("unrelated archive must roll the claim back");
    assert!(error.to_string().contains("no applied supersession claim"));
    for id in ["claimed-source", "unrelated-source"] {
        let entry = store
            .get(id)
            .expect("read fixture")
            .expect("fixture remains");
        assert!(!entry.archived);
        assert_eq!(
            store.supersession_target(id).expect("read target"),
            Some(None)
        );
    }
}

#[test]
fn transaction_refuses_a_claim_whose_target_never_materializes() {
    let mut store = MemoryStore::open_in_memory().expect("open memory store");
    store
        .insert_if_absent(&fixture_entry("dangling-source"))
        .expect("seed source");
    let error = store
        .with_immutable_supersession_transaction(|operation| {
            operation.claim_immutable_supersession("dangling-source", "missing-target")
        })
        .expect_err("commit must refuse a dangling target");
    assert!(error.to_string().contains("not materialized before commit"));
    assert_eq!(
        store
            .supersession_target("dangling-source")
            .expect("read source after rollback"),
        Some(None)
    );
}

#[test]
fn semantic_replay_returns_the_exact_stored_receipt_without_rewriting_it() {
    const ROUTE: &str = "test_semantic_replay";
    const POLICY: &str = "test_policy_v1";

    let mut store = MemoryStore::open_in_memory().expect("open memory store");
    let source = fixture_entry("semantic-replay-source");
    let target = fixture_entry("semantic-replay-target");
    store.insert_if_absent(&source).expect("seed source");
    store.insert_if_absent(&target).expect("seed target");

    let first = store
        .with_immutable_supersession_transaction(|operation| {
            let outcome = operation.claim_and_archive_immutable_supersession(
                &source.id, &target.id, None, ROUTE, POLICY,
            )?;
            assert_eq!(outcome.result, SupersessionCommitResult::Applied);
            let mut receipt = outcome.receipt;
            operation
                .finalize_supersession_receipt(&mut receipt, "test_semantic_replay_committed")?;
            Ok(receipt)
        })
        .expect("first semantic claim");
    let receipt_id = SupersessionReceipt::id_for(ROUTE, POLICY, &source.id, &target.id);
    let (stored_json, stored_version) = store
        .get_state_kv(SUPERSESSION_RECEIPT_NAMESPACE, &receipt_id)
        .expect("read first receipt")
        .expect("first receipt exists");
    let stored: SupersessionReceipt =
        serde_json::from_str(&stored_json).expect("decode first receipt");
    assert_eq!(stored_version, 1);
    assert_eq!(stored, first);

    let replay = store
        .with_immutable_supersession_transaction(|operation| {
            operation.claim_and_archive_immutable_supersession(
                &source.id, &target.id, None, ROUTE, POLICY,
            )
        })
        .expect("same-edge semantic replay");
    assert_eq!(replay.result, SupersessionCommitResult::PriorIdempotent);
    assert_eq!(
        replay.receipt, stored,
        "attempt-local replay status must not mutate the write-once receipt clone"
    );
    let (stored_after_json, stored_after_version) = store
        .get_state_kv(SUPERSESSION_RECEIPT_NAMESPACE, &receipt_id)
        .expect("read receipt after replay")
        .expect("receipt remains present");
    assert_eq!(stored_after_version, stored_version);
    assert_eq!(stored_after_json, stored_json);
}

#[test]
fn caller_receipt_tampering_is_rejected_and_pending_receipt_can_finalize() {
    let mut store = MemoryStore::open_in_memory().expect("open memory store");
    let source = fixture_entry("tamper-source");
    let target = fixture_entry("tamper-target");
    store.insert_if_absent(&source).expect("seed source");
    store.insert_if_absent(&target).expect("seed target");

    let error = store
        .with_immutable_supersession_transaction(|operation| {
            let outcome = operation.claim_and_archive_immutable_supersession(
                &source.id,
                &target.id,
                None,
                "tamper-test-route",
                "tamper-test-policy-v1",
            )?;
            let mut forged = outcome.receipt;
            forged.route = "caller-forged-route".to_string();
            operation.finalize_supersession_receipt(&mut forged, "caller-forged-disposition")
        })
        .expect_err("caller-mutated receipt must not finalize");
    assert!(
        error
            .to_string()
            .contains("diverged from the canonical pending receipt"),
        "unexpected tampering error: {error}"
    );
    assert_eq!(
        store
            .supersession_target(&source.id)
            .expect("read rolled-back source"),
        Some(None),
        "a rejected receipt tamper must roll back the semantic edge"
    );

    store
        .with_immutable_supersession_transaction(|operation| {
            operation.claim_and_archive_immutable_supersession(
                &source.id,
                &target.id,
                None,
                "tamper-test-route",
                "tamper-test-policy-v1",
            )
        })
        .expect("the transaction-held canonical receipt auto-finalizes");
    let (receipt_json, _) = store
        .get_state_kv(
            SUPERSESSION_RECEIPT_NAMESPACE,
            &SupersessionReceipt::id_for(
                "tamper-test-route",
                "tamper-test-policy-v1",
                &source.id,
                &target.id,
            ),
        )
        .expect("read canonical receipt")
        .expect("canonical receipt exists");
    let receipt: SupersessionReceipt =
        serde_json::from_str(&receipt_json).expect("decode canonical receipt");
    assert_eq!(receipt.route, "tamper-test-route");
    assert_eq!(receipt.dependent_write_disposition, "transaction_committed");
}

#[test]
fn semantic_lifecycle_claim_refuses_wiki_and_durable_sources_without_writes() {
    let mut wiki = fixture_entry("protected-wiki-source");
    wiki.category = "WiKi".to_string();
    let mut durable = fixture_entry("protected-durable-source");
    durable.retention_policy = Some("durable".to_string());

    for source in [wiki, durable] {
        let mut store = MemoryStore::open_in_memory().expect("open memory store");
        let target = fixture_entry(&format!("{}-target", source.id));
        store
            .insert_if_absent(&source)
            .expect("seed protected source");
        store.insert_if_absent(&target).expect("seed target");
        let before = database_snapshot(&store);
        let receipt_id = SupersessionReceipt::id_for(
            "test_protected_semantic_route",
            "test_policy_v1",
            &source.id,
            &target.id,
        );

        let error = store
            .with_immutable_supersession_transaction(|operation| {
                operation.claim_and_archive_immutable_supersession(
                    &source.id,
                    &target.id,
                    None,
                    "test_protected_semantic_route",
                    "test_policy_v1",
                )
            })
            .expect_err("protected semantic source must refuse");
        assert!(error.to_string().contains("source_protected"), "{error}");
        assert_eq!(database_snapshot(&store), before);
        assert!(
            store
                .get_state_kv(SUPERSESSION_RECEIPT_NAMESPACE, &receipt_id)
                .expect("read receipt namespace")
                .is_none(),
            "a protected-source refusal must not leave durable evidence"
        );
    }
}

#[test]
fn legacy_same_edge_without_receipt_refuses_instead_of_forging_prior_result() {
    let mut store = MemoryStore::open_in_memory().expect("open memory store");
    store
        .insert_if_absent(&fixture_entry("legacy-source"))
        .expect("seed source");
    store
        .insert_if_absent(&fixture_entry("legacy-target"))
        .expect("seed target");
    assert!(store
        .supersede_memory("legacy-source", "legacy-target")
        .expect("seed legacy raw edge"));

    let error = store
        .with_immutable_supersession_transaction(|operation| {
            operation.claim_immutable_supersession("legacy-source", "legacy-target")
        })
        .expect_err("an edge without durable evidence is not an idempotent prior result");
    assert!(
        error.to_string().contains("prior_receipt_missing"),
        "{error}"
    );
}

#[test]
fn checked_supersession_refuses_target_drift_without_writes() {
    let mut store = MemoryStore::open_in_memory().expect("open memory store");
    let source = fixture_entry("drift-source");
    let target = fixture_entry("drift-target");
    store.insert_if_absent(&source).expect("seed source");
    store.insert_if_absent(&target).expect("seed target");
    let source_snapshot = store.get(&source.id).expect("read source").expect("source");
    let target_snapshot = store.get(&target.id).expect("read target").expect("target");
    let expected =
        SupersessionExpectedState::active_unsuperseded(&source_snapshot, Some(&target_snapshot));

    let mut drifted = target.clone();
    drifted.summary = "target changed after planning".to_string();
    store.upsert(&drifted).expect("drift target");
    let before = database_snapshot(&store);

    let error = store
        .with_immutable_supersession_transaction(|operation| {
            operation.claim_and_archive_immutable_supersession(
                "drift-source",
                "drift-target",
                Some(&expected),
                "test_checked_route",
                "test_policy_v1",
            )
        })
        .expect_err("target drift must refuse");
    assert!(error.to_string().contains("target_drift"), "{error}");
    assert_eq!(database_snapshot(&store), before);
}

#[test]
fn dependent_failure_rolls_back_edge_and_durable_receipt() {
    let mut store = MemoryStore::open_in_memory().expect("open memory store");
    let source = fixture_entry("rollback-source");
    let target = fixture_entry("rollback-target");
    store.insert_if_absent(&source).expect("seed source");
    store.insert_if_absent(&target).expect("seed target");
    let source_snapshot = store.get(&source.id).expect("read source").expect("source");
    let target_snapshot = store.get(&target.id).expect("read target").expect("target");
    let expected =
        SupersessionExpectedState::active_unsuperseded(&source_snapshot, Some(&target_snapshot));
    let receipt_id = SupersessionReceipt::id_for(
        "test_rollback_route",
        "test_policy_v1",
        &source.id,
        &target.id,
    );

    let error = store
        .with_immutable_supersession_transaction::<()>(|operation| {
            operation.claim_and_archive_immutable_supersession(
                &source.id,
                &target.id,
                Some(&expected),
                "test_rollback_route",
                "test_policy_v1",
            )?;
            Err(MemoryError::Internal(
                "injected dependent write failure".to_string(),
            ))
        })
        .expect_err("dependent failure must abort the transaction");
    assert!(error
        .to_string()
        .contains("injected dependent write failure"));
    assert_eq!(
        store.supersession_target(&source.id).expect("read edge"),
        Some(None)
    );
    assert!(
        store
            .get_state_kv(SUPERSESSION_RECEIPT_NAMESPACE, &receipt_id)
            .expect("read receipt state")
            .is_none(),
        "rolled-back lifecycle mutation must not leave a receipt"
    );
}

#[test]
fn raw_connection_cannot_mutate_reserved_supersedes_edges() {
    let mut store = MemoryStore::open_in_memory().expect("open memory store");
    for id in ["raw-edge-source", "raw-edge-target"] {
        store
            .insert_if_absent(&fixture_entry(id))
            .expect("seed edge endpoint");
    }
    let guard_count: i64 = store
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM temp.sqlite_master
             WHERE type='trigger' AND name IN (
               'tachi_authority_hard_state_insert_guard',
               'tachi_authority_hard_state_update_guard',
               'tachi_authority_hard_state_delete_guard',
               'tachi_supersession_event_insert_guard',
               'tachi_supersession_event_update_guard',
               'tachi_supersession_event_delete_guard',
               'tachi_canonical_supersession_edge_insert_guard',
               'tachi_canonical_supersession_edge_update_guard',
               'tachi_canonical_supersession_edge_delete_guard'
             )",
            [],
            |row| row.get(0),
        )
        .expect("count exact authority guards");
    assert_eq!(guard_count, 9, "exact connection-local guard inventory");

    store
        .connection()
        .execute(
            "INSERT INTO memory_edges
               (source_id,target_id,relation,weight,metadata,created_at,valid_from)
             VALUES (?1,?2,'related_to',1.0,'{}',?3,?3)",
            params!["raw-edge-source", "raw-edge-target", db::now_utc_iso()],
        )
        .expect("ordinary raw edge remains allowed");
    {
        let authorization = store.reserved_reference_write.clone();
        let _event_authorization = db::authorize_reserved_reference_write(&authorization)
            .expect("arm generic typed event authority");
        assert!(store
            .connection()
            .execute(
                "UPDATE memory_edges SET relation='supersedes'
                 WHERE source_id='raw-edge-source' AND target_id='raw-edge-target'",
                [],
            )
            .is_err());
    }
    assert!(store
        .connection()
        .execute(
            "INSERT INTO memory_edges
               (source_id,target_id,relation,weight,metadata,created_at,valid_from)
             VALUES (?1,?2,'supersedes',1.0,'{}',?3,?3)",
            params!["raw-edge-target", "raw-edge-source", db::now_utc_iso()],
        )
        .is_err());

    store
        .with_immutable_supersession_transaction(|operation| {
            operation.claim_immutable_supersession("raw-edge-source", "raw-edge-target")?;
            let now = db::now_utc_iso();
            operation.add_canonical_supersession_edge(
                &MemoryEdge {
                    source_id: "raw-edge-target".to_string(),
                    target_id: "raw-edge-source".to_string(),
                    relation: "supersedes".to_string(),
                    weight: 1.0,
                    metadata: serde_json::json!({"source":"test"}),
                    created_at: now.clone(),
                    valid_from: now,
                    valid_to: None,
                },
                &db::EdgeProvenance {
                    authority: Some(db::EdgeAuthority::StructuralBookkeeping),
                    ..Default::default()
                },
            )
        })
        .expect("canonical edge writer remains authorized");
    assert!(store
        .connection()
        .execute(
            "DELETE FROM memory_edges
             WHERE source_id='raw-edge-target' AND target_id='raw-edge-source'
               AND relation='supersedes'",
            [],
        )
        .is_err());
    assert!(store
        .connection()
        .execute(
            "UPDATE memory_edges SET relation='related_to'
             WHERE source_id='raw-edge-target' AND target_id='raw-edge-source'
               AND relation='supersedes'",
            [],
        )
        .is_err());
    store
        .connection()
        .execute(
            "DELETE FROM memory_edges
             WHERE source_id='raw-edge-source' AND target_id='raw-edge-target'
               AND relation='related_to'",
            [],
        )
        .expect("ordinary raw edge delete remains allowed");
}

#[test]
fn idless_near_duplicate_dependent_failure_rolls_back_canonical_bundle() {
    let mut store = MemoryStore::open_in_memory().expect("open memory store");
    let shared = "alpha bravo charlie delta echo foxtrot golf hotel india juliet \
                  kilo lima mike november oscar papa quebec romeo sierra";
    let mut winner = fixture_entry("idless-rollback-winner");
    winner.path = "/notes/idless-rollback".to_string();
    winner.text = format!("{shared} tango");
    store
        .upsert_idless(&winner, "idless-rollback-winner-identity")
        .expect("seed winner");
    let mut loser = fixture_entry("idless-rollback-loser");
    loser.path = winner.path.clone();
    loser.text = format!("{shared} uniform");

    let error = store
        .with_immutable_supersession_transaction::<()>(|operation| {
            operation.upsert_idless_with_near_duplicate_merge(
                &loser,
                "idless-rollback-loser-identity",
                &Map::new(),
                &[],
                &[],
            )?;
            Err(MemoryError::Internal(
                "injected idless dependent failure".to_string(),
            ))
        })
        .expect_err("dependent failure must roll back idless canonical bundle");
    assert!(error
        .to_string()
        .contains("injected idless dependent failure"));
    assert!(store.get(&loser.id).expect("read loser").is_none());
    assert_eq!(store.supersession_target(&winner.id).unwrap(), Some(None));
    for table_count in [
        store
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM memory_edges WHERE relation='supersedes'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        store
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM hard_state WHERE namespace=?1",
                [SUPERSESSION_RECEIPT_NAMESPACE],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        store
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM tachi_events WHERE event_type=?1",
                [SUPERSESSION_RECEIPT_EVENT_TYPE],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
    ] {
        assert_eq!(table_count, 0);
    }
}

#[test]
fn idless_near_duplicate_keeps_lifecycle_protected_source_active() {
    let mut store = MemoryStore::open_in_memory().expect("open memory store");
    let shared = "alpha bravo charlie delta echo foxtrot golf hotel india juliet \
                  kilo lima mike november oscar papa quebec romeo sierra";
    let mut existing = fixture_entry("protected-near-duplicate-existing");
    existing.path = "/precedent_candidates/global/existing".to_string();
    existing.text = format!("{shared} tango");
    existing.retention_policy = Some("permanent".to_string());
    store
        .upsert_idless(&existing, "protected-existing-identity")
        .expect("seed existing protected row");

    let mut incoming = fixture_entry("protected-near-duplicate-incoming");
    incoming.path = "/precedent_candidates/global/incoming".to_string();
    incoming.text = format!("{shared} uniform");
    incoming.retention_policy = Some("permanent".to_string());
    let (result, _) = store
        .with_immutable_supersession_transaction(|operation| {
            operation.upsert_idless_with_near_duplicate_merge(
                &incoming,
                "protected-incoming-identity",
                &Map::new(),
                &[],
                &[],
            )
        })
        .expect("protected id-less save must land without semantic retirement");

    assert_eq!(result, db::IdlessUpsertResult::Saved);
    assert_eq!(store.supersession_target(&existing.id).unwrap(), Some(None));
    assert_eq!(store.supersession_target(&incoming.id).unwrap(), Some(None));
    let active_rows: i64 = store
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM memories
             WHERE id IN (?1, ?2) AND archived = 0 AND superseded_by IS NULL",
            params![&existing.id, &incoming.id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(active_rows, 2);
    for table_count in [
        store
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM memory_edges WHERE relation='supersedes'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        store
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM hard_state WHERE namespace=?1",
                [SUPERSESSION_RECEIPT_NAMESPACE],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        store
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM tachi_events WHERE event_type=?1",
                [SUPERSESSION_RECEIPT_EVENT_TYPE],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
    ] {
        assert_eq!(table_count, 0);
    }
}

#[test]
fn claimed_source_cannot_be_rewritten_or_unarchived_before_commit() {
    let mut store = MemoryStore::open_in_memory().expect("open memory store");
    let source = fixture_entry("claimed-source-rewrite");
    let target = fixture_entry("claimed-source-target");
    store.insert_if_absent(&source).expect("seed source");
    store.insert_if_absent(&target).expect("seed target");
    let receipt_id = SupersessionReceipt::id_for(
        "test_claimed_source_immutable",
        "test_policy_v1",
        &source.id,
        &target.id,
    );

    let error = store
        .with_immutable_supersession_transaction::<()>(|operation| {
            let outcome = operation.claim_and_archive_immutable_supersession(
                &source.id,
                &target.id,
                None,
                "test_claimed_source_immutable",
                "test_policy_v1",
            )?;
            assert_eq!(outcome.result, SupersessionCommitResult::Applied);
            let mut rewritten = source.clone();
            rewritten.text = "rewrite after immutable claim".to_string();
            rewritten.archived = false;
            operation.upsert(&rewritten)
        })
        .expect_err("a claimed source must be immutable until commit");
    assert!(
        error
            .to_string()
            .contains("claimed supersession source is immutable"),
        "{error}"
    );
    assert_eq!(
        store.supersession_target(&source.id).expect("read source"),
        Some(None),
        "refusal must roll back the claim"
    );
    assert!(
        store
            .get_state_kv(SUPERSESSION_RECEIPT_NAMESPACE, &receipt_id)
            .expect("read receipt")
            .is_none(),
        "refusal must not leave durable evidence"
    );
}

#[test]
fn durable_supersession_receipt_is_write_once_and_exact() {
    let mut store = MemoryStore::open_in_memory().expect("open memory store");
    let source = fixture_entry("receipt-source");
    let target = fixture_entry("receipt-target");
    store.insert_if_absent(&source).expect("seed source");
    store.insert_if_absent(&target).expect("seed target");
    let result = store
        .with_immutable_supersession_transaction(|operation| {
            operation.claim_immutable_supersession(&source.id, &target.id)
        })
        .expect("claim and persist receipt");
    assert_eq!(result, SupersessionCommitResult::Applied);
    let receipt_id = SupersessionReceipt::id_for(
        SUPERSESSION_ROUTE_IMMUTABLE_CLAIM,
        SUPERSESSION_ROUTE_IMMUTABLE_CLAIM,
        &source.id,
        &target.id,
    );
    let (receipt_json, version) = store
        .get_state_kv(SUPERSESSION_RECEIPT_NAMESPACE, &receipt_id)
        .expect("read receipt")
        .expect("receipt exists");
    let receipt: SupersessionReceipt =
        serde_json::from_str(&receipt_json).expect("deserialize receipt");
    assert!(receipt.durable);
    assert_eq!(receipt.source_revision_before, source.revision);
    assert_eq!(receipt.target_revision_before, Some(target.revision));
    assert_eq!(receipt.commit_result, SupersessionCommitResult::Applied);
    assert_eq!(version, 1);
    assert!(
        store
            .set_state(
                SUPERSESSION_RECEIPT_NAMESPACE,
                &receipt_id,
                r#"{"forged":true}"#,
            )
            .is_err(),
        "general state API must not overwrite committed supersession evidence"
    );
    assert!(
        store
            .insert_state_if_absent(
                SUPERSESSION_RECEIPT_NAMESPACE,
                "forged-receipt",
                r#"{"forged":true}"#,
            )
            .is_err(),
        "general state API must not forge committed supersession evidence"
    );
    assert!(
        store
            .delete_state(SUPERSESSION_RECEIPT_NAMESPACE, &receipt_id)
            .is_err(),
        "general state API must not delete committed supersession evidence"
    );
    assert!(
        store
            .connection()
            .execute(
                "UPDATE hard_state SET value_json = '{\"forged\":true}'
                 WHERE namespace = ?1 AND key = ?2",
                params![SUPERSESSION_RECEIPT_NAMESPACE, &receipt_id],
            )
            .is_err(),
        "default admin connection must not overwrite receipt authority"
    );
    assert!(
        store
            .connection()
            .execute("DELETE FROM tachi_events WHERE id = ?1", [&receipt_id],)
            .is_err(),
        "default admin connection must not delete the receipt event projection"
    );
}

#[test]
fn supersession_event_collision_checks_all_authority_fields() {
    let mut source_store = MemoryStore::open_in_memory().expect("open source store");
    let source = fixture_entry("event-collision-source");
    let target = fixture_entry("event-collision-target");
    source_store.insert_if_absent(&source).expect("seed source");
    source_store.insert_if_absent(&target).expect("seed target");
    source_store
        .with_immutable_supersession_transaction(|operation| {
            operation.claim_immutable_supersession(&source.id, &target.id)
        })
        .expect("produce canonical receipt");
    let receipt_id = SupersessionReceipt::id_for(
        SUPERSESSION_ROUTE_IMMUTABLE_CLAIM,
        SUPERSESSION_ROUTE_IMMUTABLE_CLAIM,
        &source.id,
        &target.id,
    );
    let (receipt_json, _) = source_store
        .get_state_kv(SUPERSESSION_RECEIPT_NAMESPACE, &receipt_id)
        .expect("read receipt")
        .expect("receipt exists");
    let mut receipt: SupersessionReceipt =
        serde_json::from_str(&receipt_json).expect("decode receipt");

    let mut store = MemoryStore::open_in_memory().expect("open collision store");
    let hostile_event = crate::types::TachiEventRecord {
        id: receipt_id.clone(),
        source_repo: String::new(),
        adapter: "hostile-adapter".to_string(),
        project: String::new(),
        domain: "memory".to_string(),
        session_id: String::new(),
        actor: "hostile-actor".to_string(),
        event_type: SUPERSESSION_RECEIPT_EVENT_TYPE.to_string(),
        authority: crate::types::AuthorityLevel::RawFact,
        effects: Vec::new(),
        projection_hints: Vec::new(),
        payload: serde_json::to_value(&receipt).expect("encode payload"),
        provenance: serde_json::json!({"producer": "hostile"}),
        created_at: db::now_utc_iso(),
    };
    let generic_error = store
        .insert_tachi_event(&hostile_event)
        .expect_err("generic event writer must reject reserved receipt events");
    assert!(generic_error.to_string().contains("reserved"));
    let idempotent_error = store
        .insert_tachi_event_if_absent(&hostile_event)
        .expect_err("idempotent generic event writer must reject reserved receipt events");
    assert!(idempotent_error.to_string().contains("reserved"));
    let authorization = store.reserved_reference_write.clone();
    let _authorization =
        db::authorize_reserved_reference_write(&authorization).expect("authorize typed write");
    store
        .conn
        .execute(
            "INSERT INTO tachi_events (
                id, source_repo, adapter, project, domain, session_id, actor,
                event_type, authority, effects, projection_hints,
                payload_json, provenance_json, created_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
            params![
                &hostile_event.id,
                &hostile_event.source_repo,
                &hostile_event.adapter,
                &hostile_event.project,
                &hostile_event.domain,
                &hostile_event.session_id,
                &hostile_event.actor,
                &hostile_event.event_type,
                hostile_event.authority.as_str(),
                serde_json::to_string(
                    &hostile_event
                        .effects
                        .iter()
                        .map(|effect| effect.as_str())
                        .collect::<Vec<_>>(),
                )
                .expect("encode effects"),
                serde_json::to_string(
                    &hostile_event
                        .projection_hints
                        .iter()
                        .map(|hint| hint.as_str())
                        .collect::<Vec<_>>(),
                )
                .expect("encode projection hints"),
                serde_json::to_string(&hostile_event.payload).expect("encode payload"),
                serde_json::to_string(&hostile_event.provenance).expect("encode provenance"),
                &hostile_event.created_at,
            ],
        )
        .expect("seed same-payload hostile event fixture");
    let tx = store
        .conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .expect("begin collision transaction");
    let error = persist_supersession_receipt(&tx, &mut receipt)
        .expect_err("same payload with different authority fields must refuse");
    assert!(
        error
            .to_string()
            .contains("supersession event identity conflict"),
        "{error}"
    );
    tx.rollback().expect("rollback collision fixture");
}

/// tachi#1645 (#1635 finding 1, item 2 — flipped from finding-pin to
/// enforcement assertion): `claim_immutable_supersession` ->
/// `refuse_ineligible_supersession_target` now reads the TARGET's
/// `archived`/`superseded_by` state before ever reaching
/// `db::supersede_memory`'s source-only CAS, so a caller that omits its
/// own target check (e.g. `apply_lifecycle_action`'s "supersede"/
/// "merge_into" arms in
/// `crates/tachi-server/src/facade_memory_ops/consolidate_ops.rs`, which
/// only call `refuse_if_protected` on the SOURCE) can no longer point a
/// fresh source at an already-archived, already-superseded target.
#[test]
fn claim_immutable_supersession_refuses_ineligible_target() {
    let mut store = MemoryStore::open_in_memory().expect("open memory store");
    store
        .insert_if_absent(&fixture_entry("ineligible-target"))
        .expect("seed target");
    store
        .insert_if_absent(&fixture_entry("ineligible-target-canonical"))
        .expect("seed target's own canonical replacement");
    store
        .insert_if_absent(&fixture_entry("source-onto-dead-target"))
        .expect("seed source");

    // The target is already archived AND already superseded before the
    // claim under test — it is neither active nor eligible.
    store
        .with_immutable_supersession_transaction(|operation| {
            operation
                .claim_immutable_supersession("ineligible-target", "ineligible-target-canonical")?;
            operation.archive_claimed_source("ineligible-target")
        })
        .expect("pre-condition: target becomes archived+superseded");

    let error = store
        .with_immutable_supersession_transaction(|operation| {
            operation.claim_immutable_supersession("source-onto-dead-target", "ineligible-target")
        })
        .expect_err("superseding onto an archived/already-superseded target must refuse");
    assert!(
        error.to_string().contains("target_ineligible"),
        "unexpected error: {error}"
    );

    let unsuperseded = store
        .with_immutable_supersession_transaction(|operation| {
            operation.memory_is_active_unsuperseded("source-onto-dead-target")
        })
        .expect("read back after refused claim");
    assert!(
        unsuperseded,
        "a refused claim onto an ineligible target must leave the would-be \
         source active and unsuperseded"
    );
}

/// tachi#1645 (#1635 finding 1): a target that does not exist yet is not
/// "retired" — Wiki ingest's `persist_wiki_ingest_entry` claims each
/// predecessor onto its brand-new replacement id BEFORE upserting that
/// row in the same transaction, and the eligibility guard must not break
/// that ordering.
#[test]
fn claim_immutable_supersession_permits_a_not_yet_materialized_target() {
    let mut store = MemoryStore::open_in_memory().expect("open memory store");
    store
        .insert_if_absent(&fixture_entry("predecessor"))
        .expect("seed predecessor");

    store
        .with_immutable_supersession_transaction(|operation| {
            operation.claim_immutable_supersession("predecessor", "not-yet-inserted")?;
            operation.upsert(&fixture_entry("not-yet-inserted"))
        })
        .expect("claim onto a target materialized later in the same transaction");
}

/// tachi#1645 (#1635 finding 2, item 3 — flipped from finding-pin to
/// enforcement assertion): `claim_immutable_supersession` now walks the
/// proposed target's `superseded_by` chain before installing a new edge,
/// so A -> B then B -> A refuses instead of both committing.
#[test]
fn claim_immutable_supersession_refuses_a_two_hop_cycle() {
    let mut store = MemoryStore::open_in_memory().expect("open memory store");
    store
        .insert_if_absent(&fixture_entry("cycle-a"))
        .expect("seed a");
    store
        .insert_if_absent(&fixture_entry("cycle-b"))
        .expect("seed b");

    store
        .with_immutable_supersession_transaction(|operation| {
            operation.claim_immutable_supersession("cycle-a", "cycle-b")
        })
        .expect("A -> B installs");

    let error = store
        .with_immutable_supersession_transaction(|operation| {
            operation.claim_immutable_supersession("cycle-b", "cycle-a")
        })
        .expect_err("B -> A after A -> B must refuse (cycle)");
    assert!(
        error.to_string().contains("cycle"),
        "unexpected error: {error}"
    );
}

/// tachi#1645 (#1635 finding 2): the chain walk must catch a cycle that
/// closes more than one hop past the immediate target — not just the
/// two-hop case a bare target-eligibility check would also happen to
/// catch (an already-superseded immediate target is refused by
/// `refuse_ineligible_supersession_target` regardless of whether it
/// leads back to `source_id`). A -> B -> C, then C -> A must refuse
/// specifically because A's chain (A -> B -> C) reaches back to C.
#[test]
fn claim_immutable_supersession_refuses_a_deeper_chain_cycle() {
    let mut store = MemoryStore::open_in_memory().expect("open memory store");
    for id in ["chain-a", "chain-b", "chain-c"] {
        store
            .insert_if_absent(&fixture_entry(id))
            .expect("seed chain node");
    }
    store
        .with_immutable_supersession_transaction(|operation| {
            operation.claim_immutable_supersession("chain-a", "chain-b")
        })
        .expect("A -> B installs");
    store
        .with_immutable_supersession_transaction(|operation| {
            operation.claim_immutable_supersession("chain-b", "chain-c")
        })
        .expect("B -> C installs");

    let error = store
        .with_immutable_supersession_transaction(|operation| {
            operation.claim_immutable_supersession("chain-c", "chain-a")
        })
        .expect_err("C -> A must refuse: A's chain (A -> B -> C) closes back to C");
    assert!(
        error.to_string().contains("cycle"),
        "unexpected error: {error}"
    );
}

/// tachi#1645 (#1635 finding 2): a chain walk that does not terminate
/// within [`MAX_SUPERSESSION_CHAIN_WALK`] hops refuses with a distinct,
/// differently-worded error than the cycle-found case above — even
/// though `probe` never appears anywhere in the chain (so this is NOT a
/// cycle, just an implausibly long lineage the walk refuses to keep
/// scanning past the cap).
#[test]
fn claim_immutable_supersession_refuses_when_chain_walk_exceeds_depth_cap() {
    let mut store = MemoryStore::open_in_memory().expect("open memory store");
    let chain_len = MAX_SUPERSESSION_CHAIN_WALK as usize + 8;
    let node_id = |i: usize| format!("cap-chain-{i}");
    for i in 0..=chain_len {
        store
            .insert_if_absent(&fixture_entry(&node_id(i)))
            .expect("seed cap-chain node");
    }
    for i in 0..chain_len {
        store
            .with_immutable_supersession_transaction(|operation| {
                operation.claim_immutable_supersession(&node_id(i), &node_id(i + 1))
            })
            .unwrap_or_else(|error| {
                panic!("{} -> {} installs: {error}", node_id(i), node_id(i + 1))
            });
    }
    store
        .insert_if_absent(&fixture_entry("probe"))
        .expect("seed probe (never part of the chain)");

    let error = store
        .with_immutable_supersession_transaction(|operation| {
            operation.claim_immutable_supersession("probe", &node_id(0))
        })
        .expect_err("a chain longer than the depth cap must refuse even though it is not a cycle");
    let message = error.to_string();
    assert!(
        message.contains("did not terminate within"),
        "unexpected error: {message}"
    );
    assert!(
        !message.contains("would close a superseded_by cycle"),
        "cap exhaustion must not be reported as a cycle-found error: {message}"
    );
}

/// tachi#1671 / #1668: `MemoryEntry::scope` is **not** a partition.
/// Two rows on one generic store may still supersede each other even
/// when their scope labels differ — that is intentional. Cross-partition
/// refusal is proven by two [`crate::private_partition::PrivatePartition`]
/// handles (separate sealed files) in
/// `private_partition::tests::cross_partition_claim_cannot_see_foreign_rows`.
#[test]
fn scope_labels_are_not_partition_authority() {
    let mut store = MemoryStore::open_in_memory().expect("open memory store");
    let mut source = fixture_entry("partition-a-source");
    source.scope = "project:alpha".to_string();
    let mut target = fixture_entry("partition-b-target");
    target.scope = "project:beta".to_string();
    store
        .insert_if_absent(&source)
        .expect("seed alpha-scope source");
    store
        .insert_if_absent(&target)
        .expect("seed beta-scope target");

    let result = store
        .with_immutable_supersession_transaction(|operation| {
            operation.claim_immutable_supersession("partition-a-source", "partition-b-target")
        })
        .expect("same-store scope labels are not a trust boundary");
    assert_eq!(result, SupersessionCommitResult::Applied);
    let receipt_id = SupersessionReceipt::id_for(
        SUPERSESSION_ROUTE_IMMUTABLE_CLAIM,
        SUPERSESSION_ROUTE_IMMUTABLE_CLAIM,
        "partition-a-source",
        "partition-b-target",
    );
    let (receipt_json, _) = store
        .get_state_kv(SUPERSESSION_RECEIPT_NAMESPACE, &receipt_id)
        .expect("read durable receipt state")
        .expect("receipt must be persisted in every memcore profile");
    let receipt: SupersessionReceipt =
        serde_json::from_str(&receipt_json).expect("deserialize durable receipt");
    assert_eq!(receipt.route, SUPERSESSION_ROUTE_IMMUTABLE_CLAIM);
    assert!(receipt.partition_id.is_none());
    assert!(receipt.source_revision_after >= 2);
    assert!(receipt.durable);
}

#[test]
fn rem_source_claim_is_insert_once_and_replay_safe() {
    let mut store = MemoryStore::open_in_memory().expect("open memory store");
    store
        .with_immutable_supersession_transaction(|operation| {
            operation.claim_rem_source(
                "rem-source:one",
                r#"{"store":"physical","id":"one"}"#,
                "draft-a",
                "2026-07-31T00:00:00Z",
            )
        })
        .expect("first claim");
    store
        .with_immutable_supersession_transaction(|operation| {
            operation.claim_rem_source(
                "rem-source:one",
                r#"{"store":"physical","id":"one"}"#,
                "draft-a",
                "2026-07-31T00:00:01Z",
            )
        })
        .expect("same operation replay");

    let error = store
        .with_immutable_supersession_transaction(|operation| {
            operation.claim_rem_source(
                "rem-source:one",
                r#"{"store":"physical","id":"one"}"#,
                "draft-b",
                "2026-07-31T00:00:02Z",
            )
        })
        .expect_err("competing draft must not steal the source");
    assert!(error.to_string().contains("REM source claim conflict"));
    let occupant: String = store
        .connection()
        .query_row(
            "SELECT draft_id FROM rem_source_claims WHERE source_key = 'rem-source:one'",
            [],
            |row| row.get(0),
        )
        .expect("read claim occupant");
    assert_eq!(occupant, "draft-a");
}

#[test]
fn rem_source_claim_ledger_validation_is_exact_and_sorted() {
    let mut store = MemoryStore::open_in_memory().expect("open memory store");
    store
        .with_immutable_supersession_transaction(|operation| {
            operation.claim_rem_source(
                "rem-source:b",
                r#"{"store":"physical","id":"b","revision":2}"#,
                "draft-a",
                "2026-07-31T00:00:00Z",
            )?;
            operation.claim_rem_source(
                "rem-source:a",
                r#"{"store":"physical","id":"a","revision":1}"#,
                "draft-a",
                "2026-07-31T00:00:00Z",
            )?;
            operation.claim_rem_source(
                "rem-source:c",
                r#"{"store":"physical","id":"c","revision":3}"#,
                "draft-b",
                "2026-07-31T00:00:00Z",
            )?;
            operation.validate_rem_source_claims_for_draft(
                "draft-a",
                &[
                    (
                        "rem-source:b".to_string(),
                        r#"{"store":"physical","id":"b","revision":2}"#.to_string(),
                    ),
                    (
                        "rem-source:a".to_string(),
                        r#"{"store":"physical","id":"a","revision":1}"#.to_string(),
                    ),
                ],
            )
        })
        .expect("sorted exact ledger validates");

    let wrong_identity = store
        .with_immutable_supersession_transaction(|operation| {
            operation.validate_rem_source_claims_for_draft(
                "draft-a",
                &[
                    (
                        "rem-source:a".to_string(),
                        r#"{"store":"physical","id":"a","revision":999}"#.to_string(),
                    ),
                    (
                        "rem-source:b".to_string(),
                        r#"{"store":"physical","id":"b","revision":2}"#.to_string(),
                    ),
                ],
            )
        })
        .expect_err("identity drift must fail exact ledger validation");
    assert!(wrong_identity.to_string().contains("ledger mismatch"));

    let missing_claim = store
        .with_immutable_supersession_transaction(|operation| {
            operation.validate_rem_source_claims_for_draft(
                "draft-a",
                &[(
                    "rem-source:a".to_string(),
                    r#"{"store":"physical","id":"a","revision":1}"#.to_string(),
                )],
            )
        })
        .expect_err("missing expected source must fail exact ledger validation");
    assert!(missing_claim.to_string().contains("ledger mismatch"));
}
