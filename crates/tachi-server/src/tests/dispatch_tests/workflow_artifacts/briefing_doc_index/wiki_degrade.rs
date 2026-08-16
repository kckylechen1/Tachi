use super::*;

fn leftover_wiki_task_sentinel() -> MemoryEntry {
    let mut entry = make_entry("leftover-wiki-task-brief-sentinel");
    entry.path = "/scratch/1761-leftover-wiki-task".to_string();
    entry.summary = "LeftoverWikiTaskBriefSentinel".to_string();
    entry.text =
        "LeftoverWikiTaskBriefSentinel lives in global so task brief can survive a wiki refuse"
            .to_string();
    entry.keywords = vec!["LeftoverWikiTaskBriefSentinel".to_string()];
    entry
}

/// Break caught: leftover schema-28 wiki made `tachi_task_brief` `?` the
/// wiki open. Memory hits must survive; `wiki_warning` must name the refuse.
#[tokio::test]
async fn tachi_task_brief_degrades_when_leftover_wiki_refuses_open() {
    let server = make_server();
    plant_leftover_shared_wiki(&server);
    server
        .with_global_store(|store| {
            store
                .upsert(&leftover_wiki_task_sentinel())
                .map_err(|error| error.to_string())
        })
        .expect("seed leftover-wiki task-brief sentinel");

    let response = server
        .tachi_task_brief(Parameters(TaskBriefParams {
            task: "LeftoverWikiTaskBriefSentinel".to_string(),
            agent_id: Some("copilot".to_string()),
            project: None,
            path_prefix: None,
            domain: None,
            top_k: 3,
        }))
        .await
        .expect("task brief must not hard-fail when leftover wiki refuses open");
    let json: Value = serde_json::from_str(&response).expect("task brief JSON");
    assert_eq!(json["status"], "ok", "{json:#}");
    assert!(
        json["wiki_hits"]
            .as_array()
            .is_some_and(|hits| hits.is_empty()),
        "wiki hits must be empty on leftover refuse: {json:#}"
    );
    assert!(
        json["wiki_warning"]
            .as_str()
            .is_some_and(|warning| warning.contains("wiki recall unavailable")),
        "task brief must name the leftover wiki refuse: {json:#}"
    );
    assert!(
        json["memory_hits"].as_array().is_some_and(|hits| hits
            .iter()
            .any(|row| row["id"] == json!("leftover-wiki-task-brief-sentinel"))),
        "memory hits must survive leftover wiki refuse: {json:#}"
    );
}

/// Same leftover against the folded `tachi_task(action='brief')` route.
#[tokio::test]
async fn tachi_task_feature_brief_degrades_when_leftover_wiki_refuses_open() {
    let server = make_server();
    plant_leftover_shared_wiki(&server);
    server
        .with_global_store(|store| {
            store
                .upsert(&leftover_wiki_task_sentinel())
                .map_err(|error| error.to_string())
        })
        .expect("seed leftover-wiki feature-brief sentinel");

    let mut params = task_params("brief");
    params.task = Some("LeftoverWikiTaskBriefSentinel".to_string());
    params.include_global = true;
    params.format = Some("json".to_string());

    let response = server
        .tachi_task(Parameters(params))
        .await
        .expect("feature brief must not hard-fail when leftover wiki refuses open");
    let json: Value = serde_json::from_str(&response).expect("feature brief JSON");
    assert_eq!(json["status"], "ok", "{json:#}");
    assert!(
        json["wiki_warning"]
            .as_str()
            .is_some_and(|warning| warning.contains("wiki recall unavailable")),
        "feature brief must name the leftover wiki refuse: {json:#}"
    );
    assert!(
        json.get("wiki_hits")
            .and_then(Value::as_array)
            .is_none_or(|hits| hits.is_empty()),
        "feature brief must not project leftover wiki hits: {json:#}"
    );
}
