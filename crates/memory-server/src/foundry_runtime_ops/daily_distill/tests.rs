use super::*;

#[test]
fn parse_distill_response_handles_array() {
    let raw = r#"[
            {"group_id":"g1","summary":"s1","text":"text one","keywords":["a","b"]},
            {"group_id":"g2","summary":"s2","text":"","skip_reason":"no signal"}
        ]"#;
    let map = parse_distill_response(raw).unwrap();
    assert_eq!(map.len(), 2);
    let g1 = map.get("g1").unwrap();
    assert_eq!(g1.text, "text one");
    assert_eq!(g1.keywords, vec!["a", "b"]);
    let g2 = map.get("g2").unwrap();
    assert_eq!(g2.text, "");
    assert_eq!(g2.skip_reason.as_deref(), Some("no signal"));
}

#[test]
fn parse_distill_response_strips_fences() {
    let raw = "```json\n[{\"group_id\":\"x\",\"text\":\"hi\",\"summary\":\"\"}]\n```";
    let map = parse_distill_response(raw).unwrap();
    assert!(map.contains_key("x"));
}

#[test]
fn parse_distill_response_rejects_non_array() {
    let err = parse_distill_response(r#"{"group_id":"x"}"#).unwrap_err();
    assert!(err.contains("must be a JSON array"), "got: {err}");
}

#[test]
fn sanitize_id_segment_keeps_safe_chars() {
    assert_eq!(sanitize_id_segment("topic:foo bar"), "topic_foo_bar");
    assert_eq!(sanitize_id_segment("/project/x"), "project_x");
}

#[test]
fn resolve_distill_backend_defaults_to_raw_api() {
    with_backend_env(None, || {
        assert_eq!(resolve_distill_backend(), DistillBackend::RawApi);
    });
}

#[test]
fn resolve_distill_backend_recognises_claude_cli() {
    with_backend_env(Some("claude_cli"), || {
        assert_eq!(resolve_distill_backend(), DistillBackend::ClaudeCli);
    });
}

#[test]
fn resolve_batch_size_defaults_to_six() {
    with_batch_size_env(None, || {
        assert_eq!(resolve_batch_size(), DEFAULT_GROUPS_PER_BATCH);
    });
}

#[test]
fn resolve_scan_limit_rejects_unbounded_values() {
    with_scan_limit_env("FOUNDRY_DISTILL_CANDIDATE_SCAN_LIMIT", Some("4"), || {
        assert_eq!(resolve_candidate_scan_limit(), 4);
    });
    with_scan_limit_env(
        "FOUNDRY_DISTILL_CANDIDATE_SCAN_LIMIT",
        Some("1000000"),
        || {
            assert_eq!(resolve_candidate_scan_limit(), DEFAULT_CANDIDATE_SCAN_LIMIT);
        },
    );
    with_scan_limit_env("FOUNDRY_DISTILL_PROCESSED_SCAN_LIMIT", Some("0"), || {
        assert_eq!(resolve_processed_scan_limit(), DEFAULT_PROCESSED_SCAN_LIMIT);
    });
}

#[tokio::test]
async fn collect_candidate_groups_respects_candidate_scan_limit() {
    with_scan_limit_env("FOUNDRY_DISTILL_CANDIDATE_SCAN_LIMIT", Some("4"), || {
        let temp = tempfile::tempdir().expect("temp daily distill db");
        let server = crate::MemoryServer::new(
            temp.path().join("global.db"),
            Some(temp.path().join("project.db")),
        )
        .expect("server");

        server
            .with_project_store(|store| {
                for idx in 0..6 {
                    store
                        .upsert(&candidate_entry(idx))
                        .map_err(|e| e.to_string())?;
                }
                Ok(())
            })
            .expect("seed candidate memories");

        let groups = collect_candidate_groups(&server, None).expect("collect candidate groups");
        assert_eq!(groups.len(), 1);
        let ids = groups[0]
            .entries
            .iter()
            .map(|entry| entry.id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            ids,
            vec!["candidate-0", "candidate-1", "candidate-2", "candidate-3"]
        );
    });
}

#[tokio::test]
async fn collect_candidate_groups_filters_namespace_noise() {
    let temp = tempfile::tempdir().expect("temp daily distill noise db");
    let server = crate::MemoryServer::new(
        temp.path().join("global.db"),
        Some(temp.path().join("project.db")),
    )
    .expect("server");

    server
        .with_project_store(|store| {
            for idx in 0..3 {
                store
                    .upsert(&candidate_entry(idx))
                    .map_err(|e| e.to_string())?;
            }

            let mut cache = candidate_entry(3);
            cache.id = "cache-noise".to_string();
            cache.path = "/scratch/recall-cache/noise".to_string();
            cache.source = memory_core::FOUNDRY_RECALL_CACHE_SOURCE.to_string();
            cache.topic = "recall_rerank_cache".to_string();
            cache.metadata = json!({"recall_rerank_cache": true});
            store.upsert(&cache).map_err(|e| e.to_string())?;

            let mut wiki = candidate_entry(4);
            wiki.id = "wiki-noise".to_string();
            wiki.path = "/scratch/wiki-noise".to_string();
            wiki.domain = Some("wiki".to_string());
            store.upsert(&wiki).map_err(|e| e.to_string())?;

            let mut quarantine = candidate_entry(5);
            quarantine.id = "quarantine-noise".to_string();
            quarantine.path = "/_quarantine/cross-db/noise".to_string();
            store.upsert(&quarantine).map_err(|e| e.to_string())?;

            Ok(())
        })
        .expect("seed candidate memories");

    let groups = collect_candidate_groups(&server, None).expect("collect candidate groups");
    assert_eq!(groups.len(), 1);
    let ids = groups[0]
        .entries
        .iter()
        .map(|entry| entry.id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(ids, vec!["candidate-0", "candidate-1", "candidate-2"]);
}

#[test]
fn persist_distill_memory_writes_graph_and_derived_item() {
    let temp = tempfile::tempdir().expect("temp daily distill persist db");
    let server = crate::MemoryServer::new(
        temp.path().join("global.db"),
        Some(temp.path().join("project.db")),
    )
    .expect("server");
    let entries = (0..3).map(candidate_entry).collect::<Vec<_>>();

    server
        .with_project_store(|store| {
            for entry in &entries {
                store.upsert(entry).map_err(|e| e.to_string())?;
            }
            Ok(())
        })
        .expect("seed source memories");

    let group = CandidateGroup {
        group_id: "bounded_scan".to_string(),
        path_prefix: "/project/bounded".to_string(),
        coherence_key: "bounded-scan".to_string(),
        entries,
    };
    let payload = GroupPayload {
        summary: "distilled bounded summary".to_string(),
        text: "distilled bounded memory with durable lesson".to_string(),
        keywords: vec!["bounded".to_string()],
        skip_reason: None,
    };

    let memory_id = persist_distill_memory(
        &server,
        &group,
        &payload,
        "batch-test",
        "raw_api",
        false,
        None,
    )
    .expect("persist distill");

    server
        .with_project_store_read(|store| {
            let conn = store.connection();
            let (path, retention, domain, metadata_raw): (
                String,
                Option<String>,
                Option<String>,
                String,
            ) = conn
                .query_row(
                    "SELECT path, retention_policy, domain, metadata FROM memories WHERE id=?1",
                    rusqlite::params![&memory_id],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
                )
                .map_err(|e| e.to_string())?;
            let metadata: serde_json::Value =
                serde_json::from_str(&metadata_raw).map_err(|e| e.to_string())?;
            let edge_count: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM memory_edges WHERE source_id=?1 OR target_id=?1",
                    rusqlite::params![&memory_id],
                    |row| row.get(0),
                )
                .map_err(|e| e.to_string())?;
            let derived_count: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM derived_items WHERE id=?1",
                    rusqlite::params![format!("derived:{memory_id}")],
                    |row| row.get(0),
                )
                .map_err(|e| e.to_string())?;
            let archived_sources: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM memories
                     WHERE id LIKE 'candidate-%' AND archived=1 AND superseded_by=?1",
                    rusqlite::params![&memory_id],
                    |row| row.get(0),
                )
                .map_err(|e| e.to_string())?;
            let supersedes_edges: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM memory_edges
                     WHERE source_id=?1 AND relation='supersedes'",
                    rusqlite::params![&memory_id],
                    |row| row.get(0),
                )
                .map_err(|e| e.to_string())?;

            assert!(
                path.starts_with("/foundry/agents/tachi_scheduler/distilled/"),
                "distill path should come from the foundry plan: {path}"
            );
            assert_eq!(retention.as_deref(), Some("permanent"));
            assert_eq!(domain.as_deref(), Some("foundry"));
            assert_eq!(metadata["source_path_prefix"], json!("/project/bounded"));
            assert_eq!(metadata["namespace_key"], json!("/project/bounded"));
            assert_eq!(metadata["coherence_key"], json!("bounded-scan"));
            assert_eq!(
                metadata["bucket_key"],
                json!("/project/bounded#bounded-scan")
            );
            assert_eq!(
                metadata["source_memory_ids"],
                json!(["candidate-0", "candidate-1", "candidate-2"])
            );
            assert!(
                edge_count >= 3,
                "expected at least one distill edge per source, got {edge_count}"
            );
            assert_eq!(derived_count, 1);
            assert_eq!(archived_sources, 3);
            assert_eq!(supersedes_edges, 3);
            Ok(())
        })
        .expect("verify distill graph and derived rows");
}

