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

#[cfg(test)]
mod tests {
    use super::*;

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
