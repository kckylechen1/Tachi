//! One-call GraphQL normalization for the production CurrentTruth refresh.
//!
//! The query requests repository visibility and the complete bounded typed
//! issue/PR relation set together. This keeps denied and accessible-private
//! refreshes on the same network-call shape and excludes title/body prose from
//! source identity and linkage.

use super::{GithubReadBundle, LoadFailure};
use serde_json::{json, Value};
use std::collections::BTreeMap;

pub(super) const GITHUB_REFRESH_QUERY: &str = r#"
query($owner:String!,$name:String!,$number:Int!){
  repository(owner:$owner,name:$name){
    visibility
    issueOrPullRequest(number:$number){
      __typename
      ... on Issue{
        number state updatedAt
        timelineItems(first:100,itemTypes:[CROSS_REFERENCED_EVENT,REOPENED_EVENT]){
          pageInfo{hasNextPage}
          nodes{
            __typename
            ... on ReopenedEvent{id createdAt}
            ... on CrossReferencedEvent{
              id createdAt
              source{
                __typename
                ... on PullRequest{
                  number state updatedAt headRefOid baseRefOid
                  mergeCommit{oid}
                  repository{nameWithOwner}
                  closingIssuesReferences(first:100){
                    pageInfo{hasNextPage}
                    nodes{number repository{nameWithOwner}}
                  }
                }
              }
            }
          }
        }
      }
    }
  }
}"#;

pub(super) fn parse_graphql_bundle(
    repo: &str,
    issue_number: u64,
    value: &Value,
) -> Result<GithubReadBundle, LoadFailure> {
    if value
        .get("errors")
        .and_then(Value::as_array)
        .is_some_and(|errors| !errors.is_empty())
    {
        return Err(LoadFailure::Unavailable);
    }
    let repository = value
        .pointer("/data/repository")
        .and_then(Value::as_object)
        .ok_or(LoadFailure::Unavailable)?;
    let visibility = repository
        .get("visibility")
        .and_then(Value::as_str)
        .ok_or(LoadFailure::Malformed)?;
    let issue = repository
        .get("issueOrPullRequest")
        .and_then(Value::as_object)
        .ok_or(LoadFailure::Unavailable)?;
    if issue.get("__typename").and_then(Value::as_str) != Some("Issue")
        || issue.get("number").and_then(Value::as_u64) != Some(issue_number)
    {
        return Err(LoadFailure::Malformed);
    }
    let issue_value = json!({
        "number": issue_number,
        "title": "",
        "body": "",
        "state": issue.get("state").cloned().ok_or(LoadFailure::Malformed)?,
        "updatedAt": issue.get("updatedAt").cloned().ok_or(LoadFailure::Malformed)?,
        "labels": [],
        "milestone": null,
        "comments": [],
    });
    let timeline = issue
        .get("timelineItems")
        .and_then(Value::as_object)
        .ok_or(LoadFailure::Malformed)?;
    let mut complete = !timeline
        .get("pageInfo")
        .and_then(|page_info| page_info.get("hasNextPage"))
        .and_then(Value::as_bool)
        .ok_or(LoadFailure::Malformed)?;
    let nodes = timeline
        .get("nodes")
        .and_then(Value::as_array)
        .ok_or(LoadFailure::Malformed)?;
    let mut normalized_events = Vec::with_capacity(nodes.len());
    let mut pull_requests = BTreeMap::new();
    for node in nodes {
        match node.get("__typename").and_then(Value::as_str) {
            Some("ReopenedEvent") => normalized_events.push(json!({
                "event": "reopened",
                "id": node.get("id").and_then(Value::as_str).ok_or(LoadFailure::Malformed)?,
                "created_at": node.get("createdAt").and_then(Value::as_str).ok_or(LoadFailure::Malformed)?,
            })),
            Some("CrossReferencedEvent") => {
                let Some(source) = node.get("source").and_then(Value::as_object) else {
                    continue;
                };
                if source.get("__typename").and_then(Value::as_str) != Some("PullRequest") {
                    continue;
                }
                let source_repo = source
                    .get("repository")
                    .and_then(|repository| repository.get("nameWithOwner"))
                    .and_then(Value::as_str)
                    .ok_or(LoadFailure::Malformed)?;
                if !source_repo.eq_ignore_ascii_case(repo) {
                    continue;
                }
                let pr_number = source
                    .get("number")
                    .and_then(Value::as_u64)
                    .ok_or(LoadFailure::Malformed)?;
                normalized_events.push(json!({
                    "event": "cross-referenced",
                    "id": node.get("id").and_then(Value::as_str).ok_or(LoadFailure::Malformed)?,
                    "created_at": node.get("createdAt").and_then(Value::as_str).ok_or(LoadFailure::Malformed)?,
                    "source": {"issue": {
                        "number": pr_number,
                        "pull_request": {},
                        "repository": {"full_name": source_repo},
                    }},
                }));

                let closing = source
                    .get("closingIssuesReferences")
                    .and_then(Value::as_object)
                    .ok_or(LoadFailure::Malformed)?;
                if closing
                    .get("pageInfo")
                    .and_then(|page_info| page_info.get("hasNextPage"))
                    .and_then(Value::as_bool)
                    .ok_or(LoadFailure::Malformed)?
                {
                    complete = false;
                }
                let links = closing
                    .get("nodes")
                    .and_then(Value::as_array)
                    .ok_or(LoadFailure::Malformed)?
                    .iter()
                    .map(|linked| {
                        let linked_repo = linked
                            .pointer("/repository/nameWithOwner")
                            .and_then(Value::as_str)
                            .ok_or(LoadFailure::Malformed)?;
                        let number = linked
                            .get("number")
                            .and_then(Value::as_u64)
                            .ok_or(LoadFailure::Malformed)?;
                        Ok((linked_repo, number))
                    })
                    .collect::<Result<Vec<_>, LoadFailure>>()?;
                let same_repo_links = links
                    .into_iter()
                    .filter(|(linked_repo, _)| linked_repo.eq_ignore_ascii_case(repo))
                    .map(|(_, number)| json!({"number": number}))
                    .collect::<Vec<_>>();
                let pr_value = json!({
                    "number": pr_number,
                    "title": "",
                    "body": "",
                    "state": source.get("state").cloned().ok_or(LoadFailure::Malformed)?,
                    "headRefOid": source.get("headRefOid").cloned().ok_or(LoadFailure::Malformed)?,
                    "baseRefOid": source.get("baseRefOid").cloned().ok_or(LoadFailure::Malformed)?,
                    "updatedAt": source.get("updatedAt").cloned().ok_or(LoadFailure::Malformed)?,
                    "mergeCommit": source.get("mergeCommit").cloned().unwrap_or(Value::Null),
                    "reviews": [],
                    "statusCheckRollup": [],
                    "closingIssuesReferences": same_repo_links,
                });
                if pull_requests
                    .insert(pr_number, pr_value.clone())
                    .is_some_and(|prior| prior != pr_value)
                {
                    return Err(LoadFailure::Malformed);
                }
            }
            _ => return Err(LoadFailure::Malformed),
        }
    }
    Ok(GithubReadBundle {
        visibility: json!({"visibility": visibility}),
        issue: issue_value,
        timeline: normalized_events,
        pull_requests,
        complete,
    })
}
