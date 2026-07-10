use super::*;
use crate::tool_params::TachiSaveParams;

const ECHO_SENTINEL: &str = "ZX9-ECHO-SENTINEL";
const MEMORY_SAVE_SENTINEL: &str = "ZX9-MEMORY-SAVE-SENTINEL";

fn memory_save_params(text: String, path: &str, format: Option<&str>) -> TachiMemoryParams {
    TachiMemoryParams {
        action: "save".to_string(),
        format: format.map(str::to_string),
        query: None,
        scope: Some("project".to_string()),
        top_k: 6,
        path_prefix: None,
        file_context: None,
        error_context: None,
        category: Some("decision".to_string()),
        include_archived: false,
        include_training: false,
        enable_rerank: false,
        as_of: None,
        synthesize: false,
        model: None,
        agent_role: None,
        text: Some(text),
        title: None,
        summary: None,
        topic: None,
        keywords: vec!["issue527".to_string()],
        entities: Vec::new(),
        importance: Some(0.7),
        retention_policy: None,
        kind: Some("memory".to_string()),
        path: Some(path.to_string()),
        id: None,
        force: true,
        source: None,
        valid_from: None,
        valid_until: None,
        metadata: None,
        emit_continuity: false,
        files: Vec::new(),
        flow_id: None,
        event: None,
        state: None,
        project: None,
        domain: None,
        compact: false,
        proposal_id: None,
        review_status: None,
        notes: None,
        confirm: false,
        state_filter: None,
        content: None,
        ingest_type: "source".to_string(),
        source_url: None,
        auto_chunk: true,
        auto_summarize: true,
        auto_link: true,
        chunk_size_chars: 1200,
        chunk_overlap_chars: 120,
        conversation_id: None,
        turn_id: None,
        event_type: None,
        messages: Vec::new(),
    }
}

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
    assert!(
        default_resp.len() < 500,
        "save receipt too large: {} bytes",
        default_resp.len()
    );
    let receipt: Value = serde_json::from_str(&default_resp).expect("save receipt JSON");
    assert_eq!(receipt["ok"], json!(true));
    assert!(receipt.get("id").is_some());
    assert!(receipt.get("path").is_some());
    assert!(receipt.get("status").is_some());

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

#[tokio::test]
async fn issue527_tachi_memory_save_default_receipt_omits_echo_full_restores_it() {
    let server = make_server();
    let text = format!("Decision routed through tachi_memory save {MEMORY_SAVE_SENTINEL}");

    let default_resp = crate::facade_memory_ops::handle_tachi_memory(
        &server,
        memory_save_params(text.clone(), "/scratch/issue527-memory-save", None),
    )
    .await
    .expect("tachi_memory save default");
    assert!(
        !default_resp.contains(MEMORY_SAVE_SENTINEL),
        "default tachi_memory save receipt echoed input: {default_resp}"
    );
    assert!(
        default_resp.len() < 500,
        "tachi_memory save receipt too large: {} bytes: {default_resp}",
        default_resp.len()
    );
    let receipt: Value = serde_json::from_str(&default_resp).expect("memory save receipt JSON");
    assert_eq!(receipt["ok"], json!(true));
    assert!(receipt.get("id").is_some());
    assert_eq!(receipt["path"], json!("/scratch/issue527-memory-save"));
    assert!(receipt.get("echo").is_none());

    let full_resp = crate::facade_memory_ops::handle_tachi_memory(
        &server,
        memory_save_params(text, "/scratch/issue527-memory-save-full", Some("full")),
    )
    .await
    .expect("tachi_memory save full");
    assert!(
        full_resp.contains(MEMORY_SAVE_SENTINEL),
        "format=full tachi_memory save should preserve legacy echo: {full_resp}"
    );
}

#[tokio::test]
async fn g5_checkpoint_receipt_default_under_500_bytes() {
    let server = make_server();
    let resp = crate::facade_memory_ops::handle_tachi_memory(
        &server,
        TachiMemoryParams {
            action: "checkpoint".to_string(),
            format: Some("json".to_string()),
            query: None,
            scope: Some("project".to_string()),
            top_k: 6,
            path_prefix: None,
            file_context: None,
            error_context: None,
            category: None,
            include_archived: false,
            include_training: false,
            enable_rerank: false,
            as_of: None,
            synthesize: false,
            model: None,
            agent_role: None,
            text: Some("Checkpoint body for g528 budget.".to_string()),
            title: Some("G528 checkpoint".to_string()),
            summary: Some("summary should not echo".to_string()),
            topic: None,
            keywords: Vec::new(),
            entities: Vec::new(),
            importance: None,
            retention_policy: None,
            kind: None,
            path: None,
            id: None,
            force: false,
            source: None,
            valid_from: None,
            valid_until: None,
            flow_id: None,
            event: None,
            state: None,
            project: None,
            domain: Some("engineering".to_string()),
            metadata: None,
            emit_continuity: false,
            compact: false,
            files: Vec::new(),
            proposal_id: None,
            review_status: None,
            notes: None,
            confirm: false,
            state_filter: None,
            content: None,
            ingest_type: "source".to_string(),
            source_url: None,
            auto_chunk: true,
            auto_summarize: true,
            auto_link: true,
            chunk_size_chars: 1200,
            chunk_overlap_chars: 120,
            conversation_id: None,
            turn_id: None,
            event_type: None,
            messages: Vec::new(),
        },
    )
    .await
    .expect("checkpoint");
    assert!(
        resp.len() < 500,
        "checkpoint receipt too large: {} bytes: {resp}",
        resp.len()
    );
    let receipt: Value = serde_json::from_str(&resp).expect("checkpoint receipt JSON");
    assert_eq!(receipt["ok"], json!(true));
    assert!(!resp.contains("summary should not echo"));
}
