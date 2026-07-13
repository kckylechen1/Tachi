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

use tachi_params::CanonicalDocRefV1;

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
}

pub(crate) struct GitRefResolver {
    pub(crate) repo_root: std::path::PathBuf,
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
            authority_receipt: None,
            verified_reachable_at: Some(chrono::Utc::now().to_rfc3339()),
        })
    }
}
