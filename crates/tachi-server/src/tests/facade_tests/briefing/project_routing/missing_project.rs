use super::*;

/// tachi#1575 (child of #1539's invariant: store/project resolution fails
/// loud, no surface silently answers from the wrong store).
///
/// Companion to `memory_search.rs`'s
/// `tachi_memory_search_names_a_missing_project_instead_of_answering_from_the_bound_db`:
/// briefing must diverge the same way search does when the caller names a
/// project whose DB does not exist, instead of silently answering from the
/// process's already-bound project DB (the `project_only` search path
/// `search_memory/rows.rs` uses for briefing falls through to
/// workspace-derived resolution on a miss rather than erroring — briefing
/// must reject the explicit name before that fallback runs).
///
/// The sentinel lives ONLY in the bound project DB, so if the named-project
/// miss ever starts falling through to the bound store again, this call
/// stops erroring and starts answering with the sentinel's project's data —
/// making this test the one that must go red against that regression.
#[tokio::test]
async fn tachi_memory_briefing_names_a_missing_project_instead_of_answering_from_the_bound_db() {
    let (server, _project_db) =
        crate::tests::make_server_with_project_fixture("briefing-missing-project");
    let sentinel = "MissingProjectBriefingDowngradeSentinel";
    let mut entry = make_entry("missing-project-briefing-downgrade-sentinel");
    entry.path = "/facade/missing-project-briefing".to_string();
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

    let mut params = tachi_memory_params("briefing");
    params.format = Some("json".to_string());
    params.query = Some(sentinel.to_string());
    params.project = Some("ZqxvNoSuchNamedBriefingProjectAnywhere".to_string());

    let err = crate::facade_memory_ops::handle_tachi_memory(&server, params)
        .await
        .expect_err(
            "a briefing naming a project whose store does not exist must error, not answer \
             from the bound project DB",
        );

    assert!(
        err.contains("ZqxvNoSuchNamedBriefingProjectAnywhere") && err.contains("not found"),
        "the failure must name the project the caller asked for: {err}"
    );
    assert!(
        !err.contains(sentinel),
        "the error must not leak rows from the bound store: {err}"
    );
}

/// Discrimination pin: the workspace/bound-store fallback this issue is
/// about is legitimate when the caller did NOT name a project — briefing
/// omitting `project` must still resolve to (and search) the bound project
/// DB unchanged. Only an explicit, nonexistent name must error.
#[tokio::test]
async fn tachi_memory_briefing_still_falls_back_to_bound_project_db_when_no_project_named() {
    let (server, _project_db) =
        crate::tests::make_server_with_project_fixture("briefing-no-explicit-project");
    let sentinel = "NoExplicitProjectBriefingSentinel";
    let mut entry = make_entry("no-explicit-project-briefing-sentinel");
    entry.path = "/facade/no-explicit-project-briefing".to_string();
    entry.summary = format!("{sentinel} summary");
    entry.text = format!("{sentinel} row reachable via the bound project DB");
    entry.keywords = vec![sentinel.to_string()];
    server
        .with_project_store(|store| {
            store
                .upsert(&entry)
                .map_err(|e| format!("seed project row: {e}"))
        })
        .expect("seed project row");

    let mut params = tachi_memory_params("briefing");
    params.format = Some("json".to_string());
    params.query = Some(sentinel.to_string());
    params.project = None;

    let body = crate::facade_memory_ops::handle_tachi_memory(&server, params)
        .await
        .expect("briefing with no explicit project must still succeed via bound-store fallback");
    let parsed: Value = serde_json::from_str(&body).expect("briefing JSON");
    assert!(
        parsed["memories"].as_array().is_some_and(|rows| rows
            .iter()
            .any(|row| row["id"] == json!("no-explicit-project-briefing-sentinel"))),
        "unscoped briefing must still search the bound project DB: {parsed:#}"
    );
}
