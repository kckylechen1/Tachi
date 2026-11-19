//! Daemon periodic sweep for memories missing vector embeddings.

use std::path::{Path, PathBuf};
use std::time::Duration;

use tokio::time::{interval, MissedTickBehavior};

use crate::manifest::Manifest;
use crate::vector_backfill::{self, SweepStoreRoute, VectorSweepStateUpdate};
use crate::MemoryServer;
use tachi_llm::LlmClient;

const DEFAULT_SWEEP_INTERVAL_SECS: u64 = 30 * 60;
const DEFAULT_SWEEP_BATCH_PER_DB: usize = 32;

fn sweep_interval_secs() -> u64 {
    std::env::var("TACHI_VECTOR_SWEEP_INTERVAL_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|&n| n >= 1)
        .unwrap_or(DEFAULT_SWEEP_INTERVAL_SECS)
}

fn sweep_batch_per_db() -> usize {
    std::env::var("TACHI_VECTOR_SWEEP_BATCH_SIZE")
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|&n| (1..=128).contains(&n))
        .unwrap_or(DEFAULT_SWEEP_BATCH_PER_DB)
}

fn skip_recall_cache() -> bool {
    !matches!(
        std::env::var("TACHI_VECTOR_SWEEP_INCLUDE_CACHE").ok().as_deref(),
        Some(v) if v == "1" || v.eq_ignore_ascii_case("true") || v.eq_ignore_ascii_case("yes")
    )
}

fn sweep_disabled() -> bool {
    matches!(
        std::env::var("TACHI_DISABLE_VECTOR_SWEEP").ok().as_deref(),
        Some(v) if v == "1" || v.eq_ignore_ascii_case("true") || v.eq_ignore_ascii_case("yes")
    )
}

#[cfg(test)]
pub(crate) fn collect_sweep_paths(
    manifest_path: &Path,
    global_db: &Path,
    project_db: Option<&Path>,
) -> Vec<PathBuf> {
    collect_sweep_paths_inner(manifest_path, global_db, project_db, true)
}

#[cfg(test)]
pub(crate) fn collect_own_sweep_paths(global_db: &Path, project_db: Option<&Path>) -> Vec<PathBuf> {
    collect_sweep_paths_inner(Path::new(""), global_db, project_db, false)
}

fn collect_sweep_paths_inner(
    manifest_path: &Path,
    global_db: &Path,
    project_db: Option<&Path>,
    include_manifest: bool,
) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    let mut seen = std::collections::HashSet::new();

    let mut push = |p: PathBuf| {
        if p.exists() {
            if let Ok(canon) = std::fs::canonicalize(&p) {
                if seen.insert(canon.clone()) {
                    paths.push(canon);
                }
            } else if seen.insert(p.clone()) {
                paths.push(p);
            }
        }
    };

    push(global_db.to_path_buf());
    if let Some(p) = project_db {
        push(p.to_path_buf());
    }

    if include_manifest {
        if let Ok(manifest) = Manifest::load(manifest_path) {
            for entry in manifest.dbs {
                if !entry.allow_write || entry.schema_kind != "tachi" {
                    continue;
                }
                match crate::path_utils::manifest_db_leaf_exists(&entry) {
                    Ok(true) => {}
                    Ok(false) => continue,
                    Err(error) => {
                        tracing::warn!(
                            path = %entry.path,
                            error = %error,
                            "vector sweep refused manifest DB"
                        );
                        continue;
                    }
                }
                push(PathBuf::from(entry.path));
            }
        }
    }

    paths
}

fn paths_match(left: &Path, right: &Path) -> bool {
    match (std::fs::canonicalize(left), std::fs::canonicalize(right)) {
        (Ok(a), Ok(b)) => a == b,
        _ => left == right,
    }
}

