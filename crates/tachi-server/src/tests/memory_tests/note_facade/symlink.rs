use super::*;

#[cfg(unix)]
#[tokio::test]
async fn tachi_save_note_rejects_symlink_leaf() {
    let (server, temp_home) = make_server_with_temp_home();
    let notes_dir = crate::notes_ops::notes_root(&server.tachi_home_dir()).join("brainstorm");
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
