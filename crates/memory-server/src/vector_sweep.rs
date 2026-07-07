//! Daemon periodic sweep for memories missing vector embeddings.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use tokio::time::{interval, MissedTickBehavior};

use crate::manifest::Manifest;
use crate::vector_backfill::{self, VectorSweepStateUpdate};
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
                push(PathBuf::from(entry.path));
            }
        }
    }

    paths
}

/// Run one lightweight sweep pass across manifest DBs. Testable entry point used
/// by the daemon scheduler on startup and on each interval tick.
pub(crate) async fn run_vector_sweep_once(
    manifest_path: &Path,
    global_db: &Path,
    project_db: Option<&Path>,
    llm: &LlmClient,
    batch_per_db: usize,
    skip_cache: bool,
) -> usize {
    let paths = collect_sweep_paths(manifest_path, global_db, project_db);
    run_vector_sweep_paths(paths, llm, batch_per_db, skip_cache).await
}

/// Background task: periodically embed missing vectors across manifest DBs.
pub struct VectorSweepScheduler {
    _cancel: tokio::sync::watch::Sender<()>,
}

impl VectorSweepScheduler {
    /// Uses the daemon's shared `LlmClient` (same provider secrets as MCP/enrichment).
    pub fn start(
        manifest_path: PathBuf,
        global_db: PathBuf,
        project_db: Option<PathBuf>,
        llm: Arc<LlmClient>,
    ) -> Self {
        Self::start_inner(manifest_path, global_db, project_db, llm, true)
    }

    /// Start a sweep that only touches the daemon's own DBs.
    pub fn start_own_dbs(
        manifest_path: PathBuf,
        global_db: PathBuf,
        project_db: Option<PathBuf>,
        llm: Arc<LlmClient>,
    ) -> Self {
        Self::start_inner(manifest_path, global_db, project_db, llm, false)
    }

    fn start_inner(
        manifest_path: PathBuf,
        global_db: PathBuf,
        project_db: Option<PathBuf>,
        llm: Arc<LlmClient>,
        include_manifest: bool,
    ) -> Self {
        let (cancel_tx, mut cancel_rx) = tokio::sync::watch::channel(());

        tokio::spawn(async move {
            if sweep_disabled() {
                let paths = collect_sweep_paths_inner(
                    &manifest_path,
                    &global_db,
                    project_db.as_deref(),
                    include_manifest,
                );
                record_disabled_sweep_state(
                    paths,
                    sweep_interval_secs(),
                    skip_recall_cache(),
                    "TACHI_DISABLE_VECTOR_SWEEP",
                );
                tracing::info!("[vector-sweep] disabled via TACHI_DISABLE_VECTOR_SWEEP");
                return;
            }

            let batch = sweep_batch_per_db();
            let skip_cache = skip_recall_cache();
            let manifest_ref = manifest_path.as_path();
            let global_ref = global_db.as_path();
            let project_ref = project_db.as_deref();

            // Run once immediately so startup does not wait a full interval.
            if include_manifest {
                run_vector_sweep_once(
                    manifest_ref,
                    global_ref,
                    project_ref,
                    llm.as_ref(),
                    batch,
                    skip_cache,
                )
                .await;
            } else {
                run_vector_sweep_once_with_scope(
                    manifest_ref,
                    global_ref,
                    project_ref,
                    llm.as_ref(),
                    batch,
                    skip_cache,
                    false,
                )
                .await;
            }

            let mut tick = interval(Duration::from_secs(sweep_interval_secs()));
            tick.set_missed_tick_behavior(MissedTickBehavior::Skip);

            loop {
                tokio::select! {
                    _ = cancel_rx.changed() => break,
                    _ = tick.tick() => {
                        if include_manifest {
                            run_vector_sweep_once(
                                manifest_ref,
                                global_ref,
                                project_ref,
                                llm.as_ref(),
                                batch,
                                skip_cache,
                            ).await;
                        } else {
                            run_vector_sweep_once_with_scope(
                                manifest_ref,
                                global_ref,
                                project_ref,
                                llm.as_ref(),
                                batch,
                                skip_cache,
                                false,
                            ).await;
                        }
                    }
                }
            }
        });

        Self { _cancel: cancel_tx }
    }
}

