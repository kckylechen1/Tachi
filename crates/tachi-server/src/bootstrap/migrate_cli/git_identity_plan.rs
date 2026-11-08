//! Git-identity aliases for project libraries (#2252 phase 1, step 4).
//!
//! The existing resolver accepts any `projects/<name>/` symlink to a
//! repo-local DB and checks it by physical identity. So giving a library its
//! git identity takes only new alias directories: no DB is opened, moved or
//! renamed, and every existing name keeps working. Without `--apply` this is
//! a read-only plan.

use std::collections::BTreeMap;
use std::error::Error;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::path_utils::git_identity::{git_library_identity, GitLibraryIdentity};

const ALIAS_DB_LEAF: &str = "tachi-memory.db";

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(tag = "op", rename_all = "snake_case")]
pub(super) enum GitIdentityOp {
    /// Create `projects/<name>/tachi-memory.db` -> `target`.
    AddAlias {
        name: String,
        target: PathBuf,
        repository: String,
    },
    /// Remove a library directory that contains nothing.
    RemoveEmptyDir { name: String },
    /// Reported, never applied.
    Blocked { name: String, reason: String },
}

struct RepoLibrary {
    name: String,
    db: PathBuf,
    identity: GitLibraryIdentity,
}

pub(super) fn plan_git_identity(tachi_home: &Path) -> Vec<GitIdentityOp> {
    let projects = tachi_home.join("projects");
    let mut existing: BTreeMap<String, Option<PathBuf>> = BTreeMap::new();
    let mut empty_dirs = Vec::new();
    let mut repo_libraries = Vec::new();
    for entry in std::fs::read_dir(&projects).into_iter().flatten().flatten() {
        if !entry.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        let dir = entry.path();
        if std::fs::read_dir(&dir).is_ok_and(|mut entries| entries.next().is_none()) {
            empty_dirs.push(name.clone());
        }
        let db = library_db(&dir);
        if let Some(db) = &db {
            let identity =
                crate::path_utils::plan_c_project_root_from_local_db_in_home(db, tachi_home)
                    .as_deref()
                    .and_then(git_library_identity);
            if let Some(identity) = identity {
                repo_libraries.push(RepoLibrary {
                    name: name.clone(),
                    db: db.clone(),
                    identity,
                });
            }
        }
        existing.insert(name, db);
    }

    // Distinct physical DBs per repository.
    let mut by_repo: BTreeMap<String, (GitLibraryIdentity, Vec<&RepoLibrary>)> = BTreeMap::new();
    for lib in &repo_libraries {
        let slot = by_repo
            .entry(lib.identity.key())
            .or_insert_with(|| (lib.identity.clone(), Vec::new()));
        if !slot.1.iter().any(|seen| seen.db == lib.db) {
            slot.1.push(lib);
        }
    }
    let mut short_owners: BTreeMap<String, usize> = BTreeMap::new();
    for (identity, _) in by_repo.values() {
        *short_owners.entry(short_name(identity)).or_default() += 1;
    }

    let mut ops = Vec::new();
    for (identity, libs) in by_repo.values() {
        let repository = identity.display().to_string();
        if libs.len() > 1 {
            let names: Vec<&str> = libs.iter().map(|lib| lib.name.as_str()).collect();
            ops.push(GitIdentityOp::Blocked {
                name: identity.dir_name(),
                reason: format!(
                    "{} databases belong to {repository} ({}); merge them first",
                    libs.len(),
                    names.join(", ")
                ),
            });
            continue;
        }
        let target = libs[0].db.clone();
        let mut aliases = vec![identity.dir_name()];
        let short = short_name(identity);
        if short_owners.get(&short) == Some(&1) {
            aliases.push(short);
        } else {
            ops.push(GitIdentityOp::Blocked {
                name: short,
                reason: "several repositories share this name".to_string(),
            });
        }
        for name in aliases {
            if !crate::path_utils::is_canonical_project_identity(&name) {
                continue;
            }
            match existing.get(&name) {
                None => ops.push(GitIdentityOp::AddAlias {
                    name,
                    target: target.clone(),
                    repository: repository.clone(),
                }),
                Some(Some(db)) if *db == target => {} // already in place
                Some(_) => ops.push(GitIdentityOp::Blocked {
                    name,
                    reason: "taken by an existing library".to_string(),
                }),
            }
        }
    }
    ops.extend(
        empty_dirs
            .into_iter()
            .map(|name| GitIdentityOp::RemoveEmptyDir { name }),
    );
    ops
}

fn short_name(identity: &GitLibraryIdentity) -> String {
    identity.repo_name().to_ascii_lowercase()
}

