//! Read-only GitHub corpus reader surface (#1059).
//!
//! The trait exposes ONLY `read_*` methods. Mutation verbs are refused by
//! [`refuse_github_mutation`]; there is no write path on this adapter.

use serde_json::Value;
use tachi_params::{IssueSnapshotV1, PullRequestSnapshotV1};

use super::parse::{
    parse_pr_snapshot_from_gh_json, CaseCorpusBundle, ParseError, ProvenanceEventKindV1,
    ProvenanceEventV1,
};
use super::pilot::CorpusCaseV1;
use crate::refinery_ops::parse::parse_issue_snapshot_from_gh_json;

/// Forbidden GitHub mutation verbs — the adapter surface must refuse each.
pub const FORBIDDEN_GITHUB_MUTATIONS: &[&str] = &[
    "comment",
    "label",
    "close",
    "reopen",
    "body_edit",
    "pr_mutate",
];

/// Always errs for known mutation verbs. Public adapter surface routes any
/// mutation attempt through this so tests can prove zero GitHub writes.
pub fn refuse_github_mutation(op: &str) -> Result<(), String> {
    Err(format!(
        "github_corpus_ops is read-only; refusing GitHub mutation op `{op}`"
    ))
}

/// Read-only corpus fetch surface. No write methods exist on this trait.
pub trait GithubCorpusReader {
    fn read_issue_json(&self, repo: &str, issue_number: u64) -> Result<Value, String>;
    fn read_pr_json(&self, repo: &str, pr_number: u64) -> Result<Value, String>;
}

/// In-memory fixture reader keyed by `(repo, number)`.
#[derive(Debug, Default, Clone)]
pub struct FixtureCorpusReader {
    pub issues: std::collections::HashMap<(String, u64), Value>,
    pub prs: std::collections::HashMap<(String, u64), Value>,
}

impl FixtureCorpusReader {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert_issue(&mut self, repo: &str, number: u64, json: Value) {
        self.issues.insert((repo.to_string(), number), json);
    }

    pub fn insert_pr(&mut self, repo: &str, number: u64, json: Value) {
        self.prs.insert((repo.to_string(), number), json);
    }
}

impl GithubCorpusReader for FixtureCorpusReader {
    fn read_issue_json(&self, repo: &str, issue_number: u64) -> Result<Value, String> {
        self.issues
            .get(&(repo.to_string(), issue_number))
            .cloned()
            .ok_or_else(|| format!("fixture missing issue {repo}#{issue_number}"))
    }

    fn read_pr_json(&self, repo: &str, pr_number: u64) -> Result<Value, String> {
        self.prs
            .get(&(repo.to_string(), pr_number))
            .cloned()
            .ok_or_else(|| format!("fixture missing pr {repo}#{pr_number}"))
    }
}

/// Wrapper that records every call for mutation-refusal discrimination tests.
#[derive(Debug)]
pub struct MutationProbe<R> {
    inner: R,
    pub calls: std::sync::Mutex<Vec<String>>,
}

impl<R> MutationProbe<R> {
    pub fn new(inner: R) -> Self {
        Self {
            inner,
            calls: std::sync::Mutex::new(Vec::new()),
        }
    }

    pub fn recorded_calls(&self) -> Vec<String> {
        self.calls.lock().expect("calls lock").clone()
    }

    pub fn assert_no_mutations(&self) {
        let calls = self.recorded_calls();
        for call in &calls {
            for forbidden in FORBIDDEN_GITHUB_MUTATIONS {
                assert!(
                    !call.contains(forbidden),
                    "unexpected mutation verb `{forbidden}` in recorded call `{call}`"
                );
            }
            assert!(
                call.starts_with("read_issue:") || call.starts_with("read_pr:"),
                "only read_* calls are allowed; got `{call}`"
            );
        }
    }

    /// Public adapter surface for mutation attempts — always refuses.
    pub fn attempt_mutation(&self, op: &str) -> Result<(), String> {
        self.calls
            .lock()
            .expect("calls lock")
            .push(format!("mutation_attempt:{op}"));
        refuse_github_mutation(op)
    }
}

impl<R: GithubCorpusReader> GithubCorpusReader for MutationProbe<R> {
    fn read_issue_json(&self, repo: &str, issue_number: u64) -> Result<Value, String> {
        self.calls
            .lock()
            .expect("calls lock")
            .push(format!("read_issue:{repo}#{issue_number}"));
        self.inner.read_issue_json(repo, issue_number)
    }

    fn read_pr_json(&self, repo: &str, pr_number: u64) -> Result<Value, String> {
        self.calls
            .lock()
            .expect("calls lock")
            .push(format!("read_pr:{repo}#{pr_number}"));
        self.inner.read_pr_json(repo, pr_number)
    }
}

/// Build baseline IssueOpened / PrOpened events from **already-parsed**
/// snapshots. `revision_hash` is the real `*_snapshot_hash` — never a
/// fabricated `open-{n}` label.
pub fn baseline_events_from_snapshots(
    issue: &IssueSnapshotV1,
    pull_request: Option<&PullRequestSnapshotV1>,
    captured_at: &str,
) -> Vec<ProvenanceEventV1> {
    let mut events = vec![ProvenanceEventV1 {
        kind: ProvenanceEventKindV1::IssueOpened,
        revision_hash: issue.issue_snapshot_hash.clone(),
        target_ref: issue.issue_ref.clone(),
        occurred_at: captured_at.to_string(),
    }];
    if let Some(pr) = pull_request {
        events.push(ProvenanceEventV1 {
            kind: ProvenanceEventKindV1::PrOpened,
            revision_hash: pr.pr_snapshot_hash.clone(),
            target_ref: pr.pr_ref.clone(),
            occurred_at: captured_at.to_string(),
        });
    }
    events
}

/// Fetch a case bundle using ONLY [`GithubCorpusReader`] read methods.
///
/// `event_hints` is an ordered list of provenance hops already known to the
/// caller (no network event polling invented here). When empty, baseline
/// open events are derived from the real snapshot hashes of the fetched
/// content — never fabricated strings labeled as hashes.
pub fn fetch_case_bundle(
    reader: &dyn GithubCorpusReader,
    case: &CorpusCaseV1,
    event_hints: Vec<ProvenanceEventV1>,
    captured_at: &str,
) -> Result<CaseCorpusBundle, String> {
    let issue_json = reader.read_issue_json(&case.repo, case.issue_number)?;
    let issue = parse_issue_snapshot_from_gh_json(&case.repo, case.issue_number, &issue_json);
    let pull_request = match case.pr_number {
        Some(n) => {
            let json = reader.read_pr_json(&case.repo, n)?;
            Some(
                parse_pr_snapshot_from_gh_json(&case.repo, n, &json)
                    .map_err(|e: ParseError| e.to_string())?,
            )
        }
        None => None,
    };

    let events = if event_hints.is_empty() {
        baseline_events_from_snapshots(&issue, pull_request.as_ref(), captured_at)
    } else {
        event_hints
    };

    Ok(CaseCorpusBundle {
        case_id: case.case_id.clone(),
        issue,
        pull_request,
        events,
        captured_at: captured_at.to_string(),
    })
}
