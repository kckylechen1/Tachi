//! Daemon periodic sweep for memories missing vector embeddings.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::time::{interval, MissedTickBehavior};

use crate::llm::LlmClient;
use crate::manifest::Manifest;
use crate::vector_backfill;

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

fn collect_sweep_paths(
    manifest_path: &Path,
    global_db: &Path,
    project_db: Option<&Path>,
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

    if let Ok(manifest) = Manifest::load(manifest_path) {
        for entry in manifest.dbs {
            if !entry.allow_write || entry.schema_kind != "tachi" {
                continue;
            }
            push(PathBuf::from(entry.path));
        }
    }

    paths
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
        let (cancel_tx, mut cancel_rx) = tokio::sync::watch::channel(());
        let runs = Arc::new(AtomicU64::new(0));

        tokio::spawn(async move {
            if sweep_disabled() {
                tracing::info!("[vector-sweep] disabled via TACHI_DISABLE_VECTOR_SWEEP");
                return;
            }
            let mut tick = interval(Duration::from_secs(sweep_interval_secs()));
            tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
            tick.tick().await;

            loop {
                tokio::select! {
                    _ = cancel_rx.changed() => break,
                    _ = tick.tick() => {
                        let paths = collect_sweep_paths(
                            &manifest_path,
                            &global_db,
                            project_db.as_deref(),
                        );
                        let batch = sweep_batch_per_db();
                        let skip_cache = skip_recall_cache();
                        let mut total_done = 0usize;
                        for path in paths {
                            match vector_backfill::sweep_db_vectors(
                                &path,
                                llm.as_ref(),
                                batch,
                                skip_cache,
                            ).await {
                                Ok((done, todo)) if todo > 0 => {
                                    total_done += done;
                                    tracing::info!(
                                        "[vector-sweep] {} embedded {done}/{todo}",
                                        path.display()
                                    );
                                }
                                Ok(_) => {}
                                Err(e) => {
                                    tracing::warn!(
                                        "[vector-sweep] {} failed: {e}",
                                        path.display()
                                    );
                                }
                            }
                        }
                        runs.fetch_add(1, Ordering::Relaxed);
                        if total_done > 0 {
                            tracing::info!(
                                "[vector-sweep] run complete, embedded {total_done} row(s)"
                            );
                        }
                    }
                }
            }
        });

        Self {
            _cancel: cancel_tx,
        }
    }
}