//! Canonical doc anchor resolution (canon doc §4.1/§5): verifies a parsed
//! `Spec-Ref:` line against real repo state before it may become a linked
//! spec. "The resolver verifies that the doc commit was reachable from the
//! named owner-controlled trusted ref at capture time; a SHA alone does not
//! grant canonical authority" (canon doc §5) — so resolution checks commit
//! existence, reachability from `trusted_ref`, that the declared `blob_sha`
//! still matches the path at that commit, AND (R4-1, build-seat
//! REQUEST-CHANGES) that the declared `#section` actually exists as a
//! markdown heading anchor in that blob's content — a Spec-Ref pinning a
//! real doc/commit/blob but a section that was since deleted must not be
//! reported as `Resolved`.
//!
//! [`GitRefResolver`] is the real, git-shelling implementation used by the
//! live `refine_issues` action. Its `resolve`/`current_repo_revision`
//! bodies are NOT exercised by any unit test in this leaf — tests inject a
//! `FixtureDocResolver` instead (see `refinery_ops::fixtures`), matching
//! #1002 acceptance criterion 7's "zero GitHub mutation, no live git"
//! requirement. The markdown-section-anchor matching logic itself
//! ([`section_anchor_exists`]) is extracted as a pure function specifically
//! so it CAN be unit-tested without shelling git (see this file's own
//! `#[cfg(test)]` module). `GitRefResolver` is deliberately fail-closed: any
//! git command failure, or a section that can't be confirmed present,
//! resolves to `Unresolved`, never a false `Resolved`.

use tachi_params::{CanonicalDocRefV1, RepoRevisionV1};

pub(crate) enum DocResolution {
    Resolved(CanonicalDocRefV1),
    Unresolved { reason: String },
}

/// #1105/fix-round-2 (cross-vendor adversarial review, PR #1191, codex-b4d8f
/// checkpoint 1): the outcome of a commit-reachability check, distinct from a
/// plain `bool` so a caller can tell a REAL negative ("checked, confirmed not
/// an ancestor of `trusted_ref`") from mere UNCERTAINTY (repo identity
/// mismatch, the commit object missing from this local checkout — which for
/// a `gh`-sourced merge-commit sha most often means a stale/unfetched
/// checkout, not a nonexistent commit — or the `git` command itself failing
/// to run/exiting with an unrecognized status). The original #1105 `bool`
/// contract collapsed all three into `false`, which `pick_shipped_evidence`
/// then silently promoted to a confirmed-negative `ShippedCheckOutcome::NotShipped`
/// — never fail-closed for the "could not determine" case. A caller MUST
/// treat `Unavailable` the same as "not checked", never as `NotReachable`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CommitReachability {
    Reachable,
    NotReachable,
    Unavailable(String),
}

/// #1105/fix-round final-gate: `Send + Sync` supertraits are required, not
/// cosmetic — `handle_refine_issues` (`mod.rs`) holds `Box<dyn
/// DocRefResolver>` across `.await` points inside an async fn whose future
/// must itself be `Send` (the MCP tool-dispatch layer drives every tool
/// handler's future on a multi-threaded executor). Without these
/// supertraits, `dyn DocRefResolver`/`Box<dyn DocRefResolver>` are neither
/// `Send` nor `Sync` by default (trait objects don't inherit auto traits
/// from their concrete implementor unless the trait itself declares them),
/// so the crate fails to compile with a "future cannot be sent between
/// threads safely" error. Every real implementor (`GitRefResolver`,
/// `NullDocResolver`, `FixtureDocResolver` in `fixtures.rs`) is built from
/// plain owned `String`/`PathBuf` fields and already satisfies both bounds
/// — this only makes that fact visible to the trait-object boundary.
pub(crate) trait DocRefResolver: Send + Sync {
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
    /// a specific doc's blob sha (F2). `Err` (R4-2, build-seat
    /// REQUEST-CHANGES) when unavailable for any reason — no live checkout,
    /// repo identity mismatch, or the git command itself failing — carrying
    /// the reason so the caller can surface a real contradiction instead of
    /// silently leaving the replay axis empty.
    fn current_repo_revision(
        &self,
        repo: &str,
        trusted_ref: &str,
    ) -> Result<RepoRevisionV1, String>;

