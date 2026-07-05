//! Daemon periodic sweep for memories missing vector embeddings.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use tokio::time::{interval, MissedTickBehavior};

use crate::manifest::Manifest;
use crate::vector_backfill;
use tachi_llm::LlmClient;

const DEFAULT_SWEEP_INTERVAL_SECS: u64 = 30 * 60;
const DEFAULT_SWEEP_BATCH_PER_DB: usize = 32;

fn sweep_interval_secs() -> u64 {
    std::env::var("TACHI_VECTOR_SWEEP_INTERVAL_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|&n| n >= 60)
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
            Ok((done, todo)) if todo > 0 => {
                total_done += done;
                tracing::info!("[vector-sweep] {} embedded {done}/{todo}", path.display());
            }
            Ok(_) => {}
            Err(e) => {
                tracing::warn!("[vector-sweep] {} failed: {e}", path.display());
            }
        }
    }
    if total_done > 0 {
        tracing::info!("[vector-sweep] run complete, embedded {total_done} row(s)");
    }
    total_done
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
}
