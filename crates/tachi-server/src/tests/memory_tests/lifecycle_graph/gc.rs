use super::*;

fn make_two_store_server() -> (tempfile::TempDir, crate::MemoryServer) {
    let temp = tempfile::tempdir().expect("two-store GC tempdir");
    let global_db = temp.path().join("global/memory.db");
    let project_db = temp.path().join("project/memory.db");
    std::fs::create_dir_all(global_db.parent().expect("global parent"))
        .expect("create global parent");
    std::fs::create_dir_all(project_db.parent().expect("project parent"))
        .expect("create project parent");
    let server =
        crate::MemoryServer::new(global_db, Some(project_db)).expect("create two-store GC server");
    (temp, server)
}

fn stale_resolved_kanban(id: &str) -> memcore::MemoryEntry {
    let mut entry = make_entry(id);
    entry.path = format!("/kanban/{id}");
    entry.category = "kanban".to_string();
    entry.metadata = json!({ "status": "resolved" });
    entry.timestamp = (Utc::now() - chrono::Duration::days(31)).to_rfc3339();
    entry
}

fn response_keys(value: &Value) -> std::collections::BTreeSet<String> {
    value
        .as_object()
        .expect("GC scope result must be an object")
        .keys()
        .cloned()
        .collect()
}

fn expected_gc_keys(include_session_claims: bool) -> std::collections::BTreeSet<String> {
    // #1099: `handoff_memories_pruned` is gone — the dedicated handoff GC
    // branch (`gc_expired_handoff_memories`) was retired along with
    // handoff_ops's write path. See handoff_ops.rs's module doc for the
    // legacy-row data policy (retain read-only).
    let mut keys = [
        "access_history_pruned",
        "query_diversity_reconciled",
        "processed_events_pruned",
        "audit_log_pruned",
        "agent_known_state_pruned",
        "orphaned_access_history",
        "orphaned_agent_known_state",
        "kanban_cards_pruned",
        "foundry_jobs_pruned",
        "recall_impression_groups_pruned",
    ]
    .into_iter()
    .map(str::to_string)
    .collect::<std::collections::BTreeSet<_>>();
    if include_session_claims {
        keys.insert("session_claims_released_pruned".to_string());
        keys.insert("session_claims_active_orphaned".to_string());
    }
    keys
}

#[tokio::test]
async fn memory_gc_prunes_expired_resolved_kanban_cards() {
    std::env::set_var("KANBAN_CLASSIFY_ENABLED", "false");
    let server = make_server();

    let post = server
        .post_card(Parameters(PostCardParams {
            from_agent: "hapi".to_string(),
            to_agent: "iris".to_string(),
            title: "Old resolved card".to_string(),
            body: "Can be pruned".to_string(),
            priority: "medium".to_string(),
            card_type: "request".to_string(),
            thread_id: None,
            workspace_id: None,
            project_id: None,
            conversation_id: None,
            agent_session_id: None,
        }))
        .await
        .expect("post_card should succeed");
    let post_json: serde_json::Value =
        serde_json::from_str(&post).expect("post_card response should be JSON");
    let card_id = post_json["card_id"]
        .as_str()
        .expect("post_card should return card_id")
        .to_string();

    server
        .update_card(Parameters(UpdateCardParams {
            card_id: card_id.clone(),
            new_status: "resolved".to_string(),
            response_text: None,
        }))
        .await
        .expect("update_card should succeed");

    let stale_timestamp = (Utc::now() - chrono::Duration::days(31)).to_rfc3339();
    server
        .with_global_store(|store| {
            store
                .connection()
                .execute(
                    "UPDATE memories SET timestamp = ?1 WHERE id = ?2",
                    (&stale_timestamp, &card_id),
                )
                .map_err(|e| format!("stale kanban timestamp update failed: {e}"))?;
            Ok(())
        })
        .expect("failed to age kanban card");

    let gc = crate::memory_ops::handle_memory_gc(&server)
        .await
        .expect("memory_gc should succeed");
    let gc_json: serde_json::Value =
        serde_json::from_str(&gc).expect("memory_gc response should be JSON");
    assert_eq!(gc_json["global"]["kanban_cards_pruned"], json!(1));

    let remaining = server
        .with_global_store_read(|store| {
            store
                .get_with_options(&card_id, true)
                .map_err(|e| format!("failed to fetch kanban card after GC: {e}"))
        })
        .expect("kanban fetch after GC should succeed");
    assert!(
        remaining.is_none(),
        "expired resolved kanban card should be deleted"
    );
}

