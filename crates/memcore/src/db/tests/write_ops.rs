use super::*;
use crate::db::update_with_revision_if_current;
use crate::store::enrichment::ENRICHMENT_AUTH_RETRY_MAX_ATTEMPTS;
use crate::MemoryStore;

#[test]
fn ordinary_upsert_preserves_existing_scored_count() {
    let mut conn = make_conn();
    let entry = make_entry("scored-upsert", "scored count survives ordinary save");
    upsert(&mut conn, &entry, false).unwrap();
    conn.execute(
        "UPDATE memories SET scored_count = 7 WHERE id = 'scored-upsert'",
        [],
    )
    .unwrap();
    upsert(&mut conn, &entry, false).unwrap();
    let scored_count: i64 = conn
        .query_row(
            "SELECT scored_count FROM memories WHERE id = 'scored-upsert'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        scored_count, 7,
        "default MemoryEntry must not erase diagnostics"
    );
}

#[test]
fn upsert_folds_persons_into_entities_without_persisting_persons_column() {
    let mut conn = make_conn();
    let mut e = make_entry("pers-1", "Kyle prefers concise handoffs");
    e.persons = vec!["Kyle".to_string()];
    e.entities = vec!["Sigil".to_string()];
    upsert(&mut conn, &e, false).unwrap();

    let has_persons_column: bool = conn
        .query_row(
            "SELECT 1 FROM pragma_table_info('memories') WHERE name='persons' LIMIT 1",
            [],
            |_| Ok(true),
        )
        .unwrap_or(false);
    assert!(!has_persons_column);

    let entities: String = conn
        .query_row("SELECT entities FROM memories WHERE id='pers-1'", [], |r| {
            r.get(0)
        })
        .unwrap();
    let ents: Vec<String> = serde_json::from_str(&entities).unwrap();
    assert!(ents.iter().any(|e| e == "Kyle"));
    assert!(ents.iter().any(|e| e == "user"));
    assert!(ents.iter().any(|e| e == "Sigil"));
}

#[test]
fn upsert_rejects_anchor_namespace_ids() {
    // tachi#773 item 4 guard (c): the `anchor:` id namespace is reserved for
    // `ensure_anchor`; ordinary upsert must fail closed, never silently
    // create or overwrite a row there.
    let mut conn = make_conn();
    let e = make_entry("anchor:issue:kckylechen1/tachi:773", "smuggled anchor row");
    let err = upsert(&mut conn, &e, false).unwrap_err();
    assert!(err.to_string().contains("anchor:"));
    assert!(err.to_string().contains("reserved"));

    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM memories WHERE id = 'anchor:issue:kckylechen1/tachi:773'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 0, "rejected write must not land any row");
}

#[test]
fn upsert_idempotent() {
    let mut conn = make_conn();
    let mut e = make_entry("dup", "first text");
    upsert(&mut conn, &e, false).unwrap();
    e.text = "updated text".into();
    upsert(&mut conn, &e, false).unwrap();

    let results = search_fts(&conn, "updated", 5, false, false, None, None, None).unwrap();
    assert!(results.contains_key("dup"));
}

