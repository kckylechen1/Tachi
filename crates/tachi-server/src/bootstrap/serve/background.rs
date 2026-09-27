use super::*;
use tokio_util::sync::CancellationToken;

pub(super) fn spawn_idle_connection_cleanup(
    server: &MemoryServer,
    shutdown: CancellationToken,
) -> tokio::task::JoinHandle<()> {
    let pool = server.pool.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(60));
        loop {
            tokio::select! {
                _ = shutdown.cancelled() => break,
                _ = interval.tick() => {
                    for key in pool.remove_idle_connections(Instant::now()) {
                        eprintln!("Idle cleanup: disconnecting '{}'", key);
                    }
                }
            }
        }
    })
}

/// Periodic WAL TRUNCATE checkpoint. SQLite's default PASSIVE auto-checkpoint
/// merges WAL frames into the DB but never shrinks the `-wal` file; a write
/// burst — or long-lived readers blocking truncation — lets it balloon
/// (observed: a 25 MB orphaned WAL on a busy agent DB, causing slow reads and
/// lock contention). Cadence via `TACHI_WAL_CHECKPOINT_SECS` (default 300s,
/// min 30s; set 0 to disable).
///
/// Also runs `PRAGMA optimize` (see `MemoryStore::run_optimize`) on the same
/// tick: both are cheap, best-effort, write-connection maintenance ops that
/// want a "quiet moment" cadence, so they share this timer rather than
/// running two near-identical interval loops. `maintain_named_projects` must
/// follow the daemon's manifest-background scope: a scoped/global-only daemon
/// must not attach DBs it does not own merely to perform maintenance.
pub(super) fn spawn_wal_checkpoint(
    server: &MemoryServer,
    maintain_named_projects: bool,
    shutdown: CancellationToken,
) -> tokio::task::JoinHandle<()> {
    let ckpt_secs = parse_env_u64("TACHI_WAL_CHECKPOINT_SECS").unwrap_or(300);
    if ckpt_secs == 0 {
        return tokio::spawn(async {});
    }
    if !maintain_named_projects {
        eprintln!("[wal] named-project maintenance disabled for scoped daemon");
    }
    let ckpt_secs = ckpt_secs.max(30);
    let ckpt_server = server.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(ckpt_secs));
        interval.tick().await; // consume the immediate first tick
        loop {
            tokio::select! {
                _ = shutdown.cancelled() => break,
                _ = interval.tick() => {
                    run_wal_checkpoint(&ckpt_server, maintain_named_projects);
                }
            }
        }
    })
}

/// Reap expired `hard_state` rows (e.g. capture-manifest staging TTLs, see
/// `memcore::db::reap_expired_state`) on one store. Best-effort like the
/// checkpoint/optimize calls above it, but unlike them a reap failure must
/// never be swallowed silently: it is always logged (even when it reaps
/// nothing worth reporting, the checkpoint/optimize calls stay silent on
/// success — a reap failure is not allowed the same silence).
fn reap_expired_hard_state(
    ckpt_server: &MemoryServer,
    label: &str,
    now_rfc3339: &str,
    run: impl FnOnce(&MemoryServer, &str) -> Result<usize, String>,
) {
    match run(ckpt_server, now_rfc3339) {
        Ok(removed) => {
            if removed > 0 {
                eprintln!("[state-reap] {label}: reaped {removed} expired hard_state row(s)");
            }
        }
        Err(e) => eprintln!("[state-reap] {label} reap failed: {e}"),
    }
}

/// Idempotent TTL backfill (#1342 follow-up) for `hard_state` namespaces that
/// historically wrote rows with no `expires_at` at all. Runs on one store,
/// right alongside the reap step above and on the same "quiet moment"
/// cadence. Like the reap step, a failure here is always logged (never
/// swallowed): `[state-ttl-backfill] ... failed` is always visible, even
/// though the backfill itself, like the reap, is best-effort.
fn backfill_hard_state_ttl(
    ckpt_server: &MemoryServer,
    label: &str,
    run: impl FnOnce(&MemoryServer) -> Result<usize, String>,
) {
    match run(ckpt_server) {
        Ok(backfilled) => {
            if backfilled > 0 {
                eprintln!(
                    "[state-ttl-backfill] {label}: stamped expires_at on {backfilled} \
                     pre-existing hard_state row(s)"
                );
            }
        }
        Err(e) => eprintln!("[state-ttl-backfill] {label} backfill failed: {e}"),
    }
}

