//! Git-repository identity for project libraries (#2252 phase 1).
//!
//! A library keyed by its checkout path splits one repository across paths,
//! machines and naming generations. The origin remote names the repository
//! itself, so every clone and worktree of it resolves to one identity.
//! Checkouts without an origin keep the path-derived identity.

use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GitLibraryIdentity {
    host: String,
    /// Repository path under the host, e.g. `kckylechen1/Tachi`.
    path: String,
}

impl GitLibraryIdentity {
    /// Case-folded comparison key, e.g. `github.com/kckylechen1/tachi`.
    pub(crate) fn key(&self) -> String {
        format!("{}/{}", self.host, self.path).to_ascii_lowercase()
    }

    /// Human name with original case, e.g. `kckylechen1/Tachi`.
    pub(crate) fn display(&self) -> &str {
        &self.path
    }

    /// Repository name, e.g. `Tachi`.
    pub(crate) fn repo_name(&self) -> &str {
        self.path.rsplit('/').next().unwrap_or(&self.path)
    }

    /// Library directory name: the key with separators made path-safe, e.g.
    /// `github.com_kckylechen1_tachi`. Always a canonical project identity.
    pub(crate) fn dir_name(&self) -> String {
        self.key()
            .chars()
            .map(|ch| match ch {
                'a'..='z' | '0'..='9' | '-' | '_' => ch,
                '.' => '.',
                _ => '_',
            })
            .collect::<String>()
            .replace("..", "_")
    }
}

/// Normalize a git remote URL to a host + repository path. Userinfo (tokens,
/// passwords) is always dropped. Local path and `file://` remotes name no
/// shared repository and return `None`.
pub(crate) fn normalize_remote_url(url: &str) -> Option<GitLibraryIdentity> {
    let url = url.trim();
    let (host, path) = if let Some((scheme, rest)) = url.split_once("://") {
        if !matches!(scheme, "https" | "http" | "ssh" | "git") {
            return None;
        }
        let rest = rest.rsplit_once('@').map_or(rest, |(_, after)| after);
        let (authority, path) = rest.split_once('/')?;
        let host = authority.split(':').next()?;
        (host, path)
    } else if !url.starts_with('/') && url.contains(':') {
        // scp-like `git@host:owner/repo.git`
        let (before, path) = url.split_once(':')?;
        let host = before.rsplit_once('@').map_or(before, |(_, after)| after);
        (host, path)
    } else {
        return None;
    };
    let host = host.trim().to_ascii_lowercase();
    let path = path.trim_matches('/');
    let path = path
        .strip_suffix(".git")
        .unwrap_or(path)
        .trim_end_matches('/');
    let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    if host.is_empty() || segments.len() < 2 || segments.iter().any(|s| *s == "." || *s == "..") {
        return None;
    }
    Some(GitLibraryIdentity {
        host,
        path: segments.join("/"),
    })
}

/// Identity of the repository checked out at `root`, read from its git
/// config without spawning git. Linked worktrees resolve through `commondir`.
pub(crate) fn git_library_identity(root: &Path) -> Option<GitLibraryIdentity> {
    let common = git_common_dir(root)?;
    let config = std::fs::read_to_string(common.join("config")).ok()?;
    normalize_remote_url(&origin_url(&config)?)
}

fn git_common_dir(root: &Path) -> Option<PathBuf> {
    let dot_git = root.join(".git");
    if dot_git.is_dir() {
        return Some(dot_git);
    }
    let pointer = std::fs::read_to_string(&dot_git).ok()?;
    let gitdir = pointer.trim().strip_prefix("gitdir:")?.trim();
    let gitdir = root.join(gitdir);
    match std::fs::read_to_string(gitdir.join("commondir")) {
        Ok(common) => Some(gitdir.join(common.trim())),
        Err(_) => Some(gitdir),
    }
}

fn origin_url(config: &str) -> Option<String> {
    let mut in_origin = false;
    for line in config.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_origin = line.replace(' ', "") == r#"[remote"origin"]"#;
            continue;
        }
        if !in_origin {
            continue;
        }
        if let Some((key, value)) = line.split_once('=') {
            if key.trim() == "url" {
                return Some(value.trim().to_string());
            }
        }
    }
    None
}

/// A library directory under `<tachi_home>/projects`, with the repository its
/// database belongs to when it is repo-local.
#[derive(Debug, Clone)]
pub(crate) struct ProjectLibrary {
    pub(crate) name: String,
    pub(crate) db: Option<PathBuf>,
    pub(crate) repo_root: Option<PathBuf>,
    pub(crate) identity: Option<GitLibraryIdentity>,
}