#[test]
fn update_enrichment_fields_clears_stale_failure_metadata() {
    let mut conn = make_conn();
    let mut entry = make_entry(
        "enrich-reset",
        "entry with stale enrichment failure metadata",
    );
    entry.metadata = json!({
        "enrichment": {
            "status": "failed",
            "failed_stage": "embedding",
            "last_error": "Voyage 429",
            "last_failure_at": "2026-06-01T00:00:00Z",
            "retry": {
                "kind": "auth",
                "attempts": 2,
                "max_attempts": 3,
                "next_retry_at": "2026-06-01T00:05:00Z"
            }
        }
    });
    upsert(&mut conn, &entry, false).unwrap();

    let vec_blob = serialize_f32(&vec![0.1_f32; 1024]);
    update_enrichment_fields(
        &mut conn,
        "enrich-reset",
        None,
        Some(&vec_blob),
        None,
        None,
        1,
        None,
        None,
        None,
    )
    .unwrap();

    let metadata: String = conn
        .query_row(
            "SELECT metadata FROM memories WHERE id='enrich-reset'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let metadata: serde_json::Value = serde_json::from_str(&metadata).unwrap();
    assert_eq!(metadata["enrichment"]["status"], "embedded");
    assert_eq!(
        metadata["enrichment"]["last_error"],
        serde_json::Value::Null
    );
    assert!(metadata["enrichment"].get("failed_stage").is_none());
    assert!(metadata["enrichment"].get("last_failure_at").is_none());
    assert!(metadata["enrichment"].get("retry").is_none());
}

/// Mixed-stage: keyword failure must survive a later embedding success (#943).
#[test]
fn update_enrichment_fields_preserves_unresolved_keyword_failure() {
    let mut conn = make_conn();
    let mut entry = make_entry(
        "enrich-mixed",
        "keyword stage failed; embedding later succeeds",
    );
    entry.metadata = json!({
        "enrichment": {
            "status": "failed",
            "failed_stage": "keywords",
            "last_error": "extract lane timeout",
            "last_failure_at": "2026-06-01T00:00:00Z",
            "keywords_status": "failed"
        }
    });
    upsert(&mut conn, &entry, false).unwrap();

    let vec_blob = serialize_f32(&vec![0.2_f32; 1024]);
    update_enrichment_fields(
        &mut conn,
        "enrich-mixed",
        None,
        Some(&vec_blob),
        None, // keywords not written — failure unresolved
        None,
        1,
        None,
        None,
        None,
    )
    .unwrap();

    let metadata: String = conn
        .query_row(
            "SELECT metadata FROM memories WHERE id='enrich-mixed'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let metadata: serde_json::Value = serde_json::from_str(&metadata).unwrap();
    assert_eq!(
        metadata["enrichment"]["status"], "failed",
        "aggregate status must remain failed: {metadata:?}"
    );
    assert_eq!(metadata["enrichment"]["failed_stage"], "keywords");
    assert_eq!(metadata["enrichment"]["last_error"], "extract lane timeout");
    assert_eq!(metadata["enrichment"]["keywords_status"], "failed");
    assert_eq!(
        metadata["enrichment"]["partial_success_status"], "embedded",
        "successful stage still recorded: {metadata:?}"
    );
    assert!(
        metadata["enrichment"].get("last_success_at").is_some(),
        "last_success_at stamped on partial success"
    );
}

#[test]
fn enrichment_receipts_commit_with_matching_fields_and_preserve_other_metadata() {
    let mut conn = make_conn();
    let mut entry = make_entry("enrich-receipts", "field/receipt atomicity probe");
    entry.metadata = json!({
        "provenance": {"origin": "kept"},
        "enrichment": {
            "invocations": {"embedding": {"engine": "voyage"}},
            "unrelated": "kept"
        }
    });
    upsert(&mut conn, &entry, false).unwrap();

    let keywords = vec!["receipt-keyword".to_string()];
    let entities = vec!["receipt-entity".to_string()];
    let summary_receipt = r#"{"schema":"model-invocation-v1","attempt":"summary-winner"}"#;
    let metadata_receipt = r#"{"schema":"model-invocation-v1","attempt":"metadata-winner"}"#;
    let keywords_receipt = r#"{"schema":"model-invocation-v1","attempt":"keywords-winner"}"#;

    assert!(update_enrichment_fields(
        &mut conn,
        &entry.id,
        Some("receipt-bearing summary"),
        None,
        Some(&keywords),
        Some(&entities),
        entry.revision,
        Some(summary_receipt),
        Some(metadata_receipt),
        Some(keywords_receipt),
    )
    .expect("matching revision accepts field/receipt bundle"));

    let (summary, stored_metadata): (String, String) = conn
        .query_row(
            "SELECT summary, metadata FROM memories WHERE id = ?1",
            params![&entry.id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    let metadata: serde_json::Value = serde_json::from_str(&stored_metadata).unwrap();
    assert_eq!(summary, "receipt-bearing summary");
    assert_eq!(metadata["provenance"]["origin"], "kept");
    assert_eq!(metadata["enrichment"]["unrelated"], "kept");
    assert_eq!(
        metadata["enrichment"]["invocations"]["embedding"]["engine"],
        "voyage"
    );
    assert_eq!(
        metadata["enrichment"]["invocations"]["summary"]["attempt"],
        "summary-winner"
    );
    assert_eq!(
        metadata["enrichment"]["invocations"]["metadata"]["attempt"],
        "metadata-winner"
    );
    assert_eq!(
        metadata["enrichment"]["invocations"]["keywords"]["attempt"],
        "keywords-winner"
    );
}

#[test]
fn stale_or_receipt_only_enrichment_never_persists_a_receipt() {
    let mut conn = make_conn();
    let entry = make_entry("enrich-receipt-reject", "receipt rejection probe");
    upsert(&mut conn, &entry, false).unwrap();
    let receipt = r#"{"schema":"model-invocation-v1","attempt":"rejected"}"#;

    assert!(!update_enrichment_fields(
        &mut conn,
        &entry.id,
        Some("stale summary"),
        None,
        None,
        None,
        entry.revision + 1,
        Some(receipt),
        None,
        None,
    )
    .expect("revision drift must be a clean rejection"));
    let receipt_only = update_enrichment_fields(
        &mut conn,
        &entry.id,
        None,
        None,
        None,
        None,
        entry.revision,
        Some(receipt),
        None,
        None,
    )
    .expect_err("receipt-only success must be rejected");
    assert!(receipt_only
        .to_string()
        .contains("receipt requires an accepted generated field"));

    let (summary, metadata): (String, String) = conn
        .query_row(
            "SELECT summary, metadata FROM memories WHERE id = ?1",
            params![&entry.id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(summary, entry.summary);
    let metadata: serde_json::Value = serde_json::from_str(&metadata).unwrap();
    assert!(
        metadata
            .pointer("/enrichment/invocations/summary")
            .is_none(),
        "rejected writes must not leave a receipt: {metadata:?}"
    );
}

#[test]
fn enrichment_sql_failure_rolls_back_field_and_receipt_together() {
    let mut conn = make_conn();
    let entry = make_entry("enrich-receipt-rollback", "rollback probe");
    upsert(&mut conn, &entry, false).unwrap();
    conn.execute_batch(
        r#"
        CREATE TRIGGER fail_enrichment_field_receipt_cas
        AFTER UPDATE OF summary, metadata ON memories
        WHEN NEW.id = 'enrich-receipt-rollback'
        BEGIN
            SELECT RAISE(ABORT, 'forced field/receipt CAS failure');
        END;
        "#,
    )
    .unwrap();
    let receipt = r#"{"schema":"model-invocation-v1","attempt":"must-rollback"}"#;

    let error = update_enrichment_fields(
        &mut conn,
        &entry.id,
        Some("must not commit"),
        None,
        None,
        None,
        entry.revision,
        Some(receipt),
        None,
        None,
    )
    .expect_err("forced field/receipt CAS failure must abort the transaction");
    assert!(error
        .to_string()
        .contains("forced field/receipt CAS failure"));

    let (summary, metadata): (String, String) = conn
        .query_row(
            "SELECT summary, metadata FROM memories WHERE id = ?1",
            params![&entry.id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(summary, entry.summary, "field write must roll back");
    let metadata: serde_json::Value = serde_json::from_str(&metadata).unwrap();
    assert!(
        metadata
            .pointer("/enrichment/invocations/summary")
            .is_none(),
        "receipt must roll back with its field: {metadata:?}"
    );
}

#[test]
fn accepted_retry_replaces_only_its_stage_receipt() {
    let mut conn = make_conn();
    let entry = make_entry("enrich-retry-receipt", "retry attribution probe");
    upsert(&mut conn, &entry, false).unwrap();
    let rejected = r#"{"schema":"model-invocation-v1","attempt":"rejected-retry"}"#;
    let accepted = r#"{"schema":"model-invocation-v1","attempt":"accepted-retry"}"#;
    let metadata_receipt = r#"{"schema":"model-invocation-v1","attempt":"metadata-stage"}"#;
    let entities = vec!["retry-entity".to_string()];

    assert!(!update_enrichment_fields(
        &mut conn,
        &entry.id,
        Some("rejected retry summary"),
        None,
        None,
        None,
        entry.revision + 1,
        Some(rejected),
        None,
        None,
    )
    .unwrap());
    assert!(update_enrichment_fields(
        &mut conn,
        &entry.id,
        Some("accepted retry summary"),
        None,
        None,
        None,
        entry.revision,
        Some(accepted),
        None,
        None,
    )
    .unwrap());
    assert!(update_enrichment_fields(
        &mut conn,
        &entry.id,
        None,
        None,
        None,
        Some(&entities),
        entry.revision,
        None,
        Some(metadata_receipt),
        None,
    )
    .unwrap());

    let metadata: String = conn
        .query_row(
            "SELECT metadata FROM memories WHERE id = ?1",
            params![&entry.id],
            |row| row.get(0),
        )
        .unwrap();
    let metadata: serde_json::Value = serde_json::from_str(&metadata).unwrap();
    assert_eq!(
        metadata["enrichment"]["invocations"]["summary"]["attempt"],
        "accepted-retry"
    );
    assert_eq!(
        metadata["enrichment"]["invocations"]["metadata"]["attempt"],
        "metadata-stage"
    );
    assert!(
        !metadata.to_string().contains("rejected-retry"),
        "a rejected retry must never be attributed as success: {metadata:?}"
    );
}

#[test]
fn set_keyword_enrichment_pending_if_unset_does_not_overwrite_terminal() {
    let mut conn = make_conn();
    let entry = make_entry("kw-pending-race", "pending must not clobber terminal");
    upsert(&mut conn, &entry, false).unwrap();

    assert!(set_keyword_enrichment_pending_if_unset(&conn, "kw-pending-race").unwrap());
    let pending: String = conn
        .query_row(
            "SELECT json_extract(metadata, '$.enrichment.keywords_status') FROM memories WHERE id='kw-pending-race'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(pending, "pending");

    set_keyword_enrichment_status(&conn, "kw-pending-race", "enriched").unwrap();
    assert!(
        !set_keyword_enrichment_pending_if_unset(&conn, "kw-pending-race").unwrap(),
        "pending must not overwrite terminal enriched"
    );
    let still: String = conn
        .query_row(
            "SELECT json_extract(metadata, '$.enrichment.keywords_status') FROM memories WHERE id='kw-pending-race'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(still, "enriched");
}

#[test]
fn record_auth_failed_enrichment_failure_carries_bounded_retry_metadata() {
    let mut conn = make_conn();
    let entry = make_entry(
        "auth-retry",
        "auth failed enrichment retry should be bounded",
    );
    upsert(&mut conn, &entry, false).unwrap();

    record_enrichment_failure(
        &conn,
        "auth-retry",
        "embedding",
        "API key unavailable for [VOYAGE_API_KEY]: all configured provider keys are unusable (auth_failed: 2)",
    )
    .unwrap();

    let metadata: String = conn
        .query_row(
            "SELECT metadata FROM memories WHERE id='auth-retry'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let metadata: serde_json::Value = serde_json::from_str(&metadata).unwrap();
    assert_eq!(metadata["enrichment"]["status"], "failed");
    assert_eq!(metadata["enrichment"]["retry"]["kind"], "auth");
    assert_eq!(metadata["enrichment"]["retry"]["attempts"], 0);
    assert_eq!(
        metadata["enrichment"]["retry"]["max_attempts"],
        ENRICHMENT_AUTH_RETRY_MAX_ATTEMPTS
    );
    assert!(
        metadata["enrichment"]["retry"]["next_retry_at"]
            .as_str()
            .is_some_and(|value| !value.is_empty()),
        "auth-class failure should be immediately retry-eligible after unlock: {metadata:?}"
    );
}

#[test]
fn auth_failed_enrichment_retry_claim_respects_backoff_and_attempt_cap() {
    let dir = tempfile::tempdir().expect("temp db dir");
    let db = dir.path().join("memory.db");
    let mut store = MemoryStore::open(db.to_str().expect("db path")).expect("open store");
    let mut entry = make_entry("auth-claim", "auth failed row should retry after unlock");
    entry.summary.clear();
    entry.keywords.clear();
    entry.entities.clear();
    store.upsert(&entry).expect("insert retry row");
    store
        .record_enrichment_failure(
            "auth-claim",
            "embedding",
            "Voyage batch API error: 401 Unauthorized",
        )
        .expect("record auth failure");

    let first = store
        .claim_auth_failed_enrichment_retries(10)
        .expect("claim retry");
    assert_eq!(first.len(), 1);
    assert_eq!(first[0].id, "auth-claim");
    assert_eq!(first[0].attempts, 0);
    assert!(first[0].max_attempts >= 1);
    assert!(first[0].needs_embedding);
    assert!(first[0].needs_summary);
    assert!(first[0].needs_metadata);

    let blocked = store
        .claim_auth_failed_enrichment_retries(10)
        .expect("backoff should block immediate reclain");
    assert!(
        blocked.is_empty(),
        "retry should honor next_retry_at backoff"
    );

    {
        // The capped-attempt fixture needs a state no production API creates;
        // keep raw fixture SQL explicitly authorized and tightly scoped.
        let _fixture_sql =
            crate::db::authorize_reserved_reference_write(&store.reserved_reference_write)
                .expect("authorize capped retry fixture SQL");
        store
            .connection()
            .execute(
                "UPDATE memories
                 SET metadata = json_set(metadata,
                     '$.enrichment.retry.attempts', ?1,
                     '$.enrichment.retry.next_retry_at', '1970-01-01T00:00:00.000Z')
                 WHERE id = 'auth-claim'",
                rusqlite::params![ENRICHMENT_AUTH_RETRY_MAX_ATTEMPTS],
            )
            .unwrap();
    }
    let capped = store
        .claim_auth_failed_enrichment_retries(10)
        .expect("claim capped retry");
    assert!(capped.is_empty(), "max retry attempts must stop storms");
}

#[test]
fn upsert_defaults_valid_from_to_timestamp() {
    let mut conn = make_conn();
    let mut entry = make_entry("temporal-default", "temporal default memory");
    entry.timestamp = "2026-01-01T00:00:00Z".to_string();
    entry.valid_from = String::new();

    upsert(&mut conn, &entry, false).unwrap();

    let stored = fetch_by_ids(&conn, &["temporal-default".to_string()], false)
        .unwrap()
        .remove("temporal-default")
        .unwrap();
    assert_eq!(stored.valid_from, "2026-01-01T00:00:00.000Z");
    assert_eq!(stored.valid_until, None);
}

#[test]
fn init_schema_backfills_valid_from_for_legacy_rows() {
    crate::db::enable_simple_auto_extension().unwrap();
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        r#"
        CREATE TABLE memories (
            id TEXT PRIMARY KEY,
            path TEXT NOT NULL DEFAULT '/',
            summary TEXT NOT NULL DEFAULT '',
            text TEXT NOT NULL DEFAULT '',
            importance REAL NOT NULL DEFAULT 0.7,
            timestamp TEXT NOT NULL,
            category TEXT NOT NULL DEFAULT 'fact',
            topic TEXT NOT NULL DEFAULT '',
            keywords TEXT NOT NULL DEFAULT '[]',
            entities TEXT NOT NULL DEFAULT '[]',
            location TEXT NOT NULL DEFAULT '',
            source TEXT NOT NULL DEFAULT 'manual',
            scope TEXT NOT NULL DEFAULT 'general',
            archived INTEGER NOT NULL DEFAULT 0,
            created_at TEXT NOT NULL DEFAULT '',
            updated_at TEXT NOT NULL DEFAULT '',
            access_count INTEGER NOT NULL DEFAULT 0,
            last_access TEXT,
            revision INTEGER NOT NULL DEFAULT 1,
            metadata TEXT NOT NULL DEFAULT '{}',
            retention_policy TEXT,
            domain TEXT,
            superseded_by TEXT
        );
        INSERT INTO memories
            (id, text, timestamp, keywords, entities, metadata)
        VALUES
            ('legacy-valid-from', 'legacy temporal row', '2026-02-03T04:05:06Z', '[]', '[]', '{}');
        "#,
    )
    .unwrap();

    init_schema(&conn).unwrap();

    let valid_from: String = conn
        .query_row(
            "SELECT valid_from FROM memories WHERE id = 'legacy-valid-from'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(valid_from, "2026-02-03T04:05:06.000Z");
}

#[test]
fn update_with_revision_detects_conflict() {
    let mut conn = make_conn();
    let e = make_entry("rev-1", "original");
    upsert(&mut conn, &e, false).unwrap();

    let metadata = serde_json::to_string(&json!({"source":"test"})).unwrap();
    let ok = update_with_revision(
        &mut conn,
        "rev-1",
        "merged",
        "merged",
        "consolidation",
        &metadata,
        None,
        1,
    )
    .unwrap();
    assert!(ok);

    let stale = update_with_revision(
        &mut conn,
        "rev-1",
        "stale",
        "stale",
        "consolidation",
        &metadata,
        None,
        1,
    )
    .unwrap();
    assert!(!stale);
}

#[test]
fn guarded_revision_update_rejects_same_revision_enrichment_drift() {
    let mut conn = make_conn();
    let mut entry = make_entry("guarded-rev-1", "original");
    entry.vector = Some(vec![0.11; 1024]);
    upsert(&mut conn, &entry, true).unwrap();

    let enriched_vector = vec![0.91; 1024];
    assert!(update_enrichment_fields(
        &mut conn,
        &entry.id,
        Some("generated after snapshot"),
        Some(&serialize_f32(&enriched_vector)),
        None,
        None,
        entry.revision,
        None,
        None,
        None,
    )
    .unwrap());

    let metadata = serde_json::to_string(&json!({"migration": "must-not-land"})).unwrap();
    let original_vector = entry.vector.as_deref().map(serialize_f32);
    let updated = update_with_revision_if_current(
        &mut conn,
        &entry.id,
        &entry.text,
        &entry.summary,
        &entry.source,
        &metadata,
        original_vector.as_deref(),
        entry.revision,
        |current, superseded_by| {
            current.summary == entry.summary
                && current.vector == entry.vector
                && superseded_by.is_none()
        },
    )
    .unwrap();
    assert!(!updated, "same-revision generated-field drift must refuse");

    let current = fetch_by_ids(&conn, std::slice::from_ref(&entry.id), true)
        .unwrap()
        .remove(&entry.id)
        .unwrap();
    assert_eq!(current.revision, entry.revision);
    assert_eq!(current.summary, "generated after snapshot");
    assert_eq!(current.vector, Some(enriched_vector));
    assert!(current.metadata.get("migration").is_none());
}

#[test]
fn normalize_for_write_clamps_fields() {
    let mut e = make_entry("norm-1", "normalize test");
    e.path = "project/alpha".into();
    e.source = "user".into();
    e.category = "FACT".into();
    e.scope = "PROJECT".into();
    e.importance = 1.5;
    normalize_for_write(&mut e);
    assert!(
        e.path.starts_with('/'),
        "path should be normalized to start with /"
    );
    assert_eq!(e.category, "fact", "category should be lowercase");
    assert_eq!(e.scope, "project", "scope should be lowercase");
    assert!(
        (e.importance - 1.0).abs() < f64::EPSILON,
        "importance should be clamped to 1.0"
    );
}

#[test]
fn normalize_for_write_empty_id_rejected_by_upsert() {
    let e = make_entry(" ", "empty id test");
    let err = upsert(&mut make_conn(), &e, false);
    assert!(err.is_err(), "empty id should be rejected");
}
