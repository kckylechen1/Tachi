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
        Ok(self.pr_view_snapshot(repo, number).await?.pr)
    }

    async fn pr_view_snapshot(&self, repo: &str, number: u64) -> Result<PrViewSnapshot, GhError> {
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
        let mut pr = parse_pr_view_json(&v, checks.clone())
            .map_err(|e| GhError::Sanitized(format!("pr_view shape: {e}")))?;
        pr.closing_issue_labels = self
            .fetch_closing_issue_labels(repo, &pr.linked_issue_refs)
            .await;
        Ok(PrViewSnapshot {
            pr,
            check_runs: Some(checks),
        })
    }

    /// Head-only read: `gh pr view --json headRefOid`, without the checks and
    /// closing-issue label fan-out of `pr_view`. A missing field reads as an
    /// empty SHA (unknown), like `parse_pr_view_json`.
    async fn pr_head_sha(&self, repo: &str, number: u64) -> Result<String, GhError> {
        validate_repo(repo).map_err(GhError::Sanitized)?;
        let mut call = GhCall::read();
        call.args(["pr", "view", &number.to_string()])
            .args(["--repo", repo])
            .args(["--json", "headRefOid"]);
        let raw = call.run(self.server).await.map_err(gh_client_error)?;
        let v: serde_json::Value = serde_json::from_str(&raw)
            .map_err(|e| GhError::Sanitized(format!("pr_head_sha parse: {e}")))?;
        Ok(v.get("headRefOid")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string())
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
    /// Labels of every closing issue, in ONE `gh api graphql` read instead of
    /// one `gh issue view --json labels` per reference. Same lookup as before:
    /// the issue number from each reference, in `repo`, via
    /// `issueOrPullRequest` (what `gh issue view` resolves) with the first 100
    /// labels (what `gh issue view --json labels` returns).
    ///
    /// Fail-closed exactly per reference, as with the per-issue calls:
    /// `labels: None` when the reference does not parse, when the batched call
    /// fails outright (including a timeout), or when that issue's entry is
    /// missing from the response (e.g. GitHub reported it NOT_FOUND). `gh api`
    /// exits non-zero on any GraphQL error but still prints the partial
    /// `data`, so the other issues keep their successful lookups.
    async fn fetch_closing_issue_labels(
        &self,
        repo: &str,
        references: &[String],
    ) -> Vec<ClosingIssueLabels> {
        let numbers: Vec<Option<u64>> = references
            .iter()
            .map(|reference| issue_number_from_reference(reference))
            .collect();
        let mut unique: Vec<u64> = numbers.iter().flatten().copied().collect();
        unique.sort_unstable();
        unique.dedup();
        let labels_by_number = if unique.is_empty() {
            BTreeMap::new()
        } else {
            self.issue_labels_batch(repo, &unique).await
        };
        references
            .iter()
            .zip(numbers)
            .map(|(reference, number)| ClosingIssueLabels {
                reference: reference.clone(),
                labels: number.and_then(|number| labels_by_number.get(&number).cloned()),
            })
            .collect()
    }

    /// Successful label lookups by issue number; an absent key means the
    /// lookup for that issue failed.
    async fn issue_labels_batch(
        &self,
        repo: &str,
        issue_numbers: &[u64],
    ) -> BTreeMap<u64, Vec<String>> {
        let Some((owner, name)) = repo.split_once('/') else {
            return BTreeMap::new();
        };
        let mut call = GhCall::read();
        call.args(["api", "graphql"])
            .args([
                "-f",
                &format!("query={}", issue_labels_batch_query(issue_numbers)),
            ])
            .args(["-f", &format!("owner={owner}")])
            .args(["-f", &format!("name={name}")])
            .context("gh api graphql (closing-issue labels)");
        match call.output(self.server).await {
            Ok(output) => parse_issue_labels_batch_json(&output.sanitized_stdout(), issue_numbers),
            Err(_) => BTreeMap::new(),
        }
    }
}

/// One aliased field per issue number (`i<N>`). Numbers are parsed `u64`s,
/// so interpolating them cannot inject query text; owner/name are variables.
fn issue_labels_batch_query(issue_numbers: &[u64]) -> String {
    let mut query = String::from(
        "query($owner: String!, $name: String!) { repository(owner: $owner, name: $name) {",
    );
    for number in issue_numbers {
        query.push_str(&format!(
            " i{number}: issueOrPullRequest(number: {number}) {{ \
             ... on Issue {{ labels(first: 100) {{ nodes {{ name }} }} }} \
             ... on PullRequest {{ labels(first: 100) {{ nodes {{ name }} }} }} }}"
        ));
    }
    query.push_str(" } }");
    query
}

