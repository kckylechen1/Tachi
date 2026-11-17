use super::*;

fn test_conn() -> Connection {
    crate::db::enable_simple_auto_extension().unwrap();
    crate::db::register_sqlite_vec();
    let conn = Connection::open_in_memory().unwrap();
    crate::db::init_schema(&conn).unwrap();
    conn
}

fn caller(id: &str) -> DeliveryCaller {
    DeliveryCaller {
        agent_identity_id: id.to_string(),
        host_identity: format!("host-{id}"),
    }
}

fn mint_new(idempotency_key: &str) -> NewDeliveryIntent {
    NewDeliveryIntent {
        idempotency_key: idempotency_key.to_string(),
        execution_source: DeliveryExecutionSource::ManagedDispatch,
        execution_ref: "outcome-1".to_string(),
        terminal_receipt_revision: 1,
        work_claim_id: Some("claim-1".to_string()),
        result_ref: "artifact://runs/outcome-1/result.json".to_string(),
        result_revision: 1,
        payload_digest: "sha256-AAAA".to_string(),
        visibility_class: DeliveryVisibilityClass::Public,
        delivery_policy: DeliveryPolicy::AnnounceRequesterSession,
        protocol_capability: "wecom-text-v1".to_string(),
        requester: DeliveryRequesterBinding {
            agent_identity_id: Some("requester-a".to_string()),
            host_identity: Some("host-requester-a".to_string()),
            session_ref: None,
        },
        expires_at: None,
        correction: false,
    }
}

/// Execution/adjudication fixture rows that must stay byte-identical
/// across the whole delivery lifecycle.
fn seed_execution_fixture(conn: &Connection) {
    conn.execute(
        "INSERT INTO dispatch_outcomes
            (outcome_id, dispatch_id, execution_outcome, idempotency_key, created_at, updated_at)
         VALUES ('outcome-1', 'dispatch-1', 'completed', 'ik-1', '2026-08-30T00:00:00Z', '2026-08-30T00:00:00Z')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO dispatch_adjudications
            (adjudication_id, outcome_id, event_key, verdict, actor, evidence_ref, created_at, insertion_seq)
         VALUES ('adj-1', 'outcome-1', 'evt-1', 'accepted', 'reviewer', 'evidence-1', '2026-08-30T00:00:00Z', 1)",
        [],
    )
    .unwrap();
}

