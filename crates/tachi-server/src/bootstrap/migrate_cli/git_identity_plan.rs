//! Read-only report of the git-repository identity each project library
//! answers to (#2252). The identity name is derived from the repository root
//! (`path_utils::plan_c_root_derived_names`), like the path generations, so
//! nothing needs migrating: this only shows the result.

use std::error::Error;
use std::path::{Path, PathBuf};

use serde::Serialize;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(super) struct GitIdentityRow {
    pub library: String,
    pub repository: String,
    pub identity_name: String,
    pub db: PathBuf,
}

pub(super) fn git_identity_rows(tachi_home: &Path) -> Vec<GitIdentityRow> {
    let mut rows: Vec<GitIdentityRow> = std::fs::read_dir(tachi_home.join("projects"))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let dir = entry.path();
            let db = ["tachi-memory.db", "memory.db"]
                .iter()
                .map(|leaf| dir.join(leaf))
                .find(|path| path.exists())
                .and_then(|path| std::fs::canonicalize(path).ok())?;
            let root =
                crate::path_utils::plan_c_project_root_from_local_db_in_home(&db, tachi_home)?;
            let identity = crate::path_utils::git_identity::git_library_identity(&root)?;
            Some(GitIdentityRow {
                library: entry.file_name().to_string_lossy().into_owned(),
                repository: identity.display().to_string(),
                identity_name: identity.dir_name(),
                db,
            })
        })
        .collect();
    rows.sort_by(|a, b| a.library.cmp(&b.library));
    rows
}

pub(in crate::bootstrap) fn run_git_identity(
    json: bool,
    tachi_home: &Path,
) -> Result<(), Box<dyn Error>> {
    let rows = git_identity_rows(tachi_home);
    if json {
        println!("{}", serde_json::to_string_pretty(&rows)?);
        return Ok(());
    }
    println!("Git identity names (read-only; derived from each repository, nothing to migrate)\n");
    for row in &rows {
        println!(
            "{:<46} {:<34} also answers to {}",
            row.library, row.repository, row.identity_name
        );
    }
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn rows_name_each_repo_local_library_by_its_repository() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let repo = temp.path().join("repos/Sigil");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        std::fs::create_dir_all(repo.join(".tachi")).unwrap();
        std::fs::write(
            repo.join(".git/config"),
            "[remote \"origin\"]\n\turl = https://github.com/kckylechen1/Tachi.git\n",
        )
        .unwrap();
        std::fs::write(repo.join(".tachi/tachi-memory.db"), b"").unwrap();
        let alias = home.join("projects/Sigil-82fc");
        std::fs::create_dir_all(&alias).unwrap();
        std::os::unix::fs::symlink(
            repo.join(".tachi/tachi-memory.db"),
            alias.join("tachi-memory.db"),
        )
        .unwrap();
        std::fs::create_dir_all(home.join("projects/hapi")).unwrap();
        std::fs::write(home.join("projects/hapi/memory.db"), b"").unwrap();

        let rows = git_identity_rows(&home);
        assert_eq!(rows.len(), 1, "{rows:#?}");
        assert_eq!(rows[0].library, "Sigil-82fc");
        assert_eq!(rows[0].repository, "kckylechen1/Tachi");
        assert_eq!(rows[0].identity_name, "github.com_kckylechen1_tachi");
    }
}
