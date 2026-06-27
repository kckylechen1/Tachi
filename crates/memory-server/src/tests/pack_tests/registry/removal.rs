use super::*;

#[tokio::test]
async fn test_pack_remove() {
    let server = make_server();

    // Register then remove
    server
        .pack_register(Parameters(PackRegisterParams {
            id: "removable".to_string(),
            name: Some("Removable Pack".to_string()),
            source: None,
            version: None,
            description: None,
            local_path: None,
            metadata: None,
        }))
        .await
        .expect("register removable");

    let result = server
        .pack_remove(Parameters(PackRemoveParams {
            id: "removable".to_string(),
            clean_files: Some(false),
        }))
        .await
        .expect("pack_remove should succeed");

    let json: Value = serde_json::from_str(&result).unwrap();
    assert_eq!(json["status"], "removed");

    // pack_get should fail
    let err = server
        .pack_get(Parameters(PackGetParams {
            id: "removable".to_string(),
        }))
        .await;
    assert!(err.is_err(), "pack_get after remove should fail");
}

#[tokio::test]
async fn test_pack_get_not_found() {
    let server = make_server();

    let err = server
        .pack_get(Parameters(PackGetParams {
            id: "nonexistent/pack".to_string(),
        }))
        .await;
    assert!(err.is_err(), "pack_get for nonexistent should fail");
    assert!(
        err.unwrap_err().contains("not found"),
        "error should mention not found"
    );
}