/// The seven-namespace state-lifecycle-hygiene pass (#1342 follow-up): every
/// `hard_state` namespace that historically wrote rows with no `expires_at`
/// field gets backfilled here, once per store scope, idempotently (a row
/// that already carries `expires_at` — freshly written, or backfilled on an
/// earlier tick — is never touched twice; see
/// `memcore::db::backfill_missing_expires_at`'s own doc for the exact
/// "missing key" vs "explicit null" distinction).
///
/// Two namespaces in the #1342 packet are deliberately absent from this list:
/// `orchestrator` (STOPPED — see `orchestrator_ops.rs`'s doc comment on
/// `ORCHESTRATOR_NS` for why no terminal-write point is unambiguous there)
/// and `dispatch_signature_evidence` / `exec_env_private_target` (excluded by
/// design — see those namespaces' own doc comments in `signature_evidence.rs`
/// / `exec_env_ops.rs`).
fn backfill_hard_state_ttls(store: &memcore::MemoryStore) -> Result<usize, String> {
    let now = chrono::Utc::now();
    let ninety_days = (now + chrono::Duration::days(90)).to_rfc3339();
    let thirty_days = (now + chrono::Duration::days(30)).to_rfc3339();

    let mut total = 0usize;

    // Unconditional 90-day TTL: a build receipt/ticket/ticket-status row has
    // no "still open" concept — each is done being useful when written.
    for namespace in [
        crate::build_broker::RECEIPT_NS,
        crate::build_broker::ticket::TICKET_NS,
        crate::build_broker::ticket::STATUS_NS,
    ] {
        total += store
            .backfill_missing_expires_at(namespace, &ninety_days, None)
            .map_err(|e| format!("backfill {namespace}: {e}"))?;
    }

    // Terminal-only 30-day TTL keyed on `$.status`: only `applied`/`rejected`
    // proposal rows — `pending`/`approved` rows must stay TTL-less until
    // their own terminal write.
    for namespace in [
        // The lifecycle namespace moved into memcore and is `pub` there, so
        // this reads the constant instead of a literal — one less string to
        // drift.
        memcore::store::memory_lifecycle::LIFECYCLE_PROPOSAL_NS,
        // `recall_proposal_ops::RECALL_CONFIG_PROPOSAL_NS` is still
        // module-private (not reachable from here) — hardcoded literal, source
        // of truth:
        // crates/tachi-server/src/tune_ops/recall_proposal_ops.rs:12.
        "recall_config_proposals",
    ] {
        total += store
            .backfill_missing_expires_at(
                namespace,
                &thirty_days,
                Some(("$.status", &["applied", "rejected"])),
            )
            .map_err(|e| format!("backfill {namespace}: {e}"))?;
    }

    // Terminal-only 30-day TTL keyed on `$.cleanup_status` (not `$.status`):
    // a credential-materialization row that has not been cleaned yet must
    // never get a TTL.
    total += store
        .backfill_missing_expires_at(
            tachi_credential_profile::CREDENTIAL_MATERIALIZATION_NAMESPACE,
            &thirty_days,
            Some(("$.cleanup_status", &["cleaned"])),
        )
        .map_err(|e| {
            format!(
                "backfill {}: {e}",
                tachi_credential_profile::CREDENTIAL_MATERIALIZATION_NAMESPACE
            )
        })?;

    Ok(total)
}

fn run_wal_checkpoint(ckpt_server: &MemoryServer, maintain_named_projects: bool) {
    let now_rfc3339 = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    if let Err(e) = ckpt_server
        .with_global_store(|store| store.checkpoint_wal_truncate().map_err(|e| e.to_string()))
    {
        eprintln!("[wal] global checkpoint skipped: {e}");
    }
    if let Err(e) =
        ckpt_server.with_global_store(|store| store.run_optimize().map_err(|e| e.to_string()))
    {
        eprintln!("[optimize] global optimize skipped: {e}");
    }
    reap_expired_hard_state(ckpt_server, "global", &now_rfc3339, |server, now| {
        server.with_global_store(|store| store.reap_expired_state(now).map_err(|e| e.to_string()))
    });
    backfill_hard_state_ttl(ckpt_server, "global", |server| {
        server.with_global_store(|store| backfill_hard_state_ttls(store))
    });
    if ckpt_server.has_project_db() {
        if let Err(e) = ckpt_server
            .with_project_store(|store| store.checkpoint_wal_truncate().map_err(|e| e.to_string()))
        {
            eprintln!("[wal] project checkpoint skipped: {e}");
        }
        if let Err(e) =
            ckpt_server.with_project_store(|store| store.run_optimize().map_err(|e| e.to_string()))
        {
            eprintln!("[optimize] project optimize skipped: {e}");
        }
        reap_expired_hard_state(ckpt_server, "project", &now_rfc3339, |server, now| {
            server.with_project_store(|store| {
                store.reap_expired_state(now).map_err(|e| e.to_string())
            })
        });
        backfill_hard_state_ttl(ckpt_server, "project", |server| {
            server.with_project_store(|store| backfill_hard_state_ttls(store))
        });
    }
    if maintain_named_projects {
        for name in crate::path_utils::list_named_projects_in_home(&ckpt_server.tachi_home_dir()) {
            if let Err(e) = ckpt_server.with_named_project_store(&name, |store| {
                store.checkpoint_wal_truncate().map_err(|e| e.to_string())
            }) {
                eprintln!("[wal] named-project '{name}' checkpoint skipped: {e}");
            }
            if let Err(e) = ckpt_server.with_named_project_store(&name, |store| {
                store.run_optimize().map_err(|e| e.to_string())
            }) {
                eprintln!("[optimize] named-project '{name}' optimize skipped: {e}");
            }
            reap_expired_hard_state(
                ckpt_server,
                &format!("named-project '{name}'"),
                &now_rfc3339,
                |server, now| {
                    server.with_named_project_store(&name, |store| {
                        store.reap_expired_state(now).map_err(|e| e.to_string())
                    })
                },
            );
            backfill_hard_state_ttl(ckpt_server, &format!("named-project '{name}'"), |server| {
                server.with_named_project_store(&name, |store| backfill_hard_state_ttls(store))
            });
        }
    }
}