fn library_db(dir: &Path) -> Option<PathBuf> {
    ["tachi-memory.db", "memory.db"]
        .iter()
        .map(|leaf| dir.join(leaf))
        .find(|path| path.exists())
        .and_then(|path| std::fs::canonicalize(path).ok())
}

/// Apply one op, rechecking its precondition. Returns whether it changed
/// anything.
fn apply_op(projects: &Path, op: &GitIdentityOp) -> Result<bool, String> {
    match op {
        GitIdentityOp::AddAlias { name, target, .. } => {
            let dir = projects.join(name);
            if dir.exists() {
                return if library_db(&dir).as_ref() == Some(target) {
                    Ok(false)
                } else {
                    Err(format!("{name}: appeared since planning; left untouched"))
                };
            }
            if !target.exists() {
                return Err(format!("{name}: target {} is gone", target.display()));
            }
            std::fs::create_dir(&dir).map_err(|e| format!("{name}: {e}"))?;
            symlink(target, &dir.join(ALIAS_DB_LEAF)).map_err(|e| {
                let _ = std::fs::remove_dir(&dir);
                format!("{name}: {e}")
            })?;
            Ok(true)
        }
        GitIdentityOp::RemoveEmptyDir { name } => {
            // remove_dir refuses a non-empty directory, so content is never lost.
            std::fs::remove_dir(projects.join(name)).map_err(|e| format!("{name}: {e}"))?;
            Ok(true)
        }
        GitIdentityOp::Blocked { .. } => Ok(false),
    }
}

#[cfg(unix)]
fn symlink(target: &Path, link: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

#[cfg(not(unix))]
fn symlink(_target: &Path, _link: &Path) -> std::io::Result<()> {
    Err(std::io::Error::other(
        "git-identity aliases need symlinks (Unix)",
    ))
}

#[derive(Serialize)]
struct Receipt<'a> {
    applied_at: String,
    applied: Vec<&'a GitIdentityOp>,
    failed: Vec<String>,
    blocked: Vec<&'a GitIdentityOp>,
}

pub(in crate::bootstrap) fn run_git_identity(
    json: bool,
    apply: bool,
    tachi_home: &Path,
) -> Result<(), Box<dyn Error>> {
    let ops = plan_git_identity(tachi_home);
    if !apply {
        if json {
            println!("{}", serde_json::to_string_pretty(&ops)?);
        } else {
            println!("Git identity plan (read-only; nothing was changed)\n");
            for op in &ops {
                println!("{}", describe(op));
            }
        }
        return Ok(());
    }

    let projects = tachi_home.join("projects");
    let mut applied = Vec::new();
    let mut failed = Vec::new();
    for op in &ops {
        match apply_op(&projects, op) {
            Ok(true) => applied.push(op),
            Ok(false) => {}
            Err(error) => failed.push(error),
        }
    }
    let receipt = Receipt {
        applied_at: chrono::Utc::now().to_rfc3339(),
        applied,
        failed,
        blocked: ops
            .iter()
            .filter(|op| matches!(op, GitIdentityOp::Blocked { .. }))
            .collect(),
    };
    let dir = tachi_home.join("migrations");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!(
        "git-identity-{}.json",
        chrono::Utc::now().format("%Y%m%dT%H%M%S%.3fZ")
    ));
    std::fs::write(&path, serde_json::to_vec_pretty(&receipt)?)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&receipt)?);
    } else {
        for op in &receipt.applied {
            println!("applied  {}", describe(op));
        }
        for error in &receipt.failed {
            println!("failed   {error}");
        }
        for op in &receipt.blocked {
            println!("         {}", describe(op));
        }
        println!("\nreceipt: {}", path.display());
    }
    if receipt.failed.is_empty() {
        Ok(())
    } else {
        Err(format!("{} git-identity action(s) failed", receipt.failed.len()).into())
    }
}

