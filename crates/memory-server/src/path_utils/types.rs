use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub(crate) struct PlanCSplitBrain {
    pub(crate) project_name: String,
    pub(crate) canonical_db: PathBuf,
    pub(crate) alias_db: PathBuf,
    pub(crate) canonical_rows: Option<i64>,
    pub(crate) alias_rows: Option<i64>,
    pub(crate) canonical_bytes: Option<u64>,
    pub(crate) alias_bytes: Option<u64>,
}

impl PlanCSplitBrain {
    pub(crate) fn warning_message(&self) -> String {
        format!(
            "Plan C split-brain detected for project '{}': repo-local DB {} (rows={}, bytes={}) and alias DB {} (rows={}, bytes={}) are different regular files. Back up both, merge by id into the repo-local DB, then replace the alias with a symlink to the repo-local DB.",
            self.project_name,
            self.canonical_db.display(),
            opt_i64(self.canonical_rows),
            opt_u64(self.canonical_bytes),
            self.alias_db.display(),
            opt_i64(self.alias_rows),
            opt_u64(self.alias_bytes),
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PlanCLinkOutcome {
    AlreadyLinked,
    Created(PathBuf),
    SplitBrain(PlanCSplitBrain),
    Skipped(&'static str),
    Failed { path: PathBuf, error: String },
}

fn opt_i64(value: Option<i64>) -> String {
    value
        .map(|value| value.to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

fn opt_u64(value: Option<u64>) -> String {
    value
        .map(|value| value.to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

pub(super) fn file_len(path: &Path) -> Option<u64> {
    std::fs::metadata(path).ok().map(|metadata| metadata.len())
}

pub(super) fn active_memory_count(path: &Path) -> Option<i64> {
    let conn =
        rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .ok()?;
    conn.query_row(
        "SELECT COUNT(*) FROM memories WHERE archived = 0",
        [],
        |row| row.get(0),
    )
    .ok()
}

pub(super) fn canonical_paths_equal(left: &Path, right: &Path) -> bool {
    std::fs::canonicalize(left)
        .ok()
        .zip(std::fs::canonicalize(right).ok())
        .is_some_and(|(left, right)| left == right)
}

#[cfg(unix)]
pub(super) fn same_file_identity(left: &Path, right: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(left)
        .ok()
        .zip(std::fs::metadata(right).ok())
        .is_some_and(|(left, right)| left.dev() == right.dev() && left.ino() == right.ino())
}

#[cfg(not(unix))]
pub(super) fn same_file_identity(left: &Path, right: &Path) -> bool {
    canonical_paths_equal(left, right)
}
