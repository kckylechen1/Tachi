use crate::tool_params::{SaveMemoryParams, TachiDispatchParams};
use crate::MemoryServer;
use chrono::Utc;
use serde_json::json;

// ─── Kanban helpers ────────────────────────────────────────────────────────────

/// Initialize a kanban task entry in the memory DB
pub(super) async fn init_kanban_task(
    server: &MemoryServer,
    dispatch_id: &str,
    params: &TachiDispatchParams,
    plan_path: Option<&str>,
) -> Result<(), String> {
    let agent = params.agent.as_deref().unwrap_or("unknown");
    let text = format!(
        "Dispatch Task\nAgent: {}\nTask: {}\nPlan: {}",
        agent,
        params.task,
        plan_path.unwrap_or("inline"),
    );
    let metadata = json!({
        "type": "a2a_task",
        "dispatch_id": dispatch_id,
        "a2a_state": "TASK_STATE_WORKING",
        "agent": agent,
        "profile": params.profile,
        "tool_profile": params.tool_profile,
        "mcp_access": params.mcp_access,
        "allowed_mcp_servers": params.allowed_mcp_servers,
        "issue_ref": params.issue_ref,
        "pr_ref": params.pr_ref,
        "flow_id": params.flow_id,
        "auto_capability_bundle": params.auto_capability_bundle,
        "plan_file": plan_path,
        "eval_ledger_id": null,
    });

    crate::memory_search_ops::handle_save_memory(
        server,
        SaveMemoryParams {
            text,
            summary: format!(
                "Kanban: {} via {}",
                params.task.chars().take(80).collect::<String>(),
                agent
            ),
            path: format!("/kanban/tasks/{}", dispatch_id),
            importance: 0.7,
            category: "fact".to_string(),
            topic: "kanban".to_string(),
            keywords: vec![
                "kanban".to_string(),
                "dispatch".to_string(),
                agent.to_string(),
            ],
            persons: Vec::new(),
            entities: Vec::new(),
            location: String::new(),
            scope: "project".to_string(),
            vector: None,
            id: None,
            force: true,
            auto_link: true,
            project: None,
            retention_policy: Some(memcore::RetentionPolicy::Pinned.as_str().to_string()),
            domain: Some("system".to_string()),
            timestamp: None,
            valid_from: None,
            valid_until: None,
            metadata: Some(metadata),
            emit_continuity: false,
        },
    )
    .await?;
    Ok(())
}

pub(crate) async fn get_kanban_state(server: &MemoryServer, dispatch_id: &str) -> Option<String> {
    let path = format!("/kanban/tasks/{}", dispatch_id);
    // Use exact path SQL query instead of semantic search to avoid
    // Foundry inline-merge returning the wrong (merged) record.
    //
    // Scope resolution: `init_kanban_task` writes with scope="project", but
    // `handle_save_memory` falls back to global when no project DB exists.
    // We must mirror that fallback here so daemon/no-project dispatches don't
    // get stuck in TASK_STATE_WORKING.
    let entries = server
        .with_project_store(|store| {
            store
                .list_by_path(&path, 1, false)
                .map_err(|e| format!("kanban list_by_path: {e}"))
        })
        .unwrap_or_default();

    let entries = if entries.is_empty() {
        server
            .with_global_store(|store| {
                store
                    .list_by_path(&path, 1, false)
                    .map_err(|e| format!("kanban list_by_path (global): {e}"))
            })
            .unwrap_or_default()
    } else {
        entries
    };

    for entry in &entries {
        if let Some(state) = entry.metadata.get("a2a_state").and_then(|v| v.as_str()) {
            return Some(state.to_string());
        }
    }
    None
}

