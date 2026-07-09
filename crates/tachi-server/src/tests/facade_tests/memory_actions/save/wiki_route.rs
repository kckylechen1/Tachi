use super::*;

#[tokio::test]
async fn tachi_memory_save_kind_wiki_routes_to_wiki() {
    let server = make_server();
    let mut params = tachi_memory_params("save");
    params.format = Some("json".to_string());
    params.kind = Some("wiki".to_string());
    params.title = Some("Memory facade wiki route".to_string());
    params.text = Some(
        "The memory facade should accept explicit kind=wiki and route to the wiki writer."
            .to_string(),
    );
    params.summary = Some("Memory facade wiki route".to_string());
    params.path = Some("/wiki/agent/tachi/memory-facade-wiki-route".to_string());
    params.category = Some("experience".to_string());
    params.keywords = vec!["facade".to_string(), "wiki".to_string()];
    params.scope = Some("global".to_string());
    params.retention_policy = Some("permanent".to_string());
    params.force = true;

    let body = crate::facade_memory_ops::handle_tachi_memory(&server, params)
        .await
        .expect("explicit kind=wiki should route through wiki write");
    let parsed: Value = serde_json::from_str(&body).expect("wiki save JSON");

    assert_eq!(
        parsed["wiki_path"],
        json!("/wiki/agent/tachi/memory-facade-wiki-route")
    );
}