fn collect_sweep_targets(
    manifest_path: &Path,
    server: &MemoryServer,
    include_manifest: bool,
) -> Vec<(PathBuf, SweepStoreRoute)> {
    let global_db = server.global_db_path_buf();
    let project_db = server.project_db_path_buf();
    let paths = collect_sweep_paths_inner(
        manifest_path,
        &global_db,
        project_db.as_deref(),
        include_manifest,
    );
    paths
        .into_iter()
        .map(|path| {
            let route = if paths_match(&path, &global_db) {
                SweepStoreRoute::ServerGlobal(server.clone())
            } else {
                // Pin non-global sweeps (including the active project DB) by
                // physical path so a hot project switch cannot redirect an
                // in-flight sweep's writeback or state write onto another DB.
                SweepStoreRoute::ServerPath(server.clone(), path.clone())
            };
            (path, route)
        })
        .collect()
}

/// Run one lightweight sweep pass across `server`'s targets.
async fn run_vector_sweep_once_for_server(
    manifest_path: &Path,
    server: &MemoryServer,
    batch_per_db: usize,
    skip_cache: bool,
    include_manifest: bool,
) -> usize {
    let targets = collect_sweep_targets(manifest_path, server, include_manifest);
    run_vector_sweep_targets(targets, server.llm.as_ref(), batch_per_db, skip_cache).await
}

/// Background task: periodically embed missing vectors across manifest DBs.
pub struct VectorSweepScheduler {
    _cancel: tokio::sync::watch::Sender<()>,
}

impl VectorSweepScheduler {
    /// Uses the daemon's shared `MemoryServer` (`DbRuntime` + `LlmClient`) so
    /// background sweeps never open uncoordinated SQLite writer handles.
    pub fn start(manifest_path: PathBuf, server: MemoryServer) -> Self {
        Self::start_inner(manifest_path, server, true)
    }

    /// Start a sweep that only touches the daemon's own DBs.
    pub fn start_own_dbs(manifest_path: PathBuf, server: MemoryServer) -> Self {
        Self::start_inner(manifest_path, server, false)
    }

    fn start_inner(manifest_path: PathBuf, server: MemoryServer, include_manifest: bool) -> Self {
        let (cancel_tx, mut cancel_rx) = tokio::sync::watch::channel(());

        tokio::spawn(async move {
            if sweep_disabled() {
                let targets = collect_sweep_targets(&manifest_path, &server, include_manifest);
                record_disabled_sweep_state_with_routes(
                    targets,
                    sweep_interval_secs(),
                    skip_recall_cache(),
                    "TACHI_DISABLE_VECTOR_SWEEP",
                )
                .await;
                tracing::info!("[vector-sweep] disabled via TACHI_DISABLE_VECTOR_SWEEP");
                return;
            }

            let batch = sweep_batch_per_db();
            let skip_cache = skip_recall_cache();

            // Run once immediately so startup does not wait a full interval.
            run_vector_sweep_once_for_server(
                &manifest_path,
                &server,
                batch,
                skip_cache,
                include_manifest,
            )
            .await;

            let mut tick = interval(Duration::from_secs(sweep_interval_secs()));
            tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
            // Consume the immediate first tick so startup does not run two
            // back-to-back sweeps while early HTTP requests arrive.
            tick.tick().await;

            loop {
                tokio::select! {
                    _ = cancel_rx.changed() => break,
                    _ = tick.tick() => {
                        run_vector_sweep_once_for_server(
                            &manifest_path,
                            &server,
                            batch,
                            skip_cache,
                            include_manifest,
                        )
                        .await;
                    }
                }
            }
        });

        Self { _cancel: cancel_tx }
    }
}

#[cfg(test)]
async fn run_vector_sweep_paths(
    paths: Vec<PathBuf>,
    llm: &LlmClient,
    batch_per_db: usize,
    skip_cache: bool,
) -> usize {
    let targets = paths
        .into_iter()
        .map(|path| {
            let route = SweepStoreRoute::DirectPath(path.clone());
            (path, route)
        })
        .collect();
    run_vector_sweep_targets(targets, llm, batch_per_db, skip_cache).await
}

