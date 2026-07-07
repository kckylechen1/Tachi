use super::*;
use crate::gh_safe_merge::{CheckRun, IssueState};
use reqwest::{StatusCode, Url};
use serde_json::Value;

const DEFAULT_GITHUB_API_BASE: &str = "https://api.github.com";
const DEFAULT_GITHUB_GRAPHQL_URL: &str = "https://api.github.com/graphql";
const MAX_HTTP_ERROR_CHARS: usize = 1_000;

pub(crate) enum SelectedGhClient<'a> {
    Cli(CliGhClient<'a>),
    Http(HttpGhClient),
}

pub(crate) fn gh_client_for_server(server: &MemoryServer) -> Result<SelectedGhClient<'_>, String> {
    match gh_transport_mode().as_deref() {
        Some("http") | Some("rest") | Some("native") => {
            HttpGhClient::new(server).map(SelectedGhClient::Http)
        }
        Some("cli") | None => Ok(SelectedGhClient::Cli(CliGhClient { server })),
        Some(other) => Err(format!(
            "Invalid TACHI_GH_TRANSPORT value '{other}'. Expected 'cli' or 'http'."
        )),
    }
}

fn gh_transport_mode() -> Option<String> {
    std::env::var("TACHI_GH_TRANSPORT")
        .ok()
        .map(|raw| raw.trim().to_ascii_lowercase())
        .filter(|raw| !raw.is_empty())
}

#[async_trait]
impl<'a> GhClient for SelectedGhClient<'a> {
    async fn pr_view(&self, repo: &str, number: u64) -> Result<PrState, GhError> {
        match self {
            SelectedGhClient::Cli(client) => client.pr_view(repo, number).await,
            SelectedGhClient::Http(client) => client.pr_view(repo, number).await,
        }
    }

    async fn pr_merge(
        &self,
        repo: &str,
        number: u64,
        strategy: MergeStrategy,
        expected_head_sha: &str,
    ) -> Result<MergeResult, GhError> {
        match self {
            SelectedGhClient::Cli(client) => {
                client
                    .pr_merge(repo, number, strategy, expected_head_sha)
                    .await
            }
            SelectedGhClient::Http(client) => {
                client
                    .pr_merge(repo, number, strategy, expected_head_sha)
                    .await
            }
        }
    }

    async fn issue_create(
        &self,
        repo: &str,
        title: &str,
        body: Option<&str>,
        labels: &[String],
    ) -> Result<IssueState, GhError> {
        match self {
            SelectedGhClient::Cli(client) => client.issue_create(repo, title, body, labels).await,
            SelectedGhClient::Http(client) => client.issue_create(repo, title, body, labels).await,
        }
    }

    async fn checks_list(&self, repo: &str, pr_number: u64) -> Result<Vec<CheckRun>, GhError> {
        match self {
            SelectedGhClient::Cli(client) => client.checks_list(repo, pr_number).await,
            SelectedGhClient::Http(client) => client.checks_list(repo, pr_number).await,
        }
    }
}

pub(crate) struct HttpGhClient {
    http: reqwest::Client,
    token: String,
    api_base: String,
    graphql_url: String,
}

impl HttpGhClient {
    fn new(server: &MemoryServer) -> Result<Self, String> {
        let token = resolve_gh_token(server)?
            .map(|token| token.trim().to_string())
            .filter(|token| !token.is_empty())
            .ok_or_else(|| {
                "TACHI_GH_TRANSPORT=http requires GH_TOKEN in Vault, GH_TOKEN, or GITHUB_TOKEN"
                    .to_string()
            })?;
        Self::with_token(token)
    }