    /// #1105 (commit-reachability shipped check, canon doc §5 delivery-state
    /// ownership table: "shipped ... requires reachability from an
    /// owner-controlled main/release ref"): `Reachable` iff `commit_sha`
    /// exists in `repo`'s history AND is a confirmed ancestor of
    /// `trusted_ref`; `NotReachable` iff that was checked and confirmed
    /// false (a real negative); `Unavailable` for everything this leaf
    /// cannot actually determine (repo identity mismatch, commit absent
    /// locally, `git` command failure) — see [`CommitReachability`]'s own
    /// doc comment (fix-round-2, PR #1191 checkpoint 1) for why collapsing
    /// `Unavailable` into `NotReachable` was the bug. Reuses the same
    /// resolver already threaded through `build_refinery_packet` (rather
    /// than a second I/O boundary) so live callers get real git
    /// verification and tests stay injectable via
    /// `FixtureDocResolver`/`NullDocResolver` with zero network/process
    /// calls. Fail-closed: any ambiguity returns `Unavailable`, never a
    /// false-positive `Reachable` AND never a false-confirmed
    /// `NotReachable`.
    fn is_commit_reachable(
        &self,
        repo: &str,
        commit_sha: &str,
        trusted_ref: &str,
    ) -> CommitReachability;
}

pub(crate) struct GitRefResolver {
    pub(crate) repo_root: std::path::PathBuf,
    /// The repo identity this local checkout actually is. A `Spec-Ref:`
    /// line declaring a DIFFERENT repo must never be resolved against this
    /// checkout's git history — that would silently grant canonical
    /// authority to a doc in a repo this process never verified (F1).
    /// R4-3 (build-seat REQUEST-CHANGES, REGRESSION finding): the caller
    /// (`refinery_ops::mod::handle_refine_issues`) is required to establish
    /// BOTH `repo_root` and `known_repo` from a verified source (a git
    /// checkout whose `origin` remote actually matches the requested repo)
    /// — never from ambient `current_dir()` alone, which in daemon mode can
    /// be the runtime/daemon directory rather than this repo's checkout.
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