async fn run_vector_sweep_targets(
    targets: Vec<(PathBuf, SweepStoreRoute)>,
    llm: &LlmClient,
    batch_per_db: usize,
    skip_cache: bool,
) -> usize {
    let mut total_done = 0usize;
    for (path, route) in targets {
        match vector_backfill::sweep_vectors_with_route(&route, llm, batch_per_db, skip_cache).await
        {
            Ok(outcome) if outcome.attempted_count > 0 => {
                record_enabled_sweep_state_with_route(
                    &path,
                    &route,
                    skip_cache,
                    outcome.embedded_count,
                    outcome.attempted_count,
                    outcome.failed_count,
                    None,
                )
                .await;
                total_done += outcome.embedded_count;
                tracing::info!(
                    "[vector-sweep] {} embedded {}/{} (skipped={}, failed={})",
                    path.display(),
                    outcome.embedded_count,
                    outcome.attempted_count,
                    outcome.skipped_count,
                    outcome.failed_count
                );
            }
            Ok(outcome) => {
                record_enabled_sweep_state_with_route(
                    &path,
                    &route,
                    skip_cache,
                    outcome.embedded_count,
                    outcome.attempted_count,
                    outcome.failed_count,
                    None,
                )
                .await;
            }
            Err(e) => {
                record_enabled_sweep_state_with_route(
                    &path,
                    &route,
                    skip_cache,
                    e.embedded_count,
                    e.attempted_count,
                    e.failed_count,
                    Some(e.message.clone()),
                )
                .await;
                tracing::warn!("[vector-sweep] {} failed: {}", path.display(), e.message);
            }
        }
    }
    if total_done > 0 {
        tracing::info!("[vector-sweep] run complete, embedded {total_done} row(s)");
    }
    total_done
}

fn provider_error(error: &str) -> Option<String> {
    let lower = error.to_ascii_lowercase();
    (lower.contains("provider") || lower.contains("voyage") || lower.contains("embed"))
        .then(|| error.to_string())
}

fn build_enabled_sweep_state_update(
    skip_recall_cache: bool,
    done: usize,
    todo: usize,
    failed: usize,
    error: Option<String>,
) -> VectorSweepStateUpdate {
    let failed_count = if error.is_some() && failed == 0 {
        1
    } else {
        failed
    };
    let last_provider_error = error.as_deref().and_then(provider_error);
    let preserve_outcome = error.is_none() && todo == 0;
    VectorSweepStateUpdate {
        enabled: true,
        disabled_reason: None,
        skip_recall_cache,
        embedded_count: done,
        failed_count,
        last_error: error,
        last_provider_error,
        interval_secs: Some(sweep_interval_secs()),
        preserve_schedule: false,
        preserve_outcome,
    }
}

async fn record_enabled_sweep_state_with_route(
    path: &Path,
    route: &SweepStoreRoute,
    skip_recall_cache: bool,
    done: usize,
    todo: usize,
    failed: usize,
    error: Option<String>,
) {
    let update = build_enabled_sweep_state_update(skip_recall_cache, done, todo, failed, error);
    if let Err(err) = vector_backfill::record_vector_sweep_state_with_route(route, update).await {
        tracing::warn!(
            "[vector-sweep] {} state write failed: {err}",
            path.display()
        );
    }
}

#[cfg(test)]
fn record_enabled_sweep_state(
    path: &Path,
    skip_recall_cache: bool,
    done: usize,
    todo: usize,
    failed: usize,
    error: Option<String>,
) {
    let update = build_enabled_sweep_state_update(skip_recall_cache, done, todo, failed, error);
    if let Err(err) = vector_backfill::record_vector_sweep_state(path, update) {
        tracing::warn!(
            "[vector-sweep] {} state write failed: {err}",
            path.display()
        );
    }
}

fn build_disabled_sweep_state_update(
    interval_secs: u64,
    skip_recall_cache: bool,
    reason: &str,
) -> VectorSweepStateUpdate {
    VectorSweepStateUpdate {
        enabled: false,
        disabled_reason: Some(reason.to_string()),
        skip_recall_cache,
        embedded_count: 0,
        failed_count: 0,
        last_error: None,
        last_provider_error: None,
        interval_secs: Some(interval_secs),
        preserve_schedule: false,
        preserve_outcome: false,
    }
}