    fn with_token(token: String) -> Result<Self, String> {
        let api_base = std::env::var("TACHI_GITHUB_API_BASE_URL")
            .ok()
            .map(|raw| raw.trim().trim_end_matches('/').to_string())
            .filter(|raw| !raw.is_empty())
            .unwrap_or_else(|| DEFAULT_GITHUB_API_BASE.to_string());
        let graphql_url = std::env::var("TACHI_GITHUB_GRAPHQL_URL")
            .ok()
            .map(|raw| raw.trim().to_string())
            .filter(|raw| !raw.is_empty())
            .unwrap_or_else(|| {
                if api_base == DEFAULT_GITHUB_API_BASE {
                    DEFAULT_GITHUB_GRAPHQL_URL.to_string()
                } else {
                    format!("{api_base}/graphql")
                }
            });
        let http = reqwest::Client::builder()
            .user_agent(format!("tachi/{}", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|err| format!("build GitHub HTTP client: {err}"))?;
        Ok(Self {
            http,
            token,
            api_base,
            graphql_url,
        })
    }

    #[cfg(test)]
    fn for_tests(api_base: String, graphql_url: String) -> Self {
        Self {
            http: reqwest::Client::new(),
            token: "test-token".to_string(),
            api_base,
            graphql_url,
        }
    }

    fn rest_url(&self, path: &str) -> String {
        format!("{}{}", self.api_base, path)
    }

    fn request(&self, method: reqwest::Method, url: String) -> reqwest::RequestBuilder {
        self.http
            .request(method, url)
            .bearer_auth(&self.token)
            .header(reqwest::header::ACCEPT, "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
    }

    async fn send_json(&self, request: reqwest::RequestBuilder) -> Result<Value, GhError> {
        let (value, _next_url) = self.send_json_page(request).await?;
        Ok(value)
    }

    async fn send_json_page(
        &self,
        request: reqwest::RequestBuilder,
    ) -> Result<(Value, Option<String>), GhError> {
        let response = request
            .send()
            .await
            .map_err(|err| GhError::Sanitized(format!("GitHub HTTP request failed: {err}")))?;
        let status = response.status();
        let next_url = parse_next_link(response.headers().get(reqwest::header::LINK));
        let body = response
            .text()
            .await
            .map_err(|err| GhError::Sanitized(format!("GitHub HTTP body read failed: {err}")))?;
        if !status.is_success() {
            return Err(classify_http_error(
                status,
                &sanitize_output(&body, &self.token),
            ));
        }
        let value = if body.trim().is_empty() {
            Value::Null
        } else {
            serde_json::from_str(&body).map_err(|err| {
                GhError::Sanitized(format!("GitHub HTTP JSON parse failed: {err}"))
            })?
        };
        Ok((value, next_url))
    }

    async fn graphql(&self, query: &str, variables: Value) -> Result<Value, GhError> {
        let value = self
            .send_json(
                self.request(reqwest::Method::POST, self.graphql_url.clone())
                    .json(&json!({
                        "query": query,
                        "variables": variables,
                    })),
            )
            .await?;
        if let Some(errors) = value.get("errors").and_then(Value::as_array) {
            if !errors.is_empty() {
                return Err(classify_graphql_errors(errors, &self.token));
            }
        }
        Ok(value)
    }

    async fn pr_view_raw(&self, repo: &str, number: u64) -> Result<Value, GhError> {
        let (owner, name) = repo_parts(repo)?;
        let query = r#"
            query($owner: String!, $name: String!, $number: Int!, $after: String) {
              repository(owner: $owner, name: $name) {
                pullRequest(number: $number) {
                  number
                  state
                  mergeable
                  reviewDecision
                  isDraft
                  headRefOid
                  headRefName
                  closingIssuesReferences(first: 100, after: $after) {
                    nodes {
                      number
                      url
                    }
                    pageInfo {
                      hasNextPage
                      endCursor
                    }
                  }
                }
              }
            }
        "#;
        let mut after = Value::Null;
        let mut merged: Option<Value> = None;
        let mut all_refs = Vec::new();
        loop {
            let page = self
                .graphql(
                    query,
                    json!({
                        "owner": owner,
                        "name": name,
                        "number": number,
                        "after": after,
                    }),
                )
                .await?;
            let mut pr = page
                .pointer("/data/repository/pullRequest")
                .cloned()
                .filter(|value| !value.is_null())
                .ok_or_else(|| {
                    GhError::NotFound(format!("pull request {repo}#{number} not found"))
                })?;
            let refs = pr
                .pointer_mut("/closingIssuesReferences/nodes")
                .and_then(Value::as_array_mut)
                .map(std::mem::take)
                .unwrap_or_default();
            all_refs.extend(refs);
            let page_info = pr
                .get("closingIssuesReferences")
                .and_then(|refs| refs.get("pageInfo"))
                .cloned()
                .unwrap_or_else(|| json!({}));
            let has_next = page_info
                .get("hasNextPage")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            after = page_info
                .get("endCursor")
                .and_then(Value::as_str)
                .map(|cursor| Value::String(cursor.to_string()))
                .unwrap_or(Value::Null);
            if merged.is_none() {
                merged = Some(pr);
            }
            if !has_next {
                break;
            }
            if after.is_null() {
                return Err(GhError::Sanitized(
                    "GitHub GraphQL closingIssuesReferences hasNextPage=true without endCursor"
                        .to_string(),
                ));
            }
        }
        let mut pr = merged
            .ok_or_else(|| GhError::NotFound(format!("pull request {repo}#{number} not found")))?;
        pr["closingIssuesReferences"] = Value::Array(all_refs);
        Ok(pr)
    }

    async fn pr_head_sha(&self, repo: &str, number: u64) -> Result<String, GhError> {
        self.pr_view_raw(repo, number)
            .await?
            .get("headRefOid")
            .and_then(Value::as_str)
            .map(str::to_string)
            .filter(|sha| !sha.is_empty())
            .ok_or_else(|| {
                GhError::Sanitized(format!("pr_view missing headRefOid for {repo}#{number}"))
            })
    }

    async fn check_runs_for_ref(
        &self,
        repo: &str,
        head_sha: &str,
    ) -> Result<Vec<CheckRun>, GhError> {
        let (owner, name) = repo_parts(repo)?;
        let path = format!(
            "/repos/{}/{}/commits/{}/check-runs?per_page=100",
            url_segment(owner),
            url_segment(name),
            url_segment(head_sha)
        );
        let pages = self.get_paginated_json(self.rest_url(&path)).await?;
        let mut checks = Vec::new();
        for value in pages {
            if let Some(runs) = value.get("check_runs").and_then(Value::as_array) {
                checks.extend(runs.iter().map(|run| {
                    CheckRun {
                        name: run
                            .get("name")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                        conclusion: run
                            .get("conclusion")
                            .and_then(Value::as_str)
                            .map(str::to_string),
                        status: run
                            .get("status")
                            .and_then(Value::as_str)
                            .unwrap_or("completed")
                            .to_string(),
                    }
                }));
            }
        }
        Ok(checks)
    }

    async fn commit_statuses_for_ref(
        &self,
        repo: &str,
        head_sha: &str,
    ) -> Result<Vec<CheckRun>, GhError> {
        let (owner, name) = repo_parts(repo)?;
        let path = format!(
            "/repos/{}/{}/statuses/{}?per_page=100",
            url_segment(owner),
            url_segment(name),
            url_segment(head_sha)
        );
        let pages = self.get_paginated_json(self.rest_url(&path)).await?;
        let mut seen = std::collections::HashSet::new();
        let mut runs = Vec::new();
        for value in pages {
            let Some(statuses) = value.as_array() else {
                continue;
            };
            for status in statuses {
                let name = status
                    .get("context")
                    .and_then(Value::as_str)
                    .unwrap_or("status")
                    .to_string();
                if !seen.insert(name.clone()) {
                    continue;
                }
                let state = status.get("state").and_then(Value::as_str).unwrap_or("");
                let (run_status, conclusion) = match state {
                    "success" => ("completed".to_string(), Some("success".to_string())),
                    "failure" | "error" => ("completed".to_string(), Some("failure".to_string())),
                    "pending" => ("in_progress".to_string(), None),
                    other => ("completed".to_string(), Some(other.to_string())),
                };
                runs.push(CheckRun {
                    name,
                    conclusion,
                    status: run_status,
                });
            }
        }
        Ok(runs)
    }

    async fn pr_merge_verify(&self, repo: &str, number: u64) -> Result<String, GhError> {
        let (owner, name) = repo_parts(repo)?;
        let path = format!(
            "/repos/{}/{}/pulls/{}",
            url_segment(owner),
            url_segment(name),
            number
        );
        let value = self
            .send_json(self.request(reqwest::Method::GET, self.rest_url(&path)))
            .await?;
        if value.get("merged").and_then(Value::as_bool) != Some(true) {
            return Err(GhError::Sanitized(
                "independent merge verification failed: pull request is not merged".to_string(),
            ));
        }
        value
            .get("merge_commit_sha")
            .and_then(Value::as_str)
            .filter(|sha| !sha.trim().is_empty())
            .map(str::to_string)
            .ok_or_else(|| {
                GhError::Sanitized(
                    "independent merge verification failed: missing merge_commit_sha".to_string(),
                )
            })
    }

    async fn issue_labels(&self, repo: &str, issue_number: u64) -> Result<Vec<String>, GhError> {
        let (owner, name) = repo_parts(repo)?;
        let path = format!(
            "/repos/{}/{}/issues/{}",
            url_segment(owner),
            url_segment(name),
            issue_number
        );
        let value = self
            .send_json(self.request(reqwest::Method::GET, self.rest_url(&path)))
            .await?;
        Ok(value
            .get("labels")
            .and_then(Value::as_array)
            .map(|labels| label_names_from_array(labels))
            .unwrap_or_default())
    }

    async fn closing_issue_labels(
        &self,
        repo: &str,
        references: &[String],
    ) -> Vec<ClosingIssueLabels> {
        let mut labels = Vec::with_capacity(references.len());
        for reference in references {
            let issue_labels = match issue_number_from_reference(reference) {
                Some(issue_number) => self.issue_labels(repo, issue_number).await.ok(),
                None => None,
            };
            labels.push(ClosingIssueLabels {
                reference: reference.clone(),
                labels: issue_labels,
            });
        }
        labels
    }

    async fn get_paginated_json(&self, first_url: String) -> Result<Vec<Value>, GhError> {
        let mut url = Some(first_url);
        let mut pages = Vec::new();
        while let Some(current_url) = url {
            let (value, next_url) = self
                .send_json_page(self.request(reqwest::Method::GET, current_url))
                .await?;
            pages.push(value);
            if let Some(next) = next_url.as_deref() {
                if !self.next_url_allowed(next) {
                    return Err(GhError::Sanitized(
                        "GitHub pagination link points outside configured API origin".to_string(),
                    ));
                }
            }
            url = next_url;
        }
        Ok(pages)
    }

    fn next_url_allowed(&self, next_url: &str) -> bool {
        let Ok(next) = Url::parse(next_url) else {
            return false;
        };
        let Ok(base) = Url::parse(&self.api_base) else {
            return false;
        };
        next.scheme() == base.scheme()
            && next.host_str() == base.host_str()
            && next.port_or_known_default() == base.port_or_known_default()
    }
}

#[async_trait]
impl GhClient for HttpGhClient {
    async fn pr_view(&self, repo: &str, number: u64) -> Result<PrState, GhError> {
        validate_repo(repo).map_err(GhError::Sanitized)?;
        let mut raw = self.pr_view_raw(repo, number).await?;
        normalize_graphql_pr_view(&mut raw);
        let checks = self.checks_list(repo, number).await?;
        let mut pr = parse_pr_view_json(&raw, checks)
            .map_err(|err| GhError::Sanitized(format!("pr_view shape: {err}")))?;
        pr.closing_issue_labels = self.closing_issue_labels(repo, &pr.linked_issue_refs).await;
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
        let (owner, name) = repo_parts(repo)?;
        let path = format!(
            "/repos/{}/{}/pulls/{}/merge",
            url_segment(owner),
            url_segment(name),
            number
        );
        let mut body = json!({
            "merge_method": merge_strategy_http_value(strategy),
        });
        if !expected_head_sha.trim().is_empty() {
            body["sha"] = Value::String(expected_head_sha.to_string());
        }
        let value = self
            .send_json(
                self.request(reqwest::Method::PUT, self.rest_url(&path))
                    .json(&body),
            )
            .await?;
        if value.get("merged").and_then(Value::as_bool) != Some(true) {
            return Err(GhError::Sanitized(
                "GitHub merge response did not confirm merged=true".to_string(),
            ));
        }
        let merge_sha = self.pr_merge_verify(repo, number).await?;
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
    ) -> Result<IssueState, GhError> {
        validate_repo(repo).map_err(GhError::Sanitized)?;
        let (owner, name) = repo_parts(repo)?;
        let path = format!("/repos/{}/{}/issues", url_segment(owner), url_segment(name));
        let value = self
            .send_json(
                self.request(reqwest::Method::POST, self.rest_url(&path))
                    .json(&json!({
                        "title": title,
                        "body": body.unwrap_or_default(),
                        "labels": labels,
                    })),
            )
            .await?;
        let number = value
            .get("number")
            .and_then(Value::as_u64)
            .filter(|number| *number > 0)
            .ok_or_else(|| {
                GhError::Sanitized("GitHub issue create response missing number".to_string())
            })?;
        let title = value
            .get("title")
            .and_then(Value::as_str)
            .filter(|title| !title.trim().is_empty())
            .ok_or_else(|| {
                GhError::Sanitized("GitHub issue create response missing title".to_string())
            })?
            .to_string();
        let state = value
            .get("state")
            .and_then(Value::as_str)
            .filter(|state| !state.trim().is_empty())
            .ok_or_else(|| {
                GhError::Sanitized("GitHub issue create response missing state".to_string())
            })?
            .to_ascii_uppercase();
        let url = value
            .get("html_url")
            .or_else(|| value.get("url"))
            .and_then(Value::as_str)
            .filter(|url| !url.trim().is_empty())
            .ok_or_else(|| {
                GhError::Sanitized("GitHub issue create response missing url".to_string())
            })?
            .to_string();
        Ok(IssueState {
            number,
            title,
            state,
            url,
        })
    }

    async fn checks_list(&self, repo: &str, pr_number: u64) -> Result<Vec<CheckRun>, GhError> {
        validate_repo(repo).map_err(GhError::Sanitized)?;
        let head_sha = self.pr_head_sha(repo, pr_number).await?;
        let mut checks = self.check_runs_for_ref(repo, &head_sha).await?;
        checks.extend(self.commit_statuses_for_ref(repo, &head_sha).await?);
        Ok(checks)
    }
}

fn repo_parts(repo: &str) -> Result<(&str, &str), GhError> {
    validate_repo(repo).map_err(GhError::Sanitized)?;
    let mut parts = repo.splitn(2, '/');
    let owner = parts.next().unwrap_or_default();
    let name = parts.next().unwrap_or_default();
    Ok((owner, name))
}

fn normalize_graphql_pr_view(value: &mut Value) {
    if value
        .get("closingIssuesReferences")
        .and_then(Value::as_array)
        .is_some()
    {
        return;
    }
    if let Some(nodes) = value
        .pointer_mut("/closingIssuesReferences/nodes")
        .and_then(Value::as_array_mut)
    {
        let refs = std::mem::take(nodes);
        value["closingIssuesReferences"] = Value::Array(refs);
    }
}

fn label_names_from_array(labels: &[Value]) -> Vec<String> {
    labels
        .iter()
        .filter_map(|label| {
            label
                .get("name")
                .and_then(Value::as_str)
                .or_else(|| label.as_str())
                .map(str::to_string)
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

fn merge_strategy_http_value(strategy: MergeStrategy) -> &'static str {
    match strategy {
        MergeStrategy::Squash => "squash",
        MergeStrategy::Merge => "merge",
        MergeStrategy::Rebase => "rebase",
    }
}

fn classify_http_error(status: StatusCode, body: &str) -> GhError {
    let message = format!(
        "GitHub HTTP {}: {}",
        status.as_u16(),
        body.chars().take(MAX_HTTP_ERROR_CHARS).collect::<String>()
    );
    if status == StatusCode::NOT_FOUND {
        GhError::NotFound(message)
    } else if status == StatusCode::FORBIDDEN && body.to_ascii_lowercase().contains("rate limit") {
        GhError::RateLimited(message)
    } else if status == StatusCode::TOO_MANY_REQUESTS {
        GhError::RateLimited(message)
    } else {
        GhError::Sanitized(message)
    }
}

fn classify_graphql_errors(errors: &[Value], token: &str) -> GhError {
    let message = errors
        .iter()
        .filter_map(|error| error.get("message").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("; ");
    let message = sanitize_output(&message, token);
    let lower = message.to_ascii_lowercase();
    if lower.contains("not found") || lower.contains("could not resolve") {
        GhError::NotFound(message)
    } else if lower.contains("rate limit") {
        GhError::RateLimited(message)
    } else {
        GhError::Sanitized(message)
    }
}

fn url_segment(segment: &str) -> String {
    segment.replace('/', "%2F")
}

fn parse_next_link(value: Option<&reqwest::header::HeaderValue>) -> Option<String> {
    let raw = value?.to_str().ok()?;
    for part in raw.split(',') {
        let (url_part, rel_part) = part.trim().split_once(';')?;
        if !rel_part
            .split(';')
            .any(|param| param.trim() == "rel=\"next\"")
        {
            continue;
        }
        let url = url_part
            .trim()
            .strip_prefix('<')?
            .strip_suffix('>')?
            .to_string();
        if Url::parse(&url).is_ok() {
            return Some(url);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    async fn http_client_fetches_pr_checks_and_issue_labels() {
        let server = TestGitHubServer::spawn().await;
        let client = HttpGhClient::for_tests(server.base_url(), server.graphql_url());

        let pr = client.pr_view("owner/repo", 42).await.unwrap();

        assert_eq!(pr.number, 42);
        assert_eq!(pr.state, PrLifecycleState::Open);
        assert_eq!(pr.mergeable, Mergeable::Mergeable);
        assert_eq!(pr.review_decision, Some(ReviewDecision::Approved));
        assert_eq!(pr.head_sha, "head-sha");
        assert!(pr
            .linked_issue_refs
            .contains(&"https://github.com/owner/repo/issues/7".to_string()));
        assert_eq!(pr.checks, ChecksState::Success);
        assert!(pr.closing_issue_labels.iter().any(|labels| {
            labels.reference == "https://github.com/owner/repo/issues/7"
                && labels.labels == Some(vec!["bug".to_string(), "safe-close".to_string()])
        }));
    }

    #[tokio::test]
    async fn http_client_merges_with_expected_head_sha() {
        let server = TestGitHubServer::spawn().await;
        let client = HttpGhClient::for_tests(server.base_url(), server.graphql_url());

        let result = client
            .pr_merge("owner/repo", 42, MergeStrategy::Squash, "head-sha")
            .await
            .unwrap();

        assert_eq!(
            result,
            MergeResult {
                pr_number: 42,
                merge_sha: "merge-sha".to_string(),
                strategy: MergeStrategy::Squash,
            }
        );
        assert!(server
            .requests()
            .await
            .iter()
            .any(|request| request.method == "PUT"
                && request.path == "/repos/owner/repo/pulls/42/merge"
                && request.body.contains("\"sha\":\"head-sha\"")
                && request.body.contains("\"merge_method\":\"squash\"")));
    }

    #[tokio::test]
    async fn http_client_creates_issue() {
        let server = TestGitHubServer::spawn().await;
        let client = HttpGhClient::for_tests(server.base_url(), server.graphql_url());

        let issue = client
            .issue_create(
                "owner/repo",
                "Promote handoff",
                Some("body"),
                &["triage".to_string()],
            )
            .await
            .unwrap();

        assert_eq!(issue.number, 99);
        assert_eq!(issue.title, "Promote handoff");
        assert_eq!(issue.state, "OPEN");
        assert_eq!(issue.url, "https://github.com/owner/repo/issues/99");
    }

    #[tokio::test]
    async fn http_client_follows_check_and_status_pagination() {
        let server = TestGitHubServer::spawn().await;
        let client = HttpGhClient::for_tests(server.base_url(), server.graphql_url());

        let checks = client.checks_list("owner/paginated", 42).await.unwrap();

        assert!(checks.iter().any(|check| check.name == "second-page-fail"
            && check.conclusion.as_deref() == Some("failure")));
        assert!(checks.iter().any(|check| check.name == "second-page-status"
            && check.conclusion.as_deref() == Some("failure")));
        assert_eq!(ChecksState::aggregate(&checks), ChecksState::Failure);
    }

    #[tokio::test]
    async fn http_client_fetches_all_closing_issue_pages() {
        let server = TestGitHubServer::spawn().await;
        let client = HttpGhClient::for_tests(server.base_url(), server.graphql_url());

        let pr = client.pr_view("owner/repo", 42).await.unwrap();

        assert!(pr
            .linked_issue_refs
            .contains(&"https://github.com/owner/repo/issues/21".to_string()));
        assert!(pr.closing_issue_labels.iter().any(|labels| {
            labels.reference == "https://github.com/owner/repo/issues/21"
                && labels.labels == Some(vec!["agent:no-close".to_string()])
        }));
    }

    #[tokio::test]
    async fn http_client_redacts_token_from_http_and_graphql_errors() {
        let server = TestGitHubServer::spawn().await;
        let client = HttpGhClient::for_tests(server.base_url(), server.graphql_url());

        let http_err = client.issue_labels("owner/repo", 500).await.unwrap_err();
        let graphql_err = client.pr_view("owner/repo", 500).await.unwrap_err();

        let http_msg = http_err.to_string();
        let graphql_msg = graphql_err.to_string();
        assert!(!http_msg.contains("test-token"), "{http_msg}");
        assert!(!graphql_msg.contains("test-token"), "{graphql_msg}");
        assert!(http_msg.contains("[REDACTED]"), "{http_msg}");
        assert!(graphql_msg.contains("[REDACTED]"), "{graphql_msg}");
    }

    #[tokio::test]
    async fn http_client_rejects_spoofed_merge_without_independent_verification() {
        let server = TestGitHubServer::spawn().await;
        let client = HttpGhClient::for_tests(server.base_url(), server.graphql_url());

        let merge_err = client
            .pr_merge("owner/repo", 44, MergeStrategy::Squash, "head-sha")
            .await
            .unwrap_err();

        assert!(
            merge_err
                .to_string()
                .contains("independent merge verification failed"),
            "{merge_err}"
        );
    }

    #[tokio::test]
    async fn http_client_rejects_malformed_success_responses() {
        let server = TestGitHubServer::spawn().await;
        let client = HttpGhClient::for_tests(server.base_url(), server.graphql_url());

        let merge_err = client
            .pr_merge("owner/repo", 43, MergeStrategy::Squash, "head-sha")
            .await
            .unwrap_err();
        let issue_err = client
            .issue_create("owner/repo", "bad", None, &[])
            .await
            .unwrap_err();

        assert!(
            merge_err
                .to_string()
                .contains("independent merge verification failed"),
            "{merge_err}"
        );
        assert!(
            issue_err.to_string().contains("missing number"),
            "{issue_err}"
        );
    }

    #[tokio::test]
    async fn http_client_rejects_cross_origin_pagination_links() {
        let server = TestGitHubServer::spawn().await;
        let client = HttpGhClient::for_tests(server.base_url(), server.graphql_url());

        assert!(client.next_url_allowed(&format!(
            "{}/repos/owner/repo/check-runs?page=2",
            server.base_url()
        )));
        assert!(!client.next_url_allowed("https://example.invalid/repos/owner/repo/check-runs"));
    }

    #[derive(Clone, Debug)]
    struct RecordedRequest {
        method: String,
        path: String,
        host: String,
        body: String,
    }

    struct TestGitHubServer {
        addr: std::net::SocketAddr,
        requests: std::sync::Arc<tokio::sync::Mutex<Vec<RecordedRequest>>>,
    }

    impl TestGitHubServer {
        async fn spawn() -> Self {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            let requests = std::sync::Arc::new(tokio::sync::Mutex::new(Vec::new()));
            let server_requests = std::sync::Arc::clone(&requests);
            tokio::spawn(async move {
                loop {
                    let Ok((mut stream, _peer)) = listener.accept().await else {
                        break;
                    };
                    let requests = std::sync::Arc::clone(&server_requests);
                    tokio::spawn(async move {
                        let Some(request) = read_request(&mut stream).await else {
                            return;
                        };
                        requests.lock().await.push(request.clone());
                        let (status, headers, body) = response_for(&request);
                        let mut response = format!("HTTP/1.1 {status}\r\n");
                        for (name, value) in headers {
                            response.push_str(&format!("{name}: {value}\r\n"));
                        }
                        response.push_str(&format!(
                            "content-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                            body.len()
                        ));
                        let _ = stream.write_all(response.as_bytes()).await;
                    });
                }
            });
            Self { addr, requests }
        }

        fn base_url(&self) -> String {
            format!("http://{}", self.addr)
        }

        fn graphql_url(&self) -> String {
            format!("{}/graphql", self.base_url())
        }

        async fn requests(&self) -> Vec<RecordedRequest> {
            self.requests.lock().await.clone()
        }
    }

    async fn read_request(stream: &mut tokio::net::TcpStream) -> Option<RecordedRequest> {
        let mut buffer = Vec::new();
        let mut temp = [0_u8; 1024];
        loop {
            let n = stream.read(&mut temp).await.ok()?;
            if n == 0 {
                break;
            }
            buffer.extend_from_slice(&temp[..n]);
            if buffer.windows(4).any(|window| window == b"\r\n\r\n") {
                break;
            }
        }
        let header_end = buffer.windows(4).position(|window| window == b"\r\n\r\n")? + 4;
        let headers = String::from_utf8_lossy(&buffer[..header_end]).to_string();
        let mut lines = headers.lines();
        let request_line = lines.next()?;
        let mut request_parts = request_line.split_whitespace();
        let method = request_parts.next()?.to_string();
        let path = request_parts.next()?.to_string();
        let mut host = String::new();
        let mut content_length = 0usize;
        for (name, value) in lines.filter_map(|line| line.split_once(':')) {
            if name.eq_ignore_ascii_case("host") {
                host = value.trim().to_string();
            } else if name.eq_ignore_ascii_case("content-length") {
                content_length = value.trim().parse::<usize>().ok().unwrap_or(0);
            }
        }
        while buffer.len() < header_end + content_length {
            let n = stream.read(&mut temp).await.ok()?;
            if n == 0 {
                break;
            }
            buffer.extend_from_slice(&temp[..n]);
        }
        let body = String::from_utf8_lossy(
            &buffer[header_end..std::cmp::min(buffer.len(), header_end + content_length)],
        )
        .to_string();
        Some(RecordedRequest {
            method,
            path,
            host,
            body,
        })
    }

    fn response_for(request: &RecordedRequest) -> (String, Vec<(String, String)>, String) {
        let status = |code: &str, body: String| -> (String, Vec<(String, String)>, String) {
            (code.to_string(), Vec::new(), body)
        };
        let link_status =
            |code: &str, link: String, body: String| -> (String, Vec<(String, String)>, String) {
                (code.to_string(), vec![("link".to_string(), link)], body)
            };
        match (request.method.as_str(), request.path.as_str()) {
            ("POST", "/graphql") if request.body.contains("\"number\":500") => status(
                "200 OK",
                json!({
                    "errors": [
                        {
                            "message": "upstream echoed test-token"
                        }
                    ]
                })
                .to_string(),
            ),
            ("POST", "/graphql") if request.body.contains("\"after\":null") => status(
                "200 OK",
                graphql_pr_page(
                    vec![(
                        7,
                        "https://github.com/owner/repo/issues/7".to_string(),
                    )],
                    true,
                    Some("cursor-1"),
                ),
            ),
            ("POST", "/graphql") => status(
                "200 OK",
                graphql_pr_page(
                    vec![(
                        21,
                        "https://github.com/owner/repo/issues/21".to_string(),
                    )],
                    false,
                    None,
                ),
            ),
            ("GET", "/repos/owner/repo/commits/head-sha/check-runs?per_page=100") => status(
                "200 OK",
                json!({
                    "total_count": 1,
                    "check_runs": [
                        {
                            "name": "ci",
                            "status": "completed",
                            "conclusion": "success"
                        }
                    ]
                })
                .to_string(),
            ),
            (
                "GET",
                "/repos/owner/paginated/commits/head-sha/check-runs?per_page=100",
            ) => link_status(
                "200 OK",
                format!(
                    "<http://{}/repos/owner/paginated/commits/head-sha/check-runs?page=2>; rel=\"next\"",
                    request.host
                ),
                json!({
                    "total_count": 2,
                    "check_runs": [
                        {
                            "name": "first-page-pass",
                            "status": "completed",
                            "conclusion": "success"
                        }
                    ]
                })
                .to_string(),
            ),
            (
                "GET",
                "/repos/owner/paginated/commits/head-sha/check-runs?page=2",
            ) => status(
                "200 OK",
                json!({
                    "total_count": 2,
                    "check_runs": [
                        {
                            "name": "second-page-fail",
                            "status": "completed",
                            "conclusion": "failure"
                        }
                    ]
                })
                .to_string(),
            ),
            ("GET", "/repos/owner/repo/statuses/head-sha?per_page=100") => {
                status("200 OK", json!([]).to_string())
            }
            ("GET", "/repos/owner/paginated/statuses/head-sha?per_page=100") => link_status(
                "200 OK",
                format!(
                    "<http://{}/repos/owner/paginated/statuses/head-sha?page=2>; rel=\"next\"",
                    request.host
                ),
                json!([
                    {
                        "context": "first-page-status",
                        "state": "success"
                    }
                ])
                .to_string(),
            ),
            ("GET", "/repos/owner/paginated/statuses/head-sha?page=2") => status(
                "200 OK",
                json!([
                    {
                        "context": "second-page-status",
                        "state": "failure"
                    }
                ])
                .to_string(),
            ),
            ("GET", "/repos/owner/repo/issues/7") => status(
                "200 OK",
                json!({
                    "number": 7,
                    "labels": [
                        {"name": "bug"},
                        {"name": "safe-close"}
                    ]
                })
                .to_string(),
            ),
            ("GET", "/repos/owner/repo/issues/21") => status(
                "200 OK",
                json!({
                    "number": 21,
                    "labels": [
                        {"name": "agent:no-close"}
                    ]
                })
                .to_string(),
            ),
            ("GET", "/repos/owner/repo/issues/500") => status(
                "500 Internal Server Error",
                json!({
                    "message": "upstream echoed test-token"
                })
                .to_string(),
            ),
            ("PUT", "/repos/owner/repo/pulls/42/merge") => status(
                "200 OK",
                json!({
                    "sha": "merge-sha",
                    "merged": true,
                    "message": "Pull Request successfully merged"
                })
                .to_string(),
            ),
            ("GET", "/repos/owner/repo/pulls/42") => status(
                "200 OK",
                json!({
                    "number": 42,
                    "merged": true,
                    "merge_commit_sha": "merge-sha",
                    "state": "closed"
                })
                .to_string(),
            ),
            ("PUT", "/repos/owner/repo/pulls/43/merge") => status(
                "200 OK",
                json!({
                    "merged": true,
                    "message": "missing sha"
                })
                .to_string(),
            ),
            ("GET", "/repos/owner/repo/pulls/43") => status(
                "200 OK",
                json!({
                    "number": 43,
                    "merged": false,
                    "merge_commit_sha": null,
                    "state": "open"
                })
                .to_string(),
            ),
            ("PUT", "/repos/owner/repo/pulls/44/merge") => status(
                "200 OK",
                json!({
                    "merged": true,
                    "sha": "fake",
                    "message": "spoofed merge response"
                })
                .to_string(),
            ),
            ("GET", "/repos/owner/repo/pulls/44") => status(
                "200 OK",
                json!({
                    "number": 44,
                    "merged": false,
                    "merge_commit_sha": null,
                    "state": "open"
                })
                .to_string(),
            ),
            ("POST", "/repos/owner/repo/issues") => status(
                "201 Created",
                if request.body.contains("\"title\":\"bad\"") {
                    json!({
                        "title": "bad",
                        "state": "open",
                        "html_url": "https://github.com/owner/repo/issues/99"
                    })
                    .to_string()
                } else {
                    json!({
                        "number": 99,
                        "title": "Promote handoff",
                        "state": "open",
                        "html_url": "https://github.com/owner/repo/issues/99"
                    })
                    .to_string()
                },
            ),
            _ => status(
                "404 Not Found",
                json!({
                    "message": format!("unexpected {} {}", request.method, request.path)
                })
                .to_string(),
            ),
        }
    }

    fn graphql_pr_page(
        refs: Vec<(u64, String)>,
        has_next_page: bool,
        end_cursor: Option<&str>,
    ) -> String {
        json!({
            "data": {
                "repository": {
                    "pullRequest": {
                        "number": 42,
                        "state": "OPEN",
                        "mergeable": "MERGEABLE",
                        "reviewDecision": "APPROVED",
                        "isDraft": false,
                        "headRefOid": "head-sha",
                        "closingIssuesReferences": {
                            "nodes": refs
                                .into_iter()
                                .map(|(number, url)| json!({
                                    "number": number,
                                    "url": url,
                                }))
                                .collect::<Vec<_>>(),
                            "pageInfo": {
                                "hasNextPage": has_next_page,
                                "endCursor": end_cursor,
                            }
                        }
                    }
                }
            }
        })
        .to_string()
    }
}
