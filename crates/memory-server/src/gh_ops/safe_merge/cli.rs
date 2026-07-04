use super::*;

pub(crate) struct CliGhClient<'a> {
    pub(crate) server: &'a MemoryServer,
}

impl<'a> CliGhClient<'a> {
    fn build(&self) -> Result<(Command, String), GhError> {
        build_gh_command(self.server).map_err(|e| GhError::Sanitized(e))
    }
}

#[async_trait]
impl<'a> GhClient for CliGhClient<'a> {
    async fn pr_view(&self, repo: &str, number: u64) -> Result<PrState, GhError> {
        validate_repo(repo).map_err(GhError::Sanitized)?;
        let (mut cmd, token) = self.build()?;
        cmd.args(["pr", "view", &number.to_string()])
            .args(["--repo", repo])
            .args([
                "--json",
                "number,state,mergeable,reviewDecision,isDraft,headRefOid,closingIssuesReferences",
            ]);
        let raw = run_gh(cmd, &token).map_err(|e| classify_gh_error(&e))?;
        let v: serde_json::Value = serde_json::from_str(&raw)
            .map_err(|e| GhError::Sanitized(format!("pr_view parse: {e}")))?;
        let checks = self.checks_list(repo, number).await?;
        let mut pr = parse_pr_view_json(&v, checks)
            .map_err(|e| GhError::Sanitized(format!("pr_view shape: {e}")))?;
        pr.closing_issue_labels = self.fetch_closing_issue_labels(repo, &pr.linked_issue_refs);
        Ok(pr)
    }

    async fn pr_merge(
        &self,
        repo: &str,
        number: u64,
        strategy: MergeStrategy,
        expected_head_sha: &str,
    ) -> Result<MergeResult, GhError> {
        validate_repo(repo).map_err(GhError::Sanitized)?;
        let (mut cmd, token) = self.build()?;
        cmd.args(["pr", "merge", &number.to_string()])
            .args(["--repo", repo])
            .arg(merge_strategy_flag(strategy));
        if !expected_head_sha.trim().is_empty() {
            cmd.args(["--match-head-commit", expected_head_sha]);
        }
        let _out = run_gh(cmd, &token).map_err(|e| classify_gh_error(&e))?;
        // gh pr merge prints a status line, not JSON. Re-fetch the merged SHA.
        let (mut cmd2, token2) = self.build()?;
        cmd2.args(["pr", "view", &number.to_string()])
            .args(["--repo", repo])
            .args(["--json", "mergeCommit"]);
        let sha_raw = run_gh(cmd2, &token2).map_err(|e| classify_gh_error(&e))?;
        let merge_sha = serde_json::from_str::<serde_json::Value>(&sha_raw)
            .ok()
            .and_then(|v| {
                v.get("mergeCommit")
                    .and_then(|mc| mc.get("oid"))
                    .and_then(|o| o.as_str())
                    .map(|s| s.to_string())
            })
            .unwrap_or_default();
        Ok(MergeResult {
            pr_number: number,
            merge_sha,
            strategy,
        })
    }

    async fn issue_create(
        &self,
        repo: &str,
        title: &str,
        body: Option<&str>,
        labels: &[String],
    ) -> Result<crate::gh_safe_merge::IssueState, GhError> {
        validate_repo(repo).map_err(GhError::Sanitized)?;
        let (mut cmd, token) = self.build()?;
        cmd.args(["issue", "create"])
            .args(["--repo", repo])
            .args(["--title", title]);
        if let Some(b) = body {
            cmd.args(["--body", b]);
        }
        for l in labels {
            cmd.args(["--label", l]);
        }
        let url = run_gh(cmd, &token)
            .map_err(|e| classify_gh_error(&e))?
            .trim()
            .to_string();
        // `gh issue create` prints the issue URL; derive the number from the trailing path segment.
        let number = url
            .rsplit('/')
            .next()
            .and_then(|s| s.parse::<u64>().ok())
            .ok_or_else(|| {
                GhError::Sanitized(format!(
                    "gh issue create returned an unparseable issue URL: {url}"
                ))
            })?;
        Ok(crate::gh_safe_merge::IssueState {
            number,
            title: title.to_string(),
            state: "OPEN".to_string(),
            url,
        })
    }

