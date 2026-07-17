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
            project_explicit: false,
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
            format: None,
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

/// tachi#1199: docs/schema teach agents the `/scratch/...` leading-slash hierarchical
/// path convention for every `tachi_save` kind (memory/wiki/note alike). The note
/// writer used to be the one holdout that rejected a leading `/`; it must now accept
/// it and normalize to the same location a caller would get by dropping the slash.
///
/// This asserts literal path identity, not merely "same parent directory": both
/// calls target the *same* logical note (`scratch/tachi-1199/probe-note.md`), one
/// spelled with a leading slash and one without. `resolve_note_path` performs no
/// existence/dedup check before writing (it's a plain `create_dir_all` + `fs::write`),
/// so a second write to an already-written path is a harmless overwrite here, not a
/// distinct code path — the test is free to reuse the exact path without exercising
/// any dedup/versioning semantics.
#[tokio::test]
async fn tachi_save_note_leading_slash_normalizes_to_same_path_as_relative() {
    let (server, _temp_home) = make_server_with_temp_home();

    let with_slash = server
        .tachi_save(Parameters(TachiSaveParams {
            text: "leading slash note body".to_string(),
            id: None,
            kind: Some("note".to_string()),
            title: None,
            summary: None,
            path: Some("/scratch/tachi-1199/probe-note.md".to_string()),
            importance: None,
            category: None,
            keywords: Vec::new(),
            entities: Vec::new(),
            scope: None,
            project: None,
            project_explicit: false,
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
            format: None,
        }))
        .await
        .expect("leading-slash note path should be accepted");
    let with_slash_json: serde_json::Value =
        serde_json::from_str(&with_slash).expect("save JSON");
    let with_slash_path = with_slash_json["note_path"]
        .as_str()
        .expect("note_path")
        .to_string();
    let with_slash_file = with_slash_json["note_file"]
        .as_str()
        .expect("note_file")
        .to_string();
    assert_eq!(
        with_slash_path, "scratch/tachi-1199/probe-note.md",
        "leading slash should be stripped, not preserved, in the stored note path"
    );

    let without_slash = server
        .tachi_save(Parameters(TachiSaveParams {
            text: "relative note body".to_string(),
            id: None,
            kind: Some("note".to_string()),
            title: None,
            summary: None,
            path: Some("scratch/tachi-1199/probe-note.md".to_string()),
            importance: None,
            category: None,
            keywords: Vec::new(),
            entities: Vec::new(),
            scope: None,
            project: None,
            project_explicit: false,
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
            format: None,
        }))
        .await
        .expect("relative note path should be accepted");
    let without_slash_json: serde_json::Value =
        serde_json::from_str(&without_slash).expect("save JSON");
    let without_slash_path = without_slash_json["note_path"]
        .as_str()
        .expect("note_path")
        .to_string();
    let without_slash_file = without_slash_json["note_file"]
        .as_str()
        .expect("note_file")
        .to_string();

    // The literal proof this test's name promises: leading-slash and relative
    // spellings of the same logical path resolve to the exact same stored path
    // and the exact same file on disk — not merely paths that share a parent dir.
    assert_eq!(
        with_slash_path, without_slash_path,
        "leading-slash and relative spellings of the same note path must resolve identically"
    );
    assert_eq!(
        with_slash_file, without_slash_file,
        "leading-slash and relative spellings of the same note path must resolve to the same file on disk"
    );
    assert_eq!(without_slash_path, "scratch/tachi-1199/probe-note.md");
}