/// #1100 Slice A equivalence matrix: the shared GC participants must produce
/// the same exact scope shape for empty, global-only, project-only, and mixed
/// stale-kanban stores. Session-claim fields stay global-only.
#[tokio::test]
async fn memory_gc_preserves_two_store_scope_and_count_contracts() {
    for (case, seed_global, seed_project) in [
        ("empty", false, false),
        ("global_only", true, false),
        ("project_only", false, true),
        ("mixed", true, true),
    ] {
        let (_temp, server) = make_two_store_server();
        let global_id = format!("{case}-global-kanban");
        let project_id = format!("{case}-project-kanban");
        if seed_global {
            server
                .with_global_store(|store| {
                    store
                        .upsert(&stale_resolved_kanban(&global_id))
                        .map_err(|error| format!("seed global {case}: {error}"))
                })
                .expect("seed global stale kanban");
        }
        if seed_project {
            server
                .with_project_store(|store| {
                    store
                        .upsert(&stale_resolved_kanban(&project_id))
                        .map_err(|error| format!("seed project {case}: {error}"))
                })
                .expect("seed project stale kanban");
        }

        let output: Value = serde_json::from_str(
            &crate::memory_ops::handle_memory_gc(&server)
                .await
                .expect("two-store GC succeeds"),
        )
        .expect("two-store GC response JSON");
        assert_eq!(
            response_keys(&output),
            ["global".to_string(), "project".to_string()]
                .into_iter()
                .collect(),
            "root scope contract changed for {case}"
        );
        assert_eq!(response_keys(&output["global"]), expected_gc_keys(true));
        assert_eq!(response_keys(&output["project"]), expected_gc_keys(false));
        assert_eq!(
            output["global"]["kanban_cards_pruned"],
            json!(usize::from(seed_global)),
            "global kanban count changed for {case}"
        );
        assert_eq!(
            output["project"]["kanban_cards_pruned"],
            json!(usize::from(seed_project)),
            "project kanban count changed for {case}"
        );
        assert_eq!(output["global"]["session_claims_active_orphaned"], json!(0));
        assert_eq!(output["global"]["session_claims_released_pruned"], json!(0));

        let global_remaining = server
            .with_global_store_read(|store| {
                store
                    .get_with_options(&global_id, true)
                    .map_err(|error| format!("read global {case}: {error}"))
            })
            .expect("read global stale kanban after GC");
        let project_remaining = server
            .with_project_store_read(|store| {
                store
                    .get_with_options(&project_id, true)
                    .map_err(|error| format!("read project {case}: {error}"))
            })
            .expect("read project stale kanban after GC");
        assert!(global_remaining.is_none(), "global {case}");
        assert!(project_remaining.is_none(), "project {case}");
    }
}

#[tokio::test]
async fn memory_gc_reaps_session_claims_only_in_global_store() {
    let (_temp, server) = make_two_store_server();
    let stale = "2020-01-01T00:00:00.000Z";
    for (claim_id, is_global) in [("global-stale-claim", true), ("project-stale-claim", false)] {
        let seed = |store: &mut memcore::MemoryStore| {
            store
                .connection_mut()
                .execute(
                    "INSERT INTO session_claims \
                     (claim_id, branch, state, created_at, heartbeat_at) \
                     VALUES (?1, '', 'active', ?2, ?2)",
                    rusqlite::params![claim_id, stale],
                )
                .map_err(|error| format!("seed {claim_id}: {error}"))?;
            Ok(())
        };
        if is_global {
            server
                .with_global_store(seed)
                .expect("seed global stale session claim");
        } else {
            server
                .with_project_store(seed)
                .expect("seed project stale session claim");
        }
    }

    let output: Value = serde_json::from_str(
        &crate::memory_ops::handle_memory_gc(&server)
            .await
            .expect("two-store GC succeeds"),
    )
    .expect("two-store GC response JSON");
    assert_eq!(output["global"]["session_claims_active_orphaned"], json!(1));
    assert!(
        output["project"]
            .get("session_claims_active_orphaned")
            .is_none(),
        "project result must never report a global-only claim sweep"
    );

    let global_state: String = server
        .with_global_store_read(|store| {
            store
                .connection()
                .query_row(
                    "SELECT state FROM session_claims WHERE claim_id = 'global-stale-claim'",
                    [],
                    |row| row.get(0),
                )
                .map_err(|error| format!("read global claim: {error}"))
        })
        .expect("read global claim state");
    let project_state: String = server
        .with_project_store_read(|store| {
            store
                .connection()
                .query_row(
                    "SELECT state FROM session_claims WHERE claim_id = 'project-stale-claim'",
                    [],
                    |row| row.get(0),
                )
                .map_err(|error| format!("read project claim: {error}"))
        })
        .expect("read project claim state");
    assert_eq!(global_state, "orphaned");
    assert_eq!(project_state, "active");
}
