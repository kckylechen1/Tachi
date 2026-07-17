//! Shared fixture builders for `refinery_ops` acceptance tests. Everything
//! here is synthetic/constructed test data — no live `gh`/`git` call, no
//! network (#1002 acceptance criterion 7).

use super::doc_resolver::{CommitReachability, DocRefResolver, DocResolution};
use tachi_params::{CanonicalDocRefV1, RepoRevisionV1};

/// A resolver whose answers are pre-registered by the test via
/// [`FixtureDocResolver::with_resolved`]. Matching is EXACT across every
/// field the real `GitRefResolver` verifies — repo, commit_sha, path,
/// blob_sha, section, AND trusted_ref — not just path+commit_sha (F6,
/// build-seat REQUEST-CHANGES: a lenient match made "exact anchor" tests
/// pass even when the fixture's registered repo/blob/section/trusted_ref
/// didn't actually match what the test body declared). Anything not
/// exactly registered comes back `Unresolved`.
pub(crate) struct FixtureDocResolver {
    resolved: Vec<CanonicalDocRefV1>,
    /// `Err` by default (R4-2, build-seat REQUEST-CHANGES): a fixture that
    /// never calls `with_repo_revision` genuinely has no repo-revision pin
    /// available, same as `GitRefResolver` failing for real — it must not
    /// silently look like a resolved-but-empty axis.
    repo_revision: Result<RepoRevisionV1, String>,
    /// #1105: pre-registered `(repo, commit_sha, trusted_ref)` triples that
    /// `is_commit_reachable` reports `Reachable` for — exact match only, same
    /// "unregistered == not verified" posture as `resolved` above. Empty by
    /// default, matching `GitRefResolver`'s fail-closed behavior when a
    /// commit genuinely isn't reachable.
    reachable_commits: Vec<(String, String, String)>,
    /// #1105/fix-round-2 (PR #1191 checkpoint 1): pre-registered
    /// `(repo, commit_sha, trusted_ref)` triples that `is_commit_reachable`
    /// reports `Unavailable` for — the fixture equivalent of a real `git`
    /// command failure or a locally-absent commit object, distinct from
    /// `NotReachable` (see `CommitReachability`'s own doc comment for why a
    /// collector-level test needs to be able to exercise this case without
    /// shelling real git).
    unavailable_commits: Vec<(String, String, String, String)>,
}

impl Default for FixtureDocResolver {
    fn default() -> Self {
        Self {
            resolved: Vec::new(),
            repo_revision: Err("fixture: no repo revision configured".to_string()),
            reachable_commits: Vec::new(),
            unavailable_commits: Vec::new(),
        }
    }
}

impl FixtureDocResolver {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn with_resolved(
        mut self,
        repo: &str,
        commit_sha: &str,
        path: &str,
        blob_sha: &str,
        section: &str,
        trusted_ref: &str,
    ) -> Self {
        self.resolved.push(CanonicalDocRefV1 {
            repo: repo.to_string(),
            trusted_ref: trusted_ref.to_string(),
            commit_sha: commit_sha.to_string(),
            path: path.to_string(),
            blob_sha: blob_sha.to_string(),
            section: section.to_string(),
            authority_receipt: "fixture:pre-registered".to_string(),
            verified_reachable_at: "2026-07-13T00:00:00Z".to_string(),
        });
        self
    }

    /// Configure what `current_repo_revision` returns on success — the
    /// fixture equivalent of `GitRefResolver`'s real
    /// `git rev-parse <trusted_ref>` (F2, build-seat REQUEST-CHANGES).
    pub(crate) fn with_repo_revision(mut self, revision: RepoRevisionV1) -> Self {
        self.repo_revision = Ok(revision);
        self
    }

    /// Configure `current_repo_revision` to fail with `reason` — the
    /// fixture equivalent of a real `git rev-parse` failure (R4-2).
    pub(crate) fn with_repo_revision_failure(mut self, reason: &str) -> Self {
        self.repo_revision = Err(reason.to_string());
        self
    }

    /// Register `commit_sha` as reachable from `trusted_ref` in `repo` —
    /// the fixture equivalent of a real `git merge-base --is-ancestor`
    /// success (#1105). Anything not exactly registered stays unreachable.
    pub(crate) fn with_reachable_commit(
        mut self,
        repo: &str,
        commit_sha: &str,
        trusted_ref: &str,
    ) -> Self {
        self.reachable_commits.push((
            repo.to_string(),
            commit_sha.to_string(),
            trusted_ref.to_string(),
        ));
        self
    }

    /// Register `commit_sha` as `Unavailable` (a real git failure/absent
    /// local object, never a confirmed negative) for `(repo, trusted_ref)`
    /// — the fixture equivalent of `GitRefResolver::is_commit_reachable`
    /// hitting a `git` command failure (#1105/fix-round-2, PR #1191
    /// checkpoint 1).
    pub(crate) fn with_unavailable_commit(
        mut self,
        repo: &str,
        commit_sha: &str,
        trusted_ref: &str,
        reason: &str,
    ) -> Self {
        self.unavailable_commits.push((
            repo.to_string(),
            commit_sha.to_string(),
            trusted_ref.to_string(),
            reason.to_string(),
        ));
        self
    }
}

