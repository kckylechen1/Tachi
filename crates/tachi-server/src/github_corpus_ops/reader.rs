//! Read-only GitHub corpus reader surface (#1059).
//!
//! The trait exposes ONLY `read_*` methods. Mutation verbs are refused by
//! [`refuse_github_mutation`]; there is no write path on this adapter.

use serde_json::Value;

use super::parse::{
    assemble_case_bundle, CaseCorpusBundle, ProvenanceEventKindV1, ProvenanceEventV1,
};
use super::pilot::CorpusCaseV1;

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

/// Fetch a case bundle using ONLY [`GithubCorpusReader`] read methods.
///
/// `event_hints` is an ordered list of provenance hops already known to the
/// caller (no network event polling invented here).
pub fn fetch_case_bundle(
    reader: &dyn GithubCorpusReader,
    case: &CorpusCaseV1,
    event_hints: Vec<ProvenanceEventV1>,
    captured_at: &str,
) -> Result<CaseCorpusBundle, String> {
    let issue_json = reader.read_issue_json(&case.repo, case.issue_number)?;
    let pr = match case.pr_number {
        Some(n) => {
            let json = reader.read_pr_json(&case.repo, n)?;
            Some((n, json))
        }
        None => None,
    };
    let pr_ref = pr.as_ref().map(|(n, json)| (*n, json));
    Ok(assemble_case_bundle(
        &case.case_id,
        &case.repo,
        case.issue_number,
        &issue_json,
        pr_ref,
        ensure_baseline_events(case, &event_hints, captured_at),
        captured_at,
    ))
}

fn ensure_baseline_events(
    case: &CorpusCaseV1,
    hints: &[ProvenanceEventV1],
    captured_at: &str,
) -> Vec<ProvenanceEventV1> {
    if !hints.is_empty() {
        return hints.to_vec();
    }
    // Minimal baseline when caller supplies no hints: issue opened (+ optional PR).
    let mut events = vec![ProvenanceEventV1 {
        kind: ProvenanceEventKindV1::IssueOpened,
        revision_hash: format!("open-{}", case.issue_number),
        target_ref: format!("{}#{}", case.repo, case.issue_number),
        occurred_at: captured_at.to_string(),
    }];
    if let Some(pr) = case.pr_number {
        events.push(ProvenanceEventV1 {
            kind: ProvenanceEventKindV1::PrOpened,
            revision_hash: format!("pr-open-{pr}"),
            target_ref: format!("{}#{pr}", case.repo),
            occurred_at: captured_at.to_string(),
        });
    }
    events
}