pub(super) fn spawn_background_gc(
    server: &MemoryServer,
    gc_enabled: bool,
    gc_initial_delay_secs: u64,
    gc_interval_secs: u64,
    shutdown: CancellationToken,
) -> tokio::task::JoinHandle<()> {
    if !gc_enabled {
        eprintln!("Background GC disabled");
        return tokio::spawn(async {});
    }
    eprintln!(
        "Background GC enabled (initial_delay={}s, interval={}s)",
        gc_initial_delay_secs, gc_interval_secs
    );
    let gc_server = server.clone();
    tokio::spawn(async move {
        tokio::select! {
            _ = shutdown.cancelled() => return,
            _ = tokio::time::sleep(Duration::from_secs(gc_initial_delay_secs)) => {}
        }
        let mut interval = tokio::time::interval(Duration::from_secs(gc_interval_secs));
        loop {
            tokio::select! {
                _ = shutdown.cancelled() => break,
                _ = interval.tick() => {
                    eprintln!("[gc] Running scheduled garbage collection...");
                    match gc_server.with_global_store(|store: &mut MemoryStore| {
                        let mut gc = store
                            .gc_tables(&memcore::GcConfig::default())
                            .map_err(|e| format!("{e}"))?;
                        let kanban_deleted =
                            gc_expired_kanban_cards(store, DEFAULT_KANBAN_GC_MAX_AGE_DAYS)?;
                        let foundry_deleted =
                            memcore::gc_foundry_jobs(store.connection(), 30).unwrap_or(0);
                        if let Some(object) = gc.as_object_mut() {
                            object.insert("kanban_cards_pruned".into(), json!(kanban_deleted));
                            object.insert("foundry_jobs_pruned".into(), json!(foundry_deleted));
                        }
                        let stale_days: u32 = std::env::var("MEMORY_GC_STALE_DAYS")
                            .ok()
                            .and_then(|v| v.parse().ok())
                            .unwrap_or(90);
                        match store.archive_stale_memories(stale_days) {
                            Ok(archived) => {
                                if archived > 0 {
                                    eprintln!("[gc] Archived {} stale memories", archived);
                                }
                                if let Some(object) = gc.as_object_mut() {
                                    object.insert("memories_archived".into(), json!(archived));
                                }
                            }
                            Err(e) => eprintln!("[gc] archive_stale_memories error: {}", e),
                        }
                        Ok(gc)
                    }) {
                        Ok(result) => eprintln!("[gc] Global DB: {}", result),
                        Err(e) => eprintln!("[gc] Global DB error: {}", e),
                    }
                    if gc_server.has_project_db() {
                        match gc_server.with_project_store(|store: &mut MemoryStore| {
                            let mut gc = store
                                .gc_tables(&memcore::GcConfig::default())
                                .map_err(|e| format!("{e}"))?;
                            let kanban_deleted =
                                gc_expired_kanban_cards(store, DEFAULT_KANBAN_GC_MAX_AGE_DAYS)?;
                            let foundry_deleted =
                                memcore::gc_foundry_jobs(store.connection(), 30).unwrap_or(0);
                            if let Some(object) = gc.as_object_mut() {
                                object.insert("kanban_cards_pruned".into(), json!(kanban_deleted));
                                object.insert("foundry_jobs_pruned".into(), json!(foundry_deleted));
                            }
                            let stale_days: u32 = std::env::var("MEMORY_GC_STALE_DAYS")
                                .ok()
                                .and_then(|v| v.parse().ok())
                                .unwrap_or(90);
                            match store.archive_stale_memories(stale_days) {
                                Ok(archived) => {
                                    if archived > 0 {
                                        eprintln!(
                                            "[gc] Archived {} stale memories (project)",
                                            archived
                                        );
                                    }
                                    if let Some(object) = gc.as_object_mut() {
                                        object.insert("memories_archived".into(), json!(archived));
                                    }
                                }
                                Err(e) => eprintln!("[gc] archive_stale_memories error: {}", e),
                            }
                            Ok(gc)
                        }) {
                            Ok(result) => eprintln!("[gc] Project DB: {}", result),
                            Err(e) => eprintln!("[gc] Project DB error: {}", e),
                        }
                    }
                }
            }
        }
    })
}