#[test]
fn persist_distill_memory_preserves_used_or_protected_raw_sources() {
    let temp = tempfile::tempdir().expect("temp daily distill guarded source db");
    let server = crate::MemoryServer::new(
        temp.path().join("global.db"),
        Some(temp.path().join("project.db")),
    )
    .expect("server");
    let mut entries = (0..5).map(candidate_entry).collect::<Vec<_>>();
    entries[1].access_count = 1;
    entries[2].recall_count = 1;
    entries[3].retention_policy = Some("pinned".to_string());
    entries[4].importance = 0.95;

    server
        .with_project_store(|store| {
            for entry in &entries {
                store.upsert(entry).map_err(|e| e.to_string())?;
            }
            Ok(())
        })
        .expect("seed source memories");

    let group = CandidateGroup {
        group_id: "guarded_sources".to_string(),
        path_prefix: "/project/bounded".to_string(),
        coherence_key: "bounded-scan".to_string(),
        entries,
    };
    let payload = GroupPayload {
        summary: "distilled guarded summary".to_string(),
        text: "distilled guarded memory with durable lesson".to_string(),
        keywords: vec!["guarded".to_string()],
        skip_reason: None,
    };

    let memory_id = persist_distill_memory(
        &server,
        &group,
        &payload,
        "batch-guarded",
        "raw_api",
        false,
        None,
    )
    .expect("persist distill");

    server
        .with_project_store_read(|store| {
            let conn = store.connection();
            let mut stmt = conn
                .prepare(
                    "SELECT id, archived, superseded_by
                     FROM memories
                     WHERE id LIKE 'candidate-%'
                     ORDER BY id",
                )
                .map_err(|e| e.to_string())?;
            let rows = stmt
                .query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, bool>(1)?,
                        row.get::<_, Option<String>>(2)?,
                    ))
                })
                .map_err(|e| e.to_string())?;
            let states = rows
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| e.to_string())?;

            assert_eq!(states.len(), 5);
            assert_eq!(
                states[0],
                ("candidate-0".to_string(), true, Some(memory_id.clone()))
            );
            for (id, archived, superseded_by) in states.iter().skip(1) {
                assert!(!archived, "{id} should stay active");
                assert_eq!(
                    superseded_by.as_deref(),
                    None,
                    "{id} should not be superseded"
                );
            }
            Ok(())
        })
        .expect("verify guarded sources");
}