    async fn checks_list(
        &self,
        repo: &str,
        pr_number: u64,
    ) -> Result<Vec<crate::gh_safe_merge::CheckRun>, GhError> {
        validate_repo(repo).map_err(GhError::Sanitized)?;
        // `gh pr checks` may exit non-zero when checks have failed; we still
        // want to parse the JSON. Run it directly and tolerate non-zero exit
        // when stdout looks like a JSON array.
        let (mut raw_cmd, token) = self.build()?;
        raw_cmd
            .args(["pr", "checks", &pr_number.to_string()])
            .args(["--repo", repo])
            .arg("--json")
            .arg("name,state,bucket");
        let output = raw_cmd
            .output()
            .map_err(|e| GhError::Sanitized(format!("gh exec: {e}")))?;
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        let sanitized = sanitize_output(&stdout, &token);
        let sanitized_stderr = sanitize_output(&stderr, &token);
        let trimmed = sanitized.trim();
        if trimmed.is_empty() || trimmed == "null" {
            if output.status.success() {
                return Ok(Vec::new());
            }
            if is_no_checks_reported(&sanitized_stderr) {
                return Ok(Vec::new());
            }
            return Err(classify_gh_error(&format!(
                "gh pr checks failed: {}",
                sanitized_stderr.trim()
            )));
        }
        if !trimmed.starts_with('[') {
            return Err(classify_gh_error(&format!(
                "gh pr checks returned non-json output: {} {}",
                trimmed,
                sanitized_stderr.trim()
            )));
        }
        let arr: Vec<serde_json::Value> = serde_json::from_str(trimmed)
            .map_err(|e| GhError::Sanitized(format!("checks_list parse: {e}")))?;
        Ok(arr
            .into_iter()
            .map(|v| {
                let name = v
                    .get("name")
                    .and_then(|n| n.as_str())
                    .unwrap_or_default()
                    .to_string();
                // `gh pr checks --json` exposes `bucket` ∈ pass/fail/pending/skipping/cancel
                // and `state` for the raw check status. We map bucket → conclusion
                // and synthesize a `completed`/`in_progress` status.
                let bucket = v
                    .get("bucket")
                    .and_then(|b| b.as_str())
                    .unwrap_or("")
                    .to_string();
                let (status, conclusion) = match bucket.as_str() {
                    "pass" => ("completed".to_string(), Some("success".to_string())),
                    "fail" => ("completed".to_string(), Some("failure".to_string())),
                    "cancel" => ("completed".to_string(), Some("cancelled".to_string())),
                    "skipping" => ("completed".to_string(), Some("skipped".to_string())),
                    "pending" | "" => ("in_progress".to_string(), None),
                    _ => ("completed".to_string(), Some(bucket.clone())),
                };
                crate::gh_safe_merge::CheckRun {
                    name,
                    conclusion,
                    status,
                }
            })
            .collect())
    }
}

impl<'a> CliGhClient<'a> {
    fn fetch_closing_issue_labels(
        &self,
        repo: &str,
        references: &[String],
    ) -> Vec<ClosingIssueLabels> {
        references
            .iter()
            .map(|reference| {
                let labels = issue_number_from_reference(reference)
                    .and_then(|issue_number| self.issue_labels(repo, issue_number).ok())
                    .unwrap_or_default();
                ClosingIssueLabels {
                    reference: reference.clone(),
                    labels,
                }
            })
            .collect()
    }

    fn issue_labels(&self, repo: &str, issue_number: u64) -> Result<Vec<String>, GhError> {
        let (mut cmd, token) = self.build()?;
        cmd.args(["issue", "view", &issue_number.to_string()])
            .args(["--repo", repo])
            .args(["--json", "labels"]);
        let raw = run_gh(cmd, &token).map_err(|e| classify_gh_error(&e))?;
        parse_issue_labels_json(&raw)
            .map_err(|e| GhError::Sanitized(format!("issue_labels parse: {e}")))
    }
}

fn issue_number_from_reference(reference: &str) -> Option<u64> {
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

fn parse_issue_labels_json(raw: &str) -> Result<Vec<String>, String> {
    let value: serde_json::Value =
        serde_json::from_str(raw).map_err(|e| format!("invalid json: {e}"))?;
    Ok(value
        .get("labels")
        .and_then(Value::as_array)
        .map(|labels| {
            labels
                .iter()
                .filter_map(|label| {
                    label
                        .get("name")
                        .and_then(Value::as_str)
                        .or_else(|| label.as_str())
                        .map(str::to_string)
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default())
}