/// Every library directory, sorted by name. Read-only: stats, symlink
/// resolution and git config reads only.
pub(crate) fn scan_project_libraries(tachi_home: &Path) -> Vec<ProjectLibrary> {
    let mut libraries: Vec<ProjectLibrary> = std::fs::read_dir(tachi_home.join("projects"))
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|t| t.is_dir()))
        .map(|entry| {
            let dir = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            let db = ["tachi-memory.db", "memory.db"]
                .iter()
                .map(|leaf| dir.join(leaf))
                .find(|path| path.exists())
                .and_then(|path| std::fs::canonicalize(path).ok());
            let repo_root = db
                .as_deref()
                .and_then(|db| super::plan_c_project_root_from_local_db_in_home(db, tachi_home));
            let identity = repo_root.as_deref().and_then(git_library_identity);
            ProjectLibrary {
                name,
                db,
                repo_root,
                identity,
            }
        })
        .collect();
    libraries.sort_by(|a, b| a.name.cmp(&b.name));
    libraries
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LibraryAlias {
    /// `name` is itself an existing library; use it unchanged.
    Exact,
    /// `name` names a repository whose library is this canonical name.
    Resolved(String),
    /// `name` names several repositories' libraries.
    Ambiguous(Vec<String>),
    /// No library or repository matches.
    Unknown,
}