fn parse_issue_labels_batch_json(raw: &str, issue_numbers: &[u64]) -> BTreeMap<u64, Vec<String>> {
    let Ok(value) = serde_json::from_str::<Value>(raw) else {
        return BTreeMap::new();
    };
    let Some(repository) = value.pointer("/data/repository").filter(|v| v.is_object()) else {
        return BTreeMap::new();
    };
    issue_numbers
        .iter()
        .filter_map(|number| {
            let nodes = repository
                .get(format!("i{number}"))?
                .get("labels")?
                .get("nodes")?
                .as_array()?;
            let names = nodes
                .iter()
                .filter_map(|label| {
                    label
                        .get("name")
                        .and_then(Value::as_str)
                        .or_else(|| label.as_str())
                        .map(str::to_string)
                })
                .collect();
            Some((*number, names))
        })
        .collect()
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

#[cfg(test)]
mod pure_tests {
    use super::*;

    #[test]
    fn issue_labels_batch_query_aliases_each_number() {
        let query = issue_labels_batch_query(&[7, 21]);
        assert!(query.starts_with(
            "query($owner: String!, $name: String!) { repository(owner: $owner, name: $name) {"
        ));
        for number in [7, 21] {
            assert!(
                query.contains(&format!(
                    " i{number}: issueOrPullRequest(number: {number}) {{ ... on Issue {{ labels(first: 100) {{ nodes {{ name }} }} }} ... on PullRequest {{ labels(first: 100) {{ nodes {{ name }} }} }} }}"
                )),
                "{query}"
            );
        }
        assert!(query.ends_with(" } }"), "{query}");
    }

    #[test]
    fn issue_labels_batch_parse_keeps_per_issue_fail_closed() {
        // Partial data: #7 resolved, #8 errored (null), #9 absent entirely.
        let raw = r#"{"data":{"repository":{
            "i7":{"labels":{"nodes":[{"name":"bug"},{"name":"agent:no-close"}]}},
            "i8":null,
            "i10":{"labels":{"nodes":[]}}
        }},"errors":[{"message":"Could not resolve to an issue or pull request with the number of 8."}]}"#;
        let parsed = parse_issue_labels_batch_json(raw, &[7, 8, 9, 10]);
        assert_eq!(
            parsed.get(&7),
            Some(&vec!["bug".to_string(), "agent:no-close".to_string()])
        );
        assert_eq!(parsed.get(&8), None, "an errored issue must stay unknown");
        assert_eq!(parsed.get(&9), None, "a missing issue must stay unknown");
        assert_eq!(parsed.get(&10), Some(&Vec::new()), "no labels is a success");

        // Whole-call failures leave every issue unknown.
        assert!(parse_issue_labels_batch_json("", &[7]).is_empty());
        assert!(parse_issue_labels_batch_json("gh: HTTP 403", &[7]).is_empty());
        assert!(parse_issue_labels_batch_json(
            r#"{"data":{"repository":null},"errors":[{"message":"x"}]}"#,
            &[7]
        )
        .is_empty());
    }
}

#[cfg(all(test, unix))]
mod shim_tests {
    use super::*;
    use crate::gh_ops::safe_merge::CheckStateReader;
    use std::ffi::OsString;
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;

    struct PathEnvGuard(Option<OsString>);

    impl PathEnvGuard {
        fn prepend(dir: &Path) -> Self {
            let original = std::env::var_os("PATH");
            let mut paths = vec![dir.to_path_buf()];
            if let Some(value) = original.as_ref() {
                paths.extend(std::env::split_paths(value));
            }
            std::env::set_var("PATH", std::env::join_paths(paths).expect("join PATH"));
            Self(original)
        }
    }

    impl Drop for PathEnvGuard {
        fn drop(&mut self) {
            match self.0.as_ref() {
                Some(path) => std::env::set_var("PATH", path),
                None => std::env::remove_var("PATH"),
            }
        }
    }

