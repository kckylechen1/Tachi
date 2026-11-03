use super::*;

fn global_row_count(server: &crate::MemoryServer) -> i64 {
    server
        .with_global_store_read(|store| {
            store
                .connection()
                .query_row("SELECT COUNT(*) FROM memories", [], |row| row.get(0))
                .map_err(|error| error.to_string())
        })
        .unwrap()
}

#[tokio::test]
async fn public_project_saves_refuse_unbound_global_fallback() {
    for action in ["save", "checkpoint"] {
        for requested in [None, Some("project")] {
            let server = make_server();
            let before = global_row_count(&server);
            let mut params = tachi_memory_params(action);
            params.scope = requested.map(str::to_string);
            params.kind = Some("memory".to_string());
            params.text = Some(
                "Project routing must bind the current workspace before saving memory.".to_string(),
            );
            params.path = Some("/workspace-routing/refused".to_string());
            params.title = Some("Workspace routing checkpoint".to_string());
            params.force = true;
            let error = crate::facade_memory_ops::handle_tachi_memory(&server, params)
                .await
                .expect_err("missing project must refuse rather than save to global");
            assert!(error.contains("No project DB is bound"), "{error}");
            assert!(error.contains("X-Tachi-Workspace-Root"), "{error}");
            assert!(error.contains("storage_scope=global"), "{error}");
            assert_eq!(global_row_count(&server), before);
        }
    }
}

#[tokio::test]
async fn explicit_global_save_lands_in_global_without_scope_warning() {
    let server = make_server();
    let mut params = tachi_memory_params("save");
    params.format = Some("json".to_string());
    params.scope = Some("global".to_string());
    params.kind = Some("memory".to_string());
    params.id = Some("explicit-global-workspace-routing".to_string());
    params.text = Some(
        "Global memory was explicitly selected for this cross-workspace convention.".to_string(),
    );
    params.path = Some("/workspace-routing/global".to_string());
    params.force = true;
    let raw = crate::facade_memory_ops::handle_tachi_memory(&server, params)
        .await
        .unwrap();
    let parsed: Value = serde_json::from_str(&raw).unwrap();
    assert!(parsed.get("scope_warning").is_none(), "{parsed}");
    assert!(server
        .with_global_store_read(|store| {
            store
                .get("explicit-global-workspace-routing")
                .map_err(|error| error.to_string())
        })
        .unwrap()
        .is_some());
}

#[tokio::test]
async fn default_project_save_uses_the_available_project_database() {
    let (server, _) = make_server_with_project_fixture("workspace-routing");
    let mut params = tachi_memory_params("save");
    params.scope = None;
    params.kind = Some("memory".to_string());
    params.id = Some("implicit-project-workspace-routing".to_string());
    params.text =
        Some("Default saves remain in the current workspace memory database.".to_string());
    params.path = Some("/workspace-routing/project".to_string());
    params.force = true;
    crate::facade_memory_ops::handle_tachi_memory(&server, params)
        .await
        .unwrap();
    assert!(server
        .with_project_store_read(|store| {
            store
                .get("implicit-project-workspace-routing")
                .map_err(|error| error.to_string())
        })
        .unwrap()
        .is_some());
    assert!(server
        .with_global_store_read(|store| {
            store
                .get("implicit-project-workspace-routing")
                .map_err(|error| error.to_string())
        })
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn invalid_named_project_does_not_fall_back_to_global() {
    let server = make_server();
    let before = global_row_count(&server);
    let mut params = tachi_memory_params("save");
    params.project = Some("missing-workspace".to_string());
    params.text =
        Some("An unknown workspace must not redirect this memory into global.".to_string());
    params.path = Some("/workspace-routing/missing".to_string());
    let error = crate::facade_memory_ops::handle_tachi_memory(&server, params)
        .await
        .unwrap_err();
    assert!(error.contains("not found"), "{error}");
    assert_eq!(global_row_count(&server), before);
}

#[tokio::test]
async fn unbound_note_refusal_precedes_filesystem_side_effects() {
    let server = make_server();
    let target = server
        .tachi_home_dir()
        .join("notes/inbox/unbound-workspace.md");
    assert!(!target.exists());
    let mut params = tachi_memory_params("save");
    params.scope = None;
    params.kind = Some("note".to_string());
    params.text =
        Some("A note requires the same project binding as an ordinary memory save.".to_string());
    params.path = Some("inbox/unbound-workspace.md".to_string());
    crate::facade_memory_ops::handle_tachi_memory(&server, params)
        .await
        .unwrap_err();
    assert!(!target.exists());
}

#[tokio::test]
async fn unusable_named_note_destination_preserves_absent_and_existing_files() {
    for failure in ["corrupt", "foreign-role"] {
        for preexisting_note in [false, true] {
            let server = make_server();
            let fixture = crate::test_support::non_skipped_fixture_tempdir("note-admission-");
            let root = fixture.path().join("NotesProject");
            std::fs::create_dir_all(root.join(".git")).unwrap();
            let db = root.join(".tachi").join(memcore::MEMORY_DB_FILENAME);
            std::fs::create_dir_all(db.parent().unwrap()).unwrap();
            let project = crate::path_utils::plan_c_dir_name_from_root(&root).unwrap();
            let role = if failure == "foreign-role" {
                "global"
            } else {
                &project
            };
            drop(memcore::MemoryStore::open_with_label(db.to_str().unwrap(), role).unwrap());
            crate::project_db_ops::register_repo_local_manifest_entry_in_home(
                &db,
                &project,
                &server.tachi_home_dir(),
            )
            .unwrap();
            if failure == "corrupt" {
                std::fs::write(&db, b"registered but damaged database").unwrap();
            }
            let database_before = std::fs::read(&db).unwrap();
            let expected_error = server
                .resolve_named_project_store_binding(&project)
                .err()
                .expect("fixture must fail read-only identity admission");
            let target = server.tachi_home_dir().join("notes/inbox/protected.md");
            let original = b"An existing note must survive destination refusal.\n";
            if preexisting_note {
                std::fs::create_dir_all(target.parent().unwrap()).unwrap();
                std::fs::write(&target, original).unwrap();
            }
            let before = global_row_count(&server);
            let params = serde_json::from_value(json!({
                "kind": "note", "project": project, "path": "inbox/protected.md",
                "text": "This note must not be published before its library is admitted."
            }))
            .unwrap();
            let error = crate::facade_save_ops::handle_tachi_save(&server, params)
                .await
                .unwrap_err();
            assert_eq!(error, expected_error, "propagate the identity refusal");
            if failure == "foreign-role" {
                assert!(error.contains("store role conflict"), "{error}");
            } else {
                assert!(
                    error.starts_with(&format!("inspect project '{project}' identity:")),
                    "{error}"
                );
            }
            assert_eq!(std::fs::read(&db).unwrap(), database_before);
            if preexisting_note {
                assert_eq!(std::fs::read(&target).unwrap(), original);
            } else {
                assert!(
                    !target.exists(),
                    "{failure} must refuse before publishing a note"
                );
            }
            assert_eq!(global_row_count(&server), before);
        }
    }
}