fn execution_fixture_unchanged(conn: &Connection) -> bool {
    let outcome: (String, String) = conn
        .query_row(
            "SELECT execution_outcome, updated_at FROM dispatch_outcomes WHERE outcome_id = 'outcome-1'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    let verdict: String = conn
        .query_row(
            "SELECT verdict FROM dispatch_adjudications WHERE outcome_id = 'outcome-1'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    outcome == ("completed".to_string(), "2026-08-30T00:00:00Z".to_string())
        && verdict == "accepted"
}

fn state_of(conn: &Connection, delivery_id: &str) -> String {
    conn.query_row(
        "SELECT delivery_state FROM delivery_intents WHERE delivery_id = ?1",
        params![delivery_id],
        |row| row.get(0),
    )
    .unwrap()
}

fn event_kinds(conn: &Connection, delivery_id: &str) -> Vec<String> {
    let mut stmt = conn
        .prepare("SELECT kind FROM delivery_events WHERE delivery_id = ?1 ORDER BY event_row_id")
        .unwrap();
    stmt.query_map(params![delivery_id], |row| row.get(0))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
}

fn set_claim_expired(conn: &Connection, delivery_id: &str) {
    conn.execute(
        "UPDATE delivery_intents SET claim_expires_at = '2026-08-29T00:00:00Z' WHERE delivery_id = ?1",
        params![delivery_id],
    )
    .unwrap();
}

fn set_retry_due(conn: &Connection, delivery_id: &str) {
    conn.execute(
        "UPDATE delivery_intents SET next_retry_at = '2026-08-29T00:00:00Z' WHERE delivery_id = ?1",
        params![delivery_id],
    )
    .unwrap();
}

fn make_ready(conn: &Connection, key: &str) -> DeliveryIntent {
    mint_delivery_intent(conn, &mint_new(key)).unwrap()
}

// --- Discrimination 1 (spine half): execution-completed + not-yet-
// delivered reads as exactly that, three states visible independently.
#[test]
fn execution_completed_not_yet_delivered_reads_independently() {
    let conn = test_conn();
    seed_execution_fixture(&conn);
    let intent = make_ready(&conn, "managed:outcome-1");
    // Execution plane: its own receipt says completed.
    let execution: String = conn
        .query_row(
            "SELECT execution_outcome FROM dispatch_outcomes WHERE outcome_id = 'outcome-1'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    // Adjudication plane: its own row already carries a verdict.
    let adjudication: String = conn
        .query_row(
            "SELECT verdict FROM dispatch_adjudications WHERE outcome_id = 'outcome-1'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    // Delivery plane: ready, never merged into either other plane.
    assert_eq!(intent.delivery_state, "ready");
    assert_eq!(execution, "completed");
    assert_eq!(adjudication, "accepted");
    assert!(execution_fixture_unchanged(&conn));
}

// --- Discrimination 1/2 (restart): requester restart re-claims the SAME
// intent; no duplicate intent, no duplicate delivery.
#[test]
fn requester_restart_reclaims_the_same_intent() {
    let conn = test_conn();
    let intent = make_ready(&conn, "managed:outcome-2");
    let first = claim_ready_delivery(
        &conn,
        &DeliveryClaimRequest {
            caller: caller("requester-a"),
            claim_key: "ck-1".to_string(),
            lease_seconds: 0,
            only_delivery_id: None,
        },
    )
    .unwrap();
    let DeliveryClaimOutcome::Claimed(claimed) = first else {
        panic!("expected claim");
    };
    assert_eq!(claimed.delivery_id, intent.delivery_id);

    // Requester crashes without acking; its claim lease expires.
    set_claim_expired(&conn, &intent.delivery_id);

    // Restart: a fresh claim key re-claims the SAME intent.
    let second = claim_ready_delivery(
        &conn,
        &DeliveryClaimRequest {
            caller: caller("requester-a"),
            claim_key: "ck-restart".to_string(),
            lease_seconds: 0,
            only_delivery_id: None,
        },
    )
    .unwrap();
    let DeliveryClaimOutcome::Claimed(reclaimed) = second else {
        panic!("expected re-claim");
    };
    assert_eq!(reclaimed.delivery_id, intent.delivery_id);
    assert_eq!(reclaimed.attempt_count, 2);
    assert_eq!(
        conn.query_row("SELECT COUNT(*) FROM delivery_intents", [], |row| row
            .get::<_, i64>(0))
            .unwrap(),
        1,
        "restart must never mint a second intent"
    );
    let kinds = event_kinds(&conn, &intent.delivery_id);
    assert!(kinds.contains(&"claim_expired".to_string()));
    assert_eq!(
        kinds.iter().filter(|k| k.as_str() == "claimed").count(),
        2,
        "one claim event per attempt, same intent"
    );
}

// --- Discrimination 3: delivery transport failure leaves execution
// terminal success intact and retries the SAME delivery id.
#[test]
fn delivery_failure_never_regresses_execution_truth() {
    let conn = test_conn();
    seed_execution_fixture(&conn);
    let intent = make_ready(&conn, "managed:outcome-3");
    let claim = claim_ready_delivery(
        &conn,
        &DeliveryClaimRequest {
            caller: caller("requester-a"),
            claim_key: "ck-t1".to_string(),
            lease_seconds: 0,
            only_delivery_id: None,
        },
    )
    .unwrap();
    assert!(matches!(claim, DeliveryClaimOutcome::Claimed(_)));

    let blocked = reject_or_block(
        &conn,
        &intent.delivery_id,
        &caller("requester-a"),
        blocker_class::TRANSPORT_FAILURE,
        Some("connection refused before send"),
        Some(30),
        None,
    )
    .unwrap();
    assert_eq!(blocked.delivery_state, "retrying");
    assert!(execution_fixture_unchanged(&conn));

    // Retry becomes due; the SAME intent is re-claimed.
    set_retry_due(&conn, &intent.delivery_id);
    let retry = claim_ready_delivery(
        &conn,
        &DeliveryClaimRequest {
            caller: caller("requester-a"),
            claim_key: "ck-t2".to_string(),
            lease_seconds: 0,
            only_delivery_id: None,
        },
    )
    .unwrap();
    let DeliveryClaimOutcome::Claimed(retried) = retry else {
        panic!("expected retry claim");
    };
    assert_eq!(retried.delivery_id, intent.delivery_id);
    assert!(execution_fixture_unchanged(&conn));
}

// --- Discrimination 4: lost ack / replayed delivery is idempotent and
// does not duplicate the visible result.
#[test]
fn replayed_ack_and_delivery_are_idempotent() {
    let conn = test_conn();
    let intent = make_ready(&conn, "managed:outcome-4");
    claim_ready_delivery(
        &conn,
        &DeliveryClaimRequest {
            caller: caller("requester-a"),
            claim_key: "ck-a1".to_string(),
            lease_seconds: 0,
            only_delivery_id: None,
        },
    )
    .unwrap();
    let ack = ack_delivered(
        &conn,
        &intent.delivery_id,
        &caller("requester-a"),
        "ak-1",
        None,
    )
    .unwrap();
    // revision: mint 1 -> claim 2 -> ack 3.
    assert_eq!(ack, DeliveryAckOutcome::Acknowledged { revision: 3 });

    // Replayed ack (new key, same delivered intent) is suppressed.
    let replay = ack_delivered(
        &conn,
        &intent.delivery_id,
        &caller("requester-a"),
        "ak-2",
        None,
    )
    .unwrap();
    assert_eq!(replay, DeliveryAckOutcome::AlreadyDelivered);

    // A delivered intent is never claimable again: no duplicate delivery.
    let after = claim_ready_delivery(
        &conn,
        &DeliveryClaimRequest {
            caller: caller("requester-a"),
            claim_key: "ck-a3".to_string(),
            lease_seconds: 0,
            only_delivery_id: None,
        },
    )
    .unwrap();
    assert_eq!(after, DeliveryClaimOutcome::NoneReady);

    let delivered_events: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM delivery_events WHERE delivery_id = ?1 AND kind = 'delivered'",
            params![intent.delivery_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(delivered_events, 1, "deterministic ack suppression");
    assert_eq!(state_of(&conn, &intent.delivery_id), "delivered");
}

// --- Discrimination 5: ambiguous post-send failure stays blocked/warning
// rather than automatically duplicating a send.
#[test]
fn ambiguous_send_parks_blocked_never_auto_redispatches() {
    let conn = test_conn();
    let intent = make_ready(&conn, "managed:outcome-5");
    claim_ready_delivery(
        &conn,
        &DeliveryClaimRequest {
            caller: caller("requester-a"),
            claim_key: "ck-b1".to_string(),
            lease_seconds: 0,
            only_delivery_id: None,
        },
    )
    .unwrap();
    let parked = reject_or_block(
        &conn,
        &intent.delivery_id,
        &caller("requester-a"),
        blocker_class::AMBIGUOUS_SEND_OUTCOME,
        Some("send outcome unknown after gateway timeout"),
        None,
        None,
    )
    .unwrap();
    assert_eq!(parked.delivery_state, "blocked");
    assert_eq!(
        parked.blocker_class.as_deref(),
        Some("ambiguous_send_outcome")
    );

    // Claims never pick up blocked intents.
    let attempt = claim_ready_delivery(
        &conn,
        &DeliveryClaimRequest {
            caller: caller("requester-a"),
            claim_key: "ck-b2".to_string(),
            lease_seconds: 0,
            only_delivery_id: None,
        },
    )
    .unwrap();
    assert_eq!(attempt, DeliveryClaimOutcome::NoneReady);

    // Resume reports the blocked intent but never re-arms it silently.
    let resumed = resume_requester_operation(&conn, &caller("requester-a"), false).unwrap();
    assert_eq!(resumed.len(), 1);
    assert_eq!(resumed[0].delivery_state, "blocked");

    // The AFFIRMATIVE re-arm (resume with re-arm authority) is the only
    // requester path out of blocked besides a corrected revision.
    let rearmed = resume_requester_operation(&conn, &caller("requester-a"), true).unwrap();
    assert_eq!(rearmed.len(), 1);
    assert_eq!(rearmed[0].delivery_state, "ready");
    let kinds = event_kinds(&conn, &intent.delivery_id);
    assert!(kinds.contains(&"transition_debt".to_string()));

    // Only a corrected result revision ALSO re-arms the SAME intent.
    let mut corrected = mint_new("managed:outcome-5");
    corrected.result_revision = 2;
    corrected.payload_digest = "sha256-BBBB".to_string();
    let rearmed = mint_delivery_intent(&conn, &corrected).unwrap();
    assert_eq!(rearmed.delivery_id, intent.delivery_id);
    assert_eq!(rearmed.delivery_state, "ready");
    let kinds = event_kinds(&conn, &intent.delivery_id);
    assert!(kinds.contains(&"result_superseded".to_string()));
    assert!(kinds.contains(&"transition_debt".to_string()));
}

// --- Discrimination 6: revoked/wrong requester receives no existence
// signal where strict policy applies.
#[test]
fn private_intent_is_invisible_to_a_mismatched_requester() {
    let conn = test_conn();
    let mut new = mint_new("managed:outcome-6");
    new.visibility_class = DeliveryVisibilityClass::Private;
    new.requester = DeliveryRequesterBinding {
        agent_identity_id: Some("requester-owner".to_string()),
        host_identity: Some("host-requester-owner".to_string()),
        session_ref: None,
    };
    let intent = mint_delivery_intent(&conn, &new).unwrap();

    // Wrong requester: no existence signal — NoneReady, not an error.
    let wrong = claim_ready_delivery(
        &conn,
        &DeliveryClaimRequest {
            caller: caller("intruder"),
            claim_key: "ck-i1".to_string(),
            lease_seconds: 0,
            only_delivery_id: None,
        },
    )
    .unwrap();
    assert_eq!(wrong, DeliveryClaimOutcome::NoneReady);
    let resumed = resume_requester_operation(&conn, &caller("intruder"), false).unwrap();
    assert!(resumed.is_empty());

    // The bound requester claims it.
    let right = claim_ready_delivery(
        &conn,
        &DeliveryClaimRequest {
            caller: caller("requester-owner"),
            claim_key: "ck-i2".to_string(),
            lease_seconds: 0,
            only_delivery_id: None,
        },
    )
    .unwrap();
    let DeliveryClaimOutcome::Claimed(claimed) = right else {
        panic!("expected the bound requester to claim");
    };
    assert_eq!(claimed.delivery_id, intent.delivery_id);
}

// --- Discrimination 6 (mint law): a private intent with no admitted
// requester binding is refused at mint (fail-closed).
#[test]
fn private_intent_without_admitted_binding_is_refused() {
    let conn = test_conn();
    let mut new = mint_new("managed:outcome-7");
    new.visibility_class = DeliveryVisibilityClass::Private;
    new.requester = DeliveryRequesterBinding::default();
    let error = mint_delivery_intent(&conn, &new).unwrap_err();
    assert!(error.to_string().contains("private delivery intents"));
}

// --- Discrimination 7 (spine half): the worker cannot mint a
// user/channel choice — no destination field exists, and the requester
// binding never appears from a claim either.
#[test]
fn worker_cannot_mint_a_destination() {
    let conn = test_conn();
    // An unbound (pull-only) intent stays unbound through claim/ack: the
    // seam caller's identity is a claim receipt, never a destination.
    let mut new = mint_new("managed:outcome-8");
    new.requester = DeliveryRequesterBinding::default();
    let intent = mint_delivery_intent(&conn, &new).unwrap();
    assert_eq!(intent.requester_agent_identity_id, None);
    claim_ready_delivery(
        &conn,
        &DeliveryClaimRequest {
            caller: caller("whoever"),
            claim_key: "ck-w1".to_string(),
            lease_seconds: 0,
            only_delivery_id: None,
        },
    )
    .unwrap();
    let after = get_delivery_intent(&conn, &intent.delivery_id)
        .unwrap()
        .unwrap();
    assert_eq!(after.requester_agent_identity_id, None);
    assert_eq!(after.requester_host_identity, None);
    assert_eq!(after.requester_session_ref, None);
    // The claim holder is recorded as a lease, not a destination.
    assert_eq!(after.claimed_by.as_deref(), Some("host-whoever"));
    // Structural: the input types name no destination field (compiled).
    let _no_destination_field: fn(&NewDeliveryIntent) -> &DeliveryPolicy =
        |new| &new.delivery_policy;
}

// --- Discrimination 8: dismiss changes only delivery state; delivered
// cannot be dismissed.
#[test]
fn dismiss_changes_only_delivery_state() {
    let conn = test_conn();
    seed_execution_fixture(&conn);
    let intent = make_ready(&conn, "managed:outcome-9");
    let dismissed =
        dismiss_delivery(&conn, &intent.delivery_id, &caller("requester-a"), None).unwrap();
    assert_eq!(dismissed.delivery_state, "dismissed");
    assert!(execution_fixture_unchanged(&conn));
    // Idempotent dismiss of an already-dismissed intent.
    let again = dismiss_delivery(&conn, &intent.delivery_id, &caller("requester-a"), None).unwrap();
    assert_eq!(again.delivery_state, "dismissed");

    // Delivered is terminal for dismissal.
    let intent2 = make_ready(&conn, "managed:outcome-10");
    claim_ready_delivery(
        &conn,
        &DeliveryClaimRequest {
            caller: caller("requester-a"),
            claim_key: "ck-d1".to_string(),
            lease_seconds: 0,
            only_delivery_id: None,
        },
    )
    .unwrap();
    ack_delivered(
        &conn,
        &intent2.delivery_id,
        &caller("requester-a"),
        "ak-d1",
        None,
    )
    .unwrap();
    let error =
        dismiss_delivery(&conn, &intent2.delivery_id, &caller("requester-a"), None).unwrap_err();
    assert!(error.to_string().contains("cannot be dismissed"));
}

// --- Discrimination 9: a corrected result revision prevents a stale
// delivery from winning.
#[test]
fn corrected_revision_prevents_stale_delivery_from_winning() {
    let conn = test_conn();
    let intent = make_ready(&conn, "managed:outcome-11");
    claim_ready_delivery(
        &conn,
        &DeliveryClaimRequest {
            caller: caller("requester-a"),
            claim_key: "ck-r1".to_string(),
            lease_seconds: 0,
            only_delivery_id: None,
        },
    )
    .unwrap();
    ack_delivered(
        &conn,
        &intent.delivery_id,
        &caller("requester-a"),
        "ak-r1",
        None,
    )
    .unwrap();
    assert_eq!(state_of(&conn, &intent.delivery_id), "delivered");

    // A corrected revision re-arms delivery of the SAME intent.
    let mut corrected = mint_new("managed:outcome-11");
    corrected.result_revision = 2;
    corrected.payload_digest = "sha256-CCCC".to_string();
    let rearmed = mint_delivery_intent(&conn, &corrected).unwrap();
    assert_eq!(rearmed.delivery_id, intent.delivery_id);
    assert_eq!(rearmed.result_revision, 2);
    assert_eq!(rearmed.delivery_state, "ready");

    // A stale mint (older revision, older ref) cannot regress it.
    let mut stale = mint_new("managed:outcome-11");
    stale.result_ref = "artifact://stale".to_string();
    let reconciled = mint_delivery_intent(&conn, &stale).unwrap();
    assert_eq!(reconciled.result_ref, corrected.result_ref);
    assert_eq!(reconciled.result_revision, 2);

    // Same revision, different content: typed conflict, never silent.
    let mut forged = mint_new("managed:outcome-11");
    forged.result_revision = 2;
    forged.payload_digest = "sha256-DDDD".to_string();
    let error = mint_delivery_intent(&conn, &forged).unwrap_err();
    assert!(matches!(error, MemoryError::DeliveryIdempotencyConflict(_)));

    // The re-armed intent is claimable again (delivered state was for
    // the older revision only).
    let reclaim = claim_ready_delivery(
        &conn,
        &DeliveryClaimRequest {
            caller: caller("requester-a"),
            claim_key: "ck-r2".to_string(),
            lease_seconds: 0,
            only_delivery_id: None,
        },
    )
    .unwrap();
    let DeliveryClaimOutcome::Claimed(claimed) = reclaim else {
        panic!("expected re-armed claim");
    };
    assert_eq!(claimed.delivery_id, intent.delivery_id);
    assert_eq!(claimed.result_revision, 2);
}

// --- Discrimination 4 (claim twin): a claim key is bound to the intent
// it created. Replaying it after that intent's claim advanced (here:
// acknowledged) is a typed conflict — never a fresh claim of ANY intent.
#[test]
fn reused_claim_key_after_advance_conflicts() {
    let conn = test_conn();
    let first = make_ready(&conn, "managed:outcome-12");
    claim_ready_delivery(
        &conn,
        &DeliveryClaimRequest {
            caller: caller("requester-a"),
            claim_key: "ck-shared".to_string(),
            lease_seconds: 0,
            only_delivery_id: None,
        },
    )
    .unwrap();
    ack_delivered(
        &conn,
        &first.delivery_id,
        &caller("requester-a"),
        "ak-shared",
        None,
    )
    .unwrap();
    let _second = make_ready(&conn, "managed:outcome-13");
    let error = claim_ready_delivery(
        &conn,
        &DeliveryClaimRequest {
            caller: caller("requester-a"),
            claim_key: "ck-shared".to_string(),
            lease_seconds: 0,
            only_delivery_id: None,
        },
    )
    .unwrap_err();
    assert!(matches!(error, MemoryError::DeliveryIdempotencyConflict(_)));
    // The delivered intent is untouched and the second stays ready.
    assert_eq!(state_of(&conn, &first.delivery_id), "delivered");
    let second_state: String = conn
        .query_row(
            "SELECT delivery_state FROM delivery_intents WHERE idempotency_key = 'managed:outcome-13'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(second_state, "ready");
}

// --- Discrimination 11 (spine half): full event replay is canonically
// equivalent to the materialized state.
#[test]
fn event_replay_is_canonically_equivalent_to_incremental_state() {
    let conn = test_conn();
    let intent = make_ready(&conn, "managed:outcome-14");
    claim_ready_delivery(
        &conn,
        &DeliveryClaimRequest {
            caller: caller("requester-a"),
            claim_key: "ck-e1".to_string(),
            lease_seconds: 0,
            only_delivery_id: None,
        },
    )
    .unwrap();
    ack_delivered(
        &conn,
        &intent.delivery_id,
        &caller("requester-a"),
        "ak-e1",
        None,
    )
    .unwrap();

    // Folding the append-only events yields the same terminal delivery
    // truth the materialized row holds.
    let last_kind: String = conn
        .query_row(
            "SELECT kind FROM delivery_events WHERE delivery_id = ?1
             ORDER BY event_row_id DESC LIMIT 1",
            params![intent.delivery_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(last_kind, "delivered");
    assert_eq!(state_of(&conn, &intent.delivery_id), "delivered");

    // Replay of the whole mint is a no-op (idempotent rebuild).
    let replayed = mint_delivery_intent(&conn, &mint_new("managed:outcome-14")).unwrap();
    assert_eq!(replayed.delivery_id, intent.delivery_id);
    assert_eq!(replayed.delivery_state, "delivered");
    assert_eq!(replayed.revision, intent.revision + 2);
}

// --- Unbound public intents are claimable by any admitted requester.
#[test]
fn unbound_public_intent_is_claimable_by_any_admitted_caller() {
    let conn = test_conn();
    let mut new = mint_new("managed:outcome-15");
    new.requester = DeliveryRequesterBinding::default();
    let intent = mint_delivery_intent(&conn, &new).unwrap();
    let claim = claim_ready_delivery(
        &conn,
        &DeliveryClaimRequest {
            caller: caller("another"),
            claim_key: "ck-u1".to_string(),
            lease_seconds: 0,
            only_delivery_id: None,
        },
    )
    .unwrap();
    let DeliveryClaimOutcome::Claimed(claimed) = claim else {
        panic!("expected claim of a public intent");
    };
    assert_eq!(claimed.delivery_id, intent.delivery_id);
}

// --- codex R2 regression: the same claim key replayed for the SAME
// intent through the general claim path returns the original receipt
// (replay-safe), not a conflict.
#[test]
fn same_claim_key_replayed_on_the_same_intent_replays() {
    let conn = test_conn();
    let intent = make_ready(&conn, "managed:outcome-17");
    let first = claim_ready_delivery(
        &conn,
        &DeliveryClaimRequest {
            caller: caller("requester-a"),
            claim_key: "ck-same".to_string(),
            lease_seconds: 0,
            only_delivery_id: None,
        },
    )
    .unwrap();
    let DeliveryClaimOutcome::Claimed(claimed) = first else {
        panic!("expected claim");
    };
    assert_eq!(claimed.delivery_id, intent.delivery_id);

    // The response was lost; the requester retries the SAME key.
    let retry = claim_ready_delivery(
        &conn,
        &DeliveryClaimRequest {
            caller: caller("requester-a"),
            claim_key: "ck-same".to_string(),
            lease_seconds: 0,
            only_delivery_id: None,
        },
    )
    .unwrap();
    let DeliveryClaimOutcome::ReplayedClaim(replayed) = retry else {
        panic!("expected replay receipt, got {retry:?}");
    };
    assert_eq!(replayed.delivery_id, intent.delivery_id);
    assert_eq!(replayed.attempt_count, 1, "no duplicate attempt");
    let claim_events: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM delivery_events WHERE delivery_id = ?1 AND kind = 'claimed'",
            params![intent.delivery_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(claim_events, 1, "deterministic claim suppression");
}

// --- codex R2 regression: only the claim holder may reject_or_block,
// and only from requester_queued.
#[test]
fn reject_or_block_is_holder_only() {
    let conn = test_conn();
    let intent = make_ready(&conn, "managed:outcome-18");
    // A non-holder while the intent is ready (unclaimed): the intent is
    // bound to requester-a, so the intruder gets the generic not-found
    // refusal (no existence signal, no state change).
    let error = reject_or_block(
        &conn,
        &intent.delivery_id,
        &caller("intruder"),
        blocker_class::TRANSPORT_FAILURE,
        None,
        None,
        None,
    )
    .unwrap_err();
    assert!(matches!(error, MemoryError::NotFound(_)));
    assert_eq!(state_of(&conn, &intent.delivery_id), "ready");

    // The holder claims; a different admitted caller is refused.
    claim_ready_delivery(
        &conn,
        &DeliveryClaimRequest {
            caller: caller("requester-a"),
            claim_key: "ck-h1".to_string(),
            lease_seconds: 0,
            only_delivery_id: None,
        },
    )
    .unwrap();
    let error = reject_or_block(
        &conn,
        &intent.delivery_id,
        &caller("intruder"),
        blocker_class::TRANSPORT_FAILURE,
        None,
        None,
        None,
    )
    .unwrap_err();
    assert!(matches!(error, MemoryError::NotFound(_)));
    assert_eq!(state_of(&conn, &intent.delivery_id), "requester_queued");
}

// --- codex R2 regression: an intent bound to requester A cannot be
// dismissed by requester B.
#[test]
fn dismiss_is_requester_bound() {
    let conn = test_conn();
    let intent = make_ready(&conn, "managed:outcome-19");
    let error =
        dismiss_delivery(&conn, &intent.delivery_id, &caller("intruder"), None).unwrap_err();
    assert!(matches!(error, MemoryError::DeliveryIncompatibleState(_)));
    assert_eq!(state_of(&conn, &intent.delivery_id), "ready");
}

// --- codex R2 regression: a refused report (over-bound detail) leaves
// the intent and ledger untouched — no state write without its event.
#[test]
fn over_bound_detail_leaves_zero_writes() {
    let conn = test_conn();
    let intent = make_ready(&conn, "managed:outcome-20");
    claim_ready_delivery(
        &conn,
        &DeliveryClaimRequest {
            caller: caller("requester-a"),
            claim_key: "ck-x1".to_string(),
            lease_seconds: 0,
            only_delivery_id: None,
        },
    )
    .unwrap();
    let long_detail = "x".repeat(1001);
    let error = reject_or_block(
        &conn,
        &intent.delivery_id,
        &caller("requester-a"),
        blocker_class::TRANSPORT_FAILURE,
        Some(long_detail.as_str()),
        None,
        None,
    )
    .unwrap_err();
    assert!(matches!(error, MemoryError::InvalidArg(_)));
    assert_eq!(state_of(&conn, &intent.delivery_id), "requester_queued");
    let events: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM delivery_events WHERE delivery_id = ?1",
            params![intent.delivery_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(events, 3, "mint receipts + claim only; no block event");
}

// --- codex R2 round-2 regression: a terminal-plane correction at an
// EQUAL wall-clock revision with different content supersedes strictly
// (prev+1) inside the mint — never a conflict, never a lost update.
// Without the correction authority the same call stays a typed conflict.
#[test]
fn same_tick_correction_supersedes_and_conflict_law_holds_without_it() {
    let conn = test_conn();
    let intent = make_ready(&conn, "managed:outcome-21");

    // Without correction authority: equal revision + different content
    // is a typed conflict (the TB-7 law for everyone else).
    let mut forged = mint_new("managed:outcome-21");
    forged.result_revision = 1;
    forged.payload_digest = "sha256-BBBB".to_string();
    let error = mint_delivery_intent(&conn, &forged).unwrap_err();
    assert!(matches!(error, MemoryError::DeliveryIdempotencyConflict(_)));

    // With correction authority: the SAME tick supersedes to prev+1.
    let mut corrected = mint_new("managed:outcome-21");
    corrected.result_revision = 1;
    corrected.payload_digest = "sha256-CCCC".to_string();
    corrected.correction = true;
    let superseded = mint_delivery_intent(&conn, &corrected).unwrap();
    assert_eq!(superseded.delivery_id, intent.delivery_id);
    assert_eq!(superseded.result_revision, 2, "strictly grows");
    assert_eq!(superseded.delivery_state, "ready");

    // An OLDER-revision mint WITHOUT correction authority is stale —
    // no-op even with different content.
    let mut stale = mint_new("managed:outcome-21");
    stale.result_revision = 1;
    stale.payload_digest = "sha256-DDDD".to_string();
    stale.result_ref = "artifact://older".to_string();
    let untouched = mint_delivery_intent(&conn, &stale).unwrap();
    assert_eq!(untouched.result_revision, 2, "stale no-op");
    assert_eq!(untouched.result_ref, corrected.result_ref);

    // With correction authority, an older-revision mint IS the
    // terminal plane asserting a canonical-latest correction: the
    // supersede grows strictly (prev+1) and mirrors the plane's truth.
    let mut authoritative = mint_new("managed:outcome-21");
    authoritative.result_revision = 1;
    authoritative.payload_digest = "sha256-DDDD".to_string();
    authoritative.result_ref = "artifact://older".to_string();
    authoritative.correction = true;
    let superseded2 = mint_delivery_intent(&conn, &authoritative).unwrap();
    assert_eq!(superseded2.result_revision, 3, "strictly grows");
    assert_eq!(superseded2.payload_digest, "sha256-DDDD");
}

// --- codex R2 round-3 regression: unchanged content at a HIGHER
// revision (a delayed replay of the same terminal receipt) carries no
// new information and must not re-arm a settled delivery.
#[test]
fn unchanged_higher_revision_replay_never_rearms() {
    let conn = test_conn();
    let mut first = mint_new("managed:outcome-23");
    first.result_revision = 10;
    first.payload_digest = "sha256-AAAA".to_string();
    let intent = mint_delivery_intent(&conn, &first).unwrap();

    // Deliver: the intent settles.
    claim_ready_delivery(
        &conn,
        &DeliveryClaimRequest {
            caller: caller("requester-a"),
            claim_key: "ck-hi1".to_string(),
            lease_seconds: 0,
            only_delivery_id: None,
        },
    )
    .unwrap();
    ack_delivered(
        &conn,
        &intent.delivery_id,
        &caller("requester-a"),
        "ak-hi1",
        None,
    )
    .unwrap();
    assert_eq!(state_of(&conn, &intent.delivery_id), "delivered");

    // Delayed replay of the SAME receipt with a higher terminal
    // revision but unchanged content: no re-arm, delivered stands.
    let mut replay = mint_new("managed:outcome-23");
    replay.result_revision = 20;
    replay.payload_digest = "sha256-AAAA".to_string();
    let reconciled = mint_delivery_intent(&conn, &replay).unwrap();
    assert_eq!(reconciled.delivery_state, "delivered");
    assert_eq!(reconciled.result_revision, 10);

    // A genuinely CHANGED payload at a higher revision does re-arm.
    let mut corrected = mint_new("managed:outcome-23");
    corrected.result_revision = 30;
    corrected.payload_digest = "sha256-BBBB".to_string();
    let rearmed = mint_delivery_intent(&conn, &corrected).unwrap();
    assert_eq!(rearmed.delivery_state, "ready");
}

// --- codex R2 round-2 regression: replaying the same claim key after
// the claim lease expired is NOT stranded: the receipt comes back and
// the ack on it succeeds.
#[test]
fn expired_claim_replay_returns_receipt_and_ack_still_lands() {
    let conn = test_conn();
    let intent = make_ready(&conn, "managed:outcome-22");
    claim_ready_delivery(
        &conn,
        &DeliveryClaimRequest {
            caller: caller("requester-a"),
            claim_key: "ck-expired".to_string(),
            lease_seconds: 0,
            only_delivery_id: None,
        },
    )
    .unwrap();
    set_claim_expired(&conn, &intent.delivery_id);

    let replay = claim_ready_delivery(
        &conn,
        &DeliveryClaimRequest {
            caller: caller("requester-a"),
            claim_key: "ck-expired".to_string(),
            lease_seconds: 0,
            only_delivery_id: None,
        },
    )
    .unwrap();
    let DeliveryClaimOutcome::ReplayedClaim(receipt) = replay else {
        panic!("expected the original receipt");
    };
    assert_eq!(receipt.delivery_id, intent.delivery_id);

    // The holder can still ack the lapsed claim: delivery lands.
    let ack = ack_delivered(
        &conn,
        &intent.delivery_id,
        &caller("requester-a"),
        "ak-expired",
        None,
    )
    .unwrap();
    assert!(matches!(ack, DeliveryAckOutcome::Acknowledged { .. }));
    assert_eq!(state_of(&conn, &intent.delivery_id), "delivered");
}

// --- CAS: an ack carrying a stale expected revision is a typed conflict.
#[test]
fn stale_expected_revision_is_a_typed_conflict() {
    let conn = test_conn();
    let intent = make_ready(&conn, "managed:outcome-16");
    claim_ready_delivery(
        &conn,
        &DeliveryClaimRequest {
            caller: caller("requester-a"),
            claim_key: "ck-c1".to_string(),
            lease_seconds: 0,
            only_delivery_id: None,
        },
    )
    .unwrap();
    let error = ack_delivered(
        &conn,
        &intent.delivery_id,
        &caller("requester-a"),
        "ak-c1",
        Some(999),
    )
    .unwrap_err();
    assert!(matches!(error, MemoryError::DeliveryRevisionConflict(_)));
    assert_eq!(state_of(&conn, &intent.delivery_id), "requester_queued");
}

#[test]
fn unrepresentable_claim_lease_leaves_intent_and_events_unchanged() {
    let conn = test_conn();
    let before = make_ready(&conn, "managed:unrepresentable-lease");
    let events_before = event_kinds(&conn, &before.delivery_id);
    let mut request = DeliveryClaimRequest {
        caller: caller("requester-a"),
        claim_key: "ck-unrepresentable-lease".to_string(),
        lease_seconds: i64::MAX,
        only_delivery_id: Some(before.delivery_id.clone()),
    };

    let error = claim_ready_delivery(&conn, &request).unwrap_err();
    assert!(matches!(error, MemoryError::InvalidArg(_)));
    assert_eq!(
        get_delivery_intent(&conn, &before.delivery_id).unwrap(),
        Some(before.clone())
    );
    assert_eq!(event_kinds(&conn, &before.delivery_id), events_before);

    request.lease_seconds = 0;
    assert!(matches!(
        claim_ready_delivery(&conn, &request).unwrap(),
        DeliveryClaimOutcome::Claimed(_)
    ));
}