/// Update kanban task state.
///
/// `reviewed` flips the `metadata.reviewed` flag on the kanban row. The
/// status dashboard surfaces completed/success dispatches without this
/// flag as "unreviewed". Explicit `tachi_complete` calls should mark
/// the task reviewed; the watchdog auto-close path must NOT, so human
/// operators can still distinguish agent-closed tasks from auto-closed
/// ones.
pub(crate) async fn update_kanban_state(
    server: &MemoryServer,
    dispatch_id: &str,
    new_state: &str,
    eval_id: Option<&str>,
    reviewed: Option<bool>,
) -> Result<(), String> {
    let path = format!("/kanban/tasks/{}", dispatch_id);
    // Use exact path SQL query instead of semantic search to avoid
    // Foundry inline-merge returning the wrong (merged) record.
    //
    // Scope resolution: try project store first; fall back to global if no
    // project DB exists. We must write the update back to the same store the
    // entry was found in, otherwise we leave a stale row.
    let project_entries = server
        .with_project_store(|store| {
            store
                .list_by_path(&path, 1, false)
                .map_err(|e| format!("kanban list_by_path: {e}"))
        })
        .unwrap_or_default();

    let (entries, write_scope) = if project_entries.is_empty() {
        let global_entries = server
            .with_global_store(|store| {
                store
                    .list_by_path(&path, 1, false)
                    .map_err(|e| format!("kanban list_by_path (global): {e}"))
            })
            .unwrap_or_default();
        (global_entries, "global")
    } else {
        (project_entries, "project")
    };

    if let Some(entry) = entries.first() {
        let mut meta = entry.metadata.clone();
        if let Some(obj) = meta.as_object_mut() {
            obj.insert("a2a_state".to_string(), json!(new_state));
            if let Some(eid) = eval_id {
                obj.insert("eval_ledger_id".to_string(), json!(eid));
            }
            if let Some(flag) = reviewed {
                obj.insert("reviewed".to_string(), json!(flag));
            }
            obj.insert("updated_at".to_string(), json!(Utc::now().to_rfc3339()));
        }
        crate::memory_search_ops::handle_save_memory(
            server,
            SaveMemoryParams {
                text: entry.text.clone(),
                summary: format!("Kanban [{}]: {}", new_state, dispatch_id),
                path,
                importance: 0.7,
                category: "fact".to_string(),
                topic: "kanban".to_string(),
                keywords: vec!["kanban".to_string()],
                persons: Vec::new(),
                entities: Vec::new(),
                location: String::new(),
                scope: write_scope.to_string(),
                vector: None,
                id: Some(entry.id.clone()),
                force: true,
                auto_link: true,
                project: None,
                retention_policy: Some(memcore::RetentionPolicy::Pinned.as_str().to_string()),
                domain: Some("system".to_string()),
                timestamp: None,
                valid_from: None,
                valid_until: None,
                metadata: Some(meta),
                emit_continuity: false,
            },
        )
        .await?;
    }
    Ok(())
}

pub(crate) fn should_cleanup_run(exit_code: Option<i32>, kanban_state: Option<&str>) -> bool {
    exit_code == Some(0) && kanban_state == Some("TASK_STATE_COMPLETED")
}

