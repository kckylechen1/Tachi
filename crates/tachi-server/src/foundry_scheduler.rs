//! Multi-DB foundry scheduler (PR-4 reduced scope).
//!
//! ### What this owns
//! The scheduler is a single tokio task spawned by the daemon at startup that
//! periodically rescans `~/.tachi/manifest.json` and, for every DB it lists,
//! runs a per-DB **safety-net poll** every [`POLL_INTERVAL`]. Each poll opens
//! the DB by absolute path, calls [`memcore::load_pending_foundry_jobs`]
//! to find queued or stale `running` jobs, and (for DBs the existing
//! single-process foundry worker can route to) re-injects them into the
//! shared `foundry_tx` mpsc channel for execution. For DBs the existing
//! worker does **not** know how to route to (agents/, hub/, vault/, anything
//! outside global + project + named-projects), jobs are counted as orphans
//! and logged with structured `tracing` warnings.
//!
//! ### What this does NOT own
//! - Job execution: the existing `run_foundry_maintenance_worker` in
//!   `foundry_runtime_ops::maintenance` keeps that role unchanged.
//! - Channel-driven low-latency enrichment: the existing 500 ms enrichment
//!   batcher → foundry_tx fast path is untouched. Sub-millisecond enqueue
//!   latency is preserved for in-process writes.
//! - Daemon singleton enforcement: that lives in [`crate::daemon_lock`].
//!
//! ### Cadences (per-design, see PR-4 spec)
//! - Manifest re-read: [`MANIFEST_REFRESH_INTERVAL`] (60 s).
//! - Per-DB safety-net poll: [`POLL_INTERVAL`] (30 s).

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::mpsc;
use tokio::time::{interval, Instant, MissedTickBehavior};

use memcore::{load_pending_foundry_jobs, MemoryStore, PersistedFoundryJob};

use crate::foundry_runtime_ops::FoundryMaintenanceItem;
use crate::manifest::{DbRole, Manifest};
use crate::DbScope;

mod routing;
mod scheduler;
mod types;
mod worker;