impl DocRefResolver for FixtureDocResolver {
    fn resolve(
        &self,
        repo: &str,
        path: &str,
        commit_sha: &str,
        blob_sha: &str,
        section: &str,
        trusted_ref: &str,
    ) -> DocResolution {
        let found = self.resolved.iter().find(|d| {
            d.repo == repo
                && d.path == path
                && d.commit_sha == commit_sha
                && d.blob_sha == blob_sha
                && d.section == section
                && d.trusted_ref == trusted_ref
        });
        match found {
            Some(doc_ref) => DocResolution::Resolved(doc_ref.clone()),
            None => DocResolution::Unresolved {
                reason: format!(
                    "fixture: no exact registration for repo={repo} path={path} \
                     commit={commit_sha} blob={blob_sha} section={section} \
                     trusted_ref={trusted_ref}"
                ),
            },
        }
    }

    fn current_repo_revision(
        &self,
        _repo: &str,
        _trusted_ref: &str,
    ) -> Result<RepoRevisionV1, String> {
        self.repo_revision.clone()
    }

    fn is_commit_reachable(
        &self,
        repo: &str,
        commit_sha: &str,
        trusted_ref: &str,
    ) -> CommitReachability {
        if let Some((.., reason)) = self
            .unavailable_commits
            .iter()
            .find(|(r, c, t, _)| r == repo && c == commit_sha && t == trusted_ref)
        {
            return CommitReachability::Unavailable(reason.clone());
        }
        if self
            .reachable_commits
            .iter()
            .any(|(r, c, t)| r == repo && c == commit_sha && t == trusted_ref)
        {
            CommitReachability::Reachable
        } else {
            CommitReachability::NotReachable
        }
    }
}

/// This leaf's own fixture convention for constructing Spec-Ref bodies:
/// repo/commit/blob/section a caller can either use as-is (matching
/// [`FixtureDocResolver::new`] registered via these same constants) or
/// override entirely via explicit `with_resolved(...)` args.
pub(crate) const FIXTURE_COMMIT_SHA: &str = "abc1234abc1234abc1234abc1234abc1234abcd";
pub(crate) const FIXTURE_DOC_PATH: &str = "docs/engineering/architecture/example.md";

pub(crate) fn fixture_blob_sha() -> String {
    "deadbeefblobsha0001".to_string()
}

pub(crate) fn spec_ref_line(repo: &str) -> String {
    format!(
        "Spec-Ref: {repo}:{FIXTURE_DOC_PATH}@{FIXTURE_COMMIT_SHA}/{}#3",
        fixture_blob_sha()
    )
}

/// A minimal, already-grounded `IssueEvidenceV1` for disposition-classifier
/// fixtures that don't need to exercise Spec-Ref/anchor parsing at all —
/// those are covered separately in `tests::grounding`.
pub(crate) fn minimal_evidence(
    issue_ref: &str,
    state: &str,
    labels: &[&str],
) -> tachi_params::IssueEvidenceV1 {
    let (repo, number) = issue_ref
        .rsplit_once('#')
        .expect("issue_ref must be 'owner/repo#N'");
    let body = "Fixture issue body for disposition-classifier tests.".to_string();
    let snapshot = tachi_params::IssueSnapshotV1 {
        issue_ref: issue_ref.to_string(),
        repo: repo.to_string(),
        number: number.parse().unwrap_or(0),
        title: "fixture".to_string(),
        body: body.clone(),
        state: state.to_string(),
        labels: labels.iter().map(|s| s.to_string()).collect(),
        milestone: None,
        dependency_refs: Vec::new(),
        selected_comment_revisions: Vec::new(),
        updated_at: "2026-07-13T00:00:00Z".to_string(),
        issue_body_hash: "fixture-body-hash".to_string(),
        issue_snapshot_hash: format!("fixture-snapshot-hash-{issue_ref}"),
    };
    super::compiler::build_issue_evidence(
        snapshot,
        &body,
        Vec::new(),
        Vec::new(),
        tachi_params::GroundingStatusV1::Grounded,
        &[],
        &[],
    )
}

/// Build a `gh issue view --json ...` result payload shape. `number` must
/// match whatever `number: u64` the test's own `build_refinery_packet` call
/// uses — `validate_gh_issue_result` (R4-1, build-seat REQUEST-CHANGES) now
/// checks it. `comments` is a list of `(id, author_login, created_at,
/// updated_at, body)` tuples — `updated_at` is `None` when a fixture wants
/// to exercise the `updatedAt`-absent fallback-to-`createdAt` path (F5).
#[allow(clippy::type_complexity)]
pub(crate) fn gh_issue_json(
    number: u64,
    title: &str,
    body: &str,
    state: &str,
    labels: &[&str],
    milestone: Option<&str>,
    updated_at: &str,
    comments: &[(&str, &str, &str, Option<&str>, &str)],
) -> serde_json::Value {
    serde_json::json!({
        "number": number,
        "title": title,
        "body": body,
        "state": state,
        "labels": labels.iter().map(|l| serde_json::json!({"name": l})).collect::<Vec<_>>(),
        "milestone": milestone.map(|m| serde_json::json!({"title": m})),
        "updatedAt": updated_at,
        "comments": comments
            .iter()
            .map(|(id, author, created_at, comment_updated_at, comment_body)| {
                let mut obj = serde_json::json!({
                    "id": id,
                    "author": {"login": author},
                    "createdAt": created_at,
                    "body": comment_body,
                });
                if let Some(u) = comment_updated_at {
                    obj["updatedAt"] = serde_json::json!(u);
                }
                obj
            })
            .collect::<Vec<_>>(),
    })
}
