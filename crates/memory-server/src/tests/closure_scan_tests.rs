use super::*;

/// Gap A + E: the cross-flow closure-debt scan flags flows whose work is done
/// but the loop never closed (no close_loop.json), and flows that closed with
/// an unresolved spec-drift advisory — while leaving cleanly-closed flows alone.
#[tokio::test]
async fn scan_open_loops_flags_unclosed_and_spec_drift() {
    let (_server, _home) = make_server_with_temp_home();
    let runs_root = crate::shell_ops::shell_runs_root();
    std::fs::create_dir_all(&runs_root).expect("create runs root");

    let unclosed = runs_root.join("flow_20260101T000000Z_unclosed_aaaa1111");
    std::fs::create_dir_all(&unclosed).unwrap();
    std::fs::write(unclosed.join("result.md"), "# Fixed the leak\nbody").unwrap();

    let clean = runs_root.join("flow_20260101T000000Z_clean_bbbb2222");
    std::fs::create_dir_all(&clean).unwrap();
    std::fs::write(clean.join("result.md"), "done").unwrap();
    std::fs::write(clean.join("close_loop.json"), "{}").unwrap();

    let drift = runs_root.join("flow_20260101T000000Z_drift_cccc3333");
    std::fs::create_dir_all(&drift).unwrap();
    std::fs::write(drift.join("result.md"), "done").unwrap();
    std::fs::write(
        drift.join("close_loop.json"),
        r#"{"closure_actions":{"spec_advisory":{"status":"advisory"}}}"#,
    )
    .unwrap();

    let debts = crate::shell_ops::scan_open_loops(8);
    let kind_for = |needle: &str| -> Option<String> {
        debts
            .iter()
            .find(|d| {
                d.get("flow_id")
                    .and_then(|f| f.as_str())
                    .is_some_and(|f| f.contains(needle))
            })
            .and_then(|d| d.get("kind").and_then(|k| k.as_str()).map(String::from))
    };
    assert_eq!(kind_for("unclosed").as_deref(), Some("unclosed_loop"));
    assert_eq!(kind_for("drift").as_deref(), Some("spec_drift"));
    assert!(
        kind_for("clean").is_none(),
        "cleanly-closed flow must not be flagged"
    );
}

/// Gap C: close_loop drafts the wiki title/body from the flow's result.md when
/// the caller omits them, so closing costs just a flow_id.
#[tokio::test]
async fn close_loop_drafts_wiki_from_result_when_missing() {
    let (server, _home) = make_server_with_temp_home();
    let flow_id = "flow_20260101T000000Z_draft_dddd4444";
    let run_dir = crate::shell_ops::run_dir_for_flow_id(flow_id).expect("run dir");
    std::fs::create_dir_all(&run_dir).unwrap();
    std::fs::write(
        run_dir.join("result.md"),
        "# Drafted lesson\nDetails of the fix.",
    )
    .unwrap();

    let resp = server
        .tachi_workflow(Parameters(TachiWorkflowParams {
            action: "close_loop".to_string(),
            issue_ref: Some("owner/repo#1".to_string()),
            pr_ref: None,
            doc_paths: vec![],
            spec_paths: vec![],
            related_issues: vec![],
            wiki_title: None,
            wiki_text: None,
            wiki_path: None,
            wiki_topic: None,
            wiki_summary: None,
            wiki_category: None,
            wiki_keywords: vec![],
            wiki_entities: vec![],
            wiki_importance: None,
            wiki_scope: None,
            wiki_domain: None,
            project: None,
            force: true,
            // Offline: do not attempt a real gh comment.
            post_comment: Some(false),
            flow_id: Some(flow_id.to_string()),
        }))
        .await
        .expect("close_loop should draft and succeed");

    let json: Value = serde_json::from_str(&resp).expect("json");
    assert_eq!(json["ok"], json!(true));
    assert_eq!(json["closure_actions"]["auto_drafted"], json!(true));
    // TACHI_DISABLE_LLM_DRAFT (set in ensure_test_env) forces the deterministic
    // fallback, so no network call is made and the source is the raw result.
    assert_eq!(json["closure_actions"]["draft_source"], json!("result_md"));
    // Drafted title (from the result.md heading) flowed into the comment body.
    assert!(json["closure_actions"]["comment_body"]
        .as_str()
        .is_some_and(|body| body.contains("Drafted lesson")));
}