async fn record_disabled_sweep_state_with_routes(
    targets: Vec<(PathBuf, SweepStoreRoute)>,
    interval_secs: u64,
    skip_recall_cache: bool,
    reason: &str,
) {
    for (path, route) in targets {
        let update = build_disabled_sweep_state_update(interval_secs, skip_recall_cache, reason);
        if let Err(err) =
            vector_backfill::record_vector_sweep_state_with_route(&route, update).await
        {
            tracing::warn!(
                "[vector-sweep] {} disabled state write failed: {err}",
                path.display()
            );
        }
    }
}

#[cfg(test)]
fn record_disabled_sweep_state(
    paths: Vec<PathBuf>,
    interval_secs: u64,
    skip_recall_cache: bool,
    reason: &str,
) {
    for path in paths {
        let update = build_disabled_sweep_state_update(interval_secs, skip_recall_cache, reason);
        if let Err(err) = vector_backfill::record_vector_sweep_state(&path, update) {
            tracing::warn!(
                "[vector-sweep] {} disabled state write failed: {err}",
                path.display()
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::EnvRestore;
    use memcore::{MemoryEntry, MemoryStore};
    use serde_json::json;
    use std::fs;
    use tempfile::TempDir;

    fn insert_memory(store: &mut MemoryStore, id: &str, path: &str, text: &str) {
        let now = memcore::time_compat::rfc3339_compat(jiff::Timestamp::now());
        store
            .upsert(&MemoryEntry {
                id: id.to_string(),
                path: path.to_string(),
                summary: String::new(),
                text: text.to_string(),
                importance: 0.5,
                timestamp: now.clone(),
                valid_from: now,
                valid_until: None,
                category: "fact".to_string(),
                topic: "status".to_string(),
                keywords: Vec::new(),
                persons: Vec::new(),
                entities: Vec::new(),
                location: String::new(),
                source: "manual".to_string(),
                scope: "project".to_string(),
                archived: false,
                access_count: 0,
                scored_count: 0,
                last_access: None,
                last_use_at: None,
                revision: 1,
                metadata: json!({}),
                vector: None,
                retention_policy: None,
                domain: None,
                recall_count: 0,
                query_diversity: 0,
                tier: "raw".to_string(),
            })
            .expect("insert typed memory fixture");
    }

    #[test]
    fn collect_sweep_paths_includes_global_and_manifest_entries() {
        let tmp = TempDir::new().unwrap();
        let global = tmp.path().join("global.db");
        fs::write(&global, b"").unwrap();
        let project = tmp.path().join("project.db");
        fs::write(&project, b"").unwrap();
        let manifest_path = tmp.path().join("manifest.json");
        fs::write(
            &manifest_path,
            format!(
                r#"{{
  "schema_version": 1,
  "generated_at": "2026-01-01T00:00:00Z",
  "dbs": [
    {{
      "path": "{}",
      "role": "project",
      "owner": "test",
      "schema_kind": "tachi",
      "vec_enabled": true,
      "allow_write": true,
      "last_doctor_at": "",
      "last_classification": "healthy",
      "scope_hint": "project:test",
      "notes": ""
    }},
    {{
      "path": "{}",
      "role": "global",
      "owner": "test",
      "schema_kind": "tachi",
      "vec_enabled": true,
      "allow_write": false,
      "last_doctor_at": "",
      "last_classification": "healthy",
      "scope_hint": "global",
      "notes": ""
    }}
  ]
}}"#,
                project.display(),
                global.display()
            ),
        )
        .unwrap();

        let paths = collect_sweep_paths(&manifest_path, &global, None);
        assert_eq!(paths.len(), 2);
        assert!(paths.iter().any(|p| p.ends_with("global.db")));
        assert!(paths.iter().any(|p| p.ends_with("project.db")));
    }

    #[test]
    fn collect_own_sweep_paths_excludes_manifest_entries() {
        let tmp = TempDir::new().unwrap();
        let global = tmp.path().join("agent-global.db");
        fs::write(&global, b"").unwrap();
        let project = tmp.path().join("project.db");
        fs::write(&project, b"").unwrap();
        let unrelated = tmp.path().join("unrelated.db");
        fs::write(&unrelated, b"").unwrap();
        let manifest_path = tmp.path().join("manifest.json");
        fs::write(
            &manifest_path,
            format!(
                r#"{{
  "schema_version": 1,
  "generated_at": "2026-01-01T00:00:00Z",
  "dbs": [
    {{
      "path": "{}",
      "role": "project",
      "owner": "test",
      "schema_kind": "tachi",
      "vec_enabled": true,
      "allow_write": true,
      "last_doctor_at": "",
      "last_classification": "healthy",
      "scope_hint": "project:test",
      "notes": ""
    }}
  ]
}}"#,
                unrelated.display()
            ),
        )
        .unwrap();

        let manifest_paths = collect_sweep_paths(&manifest_path, &global, Some(&project));
        assert_eq!(manifest_paths.len(), 3);

        let own_paths = collect_own_sweep_paths(&global, Some(&project));
        assert_eq!(own_paths.len(), 2);
        assert!(own_paths.iter().any(|p| p.ends_with("agent-global.db")));
        assert!(own_paths.iter().any(|p| p.ends_with("project.db")));
        assert!(!own_paths.iter().any(|p| p.ends_with("unrelated.db")));
    }

    #[test]
    fn sweep_interval_allows_env_shortened_debounce_for_tests() {
        let _guard = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let saved = std::env::var_os("TACHI_VECTOR_SWEEP_INTERVAL_SECS");
        std::env::set_var("TACHI_VECTOR_SWEEP_INTERVAL_SECS", "1");
        assert_eq!(sweep_interval_secs(), 1);
        if let Some(value) = saved {
            std::env::set_var("TACHI_VECTOR_SWEEP_INTERVAL_SECS", value);
        } else {
            std::env::remove_var("TACHI_VECTOR_SWEEP_INTERVAL_SECS");
        }
    }

    #[test]
    fn disabled_sweep_records_state_for_owned_paths() {
        let tmp = TempDir::new().unwrap();
        let global = tmp.path().join("global.db");
        let project = tmp.path().join("project.db");
        memcore::MemoryStore::open(global.to_str().unwrap()).unwrap();
        memcore::MemoryStore::open(project.to_str().unwrap()).unwrap();

        record_disabled_sweep_state(
            vec![global.clone(), project.clone()],
            1800,
            true,
            "TACHI_DISABLE_VECTOR_SWEEP",
        );

        let global_state = crate::vector_backfill::read_vector_sweep_state_for_status(&global)
            .unwrap()
            .unwrap();
        let project_state = crate::vector_backfill::read_vector_sweep_state_for_status(&project)
            .unwrap()
            .unwrap();
        assert!(!global_state.enabled);
        assert!(!project_state.enabled);
        assert_eq!(
            global_state.disabled_reason.as_deref(),
            Some("TACHI_DISABLE_VECTOR_SWEEP")
        );
        assert_eq!(global_state.interval_secs, Some(1800));
        assert!(global_state.skip_recall_cache);
    }

    #[tokio::test]
    async fn daemon_sweep_records_partial_progress_when_later_batch_fails() {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("partial-failure.db");
        let mut store = MemoryStore::open(db_path.to_str().unwrap()).unwrap();
        for index in 0..33 {
            insert_memory(
                &mut store,
                &format!("partial-{index}"),
                "/facts/partial",
                &format!("body {index}"),
            );
        }
        drop(store);

        crate::vector_backfill::set_test_embed_batch_results(vec![
            Ok(crate::vector_backfill::VectorBatchWriteOutcome {
                written_count: 32,
                skipped_count: 0,
                failed_count: 0,
            }),
            Err("Voyage embed batch failed: provider 429".to_string()),
        ]);
        let llm = LlmClient::new().expect("llm client");
        let embedded = run_vector_sweep_paths(vec![db_path.clone()], &llm, 33, true).await;
        crate::vector_backfill::set_test_embed_batch_results(Vec::new());

        assert_eq!(embedded, 0, "failed sweep runs do not count as complete");
        let state = crate::vector_backfill::read_vector_sweep_state_for_status(&db_path)
            .expect("read state")
            .expect("state recorded");
        assert_eq!(state.embedded_count, 32);
        assert_eq!(state.failed_count, 1);
        assert_eq!(
            state.last_provider_error.as_deref(),
            Some("Voyage embed batch failed: provider 429")
        );
    }

    #[test]
    fn daemon_sweep_state_does_not_count_benign_skips_as_failures() {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("benign-skip-state.db");
        memcore::MemoryStore::open(db_path.to_str().unwrap()).unwrap();

        record_enabled_sweep_state(&db_path, true, 3, 5, 0, None);

        let state = crate::vector_backfill::read_vector_sweep_state_for_status(&db_path)
            .expect("read state")
            .expect("state recorded");
        assert_eq!(state.embedded_count, 3);
        assert_eq!(
            state.failed_count, 0,
            "benign revision-mismatch skips remain pending, not failed"
        );
        assert_eq!(state.last_error, None);
        assert_eq!(state.last_provider_error, None);
    }

    #[test]
    fn daemon_sweep_state_counts_real_row_failures_without_error() {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("row-failure-state.db");
        memcore::MemoryStore::open(db_path.to_str().unwrap()).unwrap();

        record_enabled_sweep_state(&db_path, true, 3, 5, 1, None);

        let state = crate::vector_backfill::read_vector_sweep_state_for_status(&db_path)
            .expect("read state")
            .expect("state recorded");
        assert_eq!(state.embedded_count, 3);
        assert_eq!(
            state.failed_count, 1,
            "real per-row failures are recorded even when the sweep completes"
        );
        assert_eq!(state.last_error, None);
        assert_eq!(state.last_provider_error, None);
    }

    #[tokio::test]
    async fn daemon_sweep_does_not_count_unattempted_pending_rows_as_failed() {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("below-threshold.db");
        let mut store = MemoryStore::open(db_path.to_str().unwrap()).unwrap();
        let dummy_vec = vec![0.0_f32; 1024];
        for index in 0..100 {
            let id = format!("threshold-{index}");
            insert_memory(&mut store, &id, "/facts/skipped", "body");
            if index < 99 {
                let mut entry = store.get(&id).unwrap().expect("typed fixture exists");
                entry.vector = Some(dummy_vec.clone());
                store.upsert(&entry).expect("write typed vector fixture");
            }
        }
        drop(store);

        crate::vector_backfill::record_vector_sweep_state(
            &db_path,
            crate::vector_backfill::VectorSweepStateUpdate {
                enabled: true,
                disabled_reason: None,
                skip_recall_cache: true,
                embedded_count: 12,
                failed_count: 2,
                last_error: Some("Voyage embed batch failed: provider 429".to_string()),
                last_provider_error: Some("Voyage embed batch failed: provider 429".to_string()),
                interval_secs: Some(1800),
                preserve_schedule: false,
                preserve_outcome: false,
            },
        )
        .expect("seed prior failure state");

        let _threshold = EnvRestore::set("TACHI_VECTOR_SWEEP_PENDING_THRESHOLD", "1");
        let llm = LlmClient::new().expect("llm client");
        let embedded = run_vector_sweep_paths(vec![db_path.clone()], &llm, 1, true).await;

        assert_eq!(embedded, 0);
        let state = crate::vector_backfill::read_vector_sweep_state_for_status(&db_path)
            .expect("read state")
            .expect("state recorded");
        assert_eq!(state.embedded_count, 12);
        assert_eq!(
            state.failed_count, 2,
            "no-op sweep must preserve prior failure counts"
        );
        assert_eq!(
            state.last_provider_error.as_deref(),
            Some("Voyage embed batch failed: provider 429"),
            "no-op sweep must preserve prior provider errors"
        );
    }

    /// #1096 Class 5 regression: daemon sweeps must route `global` and
    /// `project` DBs through `MemoryServer`'s `DbRuntime` instead of opening
    /// competing `MemoryStore::open` connections, so concurrent `tachi_agent_eval`
    /// writes never contend on SQLite locks (`SQLITE_BUSY`).
    ///
    /// Runs on a multi-thread runtime and inspects `DbRuntime`'s global
    /// contention receipts directly so reverting to uncoordinated
    /// `MemoryStore::open` fails immediately instead of passing silently.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn daemon_sweep_coordinates_with_global_store_without_sqlite_busy() {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path().join("home");
        let global_db = home.join("global").join("tachi-memory.db");
        let project_db = tmp
            .path()
            .join("repo")
            .join(".tachi")
            .join("tachi-memory.db");
        std::fs::create_dir_all(global_db.parent().unwrap()).unwrap();
        std::fs::create_dir_all(project_db.parent().unwrap()).unwrap();
        drop(MemoryStore::open(project_db.to_str().unwrap()).unwrap());

        let server = MemoryServer::new_with_home_for_test(
            global_db.clone(),
            Some(project_db.clone()),
            home.clone(),
        )
        .expect("isolated memory server");

        // Verify target classification routes global -> ServerGlobal and project -> ServerPath.
        let targets = collect_sweep_targets(&home.join("manifest.json"), &server, false);
        assert!(targets
            .iter()
            .any(|(_, route)| matches!(route, SweepStoreRoute::ServerGlobal(_))));
        assert!(targets.iter().any(|(path, route)| {
            paths_match(path, &project_db) && matches!(route, SweepStoreRoute::ServerPath(_, _))
        }));

        // Seed a vectorized memory and zero out `busy_timeout` on `global_store`
        // BEFORE enabling contention receipts so we count only sweep + eval ops.
        server
            .with_global_store(|store| {
                insert_memory(store, "sweep-seed-1", "/facts/sweep", "seed body");
                let dummy_vec = vec![0.1_f32; 1024];
                store
                    .update_enrichment_fields("sweep-seed-1", None, Some(&dummy_vec), None, None, 1)
                    .map_err(|e| e.to_string())?;
                store
                    .connection()
                    .busy_timeout(Duration::ZERO)
                    .map_err(|e| e.to_string())
            })
            .expect("seed global store");

        let recorder = server.db.enable_global_contention_receipts();
        let manifest_path = home.join("manifest.json");
        let sweep_server = server.clone();
        let sweep_manifest = manifest_path.clone();
        let sweep_task = tokio::spawn(async move {
            for _ in 0..10 {
                run_vector_sweep_once_for_server(&sweep_manifest, &sweep_server, 16, true, false)
                    .await;
            }
        });

        let eval_server = server.clone();
        let eval_task = tokio::spawn(async move {
            for idx in 0..10 {
                let child_id = format!("coord-eval-{idx}");
                let reg_raw = crate::agent_eval::handle_agent_eval(
                    &eval_server,
                    crate::tool_params::TachiAgentEvalParams {
                        action: "register".to_string(),
                        register: Some(crate::tool_params::MirrorEvalRegisterParams {
                            frozen_contract_ref: "kckylechen1/Tachi#1096".to_string(),
                            execution_origin: "host_native_subagent".to_string(),
                            lifecycle_owner: "host".to_string(),
                            harness: Some("test-harness".to_string()),
                            native_child_id: Some(child_id.clone()),
                            requested_profile: Some("review".to_string()),
                            requested_model: Some("openai/gpt-5".to_string()),
                            requested_agent: Some("worker".to_string()),
                            requested_task_type: None,
                            requested_role: None,
                        }),
                        ..Default::default()
                    },
                )
                .await
                .expect("register must not hit SQLITE_BUSY");
                let reg: serde_json::Value = serde_json::from_str(&reg_raw).unwrap();
                let run_id = reg["eval_run_id"].as_str().unwrap().to_string();
                crate::agent_eval::handle_agent_eval(
                    &eval_server,
                    crate::tool_params::TachiAgentEvalParams {
                        action: "adjudicate".to_string(),
                        adjudicate: Some(crate::tool_params::MirrorEvalAdjudicateParams {
                            eval_run_id: Some(run_id),
                            native_child_id: None,
                            event_key: None,
                            actor: "reviewer".to_string(),
                            verifier_model: Some("openai/gpt-5".to_string()),
                            usefulness: "useful".to_string(),
                            failure_mode: None,
                            first_review_findings: Vec::new(),
                            plan_delta: None,
                            next_prompt_delta: None,
                            evidence_usable: true,
                            used_in_final_claim: false,
                            human_override: false,
                            evidence_ref: "test:1096".to_string(),
                            rubric: None,
                        }),
                        ..Default::default()
                    },
                )
                .await
                .expect("adjudicate must not hit SQLITE_BUSY");
            }
        });

        let (sweep_res, eval_res) = tokio::join!(sweep_task, eval_task);
        sweep_res.expect("sweep task finished without panic");
        eval_res.expect("eval task finished without panic");

        let batch = recorder.drain();
        assert_eq!(batch.dropped_samples, 0);
        let global_reads = batch
            .receipts
            .iter()
            .filter(|r| r.action == "read" && r.resource == "global_read_pool" && r.completed)
            .count();
        let global_writes = batch
            .receipts
            .iter()
            .filter(|r| r.action == "write" && r.resource == "global_store" && r.completed)
            .count();
        assert_eq!(
            global_reads, 20,
            "10 sweep passes and 10 eval reads must read through DbRuntime's global_read_pool"
        );
        assert_eq!(
            global_writes, 30,
            "all 10 sweep hard_state writes plus 20 eval writes must complete via global_store"
        );

        let state = crate::vector_backfill::read_vector_sweep_state_for_status(&global_db)
            .expect("read sweep state")
            .expect("sweep state recorded via coordinated global store");
        assert!(state.enabled);
    }

    /// #1096 Class 5 / hot-switch safety: once a sweep selects physical
    /// project DB `A`, switching the server's active project to `B` before the
    /// sweep completes must keep `A`'s sweep pinned to `A`, never writing to `B`.
    #[tokio::test]
    async fn daemon_sweep_target_stays_pinned_after_hot_project_switch() {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path().join("home");
        let global_db = home.join("global").join("tachi-memory.db");
        let project_a = tmp
            .path()
            .join("repo-a")
            .join(".tachi")
            .join("tachi-memory.db");
        let project_b = tmp
            .path()
            .join("repo-b")
            .join(".tachi")
            .join("tachi-memory.db");
        std::fs::create_dir_all(global_db.parent().unwrap()).unwrap();
        std::fs::create_dir_all(project_a.parent().unwrap()).unwrap();
        std::fs::create_dir_all(project_b.parent().unwrap()).unwrap();
        drop(MemoryStore::open(project_a.to_str().unwrap()).unwrap());
        drop(MemoryStore::open(project_b.to_str().unwrap()).unwrap());

        let server = MemoryServer::new_with_home_for_test(
            global_db.clone(),
            Some(project_a.clone()),
            home.clone(),
        )
        .expect("isolated memory server");

        let manifest_path = home.join("manifest.json");
        let targets: Vec<_> = collect_sweep_targets(&manifest_path, &server, false)
            .into_iter()
            .filter(|(path, _)| paths_match(path, &project_a))
            .collect();
        assert_eq!(targets.len(), 1);

        // Hot-switch active bound project from A to B before running A's sweep.
        server
            .db
            .activate_project_db(project_b.clone())
            .expect("hot-switch to project B");

        let _ = run_vector_sweep_targets(targets, server.llm.as_ref(), 16, true).await;

        assert!(
            crate::vector_backfill::read_vector_sweep_state_for_status(&project_a)
                .expect("read A state")
                .is_some(),
            "in-flight sweep must record state on original project A"
        );
        assert!(
            crate::vector_backfill::read_vector_sweep_state_for_status(&project_b)
                .expect("read B state")
                .is_none(),
            "hot-switched project B must not receive A's sweep state"
        );
    }
}
