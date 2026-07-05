use super::*;
use crate::tool_params::TachiSaveParams;

const ECHO_SENTINEL: &str = "ZX9-ECHO-SENTINEL";

#[tokio::test]
async fn g3_save_default_omits_echo_sentinel_full_restores_it() {
    let server = make_server();
    let text = format!("Decision with {ECHO_SENTINEL} marker");

    let default_resp = server
        .tachi_save(Parameters(TachiSaveParams {
            text: text.clone(),
            id: None,
            kind: Some("memory".to_string()),
            title: None,
            summary: None,
            path: Some("/scratch/g528".to_string()),
            importance: Some(0.7),
            category: Some("decision".to_string()),
            keywords: vec!["g528".to_string()],
            entities: Vec::new(),
            scope: Some("project".to_string()),
            project: None,
            domain: None,
            retention_policy: None,
            force: true,
            references: Vec::new(),
            topic: None,
            source: None,
            valid_from: None,
            valid_until: None,
            metadata: None,
            emit_continuity: false,
            files: Vec::new(),
            format: Some("json".to_string()),
        }))
        .await
        .expect("save default");
    assert!(
        !default_resp.contains(ECHO_SENTINEL),
        "default save receipt echoed input: {default_resp}"
    );
    assert!(default_resp.len() < 600, "save receipt too large");

    let full_resp = server
        .tachi_save(Parameters(TachiSaveParams {
            text: text.clone(),
            id: None,
            kind: Some("memory".to_string()),
            title: None,
            summary: None,
            path: Some("/scratch/g528-full".to_string()),
            importance: Some(0.7),
            category: Some("decision".to_string()),
            keywords: vec!["g528".to_string()],
            entities: Vec::new(),
            scope: Some("project".to_string()),
            project: None,
            domain: None,
            retention_policy: None,
            force: true,
            references: Vec::new(),
            topic: None,
            source: None,
            valid_from: None,
            valid_until: None,
            metadata: None,
            emit_continuity: false,
            files: Vec::new(),
            format: Some("full".to_string()),
        }))
        .await
        .expect("save full");
    assert!(
        full_resp.contains(ECHO_SENTINEL),
        "full save should restore echo: {full_resp}"
    );
}