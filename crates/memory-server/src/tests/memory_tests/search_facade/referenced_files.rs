use super::*;

#[tokio::test]
async fn tachi_search_surfaces_referenced_files() {
    let server = make_server();
    server
        .with_global_store(|store| {
            let mut memory = make_entry("row-with-files");
            memory.path = "/facts/spec-pointer".to_string();
            memory.text = "UniqueFilesNeedle references a spec.".to_string();
            memory.summary = "Row with referenced files".to_string();
            memory.metadata = json!({
                "files": ["docs/SPEC.md", "crates/memory-server/src/lib.rs"]
            });
            store.upsert(&memory).map_err(|e| e.to_string())?;

            let mut bare = make_entry("row-without-files");
            bare.path = "/facts/no-pointer".to_string();
            bare.text = "UniqueFilesNeedle without any files.".to_string();
            bare.summary = "Row without files".to_string();
            store.upsert(&bare).map_err(|e| e.to_string())
        })
        .expect("seed referenced-files entries");

    let response = server
        .tachi_search(Parameters(TachiSearchParams {
            query: "UniqueFilesNeedle".to_string(),
            scope: "memory".to_string(),
            top_k: 5,
            path_prefix: None,
            project: None,
            domain: None,
            file_context: None,
            error_context: None,
            category: None,
            include_archived: false,
            include_training: false,
            enable_rerank: false,
            as_of: None,
        }))
        .await
        .expect("memory scoped search");

    assert!(
        response.contains("📎"),
        "expected referenced-files marker in: {response}"
    );
    assert!(
        response.contains("docs/SPEC.md"),
        "expected referenced file path in: {response}"
    );
}
