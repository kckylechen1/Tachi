use serde_json::json;

use super::permission::native_permission_response;

#[test]
fn native_permission_approves_read_like_request() {
    let response = native_permission_response(
        "approve-reads",
        &json!({
            "toolCall": {
                "kind": "read",
                "title": "Read src/lib.rs"
            },
            "options": [
                {"optionId": "deny", "name": "Deny"},
                {"optionId": "allow", "name": "Allow"}
            ]
        }),
    );

    assert_eq!(response["outcome"]["outcome"], json!("selected"));
    assert_eq!(response["outcome"]["optionId"], json!("allow"));
}

#[test]
fn native_permission_denies_write_like_request() {
    let response = native_permission_response(
        "approve-reads",
        &json!({
            "toolCall": {
                "kind": "edit",
                "title": "Write src/lib.rs"
            },
            "options": [
                {"optionId": "allow", "name": "Allow"},
                {"optionId": "deny", "name": "Deny"}
            ]
        }),
    );

    assert_eq!(response["outcome"]["outcome"], json!("selected"));
    assert_eq!(response["outcome"]["optionId"], json!("deny"));
}
