#[test]
fn tachi_task_intake_parses_issue_refs_without_accepting_pr_urls() {
    assert_eq!(
        crate::task_lifecycle::parse_issue_ref("kckylechen1/tachi#194", None),
        Some(crate::task_lifecycle::GithubTarget {
            repo: "kckylechen1/tachi".to_string(),
            number: 194,
        })
    );
    assert_eq!(
        crate::task_lifecycle::parse_issue_ref(
            "https://github.com/kckylechen1/tachi/issues/194/",
            None
        ),
        Some(crate::task_lifecycle::GithubTarget {
            repo: "kckylechen1/tachi".to_string(),
            number: 194,
        })
    );
    assert_eq!(
        crate::task_lifecycle::parse_issue_ref("#194", Some("kckylechen1/tachi")),
        Some(crate::task_lifecycle::GithubTarget {
            repo: "kckylechen1/tachi".to_string(),
            number: 194,
        })
    );
    assert_eq!(
        crate::task_lifecycle::parse_issue_ref(
            "https://github.com/kckylechen1/tachi/pull/194",
            None
        ),
        None
    );
}
