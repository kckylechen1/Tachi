use super::*;

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
            .expect_err("invalid note path should be rejected");
        assert!(
            err.contains("relative") || err.contains("notes root"),
            "unexpected error for {bad_path}: {err}"
        );
    }
}
