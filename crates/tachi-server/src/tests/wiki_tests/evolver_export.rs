use super::*;
use std::path::Path;

#[tokio::test]
#[allow(clippy::await_holding_lock)] // serializes HOME/TACHI_HOME across async mock LLM + REM run
async fn rem_wiki_evolver_writes_pending_drafts_to_wiki_project() {
    let _lock = home_test_lock().lock().unwrap_or_else(|e| e.into_inner());

    use axum::{routing::post, Json, Router};
    let synthesis_barrier = std::sync::Arc::new(tokio::sync::Barrier::new(2));
    let app = Router::new().route(
        "/chat/completions",
        post({
            let synthesis_barrier = synthesis_barrier.clone();
            move |Json(_body): Json<serde_json::Value>| {
                let synthesis_barrier = synthesis_barrier.clone();
                async move {
                    synthesis_barrier.wait().await;
                    Json(json!({
                "choices": [
                    {
                        "message": {
                            "role": "assistant",
                            "content": "{\n  \"title\": \"Recall Gate Pattern\",\n  \"body\": \"## Pattern\\nUse recall diversity before promoting raw notes into durable knowledge.\\n\\n## Gotcha\\nDo not activate drafts without review.\",\n  \"summary\": \"Recall diversity gates promotion.\",\n  \"keywords\": [\"recall\", \"promotion\"],\n  \"entities\": [\"Tachi\"],\n  \"domain\": \"memory\"\n}"
                        },
                        "finish_reason": "stop"
                    }
                ],
                "usage": {"prompt_tokens": 10, "completion_tokens": 20, "total_tokens": 30}
                    }))
                }
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server_task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let original_home = std::env::var_os("HOME");
    let original_tachi_home = std::env::var_os("TACHI_HOME");
    let original_siliconflow_base = std::env::var_os("SILICONFLOW_BASE_URL");
    let original_reasoning_base = std::env::var_os("REASONING_BASE_URL");
    let original_siliconflow_key = std::env::var_os("SILICONFLOW_API_KEY");
    let original_voyage_key = std::env::var_os("VOYAGE_API_KEY");
    let temp_home =
        crate::utils::test_fixture_path(format!("tachi-rem-test-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(temp_home.join(".tachi/projects/wiki")).expect("create wiki project");
    std::env::set_var("HOME", &temp_home);
    std::env::set_var("TACHI_HOME", temp_home.join(".tachi"));
    let mock_url = format!("http://127.0.0.1:{port}/chat/completions");
    std::env::set_var("SILICONFLOW_BASE_URL", &mock_url);
    std::env::set_var("REASONING_BASE_URL", &mock_url);
    std::env::set_var("SILICONFLOW_API_KEY", "test-mock-key");
    std::env::set_var("VOYAGE_API_KEY", "test-mock-key");

    let wiki_db = temp_home.join(".tachi/projects/wiki/memory.db");
    MemoryStore::open(wiki_db.to_str().expect("wiki db utf8")).expect("init wiki db");
    let server = MemoryServer::new(
        temp_home.join("global.db"),
        Some(temp_home.join("project.db")),
    )
    .expect("server");
    server
        .with_global_store(|store| {
            let mut entry = make_entry("pattern-a");
            entry.path = "/global/tachi/pattern-a".to_string();
            entry.summary = "Global recall diversity gate".to_string();
            entry.text = "Global experience independently confirms that recall diversity should gate durable Wiki promotion and remain pending until review.".to_string();
            entry.importance = 0.9;
            entry.topic = "recall-gate".to_string();
            entry.keywords = vec!["recall".to_string(), "promotion".to_string()];
            entry.source = "manual".to_string();
            entry.tier = "pattern".to_string();
            store.upsert(&entry).map_err(|error| error.to_string())
        })
        .expect("seed same-id global pattern");
    server.with_project_store(|store| {
        for (id, summary, text) in [
            (
                "pattern-a",
                "Recall diversity gate",
                "Recall diversity should gate raw promotion before durable wiki synthesis. This fixture covers the first retrieval signal and promotion rule.",
            ),
            (
                "pattern-b",
                "Pending review draft gate",
                "Weekly REM synthesis should write drafts as pending review wiki notes. This fixture covers draft routing and review metadata safety.",
            ),
        ] {
            let mut entry = make_entry(id);
            entry.path = format!("/project/tachi/{id}");
            entry.summary = summary.to_string();
            entry.text = text.to_string();
            entry.importance = 0.9;
            entry.topic = "recall-gate".to_string();
            entry.keywords = vec!["recall".to_string(), "promotion".to_string()];
            entry.source = "manual".to_string();
            entry.tier = "pattern".to_string();
            store.upsert(&entry).map_err(|e| e.to_string())?;
        }
        let mut sft_entry = make_entry("pattern-sft-seed");
        sft_entry.path = "/sft/v4/strict/engineering/recall-gate".to_string();
        sft_entry.summary = "SFT recall gate exemplar".to_string();
        sft_entry.text =
            "SFT exemplar should not be promoted into REM wiki synthesis.".to_string();
        sft_entry.importance = 0.99;
        sft_entry.topic = "recall-gate".to_string();
        sft_entry.keywords = vec!["recall".to_string(), "promotion".to_string()];
        sft_entry.source = "sft_seed".to_string();
        sft_entry.tier = "pattern".to_string();
        sft_entry.metadata = json!({"training_sample": true});
        store.upsert(&sft_entry).map_err(|e| e.to_string())?;
        Ok(())
    }).expect("seed patterns");
    let seeded_count: i64 = server
        .with_project_store_read(|store| {
            store.connection().query_row(
            "SELECT COUNT(*) FROM memories WHERE tier = 'pattern' AND topic = 'recall-gate'",
            [],
            |row| row.get(0),
        ).map_err(|e| e.to_string())
        })
        .expect("count seeded patterns");
    assert_eq!(
        seeded_count, 3,
        "expected two live pattern memories plus one SFT seed before REM run"
    );

    let (first, second) = tokio::join!(
        crate::foundry_runtime_ops::wiki_evolver::run_weekly_wiki_evolution(&server),
        crate::foundry_runtime_ops::wiki_evolver::run_weekly_wiki_evolution(&server),
    );
    let reports = [
        first.expect("first concurrent wiki evolution"),
        second.expect("second concurrent wiki evolution"),
    ];
    assert_eq!(
        reports
            .iter()
            .map(|report| report.drafts_written)
            .sum::<usize>(),
        1,
        "concurrent replay must report exactly one newly written draft"
    );
    assert_eq!(reports.iter().map(|report| report.errors).sum::<usize>(), 0);
    let (draft_id, review_status, model_receipt, operation_status, source_count) = server.with_named_project_store_read("wiki", |store| {
        store.connection().query_row(
            "SELECT id, json_extract(metadata, '$.review_status'), json_extract(metadata, '$.provenance.model_invocation.schema'), json_extract(metadata, '$.rem.operation_status'), json_array_length(json_extract(metadata, '$.rem.sources')) FROM memories WHERE path LIKE '/wiki/drafts/%' LIMIT 1",
            [],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?, row.get::<_, Option<String>>(2)?, row.get::<_, Option<String>>(3)?, row.get::<_, i64>(4)?)),
        ).map_err(|e| e.to_string())
    }).expect("read wiki draft metadata");
    assert!(draft_id.starts_with("wiki-rem:"), "{draft_id}");
    assert_eq!(review_status.as_deref(), Some("pending"));
    assert_eq!(
        model_receipt.as_deref(),
        Some("model-invocation-v1"),
        "REM's first wiki draft write must carry the typed model receipt"
    );
    assert_eq!(operation_status.as_deref(), Some("complete"));
    assert_eq!(
        source_count, 3,
        "same memory ID in two stores stays distinct"
    );
    let operation_log = server
        .with_named_project_store_read("wiki", |store| {
            store
                .get("wiki-operation-log")
                .map_err(|error| error.to_string())?
                .ok_or_else(|| "Wiki operation log missing".to_string())
        })
        .expect("read REM Wiki operation log");
    assert!(
        operation_log.text.contains("weekly REM draft completed"),
        "successful REM draft persistence must remain visible in the Wiki operation log"
    );
    for read_marker in [
        server.with_global_store_read(|store| {
            store.get("pattern-a").map_err(|error| error.to_string())
        }),
        server.with_project_store_read(|store| {
            store.get("pattern-a").map_err(|error| error.to_string())
        }),
    ] {
        let source = read_marker
            .expect("read REM source marker")
            .expect("source exists");
        assert_eq!(source.metadata["rem"]["processed"], json!(1));
        assert_eq!(
            source.metadata["rem"]["processed_by"],
            json!(draft_id.clone())
        );
    }
    let sft_processed: Option<i64> = server
        .with_project_store_read(|store| {
            store
                .connection()
                .query_row(
                    "SELECT json_extract(metadata, '$.rem.processed') FROM memories WHERE id = 'pattern-sft-seed'",
                    [],
                    |row| row.get(0),
                )
                .map_err(|e| e.to_string())
        })
        .expect("read SFT pattern metadata");
    assert_eq!(
        sft_processed, None,
        "SFT pattern seeds must not be consumed by REM wiki evolution"
    );
    let replay = crate::foundry_runtime_ops::wiki_evolver::run_weekly_wiki_evolution(&server)
        .await
        .expect("second REM run");
    assert_eq!(replay.drafts_written, 0, "replay must not mint a draft");
    let draft_count: i64 = server
        .with_named_project_store_read("wiki", |store| {
            store
                .connection()
                .query_row(
                    "SELECT COUNT(*) FROM memories WHERE path LIKE '/wiki/drafts/%'",
                    [],
                    |row| row.get(0),
                )
                .map_err(|error| error.to_string())
        })
        .expect("count REM drafts after replay");
    assert_eq!(draft_count, 1);

    server_task.abort();
    if let Some(value) = original_home {
        std::env::set_var("HOME", value);
    } else {
        std::env::remove_var("HOME");
    }
    if let Some(value) = original_tachi_home {
        std::env::set_var("TACHI_HOME", value);
    } else {
        std::env::remove_var("TACHI_HOME");
    }
    if let Some(value) = original_siliconflow_base {
        std::env::set_var("SILICONFLOW_BASE_URL", value);
    } else {
        std::env::remove_var("SILICONFLOW_BASE_URL");
    }
    if let Some(value) = original_reasoning_base {
        std::env::set_var("REASONING_BASE_URL", value);
    } else {
        std::env::remove_var("REASONING_BASE_URL");
    }
    if let Some(value) = original_siliconflow_key {
        std::env::set_var("SILICONFLOW_API_KEY", value);
    } else {
        std::env::remove_var("SILICONFLOW_API_KEY");
    }
    if let Some(value) = original_voyage_key {
        std::env::set_var("VOYAGE_API_KEY", value);
    } else {
        std::env::remove_var("VOYAGE_API_KEY");
    }
    let _ = std::fs::remove_dir_all(temp_home);
}

struct BlockingExportHook {
    acquired: std::sync::Arc<std::sync::Barrier>,
    release: std::sync::Arc<std::sync::Barrier>,
}

impl crate::wiki_ops::ExportTestHook for BlockingExportHook {
    fn after_lock_acquired(&self) -> Result<(), String> {
        self.acquired.wait();
        self.release.wait();
        Ok(())
    }
}

struct FailExportInstallHook {
    installed_count: usize,
}

impl crate::wiki_ops::ExportTestHook for FailExportInstallHook {
    fn before_install(&self, installed: usize, _path: &Path) -> Result<(), String> {
        if installed == self.installed_count {
            Err(format!(
                "injected export commit failure after {installed} installs"
            ))
        } else {
            Ok(())
        }
    }
}

fn export_file_snapshot(root: &Path) -> std::collections::BTreeMap<String, Vec<u8>> {
    fn visit(
        root: &Path,
        current: &Path,
        snapshot: &mut std::collections::BTreeMap<String, Vec<u8>>,
    ) {
        let mut entries = std::fs::read_dir(current)
            .expect("read export snapshot directory")
            .map(|entry| entry.expect("read export snapshot entry").path())
            .collect::<Vec<_>>();
        entries.sort();
        for path in entries {
            if path.is_dir() {
                visit(root, &path, snapshot);
            } else {
                snapshot.insert(
                    path.strip_prefix(root)
                        .unwrap()
                        .to_string_lossy()
                        .replace('\\', "/"),
                    std::fs::read(&path).expect("read export snapshot file"),
                );
            }
        }
    }

    let mut snapshot = std::collections::BTreeMap::new();
    visit(root, root, &mut snapshot);
    snapshot
}

#[tokio::test]
async fn wiki_export_obsidian_writes_markdown_index_and_wikilinks() {
    let mut entry = make_entry("wiki-export-entry");
    entry.path = "/wiki/engineering/debugging/export".to_string();
    entry.summary = "Export MCP lesson".to_string();
    entry.text =
        "MCP export lesson references MCP explicitly; [[MCP]] stays linked; MCPing stays plain."
            .to_string();
    entry.topic = "export-mcp".to_string();
    entry.keywords = vec!["debugging".to_string()];
    entry.entities = vec!["MCP".to_string()];

    let (server, _home) = seed_wiki_project_entries(vec![entry]);
    let out_dir = crate::utils::test_fixture_path(format!("wiki-export-{}", uuid::Uuid::new_v4()));

    let result = crate::wiki_ops::export_wiki_obsidian(&server, "wiki", &out_dir)
        .expect("wiki export should succeed");
    assert_eq!(result["count"], json!(1));
    let md_path = out_dir.join("engineering/debugging/export/export-mcp.md");
    let markdown = std::fs::read_to_string(&md_path).expect("read exported markdown");
    assert!(markdown.contains("tags: [\"debugging\"]"));
    assert!(markdown.contains(
        "[[MCP]] export lesson references [[MCP]] explicitly; [[MCP]] stays linked; MCPing stays plain."
    ));
    let index = std::fs::read_to_string(out_dir.join("_index.md")).expect("read index");
    assert!(index.contains("[[engineering/debugging/export/export-mcp]]"));
    let _ = std::fs::remove_dir_all(out_dir);
}

#[tokio::test]
async fn wiki_export_obsidian_collision_manifest_and_rerun_are_deterministic() {
    let mut first = make_entry("wiki-export-collision-a");
    first.path = "/wiki/collisions".to_string();
    first.topic = "same-topic".to_string();
    first.summary = "First collision entry".to_string();
    first.text = "First collision body.".to_string();

    let mut second = make_entry("wiki-export-collision-b");
    second.path = first.path.clone();
    second.topic = first.topic.clone();
    second.summary = "Second collision entry".to_string();
    second.text = "Second collision body.".to_string();

    let (server, _home) = seed_wiki_project_entries(vec![first, second]);
    let out_dir =
        crate::utils::test_fixture_path(format!("wiki-export-collision-{}", uuid::Uuid::new_v4()));

    let first_result = crate::wiki_ops::export_wiki_obsidian(&server, "wiki", &out_dir)
        .expect("collision export should succeed");
    assert_eq!(first_result["count"], json!(2));
    assert_eq!(first_result["written_files"], json!(2));

    let manifest_path = out_dir.join("_manifest.json");
    let index_path = out_dir.join("_index.md");
    let first_manifest = std::fs::read_to_string(&manifest_path).expect("read export manifest");
    let first_index = std::fs::read_to_string(&index_path).expect("read export index");
    let manifest: Value = serde_json::from_str(&first_manifest).expect("parse export manifest");
    assert_eq!(manifest["count"], json!(2));
    let entries = manifest["entries"].as_array().expect("manifest entries");
    assert_eq!(entries.len(), 2);

    let mut output_paths = std::collections::BTreeSet::new();
    for entry in entries {
        let entry_id = entry["entry_id"].as_str().expect("manifest entry id");
        let output_path = entry["output_path"].as_str().expect("manifest output path");
        assert!(
            entry["store_ref"]["kind"] == json!("named_project"),
            "manifest must retain the logical store ref: {entry:?}"
        );
        assert_eq!(entry["wiki_path"], json!("/wiki/collisions"));
        assert!(
            output_paths.insert(output_path),
            "collision entries must map to distinct output paths"
        );
        assert!(
            output_path.contains(&entry_id.replace(':', "_")),
            "collision filename must carry a stable ID-derived suffix: {output_path}"
        );
        assert!(out_dir.join(output_path).is_file());
        assert!(first_index.contains(&format!("[[{}]]", output_path.trim_end_matches(".md"))));
    }
    assert_eq!(output_paths.len(), 2);
    assert_eq!(
        std::fs::read_dir(out_dir.join("collisions"))
            .expect("collision output directory")
            .filter_map(Result::ok)
            .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "md"))
            .count(),
        2,
        "written entry files must equal the manifest/source count"
    );

    let second_result = crate::wiki_ops::export_wiki_obsidian(&server, "wiki", &out_dir)
        .expect("re-running collision export should succeed");
    assert_eq!(second_result["count"], json!(2));
    assert_eq!(
        std::fs::read_to_string(&manifest_path).expect("read rerun manifest"),
        first_manifest,
        "rerunning the same export must preserve the exact deterministic plan"
    );
    assert_eq!(
        std::fs::read_to_string(&index_path).expect("read rerun index"),
        first_index,
        "rerunning the same export must preserve deterministic index ordering"
    );
    let _ = std::fs::remove_dir_all(out_dir);
}

#[test]
fn wiki_export_obsidian_preflights_existing_output_before_mutation() {
    let mut entry = make_entry("wiki-export-preflight");
    entry.path = "/wiki/preflight".to_string();
    entry.topic = "preflight".to_string();
    let (server, _home) = seed_wiki_project_entries(vec![entry]);
    let out_dir =
        crate::utils::test_fixture_path(format!("wiki-export-preflight-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(out_dir.join("preflight")).expect("create preflight output");
    let sentinel = out_dir.join("preflight/preflight.md");
    std::fs::write(&sentinel, "sentinel").expect("write preflight sentinel");

    let error = crate::wiki_ops::export_wiki_obsidian(&server, "wiki", &out_dir)
        .expect_err("existing unmanaged output must be refused before mutation");
    assert!(error.contains("refusing to overwrite"), "{error}");
    assert_eq!(std::fs::read_to_string(&sentinel).unwrap(), "sentinel");
    assert!(!out_dir.join("_index.md").exists());
    assert!(!out_dir.join("_manifest.json").exists());
    let _ = std::fs::remove_dir_all(out_dir);
}

#[test]
fn wiki_export_obsidian_refuses_unmanaged_reserved_index() {
    let mut entry = make_entry("wiki-export-reserved-index");
    entry.path = "/wiki/reserved".to_string();
    entry.topic = "reserved".to_string();
    let (server, _home) = seed_wiki_project_entries(vec![entry]);
    let out_dir = crate::utils::test_fixture_path(format!(
        "wiki-export-reserved-index-{}",
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&out_dir).expect("create reserved output");
    let index = out_dir.join("_index.md");
    std::fs::write(&index, "unmanaged index sentinel").expect("write unmanaged index");

    let error = crate::wiki_ops::export_wiki_obsidian(&server, "wiki", &out_dir)
        .expect_err("unmanaged reserved index must be refused before mutation");
    assert!(error.contains("reserved managed artifact"), "{error}");
    assert_eq!(
        std::fs::read_to_string(&index).unwrap(),
        "unmanaged index sentinel"
    );
    assert!(!out_dir.join("_manifest.json").exists());
    let _ = std::fs::remove_dir_all(out_dir);
}

#[test]
fn wiki_export_obsidian_reconciles_stale_prior_manifest_entries() {
    let mut first = make_entry("wiki-export-stale-a");
    first.path = "/wiki/stale".to_string();
    first.topic = "same-topic".to_string();
    first.summary = "Stale collision A".to_string();
    first.text = "First stale collision body.".to_string();
    let mut second = make_entry("wiki-export-stale-b");
    second.path = first.path.clone();
    second.topic = first.topic.clone();
    second.summary = "Stale collision B".to_string();
    second.text = "Second stale collision body.".to_string();
    let (server, _home) = seed_wiki_project_entries(vec![first, second]);
    let out_dir = crate::utils::test_fixture_path(format!(
        "wiki-export-stale-cleanup-{}",
        uuid::Uuid::new_v4()
    ));

    crate::wiki_ops::export_wiki_obsidian(&server, "wiki", &out_dir)
        .expect("initial collision export");
    let first_manifest: Value =
        serde_json::from_str(&std::fs::read_to_string(out_dir.join("_manifest.json")).unwrap())
            .unwrap();
    assert_eq!(first_manifest["count"], json!(2));
    let stale_paths = first_manifest["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["output_path"].as_str().unwrap().to_string())
        .collect::<Vec<_>>();
    server
        .with_named_project_store("wiki", |store| {
            store
                .connection()
                .execute(
                    "DELETE FROM memories WHERE id = ?1",
                    rusqlite::params!["wiki-export-stale-b"],
                )
                .map(|_| ())
                .map_err(|error| error.to_string())
        })
        .expect("remove second source entry");

    crate::wiki_ops::export_wiki_obsidian(&server, "wiki", &out_dir)
        .expect("rerun after source contraction");
    assert!(out_dir.join("stale/same-topic.md").is_file());
    for stale in stale_paths {
        assert!(
            !out_dir.join(&stale).exists(),
            "prior-manifest-owned stale file must be removed: {stale}"
        );
    }
    let manifest: Value =
        serde_json::from_str(&std::fs::read_to_string(out_dir.join("_manifest.json")).unwrap())
            .unwrap();
    assert_eq!(manifest["count"], json!(1));
    assert_eq!(manifest["entries"].as_array().unwrap().len(), 1);
    let _ = std::fs::remove_dir_all(out_dir);
}

#[test]
fn wiki_export_obsidian_index_links_complete_relative_output_path() {
    let mut entry = make_entry("wiki-export-relative-index");
    entry.path = "/wiki/engineering/deep".to_string();
    entry.topic = "nested-entry".to_string();
    let (server, _home) = seed_wiki_project_entries(vec![entry]);
    let out_dir = crate::utils::test_fixture_path(format!(
        "wiki-export-relative-index-{}",
        uuid::Uuid::new_v4()
    ));

    crate::wiki_ops::export_wiki_obsidian(&server, "wiki", &out_dir)
        .expect("nested export should succeed");
    let index = std::fs::read_to_string(out_dir.join("_index.md")).expect("read export index");
    assert!(
        index.contains("[[engineering/deep/nested-entry]]"),
        "index must link the complete relative output path: {index}"
    );
    let _ = std::fs::remove_dir_all(out_dir);
}

#[test]
fn wiki_export_obsidian_concurrent_run_refuses_shared_output_lock() {
    let mut entry = make_entry("wiki-export-lock");
    entry.path = "/wiki/export-lock".to_string();
    entry.topic = "locked-entry".to_string();
    let (server, _home) = seed_wiki_project_entries(vec![entry]);
    let out_dir =
        crate::utils::test_fixture_path(format!("wiki-export-lock-{}", uuid::Uuid::new_v4()));
    let acquired = std::sync::Arc::new(std::sync::Barrier::new(2));
    let release = std::sync::Arc::new(std::sync::Barrier::new(2));
    let first_server = server.clone();
    let first_output = out_dir.clone();
    let first_acquired = acquired.clone();
    let first_release = release.clone();
    let first = std::thread::spawn(move || {
        crate::wiki_ops::export_wiki_obsidian_with_hook(
            &first_server,
            "wiki",
            &first_output,
            &BlockingExportHook {
                acquired: first_acquired,
                release: first_release,
            },
        )
    });

    acquired.wait();
    let concurrent = crate::wiki_ops::export_wiki_obsidian(&server, "wiki", &out_dir);
    release.wait();
    let first_result = first.join().expect("join locked export");

    first_result.expect("lock owner export should complete after release");
    let error = concurrent.expect_err("concurrent export must not share staging or output");
    assert!(error.contains("per-output export lock"), "{error}");
    assert!(out_dir.join("_manifest.json").is_file());
    let _ = std::fs::remove_dir_all(out_dir);
}

#[test]
fn wiki_export_obsidian_commit_failure_restores_complete_prior_state() {
    let mut original_entry = make_entry("wiki-export-rollback");
    original_entry.path = "/wiki/rollback".to_string();
    original_entry.topic = "rollback-entry".to_string();
    original_entry.text = "Prior managed body.".to_string();
    let (server, _home) = seed_wiki_project_entries(vec![original_entry.clone()]);
    let out_dir =
        crate::utils::test_fixture_path(format!("wiki-export-rollback-{}", uuid::Uuid::new_v4()));

    crate::wiki_ops::export_wiki_obsidian(&server, "wiki", &out_dir)
        .expect("create prior managed export");
    std::fs::write(out_dir.join("unmanaged-sentinel.txt"), "leave unmanaged")
        .expect("write unmanaged sentinel");
    let prior = export_file_snapshot(&out_dir);

    let mut updated_entry = original_entry;
    updated_entry.text = "New body that must roll back.".to_string();
    server
        .with_named_project_store("wiki", |store| {
            store
                .upsert(&updated_entry)
                .map_err(|error| error.to_string())
        })
        .expect("update export source");

    let error = crate::wiki_ops::export_wiki_obsidian_with_hook(
        &server,
        "wiki",
        &out_dir,
        &FailExportInstallHook { installed_count: 1 },
    )
    .expect_err("injected commit failure must abort export");
    assert!(
        error.contains("prior managed state was restored"),
        "{error}"
    );
    assert_eq!(
        export_file_snapshot(&out_dir),
        prior,
        "rollback must restore entries, index, manifest, and preserve unmanaged files exactly"
    );
    let _ = std::fs::remove_dir_all(out_dir);
}

#[test]
fn wiki_export_obsidian_refuses_invalid_prior_manifest_count_without_mutation() {
    let mut entry = make_entry("wiki-export-invalid-manifest");
    entry.path = "/wiki/invalid-manifest".to_string();
    entry.topic = "manifest-entry".to_string();
    let (server, _home) = seed_wiki_project_entries(vec![entry]);
    let out_dir = crate::utils::test_fixture_path(format!(
        "wiki-export-invalid-manifest-{}",
        uuid::Uuid::new_v4()
    ));
    crate::wiki_ops::export_wiki_obsidian(&server, "wiki", &out_dir)
        .expect("create managed export");
    let manifest_path = out_dir.join("_manifest.json");
    let mut manifest: Value =
        serde_json::from_str(&std::fs::read_to_string(&manifest_path).unwrap()).unwrap();
    manifest["count"] = json!(2);
    std::fs::write(
        &manifest_path,
        serde_json::to_string_pretty(&manifest).unwrap() + "\n",
    )
    .unwrap();
    let before = export_file_snapshot(&out_dir);

    let error = crate::wiki_ops::export_wiki_obsidian(&server, "wiki", &out_dir)
        .expect_err("invalid ownership manifest must be refused");
    assert!(error.contains("count"), "{error}");
    assert_eq!(export_file_snapshot(&out_dir), before);
    let _ = std::fs::remove_dir_all(out_dir);
}

#[test]
fn wiki_export_obsidian_explicit_project_never_falls_back_to_legacy_global() {
    let (server, _project_db) = crate::tests::make_server_with_project_fixture("export-target");
    let mut target = make_entry("wiki-export-store-identity");
    target.path = "/wiki/export/store-identity".to_string();
    target.topic = "named-store".to_string();
    target.text = "Named project export sentinel.".to_string();
    target.metadata = json!({"lifecycle": "active"});
    let mut legacy = target.clone();
    legacy.topic = "legacy-global".to_string();
    legacy.text = "Legacy global export sentinel.".to_string();

    server
        .with_named_project_store("export-target", |store| {
            store.upsert(&target).map_err(|error| error.to_string())
        })
        .expect("seed named export target");
    server
        .with_global_store(|store| store.upsert(&legacy).map_err(|error| error.to_string()))
        .expect("seed legacy global export decoy");

    let out_dir = crate::utils::test_fixture_path(format!(
        "wiki-export-store-identity-{}",
        uuid::Uuid::new_v4()
    ));
    let result = crate::wiki_ops::export_wiki_obsidian(&server, "export-target", &out_dir)
        .expect("strict named export");
    assert_eq!(result["count"], json!(1));
    let markdown = std::fs::read_to_string(out_dir.join("export/store-identity/named-store.md"))
        .expect("read named export");
    assert!(markdown.contains("Named project export sentinel."));
    assert!(!markdown.contains("Legacy global export sentinel."));
    assert!(!out_dir
        .join("export/store-identity/legacy-global.md")
        .exists());
    let _ = std::fs::remove_dir_all(out_dir);
}

#[tokio::test]
async fn wiki_export_obsidian_prefers_typed_refs_when_both_channels_exist() {
    let mut entry = make_entry("wiki-export-dual-refs");
    entry.path = "/wiki/engineering/debugging/dual-refs".to_string();
    entry.summary = "Dual refs export lesson".to_string();
    entry.text = "Dual refs export lesson body.".to_string();
    entry.topic = "dual-refs".to_string();
    entry.metadata = json!({
        "source_refs": ["kckylechen1/tachi#1072"],
        "evidence_refs_v1": [
            {"ref": "kckylechen1/tachi#1072", "target_kind": "issue", "captured_at": "2026-07-17T00:00:00Z"},
            {"ref": "docs/engineering/architecture/issue-refinery-memory-lanes.md", "target_kind": "canonical_doc", "captured_at": "2026-07-17T00:00:00Z"},
        ],
    });

    let (server, _home) = seed_wiki_project_entries(vec![entry]);
    let out_dir =
        crate::utils::test_fixture_path(format!("wiki-export-dual-refs-{}", uuid::Uuid::new_v4()));

    let result = crate::wiki_ops::export_wiki_obsidian(&server, "wiki", &out_dir)
        .expect("wiki export should succeed");
    assert_eq!(result["count"], json!(1));
    let md_path = out_dir.join("engineering/debugging/dual-refs/dual-refs.md");
    let markdown = std::fs::read_to_string(&md_path).expect("read exported markdown");
    assert!(
        !markdown.contains("## References\n"),
        "legacy section must be suppressed: {markdown}"
    );
    assert!(
        markdown.contains("## Evidence Refs (typed)"),
        "typed refs section must be selected when both channels exist: {markdown}"
    );
    assert!(
        markdown.contains("docs/engineering/architecture/issue-refinery-memory-lanes.md"),
        "typed ref target must render: {markdown}"
    );
    assert_eq!(markdown.matches("kckylechen1/tachi#1072").count(), 1);
    let _ = std::fs::remove_dir_all(out_dir);
}

#[tokio::test]
async fn wiki_export_obsidian_exports_legacy_only_refs() {
    let mut entry = make_entry("wiki-export-legacy-refs");
    entry.path = "/wiki/export/legacy".to_string();
    entry.topic = "legacy-refs".to_string();
    entry.metadata = json!({"source_refs": ["kckylechen1/tachi#1072"]});
    let (server, _home) = seed_wiki_project_entries(vec![entry]);
    let out_dir =
        crate::utils::test_fixture_path(format!("wiki-export-legacy-{}", uuid::Uuid::new_v4()));
    crate::wiki_ops::export_wiki_obsidian(&server, "wiki", &out_dir).unwrap();
    let markdown = std::fs::read_to_string(out_dir.join("export/legacy/legacy-refs.md")).unwrap();
    assert!(markdown.contains("## References\n"));
    assert!(!markdown.contains("## Evidence Refs (typed)"));
    assert_eq!(markdown.matches("kckylechen1/tachi#1072").count(), 1);
    let _ = std::fs::remove_dir_all(out_dir);
}

#[tokio::test]
async fn wiki_export_obsidian_exports_typed_only_refs() {
    let mut entry = make_entry("wiki-export-typed-refs");
    entry.path = "/wiki/export/typed".to_string();
    entry.topic = "typed-refs".to_string();
    entry.metadata = json!({"evidence_refs_v1": [{"ref": "#1296", "target_kind": "issue", "captured_at": "2026-07-19T00:00:00Z"}]});
    let (server, _home) = seed_wiki_project_entries(vec![entry]);
    let out_dir =
        crate::utils::test_fixture_path(format!("wiki-export-typed-{}", uuid::Uuid::new_v4()));
    crate::wiki_ops::export_wiki_obsidian(&server, "wiki", &out_dir).unwrap();
    let markdown = std::fs::read_to_string(out_dir.join("export/typed/typed-refs.md")).unwrap();
    assert!(markdown.contains("## Evidence Refs (typed)"));
    assert!(!markdown.contains("## References\n"));
    assert_eq!(markdown.matches("#1296").count(), 1);
    let _ = std::fs::remove_dir_all(out_dir);
}

#[tokio::test]
async fn wiki_export_obsidian_ignores_invalid_typed_refs_and_falls_back_to_legacy() {
    let mut entry = make_entry("wiki-export-invalid-typed-refs");
    entry.path = "/wiki/export/invalid-typed".to_string();
    entry.topic = "invalid-typed-refs".to_string();
    entry.metadata = json!({
        "evidence_refs_v1": [{"ref": "  "}, {"ref": 42}],
        "source_refs": [null, "", "#legacy-valid"]
    });
    let (server, _home) = seed_wiki_project_entries(vec![entry]);
    let out_dir = crate::utils::test_fixture_path(format!(
        "wiki-export-invalid-typed-{}",
        uuid::Uuid::new_v4()
    ));
    crate::wiki_ops::export_wiki_obsidian(&server, "wiki", &out_dir).unwrap();
    let markdown =
        std::fs::read_to_string(out_dir.join("export/invalid-typed/invalid-typed-refs.md"))
            .unwrap();
    assert!(!markdown.contains("## Evidence Refs (typed)"));
    assert!(markdown.contains("## References\n"));
    assert_eq!(markdown.matches("#legacy-valid").count(), 1);
    let _ = std::fs::remove_dir_all(out_dir);
}