pub(super) fn run_startup_integrity_checks(
    server: &MemoryServer,
    check_project_db: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    // Integrity check on global DB
    server
        .with_global_store(|store| {
            match store.quick_check() {
                Ok(true) => eprintln!("Global database integrity: OK"),
                Ok(false) => eprintln!("WARNING: Global database may be corrupted!"),
                Err(e) => eprintln!("WARNING: Could not check global database integrity: {e}"),
            }
            Ok(())
        })
        .map_err(|e| format!("startup check: {e}"))?;

    // Integrity check on project DB
    if check_project_db {
        server
            .with_project_store(|store| {
                match store.quick_check() {
                    Ok(true) => eprintln!("Project database integrity: OK"),
                    Ok(false) => eprintln!("WARNING: Project database may be corrupted!"),
                    Err(e) => {
                        eprintln!("WARNING: Could not check project database integrity: {e}")
                    }
                }
                Ok(())
            })
            .map_err(|e| format!("startup check: {e}"))?;
    }
    Ok(())
}

pub(super) fn load_cached_hub_tools(server: &MemoryServer) {
    // Load cached proxy tools from Hub
    {
        let load_proxy_tools = |store: &mut MemoryStore| -> Result<(), String> {
            let mcp_caps = store
                .hub_list(Some("mcp"), true)
                .map_err(|e| format!("hub list: {e}"))?;
            for cap in mcp_caps {
                let def: serde_json::Value = match serde_json::from_str(&cap.definition) {
                    Ok(def) => def,
                    Err(e) => {
                        eprintln!(
                            "[startup] skip MCP '{}' due to invalid definition JSON: {e}",
                            cap.id
                        );
                        continue;
                    }
                };
                if let Some(tools_json) = def.get("discovered_tools") {
                    match serde_json::from_value::<Vec<rmcp::model::Tool>>(tools_json.clone()) {
                        Ok(tools) => {
                            let server_name = cap.id.strip_prefix("mcp:").unwrap_or(&cap.id);
                            let filtered_tools = filter_mcp_tools_by_permissions(&def, tools);
                            lock_or_recover(&server.tool_discovery.proxy_tools, "proxy_tools")
                                .insert(server_name.to_string(), filtered_tools);
                        }
                        Err(e) => {
                            eprintln!(
                                "[startup] skip cached tools for '{}' due to invalid tool payload: {e}",
                                cap.id
                            );
                        }
                    }
                }
            }
            Ok(())
        };
        if let Err(e) = server.with_global_store(load_proxy_tools) {
            eprintln!("[startup] failed loading global MCP proxy cache: {e}");
        }
        if server.has_project_db() {
            if let Err(e) = server.with_project_store(load_proxy_tools) {
                eprintln!("[startup] failed loading project MCP proxy cache: {e}");
            }
        }
    }
    {
        let load_skill_tools = |store: &mut MemoryStore| -> Result<(), String> {
            let skill_caps = store
                .hub_list(Some("skill"), true)
                .map_err(|e| format!("hub list: {e}"))?;
            for cap in skill_caps {
                if should_expose_skill_tool(&cap) {
                    if let Err(e) = server.register_skill_tool(&cap) {
                        eprintln!(
                            "[startup] failed to register skill tool for '{}': {}",
                            cap.id, e
                        );
                    }
                }
            }
            Ok(())
        };
        if let Err(e) = server.with_global_store(load_skill_tools) {
            eprintln!("[startup] failed loading global skill tools: {e}");
        }
        if server.has_project_db() {
            if let Err(e) = server.with_project_store(load_skill_tools) {
                eprintln!("[startup] failed loading project skill tools: {e}");
            }
        }
    }
}

const DEFAULT_DISTILL_INTERVAL_SECS: u64 = 86_400;

fn configured_distill_interval_secs() -> u64 {
    std::env::var("DISTILL_INTERVAL_SECS")
        .ok()
        .and_then(|value| value.parse().ok())
        .filter(|&value| value > 0)
        .unwrap_or(DEFAULT_DISTILL_INTERVAL_SECS)
}

