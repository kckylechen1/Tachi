use super::*;

fn leftover_wiki_sentinel_entry() -> memcore::MemoryEntry {
    let mut entry = make_entry("leftover-wiki-briefing-sentinel");
    entry.path = "/scratch/1761-leftover-wiki".to_string();
    entry.summary = "LeftoverWikiBriefingSentinel".to_string();
    entry.text =
        "LeftoverWikiBriefingSentinel lives in global so briefing can survive a wiki refuse"
            .to_string();
    entry.keywords = vec!["LeftoverWikiBriefingSentinel".to_string()];
    entry
}

fn seed_project_sentinel(server: &crate::server_state::MemoryServer) {
    server
        .with_project_store(|store| {
            store
                .upsert(&leftover_wiki_sentinel_entry())
                .map_err(|error| error.to_string())
        })
        .expect("seed leftover-wiki briefing sentinel");
}

fn health_warnings(response: &Value) -> Vec<String> {
    response["health"]["warnings"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|warning| warning.as_str().map(str::to_string))
        .collect()
}

/// Break caught: leftover schema-28 wiki made `tachi_memory` briefing `?`
/// the wiki open and kill session start. Memory/kanban must survive; the
/// refuse must stay visible in health warnings.
#[tokio::test]
async fn tachi_memory_briefing_degrades_when_leftover_wiki_refuses_open() {
    let (server, _project_db) = make_server_with_project_fixture("leftover-wiki-briefing");
    plant_leftover_shared_wiki(&server);
    seed_project_sentinel(&server);

    let mut params = tachi_memory_params("briefing");
    params.format = Some("json".to_string());
    params.query = Some("LeftoverWikiBriefingSentinel".to_string());
    params.compact = false;

    let body = crate::facade_memory_ops::handle_tachi_memory(&server, params)
        .await
        .expect("briefing must not hard-fail when leftover wiki refuses open");
    let parsed: Value = serde_json::from_str(&body).expect("briefing JSON");
    assert_eq!(parsed["status"], "completed", "{parsed:#}");
    assert!(
        parsed["memories"].as_array().is_some_and(|rows| rows
            .iter()
            .any(|row| row["id"] == json!("leftover-wiki-briefing-sentinel"))),
        "memory section must survive leftover wiki refuse: {parsed:#}"
    );
    assert_eq!(
        parsed["wiki"]
            .as_array()
            .map(Vec::len)
            .unwrap_or(usize::MAX),
        0,
        "wiki section must be empty on leftover refuse: {parsed:#}"
    );
    let warnings = health_warnings(&parsed);
    assert!(
        warnings
            .iter()
            .any(|warning| warning.contains("wiki recall unavailable")
                || warning.contains("wiki hygiene unavailable")),
        "leftover wiki refuse must be loud in health warnings: {warnings:?}\n{parsed:#}"
    );
}

/// Break caught: `tachi_memory` alerts died on the same leftover wiki
/// hygiene open. Alerts must return; the refuse must be a warning.
#[tokio::test]
async fn tachi_memory_alerts_degrade_when_leftover_wiki_hygiene_refuses() {
    let server = make_server();
    plant_leftover_shared_wiki(&server);

    let mut params = tachi_memory_params("alerts");
    params.format = Some("json".to_string());

    let body = crate::facade_memory_ops::handle_tachi_memory(&server, params)
        .await
        .expect("alerts must not hard-fail when leftover wiki hygiene refuses");
    let parsed: Value = serde_json::from_str(&body).expect("alerts JSON");
    assert_eq!(parsed["status"], "completed", "{parsed:#}");
    let warnings = parsed["warnings"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|warning| warning.as_str())
        .collect::<Vec<_>>();
    assert!(
        warnings
            .iter()
            .any(|warning| warning.contains("wiki hygiene unavailable")),
        "leftover wiki hygiene refuse must be loud: {warnings:?}\n{parsed:#}"
    );
}

/// The other side of the tray degrade: explicit wiki search stays a
/// schema-gate failure. Do not turn `tachi_wiki` into a silent empty hit.
#[tokio::test]
async fn tachi_wiki_search_stays_loud_when_leftover_wiki_refuses_open() {
    let server = make_server();
    plant_leftover_shared_wiki(&server);

    let params: crate::tool_params::TachiWikiParams = serde_json::from_value(json!({
        "action": "search",
        "query": "LeftoverWikiBriefingSentinel",
    }))
    .expect("wiki search params");
    let err = server
        .tachi_wiki(Parameters(params))
        .await
        .expect_err("explicit wiki search must stay loud on leftover schema refuse");
    assert!(
        err.contains("refusing to migrate") || err.contains("SchemaMigration"),
        "wiki search must name the schema gate, not look like an empty hit: {err}"
    );
}