    /// Logging `gh` shim: a PR with closing issues #100 and #101, where the
    /// batched label read resolves #100 and reports #101 as an error.
    fn install_logging_gh(dir: &Path, log: &Path) {
        let script = format!(
            r#"#!/bin/sh
echo "$*" >> '{log}'
case "$1 $2" in
  "pr view")
    case "$*" in
      *closingIssuesReferences*) echo '{{"number":42,"state":"OPEN","mergeable":"MERGEABLE","reviewDecision":"APPROVED","isDraft":false,"headRefOid":"HEADSHA1","headRefName":"feat/x","closingIssuesReferences":[{{"number":100,"url":"https://github.com/o/r/issues/100"}},{{"number":101,"url":"https://github.com/o/r/issues/101"}},{{"url":"not-a-ref"}}]}}' ;;
      *headRefOid*) echo '{{"headRefOid":"HEADSHA2"}}' ;;
      *) echo "unexpected pr view: $*" >&2; exit 3 ;;
    esac ;;
  "pr checks") echo '[{{"name":"ci","state":"SUCCESS","bucket":"pass"}}]' ;;
  "api graphql")
    echo '{{"data":{{"repository":{{"i100":{{"labels":{{"nodes":[{{"name":"agent:no-close"}}]}}}},"i101":null}}}},"errors":[{{"message":"Could not resolve"}}]}}'
    exit 1 ;;
  *) echo "unexpected gh call: $*" >&2; exit 3 ;;
esac
"#,
            log = log.display()
        );
        let path = dir.join("gh");
        std::fs::write(&path, script).expect("write gh shim");
        let mut permissions = std::fs::metadata(&path).expect("meta").permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&path, permissions).expect("chmod gh shim");
    }

    fn logged_calls(log: &Path) -> Vec<String> {
        std::fs::read_to_string(log)
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test runtime")
    }

    #[test]
    fn check_state_poll_with_expected_head_reads_checks_then_head_only() {
        let _lock = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let bin = tempfile::tempdir().expect("bin dir");
        let log = bin.path().join("gh.log");
        install_logging_gh(bin.path(), &log);
        let _path = PathEnvGuard::prepend(bin.path());
        let server = crate::tests::make_server();
        let client = CliGhClient { server: &server };

        let read = runtime()
            .block_on(CheckStateReader::read_check_state(
                &client,
                "o/r",
                42,
                Some("HEADSHA1"),
            ))
            .expect("check-state read");

        assert_eq!(read.observed_head_sha.as_deref(), Some("HEADSHA2"));
        assert_eq!(read.checks.len(), 1);
        assert_eq!(
            logged_calls(&log),
            vec![
                "pr checks 42 --repo o/r --json name,state,bucket".to_string(),
                "pr view 42 --repo o/r --json headRefOid".to_string(),
            ],
            "one poll = the check list plus a head-only read; no full pr_view fan-out"
        );
    }

    #[test]
    fn pr_view_snapshot_batches_closing_issue_labels_and_keeps_check_runs() {
        let _lock = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let bin = tempfile::tempdir().expect("bin dir");
        let log = bin.path().join("gh.log");
        install_logging_gh(bin.path(), &log);
        let _path = PathEnvGuard::prepend(bin.path());
        let server = crate::tests::make_server();
        let client = CliGhClient { server: &server };

        let snapshot = runtime()
            .block_on(client.pr_view_snapshot("o/r", 42))
            .expect("pr_view_snapshot");

        assert_eq!(snapshot.pr.head_sha, "HEADSHA1");
        assert_eq!(snapshot.pr.checks, ChecksState::Success);
        assert_eq!(
            snapshot.check_runs.as_ref().map(Vec::len),
            Some(1),
            "the raw check runs behind pr.checks are returned for reuse"
        );
        assert_eq!(
            snapshot.pr.closing_issue_labels,
            vec![
                ClosingIssueLabels {
                    reference: "https://github.com/o/r/issues/100".to_string(),
                    labels: Some(vec!["agent:no-close".to_string()]),
                },
                ClosingIssueLabels {
                    reference: "https://github.com/o/r/issues/101".to_string(),
                    labels: None,
                },
                ClosingIssueLabels {
                    reference: "not-a-ref".to_string(),
                    labels: None,
                },
            ],
            "per-reference fail-closed results are unchanged by batching"
        );
        let calls = logged_calls(&log);
        assert_eq!(calls.len(), 3, "{calls:#?}");
        assert!(calls[0].starts_with("pr view 42 --repo o/r --json "));
        assert_eq!(calls[1], "pr checks 42 --repo o/r --json name,state,bucket");
        assert!(
            calls[2].starts_with("api graphql -f query="),
            "{}",
            calls[2]
        );
        assert!(calls[2].contains("i100: issueOrPullRequest(number: 100)"));
        assert!(calls[2].contains("i101: issueOrPullRequest(number: 101)"));
        assert!(calls[2].ends_with("-f owner=o -f name=r"), "{}", calls[2]);
    }
}