pub(super) fn report_pipeline_and_spawn_daily_distill(
    server: &MemoryServer,
    app_home: &std::path::Path,
    shutdown: CancellationToken,
) -> tokio::task::JoinHandle<()> {
    if server.pipeline_enabled {
        eprintln!("Pipeline workers: ENABLED (external)");
    } else {
        eprintln!("Pipeline workers: DISABLED (set ENABLE_PIPELINE=true to enable)");
    }

    // #1605: refuse loudly or not at all. The gate carries its own reason, and
    // both branches log through the tracing sink so `tachi.log` records the
    // decision deterministically — the old silent no-spawn cost six days of
    // zero distillation on the global-only owner daemon.
    let (bound_project, named_projects) = match daily_distill_scheduler_gate(server) {
        DailyDistillGate::Enabled {
            bound_project,
            named_projects,
        } => (bound_project, named_projects),
        DailyDistillGate::Disabled { reason, remedy } => {
            tracing::warn!(
                target: "tachi::distill",
                reason = %reason,
                remedy = %remedy,
                "Distill scheduler: DISABLED"
            );
            return tokio::spawn(async {});
        }
    };

    {
        // Phase 1 daily batch distill. Default cadence is 24h; the legacy
        // per-capture `MemoryDistill` enqueue is gone, so this scheduler must
        // remain active even when external pipeline workers are disabled.
        let distill_interval_secs = configured_distill_interval_secs();

        // Logged at decision time, not after the 60s warmup: a daemon that
        // dies inside the warmup must still have said what it decided.
        tracing::info!(
            target: "tachi::distill",
            interval_secs = distill_interval_secs,
            bound_project,
            named_projects,
            "Distill scheduler: ENABLED (daily batch)"
        );

        let distill_server = server.clone();
        let marker_path = daily_distill_marker_path(app_home);
        tokio::spawn(async move {
            tokio::select! {
                _ = shutdown.cancelled() => return,
                _ = tokio::time::sleep(Duration::from_secs(60)) => {}
            }
            tracing::debug!(
                target: "tachi::distill",
                interval_secs = distill_interval_secs,
                "Distill scheduler: warmup complete, entering run loop"
            );

            let run_once = |server: &crate::MemoryServer, marker: &std::path::Path| {
                let server = server.clone();
                let marker = marker.to_path_buf();
                async move {
                    match crate::foundry_runtime_ops::run_daily_batch_distill(&server).await {
                        Ok(report) => {
                            eprintln!(
                                "[distill] daily batch: dispatched={} distilled={} skipped={} fallback={} errors={}",
                                report.batches_dispatched,
                                report.groups_distilled,
                                report.groups_skipped,
                                report.fallback_used,
                                report.errors.len()
                            );
                            if let Some(parent) = marker.parent() {
                                if let Err(err) = tokio::fs::create_dir_all(parent).await {
                                    eprintln!(
                                        "[distill] failed to create marker directory {}: {err}",
                                        parent.display()
                                    );
                                    return;
                                }
                            }
                            // Marker carries the batch quality summary (not just a
                            // timestamp) so `tachi_status` can surface distill
                            // degradation and so hard errors reach the health score.
                            // read_distill_marker stays backward-compatible with the
                            // legacy bare-timestamp format.
                            let marker_body = serde_json::json!({
                                "ts": chrono::Utc::now().to_rfc3339(),
                                "groups_distilled": report.groups_distilled,
                                "groups_skipped": report.groups_skipped,
                                "fallback_used": report.fallback_used,
                                "errors": report.errors.len(),
                            })
                            .to_string();
                            if let Err(err) = tokio::fs::write(&marker, marker_body).await {
                                eprintln!(
                                    "[distill] failed to write marker {}: {err}",
                                    marker.display()
                                );
                            }
                        }
                        Err(err) => {
                            eprintln!("[distill] daily batch error: {err}");
                            // Write a failure marker so `tachi_status` can surface
                            // the *reason* the marker is stale instead of a bare
                            // "stale" that repeats day after day with no owner.
                            // Same JSON shape as the success marker plus `error`;
                            // read_distill_marker is backward-compatible.
                            if let Some(parent) = marker.parent() {
                                if let Err(mk_err) = tokio::fs::create_dir_all(parent).await {
                                    eprintln!(
                                        "[distill] failed to create marker directory {}: {mk_err}",
                                        parent.display()
                                    );
                                    return;
                                }
                            }
                            let marker_body = serde_json::json!({
                                "ts": chrono::Utc::now().to_rfc3339(),
                                "error": err.to_string(),
                                "groups_distilled": 0,
                                "groups_skipped": 0,
                                "fallback_used": 0,
                                "errors": 0,
                            })
                            .to_string();
                            if let Err(write_err) = tokio::fs::write(&marker, marker_body).await {
                                eprintln!(
                                    "[distill] failed to write failure marker {}: {write_err}",
                                    marker.display()
                                );
                            }
                        }
                    }
                }
            };

            // Catch-up: if the marker is missing or older than the cadence,
            // run immediately after the 60s warmup.
            let should_run_now = match tokio::fs::metadata(&marker_path)
                .await
                .and_then(|m| m.modified())
            {
                Ok(modified) => modified
                    .elapsed()
                    .map(|d| d.as_secs() >= distill_interval_secs)
                    .unwrap_or(true),
                Err(_) => true,
            };
            if should_run_now {
                run_once(&distill_server, &marker_path).await;
            }

            let mut interval = tokio::time::interval(Duration::from_secs(distill_interval_secs));
            // First tick fires immediately; consume it so we wait a full cadence.
            interval.tick().await;
            loop {
                tokio::select! {
                    _ = shutdown.cancelled() => break,
                    _ = interval.tick() => {
                        run_once(&distill_server, &marker_path).await;
                    }
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::EnvRestore;

    fn make_server() -> (tempfile::TempDir, MemoryServer) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let global_db = tmp.path().join("global.db");
        let server = MemoryServer::new(global_db, None).expect("server");
        (tmp, server)
    }

    #[tokio::test]
    async fn idle_cleanup_exits_on_shutdown_cancel() {
        let (_tmp, server) = make_server();
        let shutdown = CancellationToken::new();
        let handle = spawn_idle_connection_cleanup(&server, shutdown.clone());
        shutdown.cancel();
        tokio::time::timeout(Duration::from_secs(2), handle)
            .await
            .expect("idle cleanup task did not exit within 2s after shutdown")
            .expect("task panicked");
    }

    #[tokio::test]
    async fn wal_checkpoint_exits_on_shutdown_cancel() {
        let (_tmp, server) = make_server();
        let shutdown = CancellationToken::new();
        let handle = spawn_wal_checkpoint(&server, true, shutdown.clone());
        shutdown.cancel();
        tokio::time::timeout(Duration::from_secs(2), handle)
            .await
            .expect("wal checkpoint task did not exit within 2s after shutdown")
            .expect("task panicked");
    }

    #[test]
    fn wal_checkpoint_scoped_to_active_dbs_does_not_attach_named_projects() {
        let _guard = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().expect("tempdir");
        let tachi_home = tmp.path().join("home");
        let _tachi_home = EnvRestore::set_path("TACHI_HOME", &tachi_home);

        // Built as an unlabelled store, NOT via `MemoryServer::new` on this
        // path: that constructor confers the role `"global"` on whatever file
        // it is handed, and since tachi#1579 the conferral is a write-once
        // stamp inside the file. A named-project DB stamped `"global"` refuses
        // every later open that correctly claims `"foreign"`. Production never
        // creates a named-project store that way — `create_named_project_db`
        // is the fixture that matches how one really comes into existence.
        let foreign_db = crate::tests::create_named_project_db(&tachi_home, "foreign");

        let server = MemoryServer::new(tachi_home.join("global.db"), None).expect("server");
        run_wal_checkpoint(&server, false);

        assert!(
            server
                .db
                .attached_project_dbs
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .is_empty(),
            "active-DB-only maintenance must not open or cache named-project DBs"
        );

        run_wal_checkpoint(&server, true);
        let attached = server
            .db
            .attached_project_dbs
            .read()
            .unwrap_or_else(|e| e.into_inner());
        assert_eq!(attached.len(), 1);
        assert!(
            attached
                .contains_key(&std::fs::canonicalize(foreign_db).expect("canonical foreign DB")),
            "manifest-wide maintenance must keep maintaining named-project DBs"
        );
    }

    #[test]
    fn background_checkpoint_enumerates_server_home_after_environment_drift() {
        let _guard = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let server = crate::tests::make_server();
        let fixture_db =
            crate::tests::create_named_project_db(&server.tachi_home_dir(), "fixture-checkpoint");
        let ambient_home = tempfile::tempdir().expect("ambient home");
        let ambient_db =
            crate::tests::create_named_project_db(ambient_home.path(), "ambient-checkpoint");
        let _ambient_home = EnvRestore::set_path("TACHI_HOME", ambient_home.path());

        run_wal_checkpoint(&server, true);

        let attached = server
            .db
            .attached_project_dbs
            .read()
            .unwrap_or_else(|error| error.into_inner());
        assert!(attached.contains_key(&std::fs::canonicalize(fixture_db).expect("fixture DB")));
        assert!(!attached.contains_key(&std::fs::canonicalize(ambient_db).expect("ambient DB")));
    }

    #[test]
    fn wal_checkpoint_reaps_expired_hard_state_on_global_and_project_scope() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let global_db = tmp.path().join("global.db");
        let project_db = tmp.path().join("project.db");
        let server =
            MemoryServer::new(global_db, Some(project_db)).expect("server with project db");

        server
            .with_global_store(|store| {
                store
                    .set_state(
                        "capture_manifest",
                        "expired-global",
                        r#"{"expires_at":"2000-01-01T00:00:00Z"}"#,
                    )
                    .map_err(|e| e.to_string())
            })
            .expect("seed expired global hard_state row");
        server
            .with_global_store(|store| {
                store
                    .set_state(
                        "capture_manifest",
                        "not-yet-expired-global",
                        r#"{"expires_at":"2999-01-01T00:00:00Z"}"#,
                    )
                    .map_err(|e| e.to_string())
            })
            .expect("seed not-yet-expired global hard_state row");
        server
            .with_project_store(|store| {
                store
                    .set_state(
                        "capture_manifest",
                        "expired-project",
                        r#"{"expires_at":"2000-01-01T00:00:00Z"}"#,
                    )
                    .map_err(|e| e.to_string())
            })
            .expect("seed expired project hard_state row");

        run_wal_checkpoint(&server, false);

        let global_expired = server
            .with_global_store(|store| {
                store
                    .get_state_kv("capture_manifest", "expired-global")
                    .map_err(|e| e.to_string())
            })
            .expect("read global expired state");
        assert!(
            global_expired.is_none(),
            "expired global hard_state row must be reaped by wal-checkpoint maintenance"
        );

        let global_future = server
            .with_global_store(|store| {
                store
                    .get_state_kv("capture_manifest", "not-yet-expired-global")
                    .map_err(|e| e.to_string())
            })
            .expect("read global not-yet-expired state");
        assert!(
            global_future.is_some(),
            "not-yet-expired global hard_state row must survive"
        );

        let project_expired = server
            .with_project_store(|store| {
                store
                    .get_state_kv("capture_manifest", "expired-project")
                    .map_err(|e| e.to_string())
            })
            .expect("read project expired state");
        assert!(
            project_expired.is_none(),
            "expired project hard_state row must be reaped by wal-checkpoint maintenance \
             (own-project scope, no maintain_named_projects gate)"
        );
    }

    /// #1342 follow-up: `run_wal_checkpoint` must also backfill the
    /// seven-namespace TTL pass, idempotently, on both global and project
    /// scope — this is the wiring test; the per-namespace "what counts as
    /// terminal" logic is unit-tested at its own write points.
    #[test]
    fn wal_checkpoint_backfills_missing_expires_at_idempotently() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let global_db = tmp.path().join("global.db");
        let project_db = tmp.path().join("project.db");
        let server =
            MemoryServer::new(global_db, Some(project_db)).expect("server with project db");

        // Unconditional-TTL namespace (build_receipt): a pre-#1342 row with
        // no expires_at key at all.
        server
            .with_global_store(|store| {
                store
                    .set_state("build_receipt", "t-legacy", r#"{"outcome":"success"}"#)
                    .map_err(|e| e.to_string())
            })
            .expect("seed legacy global build_receipt row");
        // Terminal-scoped namespace (memory_lifecycle_proposals): an applied
        // (terminal) row with no expires_at, and a still-pending row that
        // must NEVER get one.
        server
            .with_project_store(|store| {
                store
                    .set_state(
                        "memory_lifecycle_proposals",
                        "p-legacy-applied",
                        r#"{"status":"applied"}"#,
                    )
                    .map_err(|e| e.to_string())?;
                store
                    .set_state(
                        "memory_lifecycle_proposals",
                        "p-legacy-pending",
                        r#"{"status":"pending"}"#,
                    )
                    .map_err(|e| e.to_string())
            })
            .expect("seed project lifecycle proposal rows");

        run_wal_checkpoint(&server, false);

        let receipt = server
            .with_global_store(|store| {
                store
                    .get_state_kv("build_receipt", "t-legacy")
                    .map_err(|e| e.to_string())
            })
            .expect("read backfilled receipt")
            .expect("receipt row still present");
        assert!(
            receipt.0.contains("expires_at"),
            "a legacy build_receipt row must be backfilled with an expires_at: {}",
            receipt.0
        );

        let applied = server
            .with_project_store(|store| {
                store
                    .get_state_kv("memory_lifecycle_proposals", "p-legacy-applied")
                    .map_err(|e| e.to_string())
            })
            .expect("read backfilled applied proposal")
            .expect("applied proposal row still present");
        assert!(
            applied.0.contains("expires_at"),
            "a legacy applied proposal row must be backfilled with an expires_at: {}",
            applied.0
        );

        let pending = server
            .with_project_store(|store| {
                store
                    .get_state_kv("memory_lifecycle_proposals", "p-legacy-pending")
                    .map_err(|e| e.to_string())
            })
            .expect("read pending proposal")
            .expect("pending proposal row still present");
        assert!(
            !pending.0.contains("expires_at"),
            "a still-pending proposal row must NEVER be backfilled with a TTL: {}",
            pending.0
        );

        // Idempotent: a second checkpoint tick must not error or double-stamp.
        run_wal_checkpoint(&server, false);
        let receipt_again = server
            .with_global_store(|store| {
                store
                    .get_state_kv("build_receipt", "t-legacy")
                    .map_err(|e| e.to_string())
            })
            .expect("read receipt after second checkpoint")
            .expect("receipt row still present");
        assert_eq!(
            receipt.0, receipt_again.0,
            "a second backfill pass over already-backfilled rows must be a no-op"
        );
    }

    #[test]
    fn wal_checkpoint_reaps_named_project_hard_state_only_when_maintenance_enabled() {
        let _guard = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().expect("tempdir");
        let tachi_home = tmp.path().join("home");
        let _tachi_home = EnvRestore::set_path("TACHI_HOME", &tachi_home);

        // Unlabelled seed open, for the same reason as
        // `wal_checkpoint_scoped_to_active_dbs_does_not_attach_named_projects`:
        // seeding through a `MemoryServer`'s *global* store would stamp this
        // named-project file `"global"` (tachi#1579) and every
        // `with_named_project_store("foreign", …)` below would then be refused
        // with `StoreRoleConflict` before it could read anything.
        let foreign_db = crate::tests::create_named_project_db(&tachi_home, "foreign");
        memcore::MemoryStore::open(foreign_db.to_str().expect("utf8 foreign named-project DB"))
            .expect("open foreign named-project DB")
            .set_state(
                "capture_manifest",
                "expired-named",
                r#"{"expires_at":"2000-01-01T00:00:00Z"}"#,
            )
            .expect("seed expired named-project hard_state row");

        let server = MemoryServer::new(tachi_home.join("global.db"), None).expect("server");

        run_wal_checkpoint(&server, false);
        let row_after_disabled = server
            .with_named_project_store("foreign", |store| {
                store
                    .get_state_kv("capture_manifest", "expired-named")
                    .map_err(|e| e.to_string())
            })
            .expect("read named-project state after disabled maintenance");
        assert!(
            row_after_disabled.is_some(),
            "named-project hard_state reap must not run when maintain_named_projects=false, \
             mirroring the checkpoint/optimize scope gate"
        );

        run_wal_checkpoint(&server, true);
        let row_after_enabled = server
            .with_named_project_store("foreign", |store| {
                store
                    .get_state_kv("capture_manifest", "expired-named")
                    .map_err(|e| e.to_string())
            })
            .expect("read named-project state after enabled maintenance");
        assert!(
            row_after_enabled.is_none(),
            "named-project hard_state reap must run when maintain_named_projects=true"
        );
    }

    #[tokio::test]
    async fn background_gc_exits_on_shutdown_cancel_during_initial_delay() {
        let (_tmp, server) = make_server();
        let shutdown = CancellationToken::new();
        let handle = spawn_background_gc(&server, true, 3600, 3600, shutdown.clone());
        shutdown.cancel();
        tokio::time::timeout(Duration::from_secs(2), handle)
            .await
            .expect("background gc task did not exit within 2s after shutdown")
            .expect("task panicked");
    }

    #[tokio::test]
    async fn background_gc_returns_noop_handle_when_disabled() {
        let (_tmp, server) = make_server();
        let shutdown = CancellationToken::new();
        let handle = spawn_background_gc(&server, false, 0, 1, shutdown);
        tokio::time::timeout(Duration::from_secs(2), handle)
            .await
            .expect("disabled gc handle should complete immediately")
            .expect("task panicked");
    }

    /// Hermetic server whose Tachi home is `home`, so the #1605 distill gate's
    /// named-project scan sees only what the test put there.
    fn server_with_home(home: &std::path::Path) -> MemoryServer {
        MemoryServer::new_with_home_for_test(home.join("global.db"), None, home.to_path_buf())
            .expect("server")
    }

    #[test]
    fn zero_distill_interval_uses_daily_default() {
        let _guard = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let _interval = EnvRestore::set("DISTILL_INTERVAL_SECS", "0");
        assert_eq!(
            configured_distill_interval_secs(),
            DEFAULT_DISTILL_INTERVAL_SECS
        );
    }

    /// #1605 discriminating test: a global-store-only daemon that still has a
    /// manifest-attached named project MUST spawn the scheduler. The enabled
    /// task parks in its 60s warmup, so "still running after 250ms" separates
    /// it from the disabled branch's immediately-completing no-op handle.
    #[tokio::test]
    async fn daily_distill_spawns_for_global_only_daemon_with_named_project() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home = tmp.path();
        let project_dir = home.join("projects").join("Sigil");
        std::fs::create_dir_all(&project_dir).expect("named project dir");
        std::fs::write(project_dir.join(memcore::MEMORY_DB_FILENAME), b"")
            .expect("named project db file");

        let server = server_with_home(home);
        assert!(
            !server.has_project_db(),
            "test precondition: global-store-only daemon"
        );

        let shutdown = CancellationToken::new();
        let mut handle = report_pipeline_and_spawn_daily_distill(&server, home, shutdown.clone());
        assert!(
            tokio::time::timeout(Duration::from_millis(250), &mut handle)
                .await
                .is_err(),
            "scheduler must be live (parked in warmup), not a silent no-op handle"
        );

        shutdown.cancel();
        tokio::time::timeout(Duration::from_secs(2), handle)
            .await
            .expect("distill scheduler did not exit within 2s after shutdown")
            .expect("task panicked");
    }

    /// The genuinely-nothing-to-distill home still returns the no-op handle —
    /// paired with `serve.rs`'s gate test, which pins the `DISABLED` reason and
    /// remedy text that this branch hands to `tracing::warn!`.
    #[tokio::test]
    async fn daily_distill_returns_noop_handle_when_no_store_is_distillable() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home = tmp.path();
        let server = server_with_home(home);

        let shutdown = CancellationToken::new();
        let handle = report_pipeline_and_spawn_daily_distill(&server, home, shutdown);
        tokio::time::timeout(Duration::from_secs(2), handle)
            .await
            .expect("disabled distill handle should complete immediately")
            .expect("task panicked");
    }
}