fn describe(op: &GitIdentityOp) -> String {
    match op {
        GitIdentityOp::AddAlias {
            name,
            target,
            repository,
        } => format!(
            "add_alias        {name} -> {} ({repository})",
            target.display()
        ),
        GitIdentityOp::RemoveEmptyDir { name } => format!("remove_empty_dir {name}"),
        GitIdentityOp::Blocked { name, reason } => format!("blocked          {name}: {reason}"),
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    struct Fixture {
        _temp: tempfile::TempDir,
        root: PathBuf,
        home: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            let temp = tempfile::tempdir().unwrap();
            let root = temp.path().to_path_buf();
            let home = root.join("tachi-home");
            std::fs::create_dir_all(home.join("projects")).unwrap();
            Self {
                _temp: temp,
                root,
                home,
            }
        }

        /// A repo-local DB with an origin, aliased under `projects/<alias>`.
        fn repo(&self, dir: &str, origin: &str, alias: &str) -> PathBuf {
            let repo = self.root.join("repos").join(dir);
            std::fs::create_dir_all(repo.join(".git")).unwrap();
            std::fs::create_dir_all(repo.join(".tachi")).unwrap();
            std::fs::write(
                repo.join(".git/config"),
                format!("[remote \"origin\"]\n\turl = {origin}\n"),
            )
            .unwrap();
            let db = repo.join(".tachi/tachi-memory.db");
            std::fs::write(&db, b"").unwrap();
            self.link(alias, &db);
            std::fs::canonicalize(db).unwrap()
        }

        fn link(&self, alias: &str, db: &Path) {
            let dir = self.home.join("projects").join(alias);
            std::fs::create_dir_all(&dir).unwrap();
            std::os::unix::fs::symlink(db, dir.join(ALIAS_DB_LEAF)).unwrap();
        }

        fn standalone(&self, name: &str) {
            let dir = self.home.join("projects").join(name);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("memory.db"), b"").unwrap();
        }
    }

    #[test]
    fn plan_adds_identity_and_short_aliases_and_reports_conflicts() {
        let f = Fixture::new();
        let tachi = f.repo(
            "Sigil",
            "https://github.com/kckylechen1/Tachi.git",
            "Sigil-82fc",
        );
        f.link("Sigil-ecbd", &tachi); // second name, same DB
        let zc = f.repo(
            "zeroclaw",
            "git@github.com:kckylechen1/zeroclaw.git",
            "zeroclaw-8820",
        );
        f.standalone("tachi"); // legacy library holding the short name
        std::fs::create_dir_all(f.home.join("projects/grok-79e8")).unwrap();

        let ops = plan_git_identity(&f.home);
        let add = |name: &str, target: &Path, repo: &str| GitIdentityOp::AddAlias {
            name: name.to_string(),
            target: target.to_path_buf(),
            repository: repo.to_string(),
        };
        assert!(ops.contains(&add(
            "github.com_kckylechen1_tachi",
            &tachi,
            "kckylechen1/Tachi"
        )));
        assert!(ops.contains(&add(
            "github.com_kckylechen1_zeroclaw",
            &zc,
            "kckylechen1/zeroclaw"
        )));
        assert!(ops.contains(&add("zeroclaw", &zc, "kckylechen1/zeroclaw")));
        assert!(ops.contains(&GitIdentityOp::Blocked {
            name: "tachi".to_string(),
            reason: "taken by an existing library".to_string(),
        }));
        assert!(ops.contains(&GitIdentityOp::RemoveEmptyDir {
            name: "grok-79e8".to_string()
        }));
        assert_eq!(ops.len(), 5, "{ops:#?}");
    }

    #[test]
    fn a_repository_with_two_databases_is_blocked() {
        let f = Fixture::new();
        f.repo("a", "https://github.com/o/r.git", "a-1");
        f.repo("b", "git@github.com:o/r.git", "b-2");
        let ops = plan_git_identity(&f.home);
        assert!(
            matches!(
                ops.as_slice(),
                [GitIdentityOp::Blocked { name, reason }]
                    if name == "github.com_o_r" && reason.contains("merge them first")
            ),
            "{ops:#?}"
        );
    }

    #[test]
    fn apply_creates_resolvable_aliases_once_and_never_removes_content() {
        let f = Fixture::new();
        let tachi = f.repo(
            "Sigil",
            "https://github.com/kckylechen1/Tachi.git",
            "Sigil-82fc",
        );
        std::fs::create_dir_all(f.home.join("projects/grok-79e8")).unwrap();
        f.standalone("hapi");

        run_git_identity(false, true, &f.home).expect("apply");
        for alias in ["github.com_kckylechen1_tachi", "tachi"] {
            let resolved =
                crate::MemoryServer::resolve_existing_named_project_db_path_in_home(alias, &f.home)
                    .expect("resolver accepts the alias")
                    .expect("alias resolves");
            assert_eq!(std::fs::canonicalize(resolved).unwrap(), tachi, "{alias}");
        }
        assert!(!f.home.join("projects/grok-79e8").exists());
        assert!(f.home.join("projects/hapi/memory.db").exists());
        let receipts: Vec<_> = std::fs::read_dir(f.home.join("migrations"))
            .unwrap()
            .collect();
        assert_eq!(receipts.len(), 1);

        // Idempotent: nothing left to do.
        let again = plan_git_identity(&f.home);
        assert!(again.is_empty(), "{again:#?}");
    }
}
