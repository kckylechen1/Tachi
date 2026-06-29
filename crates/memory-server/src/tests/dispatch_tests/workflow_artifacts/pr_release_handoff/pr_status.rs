use super::*;

#[test]
fn tachi_task_pr_status_parses_repo_number_and_pr_ref() {
    let mut params = task_params("pr_status");
    params.repo = Some("kckylechen1/tachi".to_string());
    params.number = Some(228);
    assert_eq!(
        crate::tools::resolve_task_pr_status_target(&params).expect("repo+number"),
        ("kckylechen1/tachi".to_string(), 228)
    );

    params.repo = None;
    params.number = None;
    params.pr_ref = Some("kckylechen1/tachi#228".to_string());
    assert_eq!(
        crate::tools::resolve_task_pr_status_target(&params).expect("owner/repo#number"),
        ("kckylechen1/tachi".to_string(), 228)
    );

    params.pr_ref = Some("https://github.com/kckylechen1/tachi/pull/228".to_string());
    assert_eq!(
        crate::tools::resolve_task_pr_status_target(&params).expect("github PR URL"),
        ("kckylechen1/tachi".to_string(), 228)
    );

    params.pr_ref = Some(" https://github.com/kckylechen1/tachi/pull/228/ ".to_string());
    assert_eq!(
        crate::tools::resolve_task_pr_status_target(&params).expect("trimmed github PR URL"),
        ("kckylechen1/tachi".to_string(), 228)
    );
}

#[test]
fn tachi_task_pr_status_rejects_ambiguous_pr_refs() {
    for pr_ref in [
        "",
        "kckylechen1/tachi#",
        "kckylechen1/tachi/extra#228",
        "https://github.com/kckylechen1/tachi/issues/228",
        "https://github.com/kckylechen1/tachi/pull/228/files",
    ] {
        let mut params = task_params("pr_status");
        params.pr_ref = Some(pr_ref.to_string());
        assert!(
            crate::tools::resolve_task_pr_status_target(&params).is_err(),
            "unexpectedly accepted pr_ref={pr_ref:?}"
        );
    }
}

#[test]
fn tachi_task_pr_status_builds_safe_merge_preview_params() {
    let mut params = task_params("pr_status");
    params.pr_ref = Some("kckylechen1/tachi#228".to_string());
    params.flow_id = Some("flow_pr_status".to_string());
    params.merge_policy = Some("strict".to_string());
    params.strategy = Some("squash".to_string());
    params.confirm = true;

    let gh_params =
        crate::tools::build_task_pr_status_gh_params(&params).expect("pr_status params");
    assert_eq!(gh_params.action, "safe_merge");
    assert_eq!(gh_params.repo.as_deref(), Some("kckylechen1/tachi"));
    assert_eq!(gh_params.number, Some(228));
    assert_eq!(gh_params.dry_run, Some(true));
    assert!(!gh_params.confirm);
    assert_eq!(gh_params.flow_id.as_deref(), Some("flow_pr_status"));
    assert_eq!(gh_params.merge_policy.as_deref(), Some("strict"));
    assert_eq!(gh_params.merge_strategy, None);
}

#[tokio::test]
async fn tachi_task_pr_status_requires_repo_number_or_parseable_ref() {
    let server = make_server();
    let params = task_params("pr_status");
    let err = server
        .tachi_task(Parameters(params))
        .await
        .expect_err("missing target should fail before GitHub access");
    assert_eq!(
        err,
        "pr_status requires either repo+number or pr_ref='owner/repo#123' / GitHub PR URL"
    );
}
