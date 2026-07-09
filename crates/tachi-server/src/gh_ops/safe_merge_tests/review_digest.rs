use super::*;

#[test]
fn pr_comments_merge_preserves_chronological_order() {
    let reviews = vec![json!({
        "kind": "review",
        "id": 2,
        "created_at": "2026-06-06T10:10:00Z",
        "body": "summary",
    })];
    let inline_comments = vec![json!({
        "kind": "inline_comment",
        "id": 1,
        "created_at": "2026-06-06T10:05:00Z",
        "body": "line comment",
    })];

    let merged = merge_pr_comment_entries(reviews, inline_comments);

    assert_eq!(merged.len(), 2);
    assert_eq!(merged[0]["kind"], "inline_comment");
    assert_eq!(merged[1]["kind"], "review");
}

#[test]
fn pr_comments_flatten_paginated_arrays() {
    let pages = json!([
        [{"id": 1}],
        [{"id": 2}, {"id": 3}]
    ]);

    let flattened = flatten_paginated_array(pages, "comments").expect("flat");

    assert_eq!(flattened.len(), 3);
    assert_eq!(flattened[0]["id"], 1);
    assert_eq!(flattened[2]["id"], 3);
}

#[test]
fn pr_review_digest_filters_gemini_and_builds_candidates() {
    let comments = vec![
        json!({
            "kind": "inline_comment",
            "id": 11,
            "author": "gemini-code-assist",
            "path": "crates/tachi-server/src/gh_ops.rs",
            "line": 42,
            "body": "![medium](https://www.gstatic.com/codereviewagent/medium-priority.svg)\nPlease add coverage for paginated comments.",
            "created_at": "2026-06-06T10:05:00Z",
            "url": "https://example.test/comment/11",
        }),
        json!({
            "kind": "inline_comment",
            "id": 12,
            "author": "human-reviewer",
            "path": "README.md",
            "line": 1,
            "body": "Looks good.",
        }),
    ];

    let digest = build_pr_review_digest("o/r", 202, "gemini", &comments);

    assert_eq!(digest["comment_count"], 1);
    assert_eq!(digest["counts"]["tests"], 1);
    assert_eq!(digest["items"][0]["verdict"], "needs_leader_verdict");
    assert_eq!(
        digest["items"][0]["summary"],
        "Please add coverage for paginated comments."
    );
    assert_eq!(digest["memory_candidates"].as_array().unwrap().len(), 1);
    assert_eq!(digest["handbook_candidates"].as_array().unwrap().len(), 1);
    assert!(digest["handbook_candidates"][0]["rule"]
        .as_str()
        .unwrap()
        .contains("regression coverage"));
    let destinations = digest["routing_plan"]["items"][0]["destinations"]
        .as_array()
        .unwrap();
    for expected in [
        "pr_comment",
        "github_issue",
        "feedback_rule",
        "guide",
        "project_wiki",
        "eval",
    ] {
        assert!(
            destinations.iter().any(|value| value == expected),
            "missing {expected} in {destinations:#?}"
        );
    }
    assert_eq!(
        digest["items"][0]["routing"]["primary_destination"],
        json!("github_issue")
    );
    assert_eq!(
        digest["items"][0]["routing"]["promotion_requires"],
        json!("leader_verdict")
    );
    assert_eq!(
        digest["routing_plan"]["status"],
        json!("needs_leader_verdict")
    );
    assert_eq!(
        digest["routing_plan"]["destination_counts"]["feedback_rule"],
        1
    );
}

#[test]
fn pr_review_digest_keeps_style_out_of_handbook_candidates() {
    let comments = vec![json!({
        "kind": "inline_comment",
        "id": 21,
        "author": "gemini-code-assist",
        "path": "src/lib.rs",
        "line": 7,
        "body": "Nit: this naming is a little unclear.",
    })];

    let digest = build_pr_review_digest("o/r", 7, "gemini", &comments);

    assert_eq!(digest["counts"]["style"], 1);
    assert_eq!(digest["memory_candidates"].as_array().unwrap().len(), 1);
    assert_eq!(digest["handbook_candidates"].as_array().unwrap().len(), 0);
    assert_eq!(
        digest["items"][0]["routing"]["primary_destination"],
        json!("pr_comment")
    );
    let destinations = digest["routing_plan"]["items"][0]["destinations"]
        .as_array()
        .unwrap();
    assert!(destinations.iter().any(|value| value == "pr_comment"));
    assert!(destinations.iter().any(|value| value == "eval"));
    assert!(!destinations.iter().any(|value| value == "feedback_rule"));
    assert!(digest["routing_plan"]["destination_counts"]
        .get("feedback_rule")
        .is_none());
}

#[test]
fn pr_review_digest_artifacts_write_json_and_markdown() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    let original = std::env::var_os("TACHI_REVIEW_ROOT");
    std::env::set_var("TACHI_REVIEW_ROOT", tmp.path());
    let comments = vec![json!({
        "kind": "inline_comment",
        "id": 31,
        "author": "gemini-code-assist",
        "path": "src/lib.rs",
        "line": 9,
        "body": "Incorrect state handling can cause a regression.",
    })];
    let digest = build_pr_review_digest("owner/repo", 31, "gemini", &comments);

    let artifacts = write_pr_review_digest_artifacts(&digest).unwrap();

    let md_path = PathBuf::from(artifacts["digest_md_path"].as_str().unwrap());
    let json_path = PathBuf::from(artifacts["digest_json_path"].as_str().unwrap());
    assert!(md_path.exists());
    assert!(json_path.exists());
    let markdown = std::fs::read_to_string(md_path).unwrap();
    assert!(markdown.contains("Triage Contract"));
    assert!(markdown.contains("Review Output Routing"));
    assert!(markdown.contains("Primary route:"));
    assert!(markdown.contains("needs_leader_verdict"));
    let leftovers: Vec<_> = std::fs::read_dir(json_path.parent().unwrap())
        .unwrap()
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with("digest.json.tmp.") || name.starts_with("digest.md.tmp."))
        .collect();
    assert!(
        leftovers.is_empty(),
        "digest artifact writes should not leave temp files: {leftovers:?}"
    );
    if let Some(v) = original {
        std::env::set_var("TACHI_REVIEW_ROOT", v);
    } else {
        std::env::remove_var("TACHI_REVIEW_ROOT");
    }
}
