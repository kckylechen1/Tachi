use super::*;

#[test]
fn gc_tables_prunes_retention_and_orphans() {
    let mut conn = make_conn();
    let e = make_entry("gc-keep", "gc target");
    upsert(&mut conn, &e, false).unwrap();

    for _ in 0..300 {
        conn.execute(
            "INSERT INTO access_history (memory_id, accessed_at, query_hash) VALUES (?1, ?2, ?3)",
            params!["gc-keep", now_utc_iso(), ""],
        )
        .unwrap();
    }
    conn.execute(
        "INSERT INTO access_history (memory_id, accessed_at, query_hash) VALUES (?1, ?2, ?3)",
        params!["gc-orphan", now_utc_iso(), ""],
    )
    .unwrap();

    conn.execute(
            "INSERT INTO processed_events (event_hash, event_id, worker, created_at) VALUES (?1, ?2, ?3, ?4)",
            params!["ev-old", "id-old", "ingest", "2000-01-01T00:00:00.000Z"],
        )
        .unwrap();
    conn.execute(
            "INSERT INTO processed_events (event_hash, event_id, worker, created_at) VALUES (?1, ?2, ?3, ?4)",
            params!["ev-new", "id-new", "ingest", "2999-01-01T00:00:00.000Z"],
        )
        .unwrap();

    conn.execute(
            "INSERT INTO audit_log (timestamp, server_id, tool_name, args_hash, success, duration_ms, error_kind, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                "2000-01-01T00:00:00.000Z",
                "mcp:test",
                "tool_old",
                "",
                1,
                1,
                Option::<String>::None,
                "2000-01-01T00:00:00.000Z"
            ],
        )
        .unwrap();
    conn.execute(
            "INSERT INTO audit_log (timestamp, server_id, tool_name, args_hash, success, duration_ms, error_kind, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                "2999-01-01T00:00:00.000Z",
                "mcp:test",
                "tool_new",
                "",
                1,
                1,
                Option::<String>::None,
                "2999-01-01T00:00:00.000Z"
            ],
        )
        .unwrap();

    conn.execute(
            "INSERT INTO agent_known_state (agent_id, memory_id, revision, synced_at) VALUES (?1, ?2, ?3, ?4)",
            params!["agent-old", "gc-keep", 1, "2000-01-01T00:00:00.000Z"],
        )
        .unwrap();
    conn.execute(
            "INSERT INTO agent_known_state (agent_id, memory_id, revision, synced_at) VALUES (?1, ?2, ?3, ?4)",
            params!["agent-new", "gc-keep", 2, "2999-01-01T00:00:00.000Z"],
        )
        .unwrap();
    conn.execute(
            "INSERT INTO agent_known_state (agent_id, memory_id, revision, synced_at) VALUES (?1, ?2, ?3, ?4)",
            params!["agent-orphan", "gc-orphan", 1, "2999-01-01T00:00:00.000Z"],
        )
        .unwrap();

    let summary = gc_tables(&mut conn, &GcConfig::default(), StoreProfile::TachiFull).unwrap();

    let kept_access: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM access_history WHERE memory_id = ?1",
            params!["gc-keep"],
            |row| row.get(0),
        )
        .unwrap();
    let orphan_access: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM access_history WHERE memory_id = ?1",
            params!["gc-orphan"],
            |row| row.get(0),
        )
        .unwrap();
    let processed_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM processed_events", [], |row| {
            row.get(0)
        })
        .unwrap();
    let audit_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM audit_log", [], |row| row.get(0))
        .unwrap();
    let known_state_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM agent_known_state", [], |row| {
            row.get(0)
        })
        .unwrap();
    let orphan_known_state: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM agent_known_state WHERE memory_id = ?1",
            params!["gc-orphan"],
            |row| row.get(0),
        )
        .unwrap();

    assert_eq!(
        kept_access, 256,
        "access_history should retain latest 256 per memory"
    );
    assert_eq!(orphan_access, 0, "orphaned access rows should be removed");
    assert_eq!(
        processed_count, 1,
        "old processed_events row should be pruned"
    );
    assert_eq!(audit_count, 1, "old audit_log row should be pruned");
    assert_eq!(
        known_state_count, 1,
        "old + orphan known-state rows should be pruned"
    );
    assert_eq!(
        orphan_known_state, 0,
        "orphaned known-state rows should be removed"
    );

    assert!(summary["access_history_pruned"].as_u64().unwrap_or(0) > 0);
    assert!(summary["orphaned_agent_known_state"].as_u64().unwrap_or(0) > 0);
}