        if !git_commit_reachable(&self.repo_root, commit_sha, trusted_ref) {
            return DocResolution::Unresolved {
                reason: format!(
                    "commit {commit_sha} not found in local checkout, or not reachable from trusted ref {trusted_ref}"
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

        // R4-1: the blob exists and matches, but the specific `#section`
        // anchor it claims might not — read the blob's content and check.
        let content_output = std::process::Command::new("git")
            .arg("-C")
            .arg(&self.repo_root)
            .arg("cat-file")
            .arg("-p")
            .arg(&actual_blob_sha)
            .output();
        let content = match content_output {
            Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout).to_string(),
            _ => {
                return DocResolution::Unresolved {
                    reason: format!(
                        "could not read blob content for {path}@{commit_sha} to verify section '{section}'"
                    ),
                }
            }
        };
        if !section_anchor_exists(&content, section) {
            return DocResolution::Unresolved {
                reason: format!("section '{section}' not found in {path}@{commit_sha}"),
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

    fn current_repo_revision(
        &self,
        repo: &str,
        trusted_ref: &str,
    ) -> Result<RepoRevisionV1, String> {
        if repo != self.known_repo {
            return Err(format!(
                "repo identity mismatch: requested '{repo}' but this checkout is known as '{}'",
                self.known_repo
            ));
        }
        let output = std::process::Command::new("git")
            .arg("-C")
            .arg(&self.repo_root)
            .arg("rev-parse")
            .arg(trusted_ref)
            .output()
            .map_err(|e| format!("failed to run git rev-parse {trusted_ref}: {e}"))?;
        if !output.status.success() {
            return Err(format!(
                "git rev-parse {trusted_ref} failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        let commit_sha = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if commit_sha.is_empty() {
            return Err(format!("git rev-parse {trusted_ref} returned empty output"));
        }
        Ok(RepoRevisionV1 {
            repo: repo.to_string(),
            git_ref: trusted_ref.to_string(),
            commit_sha,
            verified_at: chrono::Utc::now().to_rfc3339(),
        })
    }

    fn is_commit_reachable(
        &self,
        repo: &str,
        commit_sha: &str,
        trusted_ref: &str,
    ) -> CommitReachability {
        if repo != self.known_repo {
            return CommitReachability::Unavailable(format!(
                "repo identity mismatch: requested '{repo}' but this checkout is known as '{}'",
                self.known_repo
            ));
        }
        git_commit_reachability_detailed(&self.repo_root, commit_sha, trusted_ref)
    }
}

/// Shared by `GitRefResolver::resolve` (Spec-Ref commit verification) and
/// `GitRefResolver::is_commit_reachable` (#1105 shipped-evidence check):
/// true iff `commit_sha` exists in this checkout's history AND is an
/// ancestor of `trusted_ref`. Fail-closed — any git command failure (commit
/// absent, not a git repo, `git` not on PATH) returns `false`.
fn git_commit_reachable(repo_root: &std::path::Path, commit_sha: &str, trusted_ref: &str) -> bool {
    let commit_exists = std::process::Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .arg("cat-file")
        .arg("-e")
        .arg(format!("{commit_sha}^{{commit}}"))
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !commit_exists {
        return false;
    }

    std::process::Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .arg("merge-base")
        .arg("--is-ancestor")
        .arg(commit_sha)
        .arg(trusted_ref)
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// #1105/fix-round-2 (PR #1191 checkpoint 1): the tri-state counterpart of
/// [`git_commit_reachable`] for [`GitRefResolver::is_commit_reachable`] —
/// SAME two git calls, but distinguishes a git command/process failure or a
/// locally-absent commit object (`Unavailable`, uncertain — never treated as
/// a confirmed negative) from a git call that actually completed and
/// answered "not an ancestor" (`NotReachable`, a real negative). Not shared
/// with `git_commit_reachable` (used by `resolve()`'s Spec-Ref anchor
/// verification): that call site's existing "any failure -> Unresolved"
/// posture is untouched by this leaf — this finer distinction exists only
/// where the aggregate result (`ShippedCheckOutcome`) can promote a `false`
/// into a `ClosedUnshipped` disposition input.
fn git_commit_reachability_detailed(
    repo_root: &std::path::Path,
    commit_sha: &str,
    trusted_ref: &str,
) -> CommitReachability {
    let exists = std::process::Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .arg("cat-file")
        .arg("-e")
        .arg(format!("{commit_sha}^{{commit}}"))
        .status();
    match exists {
        Ok(status) if status.success() => {}
        Ok(_) => {
            return CommitReachability::Unavailable(format!(
                "commit {commit_sha} not found in local checkout — cannot confirm shipped/unshipped \
                 (a stale or unfetched local checkout looks identical to a nonexistent commit; not \
                 a confirmed negative)"
            ));
        }
        Err(e) => {
            return CommitReachability::Unavailable(format!(
                "failed to run git cat-file -e {commit_sha}^{{{{commit}}}}: {e}"
            ));
        }
    }

    // `git merge-base --is-ancestor` exits 0 (yes) / 1 (no, a real answer) /
    // anything else (128 etc.) on a genuine error — `.output()` (not
    // `.status()`) so the exact exit code can be inspected instead of
    // treating "no" and "error" the same way.
    let ancestor_output = std::process::Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .arg("merge-base")
        .arg("--is-ancestor")
        .arg(commit_sha)
        .arg(trusted_ref)
        .output();
    match ancestor_output {
        Ok(out) => match out.status.code() {
            Some(0) => CommitReachability::Reachable,
            Some(1) => CommitReachability::NotReachable,
            other => CommitReachability::Unavailable(format!(
                "git merge-base --is-ancestor {commit_sha} {trusted_ref} exited with an \
                 unrecognized status ({other:?}): {}",
                String::from_utf8_lossy(&out.stderr).trim()
            )),
        },
        Err(e) => CommitReachability::Unavailable(format!(
            "failed to run git merge-base --is-ancestor {commit_sha} {trusted_ref}: {e}"
        )),
    }
}

/// R4-3: never resolves anything and never reports a repo revision — used
/// by `handle_refine_issues` when it cannot establish a trusted repo root
/// (no local checkout whose `origin` remote matches the requested repo),
/// rather than guessing from an unverified ambient `current_dir()`.
pub(crate) struct NullDocResolver {
    pub(crate) reason: String,
}

impl DocRefResolver for NullDocResolver {
    fn resolve(
        &self,
        _repo: &str,
        _path: &str,
        _commit_sha: &str,
        _blob_sha: &str,
        _section: &str,
        _trusted_ref: &str,
    ) -> DocResolution {
        DocResolution::Unresolved {
            reason: self.reason.clone(),
        }
    }

    fn current_repo_revision(
        &self,
        _repo: &str,
        _trusted_ref: &str,
    ) -> Result<RepoRevisionV1, String> {
        Err(self.reason.clone())
    }

    fn is_commit_reachable(
        &self,
        _repo: &str,
        _commit_sha: &str,
        _trusted_ref: &str,
    ) -> CommitReachability {
        CommitReachability::Unavailable(self.reason.clone())
    }
}

/// True if `doc_content`'s markdown contains a heading anchor matching
/// `section` — either a GitHub-style slug of the heading text, or (since
/// this leaf's own fixtures and the canon doc's own numbered-section style
/// both use bare numbers like `#3`) a heading whose text begins with a
/// numeric prefix equal to `section` (e.g. heading "3. One typed evidence
/// envelope" matches `section = "3"`). Pure, no I/O — the real resolver
/// fetches `doc_content` via `git cat-file -p <blob_sha>` first (untested
/// here, same policy as the rest of `GitRefResolver`); this matching logic
/// is unit-tested directly in this file's `#[cfg(test)]` module.
pub(crate) fn section_anchor_exists(doc_content: &str, section: &str) -> bool {
    let section = section.trim();
    if section.is_empty() {
        return false;
    }
    let section_lower = section.to_ascii_lowercase();
    for line in doc_content.lines() {
        let trimmed = line.trim_start();
        let Some(heading_text) = trimmed
            .strip_prefix('#')
            .map(|rest| rest.trim_start_matches('#').trim())
        else {
            continue;
        };
        if heading_text.is_empty() {
            continue;
        }
        let numeric_prefix: String = heading_text
            .chars()
            .take_while(|c| c.is_ascii_digit())
            .collect();
        if !numeric_prefix.is_empty() && numeric_prefix == section {
            return true;
        }
        if github_style_slug(heading_text) == section_lower {
            return true;
        }
    }
    false
}

fn github_style_slug(heading_text: &str) -> String {
    heading_text
        .to_ascii_lowercase()
        .chars()
        .filter_map(|c| {
            if c.is_ascii_alphanumeric() || c == ' ' || c == '-' {
                Some(if c == ' ' { '-' } else { c })
            } else {
                None
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_DOC: &str = "\
# Issue Refinery and Memory Knowledge Lanes

## 1. Decision

Some decision text.

## 3. One typed evidence envelope

Envelope text.

## 5. Issue-to-doc contract

Contract text.
";

    /// #1105/fix-round-2: `NullDocResolver` never resolves anything AND never
    /// confirms a commit reachable — pure, no I/O, so unlike
    /// `GitRefResolver`'s real git shelling this is directly unit-testable
    /// (same posture as `resolve`/`current_repo_revision`'s existing
    /// fail-closed contract on this type). It reports `Unavailable` (never
    /// `Reachable`, and — post fix-round-2 — never the confirmed-negative
    /// `NotReachable` either, since it genuinely never checked).
    #[test]
    fn null_doc_resolver_never_reports_a_commit_reachable() {
        let resolver = NullDocResolver {
            reason: "no verified repo root".to_string(),
        };
        assert_eq!(
            resolver.is_commit_reachable("owner/repo", "deadbeef", "origin/main"),
            CommitReachability::Unavailable("no verified repo root".to_string())
        );
    }

    #[test]
    fn section_anchor_exists_matches_numeric_prefix() {
        assert!(section_anchor_exists(SAMPLE_DOC, "3"));
        assert!(section_anchor_exists(SAMPLE_DOC, "5"));
    }

    #[test]
    fn section_anchor_exists_matches_github_style_slug() {
        // "## 1. Decision" -> lowercase "1. decision" -> '.' dropped, ' '
        // becomes '-' -> slug "1-decision".
        assert!(section_anchor_exists(SAMPLE_DOC, "1-decision"));
    }

    /// R4-1: a section that was never present (or was deleted) must not be
    /// reported as existing — this is the pure-logic half of "Spec-Ref
    /// pins a real doc/commit/blob but a since-deleted section".
    #[test]
    fn section_anchor_exists_returns_false_for_a_missing_section() {
        assert!(!section_anchor_exists(SAMPLE_DOC, "99"));
        assert!(!section_anchor_exists(SAMPLE_DOC, "not-a-real-heading"));
    }

    #[test]
    fn section_anchor_exists_returns_false_for_empty_section() {
        assert!(!section_anchor_exists(SAMPLE_DOC, ""));
        assert!(!section_anchor_exists(SAMPLE_DOC, "   "));
    }
}
