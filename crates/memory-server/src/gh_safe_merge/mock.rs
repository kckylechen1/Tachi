use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::Mutex;

use super::{CheckRun, GhClient, GhError, IssueState, MergeResult, MergeStrategy, PrState};

/// In-memory fixture for integration-testing the `safe_merge` orchestrator
/// without spawning `gh`. Registered PRs/issues are returned verbatim;
/// `pr_merge` records the call and returns a deterministic SHA.
///
/// Builder-style: `MockGhClient::new().with_pr(...).with_checks(...)`.
/// Threadsafe via `Mutex` so the orchestrator's `&self` borrows compose.
pub struct MockGhClient {
    prs: Mutex<HashMap<(String, u64), PrState>>,
    issues: Mutex<HashMap<(String, u64), IssueState>>,
    checks: Mutex<HashMap<(String, u64), Vec<CheckRun>>>,
    merge_calls: Mutex<Vec<(String, u64, MergeStrategy, String)>>,
    next_issue_number: Mutex<u64>,
}

impl MockGhClient {
    pub fn new() -> Self {
        Self {
            prs: Mutex::new(HashMap::new()),
            issues: Mutex::new(HashMap::new()),
            checks: Mutex::new(HashMap::new()),
            merge_calls: Mutex::new(Vec::new()),
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
    pub fn merge_calls(&self) -> Vec<(String, u64, MergeStrategy, String)> {
        self.merge_calls.lock().unwrap().clone()
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
        self.prs
            .lock()
            .unwrap()
            .get(&(repo.to_string(), number))
            .cloned()
            .ok_or_else(|| GhError::NotFound(format!("pr {repo}#{number}")))
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
        Ok(self
            .checks
            .lock()
            .unwrap()
            .get(&(repo.to_string(), pr_number))
            .cloned()
            .unwrap_or_default())
    }
}