#[test]
fn gc_tables_reconciles_query_diversity_after_prune() {
    let mut conn = make_conn();
    let e = make_entry("gc-qd", "diversity target");
    upsert(&mut conn, &e, false).unwrap();

    for i in 0..5 {
        let hash = format!("hash-{i}");
        conn.execute(
            "INSERT INTO access_history (memory_id, accessed_at, query_hash) VALUES (?1, ?2, ?3)",
            params!["gc-qd", now_utc_iso(), hash],
        )
        .unwrap();
    }
    conn.execute(
        "UPDATE memories SET query_diversity = 99 WHERE id = ?1",
        params!["gc-qd"],
    )
    .unwrap();

    let cfg = GcConfig {
        access_history_keep_per_memory: 2,
        ..GcConfig::default()
    };
    gc_tables(&mut conn, &cfg, StoreProfile::TachiFull).unwrap();

    let qd: i64 = conn
        .query_row(
            "SELECT query_diversity FROM memories WHERE id = ?1",
            params!["gc-qd"],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        qd, 2,
        "query_diversity should match distinct query hashes kept after GC"
    );
}

/// tachi#1446: the access-history quota is per `(memory_id, event_kind)`.
///
/// Display events outnumber use events by construction, so a quota that
/// partitioned by `memory_id` alone would spend the entire budget on display
/// rows and delete the rare use rows first — silently, since the summary
/// counter reports the same "pruned N" either way. The fixture is built so a
/// single-partition quota provably fails it: with a budget of 2 and 10 display
/// rows all NEWER than the 2 use rows, a `PARTITION BY memory_id` quota keeps
/// two display rows and deletes BOTH use rows.
#[test]
fn gc_tables_gives_each_event_kind_its_own_quota() {
    let mut conn = make_conn();
    let e = make_entry("gc-kinds", "per-kind quota target");
    upsert(&mut conn, &e, false).unwrap();

    // Use events first, so they are the OLDEST rows: under a single partition
    // ordered by accessed_at DESC they are exactly the rows that get cut.
    for i in 0..2 {
        conn.execute(
            "INSERT INTO access_history (memory_id, accessed_at, query_hash, event_kind)
             VALUES (?1, ?2, '', 'use')",
            params!["gc-kinds", format!("2026-01-01T00:00:0{i}Z")],
        )
        .unwrap();
    }
    for i in 0..10 {
        conn.execute(
            "INSERT INTO access_history (memory_id, accessed_at, query_hash, event_kind)
             VALUES (?1, ?2, '', 'display')",
            params!["gc-kinds", format!("2026-06-01T00:00:{i:02}Z")],
        )
        .unwrap();
    }

    let cfg = GcConfig {
        access_history_keep_per_memory: 2,
        ..GcConfig::default()
    };
    gc_tables(&mut conn, &cfg, StoreProfile::TachiFull).unwrap();

    let count_kind = |kind: &str| -> i64 {
        conn.query_row(
            "SELECT COUNT(*) FROM access_history WHERE memory_id = 'gc-kinds' AND event_kind = ?1",
            params![kind],
            |row| row.get(0),
        )
        .unwrap()
    };

    assert_eq!(
        count_kind("use"),
        2,
        "both use events must survive a prune that removes display events — they are the \
         scarce provenance the ranking knob depends on"
    );
    assert_eq!(
        count_kind("display"),
        2,
        "display events must still be capped at the configured quota"
    );
}

// ── Auto-archive receipts (tachi#1463) ───────────────────────────────────────
//
// `archive_stale_memories` had no test at all before this block, which is the
// same gap as the missing receipt wearing a different hat: a scheduled sweep
// that silently changes what recall returns, with nothing asserting either its
// effect or its record.

