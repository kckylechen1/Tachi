use super::*;

#[path = "project_binding_tests.rs"]
mod project_binding;

fn open_conn() -> Connection {
    crate::db::enable_simple_auto_extension().unwrap();
    crate::db::register_sqlite_vec();
    let conn = Connection::open_in_memory().unwrap();
    crate::db::init_schema(&conn).unwrap();
    conn
}

/// A file-backed (not `:memory:`) connection, required for a real
/// multi-connection concurrency test — two `:memory:` connections are
/// two independent databases, so they cannot race each other at all.
fn open_file_conn(path: &std::path::Path) -> Connection {
    crate::db::enable_simple_auto_extension().unwrap();
    crate::db::register_sqlite_vec();
    let conn = Connection::open(path).unwrap();
    crate::db::init_schema(&conn).unwrap();
    conn
}

const AUTO_EXTENSION_RACE_CHILD: &str = "TACHI_SESSION_CLAIMS_AUTO_EXTENSION_RACE_CHILD";

/// Runs the concurrent file-open path in a fresh test process.  The
/// extension registry is process-global, so an in-process test could be
/// accidentally pre-initialized by an earlier test and miss the race.
#[test]
fn concurrent_file_opens_do_not_race_simple_auto_extension_registration() {
    if std::env::var_os(AUTO_EXTENSION_RACE_CHILD).is_some() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let workers = 32;
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(workers));
        let mut handles = Vec::with_capacity(workers);

        for worker in 0..workers {
            let path = temp_dir.path().join(format!("worker-{worker}.sqlite"));
            let barrier = std::sync::Arc::clone(&barrier);
            handles.push(std::thread::spawn(move || {
                barrier.wait();
                let _conn = open_file_conn(&path);
            }));
        }

        for handle in handles {
            handle.join().expect("file-open worker must not panic");
        }
        return;
    }

    let output = std::process::Command::new(std::env::current_exe().expect("test executable"))
        .args([
            "--exact",
            "db::session_claims::tests::concurrent_file_opens_do_not_race_simple_auto_extension_registration",
        ])
        .env(AUTO_EXTENSION_RACE_CHILD, "1")
        .output()
        .expect("run isolated concurrent file-open test");

    assert!(
        output.status.success(),
        "concurrent file opens must not race extension registration:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

fn new_claim(claim_id: &str, issue_ref: &str) -> NewSessionClaim {
    NewSessionClaim {
        claim_id: claim_id.to_string(),
        session_client: Some("claude-code".to_string()),
        issue_ref: Some(issue_ref.to_string()),
        flow_id: Some("flow-1".to_string()),
        dispatch_id: Some("dispatch-1".to_string()),
        branch: "feat/x".to_string(),
        declared_file_scope: Some(r#"["crates/foo/src/lib.rs"]"#.to_string()),
        created_at: String::new(),
    }
}

#[test]
fn insert_and_get_roundtrip_defaults_to_active() {
    let conn = open_conn();
    insert_claim(&conn, &new_claim("claim-1", "org/repo#1001")).unwrap();
    let got = get_claim(&conn, "claim-1").unwrap().expect("claim present");
    assert_eq!(got.claim_id, "claim-1");
    assert_eq!(got.state, ClaimState::Active);
    assert_eq!(got.issue_ref.as_deref(), Some("org/repo#1001"));
    assert_eq!(got.session_client.as_deref(), Some("claude-code"));
    assert!(got.released_at.is_none());
    assert!(!got.created_at.is_empty());
    assert_eq!(
        got.heartbeat_at, got.created_at,
        "heartbeat starts == created_at"
    );
}

#[test]
fn duplicate_claim_id_is_rejected_not_silently_overwritten() {
    let conn = open_conn();
    insert_claim(&conn, &new_claim("claim-dup", "org/repo#1")).unwrap();
    let err = insert_claim(&conn, &new_claim("claim-dup", "org/repo#2"));
    assert!(err.is_err(), "duplicate claim_id must be rejected");
    let got = get_claim(&conn, "claim-dup").unwrap().unwrap();
    assert_eq!(got.issue_ref.as_deref(), Some("org/repo#1"));
}

#[test]
fn release_flips_active_to_released_and_stamps() {
    let mut conn = open_conn();
    insert_claim(&conn, &new_claim("claim-2", "org/repo#2")).unwrap();
    let outcome = release_claim(
        &mut conn,
        &ClaimSelector::ClaimId("claim-2".to_string()),
        Some("manual release"),
    )
    .unwrap();
    assert_eq!(
        outcome,
        ReleaseOutcome::Released {
            claim_id: "claim-2".to_string()
        }
    );
    let got = get_claim(&conn, "claim-2").unwrap().unwrap();
    assert_eq!(got.state, ClaimState::Released);
    assert_eq!(got.release_reason.as_deref(), Some("manual release"));
    assert!(got.released_at.is_some());
}

#[test]
fn release_is_idempotent_no_second_write() {
    let mut conn = open_conn();
    insert_claim(&conn, &new_claim("claim-3", "org/repo#3")).unwrap();
    let first = release_claim(
        &mut conn,
        &ClaimSelector::ClaimId("claim-3".to_string()),
        None,
    )
    .unwrap();
    assert_eq!(
        first,
        ReleaseOutcome::Released {
            claim_id: "claim-3".to_string()
        }
    );
    let stamp_after_first = get_claim(&conn, "claim-3").unwrap().unwrap().released_at;

    let second = release_claim(
        &mut conn,
        &ClaimSelector::ClaimId("claim-3".to_string()),
        Some("second-reason"),
    )
    .unwrap();
    assert_eq!(
        second,
        ReleaseOutcome::AlreadyReleased {
            claim_id: "claim-3".to_string()
        }
    );
    let after = get_claim(&conn, "claim-3").unwrap().unwrap();
    assert_eq!(
        after.released_at, stamp_after_first,
        "stamp unchanged on no-op"
    );
    assert!(
        after.release_reason.as_deref() != Some("second-reason"),
        "reason must not be overwritten by an idempotent release"
    );
}

#[test]
fn release_missing_claim_reports_not_found() {
    let mut conn = open_conn();
    let outcome =
        release_claim(&mut conn, &ClaimSelector::ClaimId("nope".to_string()), None).unwrap();
    assert_eq!(outcome, ReleaseOutcome::NotFound);
}

#[test]
fn release_by_claim_id_rejects_v21_work_claim_alias() {
    let mut conn = open_conn();
    identity(&conn, "agent-v21-claim-id");
    insert_work_claim(&conn, &work_claim("claim-v21-direct", "agent-v21-claim-id")).unwrap();

    let err = release_claim(
        &mut conn,
        &ClaimSelector::ClaimId("claim-v21-direct".to_string()),
        Some("legacy alias"),
    )
    .expect_err("legacy release must reject direct v21 WorkClaim aliases");
    let text = err.to_string();
    assert!(text.contains("canonical tachi_task release"), "{text}");
    assert!(text.contains("transition_version"), "{text}");

    let got = get_claim(&conn, "claim-v21-direct").unwrap().unwrap();
    assert_eq!(got.state, ClaimState::Active);
    assert_eq!(got.transition_version, 0);
    assert!(got.released_at.is_none());
    assert!(got.release_reason.is_none());
}

#[test]
fn release_by_dispatch_id_targets_the_active_claim() {
    let mut conn = open_conn();
    // A stale released row plus a live active row for the same dispatch.
    let mut old = new_claim("claim-old", "org/repo#4");
    old.dispatch_id = Some("dispatch-shared".to_string());
    insert_claim(&conn, &old).unwrap();
    release_claim(
        &mut conn,
        &ClaimSelector::ClaimId("claim-old".to_string()),
        None,
    )
    .unwrap();
    let mut fresh = new_claim("claim-new", "org/repo#4");
    fresh.dispatch_id = Some("dispatch-shared".to_string());
    insert_claim(&conn, &fresh).unwrap();

    let outcome = release_claim(
        &mut conn,
        &ClaimSelector::DispatchId("dispatch-shared".to_string()),
        Some("complete"),
    )
    .unwrap();
    assert_eq!(
        outcome,
        ReleaseOutcome::Released {
            claim_id: "claim-new".to_string()
        }
    );
}

#[test]
fn release_by_dispatch_id_rejects_v21_work_claim_alias() {
    let mut conn = open_conn();
    identity(&conn, "agent-v21-dispatch");
    let mut claim = work_claim("claim-v21-dispatch", "agent-v21-dispatch");
    claim.dispatch_id = Some("dispatch-v21-only".to_string());
    insert_work_claim(&conn, &claim).unwrap();

    let err = release_claim(
        &mut conn,
        &ClaimSelector::DispatchId("dispatch-v21-only".to_string()),
        Some("complete"),
    )
    .expect_err("legacy release must reject v21 WorkClaim dispatch aliases");
    let text = err.to_string();
    assert!(text.contains("canonical tachi_task release"), "{text}");
    assert!(text.contains("transition_version"), "{text}");

    let got = get_claim(&conn, "claim-v21-dispatch").unwrap().unwrap();
    assert_eq!(got.state, ClaimState::Active);
    assert_eq!(got.transition_version, 0);
    assert!(got.released_at.is_none());
    assert!(got.release_reason.is_none());
}

#[test]
fn release_by_dispatch_id_releases_only_legacy_presence_when_mixed_with_v21_work_claim() {
    let mut conn = open_conn();
    identity(&conn, "agent-v21-mixed");

    let mut v21 = work_claim("claim-v21-mixed", "agent-v21-mixed");
    v21.dispatch_id = Some("dispatch-mixed".to_string());
    v21.created_at = "2030-01-01T00:00:00Z".to_string();
    insert_work_claim(&conn, &v21).unwrap();

    let mut legacy = new_claim("claim-legacy-mixed", "org/repo#legacy-mixed");
    legacy.dispatch_id = Some("dispatch-mixed".to_string());
    legacy.created_at = "2020-01-01T00:00:00Z".to_string();
    insert_claim(&conn, &legacy).unwrap();

    let outcome = release_claim(
        &mut conn,
        &ClaimSelector::DispatchId("dispatch-mixed".to_string()),
        Some("complete"),
    )
    .unwrap();
    assert_eq!(
        outcome,
        ReleaseOutcome::Released {
            claim_id: "claim-legacy-mixed".to_string()
        }
    );

    let released_legacy = get_claim(&conn, "claim-legacy-mixed").unwrap().unwrap();
    assert_eq!(released_legacy.state, ClaimState::Released);
    assert_eq!(released_legacy.release_reason.as_deref(), Some("complete"));
    assert!(released_legacy.released_at.is_some());

    let active_v21 = get_claim(&conn, "claim-v21-mixed").unwrap().unwrap();
    assert_eq!(active_v21.state, ClaimState::Active);
    assert_eq!(active_v21.transition_version, 0);
    assert!(active_v21.released_at.is_none());
    assert!(active_v21.release_reason.is_none());
}

#[test]
fn list_filters_by_state() {
    let mut conn = open_conn();
    insert_claim(&conn, &new_claim("claim-a", "org/repo#a")).unwrap();
    insert_claim(&conn, &new_claim("claim-b", "org/repo#b")).unwrap();
    release_claim(
        &mut conn,
        &ClaimSelector::ClaimId("claim-b".to_string()),
        None,
    )
    .unwrap();

    let active = list_claims(&conn, Some(ClaimState::Active)).unwrap();
    assert_eq!(active.len(), 1);
    assert_eq!(active[0].claim_id, "claim-a");

    let released = list_claims(&conn, Some(ClaimState::Released)).unwrap();
    assert_eq!(released.len(), 1);
    assert_eq!(released[0].claim_id, "claim-b");

    let all = list_claims(&conn, None).unwrap();
    assert_eq!(all.len(), 2);
}

#[test]
fn state_parse_rejects_unknown() {
    assert!(ClaimState::parse("provisioned").is_err());
    assert_eq!(ClaimState::parse("active").unwrap(), ClaimState::Active);
    assert_eq!(ClaimState::parse("released").unwrap(), ClaimState::Released);
}

// --- TTL expiry / heartbeat discrimination (#1001 acceptance) ---------

fn claim_with_heartbeat(heartbeat_at: &str) -> SessionClaim {
    SessionClaim {
        claim_id: "c".to_string(),
        session_client: None,
        issue_ref: None,
        flow_id: None,
        dispatch_id: None,
        branch: String::new(),
        worktree_path: None,
        declared_file_scope: None,
        agent_identity_id: None,
        role: None,
        mode: None,
        expected_head: None,
        lease_expires_at: None,
        transition_version: 0,
        exec_env_id: None,
        orphaned_at: None,
        state: ClaimState::Active,
        release_reason: None,
        created_at: heartbeat_at.to_string(),
        heartbeat_at: heartbeat_at.to_string(),
        released_at: None,
    }
}

#[test]
fn fresh_heartbeat_survives_ttl_boundary() {
    let now = chrono::DateTime::parse_from_rfc3339("2026-07-11T12:00:00Z")
        .unwrap()
        .to_utc();
    // Heartbeat 5 minutes ago, TTL 30 minutes: must NOT be stale.
    let claim = claim_with_heartbeat("2026-07-11T11:55:00Z");
    assert!(!is_claim_stale(&claim, now, 1800));
}

#[test]
fn stale_heartbeat_past_ttl_is_reaped() {
    let now = chrono::DateTime::parse_from_rfc3339("2026-07-11T12:00:00Z")
        .unwrap()
        .to_utc();
    // Heartbeat 31 minutes ago, TTL 30 minutes: must BE stale.
    let claim = claim_with_heartbeat("2026-07-11T11:29:00Z");
    assert!(is_claim_stale(&claim, now, 1800));
}

#[test]
fn ttl_boundary_is_exact_not_off_by_one() {
    let now = chrono::DateTime::parse_from_rfc3339("2026-07-11T12:00:00Z")
        .unwrap()
        .to_utc();
    // Heartbeat exactly TTL seconds ago: boundary is "> ttl", so exactly
    // at the TTL must still be fresh (not stale).
    let claim = claim_with_heartbeat("2026-07-11T11:30:00Z");
    assert!(!is_claim_stale(&claim, now, 1800));
    // One second further back must be stale.
    let claim2 = claim_with_heartbeat("2026-07-11T11:29:59Z");
    assert!(is_claim_stale(&claim2, now, 1800));
}

#[test]
fn unparsable_heartbeat_is_treated_as_stale_fail_closed() {
    let now = chrono::DateTime::parse_from_rfc3339("2026-07-11T12:00:00Z")
        .unwrap()
        .to_utc();
    let claim = claim_with_heartbeat("not-a-timestamp");
    assert!(is_claim_stale(&claim, now, 1_000_000_000));
}

#[test]
fn list_active_claims_excludes_stale_and_released() {
    let conn = open_conn();
    insert_claim(&conn, &new_claim("fresh", "org/repo#f")).unwrap();

    // Manually backdate a second claim's heartbeat past the TTL horizon
    // (simulating a dead session that stopped heartbeating), rather than
    // sleeping in the test.
    insert_claim(&conn, &new_claim("stale", "org/repo#s")).unwrap();
    conn.execute(
        "UPDATE session_claims SET heartbeat_at = '2020-01-01T00:00:00Z' WHERE claim_id = 'stale'",
        [],
    )
    .unwrap();

    let now_iso = normalize_utc_iso_or_now("");
    let active = list_active_claims(&conn, &now_iso, 1800).unwrap();
    assert_eq!(active.len(), 1);
    assert_eq!(active[0].claim_id, "fresh");
}

#[test]
fn two_concurrent_sessions_each_see_the_others_claim() {
    // Discrimination for the briefing acceptance criterion: two sessions
    // claim different issues; both rows are visible via list_active_claims
    // (the read path the briefing splice uses) regardless of who inserted
    // which row.
    let conn = open_conn();
    let mut session_a = new_claim("sess-a-claim", "org/repo#100");
    session_a.session_client = Some("claude-code".to_string());
    insert_claim(&conn, &session_a).unwrap();

    let mut session_b = new_claim("sess-b-claim", "org/repo#200");
    session_b.session_client = Some("codex".to_string());
    insert_claim(&conn, &session_b).unwrap();

    let now_iso = normalize_utc_iso_or_now("");
    let active = list_active_claims(&conn, &now_iso, 1800).unwrap();
    assert_eq!(active.len(), 2);
    assert!(active.iter().any(
        |c| c.claim_id == "sess-a-claim" && c.session_client.as_deref() == Some("claude-code")
    ));
    assert!(active
        .iter()
        .any(|c| c.claim_id == "sess-b-claim" && c.session_client.as_deref() == Some("codex")));
}

// --- upsert_or_heartbeat_claim (#1001 zero-ceremony hook path) --------

#[test]
fn upsert_inserts_fresh_claim_when_none_exists() {
    let mut conn = open_conn();
    let claim = new_claim("hook-claim-1", "org/repo#500");
    let claim_id = upsert_or_heartbeat_claim(&mut conn, &claim).unwrap();
    assert_eq!(claim_id, "hook-claim-1");
    let got = get_claim(&conn, "hook-claim-1").unwrap().unwrap();
    assert_eq!(got.state, ClaimState::Active);
}

#[test]
fn upsert_heartbeats_existing_active_claim_for_same_identity_no_duplicate_row() {
    let mut conn = open_conn();
    let first = new_claim("hook-claim-2", "org/repo#501");
    let claim_id_1 = upsert_or_heartbeat_claim(&mut conn, &first).unwrap();

    // Re-entering briefing/intake/dispatch for the SAME session+issue+flow
    // must heartbeat the existing row, not insert a second one — even
    // though `claim_id` on the second call differs (a caller re-generates
    // a fresh candidate id every call; the upsert key is
    // session_client+issue_ref+flow_id, not claim_id).
    let mut second = new_claim("hook-claim-2-b", "org/repo#501");
    second.branch = "feat/x".to_string();
    let claim_id_2 = upsert_or_heartbeat_claim(&mut conn, &second).unwrap();

    assert_eq!(
        claim_id_1, claim_id_2,
        "second call must resolve to the same existing claim, not insert a new row"
    );
    let all = list_claims(&conn, Some(ClaimState::Active)).unwrap();
    assert_eq!(
        all.len(),
        1,
        "must not duplicate the row for the same session+issue+flow"
    );
}

#[test]
fn upsert_creates_separate_claims_for_different_issues() {
    let mut conn = open_conn();
    let mut a = new_claim("hook-claim-3a", "org/repo#600");
    a.flow_id = Some("flow-a".to_string());
    let mut b = new_claim("hook-claim-3b", "org/repo#601");
    b.flow_id = Some("flow-b".to_string());

    upsert_or_heartbeat_claim(&mut conn, &a).unwrap();
    upsert_or_heartbeat_claim(&mut conn, &b).unwrap();

    let all = list_claims(&conn, Some(ClaimState::Active)).unwrap();
    assert_eq!(
        all.len(),
        2,
        "distinct issue/flow identities get distinct rows"
    );
}

/// #1001 round 3, item 2 (codex "identity upsert not atomic" finding):
/// two REAL concurrent same-identity `upsert_or_heartbeat_claim` callers
/// (separate OS threads, separate connections, a `Barrier` forcing both
/// to race the `INSERT ... ON CONFLICT` at the same instant) must both
/// return Ok, must resolve to exactly one row for the identity, and that
/// row's heartbeat must reflect the later of the two calls — never a
/// lost heartbeat, never an error from either side. A read-then-write
/// upsert loses this race (both threads can observe "no existing row"
/// before either commits); the `ON CONFLICT ... DO UPDATE` statement
/// closes it at the database level.
#[test]
fn two_concurrent_same_identity_upserts_both_succeed_exactly_one_row() {
    let tmp = tempfile::NamedTempFile::new().expect("tempfile");
    let path = tmp.path().to_path_buf();
    // Establish the schema (and the partial unique index) once before
    // spawning the racing connections.
    {
        let _ = open_file_conn(&path);
    }

    // Prepare both connections before either worker reaches the rendezvous.
    // `open_file_conn` runs schema initialization, which is fallible and
    // must not happen inside one side of a two-party barrier: if it panics,
    // the peer can otherwise wait forever and hide the original failure.
    let db_path = path.to_str().expect("temporary DB path must be UTF-8");
    let mut conn_a = crate::db::open_read_write(db_path).expect("open racing connection A");
    let mut conn_b = crate::db::open_read_write(db_path).expect("open racing connection B");

    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let mut claim_a = new_claim("race-claim-a", "org/repo#900");
    claim_a.dispatch_id = Some("dispatch-a".to_string());
    let barrier_a = barrier.clone();
    let handle_a = std::thread::spawn(move || {
        barrier_a.wait();
        upsert_or_heartbeat_claim(&mut conn_a, &claim_a)
    });

    let mut claim_b = new_claim("race-claim-b", "org/repo#900");
    claim_b.dispatch_id = Some("dispatch-b".to_string());
    let barrier_b = barrier;
    let handle_b = std::thread::spawn(move || {
        barrier_b.wait();
        upsert_or_heartbeat_claim(&mut conn_b, &claim_b)
    });

    // Reap both workers before surfacing either panic so one failure cannot
    // detach the other worker from the test harness.
    let join_a = handle_a.join();
    let join_b = handle_b.join();
    let result_a = join_a.expect("thread a must not panic");
    let result_b = join_b.expect("thread b must not panic");

    assert!(
        result_a.is_ok(),
        "concurrent identity race must not error the loser: {result_a:?}"
    );
    assert!(
        result_b.is_ok(),
        "concurrent identity race must not error the winner: {result_b:?}"
    );

    let verify_conn = open_file_conn(&path);
    let active = list_claims(&verify_conn, Some(ClaimState::Active))
        .unwrap()
        .into_iter()
        .filter(|c| c.issue_ref.as_deref() == Some("org/repo#900"))
        .collect::<Vec<_>>();
    assert_eq!(
        active.len(),
        1,
        "exactly one active row must exist for the raced identity, got {active:?}"
    );

    // Both calls resolved to the SAME claim_id (whichever inserted first;
    // the other heartbeated onto it) — the two threads never produced two
    // independent claim ids.
    let claim_id_a = result_a.unwrap();
    let claim_id_b = result_b.unwrap();
    assert_eq!(
        claim_id_a, claim_id_b,
        "both concurrent callers must resolve to the identical winning claim_id"
    );
    assert_eq!(active[0].claim_id, claim_id_a);
}

// --- gc_session_claims (#1001 follow-up / R2 review of #1007 CONCERN) -

#[test]
fn gc_prunes_released_rows_older_than_max_age() {
    let mut conn = open_conn();
    insert_claim(&conn, &new_claim("aged-released", "org/repo#700")).unwrap();
    release_claim(
        &mut conn,
        &ClaimSelector::ClaimId("aged-released".to_string()),
        Some("manual release"),
    )
    .unwrap();
    // Backdate released_at to 35 days before "now" — past the 30-day
    // prune window.
    conn.execute(
        "UPDATE session_claims SET released_at = '2026-06-06T00:00:00.000Z' \
         WHERE claim_id = 'aged-released'",
        [],
    )
    .unwrap();

    let now = chrono::DateTime::parse_from_rfc3339("2026-07-11T00:00:00Z")
        .unwrap()
        .to_utc();
    let outcome = gc_session_claims(&conn, now, 7, 30).unwrap();
    assert_eq!(outcome.released_pruned, 1);
    assert_eq!(outcome.active_orphaned, 0);
    assert!(
        get_claim(&conn, "aged-released").unwrap().is_none(),
        "released row older than the 30-day window must be deleted, not just marked"
    );
}

#[test]
fn gc_staleness_orphans_dead_active_rows_past_heartbeat_ttl() {
    let conn = open_conn();
    insert_claim(&conn, &new_claim("dead-heartbeat", "org/repo#701")).unwrap();
    // Backdate heartbeat_at to 8 days before "now" — past the 7-day
    // staleness window, simulating a crashed session that stopped
    // heartbeating and never called release_claim.
    conn.execute(
        "UPDATE session_claims SET heartbeat_at = '2026-07-03T00:00:00.000Z' \
         WHERE claim_id = 'dead-heartbeat'",
        [],
    )
    .unwrap();

    let now = chrono::DateTime::parse_from_rfc3339("2026-07-11T00:00:00Z")
        .unwrap()
        .to_utc();
    let outcome = gc_session_claims(&conn, now, 7, 30).unwrap();
    assert_eq!(outcome.active_orphaned, 1);
    assert_eq!(outcome.released_pruned, 0);

    let got = get_claim(&conn, "dead-heartbeat").unwrap().unwrap();
    assert_eq!(
        got.state,
        ClaimState::Orphaned,
        "dead-heartbeat active row must be orphaned, never server-released"
    );
    assert!(got.release_reason.is_none());
    assert!(got.orphaned_at.is_some());
}

#[test]
fn gc_leaves_fresh_active_row_untouched() {
    let conn = open_conn();
    insert_claim(&conn, &new_claim("fresh-claim", "org/repo#702")).unwrap();
    // heartbeat_at defaults to created_at == real "now" via new_claim's
    // empty created_at, so use the actual current time as the GC clock
    // too — this row is well within both windows either way.
    let now = chrono::Utc::now();
    let outcome = gc_session_claims(&conn, now, 7, 30).unwrap();
    assert_eq!(
        outcome.active_orphaned, 0,
        "a fresh active row must not be orphaned"
    );
    assert_eq!(
        outcome.released_pruned, 0,
        "no released rows exist yet to prune"
    );

    let got = get_claim(&conn, "fresh-claim").unwrap().unwrap();
    assert_eq!(got.state, ClaimState::Active, "fresh active row untouched");
    assert!(got.released_at.is_none());
}

fn identity(conn: &Connection, id: &str) {
    insert_agent_identity(
        conn,
        &AgentIdentity {
            agent_identity_id: id.into(),
            display_name: Some("display only".into()),
            seat: None,
            capability_json: None,
            created_at: String::new(),
        },
    )
    .unwrap();
}

fn work_claim(id: &str, identity_id: &str) -> NewWorkClaim {
    NewWorkClaim {
        claim_id: id.into(),
        agent_identity_id: identity_id.into(),
        session_client: None,
        issue_ref: Some("org/repo#1253".into()),
        flow_id: None,
        dispatch_id: None,
        branch: "lane/1253".into(),
        worktree_path: "/wt/1253".into(),
        declared_file_scope: "crates/memcore/src/db/**".into(),
        role: "executor".into(),
        mode: WorkClaimMode::Writable,
        expected_head: "3c09b425".into(),
        lease_expires_at: "2026-07-19T00:00:00Z".into(),
        created_at: String::new(),
    }
}

#[test]
fn v21_claim_binding_is_atomic_and_holder_evidence_is_six_way_queryable() {
    let mut conn = open_conn();
    identity(&conn, "agent-a");
    record_unverified_admission(
        &conn,
        "admission-a",
        "agent-a",
        "connection-a",
        UnverifiedAdmissionState::SelfAsserted,
    )
    .unwrap();
    insert_work_claim(&conn, &work_claim("claim-a", "agent-a")).unwrap();
    crate::db::exec_env::insert_exec_env(
        &conn,
        &crate::db::exec_env::NewExecEnvLease {
            env_id: "env-a".into(),
            kind: "worktree".into(),
            path: "/wt/a".into(),
            repo_root: "/repo".into(),
            branch: "lane/1253".into(),
            base_sha: "3c09b425".into(),
            dispatch_id: None,
            env_class: crate::db::exec_env::EnvClass::EditOnly,
            created_at: String::new(),
        },
    )
    .unwrap();
    assert_eq!(
        holder_evidence(&conn, "env-a").unwrap(),
        HolderEvidence::NotApplicable
    );
    assert_eq!(
        bind_work_claim_exec_env(&mut conn, "claim-a", "env-a", 0).unwrap(),
        1
    );
    assert_eq!(
        holder_evidence(&conn, "env-a").unwrap(),
        HolderEvidence::Held
    );
    assert!(
        matches!(
            bind_work_claim_exec_env(&mut conn, "claim-a", "env-a", 0),
            Err(MemoryError::WorkClaimConflict(_))
        ),
        "stale bind version must not overwrite the first binding"
    );
    assert_eq!(
        release_work_claim(&conn, "claim-a", "agent-a", 1, "explicit").unwrap(),
        2
    );
    assert_eq!(
        holder_evidence(&conn, "env-a").unwrap(),
        HolderEvidence::Clear
    );
}

#[test]
fn rejected_admission_keeps_connection_and_evidence_without_identity() {
    let conn = open_conn();
    record_rejected_admission(
        &conn,
        "admission-rejected-no-identity",
        "connection-untrusted",
        "stable identity receipt missing",
    )
    .unwrap();
    let row: (Option<String>, String, String, Option<String>) = conn
        .query_row(
            "SELECT agent_identity_id, connection_id, state, rejection_evidence \
             FROM identity_admissions WHERE admission_id=?1",
            params!["admission-rejected-no-identity"],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    assert_eq!(row.0, None, "a rejected row must not fabricate identity");
    assert_eq!(row.1, "connection-untrusted");
    assert_eq!(row.2, "rejected");
    assert_eq!(row.3.as_deref(), Some("stable identity receipt missing"));
}

#[test]
fn incompatible_or_missing_holder_links_refuse_with_distinct_evidence() {
    let conn = open_conn();
    crate::db::exec_env::insert_exec_env(
        &conn,
        &crate::db::exec_env::NewExecEnvLease {
            env_id: "env-b".into(),
            kind: "worktree".into(),
            path: "/wt/b".into(),
            repo_root: "/repo".into(),
            branch: String::new(),
            base_sha: String::new(),
            dispatch_id: None,
            env_class: crate::db::exec_env::EnvClass::EditOnly,
            created_at: String::new(),
        },
    )
    .unwrap();
    conn.execute(
        "UPDATE exec_envs SET agent_identity_id='agent-a', claim_id='missing' WHERE env_id='env-b'",
        [],
    )
    .unwrap();
    assert_eq!(
        holder_evidence(&conn, "env-b").unwrap(),
        HolderEvidence::Unverifiable
    );
    conn.execute(
        "UPDATE exec_envs SET claim_id=NULL WHERE env_id='env-b'",
        [],
    )
    .unwrap();
    assert_eq!(
        holder_evidence(&conn, "env-b").unwrap(),
        HolderEvidence::Contradictory
    );
    conn.execute(
        "UPDATE exec_envs SET agent_identity_id=NULL, claim_id=NULL WHERE env_id='env-b'",
        [],
    )
    .unwrap();
    identity(&conn, "agent-b");
    insert_work_claim(&conn, &work_claim("claim-b", "agent-b")).unwrap();
    conn.execute(
        "UPDATE exec_envs SET agent_identity_id='agent-b', claim_id='claim-b' WHERE env_id='env-b'",
        [],
    )
    .unwrap();
    conn.execute(
        "UPDATE session_claims SET exec_env_id='env-b', state='bad' WHERE claim_id='claim-b'",
        [],
    )
    .unwrap();
    assert_eq!(
        holder_evidence(&conn, "env-b").unwrap(),
        HolderEvidence::Unavailable
    );
    assert_eq!(
        holder_evidence(&conn, "missing-env").unwrap(),
        HolderEvidence::Unverifiable
    );
}

#[test]
fn reverse_only_holder_link_is_contradictory_not_legacy_not_applicable() {
    let conn = open_conn();
    identity(&conn, "agent-reverse");
    insert_work_claim(&conn, &work_claim("claim-reverse", "agent-reverse")).unwrap();
    crate::db::exec_env::insert_exec_env(
        &conn,
        &crate::db::exec_env::NewExecEnvLease {
            env_id: "env-reverse".into(),
            kind: "worktree".into(),
            path: "/wt/reverse".into(),
            repo_root: "/repo".into(),
            branch: "lane/1253".into(),
            base_sha: "3c09b425".into(),
            dispatch_id: None,
            env_class: crate::db::exec_env::EnvClass::EditOnly,
            created_at: String::new(),
        },
    )
    .unwrap();
    conn.execute(
        "UPDATE session_claims SET exec_env_id='env-reverse' WHERE claim_id='claim-reverse'",
        [],
    )
    .unwrap();

    assert_eq!(
        holder_evidence(&conn, "env-reverse").unwrap(),
        HolderEvidence::Contradictory
    );
}

#[test]
fn forward_claim_with_different_reverse_claim_is_contradictory() {
    let conn = open_conn();
    identity(&conn, "agent-forward");
    identity(&conn, "agent-reverse-other");
    insert_work_claim(&conn, &work_claim("claim-forward", "agent-forward")).unwrap();
    let mut reverse_other = work_claim("claim-reverse-other", "agent-reverse-other");
    reverse_other.issue_ref = Some("org/repo#1253-other".into());
    reverse_other.worktree_path = "/wt/1253-other".into();
    reverse_other.declared_file_scope = "crates/memcore/src/db/other.rs".into();
    insert_work_claim(&conn, &reverse_other).unwrap();
    crate::db::exec_env::insert_exec_env(
        &conn,
        &crate::db::exec_env::NewExecEnvLease {
            env_id: "env-split".into(),
            kind: "worktree".into(),
            path: "/wt/split".into(),
            repo_root: "/repo".into(),
            branch: "lane/1253".into(),
            base_sha: "3c09b425".into(),
            dispatch_id: None,
            env_class: crate::db::exec_env::EnvClass::EditOnly,
            created_at: String::new(),
        },
    )
    .unwrap();
    conn.execute(
        "UPDATE exec_envs SET agent_identity_id='agent-forward', claim_id='claim-forward' WHERE env_id='env-split'",
        [],
    )
    .unwrap();
    conn.execute(
        "UPDATE session_claims SET exec_env_id='env-split' WHERE claim_id='claim-reverse-other'",
        [],
    )
    .unwrap();
    conn.execute(
        "UPDATE session_claims SET exec_env_id='env-split' WHERE claim_id='claim-forward'",
        [],
    )
    .unwrap();

    assert_eq!(
        holder_evidence(&conn, "env-split").unwrap(),
        HolderEvidence::Contradictory
    );
}

#[test]
fn work_claim_collision_matrix_keeps_read_only_and_disjoint_writes_but_protects_orphans() {
    let conn = open_conn();
    for identity_id in [
        "agent-ro-a",
        "agent-ro-b",
        "agent-write-a",
        "agent-write-b",
        "agent-scope-conflict",
        "agent-path-conflict",
        "agent-head-conflict",
        "agent-orphan-a",
        "agent-orphan-b",
    ] {
        identity(&conn, identity_id);
    }

    let mut read_only_a = work_claim("ro-a", "agent-ro-a");
    read_only_a.mode = WorkClaimMode::ReadOnly;
    read_only_a.worktree_path = String::new();
    read_only_a.declared_file_scope = r#"["crates/one/**"]"#.into();
    insert_work_claim(&conn, &read_only_a).unwrap();

    let mut read_only_b = work_claim("ro-b", "agent-ro-b");
    read_only_b.mode = WorkClaimMode::ReadOnly;
    read_only_b.worktree_path = String::new();
    read_only_b.declared_file_scope = r#"["crates/one/**"]"#.into();
    insert_work_claim(&conn, &read_only_b).unwrap();

    let mut write_a = work_claim("write-a", "agent-write-a");
    write_a.worktree_path = "/worktrees/a".into();
    write_a.declared_file_scope = r#"["crates/a/**"]"#.into();
    insert_work_claim(&conn, &write_a).unwrap();

    let mut write_b = work_claim("write-b", "agent-write-b");
    write_b.worktree_path = "/worktrees/b".into();
    write_b.declared_file_scope = r#"["crates/b/**"]"#.into();
    insert_work_claim(&conn, &write_b).unwrap();

    let mut scope_conflict = work_claim("scope-conflict", "agent-scope-conflict");
    scope_conflict.worktree_path = "/worktrees/c".into();
    scope_conflict.declared_file_scope = r#"["crates/a/subtree/**"]"#.into();
    assert!(matches!(
        insert_work_claim(&conn, &scope_conflict),
        Err(MemoryError::WorkClaimConflict(_))
    ));

    let mut path_conflict = work_claim("path-conflict", "agent-path-conflict");
    path_conflict.worktree_path = "/worktrees/a/nested".into();
    path_conflict.declared_file_scope = r#"["crates/c/**"]"#.into();
    assert!(matches!(
        insert_work_claim(&conn, &path_conflict),
        Err(MemoryError::WorkClaimConflict(_))
    ));

    let mut head_conflict = work_claim("head-conflict", "agent-head-conflict");
    head_conflict.mode = WorkClaimMode::ReadOnly;
    head_conflict.worktree_path = String::new();
    head_conflict.expected_head = "different-head".into();
    assert!(matches!(
        insert_work_claim(&conn, &head_conflict),
        Err(MemoryError::WorkClaimConflict(_))
    ));

    let mut orphaned = work_claim("orphaned", "agent-orphan-a");
    orphaned.issue_ref = Some("org/repo#orphan".into());
    orphaned.worktree_path = "/worktrees/orphan".into();
    orphaned.declared_file_scope = r#"["crates/orphan/**"]"#.into();
    insert_work_claim(&conn, &orphaned).unwrap();
    conn.execute(
        "UPDATE session_claims SET state='orphaned' WHERE claim_id='orphaned'",
        [],
    )
    .unwrap();
    let mut takeover = work_claim("takeover", "agent-orphan-b");
    takeover.issue_ref = Some("org/repo#orphan".into());
    takeover.worktree_path = "/worktrees/orphan".into();
    takeover.declared_file_scope = r#"["crates/orphan/**"]"#.into();
    assert!(matches!(
        insert_work_claim(&conn, &takeover),
        Err(MemoryError::WorkClaimConflict(_))
    ));
}

#[test]
fn writable_work_claims_collide_on_one_tree_across_issue_boundaries() {
    let conn = open_conn();
    for identity_id in ["agent-tree-a", "agent-tree-b", "agent-tree-none"] {
        identity(&conn, identity_id);
    }

    let mut first = work_claim("tree-a", "agent-tree-a");
    first.issue_ref = Some("org/repo#tree-a".into());
    first.worktree_path = "/worktrees/canonical-shared".into();
    first.declared_file_scope = r#"["crates/a/**"]"#.into();
    insert_work_claim(&conn, &first).unwrap();

    let mut different_issue = work_claim("tree-b", "agent-tree-b");
    different_issue.issue_ref = Some("org/repo#tree-b".into());
    different_issue.worktree_path = first.worktree_path.clone();
    different_issue.declared_file_scope = r#"["crates/b/**"]"#.into();
    let different_issue_error = insert_work_claim(&conn, &different_issue)
        .expect_err("different issues cannot own the same writable physical tree");
    assert!(matches!(
        different_issue_error,
        MemoryError::WorkClaimConflict(_)
    ));
    assert!(
        different_issue_error
            .to_string()
            .contains("writable worktree path overlaps"),
        "unexpected conflict: {different_issue_error}"
    );

    let mut no_issue = work_claim("tree-none", "agent-tree-none");
    no_issue.issue_ref = None;
    no_issue.worktree_path = first.worktree_path.clone();
    no_issue.declared_file_scope = r#"["crates/no-issue/**"]"#.into();
    let no_issue_error = insert_work_claim(&conn, &no_issue)
        .expect_err("an unscoped claim cannot own an already-held writable physical tree");
    assert!(matches!(no_issue_error, MemoryError::WorkClaimConflict(_)));
    assert!(
        no_issue_error
            .to_string()
            .contains("writable worktree path overlaps"),
        "unexpected conflict: {no_issue_error}"
    );
}

#[test]
fn work_claim_refuses_an_empty_serialized_scope() {
    let conn = open_conn();
    identity(&conn, "agent-empty-scope");
    let mut claim = work_claim("empty-scope", "agent-empty-scope");
    claim.declared_file_scope = "[]".into();
    assert!(matches!(
        insert_work_claim(&conn, &claim),
        Err(MemoryError::InvalidArg(_))
    ));
}

#[test]
fn work_claim_transition_authorization_is_holder_enforced_and_typed() {
    let conn = open_conn();
    identity(&conn, "agent-holder");
    identity(&conn, "agent-intruder");

    let mut heartbeat = work_claim("auth-heartbeat", "agent-holder");
    heartbeat.issue_ref = Some("org/repo#auth-heartbeat".into());
    heartbeat.mode = WorkClaimMode::ReadOnly;
    heartbeat.worktree_path = String::new();
    insert_work_claim(&conn, &heartbeat).unwrap();
    let heartbeat_error = heartbeat_work_claim(
        &conn,
        "auth-heartbeat",
        "agent-intruder",
        0,
        "2030-01-02T00:00:00Z",
    )
    .expect_err("non-holder heartbeat must be refused");
    assert!(matches!(
        heartbeat_error,
        MemoryError::WorkClaimTransitionRefused {
            reason: WorkClaimTransitionReason::HolderMismatch,
            ..
        }
    ));

    let mut release = work_claim("auth-release", "agent-holder");
    release.issue_ref = Some("org/repo#auth-release".into());
    release.mode = WorkClaimMode::ReadOnly;
    release.worktree_path = String::new();
    insert_work_claim(&conn, &release).unwrap();
    let release_error = release_work_claim(&conn, "auth-release", "agent-intruder", 0, "intruder")
        .expect_err("non-holder release must be refused");
    assert!(matches!(
        release_error,
        MemoryError::WorkClaimTransitionRefused {
            reason: WorkClaimTransitionReason::HolderMismatch,
            ..
        }
    ));

    let mut handoff = work_claim("auth-handoff", "agent-holder");
    handoff.issue_ref = Some("org/repo#auth-handoff".into());
    handoff.mode = WorkClaimMode::ReadOnly;
    handoff.worktree_path = String::new();
    insert_work_claim(&conn, &handoff).unwrap();
    let successor = WorkClaimHandoffRequest {
        agent_identity_id: "agent-intruder".into(),
        role: "executor".into(),
        mode: WorkClaimMode::ReadOnly,
        worktree_path: String::new(),
        declared_file_scope: r#"["crates/auth/**"]"#.into(),
        expected_head: "3c09b425".into(),
        lease_expires_at: "2030-01-02T00:00:00Z".into(),
    };
    let handoff_error = handoff_work_claim(&conn, "auth-handoff", "agent-intruder", 0, &successor)
        .expect_err("non-holder active handoff must be refused");
    assert!(matches!(
        handoff_error,
        MemoryError::WorkClaimTransitionRefused {
            reason: WorkClaimTransitionReason::HolderMismatch,
            ..
        }
    ));
    let unchanged = get_claim(&conn, "auth-handoff").unwrap().unwrap();
    assert_eq!(unchanged.state, ClaimState::Active);
    assert_eq!(unchanged.transition_version, 0);
    assert_eq!(unchanged.agent_identity_id.as_deref(), Some("agent-holder"));

    conn.execute(
        "UPDATE session_claims SET state='orphaned' WHERE claim_id='auth-handoff'",
        [],
    )
    .unwrap();
    let recovered = handoff_work_claim(&conn, "auth-handoff", "agent-intruder", 0, &successor)
        .expect("an admitted successor may recover an orphaned claim");
    assert_eq!(recovered.from_agent_identity_id, "agent-holder");
    assert_eq!(recovered.to_agent_identity_id, "agent-intruder");
    assert_eq!(recovered.transition_version, 1);
}

#[test]
fn work_claim_heartbeat_and_handoff_are_versioned_and_never_auto_take_over() {
    let conn = open_conn();
    for identity_id in [
        "agent-heartbeat",
        "agent-handoff-from",
        "agent-handoff-to",
        "agent-blocker",
    ] {
        identity(&conn, identity_id);
    }

    let mut heartbeat = work_claim("heartbeat", "agent-heartbeat");
    heartbeat.issue_ref = Some("org/repo#heartbeat".into());
    insert_work_claim(&conn, &heartbeat).unwrap();
    let receipt = heartbeat_work_claim(
        &conn,
        "heartbeat",
        "agent-heartbeat",
        0,
        "2026-07-20T00:00:00Z",
    )
    .unwrap();
    assert_eq!(receipt.transition_version, 1);
    assert_eq!(receipt.lease_expires_at, "2026-07-20T00:00:00Z");
    assert!(matches!(
        heartbeat_work_claim(
            &conn,
            "heartbeat",
            "agent-heartbeat",
            0,
            "2026-07-21T00:00:00Z"
        ),
        Err(MemoryError::WorkClaimConflict(_))
    ));
    release_work_claim(&conn, "heartbeat", "agent-heartbeat", 1, "done").unwrap();
    assert!(matches!(
        heartbeat_work_claim(
            &conn,
            "heartbeat",
            "agent-heartbeat",
            2,
            "2026-07-21T00:00:00Z"
        ),
        Err(MemoryError::WorkClaimIncompatibleState(_))
    ));
    assert!(matches!(
        heartbeat_work_claim(
            &conn,
            "missing",
            "agent-heartbeat",
            0,
            "2026-07-21T00:00:00Z"
        ),
        Err(MemoryError::NotFound(_))
    ));
    let mut orphaned_heartbeat = work_claim("orphaned-heartbeat", "agent-heartbeat");
    orphaned_heartbeat.issue_ref = Some("org/repo#orphaned-heartbeat".into());
    insert_work_claim(&conn, &orphaned_heartbeat).unwrap();
    conn.execute(
        "UPDATE session_claims SET state='orphaned' WHERE claim_id='orphaned-heartbeat'",
        [],
    )
    .unwrap();
    assert!(matches!(
        heartbeat_work_claim(
            &conn,
            "orphaned-heartbeat",
            "agent-heartbeat",
            0,
            "2026-07-21T00:00:00Z"
        ),
        Err(MemoryError::WorkClaimIncompatibleState(_))
    ));

    let mut handoff = work_claim("handoff", "agent-handoff-from");
    handoff.issue_ref = Some("org/repo#handoff".into());
    handoff.worktree_path = "/worktrees/handoff".into();
    handoff.declared_file_scope = r#"["crates/handoff/**"]"#.into();
    insert_work_claim(&conn, &handoff).unwrap();
    conn.execute(
        "UPDATE session_claims SET state='orphaned' WHERE claim_id='handoff'",
        [],
    )
    .unwrap();
    let successor = WorkClaimHandoffRequest {
        agent_identity_id: "agent-handoff-to".into(),
        role: "executor".into(),
        mode: WorkClaimMode::Writable,
        worktree_path: "/worktrees/handoff-new".into(),
        declared_file_scope: r#"["crates/handoff/**"]"#.into(),
        expected_head: "3c09b425".into(),
        lease_expires_at: "2026-07-20T00:00:00Z".into(),
    };
    assert!(matches!(
        handoff_work_claim(&conn, "handoff", "agent-handoff-to", 7, &successor),
        Err(MemoryError::WorkClaimConflict(_))
    ));
    let handoff_result =
        handoff_work_claim(&conn, "handoff", "agent-handoff-to", 0, &successor).unwrap();
    assert_eq!(handoff_result.from_agent_identity_id, "agent-handoff-from");
    assert_eq!(handoff_result.to_agent_identity_id, "agent-handoff-to");
    let handed_off = get_claim(&conn, "handoff").unwrap().unwrap();
    assert_eq!(handed_off.state, ClaimState::Active);
    assert_eq!(
        handed_off.agent_identity_id.as_deref(),
        Some("agent-handoff-to")
    );
    assert_eq!(handed_off.transition_version, 1);

    let mut blocker = work_claim("blocker", "agent-blocker");
    blocker.issue_ref = Some("org/repo#handoff-conflict".into());
    blocker.worktree_path = "/worktrees/blocker".into();
    blocker.declared_file_scope = r#"["crates/blocker/**"]"#.into();
    insert_work_claim(&conn, &blocker).unwrap();
    let mut source = work_claim("handoff-conflict", "agent-handoff-from");
    source.issue_ref = Some("org/repo#handoff-conflict".into());
    source.mode = WorkClaimMode::ReadOnly;
    source.worktree_path = String::new();
    source.declared_file_scope = r#"["crates/source/**"]"#.into();
    insert_work_claim(&conn, &source).unwrap();
    let conflicting_successor = WorkClaimHandoffRequest {
        agent_identity_id: "agent-handoff-from".into(),
        role: "executor".into(),
        mode: WorkClaimMode::Writable,
        worktree_path: "/worktrees/blocker/nested".into(),
        declared_file_scope: r#"["crates/other/**"]"#.into(),
        expected_head: "3c09b425".into(),
        lease_expires_at: "2026-07-20T00:00:00Z".into(),
    };
    assert!(matches!(
        handoff_work_claim(
            &conn,
            "handoff-conflict",
            "agent-handoff-from",
            0,
            &conflicting_successor
        ),
        Err(MemoryError::WorkClaimConflict(_))
    ));
    let unchanged = get_claim(&conn, "handoff-conflict").unwrap().unwrap();
    assert_eq!(
        unchanged.agent_identity_id.as_deref(),
        Some("agent-handoff-from")
    );
    assert_eq!(unchanged.transition_version, 0);
}

#[test]
fn handoff_refuses_an_empty_serialized_scope() {
    let conn = open_conn();
    identity(&conn, "agent-empty-handoff-from");
    identity(&conn, "agent-empty-handoff-to");
    let mut claim = work_claim("empty-handoff", "agent-empty-handoff-from");
    claim.issue_ref = Some("org/repo#empty-handoff".into());
    insert_work_claim(&conn, &claim).unwrap();

    let successor = WorkClaimHandoffRequest {
        agent_identity_id: "agent-empty-handoff-to".into(),
        role: "executor".into(),
        mode: WorkClaimMode::Writable,
        worktree_path: "/worktrees/empty-handoff".into(),
        declared_file_scope: "[]".into(),
        expected_head: "3c09b425".into(),
        lease_expires_at: "2026-07-20T00:00:00Z".into(),
    };
    assert!(matches!(
        handoff_work_claim(
            &conn,
            "empty-handoff",
            "agent-empty-handoff-to",
            0,
            &successor
        ),
        Err(MemoryError::InvalidArg(_))
    ));
}
