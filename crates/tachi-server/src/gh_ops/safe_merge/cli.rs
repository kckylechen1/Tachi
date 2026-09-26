use super::*;

pub(crate) struct CliGhClient<'a> {
    pub(crate) server: &'a MemoryServer,
}

/// Map a bounded-call failure onto the safe-merge error surface. Preparation
/// failures and timeouts are never reclassified by substring (a missing `gh`
/// binary is not a missing PR; a timed-out mutation is UNKNOWN, not failed).
fn gh_client_error(error: GhRunError) -> GhError {
    match error {
        GhRunError::Prepare(_) | GhRunError::TimedOut { .. } => {
            GhError::Sanitized(error.to_string())
        }
        GhRunError::Execute(_) | GhRunError::Exit { .. } => classify_gh_error(&error.to_string()),
    }
}

#[async_trait]
impl<'a> GhClient for CliGhClient<'a> {
    async fn pr_view(&self, repo: &str, number: u64) -> Result<PrState, GhError> {
        validate_repo(repo).map_err(GhError::Sanitized)?;
        let mut call = GhCall::read();
        call.args(["pr", "view", &number.to_string()])
            .args(["--repo", repo])
            .args([
                "--json",
                "number,state,mergeable,reviewDecision,isDraft,headRefOid,headRefName,closingIssuesReferences",
            ]);
        let raw = call.run(self.server).await.map_err(gh_client_error)?;
        let v: serde_json::Value = serde_json::from_str(&raw)
            .map_err(|e| GhError::Sanitized(format!("pr_view parse: {e}")))?;
        let checks = self.checks_list(repo, number).await?;
        let mut pr = parse_pr_view_json(&v, checks)
            .map_err(|e| GhError::Sanitized(format!("pr_view shape: {e}")))?;
        pr.closing_issue_labels = self
            .fetch_closing_issue_labels(repo, &pr.linked_issue_refs)
            .await;
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
        let mut merge = GhCall::mutation();
        merge
            .args(["pr", "merge", &number.to_string()])
            .args(["--repo", repo])
            .arg(merge_strategy_flag(strategy));
        if !expected_head_sha.trim().is_empty() {
            merge.args(["--match-head-commit", expected_head_sha]);
        }
        // A timeout here is surfaced as outcome UNKNOWN (never retried); the
        // verification read below only runs after a completed merge call.
        let _out = merge.run(self.server).await.map_err(gh_client_error)?;
        // gh pr merge prints a status line, not JSON. Re-fetch the merged SHA.
        let mut verify = GhCall::read();
        verify
            .args(["pr", "view", &number.to_string()])
            .args(["--repo", repo])
            .args(["--json", "mergeCommit"]);
        let sha_raw = verify.run(self.server).await.map_err(gh_client_error)?;
        // Independent proof of merge, symmetric to the HTTP path: a present,
        // non-empty mergeCommit.oid is the only evidence the PR actually merged.
        // An empty/missing oid means the re-fetch could not confirm the merge —
        // fail closed rather than reporting success with an empty sha.
        let merge_sha = serde_json::from_str::<serde_json::Value>(&sha_raw)
            .ok()
            .and_then(|v| {
                v.get("mergeCommit")
                    .and_then(|mc| mc.get("oid"))
                    .and_then(|o| o.as_str())
                    .map(|s| s.to_string())
            })
            .filter(|sha| !sha.trim().is_empty())
            .ok_or_else(|| {
                GhError::Sanitized(
                    "independent merge verification failed: missing mergeCommit".to_string(),
                )
            })?;
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
    ) -> Result<tachi_gh_safe_merge::IssueState, GhError> {
        validate_repo(repo).map_err(GhError::Sanitized)?;
        let mut call = GhCall::mutation();
        call.args(["issue", "create"])
            .args(["--repo", repo])
            .args(["--title", title]);
        if let Some(b) = body {
            call.args(["--body", b]);
        }
        for l in labels {
            call.args(["--label", l]);
        }
        let url = call
            .run(self.server)
            .await
            .map_err(gh_client_error)?
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
        Ok(tachi_gh_safe_merge::IssueState {
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
    ) -> Result<Vec<tachi_gh_safe_merge::CheckRun>, GhError> {
        validate_repo(repo).map_err(GhError::Sanitized)?;
        // `gh pr checks` may exit non-zero when checks have failed; we still
        // want to parse the JSON. Run it directly and tolerate non-zero exit
        // when stdout looks like a JSON array.
        let mut call = GhCall::read();
        call.args(["pr", "checks", &pr_number.to_string()])
            .args(["--repo", repo])
            .arg("--json")
            .arg("name,state,bucket");
        let output = call
            .output(self.server)
            .await
            .map_err(|error| match error {
                GhRunError::Execute(reason) => GhError::Sanitized(format!("gh exec: {reason}")),
                other => gh_client_error(other),
            })?;
        let sanitized = output.sanitized_stdout();
        let sanitized_stderr = output.sanitized_stderr();
        let trimmed = sanitized.trim();
        if trimmed.is_empty() || trimmed == "null" {
            if output.success() {
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
                tachi_gh_safe_merge::CheckRun {
                    name,
                    conclusion,
                    status,
                }
            })
            .collect())
    }
}

impl<'a> CliGhClient<'a> {
    async fn fetch_closing_issue_labels(
        &self,
        repo: &str,
        references: &[String],
    ) -> Vec<ClosingIssueLabels> {
        let mut closing = Vec::with_capacity(references.len());
        for reference in references {
            // `None` when the ref can't be parsed or the label lookup errored
            // (including a timeout) → the gate fails CLOSED on this issue.
            // `Some(vec)` (incl. empty) means the lookup succeeded.
            let labels = match issue_number_from_reference(reference) {
                Some(issue_number) => self.issue_labels(repo, issue_number).await.ok(),
                None => None,
            };
            closing.push(ClosingIssueLabels {
                reference: reference.clone(),
                labels,
            });
        }
        closing
    }

    async fn issue_labels(&self, repo: &str, issue_number: u64) -> Result<Vec<String>, GhError> {
        let mut call = GhCall::read();
        call.args(["issue", "view", &issue_number.to_string()])
            .args(["--repo", repo])
            .args(["--json", "labels"]);
        let raw = call.run(self.server).await.map_err(gh_client_error)?;
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