fn candidate_entry(idx: usize) -> MemoryEntry {
    MemoryEntry {
        id: format!("candidate-{idx}"),
        path: format!("/project/bounded/{idx}"),
        summary: format!("bounded scan candidate {idx}"),
        text: format!("bounded scan candidate memory {idx}"),
        importance: 0.7,
        timestamp: format!("2026-01-01T00:00:{idx:02}Z"),
        valid_from: String::new(),
        valid_until: None,
        category: "fact".to_string(),
        topic: "bounded-scan".to_string(),
        keywords: vec!["bounded".to_string()],
        persons: Vec::new(),
        entities: vec!["bounded-scan".to_string()],
        location: String::new(),
        source: "manual".to_string(),
        scope: "project".to_string(),
        archived: false,
        access_count: 0,
        last_access: None,
        revision: 1,
        metadata: json!({}),
        vector: None,
        retention_policy: None,
        domain: None,
        recall_count: 0,
        query_diversity: 0,
        tier: "raw".to_string(),
    }
}

fn with_backend_env<F: FnOnce()>(value: Option<&str>, f: F) {
    use std::sync::{Mutex, OnceLock};
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    let _guard = LOCK.get_or_init(|| Mutex::new(())).lock().unwrap();
    let key = "FOUNDRY_DISTILL_BACKEND";
    let previous = std::env::var(key).ok();
    match value {
        Some(v) => std::env::set_var(key, v),
        None => std::env::remove_var(key),
    }
    f();
    match previous {
        Some(v) => std::env::set_var(key, v),
        None => std::env::remove_var(key),
    }
}

fn with_batch_size_env<F: FnOnce()>(value: Option<&str>, f: F) {
    use std::sync::{Mutex, OnceLock};
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    let _guard = LOCK.get_or_init(|| Mutex::new(())).lock().unwrap();
    let key = "FOUNDRY_DISTILL_BATCH_SIZE";
    let previous = std::env::var(key).ok();
    match value {
        Some(v) => std::env::set_var(key, v),
        None => std::env::remove_var(key),
    }
    f();
    match previous {
        Some(v) => std::env::set_var(key, v),
        None => std::env::remove_var(key),
    }
}

fn with_scan_limit_env<F: FnOnce()>(key: &'static str, value: Option<&str>, f: F) {
    use std::sync::{Mutex, OnceLock};
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    let _guard = LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let previous = std::env::var(key).ok();
    match value {
        Some(v) => std::env::set_var(key, v),
        None => std::env::remove_var(key),
    }
    f();
    match previous {
        Some(v) => std::env::set_var(key, v),
        None => std::env::remove_var(key),
    }
}
