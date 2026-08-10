use super::*;

/// tachi#1575 fix-round: the folded `tachi_task(action='brief')` route
/// reaches `handle_tachi_feature_briefing`
/// (`tools/task_router.rs`), whose memory/eval searches run
/// `project_only=true` by default (`!params.include_global`). That mode
/// falls through to workspace/bound-store resolution on a named-project miss
/// instead of erroring (`search_memory/rows.rs`) — the same shape
/// `facade_memory_ops::briefing_ops::handle_memory_briefing`'s companion
/// test (`facade_tests/briefing/project_routing/missing_project.rs`) already
/// pins for `tachi_memory(action='briefing')`. This is that same regression
/// pin for the task facade's brief route.
///
/// The sentinel lives ONLY in the bound project DB, so if the named-project
/// miss ever starts falling through to the bound store again, this call
/// stops erroring and starts answering with the sentinel's project's data —
/// making this test the one that must go red against that regression.
#[tokio::test]
async fn tachi_task_brief_names_a_missing_project_instead_of_answering_from_the_bound_db() {
    let (server, _project_db) =
        crate::tests::make_server_with_project_fixture("task-briefing-missing-project");
    let sentinel = "MissingProjectTaskBriefingDowngradeSentinel";
    let mut entry = make_entry("missing-project-task-briefing-downgrade-sentinel");
    entry.path = "/facade/missing-project-task-briefing".to_string();
    entry.summary = format!("{sentinel} summary");
    entry.text = format!("{sentinel} row that lives only in the bound project DB");
    entry.keywords = vec![sentinel.to_string()];
    server
        .with_project_store(|store| {
            store
                .upsert(&entry)
                .map_err(|e| format!("seed project row: {e}"))
        })
        .expect("seed project row");

    let mut params = task_params("brief");
    params.task = Some(sentinel.to_string());
    params.project = Some("ZqxvNoSuchNamedTaskBriefingProjectAnywhere".to_string());

    let err = server.tachi_task(Parameters(params)).await.expect_err(
        "a tachi_task briefing naming a project whose store does not exist must error, not \
         answer from the bound project DB",
    );

    assert!(
        err.contains("ZqxvNoSuchNamedTaskBriefingProjectAnywhere") && err.contains("not found"),
        "the failure must name the project the caller asked for: {err}"
    );
    assert!(
        !err.contains(sentinel),
        "the error must not leak rows from the bound store: {err}"
    );
}

/// Keep a second discriminator for the folded brief action: it must retain the
/// same named-project guard when the request carries a full-board shape.
#[tokio::test]
async fn tachi_task_brief_full_board_names_a_missing_project_instead_of_answering_from_the_bound_db(
) {
    let (server, _project_db) =
        crate::tests::make_server_with_project_fixture("task-doc-index-missing-project");
    let sentinel = "MissingProjectTaskDocIndexDowngradeSentinel";
    let mut entry = make_entry("missing-project-task-doc-index-downgrade-sentinel");
    entry.path = "/facade/missing-project-task-doc-index".to_string();
    entry.summary = format!("{sentinel} summary");
    entry.text = format!("{sentinel} row that lives only in the bound project DB");
    entry.keywords = vec![sentinel.to_string()];
    server
        .with_project_store(|store| {
            store
                .upsert(&entry)
                .map_err(|e| format!("seed project row: {e}"))
        })
        .expect("seed project row");

    let mut params = task_params("brief");
    params.compact = Some(false);
    params.task = Some(sentinel.to_string());
    params.project = Some("ZqxvNoSuchNamedTaskDocIndexProjectAnywhere".to_string());

    let err = server.tachi_task(Parameters(params)).await.expect_err(
        "a tachi_task brief naming a project whose store does not exist must error, not \
         answer from the bound project DB",
    );

    assert!(
        err.contains("ZqxvNoSuchNamedTaskDocIndexProjectAnywhere") && err.contains("not found"),
        "the failure must name the project the caller asked for: {err}"
    );
    assert!(
        !err.contains(sentinel),
        "the error must not leak rows from the bound store: {err}"
    );
}

/// tachi#1575 fix-round trim mismatch: the guard validates the TRIMMED name,
/// but (pre-fix) downstream searches received the untrimmed original —
/// `path_utils::is_canonical_project_identity` rejects any leading/trailing
/// whitespace, so the untrimmed lookup would itself silently miss and
/// re-trigger the exact same "no project matched" fallback this guard exists
/// to close. This server has NO bound project DB (`make_server()`), so if
/// the untrimmed name reaches the search layer, the project-only search
/// finds nothing and the call still SUCCEEDS with empty rows — masking the
/// bug behind a green response instead of a hard failure. Asserting the
/// sentinel is actually found (not just that the call doesn't error) is what
/// makes this a real regression pin on the trim, not just the guard.
#[tokio::test]
async fn tachi_task_briefing_trims_whitespace_padded_project_and_still_finds_it() {
    let server = make_server();
    let project_name = "TrimEndToEndTaskBriefingProject";
    crate::tests::create_named_project_db(&server.tachi_home_dir(), project_name);
    let sentinel = "TrimEndToEndTaskBriefingSentinel";
    let mut entry = make_entry("trim-end-to-end-task-briefing-sentinel");
    entry.path = "/facade/trim-end-to-end-task-briefing".to_string();
    entry.summary = format!("{sentinel} summary");
    entry.text = format!("{sentinel} row seeded in the named project DB");
    entry.keywords = vec![sentinel.to_string()];
    server
        .with_named_project_store(project_name, |store| {
            store
                .upsert(&entry)
                .map_err(|e| format!("seed named project row: {e}"))
        })
        .expect("seed named project row");

    let mut params = task_params("brief");
    params.task = Some(sentinel.to_string());
    params.project = Some(format!("  {project_name}  "));

    let raw = server
        .tachi_task(Parameters(params))
        .await
        .expect("a whitespace-padded but otherwise valid project must succeed once trimmed");
    let parsed: Value = serde_json::from_str(&raw).expect("briefing JSON");

    assert_eq!(
        parsed["scope"]["project"],
        json!(project_name),
        "the response must echo the TRIMMED identity, not the caller's padded string: {parsed:#}"
    );
    assert!(
        parsed["memory_fragments"]
            .as_array()
            .is_some_and(|rows| rows
                .iter()
                .any(|row| row["id"] == json!("trim-end-to-end-task-briefing-sentinel"))),
        "trimming must not stop the guarded identity from reaching the actual search: {parsed:#}"
    );
}

/// Empty-after-trim (`""`/whitespace-only) is a loud typed error, not a
/// silent fallback to "unnamed"/alias luck.
#[tokio::test]
async fn tachi_task_briefing_rejects_whitespace_only_project_loudly() {
    let server = make_server();
    let mut params = task_params("brief");
    params.project = Some("   ".to_string());

    let err = server
        .tachi_task(Parameters(params))
        .await
        .expect_err("a whitespace-only project must error, not silently fall back to unnamed");

    assert!(
        err.contains("cannot be empty"),
        "expected a typed empty-project error, got: {err}"
    );
}
