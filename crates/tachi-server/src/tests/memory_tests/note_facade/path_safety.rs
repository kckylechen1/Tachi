use super::*;

#[tokio::test]
async fn tachi_save_note_rejects_paths_outside_notes_root() {
    let (server, _temp_home) = make_server_with_temp_home();

    // A leading `/` is accepted and normalized to notes-root-relative (tachi#1199),
    // so `/tmp/escape.md` is no longer an escape attempt — it just becomes the
    // `tmp/escape.md` subpath under notes root. `..` traversal (with or without a
    // leading slash) and mid-path traversal remain rejected.
    for bad_path in ["/../escape.md", "../escape.md", "brainstorm/../escape.md"] {
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

#[tokio::test]
async fn tachi_save_note_rejects_paths_that_normalize_to_empty() {
    let (server, _temp_home) = make_server_with_temp_home();

    // Pure-slash paths strip down to nothing and must not silently fall back to
    // some default location — they should be rejected (tachi#1199).
    for empty_path in ["/", "//"] {
        let err = server
            .tachi_save(Parameters(TachiSaveParams {
                text: "empty-after-normalize path should not be saved".to_string(),
                id: None,
                kind: Some("note".to_string()),
                title: Some("empty path".to_string()),
                summary: None,
                path: Some(empty_path.to_string()),
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
                files: Vec::new(),
                format: None,
            }))
            .await
            .expect_err("empty-after-normalize note path should be rejected");
        assert!(
            err.contains("notes root"),
            "unexpected error for {empty_path:?}: {err}"
        );
    }
}
