use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::Mutex;

use super::{
    CheckRun, ClosingIssueLabels, GhClient, GhError, IssueState, MergeResult, MergeStrategy,
    PrState,
};

/// In-memory fixture for integration-testing the `safe_merge` orchestrator
/// without spawning `gh`. Registered PRs/issues are returned verbatim;
/// `pr_merge` records the call and returns a deterministic SHA.
///
/// Builder-style: `MockGhClient::new().with_pr(...).with_checks(...)`.
/// Threadsafe via `Mutex` so the orchestrator's `&self` borrows compose.
pub struct MockGhClient {
    prs: Mutex<HashMap<(String, u64), PrState>>,
    issues: Mutex<HashMap<(String, u64), IssueState>>,
    issue_labels: Mutex<HashMap<(String, u64), Vec<String>>>,
    checks: Mutex<HashMap<(String, u64), Vec<CheckRun>>>,
    checks_list_error: Mutex<Option<GhError>>,
    merge_calls: Mutex<Vec<(String, u64, MergeStrategy, String)>>,
    /// Recorded `pr_view` calls so tests can prove the blanket
    /// `CheckStateReader` gate (only fetch head SHA when there's an expected
    /// SHA to compare against) actually skips `pr_view` on a first poll.
    pr_view_calls: Mutex<Vec<(String, u64)>>,
    next_issue_number: Mutex<u64>,
}

impl MockGhClient {
    pub fn new() -> Self {
        Self {
            prs: Mutex::new(HashMap::new()),
            issues: Mutex::new(HashMap::new()),
            issue_labels: Mutex::new(HashMap::new()),
            checks: Mutex::new(HashMap::new()),
            checks_list_error: Mutex::new(None),
            merge_calls: Mutex::new(Vec::new()),
            pr_view_calls: Mutex::new(Vec::new()),
            next_issue_number: Mutex::new(1000),
        }
    }
    pub fn with_pr(self, repo: &str, pr: PrState) -> Self {
        self.prs
            .lock()
            .unwrap()
            .insert((repo.to_string(), pr.number), pr);
        self
    }
    pub fn with_checks(self, repo: &str, pr_number: u64, runs: Vec<CheckRun>) -> Self {
        self.checks
            .lock()
            .unwrap()
            .insert((repo.to_string(), pr_number), runs);
        self
    }
    /// Force `checks_list` to return this error instead of reading the
    /// registered check list, exercising the degraded-input error path.
    pub fn with_checks_list_error(self, error: GhError) -> Self {
        *self.checks_list_error.lock().unwrap() = Some(error);
        self
    }
    pub fn with_issue_labels(self, repo: &str, issue_number: u64, labels: Vec<&str>) -> Self {
        self.issue_labels.lock().unwrap().insert(
            (repo.to_string(), issue_number),
            labels.into_iter().map(str::to_string).collect(),
        );
        self
    }
    pub fn merge_calls(&self) -> Vec<(String, u64, MergeStrategy, String)> {
        self.merge_calls.lock().unwrap().clone()
    }
    /// Snapshot of recorded `pr_view` calls, in call order. Used to prove the
    /// blanket `CheckStateReader` gate skips `pr_view` on a first poll (no
    /// expected head SHA) and calls it on subsequent polls.
    pub fn pr_view_calls(&self) -> Vec<(String, u64)> {
        self.pr_view_calls.lock().unwrap().clone()
    }

    pub async fn issue_view(&self, repo: &str, number: u64) -> Result<IssueState, GhError> {
        self.issues
            .lock()
            .unwrap()
            .get(&(repo.to_string(), number))
            .cloned()
            .ok_or_else(|| GhError::NotFound(format!("issue {repo}#{number}")))
    }
}

#[async_trait]
impl GhClient for MockGhClient {
    async fn pr_view(&self, repo: &str, number: u64) -> Result<PrState, GhError> {
        // Record the call before any state lookup so tests can assert the
        // blanket reader gate did NOT skip into the call.
        self.pr_view_calls
            .lock()
            .unwrap()
            .push((repo.to_string(), number));
        let mut pr = self
            .prs
            .lock()
            .unwrap()
            .get(&(repo.to_string(), number))
            .cloned()
            .ok_or_else(|| GhError::NotFound(format!("pr {repo}#{number}")))?;
        if pr.closing_issue_labels.is_empty() {
            let labels = self.issue_labels.lock().unwrap();
            pr.closing_issue_labels = pr
                .linked_issue_refs
                .iter()
                .map(|reference| {
                    // Mock a SUCCESSFUL gh label lookup: registered labels, or an
                    // empty set when none are registered → always `Some`. The
                    // fetch-FAILURE (`None` → fail-closed) path is exercised by the
                    // gate's direct-construction goldens, not through this mock.
                    let issue_labels = Some(
                        mock_issue_number_from_reference(reference)
                            .and_then(|issue_number| {
                                labels.get(&(repo.to_string(), issue_number)).cloned()
                            })
                            .unwrap_or_default(),
                    );
                    ClosingIssueLabels {
                        reference: reference.clone(),
                        labels: issue_labels,
                    }
                })
                .collect();
        }
        Ok(pr)
    }
    async fn pr_merge(
        &self,
        repo: &str,
        number: u64,
        strategy: MergeStrategy,
        expected_head_sha: &str,
    ) -> Result<MergeResult, GhError> {
        // Record call before any state lookup so tests can assert the
        // orchestrator did NOT short-circuit before reaching merge.
        self.merge_calls.lock().unwrap().push((
            repo.to_string(),
            number,
            strategy,
            expected_head_sha.to_string(),
        ));
        let pr = self
            .prs
            .lock()
            .unwrap()
            .get(&(repo.to_string(), number))
            .cloned()
            .ok_or_else(|| GhError::NotFound(format!("pr {repo}#{number}")))?;
        if !expected_head_sha.is_empty() && pr.head_sha != expected_head_sha {
            return Err(GhError::Sanitized(format!(
                "head SHA changed: expected {expected_head_sha}, got {}",
                pr.head_sha
            )));
        }
        Ok(MergeResult {
            pr_number: number,
            merge_sha: format!("mock-sha-for-{}", pr.head_sha),
            strategy,
        })
    }
    async fn issue_create(
        &self,
        repo: &str,
        title: &str,
        _body: Option<&str>,
        _labels: &[String],
    ) -> Result<IssueState, GhError> {
        let mut next = self.next_issue_number.lock().unwrap();
        let number = *next;
        *next += 1;
        let issue = IssueState {
            number,
            title: title.to_string(),
            state: "open".to_string(),
            url: format!("https://github.com/{repo}/issues/{number}"),
        };
        self.issues
            .lock()
            .unwrap()
            .insert((repo.to_string(), number), issue.clone());
        Ok(issue)
    }
    async fn checks_list(&self, repo: &str, pr_number: u64) -> Result<Vec<CheckRun>, GhError> {
        if let Some(error) = self.checks_list_error.lock().unwrap().clone() {
            return Err(error);
        }
        Ok(self
            .checks
            .lock()
            .unwrap()
            .get(&(repo.to_string(), pr_number))
            .cloned()
            .unwrap_or_default())
    }
}

fn mock_issue_number_from_reference(reference: &str) -> Option<u64> {
    let trimmed = reference.trim();
    if let Some(number) = trimmed.strip_prefix('#') {
        return number.parse::<u64>().ok();
    }
    trimmed
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .and_then(|tail| tail.parse::<u64>().ok())
}
