use super::*;

#[tokio::test]
async fn tachi_memory_get_hides_archived_by_default_and_returns_it_on_explicit_opt_in() {
    let server = make_server();
    let mut entry = make_entry("facade-get-archived");
    entry.path = "/scratch/facade-get-archived".to_string();
    entry.text = "archived provenance remains addressable in its bound library".to_string();
    server
        .with_global_store(|store| {
            store.upsert(&entry).map_err(|error| error.to_string())?;
            store
                .archive_memory(&entry.id)
                .map_err(|error| error.to_string())?;
            Ok::<(), String>(())
        })
        .expect("seed archived row");

    let mut hidden = tachi_memory_params("get");
    hidden.format = Some("json".to_string());
    hidden.id = Some(entry.id.clone());
    let hidden = crate::facade_memory_ops::handle_tachi_memory(&server, hidden)
        .await
        .expect("default get response");
    assert_eq!(
        serde_json::from_str::<Value>(&hidden).expect("default get JSON")["error"],
        json!("Memory not found")
    );

    let mut included = tachi_memory_params("get");
    included.format = Some("json".to_string());
    included.id = Some(entry.id);
    included.include_archived = true;
    let included = crate::facade_memory_ops::handle_tachi_memory(&server, included)
        .await
        .expect("archived opt-in get response");
    let included: Value = serde_json::from_str(&included).expect("archived get JSON");
    assert_eq!(included["id"], json!("facade-get-archived"));
    assert_eq!(included["archived"], json!(true));
    assert_eq!(included["db"], json!("global"));
}