async fn run_vector_sweep_once_with_scope(
    manifest_path: &Path,
    global_db: &Path,
    project_db: Option<&Path>,
    llm: &LlmClient,
    batch_per_db: usize,
    skip_cache: bool,
    include_manifest: bool,
) -> usize {
    let paths = collect_sweep_paths_inner(manifest_path, global_db, project_db, include_manifest);
    run_vector_sweep_paths(paths, llm, batch_per_db, skip_cache).await
}

async fn run_vector_sweep_paths(
    paths: Vec<PathBuf>,
    llm: &LlmClient,
    batch_per_db: usize,
    skip_cache: bool,
) -> usize {
    let mut total_done = 0usize;
    for path in paths {
        match vector_backfill::sweep_db_vectors(&path, llm, batch_per_db, skip_cache).await {
            Ok(outcome) if outcome.attempted_count > 0 => {
                record_enabled_sweep_state(
                    &path,
                    skip_cache,
                    outcome.embedded_count,
                    outcome.attempted_count,
                    None,
                );
                total_done += outcome.embedded_count;
                tracing::info!(
                    "[vector-sweep] {} embedded {}/{}",
                    path.display(),
                    outcome.embedded_count,
                    outcome.attempted_count
                );
            }
            Ok(outcome) => {
                record_enabled_sweep_state(
                    &path,
                    skip_cache,
                    outcome.embedded_count,
                    outcome.attempted_count,
                    None,
                );
            }
            Err(e) => {
                record_enabled_sweep_state(
                    &path,
                    skip_cache,
                    e.embedded_count,
                    e.attempted_count,
                    Some(e.message.clone()),
                );
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

fn record_enabled_sweep_state(
    path: &Path,
    skip_recall_cache: bool,
    done: usize,
    todo: usize,
    error: Option<String>,
) {
    let failed_count = if error.is_some() {
        todo.saturating_sub(done).max(1)
    } else {
        todo.saturating_sub(done)
    };
    let last_provider_error = error.as_deref().and_then(provider_error);
    if let Err(err) = vector_backfill::record_vector_sweep_state(
        path,
        VectorSweepStateUpdate {
            enabled: true,
            disabled_reason: None,
            skip_recall_cache,
            embedded_count: done,
            failed_count,
            last_error: error,
            last_provider_error,
            interval_secs: Some(sweep_interval_secs()),
        },
    ) {
        tracing::warn!(
            "[vector-sweep] {} state write failed: {err}",
            path.display()
        );
    }
}

fn record_disabled_sweep_state(
    paths: Vec<PathBuf>,
    interval_secs: u64,
    skip_recall_cache: bool,
    reason: &str,
) {
    for path in paths {
        if let Err(err) = vector_backfill::record_vector_sweep_state(
            &path,
            VectorSweepStateUpdate {
                enabled: false,
                disabled_reason: Some(reason.to_string()),
                skip_recall_cache,
                embedded_count: 0,
                failed_count: 0,
                last_error: None,
                last_provider_error: None,
                interval_secs: Some(interval_secs),
            },
        ) {
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
    use std::fs;
    use tempfile::TempDir;

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
        memory_core::MemoryStore::open(global.to_str().unwrap()).unwrap();
        memory_core::MemoryStore::open(project.to_str().unwrap()).unwrap();

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
        let store = memory_core::MemoryStore::open(db_path.to_str().unwrap()).unwrap();
        let now = chrono::Utc::now().to_rfc3339();
        for index in 0..33 {
            store
                .connection()
                .execute(
                    "INSERT INTO memories
                     (id, path, summary, text, importance, timestamp, category, topic, keywords, entities, source, scope, archived, created_at, updated_at, access_count, revision, metadata)
                     VALUES (?1, '/facts/partial', '', ?2, 0.5, ?3, 'fact', 'status', '[]', '[]', 'manual', 'project', 0, ?3, ?3, 0, 1, '{}')",
                    rusqlite::params![format!("partial-{index}"), format!("body {index}"), now],
                )
                .unwrap();
        }
        drop(store);

        crate::vector_backfill::set_test_embed_batch_results(vec![
            Ok(32),
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
}
