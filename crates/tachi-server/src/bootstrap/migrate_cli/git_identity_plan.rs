//! Read-only plan for moving project libraries to git-repository identity
//! (#2252 phase 1). It opens nothing for writing and changes no file; the
//! apply step ships separately after the plan is reviewed.

use std::collections::BTreeMap;
use std::error::Error;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::path_utils::git_identity::scan_project_libraries;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(super) enum GitIdentityAction {
    /// Already the git-identity library.
    Current,
    /// Same physical DB as the rest of its group; only the name changes.
    Rename,
    /// A different physical DB of the same repository; contents merge.
    Merge,
    /// No repository, but the name equals a repository's name; merge only if
    /// confirmed.
    MergeCandidate,
    /// No git repository; keeps its name.
    Keep,
    /// Directory with no database.
    CleanupEmpty,
}

impl GitIdentityAction {
    fn label(&self) -> &'static str {
        match self {
            Self::Current => "current",
            Self::Rename => "rename",
            Self::Merge => "merge",
            Self::MergeCandidate => "merge_candidate",
            Self::Keep => "keep",
            Self::CleanupEmpty => "cleanup_empty",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub(super) struct GitIdentityEntry {
    pub library: String,
    pub db: Option<PathBuf>,
    pub rows: Option<i64>,
    pub repo_root: Option<PathBuf>,
    pub git_identity: Option<String>,
    pub target: Option<String>,
    pub action: GitIdentityAction,
}

pub(super) fn plan_git_identity(tachi_home: &Path) -> Vec<GitIdentityEntry> {
    let scanned = scan_project_libraries(tachi_home).libraries;

    // Repositories known on this machine, by name, for merge candidates.
    let repo_by_name: BTreeMap<String, String> = scanned
        .iter()
        .filter_map(|s| s.identity.as_ref())
        .map(|id| (id.repo_name().to_ascii_lowercase(), id.dir_name()))
        .collect();
    // First physical DB seen per target decides Rename vs Merge.
    let mut first_db: BTreeMap<String, PathBuf> = BTreeMap::new();

    scanned
        .into_iter()
        .map(|s| {
            let rows = s.db.as_deref().and_then(count_rows);
            let (target, action) = match (&s.db, &s.identity) {
                (None, _) => (None, GitIdentityAction::CleanupEmpty),
                (Some(db), Some(id)) => {
                    let target = id.dir_name();
                    let action = if s.name == target {
                        GitIdentityAction::Current
                    } else {
                        match first_db.get(&target) {
                            Some(seen) if seen != db => GitIdentityAction::Merge,
                            _ => GitIdentityAction::Rename,
                        }
                    };
                    first_db.entry(target.clone()).or_insert_with(|| db.clone());
                    (Some(target), action)
                }
                (Some(_), None) => match repo_by_name.get(&s.name.to_ascii_lowercase()) {
                    Some(target) => (Some(target.clone()), GitIdentityAction::MergeCandidate),
                    None => (None, GitIdentityAction::Keep),
                },
            };
            GitIdentityEntry {
                library: s.name,
                db: s.db,
                rows,
                repo_root: s.repo_root,
                git_identity: s.identity.map(|id| id.display().to_string()),
                target,
                action,
            }
        })
        .collect()
}

fn count_rows(db: &Path) -> Option<i64> {
    let conn = rusqlite::Connection::open_with_flags(
        db,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .ok()?;
    conn.query_row(
        "SELECT count(*) FROM memories WHERE coalesce(archived, 0) = 0",
        [],
        |row| row.get(0),
    )
    .ok()
}

pub(in crate::bootstrap) fn run_git_identity_plan(
    json: bool,
    tachi_home: &Path,
) -> Result<(), Box<dyn Error>> {
    let plan = plan_git_identity(tachi_home);
    if json {
        println!("{}", serde_json::to_string_pretty(&plan)?);
        return Ok(());
    }
    println!("Git identity plan (read-only; nothing was changed)\n");
    if scan_project_libraries(tachi_home).truncated {
        println!("WARNING: more libraries than the scan bound; this plan is partial.\n");
    }
    for entry in &plan {
        let rows = entry.rows.map_or("-".to_string(), |n| n.to_string());
        let target = entry.target.as_deref().unwrap_or("-");
        println!(
            "{:<16} {:<46} rows={:<6} -> {}",
            entry.action.label(),
            entry.library,
            rows,
            target
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo(home: &Path, dir: &str, origin: Option<&str>) -> PathBuf {
        let root = home.join("repos").join(dir);
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::create_dir_all(root.join(".tachi")).unwrap();
        let config = origin.map_or(String::new(), |url| {
            format!("[remote \"origin\"]\n\turl = {url}\n")
        });
        std::fs::write(root.join(".git/config"), config).unwrap();
        root
    }

    fn db(path: &Path, rows: usize) {
        let conn = rusqlite::Connection::open(path).unwrap();
        conn.execute_batch("CREATE TABLE memories (id TEXT, archived INTEGER);")
            .unwrap();
        for i in 0..rows {
            conn.execute("INSERT INTO memories VALUES (?1, 0)", [i.to_string()])
                .unwrap();
        }
    }

    fn alias(home: &Path, name: &str, target: &Path) {
        let dir = home.join("projects").join(name);
        std::fs::create_dir_all(&dir).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(target, dir.join("tachi-memory.db")).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn plan_groups_libraries_by_repository_and_changes_nothing() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("tachi-home");
        std::fs::create_dir_all(home.join("projects")).unwrap();

        // Two path-generation aliases of one repo-local DB.
        let sigil = repo(
            temp.path(),
            "Sigil",
            Some("https://github.com/kckylechen1/Tachi.git"),
        );
        db(&sigil.join(".tachi/tachi-memory.db"), 3);
        alias(&home, "Sigil-82fc", &sigil.join(".tachi/tachi-memory.db"));
        alias(&home, "Sigil-ecbd", &sigil.join(".tachi/tachi-memory.db"));
        // A legacy standalone library named like the repository.
        std::fs::create_dir_all(home.join("projects/tachi")).unwrap();
        db(&home.join("projects/tachi/memory.db"), 2);
        // A non-git named library, and an empty directory.
        std::fs::create_dir_all(home.join("projects/hapi")).unwrap();
        db(&home.join("projects/hapi/memory.db"), 5);
        std::fs::create_dir_all(home.join("projects/grok-79e8")).unwrap();

        let before: Vec<_> = walk(&home);
        let plan = plan_git_identity(&home);
        assert_eq!(walk(&home), before, "plan must not change files");

        let by_name = |n: &str| plan.iter().find(|e| e.library == n).unwrap();
        let target = "github.com_kckylechen1_tachi";
        assert_eq!(by_name("Sigil-82fc").action, GitIdentityAction::Rename);
        assert_eq!(by_name("Sigil-ecbd").action, GitIdentityAction::Rename);
        assert_eq!(by_name("Sigil-82fc").target.as_deref(), Some(target));
        assert_eq!(by_name("Sigil-82fc").rows, Some(3));
        assert_eq!(
            by_name("Sigil-82fc").git_identity.as_deref(),
            Some("kckylechen1/Tachi")
        );
        assert_eq!(by_name("tachi").action, GitIdentityAction::MergeCandidate);
        assert_eq!(by_name("tachi").target.as_deref(), Some(target));
        assert_eq!(by_name("hapi").action, GitIdentityAction::Keep);
        assert_eq!(by_name("grok-79e8").action, GitIdentityAction::CleanupEmpty);
    }

    #[cfg(unix)]
    #[test]
    fn a_second_physical_db_of_the_same_repository_is_a_merge() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("tachi-home");
        std::fs::create_dir_all(home.join("projects")).unwrap();
        let a = repo(temp.path(), "a", Some("git@github.com:o/r.git"));
        let b = repo(temp.path(), "b", Some("https://github.com/o/r"));
        db(&a.join(".tachi/tachi-memory.db"), 1);
        db(&b.join(".tachi/tachi-memory.db"), 1);
        alias(&home, "a-1", &a.join(".tachi/tachi-memory.db"));
        alias(&home, "b-2", &b.join(".tachi/tachi-memory.db"));
        let plan = plan_git_identity(&home);
        let actions: Vec<_> = plan.iter().map(|e| e.action.clone()).collect();
        assert_eq!(
            actions,
            [GitIdentityAction::Rename, GitIdentityAction::Merge]
        );
    }

    fn walk(dir: &Path) -> Vec<(PathBuf, u64)> {
        let mut out = Vec::new();
        for entry in std::fs::read_dir(dir).unwrap().flatten() {
            let path = entry.path();
            let meta = std::fs::symlink_metadata(&path).unwrap();
            if meta.is_dir() {
                out.extend(walk(&path));
            } else {
                out.push((path, meta.len()));
            }
        }
        out.sort();
        out
    }
}
