//! One-call GraphQL normalization for the production CurrentTruth refresh.
//!
//! The query requests repository visibility and the complete bounded typed
//! issue/PR relation set together. This keeps denied and accessible-private
//! refreshes on the same network-call shape and excludes title/body prose from
//! source identity and linkage.

use super::{GithubReadBundle, GithubReadFailure, LoadFailure};
use chrono::DateTime;
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

/// Parse only the typed repository visibility, independently of issue data
/// and GraphQL errors. Failure callers may retain restrictions, not fresh truth.
pub(super) fn repository_visibility(value: &Value) -> Option<super::VisibilityClassV1> {
    value
        .pointer("/data/repository")
        .and_then(Value::as_object)
        .and_then(|repository| repository.get("visibility"))
        .and_then(Value::as_str)
        .and_then(|visibility| match visibility {
            "PUBLIC" | "public" => Some(super::VisibilityClassV1::Public),
            "PRIVATE" | "private" | "INTERNAL" | "internal" => {
                Some(super::VisibilityClassV1::Private)
            }
            _ => None,
        })
}

pub(super) fn parse_graphql_bundle(
    repo: &str,
    issue_number: u64,
    value: &Value,
) -> Result<GithubReadBundle, GithubReadFailure> {
    let raw_repository = value.pointer("/data/repository").and_then(Value::as_object);
    let repository_visibility = repository_visibility(value);
    let fail = |failure| GithubReadFailure::new(failure, repository_visibility);

    match value.get("errors") {
        None => {}
        Some(Value::Array(errors)) if errors.is_empty() => {}
        Some(Value::Array(_)) => return Err(fail(LoadFailure::Unavailable)),
        Some(_) => return Err(fail(LoadFailure::Malformed)),
    }
    let repository = raw_repository.ok_or_else(|| fail(LoadFailure::Unavailable))?;
    let visibility = repository_visibility.ok_or_else(|| fail(LoadFailure::Malformed))?;
    let issue = repository
        .get("issueOrPullRequest")
        .and_then(Value::as_object)
        .ok_or_else(|| fail(LoadFailure::Unavailable))?;
    if issue.get("__typename").and_then(Value::as_str) != Some("Issue")
        || issue.get("number").and_then(Value::as_u64) != Some(issue_number)
    {
        return Err(fail(LoadFailure::Malformed));
    }
    let issue_value = json!({
        "number": issue_number,
        "title": "",
        "body": "",
        "state": issue.get("state").cloned().ok_or_else(|| fail(LoadFailure::Malformed))?,
        "updatedAt": issue.get("updatedAt").cloned().ok_or_else(|| fail(LoadFailure::Malformed))?,
        "labels": [],
        "milestone": null,
        "comments": [],
    });
    let timeline = issue
        .get("timelineItems")
        .and_then(Value::as_object)
        .ok_or_else(|| fail(LoadFailure::Malformed))?;
    let mut complete = !timeline
        .get("pageInfo")
        .and_then(|page_info| page_info.get("hasNextPage"))
        .and_then(Value::as_bool)
        .ok_or_else(|| fail(LoadFailure::Malformed))?;
    let nodes = timeline
        .get("nodes")
        .and_then(Value::as_array)
        .ok_or_else(|| fail(LoadFailure::Malformed))?;
    let mut normalized_events = Vec::with_capacity(nodes.len());
    let mut pull_requests = BTreeMap::new();
    for node in nodes {
        match node.get("__typename").and_then(Value::as_str) {
            Some("ReopenedEvent") => normalized_events.push(json!({
                "event": "reopened",
                "id": node.get("id").and_then(Value::as_str).filter(|id| !id.is_empty()).ok_or_else(|| fail(LoadFailure::Malformed))?,
                "created_at": node.get("createdAt").and_then(Value::as_str).filter(|at| DateTime::parse_from_rfc3339(at).is_ok()).ok_or_else(|| fail(LoadFailure::Malformed))?,
            })),
            Some("CrossReferencedEvent") => {
                // GitHub declares CrossReferencedEvent.source non-null. A
                // missing/malformed source therefore means the bounded typed
                // relation set is not authoritative; never normalize it into
                // an empty linkage set. A well-formed Issue source is a valid
                // non-PR relation and may be ignored by this adapter.
                let source = node
                    .get("source")
                    .and_then(Value::as_object)
                    .ok_or_else(|| fail(LoadFailure::Malformed))?;
                let event_id = node
                    .get("id")
                    .and_then(Value::as_str)
                    .filter(|id| !id.is_empty())
                    .ok_or_else(|| fail(LoadFailure::Malformed))?;
                let event_created_at = node
                    .get("createdAt")
                    .and_then(Value::as_str)
                    .filter(|at| DateTime::parse_from_rfc3339(at).is_ok())
                    .ok_or_else(|| fail(LoadFailure::Malformed))?;
                match source.get("__typename").and_then(Value::as_str) {
                    Some("Issue") => continue,
                    Some("PullRequest") => {}
                    _ => return Err(fail(LoadFailure::Malformed)),
                }
                let source_repo = source
                    .get("repository")
                    .and_then(|repository| repository.get("nameWithOwner"))
                    .and_then(Value::as_str)
                    .filter(|repo| !repo.is_empty() && repo.matches('/').count() == 1)
                    .ok_or_else(|| fail(LoadFailure::Malformed))?;
                let pr_number = source
                    .get("number")
                    .and_then(Value::as_u64)
                    .filter(|number| *number > 0)
                    .ok_or_else(|| fail(LoadFailure::Malformed))?;
                let state = source
                    .get("state")
                    .and_then(Value::as_str)
                    .filter(|state| matches!(*state, "OPEN" | "CLOSED" | "MERGED"))
                    .ok_or_else(|| fail(LoadFailure::Malformed))?;
                let updated_at = source
                    .get("updatedAt")
                    .and_then(Value::as_str)
                    .filter(|at| DateTime::parse_from_rfc3339(at).is_ok())
                    .ok_or_else(|| fail(LoadFailure::Malformed))?;
                let head_ref_oid = source
                    .get("headRefOid")
                    .and_then(Value::as_str)
                    .filter(|oid| !oid.is_empty())
                    .ok_or_else(|| fail(LoadFailure::Malformed))?;
                let base_ref_oid = source
                    .get("baseRefOid")
                    .and_then(Value::as_str)
                    .filter(|oid| !oid.is_empty())
                    .ok_or_else(|| fail(LoadFailure::Malformed))?;
                let merge_commit = match source.get("mergeCommit") {
                    Some(Value::Null) => Value::Null,
                    Some(Value::Object(commit)) => {
                        let oid = commit
                            .get("oid")
                            .and_then(Value::as_str)
                            .filter(|oid| !oid.is_empty())
                            .ok_or_else(|| fail(LoadFailure::Malformed))?;
                        json!({"oid": oid})
                    }
                    _ => return Err(fail(LoadFailure::Malformed)),
                };

                let closing = source
                    .get("closingIssuesReferences")
                    .and_then(Value::as_object)
                    .ok_or_else(|| fail(LoadFailure::Malformed))?;
                if closing
                    .get("pageInfo")
                    .and_then(|page_info| page_info.get("hasNextPage"))
                    .and_then(Value::as_bool)
                    .ok_or_else(|| fail(LoadFailure::Malformed))?
                {
                    complete = false;
                }
                let links = closing
                    .get("nodes")
                    .and_then(Value::as_array)
                    .ok_or_else(|| fail(LoadFailure::Malformed))?
                    .iter()
                    .map(|linked| {
                        let linked_repo = linked
                            .pointer("/repository/nameWithOwner")
                            .and_then(Value::as_str)
                            .filter(|repo| !repo.is_empty() && repo.matches('/').count() == 1)
                            .ok_or_else(|| fail(LoadFailure::Malformed))?;
                        let number = linked
                            .get("number")
                            .and_then(Value::as_u64)
                            .filter(|number| *number > 0)
                            .ok_or_else(|| fail(LoadFailure::Malformed))?;
                        Ok((linked_repo, number))
                    })
                    .collect::<Result<Vec<_>, GithubReadFailure>>()?;
                if !source_repo.eq_ignore_ascii_case(repo) {
                    continue;
                }
                normalized_events.push(json!({
                    "event": "cross-referenced",
                    "id": event_id,
                    "created_at": event_created_at,
                    "source": {"issue": {
                        "number": pr_number,
                        "pull_request": {},
                        "repository": {"full_name": source_repo},
                    }},
                }));
                let same_repo_links = links
                    .into_iter()
                    .filter(|(linked_repo, _)| linked_repo.eq_ignore_ascii_case(repo))
                    .map(|(_, number)| json!({"number": number}))
                    .collect::<Vec<_>>();
                let pr_value = json!({
                    "number": pr_number,
                    "title": "",
                    "body": "",
                    "state": state,
                    "headRefOid": head_ref_oid,
                    "baseRefOid": base_ref_oid,
                    "updatedAt": updated_at,
                    "mergeCommit": merge_commit,
                    "reviews": [],
                    "statusCheckRollup": [],
                    "closingIssuesReferences": same_repo_links,
                });
                if pull_requests
                    .insert(pr_number, pr_value.clone())
                    .is_some_and(|prior| prior != pr_value)
                {
                    return Err(fail(LoadFailure::Malformed));
                }
            }
            _ => return Err(fail(LoadFailure::Malformed)),
        }
    }
    Ok(GithubReadBundle {
        visibility,
        issue: issue_value,
        timeline: normalized_events,
        pull_requests,
        complete,
    })
}