use routing::{classify_route, manifest_label_for, path_hash};
pub use scheduler::FoundryScheduler;
use types::{Route, WorkerHandle};
pub use types::{WorkerMetrics, MANIFEST_REFRESH_INTERVAL, POLL_INTERVAL};
use worker::run_db_worker;

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn entry(role: DbRole, scope_hint: &str) -> crate::manifest::DbEntry {
        crate::manifest::DbEntry {
            path: "/tmp/sched-test/memory.db".to_string(),
            role,
            owner: "test".to_string(),
            schema_kind: "tachi".to_string(),
            vec_enabled: true,
            allow_write: true,
            last_doctor_at: String::new(),
            last_classification: "healthy".to_string(),
            scope_hint: scope_hint.to_string(),
            notes: String::new(),
        }
    }

    #[test]
    fn classify_route_routes_own_global() {
        let global = PathBuf::from("/tmp/sched-test/global.db");
        let r = classify_route(&entry(DbRole::Global, "global"), &global, &global, None);
        assert!(matches!(r, Route::Global));
    }

    #[test]
    fn classify_route_routes_own_project() {
        let global = PathBuf::from("/tmp/sched-test/global.db");
        let project = PathBuf::from("/tmp/sched-test/proj.db");
        let r = classify_route(
            &entry(DbRole::Project, "project"),
            &project,
            &global,
            Some(&project),
        );
        assert!(matches!(r, Route::Project));
    }

    #[test]
    fn classify_route_recognizes_named_project() {
        let _guard = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let saved = std::env::var_os("TACHI_HOME");
        std::env::set_var("TACHI_HOME", "/tmp/sched-tachi-home");
        let global = PathBuf::from("/tmp/sched-test/global.db");
        let np = PathBuf::from("/tmp/sched-tachi-home/projects/sigil/memory.db");
        let r = classify_route(&entry(DbRole::Project, ""), &np, &global, None);
        match r {
            Route::NamedProject(n) => assert_eq!(n, "sigil"),
            other => panic!("expected NamedProject(sigil), got {other:?}"),
        }
        if let Some(v) = saved {
            std::env::set_var("TACHI_HOME", v);
        } else {
            std::env::remove_var("TACHI_HOME");
        }
    }

    #[test]
    fn classify_route_recognizes_plan_c_symlink_target() {
        let _guard = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().expect("tmp");
        let saved = std::env::var_os("TACHI_HOME");
        std::env::set_var("TACHI_HOME", tmp.path().join("home"));

        let repo = tmp.path().join("Quant Analyzer");
        let local_db = repo.join(".tachi/memory.db");
        std::fs::create_dir_all(local_db.parent().unwrap()).expect("local parent");
        std::fs::write(&local_db, b"").expect("local db placeholder");
        crate::path_utils::ensure_plan_c_symlink(&local_db, &repo);

        let global = tmp.path().join("global/memory.db");
        let r = classify_route(
            &entry(DbRole::Project, "project:Quant_Analyzer"),
            &local_db,
            &global,
            None,
        );
        // The alias dir name now carries a stable-hash suffix; the route must
        // carry that exact name (so resolve_named_project_db_path can find the
        // hashed alias dir). It still starts with the sanitized basename.
        let expected = crate::path_utils::plan_c_dir_name_from_root(&repo).expect("name");
        assert!(expected.starts_with("Quant_Analyzer-"), "{expected}");
        match r {
            Route::NamedProject(n) => assert_eq!(n, expected),
            other => panic!("expected NamedProject({expected}), got {other:?}"),
        }

        if let Some(v) = saved {
            std::env::set_var("TACHI_HOME", v);
        } else {
            std::env::remove_var("TACHI_HOME");
        }
    }

    #[test]
    fn classify_route_path_for_agent_db() {
        let global = PathBuf::from("/tmp/sched-test/global.db");
        let agent = PathBuf::from("/home/u/.tachi/agents/main/memory.db");
        let r = classify_route(&entry(DbRole::Agent, "agent"), &agent, &global, None);
        assert!(matches!(r, Route::Path));
    }

    #[test]
    fn classify_route_orphan_default_reason_when_no_hint() {
        let global = PathBuf::from("/tmp/sched-test/global.db");
        let weird = PathBuf::from("/somewhere/else/x.db");
        let mut e = entry(DbRole::Unknown, "");
        e.allow_write = false;
        let r = classify_route(&e, &weird, &global, None);
        match r {
            Route::Orphan(reason) => assert_eq!(reason, "unscoped"),
            other => panic!("expected Orphan(unscoped), got {other:?}"),
        }
    }

    #[test]
    fn manifest_label_prefers_scope_hint() {
        let p = PathBuf::from("/x/y/z.db");
        assert_eq!(manifest_label_for(&p, "global"), "global");
    }

    #[test]
    fn manifest_label_falls_back_to_parent_filename() {
        let p = PathBuf::from("/x/agents/main/memory.db");
        assert_eq!(manifest_label_for(&p, ""), "main/memory.db");
    }

    #[test]
    fn named_project_extracted_from_canonical_layout() {
        let _guard = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let saved = std::env::var_os("TACHI_HOME");
        std::env::set_var("TACHI_HOME", "/tmp/sched-tachi-home");
        let p = PathBuf::from("/tmp/sched-tachi-home/projects/myproj/memory.db");
        assert_eq!(
            crate::path_utils::named_project_from_path(&p).as_deref(),
            Some("myproj")
        );
        if let Some(v) = saved {
            std::env::set_var("TACHI_HOME", v);
        } else {
            std::env::remove_var("TACHI_HOME");
        }
    }

    #[test]
    fn named_project_rejects_non_canonical_layout() {
        let p = PathBuf::from("/x/y/notprojects/foo/memory.db");
        assert!(crate::path_utils::named_project_from_path(&p).is_none());
    }

    #[test]
    fn named_project_rejects_external_projects_dir() {
        let p = PathBuf::from("/home/u/work/data/tachi/projects/hyperion/memory.db");
        assert!(crate::path_utils::named_project_from_path(&p).is_none());
    }
}
