use super::*;

fn local(conn: &Connection, id: &str) {
    identity(conn, id);
    record_unverified_admission(
        conn,
        &format!("admit-{id}"),
        id,
        &format!("conn-{id}"),
        UnverifiedAdmissionState::SelfAsserted,
    )
    .unwrap();
}

fn bound_claim(id: &str, agent: &str) -> NewWorkClaim {
    let mut claim = work_claim(id, agent);
    claim.flow_id = Some("flow_owned".into());
    claim.worktree_path = format!("/owned/{id}");
    claim.declared_file_scope = format!("{id}/**");
    claim.lease_expires_at = "2035-01-01T00:00:00Z".into();
    claim
}

#[test]
fn local_project_binding_does_not_upgrade_legacy_or_accept_remote_admission() {
    let conn = open_conn();
    local(&conn, "local");
    identity(&conn, "remote");
    record_unverified_admission(
        &conn,
        "admit-remote",
        "remote",
        "conn-remote",
        UnverifiedAdmissionState::Unavailable,
    )
    .unwrap();
    insert_work_claim(&conn, &bound_claim("legacy", "local")).unwrap();
    assert_eq!(work_claim_project(&conn, "legacy").unwrap(), None);
    assert!(insert_local_project_work_claim(
        &conn,
        &bound_claim("remote", "remote"),
        "conn-remote",
        "alpha"
    )
    .is_err());
    assert!(get_claim(&conn, "remote").unwrap().is_none());
    insert_local_project_work_claim(&conn, &bound_claim("owned", "local"), "conn-local", "alpha")
        .unwrap();
    assert_eq!(
        work_claim_project(&conn, "owned").unwrap().as_deref(),
        Some("alpha")
    );
    assert!(insert_local_project_work_claim(
        &conn,
        &bound_claim("owned", "local"),
        "conn-local",
        "beta"
    )
    .is_err());
    assert_eq!(
        work_claim_project(&conn, "owned").unwrap().as_deref(),
        Some("alpha")
    );
    assert_eq!(list_project_work_claims(&conn, "alpha").unwrap().len(), 1);
    assert!(list_project_work_claims(&conn, "beta").unwrap().is_empty());
}

#[test]
fn project_write_rechecks_local_admission_and_preserves_lifecycle_guards() {
    let conn = open_conn();
    local(&conn, "holder");
    insert_local_project_work_claim(
        &conn,
        &bound_claim("owned", "holder"),
        "conn-holder",
        "alpha",
    )
    .unwrap();
    let write = || {
        with_local_project_claim_write(
            &conn,
            "owned",
            "flow_owned",
            "alpha",
            "holder",
            "conn-holder",
            |conn, _| {
                conn.execute(
                    "UPDATE session_claims SET branch='written' WHERE claim_id='owned'",
                    [],
                )?;
                Ok(())
            },
        )
    };
    write().unwrap();
    conn.execute(
        "UPDATE identity_admissions SET state='rejected' WHERE connection_id='conn-holder'",
        [],
    )
    .unwrap();
    conn.execute(
        "UPDATE session_claims SET branch='unchanged' WHERE claim_id='owned'",
        [],
    )
    .unwrap();
    assert!(write().is_err());
    assert_eq!(
        get_claim(&conn, "owned").unwrap().unwrap().branch,
        "unchanged"
    );
    conn.execute(
        "UPDATE identity_admissions SET state='self_asserted' WHERE connection_id='conn-holder'",
        [],
    )
    .unwrap();
    release_work_claim(&conn, "owned", "holder", 0, "explicit").unwrap();
    assert!(write().is_err());
    assert_eq!(
        work_claim_project(&conn, "owned").unwrap().as_deref(),
        Some("alpha")
    );
    gc_session_claims(
        &conn,
        chrono::Utc::now() + chrono::Duration::days(60),
        7,
        30,
    )
    .unwrap();
    assert_eq!(work_claim_project(&conn, "owned").unwrap(), None);
    assert!(write().is_err());
}

#[test]
fn independent_connections_cannot_bind_one_flow_to_two_projects() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("claims.db");
    let conn = open_file_conn(&path);
    local(&conn, "alpha");
    local(&conn, "beta");
    drop(conn);
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    // Open before the race: SQLite runs auto-extensions during open, before a
    // busy timeout exists, so a racing open can fail and strand the barrier.
    let connections: Vec<_> = ["alpha", "beta"]
        .into_iter()
        .map(|project| {
            let conn = Connection::open(&path).unwrap();
            conn.busy_timeout(std::time::Duration::from_secs(5))
                .unwrap();
            (project, conn)
        })
        .collect();
    let outcomes = std::thread::scope(|scope| {
        let mut handles = Vec::new();
        for (project, conn) in connections {
            let barrier = barrier.clone();
            handles.push(scope.spawn(move || {
                barrier.wait();
                insert_local_project_work_claim(
                    &conn,
                    &bound_claim(project, project),
                    &format!("conn-{project}"),
                    project,
                )
            }));
        }
        handles
            .into_iter()
            .map(|h| h.join().unwrap())
            .collect::<Vec<_>>()
    });
    assert_eq!(outcomes.iter().filter(|result| result.is_ok()).count(), 1);
    let conn = Connection::open(&path).unwrap();
    assert_eq!(
        list_project_work_claims(&conn, "alpha").unwrap().len()
            + list_project_work_claims(&conn, "beta").unwrap().len(),
        1
    );
}

#[test]
fn v40_migration_leaves_existing_claim_project_null() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("legacy.db");
    let conn =
        crate::db::migrations::catalogue::test_support::with_prefix(39, || open_file_conn(&path));
    local(&conn, "legacy");
    insert_work_claim(&conn, &bound_claim("legacy", "legacy")).unwrap();
    drop(conn);
    let store = crate::MemoryStore::open_with_context(
        path.to_str().unwrap(),
        &crate::db::DbOpenContext::open_existing_allow("test:ownership-migration"),
    )
    .unwrap();
    assert_eq!(
        work_claim_project(store.connection(), "legacy").unwrap(),
        None
    );
    assert_eq!(
        get_claim(store.connection(), "legacy")
            .unwrap()
            .unwrap()
            .agent_identity_id
            .as_deref(),
        Some("legacy")
    );
}

#[test]
fn v40_required_inventory_admits_fresh_and_reopened_full_and_portable_stores() {
    for profile in [
        crate::db::StoreProfile::TachiFull,
        crate::db::StoreProfile::PortableKernel,
    ] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("inventory.db");
        let context = crate::db::DbOpenContext {
            intent: crate::db::OpenIntent::OpenExisting,
            migration: crate::db::MigrationAuthority::Deny,
            required_profile: profile.into(),
        };
        for _ in 0..2 {
            let store =
                crate::MemoryStore::open_with_context(path.to_str().unwrap(), &context).unwrap();
            let version: u32 = store
                .connection()
                .query_row("PRAGMA user_version", [], |row| row.get(0))
                .unwrap();
            // D7: each profile stamps its own head — Full reaches the Product
            // head; Portable carries the portable output stamp (39).
            let expected = if profile.includes_product() {
                crate::db::migrations::EXPECTED_SCHEMA_VERSION
            } else {
                crate::db::version_policy::portable_output_stamp(0)
            };
            assert_eq!(version, expected);
            let columns:i64 = store.connection().query_row("SELECT count(*) FROM pragma_table_xinfo('session_claims') WHERE name='canonical_project'", [], |row|row.get(0)).unwrap();
            assert_eq!(columns, i64::from(profile.includes_product()));
        }
    }
}
