//! Shared fixture builders for `refinery_ops` acceptance tests. Everything
//! here is synthetic/constructed test data — no live `gh`/`git` call, no
//! network (#1002 acceptance criterion 7).

use super::doc_resolver::{DocRefResolver, DocResolution};
use std::collections::HashMap;
use tachi_params::CanonicalDocRefV1;

/// A resolver whose answers are pre-registered by the test. Anything not
/// explicitly registered as resolved comes back `Unresolved` — this is the
/// injection point `build_refinery_packet` takes in place of the real
/// `GitRefResolver` (see `refinery_ops::doc_resolver`).
#[derive(Default)]
pub(crate) struct FixtureDocResolver {
    resolved: HashMap<String, CanonicalDocRefV1>,
}

impl FixtureDocResolver {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn with_resolved(mut self, commit_sha: &str, path: &str) -> Self {
        let doc_ref = CanonicalDocRefV1 {
            repo: "owner/repo".to_string(),
            trusted_ref: "origin/main".to_string(),
            commit_sha: commit_sha.to_string(),
            path: path.to_string(),
            blob_sha: fixture_blob_sha(),
            section: "3".to_string(),
            authority_receipt: None,
            verified_reachable_at: Some("2026-07-13T00:00:00Z".to_string()),
        };
        self.resolved
            .insert(format!("{path}@{commit_sha}"), doc_ref);
        self
    }
}

impl DocRefResolver for FixtureDocResolver {
    fn resolve(
        &self,
        _repo: &str,
        path: &str,
        commit_sha: &str,
        _blob_sha: &str,
        _section: &str,
        _trusted_ref: &str,
    ) -> DocResolution {
        let key = format!("{path}@{commit_sha}");
        match self.resolved.get(&key) {
            Some(doc_ref) => DocResolution::Resolved(doc_ref.clone()),
            None => DocResolution::Unresolved {
                reason: format!("fixture: no resolution registered for {key}"),
            },
        }
    }
}

/// A resolver matching [`FixtureDocResolver::with_resolved`]'s exact commit
/// SHA + blob SHA so `build_refinery_packet`'s Spec-Ref parsing round-trips
/// cleanly in tests: the commit/blob pair a fixture issue body declares.
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
    let snapshot = tachi_params::IssueSnapshotV1 {
        issue_ref: issue_ref.to_string(),
        repo: repo.to_string(),
        number: number.parse().unwrap_or(0),
        title: "fixture".to_string(),
        body: "Fixture issue body for disposition-classifier tests.".to_string(),
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
        Vec::new(),
        Vec::new(),
        tachi_params::GroundingStatusV1::Grounded,
        &[],
        &[],
    )
}

/// Build a `gh issue view --json ...` result payload shape. `comments` is a
/// list of `(id, author_login, created_at, body)` tuples.
pub(crate) fn gh_issue_json(
    title: &str,
    body: &str,
    state: &str,
    labels: &[&str],
    milestone: Option<&str>,
    updated_at: &str,
    comments: &[(&str, &str, &str, &str)],
) -> serde_json::Value {
    serde_json::json!({
        "title": title,
        "body": body,
        "state": state,
        "labels": labels.iter().map(|l| serde_json::json!({"name": l})).collect::<Vec<_>>(),
        "milestone": milestone.map(|m| serde_json::json!({"title": m})),
        "updatedAt": updated_at,
        "comments": comments
            .iter()
            .map(|(id, author, created_at, comment_body)| {
                serde_json::json!({
                    "id": id,
                    "author": {"login": author},
                    "createdAt": created_at,
                    "body": comment_body,
                })
            })
            .collect::<Vec<_>>(),
    })
}