/// Seed a row that the named archival pass will claim, backdating the recency
/// columns directly so the fixture does not depend on `upsert`'s normalization
/// of `timestamp`.
fn seed_archivable(
    conn: &mut Connection,
    id: &str,
    importance: f64,
    retention_policy: Option<&str>,
    last_use_at: Option<&str>,
) {
    let mut entry = make_entry(id, &format!("archivable row {id}"));
    entry.importance = importance;
    entry.retention_policy = retention_policy.map(str::to_string);
    upsert(conn, &entry, false).unwrap();
    conn.execute(
        "UPDATE memories
            SET timestamp = '2000-01-01T00:00:00.000Z',
                updated_at = '2000-01-01T00:00:00.000Z',
                last_use_at = ?2
          WHERE id = ?1",
        params![id, last_use_at],
    )
    .unwrap();
}

fn gc_archival_receipts(conn: &Connection) -> Vec<TachiEventRecord> {
    list_tachi_events(
        conn,
        &TachiEventQuery {
            event_type: Some(GC_MEMORY_ARCHIVED_EVENT_TYPE.to_string()),
            limit: 50,
            ..TachiEventQuery::default()
        },
    )
    .unwrap()
}

fn row_field<T: rusqlite::types::FromSql>(conn: &Connection, id: &str, column: &str) -> T {
    conn.query_row(
        &format!("SELECT {column} FROM memories WHERE id = ?1"),
        params![id],
        |row| row.get(0),
    )
    .unwrap()
}

#[test]
fn gc_archival_writes_a_receipt_naming_rows_predicate_and_threshold() {
    let mut conn = make_conn();
    seed_archivable(&mut conn, "gc-arch-stale", 0.2, None, None);

    let archived = archive_stale_memories(&conn, 90).unwrap();
    assert_eq!(archived, 1);

    let events = gc_archival_receipts(&conn);
    assert_eq!(
        events.len(),
        1,
        "a sweep that archived a row must leave exactly one receipt; \
         a bare count into stderr is what made proving GC's history a forensic exercise"
    );
    let payload = &events[0].payload;
    assert_eq!(
        payload["stale_days"],
        json!(90),
        "the receipt must name the threshold it applied"
    );
    assert_eq!(payload["archived_total"], json!(1));

    let passes = payload["passes"].as_array().expect("passes array");
    let fired: Vec<&serde_json::Value> = passes
        .iter()
        .filter(|pass| pass["archived_count"].as_u64() == Some(1))
        .collect();
    assert_eq!(
        fired.len(),
        1,
        "exactly one predicate should have claimed the row"
    );
    assert_eq!(
        fired[0]["predicate"],
        json!("durable_never_used_by_timestamp")
    );
    assert_eq!(fired[0]["recency_column"], json!("timestamp"));
    assert_eq!(fired[0]["importance_below"], json!(0.3));
    assert_eq!(
        fired[0]["memory_ids"],
        json!(["gc-arch-stale"]),
        "the receipt must name which rows moved, not just how many"
    );

    assert_eq!(events[0].authority, AuthorityLevel::RawFact);
    assert!(
        events[0].effects.contains(&EffectScope::Recall),
        "archiving drops rows out of default search, so the receipt must declare a recall effect; \
         got {:?}",
        events[0].effects
    );
}

#[test]
fn gc_archival_bumps_revision_so_restore_cas_refuses_a_pre_gc_revision() {
    let mut conn = make_conn();
    seed_archivable(&mut conn, "gc-arch-cas", 0.2, None, None);
    let pre_gc_revision: i64 = row_field(&conn, "gc-arch-cas", "revision");

    assert_eq!(archive_stale_memories(&conn, 90).unwrap(), 1);

    assert!(
        !restore_archived_if_revision(&conn, "gc-arch-cas", pre_gc_revision).unwrap(),
        "a caller holding the pre-GC revision must NOT be able to un-archive the row: the CAS \
         guard exists to detect 'state changed under me', and a GC sweep is the one mutation no \
         caller can observe. If this passes, GC archived without moving `revision` and the guard \
         is blind to it."
    );
    assert_eq!(
        row_field::<i64>(&conn, "gc-arch-cas", "archived"),
        1,
        "the refused restore must leave the row archived"
    );

    let post_gc_revision: i64 = row_field(&conn, "gc-arch-cas", "revision");
    assert_eq!(post_gc_revision, pre_gc_revision + 1);
    assert!(
        restore_archived_if_revision(&conn, "gc-arch-cas", post_gc_revision).unwrap(),
        "a caller that re-reads the post-GC revision must still be able to restore"
    );
}

