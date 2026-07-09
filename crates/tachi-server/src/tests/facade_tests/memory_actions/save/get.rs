use super::*;

#[tokio::test]
async fn tachi_memory_get_action_returns_saved_entry() {
    let server = make_server();
    let mem_id = "facade-get-action-001";

    let mut save = tachi_memory_params("save");
    save.format = Some("json".to_string());
    save.scope = Some("project".to_string());
    save.text = Some("Facade get action should return the full saved memory text.".to_string());
    save.summary = Some("Facade get action".to_string());
    save.path = Some("/scratch/tachi/facade-get-action".to_string());
    save.id = Some(mem_id.to_string());
    save.force = true;
    crate::facade_memory_ops::handle_tachi_memory(&server, save)
        .await
        .expect("save should succeed");

    let mut get = tachi_memory_params("get");
    get.format = Some("json".to_string());
    get.id = Some(mem_id.to_string());
    let body = crate::facade_memory_ops::handle_tachi_memory(&server, get)
        .await
        .expect("get should succeed");
    let parsed: Value = serde_json::from_str(&body).expect("get response json");

    assert_eq!(parsed["id"], json!(mem_id));
    assert_eq!(parsed["path"], json!("/scratch/tachi/facade-get-action"));
    assert!(
        parsed["text"]
            .as_str()
            .is_some_and(|text| text.contains("full saved memory text")),
        "expected full text in get response: {body}"
    );
}
