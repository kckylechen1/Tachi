use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use tokio::task::JoinHandle;

use crate::continuity_ops::{self, ContinuityEventTarget};
use crate::manifest::Manifest;
use crate::{DbScope, MemoryServer};

pub(crate) struct ContinuityProjectionScheduler {
    cancel_tx: tokio::sync::watch::Sender<()>,
    join: JoinHandle<()>,
}

impl ContinuityProjectionScheduler {
    pub(crate) fn start(
        manifest_path: PathBuf,
        server: MemoryServer,
        own_global: PathBuf,
        own_project: Option<PathBuf>,
    ) -> Self {
        Self::start_inner(manifest_path, server, own_global, own_project, true)
    }

    pub(crate) fn start_own_dbs(
        manifest_path: PathBuf,
        server: MemoryServer,
        own_global: PathBuf,
        own_project: Option<PathBuf>,
    ) -> Self {
        Self::start_inner(manifest_path, server, own_global, own_project, false)
    }

    fn start_inner(
        manifest_path: PathBuf,
        server: MemoryServer,
        own_global: PathBuf,
        own_project: Option<PathBuf>,
        include_manifest: bool,
    ) -> Self {
        let (cancel_tx, mut cancel_rx) = tokio::sync::watch::channel(());
        let join = tokio::spawn(async move {
            run_projection_pass(
                &server,
                &manifest_path,
                &own_global,
                own_project.as_deref(),
                include_manifest,
            );

            let mut tick = tokio::time::interval(projection_interval());
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tokio::select! {
                    _ = cancel_rx.changed() => break,
                    _ = tick.tick() => {
                        run_projection_pass(
                            &server,
                            &manifest_path,
                            &own_global,
                            own_project.as_deref(),
                            include_manifest,
                        );
                    }
                }
            }
        });

        Self { cancel_tx, join }
    }
}

impl Drop for ContinuityProjectionScheduler {
    fn drop(&mut self) {
        if let Err(error) = self.cancel_tx.send(()) {
            tracing::warn!(error = %error, "failed to send continuity projection cancel signal");
        }
        self.join.abort();
    }
}

fn projection_interval() -> Duration {
    let secs = std::env::var("TACHI_CONTINUITY_PROJECTION_INTERVAL_SECS")
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
        .unwrap_or(300)
        .clamp(30, 86_400);
    Duration::from_secs(secs)
}

fn projection_limit() -> usize {
    std::env::var("TACHI_CONTINUITY_PROJECTION_LIMIT")
        .ok()
        .and_then(|value| value.trim().parse::<usize>().ok())
        .unwrap_or(500)
        .clamp(1, 500)
}

fn run_projection_pass(
    server: &MemoryServer,
    manifest_path: &Path,
    own_global: &Path,
    own_project: Option<&Path>,
    include_manifest: bool,
) {
    for target in projection_targets(manifest_path, own_global, own_project, include_manifest) {
        match continuity_ops::project_auto_continuity_events_for_target(
            server,
            target,
            projection_limit(),
        ) {
            Ok(report) => {
                let projected = report
                    .get("projected_count")
                    .and_then(|value| value.as_u64())
                    .unwrap_or(0);
                let errors = report
                    .get("error_count")
                    .and_then(|value| value.as_u64())
                    .unwrap_or(0);
                let promotion_candidates = report
                    .get("promotion_candidate_count")
                    .and_then(|value| value.as_u64())
                    .unwrap_or(0);
                if projected > 0 || promotion_candidates > 0 || errors > 0 {
                    tracing::info!(
                        target: "tachi::continuity_projector",
                        projected,
                        promotion_candidates,
                        errors,
                        "continuity projection pass completed"
                    );
                }
            }
            Err(error) => {
                tracing::warn!(
                    target: "tachi::continuity_projector",
                    error = %error,
                    "continuity projection pass failed"
                );
            }
        }
    }
}

fn projection_targets(
    manifest_path: &Path,
    own_global: &Path,
    own_project: Option<&Path>,
    include_manifest: bool,
) -> Vec<ContinuityEventTarget> {
    let mut targets = Vec::new();
    let mut seen = HashSet::new();
    let mut push_path = |path: PathBuf| {
        if path.exists() && seen.insert(path.clone()) {
            targets.push(ContinuityEventTarget::new(
                DbScope::Project,
                None,
                Some(path),
            ));
        }
    };

    if include_manifest {
        if let Ok(manifest) = Manifest::load(manifest_path) {
            for entry in manifest.dbs {
                if entry.allow_write && entry.schema_kind == "tachi" {
                    push_path(PathBuf::from(entry.path));
                }
            }
        }
    } else {
        push_path(own_global.to_path_buf());
        if let Some(project) = own_project {
            push_path(project.to_path_buf());
        }
    }

    targets
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn projection_targets_scoped_mode_uses_only_daemon_dbs() {
        let tmp = TempDir::new().expect("tmp");
        let global = tmp.path().join("agent").join("memory.db");
        let project = tmp.path().join("project").join("memory.db");
        let unrelated = tmp.path().join("unrelated").join("memory.db");
        std::fs::create_dir_all(global.parent().unwrap()).expect("global parent");
        std::fs::create_dir_all(project.parent().unwrap()).expect("project parent");
        std::fs::create_dir_all(unrelated.parent().unwrap()).expect("unrelated parent");
        std::fs::write(&global, b"").expect("global");
        std::fs::write(&project, b"").expect("project");
        std::fs::write(&unrelated, b"").expect("unrelated");

        let targets = projection_targets(Path::new("missing.json"), &global, Some(&project), false);
        assert_eq!(targets.len(), 2);
    }
}
