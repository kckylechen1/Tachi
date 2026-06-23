use super::*;

#[tokio::test]
async fn tachi_arena_facade_defaults_to_json_and_keeps_markdown_escape_hatch() {
    let server = server();

    let json_params: rmcp::handler::server::wrapper::Parameters<TachiArenaParams> =
        rmcp::handler::server::wrapper::Parameters(params("board"));
    let json_body: String = server
        .tachi_arena(json_params)
        .await
        .expect("default board should succeed");
    let parsed: Value = serde_json::from_str(&json_body).expect("default board JSON");
    assert_eq!(parsed["action"], json!("board"));

    let mut markdown_params = params("board");
    markdown_params.format = Some("markdown".to_string());
    let markdown_params: rmcp::handler::server::wrapper::Parameters<TachiArenaParams> =
        rmcp::handler::server::wrapper::Parameters(markdown_params);
    let markdown: String = server
        .tachi_arena(markdown_params)
        .await
        .expect("markdown board should succeed");
    assert!(markdown.starts_with("## Tachi arena board"), "{markdown}");
    assert!(markdown.contains("```json"), "{markdown}");
}
