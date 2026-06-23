use super::super::*;

pub(in crate::gh_ops) fn flatten_paginated_array(
    value: Value,
    label: &str,
) -> Result<Vec<Value>, String> {
    match value {
        Value::Array(items) if items.iter().all(Value::is_array) => {
            let mut flattened = Vec::new();
            for page in items {
                if let Value::Array(page_items) = page {
                    flattened.extend(page_items);
                }
            }
            Ok(flattened)
        }
        Value::Array(items) => Ok(items),
        other => Err(format!("{label} response was not an array: {other}")),
    }
}

pub(in crate::gh_ops) fn normalize_review_entry(entry: Value) -> Value {
    json!({
        "kind": "review",
        "id": entry.get("id").cloned().unwrap_or(Value::Null),
        "review_id": entry.get("id").cloned().unwrap_or(Value::Null),
        "author": entry
            .get("user")
            .and_then(|user| user.get("login"))
            .cloned()
            .unwrap_or(Value::Null),
        "body": entry.get("body").cloned().unwrap_or(Value::Null),
        "state": entry.get("state").cloned().unwrap_or(Value::Null),
        "submitted_at": entry.get("submitted_at").cloned().unwrap_or(Value::Null),
        "created_at": entry.get("submitted_at").cloned().unwrap_or(Value::Null),
        "url": entry.get("html_url").cloned().unwrap_or(Value::Null),
    })
}

pub(in crate::gh_ops) fn normalize_inline_comment_entry(entry: Value) -> Value {
    json!({
        "kind": "inline_comment",
        "id": entry.get("id").cloned().unwrap_or(Value::Null),
        "comment_id": entry.get("id").cloned().unwrap_or(Value::Null),
        "review_id": entry
            .get("pull_request_review_id")
            .cloned()
            .unwrap_or(Value::Null),
        "in_reply_to_id": entry.get("in_reply_to_id").cloned().unwrap_or(Value::Null),
        "author": entry
            .get("user")
            .and_then(|user| user.get("login"))
            .cloned()
            .unwrap_or(Value::Null),
        "path": entry.get("path").cloned().unwrap_or(Value::Null),
        "line": entry.get("line").cloned().unwrap_or(Value::Null),
        "start_line": entry.get("start_line").cloned().unwrap_or(Value::Null),
        "side": entry.get("side").cloned().unwrap_or(Value::Null),
        "body": entry.get("body").cloned().unwrap_or(Value::Null),
        "created_at": entry.get("created_at").cloned().unwrap_or(Value::Null),
        "updated_at": entry.get("updated_at").cloned().unwrap_or(Value::Null),
        "url": entry.get("html_url").cloned().unwrap_or(Value::Null),
    })
}

pub(in crate::gh_ops) fn comment_entry_time(entry: &Value) -> Option<&str> {
    entry
        .get("created_at")
        .and_then(Value::as_str)
        .or_else(|| entry.get("submitted_at").and_then(Value::as_str))
}

pub(in crate::gh_ops) fn merge_pr_comment_entries(
    mut reviews: Vec<Value>,
    mut inline_comments: Vec<Value>,
) -> Vec<Value> {
    let mut comments = Vec::with_capacity(reviews.len() + inline_comments.len());
    comments.append(&mut reviews);
    comments.append(&mut inline_comments);
    comments.sort_by(|left, right| {
        comment_entry_time(left)
            .unwrap_or("")
            .cmp(comment_entry_time(right).unwrap_or(""))
            .then_with(|| {
                left.get("id")
                    .and_then(Value::as_i64)
                    .unwrap_or_default()
                    .cmp(&right.get("id").and_then(Value::as_i64).unwrap_or_default())
            })
    });
    comments
}