/// Resolve a caller-supplied project name. An existing library name always
/// wins, so current addressing never changes. Otherwise the name may be a
/// repository name (`Tachi`), `owner/repo`, the identity key or its directory
/// form; matching is case-insensitive.
pub(crate) fn resolve_library_alias(tachi_home: &Path, name: &str) -> LibraryAlias {
    // Fast path: canonical names are the common case and need no scan.
    if super::is_canonical_project_identity(name) {
        let dir = tachi_home.join("projects").join(name);
        if ["tachi-memory.db", "memory.db"]
            .iter()
            .any(|leaf| dir.join(leaf).exists())
        {
            return LibraryAlias::Exact;
        }
    }
    let libraries = scan_project_libraries(tachi_home);
    let wanted = name.trim().trim_end_matches(".git").to_ascii_lowercase();
    // One entry per physical DB: several path-generation names can alias it.
    let mut by_db: std::collections::BTreeMap<PathBuf, Vec<&ProjectLibrary>> =
        std::collections::BTreeMap::new();
    for lib in &libraries {
        let (Some(db), Some(id)) = (&lib.db, &lib.identity) else {
            continue;
        };
        let matches = [
            id.repo_name().to_ascii_lowercase(),
            id.display().to_ascii_lowercase(),
            id.key(),
            id.dir_name(),
        ]
        .contains(&wanted);
        if matches {
            by_db.entry(db.clone()).or_default().push(lib);
        }
    }
    let mut targets: Vec<String> = by_db
        .values()
        .map(|libs| {
            // Prefer the current-generation name for the repository root.
            let preferred = libs[0]
                .repo_root
                .as_deref()
                .and_then(super::plan_c_dir_name_from_root);
            libs.iter()
                .find(|lib| Some(&lib.name) == preferred.as_ref())
                .unwrap_or(&libs[0])
                .name
                .clone()
        })
        .collect();
    match targets.len() {
        0 => LibraryAlias::Unknown,
        1 => LibraryAlias::Resolved(targets.remove(0)),
        _ => LibraryAlias::Ambiguous(targets),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A tachi home with repo-local libraries aliased under `projects/`.
    #[cfg(unix)]
    pub(crate) fn alias_fixture() -> tempfile::TempDir {
        let temp = tempfile::tempdir().expect("tempdir");
        let home = temp.path().join("home");
        let repo = |dir: &str, url: &str| {
            let root = temp.path().join("repos").join(dir);
            std::fs::create_dir_all(root.join(".git")).unwrap();
            std::fs::create_dir_all(root.join(".tachi")).unwrap();
            std::fs::write(
                root.join(".git/config"),
                format!("[remote \"origin\"]\n\turl = {url}\n"),
            )
            .unwrap();
            std::fs::write(root.join(".tachi/tachi-memory.db"), b"").unwrap();
            root.join(".tachi/tachi-memory.db")
        };
        let link = |name: &str, db: &Path| {
            let dir = home.join("projects").join(name);
            std::fs::create_dir_all(&dir).unwrap();
            std::os::unix::fs::symlink(db, dir.join("tachi-memory.db")).unwrap();
        };
        let tachi = repo("Sigil", "https://github.com/kckylechen1/Tachi.git");
        link("Sigil-82fc", &tachi);
        link("Sigil-ecbd", &tachi);
        let a = repo("a", "https://github.com/alice/shared.git");
        let b = repo("b", "https://github.com/bob/shared.git");
        link("a-1", &a);
        link("b-2", &b);
        temp
    }

    #[cfg(unix)]
    #[test]
    fn repository_names_resolve_and_existing_names_win() {
        let temp = alias_fixture();
        let home = temp.path().join("home");
        for alias in [
            "Tachi",
            "tachi",
            "kckylechen1/Tachi",
            "github.com/kckylechen1/tachi",
            "github.com_kckylechen1_tachi",
        ] {
            assert_eq!(
                resolve_library_alias(&home, alias),
                LibraryAlias::Resolved("Sigil-82fc".to_string()),
                "{alias}"
            );
        }
        assert_eq!(
            resolve_library_alias(&home, "Sigil-ecbd"),
            LibraryAlias::Exact
        );
        assert_eq!(
            resolve_library_alias(&home, "shared"),
            LibraryAlias::Ambiguous(vec!["a-1".to_string(), "b-2".to_string()])
        );
        assert_eq!(
            resolve_library_alias(&home, "alice/shared"),
            LibraryAlias::Resolved("a-1".to_string())
        );
        assert_eq!(
            resolve_library_alias(&home, "nothing-here"),
            LibraryAlias::Unknown
        );

        // A legacy library that already owns the bare name keeps it.
        std::fs::create_dir_all(home.join("projects/tachi")).unwrap();
        std::fs::write(home.join("projects/tachi/memory.db"), b"").unwrap();
        assert_eq!(resolve_library_alias(&home, "tachi"), LibraryAlias::Exact);
    }

    #[test]
    fn https_ssh_and_scp_forms_name_the_same_repository() {
        let expected = "github.com/kckylechen1/tachi";
        for url in [
            "https://github.com/kckylechen1/Tachi.git",
            "https://github.com/kckylechen1/Tachi",
            "https://GitHub.com/kckylechen1/Tachi/",
            "git@github.com:kckylechen1/Tachi.git",
            "ssh://git@github.com/kckylechen1/Tachi",
            "ssh://git@github.com:22/kckylechen1/Tachi.git",
        ] {
            let id = normalize_remote_url(url).unwrap_or_else(|| panic!("{url}"));
            assert_eq!(id.key(), expected, "{url}");
            assert_eq!(id.display(), "kckylechen1/Tachi", "{url}");
        }
        let id = normalize_remote_url("https://github.com/kckylechen1/Tachi.git").unwrap();
        assert_eq!(id.repo_name(), "Tachi");
        assert_eq!(id.dir_name(), "github.com_kckylechen1_tachi");
        assert!(crate::path_utils::is_canonical_project_identity(
            &id.dir_name()
        ));
    }

    #[test]
    fn credentials_never_reach_the_identity() {
        let id = normalize_remote_url("https://user:ghp_secret@github.com/o/r.git").unwrap();
        assert_eq!(id.key(), "github.com/o/r");
        assert!(!id.dir_name().contains("secret"));
    }

    #[test]
    fn nested_groups_are_kept_and_unshared_remotes_are_refused() {
        let id = normalize_remote_url("https://gitlab.com/group/sub/repo.git").unwrap();
        assert_eq!(id.display(), "group/sub/repo");
        assert_eq!(id.dir_name(), "gitlab.com_group_sub_repo");
        for url in [
            "/srv/repo.git",
            "file:///srv/repo.git",
            "https://github.com/onlyowner",
            "",
        ] {
            assert!(normalize_remote_url(url).is_none(), "{url}");
        }
    }

    #[test]
    fn worktrees_resolve_to_the_main_repository_origin() {
        let temp = tempfile::tempdir().expect("tempdir");
        let main = temp.path().join("main");
        let git = main.join(".git");
        std::fs::create_dir_all(git.join("worktrees/wt")).unwrap();
        std::fs::write(
            git.join("config"),
            "[core]\n\tbare = false\n[remote \"upstream\"]\n\turl = https://github.com/other/fork.git\n[remote \"origin\"]\n\turl = git@github.com:kckylechen1/Tachi.git\n",
        )
        .unwrap();
        std::fs::write(git.join("worktrees/wt/commondir"), "../..\n").unwrap();
        let wt = temp.path().join("wt");
        std::fs::create_dir_all(&wt).unwrap();
        std::fs::write(
            wt.join(".git"),
            format!("gitdir: {}\n", git.join("worktrees/wt").display()),
        )
        .unwrap();

        assert_eq!(
            git_library_identity(&main).map(|id| id.key()),
            Some("github.com/kckylechen1/tachi".to_string())
        );
        assert_eq!(git_library_identity(&wt), git_library_identity(&main));
        assert!(git_library_identity(temp.path()).is_none(), "no .git");
    }
}