#[test]
fn gc_archival_moves_updated_at_off_its_pre_sweep_value() {
    let mut conn = make_conn();
    seed_archivable(&mut conn, "gc-arch-touch", 0.2, None, None);
    let before: String = row_field(&conn, "gc-arch-touch", "updated_at");
    assert_eq!(before, "2000-01-01T00:00:00.000Z");

    assert_eq!(archive_stale_memories(&conn, 90).unwrap(), 1);

    let after: String = row_field(&conn, "gc-arch-touch", "updated_at");
    assert_ne!(
        after, before,
        "every sibling archival path sets `updated_at` alongside the revision bump; a row mutated \
         at a time its `updated_at` does not mention is a column that lies"
    );
    let receipt = gc_archival_receipts(&conn);
    assert_eq!(
        receipt[0].payload["archived_at"],
        json!(after),
        "the receipt's archival time must be the same instant stamped on the rows"
    );
}

#[test]
fn gc_archival_writes_no_receipt_when_nothing_qualifies() {
    let mut conn = make_conn();
    // Fresh, important, and therefore claimed by no predicate.
    let entry = make_entry("gc-arch-fresh", "fresh important row");
    upsert(&mut conn, &entry, false).unwrap();

    assert_eq!(archive_stale_memories(&conn, 90).unwrap(), 0);
    assert!(
        gc_archival_receipts(&conn).is_empty(),
        "nothing prunes `tachi_events`; a sweep that changed no state must not add a row to it \
         every six hours forever"
    );
}

