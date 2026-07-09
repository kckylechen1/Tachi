use super::*;

#[test]
fn parse_issue_ref_owner_repo_form() {
    let t = parse_issue_ref("owner/repo#123", None).expect("owner/repo#N");
    assert_eq!(t.repo, "owner/repo");
    assert_eq!(t.number, 123);
    // trailing slash + surrounding whitespace are trimmed
    let t = parse_issue_ref("  owner/repo#5/ ", None).expect("trimmed");
    assert_eq!((t.repo.as_str(), t.number), ("owner/repo", 5));
}

#[test]
fn parse_issue_ref_url_and_hash_forms() {
    let t = parse_issue_ref("https://github.com/o/r/issues/7", None).expect("issue url");
    assert_eq!((t.repo.as_str(), t.number), ("o/r", 7));
    // bare #N needs a valid default repo
    let t = parse_issue_ref("#9", Some("o/r")).expect("#N with default");
    assert_eq!((t.repo.as_str(), t.number), ("o/r", 9));
}

#[test]
fn parse_issue_ref_rejects_bad_input() {
    assert!(parse_issue_ref("", None).is_none());
    assert!(parse_issue_ref("#9", None).is_none(), "no default repo");
    assert!(parse_issue_ref("#9", Some("noslash")).is_none(), "bad repo");
    assert!(
        parse_issue_ref("owner/repo#abc", None).is_none(),
        "non-numeric"
    );
    assert!(parse_issue_ref("a/b/c#1", None).is_none(), "two slashes");
    // a PR URL is not an issue ref
    assert!(parse_issue_ref("https://github.com/o/r/pull/7", None).is_none());
}

#[test]
fn parse_pr_ref_forms_and_rejections() {
    let t = parse_pr_ref("owner/repo#42").expect("owner/repo#N");
    assert_eq!((t.repo.as_str(), t.number), ("owner/repo", 42));
    let t = parse_pr_ref("https://github.com/o/r/pull/8").expect("pull url");
    assert_eq!((t.repo.as_str(), t.number), ("o/r", 8));
    // an issue URL is not a PR ref; bare #N has no repo
    assert!(parse_pr_ref("https://github.com/o/r/issues/8").is_none());
    assert!(parse_pr_ref("#8").is_none());
    assert!(parse_pr_ref("").is_none());
}

#[test]
fn slug_for_branch_normalizes_and_floors() {
    assert_eq!(slug_for_branch("Fix the Bug"), "fix-the-bug");
    assert_eq!(slug_for_branch("a   b"), "a-b"); // runs collapse to one dash
    assert_eq!(slug_for_branch("feature/foo:bar"), "feature-foo-bar");
    assert_eq!(slug_for_branch("  Lead"), "lead"); // no leading dash
    assert_eq!(slug_for_branch(""), "work"); // empty floor
    assert_eq!(slug_for_branch("!!!"), "work"); // no alphanumerics
    let long = slug_for_branch(&"x".repeat(60));
    assert!(long.len() <= 48 && !long.ends_with('-'));
}

#[test]
fn normalize_issue_ref_roundtrips_or_errors() {
    assert_eq!(
        normalize_issue_ref("https://github.com/o/r/issues/3").unwrap(),
        "o/r#3"
    );
    assert_eq!(normalize_issue_ref("o/r#3").unwrap(), "o/r#3");
    assert!(normalize_issue_ref("#3").is_err(), "no default repo");
    assert!(normalize_issue_ref("garbage").is_err());
}

#[test]
fn initial_merge_state_maps_pr_state() {
    assert_eq!(initial_merge_state_for_pr(Some("MERGED")), "merged");
    assert_eq!(initial_merge_state_for_pr(Some("merged")), "merged"); // case-insensitive
    assert_eq!(initial_merge_state_for_pr(Some("CLOSED")), "blocked");
    assert_eq!(initial_merge_state_for_pr(Some("OPEN")), "pending");
    assert_eq!(initial_merge_state_for_pr(None), "pending");
}

#[test]
fn string_array_field_trims_and_guards() {
    let v = json!({ "items": ["x", " y ", "", "  ", "z"] });
    assert_eq!(string_array_field(&v, "items"), vec!["x", "y", "z"]);
    assert!(string_array_field(&v, "missing").is_empty());
    assert!(string_array_field(&json!({ "items": "notarray" }), "items").is_empty());
}

#[test]
fn extract_markdown_paths_only_doc_md() {
    let got = extract_markdown_paths("edit `docs/a.md`, also lib.md, and src/docs/b.md done");
    assert_eq!(got, vec!["docs/a.md", "src/docs/b.md"]);
    // a bare README.md (no docs/ prefix, no /docs/ segment) is excluded
    assert!(extract_markdown_paths("README.md changelog.md").is_empty());
}

#[test]
fn dedupe_strings_keeps_first_occurrence() {
    let mut v = vec![
        "a".to_string(),
        "b".to_string(),
        "a".to_string(),
        "c".to_string(),
        "b".to_string(),
    ];
    dedupe_strings(&mut v);
    assert_eq!(v, vec!["a", "b", "c"]);
}