/// #971 review-fix (F3): idempotent-safe close for the BOARD-FIRST kanban
/// row on an early exit out of `handle_tachi_dispatch`'s post-init section.
///
/// The row is seeded `TASK_STATE_WORKING` by `init_kanban_task` before any
/// of the fallible post-init stages (V2 plan stage, backend prep, credential
/// materialization, harness preflight, slot reservation) run. Some of those
/// stages (the plan stage's own failure/timeout branches) already close the
/// row to a terminal state themselves before propagating their `Err` — this
/// helper must NOT clobber that with a second, possibly-redundant write, so
/// it checks the current state first and only writes `TASK_STATE_FAILED`
/// if the row is still non-terminal (i.e. still WORKING/PENDING/etc.).
/// Best-effort: a kanban write failure here is logged, not escalated — the
/// caller is already on an error path and must propagate the original error,
/// not a secondary bookkeeping failure.
pub(crate) async fn close_kanban_row_on_early_exit(
    server: &MemoryServer,
    dispatch_id: &str,
    context: &str,
    project: Option<&str>,
) {
    // #773 Layer-2 ② (hole b): every post-init early exit is a terminal
    // dispatch state the agent never `tachi_complete`s. Record a canonical
    // outcome row here as the GENERIC catch (covers credential/slot/plan-stage
    // failures that have no more-specific writer). FIRST-WRITER-WINS on
    // dispatch_id means the specific classifiers upstream (backend/preflight,
    // which run before this closer) keep their precise class — this coarse
    // 'dispatch' class only lands when nothing else already recorded the row.
    // Written even when the kanban row is already terminal (plan-stage closed
    // it), since that path still produced no outcome row of its own.
    // `project` (#774 round 2): the dispatch's `TachiDispatchParams::project`,
    // so this coarse catch-all lands in the same DB the upstream classifiers
    // (and any later `tachi_complete`) would resolve to — see
    // `record_terminal_failure_outcome`'s scope-symmetry doc.
    crate::complete_ops::dispatch_outcome::record_terminal_failure_outcome(
        server,
        dispatch_id,
        "dispatch",
        None,
        project,
    );

    let is_terminal = matches!(
        get_kanban_state(server, dispatch_id).await.as_deref(),
        Some(
            "TASK_STATE_COMPLETED"
                | "TASK_STATE_FAILED"
                | "TASK_STATE_CANCELED"
                // #971 review-fix (F2, second pass): the plan-review early
                // response now writes TASK_STATE_INPUT_REQUIRED to the
                // kanban row (see `plan_stage.rs`), not
                // TASK_STATE_PENDING_REVIEW. That early return is a
                // legitimate non-failure exit out of
                // `handle_tachi_dispatch`'s post-init section, so this guard
                // must treat INPUT_REQUIRED as already-settled here too —
                // otherwise this early-exit closer would immediately stomp
                // the just-written INPUT_REQUIRED row to FAILED.
                | "TASK_STATE_INPUT_REQUIRED"
        )
    );
    if is_terminal {
        return;
    }
    if let Err(kanban_err) =
        update_kanban_state(server, dispatch_id, "TASK_STATE_FAILED", None, Some(false)).await
    {
        eprintln!(
            "[dispatch] failed to mark dispatch {} FAILED in kanban after early exit ({}): {}",
            dispatch_id, context, kanban_err
        );
    }
    // This attempted terminal transition has independent durable delivery and
    // lease cleanup obligations. A failed kanban projection must not leave a
    // receipt un-emitted or a hung backend's presence claim active.
    crate::claims_ops::emit_terminal_receipt(
        server,
        dispatch_id,
        "TASK_STATE_FAILED",
        "Dispatch ended during backend setup before execution.",
        None,
    );
    crate::claims_ops::release_claim_for_dispatch(server, dispatch_id, "early_backend_failure");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::claims_ops::{auto_register_or_heartbeat_claim, ClaimHookInput};
    use chrono::Utc;
    use memcore::{ClaimState, MemoryEntry, RetentionPolicy};
    use serde_json::json;

    /// Build a dispatch card mirroring how `init_kanban_task` writes them, so
    /// `get_kanban_state`/`update_kanban_state` find it via `list_by_path`.
    fn kanban_card(dispatch_id: &str, a2a_state: &str) -> MemoryEntry {
        MemoryEntry {
            id: format!("kanban-{dispatch_id}"),
            path: format!("/kanban/tasks/{dispatch_id}"),
            summary: format!("Kanban: {dispatch_id}"),
            text: "Dispatch Task".to_string(),
            importance: 0.7,
            timestamp: Utc::now().to_rfc3339(),
            valid_from: String::new(),
            valid_until: None,
            category: "fact".to_string(),
            topic: "kanban".to_string(),
            keywords: vec!["kanban".to_string()],
            persons: Vec::new(),
            entities: Vec::new(),
            location: String::new(),
            source: "test".to_string(),
            scope: "project".to_string(),
            archived: false,
            access_count: 0,
            last_access: None,
            revision: 1,
            vector: None,
            metadata: json!({
                "type": "a2a_task",
                "dispatch_id": dispatch_id,
                "a2a_state": a2a_state,
            }),
            retention_policy: Some(RetentionPolicy::Pinned.as_str().to_string()),
            domain: Some("system".to_string()),
            recall_count: 0,
            query_diversity: 0,
            tier: "raw".to_string(),
        }
    }

    /// Discrimination for the unconditional receipt+release invariant in
    /// `close_kanban_row_on_early_exit`: even when `update_kanban_state`
    /// genuinely fails (a real DB write fault induced via SQLite triggers),
    /// the post-attempt `emit_terminal_receipt` and
    /// `release_claim_for_dispatch` calls must still execute. If those two
    /// calls were ever moved inside the error-handler `if let Err` branch,
    /// or guarded by a success check, this test would fail: the receipt would
    /// be absent and the claim would remain `active`.
    #[tokio::test]
    async fn close_kanban_row_on_early_exit_emits_receipt_and_releases_claim_when_kanban_update_genuinely_fails(
    ) {
        let server = crate::tests::make_server();
        server.set_session_identity(Some("early-exit-seat".to_string()), None, None);

        let dispatch_id = "dispatch-early-exit-kanban-write-fault";

        // Seed a kanban row in WORKING (non-terminal) state. This mirrors
        // what `init_kanban_task` does before any fallible post-init stage.
        server
            .with_global_store(|store| {
                store
                    .upsert(&kanban_card(dispatch_id, "TASK_STATE_WORKING"))
                    .map_err(|e| e.to_string())
            })
            .expect("seed kanban WORKING row");

        // The row must read back as WORKING so `close_kanban_row_on_early_exit`
        // does NOT short-circuit on the is-terminal guard.
        assert_eq!(
            get_kanban_state(&server, dispatch_id).await.as_deref(),
            Some("TASK_STATE_WORKING"),
        );

        // Register a claim so `emit_terminal_receipt` resolves a recipient
        // and `release_claim_for_dispatch` has a live target.
        auto_register_or_heartbeat_claim(
            &server,
            &ClaimHookInput {
                issue_ref: None,
                flow_id: None,
                dispatch_id: Some(dispatch_id.to_string()),
                branch: None,
                declared_file_scope: None,
            },
        );
        let claim_before = server
            .with_global_store_read(|store| {
                memcore::get_claim_for_dispatch(store.connection(), dispatch_id)
                    .map_err(|e| e.to_string())
            })
            .expect("claim read")
            .expect("claim seeded");
        assert_eq!(claim_before.state, ClaimState::Active);

        // Induce a REAL `update_kanban_state` failure: block all future
        // writes to the `memories` table via BEFORE INSERT/UPDATE triggers.
        // The seeded row survives (written before the triggers), so reads
        // still work; but `update_kanban_state`'s internal `handle_save_memory`
        // upsert hits RAISE(ABORT) and returns Err. This is a genuine DB-layer
        // write fault, not a wrapper-that-calls-delegate mock.
        server
            .with_global_store(|store| {
                store
                    .connection_mut()
                    .execute_batch(
                        "CREATE TRIGGER test_block_mem_insert BEFORE INSERT ON memories \
                         BEGIN SELECT RAISE(ABORT, 'test-induced memories write failure'); END; \
                         CREATE TRIGGER test_block_mem_update BEFORE UPDATE ON memories \
                         BEGIN SELECT RAISE(ABORT, 'test-induced memories write failure'); END;",
                    )
                    .map_err(|e| e.to_string())
            })
            .expect("create write-blocking triggers");

        // Prove the trigger is live: a fresh upsert must now fail.
        let blocked = server.with_global_store(|store| {
            store
                .upsert(&kanban_card("probe-blocked", "TASK_STATE_WORKING"))
                .map_err(|e| e.to_string())
        });
        assert!(
            blocked.is_err(),
            "trigger must genuinely block memories writes, got: {blocked:?}"
        );

        // The action under test: `close_kanban_row_on_early_exit` attempts
        // `update_kanban_state` (which fails) and then must unconditionally
        // emit the receipt and release the claim.
        close_kanban_row_on_early_exit(&server, dispatch_id, "test early exit", None).await;

        // PROOF 1 — receipt was emitted despite the kanban write failure.
        let receipt = server
            .with_global_store_read(|store| {
                memcore::get_terminal_receipt(store.connection(), dispatch_id)
                    .map_err(|e| e.to_string())
            })
            .expect("receipt read");
        assert!(
            receipt.is_some(),
            "receipt must be emitted even when update_kanban_state genuinely fails"
        );
        assert_eq!(
            receipt.unwrap().terminal_state,
            "TASK_STATE_FAILED",
            "receipt must carry the FAILED state the closer attempted"
        );

        // PROOF 2 — claim was released despite the kanban write failure.
        let claim_after = server
            .with_global_store_read(|store| {
                memcore::get_claim_for_dispatch(store.connection(), dispatch_id)
                    .map_err(|e| e.to_string())
            })
            .expect("claim read after")
            .expect("claim row retained for audit");
        assert_eq!(
            claim_after.state,
            ClaimState::Released,
            "claim must be released even when update_kanban_state genuinely fails"
        );
    }
}