#[test]
fn gc_archival_does_not_treat_display_as_use() {
    let mut conn = make_conn();
    seed_archivable(&mut conn, "gc-display-only", 0.2, Some("durable"), None);
    conn.execute(
        "UPDATE memories SET last_access = '2999-01-01T00:00:00.000Z' WHERE id = 'gc-display-only'",
        [],
    )
    .unwrap();

    assert_eq!(
        archive_stale_memories(&conn, 90).unwrap(),
        1,
        "a fresh display timestamp must not protect an old, low-value memory that has no genuine use event"
    );
    let receipts = gc_archival_receipts(&conn);
    let fired = receipts[0].payload["passes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|pass| pass["archived_count"].as_u64() == Some(1))
        .unwrap();
    assert_eq!(fired["recency_column"], json!("timestamp"));
    assert_eq!(fired["predicate"], json!("durable_never_used_by_timestamp"));
}

#[test]
fn gc_archival_spares_exempt_rows_and_leaves_them_out_of_the_receipt() {
    let mut conn = make_conn();
    seed_archivable(&mut conn, "gc-arch-permanent", 0.1, Some("permanent"), None);
    seed_archivable(&mut conn, "gc-arch-pinned", 0.1, Some("pinned"), None);

    assert_eq!(
        archive_stale_memories(&conn, 90).unwrap(),
        0,
        "permanent and pinned rows are GC-exempt however stale and low-value they are"
    );
    assert_eq!(row_field::<i64>(&conn, "gc-arch-permanent", "archived"), 0);
    assert_eq!(row_field::<i64>(&conn, "gc-arch-pinned", "archived"), 0);
    assert_eq!(
        row_field::<i64>(&conn, "gc-arch-permanent", "revision"),
        1,
        "an untouched row must not have its revision moved — a spurious bump would break other \
         callers' CAS for no state change at all"
    );
    assert!(gc_archival_receipts(&conn).is_empty());
}

#[test]
fn gc_archival_receipt_attributes_each_row_to_the_predicate_that_took_it() {
    let mut conn = make_conn();
    // importance 0.4: below the ephemeral-by-last_access bar (0.7), above the
    // durable-by-last_access bar (0.5) — so only the ephemeral pass can take it.
    seed_archivable(
        &mut conn,
        "gc-arch-ephemeral",
        0.4,
        Some("ephemeral"),
        Some("2000-01-01T00:00:00.000Z"),
    );
    seed_archivable(&mut conn, "gc-arch-durable", 0.2, Some("durable"), None);

    assert_eq!(archive_stale_memories(&conn, 90).unwrap(), 2);

    let events = gc_archival_receipts(&conn);
    assert_eq!(
        events.len(),
        1,
        "one sweep writes one receipt covering every pass"
    );
    let passes = events[0].payload["passes"].as_array().unwrap();
    let ids_for = |name: &str| -> serde_json::Value {
        passes
            .iter()
            .find(|pass| pass["predicate"] == json!(name))
            .unwrap_or_else(|| panic!("receipt must carry a `{name}` pass"))["memory_ids"]
            .clone()
    };
    assert_eq!(
        ids_for("ephemeral_stale_by_last_use"),
        json!(["gc-arch-ephemeral"])
    );
    assert_eq!(
        ids_for("durable_never_used_by_timestamp"),
        json!(["gc-arch-durable"])
    );
    assert_eq!(
        ids_for("durable_stale_by_last_use"),
        json!([]),
        "a predicate that took nothing must report an empty list, not be omitted"
    );
    assert_eq!(events[0].payload["archived_total"], json!(2));
}

#[test]
fn gc_archival_receipt_names_every_row_beyond_the_former_sample_boundary() {
    let mut conn = make_conn();
    let expected_ids: Vec<String> = (0..501)
        .map(|index| format!("gc-arch-all-{index:03}"))
        .collect();
    for id in &expected_ids {
        seed_archivable(&mut conn, id, 0.2, None, None);
    }

    assert_eq!(
        archive_stale_memories(&conn, 90).unwrap(),
        expected_ids.len() as u64
    );
    let receipt = gc_archival_receipts(&conn);
    let passes = receipt[0].payload["passes"].as_array().unwrap();
    let pass = passes
        .iter()
        .find(|pass| pass["predicate"] == json!("durable_never_used_by_timestamp"))
        .unwrap();
    let ids: Vec<String> = serde_json::from_value(pass["memory_ids"].clone()).unwrap();
    assert_eq!(pass["archived_count"], json!(expected_ids.len()));
    assert_eq!(ids, expected_ids);
}

#[test]
fn gc_archival_receipts_survive_the_table_sweep_that_reaps_audit_log() {
    let mut conn = make_conn();
    seed_archivable(&mut conn, "gc-arch-durable-receipt", 0.2, None, None);
    assert_eq!(archive_stale_memories(&conn, 90).unwrap(), 1);
    assert_eq!(gc_archival_receipts(&conn).len(), 1);

    // Backdate the receipt past every retention window `gc_tables` enforces,
    // then run the sweep that prunes `audit_log` by exactly those windows.
    conn.execute(
        "UPDATE tachi_events SET created_at = '2000-01-01T00:00:00.000Z'",
        [],
    )
    .unwrap();
    let cfg = GcConfig {
        audit_log_max_days: 1,
        audit_log_max_rows: 1,
        processed_events_max_days: 1,
        agent_known_state_max_days: 1,
        ..GcConfig::default()
    };
    gc_tables(&mut conn, &cfg, StoreProfile::TachiFull).unwrap();

    assert_eq!(
        gc_archival_receipts(&conn).len(),
        1,
        "the receipt must outlive the collector it records. This is why it is not in `audit_log`: \
         `gc_tables` prunes that table, so GC would reap its own evidence."
    );
}

#[test]
fn gc_identity_admissions_prunes_aged_unreferenced_and_preserves_invariants() {
    let conn = make_conn();
    crate::db::schema::install_harness_session_attachments_schema(&conn).unwrap();
    let now: jiff::Timestamp = "2026-11-20T00:00:00Z".parse().unwrap();
    let old = "2026-09-01T00:00:00.000Z";
    let older = "2026-08-01T00:00:00.000Z";
    let recent = "2026-11-10T00:00:00.000Z";

    conn.execute_batch(
        "INSERT INTO agent_identities(agent_identity_id, created_at) VALUES
            ('agent-revoked', '2026-07-01T00:00:00Z'),
            ('agent-verified', '2026-07-01T00:00:00Z'),
            ('agent-a2a', '2026-07-01T00:00:00Z'),
            ('agent-attached', '2026-07-01T00:00:00Z');",
    )
    .unwrap();

    // 1. Old rejected without identity -> MUST be pruned.
    conn.execute(
        "INSERT INTO identity_admissions
            (admission_id, agent_identity_id, connection_id, state, rejection_evidence, created_at)
         VALUES ('rej-old', NULL, 'conn-rej-old', 'rejected', 'missing identity', ?1)",
        params![old],
    )
    .unwrap();
    // 2. Recent rejected without identity -> MUST stay.
    conn.execute(
        "INSERT INTO identity_admissions
            (admission_id, agent_identity_id, connection_id, state, rejection_evidence, created_at)
         VALUES ('rej-recent', NULL, 'conn-rej-recent', 'rejected', 'missing identity', ?1)",
        params![recent],
    )
    .unwrap();
    // Historical locality and the newer unavailable state are both retained.
    conn.execute(
        "INSERT INTO identity_admissions
            (admission_id, agent_identity_id, connection_id, state, created_at)
         VALUES
            ('rev-1-self', 'agent-revoked', 'conn-rev-1', 'self_asserted', ?1),
            ('rev-2-unavail', 'agent-revoked', 'conn-rev-2', 'unavailable', ?2)",
        params![older, old],
    )
    .unwrap();
    // Verified evidence does not replace historical local admission evidence.
    conn.execute(
        "INSERT INTO identity_admissions
            (admission_id, agent_identity_id, connection_id, state, created_at)
         VALUES
            ('ver-old', 'agent-verified', 'conn-ver', 'verified', ?1),
            ('ver-older-sa', 'agent-verified', 'conn-ver-sa', 'self_asserted', ?2)",
        params![old, older],
    )
    .unwrap();
    // 5. Old rows referenced by a2a_envelopes / a2a_delivery_receipts -> preserved!
    conn.execute(
        "INSERT INTO identity_admissions
            (admission_id, agent_identity_id, connection_id, state, created_at)
         VALUES
            ('a2a-issuer-old', 'agent-a2a', 'conn-a2a-1', 'self_asserted', ?1),
            ('a2a-recip-old', 'agent-a2a', 'conn-a2a-2', 'self_asserted', ?2),
            ('a2a-actor-old', 'agent-a2a', 'conn-a2a-3', 'self_asserted', ?2),
            ('a2a-latest', 'agent-a2a', 'conn-a2a-latest', 'self_asserted', ?3)",
        params![older, old, recent],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO a2a_envelopes
            (envelope_id, kind, issuer_agent_identity_id, issuer_admission_id,
             recipient_agent_identity_id, recipient_admission_id, subject_ref, body,
             body_digest, issuer_identity_assurance, recipient_identity_assurance,
             issuer_trust_domain, recipient_trust_domain, issuer_trust_basis,
             recipient_trust_basis, idempotency_key, created_at, expires_at,
             current_state, state_version)
         VALUES
            ('env-1', 'turn_response/v1', 'agent-a2a', 'a2a-issuer-old',
             'agent-a2a', 'a2a-recip-old', 'peer:sub', 'ok',
             'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',
             'self_asserted', 'self_asserted', 'same_host', 'same_host',
             'current_local_connection', 'historical_local_admission',
             'idem-1', ?1, '2026-12-01T00:00:00.000Z', 'received', 1)",
        params![old],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO a2a_delivery_receipts
            (receipt_id, envelope_id, state, envelope_version, actor_agent_identity_id,
             actor_admission_id, identity_assurance, trust_domain, trust_basis, occurred_at)
         VALUES ('rcpt-1', 'env-1', 'received', 1, 'agent-a2a', 'a2a-actor-old',
                 'self_asserted', 'same_host', 'current_local_connection', ?1)",
        params![old],
    )
    .unwrap();
    // 6. Old row referenced by harness_session_attachments via connection_id -> preserved!
    conn.execute(
        "INSERT INTO identity_admissions
            (admission_id, agent_identity_id, connection_id, state, created_at)
         VALUES
            ('att-old', 'agent-attached', 'conn-att-old', 'self_asserted', ?1),
            ('att-newer', 'agent-attached', 'conn-att-newer', 'unavailable', ?2)",
        params![older, old],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO harness_session_attachments
            (attachment_id, host_identity, identity_attribution_basis, protocol_version,
             adapter_connection_identity, remote_session_id, work_claim_id,
             expected_transition_version, agent_identity_id, contract_digest,
             capabilities_json, tool_profile, capability_class, policy_digest,
             descriptor_digest, idempotency_key, admission_receipt_ref, state,
             created_at, updated_at)
         VALUES
            ('att-row-1', 'host-1', 'trusted_local_host_declared', 'acp/1',
             'adapter-1', 'remote-1', 'claim-1', 0, 'agent-attached',
             'digest-1', '{}', 'read', 'read', 'pol-1', 'desc-1',
             'idem-att-1', 'conn-att-old', 'attached', ?1, ?1)",
        params![older],
    )
    .unwrap();

    // PortableKernel is a no-op.
    assert_eq!(
        gc_identity_admissions(&conn, StoreProfile::PortableKernel, now, 30).unwrap(),
        0
    );

    let recipient_before =
        crate::db::a2a::resolve_a2a_recipient_eligibility(&conn, "agent-revoked")
            .unwrap()
            .expect("historical local admission permits offline mail");
    let pruned = gc_identity_admissions(&conn, StoreProfile::TachiFull, now, 30).unwrap();
    assert_eq!(pruned, 1, "only the unbound rejection is disposable");
    assert_eq!(
        crate::db::a2a::resolve_a2a_recipient_eligibility(&conn, "agent-revoked")
            .unwrap()
            .expect("GC must preserve offline recipient eligibility"),
        recipient_before
    );
    assert!(crate::db::a2a::a2a_actor_is_current(
        &conn,
        &crate::db::a2a::A2aTransitionActor {
            agent_identity_id: "agent-revoked".into(),
            connection_id: "conn-rev-1".into(),
        },
    )
    .unwrap());

    let mut stmt = conn
        .prepare("SELECT admission_id FROM identity_admissions ORDER BY admission_id")
        .unwrap();
    let remaining: Vec<String> = stmt
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    assert_eq!(
        remaining,
        vec![
            "a2a-actor-old",
            "a2a-issuer-old",
            "a2a-latest",
            "a2a-recip-old",
            "att-newer",
            "att-old",
            "rej-recent",
            "rev-1-self",
            "rev-2-unavail",
            "ver-old",
            "ver-older-sa",
        ]
    );
}

#[test]
fn freelist_stats_tracks_freed_pages_until_vacuum() {
    let conn = make_conn();
    conn.execute_batch(
        "CREATE TABLE bloat_tmp (id INTEGER PRIMARY KEY, body TEXT);
         WITH RECURSIVE cnt(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM cnt WHERE x < 200)
         INSERT INTO bloat_tmp(id, body) SELECT x, printf('%.*c', 1500, 'x') FROM cnt;
         DELETE FROM bloat_tmp;",
    )
    .unwrap();
    let before = freelist_stats(&conn).unwrap();
    assert!(
        before.freelist_count > 0,
        "deleted rows should populate freelist"
    );
    assert!(before.freelist_bytes() > 0);
    assert!(before.freelist_percent() > 0);

    conn.execute_batch("VACUUM;").unwrap();
    let after = freelist_stats(&conn).unwrap();
    assert_eq!(after.freelist_count, 0);
    assert_eq!(after.freelist_bytes(), 0);
}
