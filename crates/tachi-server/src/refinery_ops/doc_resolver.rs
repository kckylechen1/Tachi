//! Canonical doc anchor resolution (canon doc §4.1/§5): verifies a parsed
//! `Spec-Ref:` line against real repo state before it may become a linked
//! spec. "The resolver verifies that the doc commit was reachable from the
//! named owner-controlled trusted ref at capture time; a SHA alone does not
//! grant canonical authority" (canon doc §5) — so resolution checks commit
//! existence, reachability from `trusted_ref`, AND that the declared
//! `blob_sha` still matches the path at that commit.
//!
//! [`GitRefResolver`] is the real, git-shelling implementation used by the
//! live `refine_issues` action. It is NOT exercised by any unit test in this
//! leaf — tests inject a `FixtureDocResolver` instead (see
//! `refinery_ops::fixtures`), matching #1002 acceptance criterion 7's "zero
//! GitHub mutation, no live git" requirement. `GitRefResolver` is
//! deliberately fail-closed: any git command failure resolves to
//! `Unresolved`, never a false `Resolved`.

use tachi_params::{CanonicalDocRefV1, RepoRevisionV1};

pub(crate) enum DocResolution {
    Resolved(CanonicalDocRefV1),
    Unresolved { reason: String },
}

pub(crate) trait DocRefResolver {
    #[allow(clippy::too_many_arguments)]
    fn resolve(
        &self,
        repo: &str,
        path: &str,
        commit_sha: &str,
        blob_sha: &str,
        section: &str,
        trusted_ref: &str,
    ) -> DocResolution;

    /// The repo's current commit at `trusted_ref` (canon doc §3/§5's
    /// `RepoRevisionV1`) — pins a proposal's `based_on_repo_revisions` so
    /// `check_proposal_replay` can detect the repo itself moving, not just
    /// a specific doc's blob sha (F2). `None` when unavailable (e.g. no
    /// live git checkout to inspect) — a caller with no repo revision to
    /// pin simply has an empty axis on replay, same as today.
    fn current_repo_revision(&self, repo: &str, trusted_ref: &str) -> Option<RepoRevisionV1>;
}

pub(crate) struct GitRefResolver {
    pub(crate) repo_root: std::path::PathBuf,
    /// The repo identity this local checkout actually is. A `Spec-Ref:`
    /// line declaring a DIFFERENT repo must never be resolved against this
    /// checkout's git history — that would silently grant canonical
    /// authority to a doc in a repo this process never verified (F1).
    pub(crate) known_repo: String,
}

impl DocRefResolver for GitRefResolver {
    fn resolve(
        &self,
        repo: &str,
        path: &str,
        commit_sha: &str,
        blob_sha: &str,
        section: &str,
        trusted_ref: &str,
    ) -> DocResolution {
        if repo != self.known_repo {
            return DocResolution::Unresolved {
                reason: format!(
                    "repo identity mismatch: Spec-Ref declares '{repo}' but this checkout is '{}' — refusing to resolve against the wrong repo's git history",
                    self.known_repo
                ),
            };
        }

        let commit_exists = std::process::Command::new("git")
            .arg("-C")
            .arg(&self.repo_root)
            .arg("cat-file")
            .arg("-e")
            .arg(format!("{commit_sha}^{{commit}}"))
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !commit_exists {
            return DocResolution::Unresolved {
                reason: format!("commit {commit_sha} not found in local checkout"),
            };
        }

        let reachable = std::process::Command::new("git")
            .arg("-C")
            .arg(&self.repo_root)
            .arg("merge-base")
            .arg("--is-ancestor")
            .arg(commit_sha)
            .arg(trusted_ref)
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !reachable {
            return DocResolution::Unresolved {
                reason: format!(
                    "commit {commit_sha} is not reachable from trusted ref {trusted_ref}"
                ),
            };
        }

        let output = std::process::Command::new("git")
            .arg("-C")
            .arg(&self.repo_root)
            .arg("rev-parse")
            .arg(format!("{commit_sha}:{path}"))
            .output();
        let actual_blob_sha = match output {
            Ok(out) if out.status.success() => {
                String::from_utf8_lossy(&out.stdout).trim().to_string()
            }
            _ => {
                return DocResolution::Unresolved {
                    reason: format!("path {path} not found at commit {commit_sha}"),
                }
            }
        };
        if actual_blob_sha != blob_sha {
            return DocResolution::Unresolved {
                reason: format!(
                    "blob sha drift for {path}@{commit_sha}: pinned {blob_sha}, actual {actual_blob_sha}"
                ),
            };
        }

        DocResolution::Resolved(CanonicalDocRefV1 {
            repo: repo.to_string(),
            trusted_ref: trusted_ref.to_string(),
            commit_sha: commit_sha.to_string(),
            path: path.to_string(),
            blob_sha: blob_sha.to_string(),
            section: section.to_string(),
            authority_receipt: format!("git:reachable-from:{trusted_ref}"),
            verified_reachable_at: chrono::Utc::now().to_rfc3339(),
        })
    }

    fn current_repo_revision(&self, repo: &str, trusted_ref: &str) -> Option<RepoRevisionV1> {
        if repo != self.known_repo {
            return None;
        }
        let output = std::process::Command::new("git")
            .arg("-C")
            .arg(&self.repo_root)
            .arg("rev-parse")
            .arg(trusted_ref)
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let commit_sha = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if commit_sha.is_empty() {
            return None;
        }
        Some(RepoRevisionV1 {
            repo: repo.to_string(),
            git_ref: trusted_ref.to_string(),
            commit_sha,
            verified_at: chrono::Utc::now().to_rfc3339(),
        })
    }
}
