use super::*;

#[tokio::test]
async fn tachi_save_note_writes_markdown_file_and_normalizes_scope() {
    let (server, _temp_home) = make_server_with_temp_home();

    let saved = server
        .tachi_save(Parameters(TachiSaveParams {
            text: "中文 note body for UTF-8 slug safety".to_string(),
            id: None,
            kind: None,
            title: Some("中文 Note 标题".to_string()),
            summary: Some("note summary".to_string()),
            path: Some("brainstorm/demo.md".to_string()),
            importance: Some(0.7),
            category: None,
            keywords: vec!["brainstorm".to_string()],
            entities: Vec::new(),
            scope: Some("note".to_string()),
            project: None,
            domain: None,
            retention_policy: None,
            force: true,
            references: Vec::new(),
            topic: Some("notes-test".to_string()),
            source: None,
            valid_from: None,
            valid_until: None,
            metadata: None,
            emit_continuity: false,
            files: Vec::new(),
        }))
        .await
        .expect("tachi_save note should succeed");
    let json: serde_json::Value = serde_json::from_str(&saved).expect("save JSON");
    let note_file = json["note_file"].as_str().expect("note file returned");
    let note_path = json["note_path"].as_str().expect("note path returned");
    assert_eq!(note_path, "brainstorm/demo.md");
    let md = std::fs::read_to_string(note_file).expect("note markdown should exist");
    assert!(md.contains("title: \"中文 Note 标题\""));
    assert!(md.contains("中文 note body for UTF-8 slug safety"));

    let id = json["id"].as_str().expect("memory id returned").to_string();
    let fetched = server
        .get_memory(Parameters(GetMemoryParams {
            id,
            include_archived: false,
            project: None,
        }))
        .await
        .expect("get note memory");
    let fetched_json: serde_json::Value = serde_json::from_str(&fetched).expect("memory JSON");
    assert_eq!(
        fetched_json["path"],
        serde_json::json!("/notes/brainstorm/demo.md")
    );
    assert_ne!(fetched_json["scope"], serde_json::json!("note"));
}

#[tokio::test]
async fn tachi_save_note_rejects_paths_outside_notes_root() {
    let (server, _temp_home) = make_server_with_temp_home();

    for bad_path in ["/tmp/escape.md", "../escape.md", "brainstorm/../escape.md"] {
        let err = server
            .tachi_save(Parameters(TachiSaveParams {
                text: "bad path should not be saved".to_string(),
                id: None,
                kind: Some("note".to_string()),
                title: Some("bad path".to_string()),
                summary: None,
                path: Some(bad_path.to_string()),
                importance: None,
                category: None,
                keywords: Vec::new(),
                entities: Vec::new(),
                scope: None,
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
            }))
            .await
            .expect_err("invalid note path should be rejected");
        assert!(
            err.contains("relative") || err.contains("notes root"),
            "unexpected error for {bad_path}: {err}"
        );
    }
}

#[tokio::test]
async fn tachi_memory_save_with_title_stays_memory() {
    let server = make_server();

    let saved = server
        .tachi_memory(Parameters(TachiMemoryParams {
            action: "save".to_string(),
            format: Some("markdown".to_string()),
            query: None,
            scope: None,
            top_k: 6,
            path_prefix: None,
            file_context: None,
            error_context: None,
            category: Some("fact".to_string()),
            include_archived: false,
            include_training: false,
            enable_rerank: false,
            as_of: None,
            synthesize: false,
            model: None,
            text: Some("A one-line memory fact with a title should not become wiki.".to_string()),
            title: Some("Memory title only".to_string()),
            summary: Some("Memory title only".to_string()),
            topic: Some("memory-boundary".to_string()),
            keywords: vec!["memory-boundary".to_string()],
            entities: Vec::new(),
            importance: Some(0.7),
            retention_policy: None,
            kind: None,
            path: Some("/facts/memory-boundary".to_string()),
            id: None,
            force: true,
            source: None,
            valid_from: None,
            valid_until: None,
            flow_id: None,
            event: None,
            state: None,
            project: None,
            domain: None,
            metadata: None,
            emit_continuity: false,
            compact: false,
            files: Vec::new(),
        }))
        .await
        .expect("tachi_memory save should succeed");
    assert!(saved.contains("Saved ->"));
    assert!(saved.contains("/facts/memory-boundary"));
    let id = saved
        .split("id: `")
        .nth(1)
        .and_then(|rest| rest.split('`').next())
        .expect("save markdown id")
        .to_string();
    assert!(!id.is_empty());
    let fetched = server
        .get_memory(Parameters(GetMemoryParams {
            id,
            include_archived: false,
            project: None,
        }))
        .await
        .expect("get memory");
    let fetched_json: serde_json::Value = serde_json::from_str(&fetched).expect("memory JSON");
    assert_eq!(fetched_json["path"], json!("/facts/memory-boundary"));
    assert_ne!(fetched_json["metadata"]["wiki"], json!(true));
}

#[test]
fn write_note_file_falls_back_when_slug_has_no_ascii_tokens() {
    let _temp_home = TempHomeGuard::new();

    let (abs_path, rel_path) = crate::notes_ops::write_note_file(
        "body for non-ascii title",
        None,
        Some("中文 标题"),
        Some("notes-test"),
        Some("note"),
        &[],
    )
    .expect("write note file");

    assert!(abs_path.exists(), "note path should exist: {abs_path:?}");
    assert!(
        rel_path.starts_with("inbox/"),
        "unexpected note path: {rel_path}"
    );
    assert!(
        rel_path.ends_with("-note.md"),
        "non-ascii title should use note slug fallback: {rel_path}"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn tachi_save_note_rejects_symlink_leaf() {
    let (server, temp_home) = make_server_with_temp_home();
    let notes_dir = crate::notes_ops::notes_root().join("brainstorm");
    std::fs::create_dir_all(&notes_dir).expect("create notes dir");
    let outside = temp_home.temp_home.join("outside.md");
    std::fs::write(&outside, "outside").expect("write outside target");
    std::os::unix::fs::symlink(&outside, notes_dir.join("escape.md")).expect("create symlink");

    let err = server
        .tachi_save(Parameters(TachiSaveParams {
            text: "must not follow symlink".to_string(),
            id: None,
            kind: Some("note".to_string()),
            title: Some("symlink leaf".to_string()),
            summary: None,
            path: Some("brainstorm/escape.md".to_string()),
            importance: None,
            category: None,
            keywords: Vec::new(),
            entities: Vec::new(),
            scope: None,
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
        }))
        .await
        .expect_err("symlink note leaf should be rejected");
    assert!(
        err.contains("notes root"),
        "unexpected symlink rejection error: {err}"
    );
    assert_eq!(
        std::fs::read_to_string(&outside).expect("outside target should remain readable"),
        "outside"
    );
}
