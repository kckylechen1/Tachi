use super::make_server;
use chrono::Utc;
use serde_json::json;
use std::fs;
use std::path::{Path, PathBuf};
use tempfile::tempdir;

pub(super) struct DocsWorktree {
    root: tempfile::TempDir,
    original_cwd: PathBuf,
    _lock: std::sync::MutexGuard<'static, ()>,
}

impl DocsWorktree {
    pub(super) fn new() -> Self {
        let lock = super::home_test_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let root = tempdir().expect("create docs worktree fixture");
        fs::create_dir_all(root.path().join(".git")).expect("create fixture .git directory");
        fs::create_dir_all(root.path().join("docs")).expect("create fixture docs directory");
        let original_cwd = std::env::current_dir().expect("read current directory");
        std::env::set_current_dir(root.path()).expect("enter docs worktree fixture");
        Self {
            root,
            original_cwd,
            _lock: lock,
        }
    }

    pub(super) fn repo_path(&self) -> &Path {
        self.root.path()
    }

    pub(super) fn docs_path(&self) -> PathBuf {
        self.root.path().join("docs")
    }
}

impl Drop for DocsWorktree {
    fn drop(&mut self) {
        std::env::set_current_dir(&self.original_cwd).expect("restore current directory");
    }
}

mod component_governance;
mod conflicts;
mod contract;
mod downstream_sync_surface;
mod external_staffing_contract;
mod http_direct_connect;
mod hypermem_gate;
mod kernel_surface;
mod library_identity_runtime;
mod memories_writer_census;
mod organize;
mod portable_kernel_split;
mod release_distribution;
mod safety;
mod store_trigger_ddl_census;
