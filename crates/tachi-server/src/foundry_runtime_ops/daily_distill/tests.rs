use super::*;
use axum::{
    extract::State,
    response::{IntoResponse, Response},
    routing::post,
    Json, Router,
};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use tachi_llm::{
    llm::{ChatLaneConfig, ProviderRuntimeConfig},
    LlmClient, ProviderSecret, RerankConfig, RerankProviderKind,
};

struct DailyMockResponses {
    calls: Arc<AtomicUsize>,
    bodies: Vec<String>,
}

struct MockDailyProvider {
    llm: LlmClient,
    calls: Arc<AtomicUsize>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for MockDailyProvider {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl MockDailyProvider {
    async fn start(bodies: Vec<String>) -> Self {
        assert!(!bodies.is_empty(), "daily mock needs at least one response");
        let calls = Arc::new(AtomicUsize::new(0));
        let state = Arc::new(DailyMockResponses {
            calls: Arc::clone(&calls),
            bodies,
        });
        let app = Router::new()
            .route("/chat/completions", post(daily_mock_response))
            .with_state(state);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock daily provider");
        let port = listener
            .local_addr()
            .expect("mock daily provider address")
            .port();
        let task = tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("serve mock daily provider");
        });

        let unused_lane = || ChatLaneConfig {
            base_url: "https://unused.test/v1/chat/completions".to_string(),
            model: "unused".to_string(),
            api_key_envs: vec!["UNUSED_API_KEY"],
        };
        let llm = LlmClient::new_with_config(
            ProviderRuntimeConfig {
                extract: unused_lane(),
                summary: unused_lane(),
                reasoning: unused_lane(),
                distill: ChatLaneConfig {
                    base_url: format!("http://127.0.0.1:{port}/chat/completions"),
                    model: "mock-daily-distill".to_string(),
                    api_key_envs: vec!["DAILY_TEST_DISTILL_API_KEY"],
                },
                rerank: RerankConfig {
                    provider: RerankProviderKind::Voyage,
                    local_endpoint: None,
                },
            },
            None,
        )
        .expect("initialize mock daily LLM client");
        assert!(llm.set_provider_secret_pool(
            "DAILY_TEST_DISTILL_API_KEY",
            vec![ProviderSecret {
                key_id: "daily-test-key".to_string(),
                value: "test-key".to_string(),
            }],
        ));

        Self { llm, calls, task }
    }
}

async fn daily_mock_response(State(state): State<Arc<DailyMockResponses>>) -> Response {
    let call = state.calls.fetch_add(1, Ordering::SeqCst);
    let content = state
        .bodies
        .get(call)
        .or_else(|| state.bodies.last())
        .expect("daily mock always has a response")
        .clone();
    Json(json!({
        "choices": [{
            "message": {"role": "assistant", "content": content},
            "finish_reason": "stop",
        }],
        "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2},
        "model": "mock-daily-distill",
    }))
    .into_response()
}

fn daily_server_with_llm(path: std::path::PathBuf, llm: &LlmClient) -> crate::MemoryServer {
    let mut server =
        crate::MemoryServer::new(path.join("global.db"), Some(path.join("project.db")))
            .expect("open daily test server");
    server.llm = Arc::new(llm.clone());
    server
}

fn receipt_test_group(group_id: &str, offset: usize) -> CandidateGroup {
    let entries = (offset..offset + 3)
        .map(|idx| {
            let mut entry = candidate_entry(idx);
            entry.id = format!("receipt-{group_id}-{idx}");
            entry.path = format!("/project/receipt/{group_id}/{idx}");
            entry.topic = group_id.to_string();
            entry.entities = vec![group_id.to_string()];
            entry
        })
        .collect::<Vec<_>>();
    CandidateGroup {
        group_id: group_id.to_string(),
        path_prefix: format!("/project/receipt/{group_id}"),
        coherence_key: group_id.to_string(),
        entries,
    }
}

fn seed_receipt_test_groups(server: &crate::MemoryServer, groups: &[CandidateGroup]) {
    server
        .with_project_store(|store| {
            for entry in groups.iter().flat_map(|group| &group.entries) {
                store.upsert(entry).map_err(|error| error.to_string())?;
            }
            Ok(())
        })
        .expect("seed daily receipt source entries");
}

fn persisted_daily_metadata(server: &crate::MemoryServer) -> Vec<serde_json::Value> {
    server
        .with_project_store_read(|store| {
            let mut statement = store
                .connection()
                .prepare(
                    "SELECT metadata FROM memories WHERE source = 'foundry_distill' ORDER BY id",
                )
                .map_err(|error| format!("prepare daily metadata query: {error}"))?;
            let rows = statement
                .query_map([], |row| row.get::<_, String>(0))
                .map_err(|error| format!("query daily metadata: {error}"))?;
            rows.map(|row| {
                let value = row.map_err(|error| format!("read daily metadata: {error}"))?;
                serde_json::from_str(&value)
                    .map_err(|error| format!("parse daily metadata: {error}"))
            })
            .collect()
        })
        .expect("read persisted daily metadata")
}

#[tokio::test]
async fn daily_batch_copies_one_invocation_receipt_to_every_group_first_write() {
    let provider = MockDailyProvider::start(vec![r#"[
            {"group_id":"receipt-a","summary":"A","text":"batch output A","keywords":["batch"]},
            {"group_id":"receipt-b","summary":"B","text":"batch output B","keywords":["batch"]}
        ]"#
    .to_string()])
    .await;
    let temp = tempfile::tempdir().expect("temp daily batch receipt database");
    let server = daily_server_with_llm(temp.path().to_path_buf(), &provider.llm);
    let groups = vec![
        receipt_test_group("receipt-a", 0),
        receipt_test_group("receipt-b", 10),
    ];
    seed_receipt_test_groups(&server, &groups);

    let mut report = DistillBatchReport::default();
    let mut manifest = Vec::new();
    process_api_batch(
        &server,
        &groups,
        0,
        &mut report,
        &mut manifest,
        "receipt-batch",
        None,
    )
    .await;

    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
    assert_eq!(report.groups_distilled, 2);
    assert_eq!(report.fallback_used, 0);
    let metadata = persisted_daily_metadata(&server);
    assert_eq!(metadata.len(), 2);
    let first = metadata[0]
        .pointer("/provenance/model_invocation")
        .cloned()
        .expect("first batch artifact receipt");
    let second = metadata[1]
        .pointer("/provenance/model_invocation")
        .cloned()
        .expect("second batch artifact receipt");
    assert_eq!(
        first, second,
        "one batch invocation must be copied verbatim"
    );
    assert_eq!(first["schema"], "model-invocation-v1");
    assert_eq!(first["lane"], "distill");
    assert_eq!(metadata[0]["fallback_used"], false);
    assert_eq!(metadata[1]["fallback_used"], false);
}

#[tokio::test]
async fn daily_parse_failure_uses_a_fresh_receipt_for_each_group_fallback() {
    // One malformed two-group response is split into two malformed one-group
    // responses; only then does each group invoke its own successful fallback.
    let provider = MockDailyProvider::start(vec![
        "not-json-batch".to_string(),
        "not-json-left".to_string(),
        "not-json-right".to_string(),
        "fallback output for one group".to_string(),
        "fallback output for the other group".to_string(),
    ])
    .await;
    let temp = tempfile::tempdir().expect("temp daily fallback receipt database");
    let server = daily_server_with_llm(temp.path().to_path_buf(), &provider.llm);
    let groups = vec![
        receipt_test_group("fallback-a", 30),
        receipt_test_group("fallback-b", 40),
    ];
    seed_receipt_test_groups(&server, &groups);

    let mut report = DistillBatchReport::default();
    let mut manifest = Vec::new();
    process_api_batch(
        &server,
        &groups,
        0,
        &mut report,
        &mut manifest,
        "receipt-fallback",
        None,
    )
    .await;

    assert_eq!(
        provider.calls.load(Ordering::SeqCst),
        5,
        "the batch plus each leaf parse and fallback must be separate calls"
    );
    assert_eq!(report.groups_distilled, 2);
    assert_eq!(report.fallback_used, 2);
    let metadata = persisted_daily_metadata(&server);
    assert_eq!(metadata.len(), 2);
    for item in metadata {
        assert_eq!(item["fallback_used"], true);
        let receipt = item
            .pointer("/provenance/model_invocation")
            .expect("fallback durable write receipt");
        assert_eq!(receipt["schema"], "model-invocation-v1");
        assert_eq!(receipt["lane"], "distill");
    }
}

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
fn daily_distill_source_set_v1_has_frozen_name_bytes_and_uuid() {
    let group = CandidateGroup {
        group_id: "golden".to_string(),
        path_prefix: "/project/example".to_string(),
        coherence_key: "example".to_string(),
        entries: Vec::new(),
    };
    let source_ids = vec!["a".to_string(), "b".to_string()];
    let bytes = serialized_source_set_identity_bytes(&group, &source_ids);

    assert_eq!(
        String::from_utf8(bytes).expect("identity bytes are UTF-8 JSON"),
        r#"{"contract":"daily-distill-source-set-v1","path_prefix":"/project/example","coherence_key":"example","source_memory_ids":["a","b"]}"#
    );
    assert_eq!(
        stable_distill_memory_id(&group, &source_ids),
        "distill:c189ceb9-dd50-5af4-95a7-34e89b2d3669"
    );
}

#[test]
fn daily_distill_source_set_v1_distinguishes_each_identity_dimension() {
    let base = CandidateGroup {
        group_id: "identity-dimensions".to_string(),
        path_prefix: "/project/base".to_string(),
        coherence_key: "base-key".to_string(),
        entries: vec![candidate_entry(0), candidate_entry(1)],
    };
    let base_source_ids = normalized_source_memory_ids(&base);
    let base_id = stable_distill_memory_id(&base, &base_source_ids);

    let mut different_sources = base.clone();
    different_sources.entries.push(candidate_entry(2));
    let different_source_ids = normalized_source_memory_ids(&different_sources);
    assert_ne!(
        base_id,
        stable_distill_memory_id(&different_sources, &different_source_ids)
    );

    let mut different_path = base.clone();
    different_path.path_prefix = "/project/other".to_string();
    assert_ne!(
        base_id,
        stable_distill_memory_id(&different_path, &base_source_ids)
    );

    let mut different_coherence = base.clone();
    different_coherence.coherence_key = "other-key".to_string();
    assert_ne!(
        base_id,
        stable_distill_memory_id(&different_coherence, &base_source_ids)
    );
}

#[test]
fn daily_distill_existing_winner_validation_is_strict_and_typed() {
    let group = CandidateGroup {
        group_id: "strict-validation".to_string(),
        path_prefix: "/project/bounded".to_string(),
        coherence_key: "bounded-scan".to_string(),
        entries: (0..3).map(candidate_entry).collect(),
    };
    let source_ids = normalized_source_memory_ids(&group);
    let stable_id = stable_distill_memory_id(&group, &source_ids);
    let mut canonical = candidate_entry(99);
    canonical.id = stable_id.clone();
    canonical.source = "foundry_distill".to_string();
    canonical.metadata = json!({
        "source_memory_ids": source_ids,
        "source_set_contract": "daily-distill-source-set-v1",
        "source_set_identity": stable_id,
        "source_path_prefix": group.path_prefix,
        "coherence_key": group.coherence_key,
    });
    validate_existing_distill_winner(
        &canonical,
        &stable_id,
        &group.path_prefix,
        &group.coherence_key,
        &source_ids,
    )
    .expect("canonical winner must validate");

    let assert_conflict = |occupant: &MemoryEntry| {
        let error = validate_existing_distill_winner(
            occupant,
            &stable_id,
            &group.path_prefix,
            &group.coherence_key,
            &source_ids,
        )
        .expect_err("malformed identity evidence must fail");
        assert!(
            matches!(error, memcore::MemoryError::InvalidArg(_)),
            "identity collision must remain a typed InvalidArg: {error}"
        );
        assert!(
            error
                .to_string()
                .contains("daily_distill_identity_conflict"),
            "identity collision must retain its stable marker: {error}"
        );
    };

    let mut non_string = canonical.clone();
    non_string.metadata["source_memory_ids"] =
        json!(["candidate-0", 17, "candidate-1", "candidate-2"]);
    assert_conflict(&non_string);

    let mut duplicate = canonical.clone();
    duplicate.metadata["source_memory_ids"] =
        json!(["candidate-0", "candidate-1", "candidate-2", "candidate-2"]);
    assert_conflict(&duplicate);

    let mut wrong_order = canonical.clone();
    wrong_order.metadata["source_memory_ids"] =
        json!(["candidate-2", "candidate-1", "candidate-0"]);
    assert_conflict(&wrong_order);

    let mut wrong_path = canonical.clone();
    wrong_path.metadata["source_path_prefix"] = json!("/project/forged");
    assert_conflict(&wrong_path);

    let mut wrong_coherence = canonical.clone();
    wrong_coherence.metadata["coherence_key"] = json!("forged-key");
    assert_conflict(&wrong_coherence);

    let mut missing_contract = canonical;
    missing_contract
        .metadata
        .as_object_mut()
        .expect("metadata object")
        .remove("source_set_contract");
    assert_conflict(&missing_contract);
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
            cache.source = memcore::FOUNDRY_RECALL_CACHE_SOURCE.to_string();
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
            assert_eq!(
                metadata["source_set_contract"],
                json!("daily-distill-source-set-v1")
            );
            assert_eq!(metadata["source_set_identity"], json!(memory_id));
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

#[test]
fn persist_distill_memory_replay_preserves_the_first_protected_source_set_output() {
    let temp = tempfile::tempdir().expect("temp daily distill replay db");
    let server = crate::MemoryServer::new(
        temp.path().join("global.db"),
        Some(temp.path().join("project.db")),
    )
    .expect("server");
    let mut entries = (0..3).map(candidate_entry).collect::<Vec<_>>();
    for entry in &mut entries {
        entry.retention_policy = Some("pinned".to_string());
    }

    server
        .with_project_store(|store| {
            for entry in &entries {
                store.upsert(entry).map_err(|e| e.to_string())?;
            }
            Ok(())
        })
        .expect("seed protected source memories");

    let group = CandidateGroup {
        group_id: "protected-replay".to_string(),
        path_prefix: "/project/bounded".to_string(),
        coherence_key: "bounded-scan".to_string(),
        entries: entries.clone(),
    };
    let first_payload = GroupPayload {
        summary: "first committed summary".to_string(),
        text: "first committed distilled output".to_string(),
        keywords: vec!["first".to_string()],
        skip_reason: None,
    };
    let first_id = persist_distill_memory(
        &server,
        &group,
        &first_payload,
        "batch-first",
        "raw_api",
        false,
        None,
    )
    .expect("persist first output");

    let snapshot = || {
        server.with_project_store_read(|store| {
            let conn = store.connection();
            let memory: (String, String, String, String, bool, Option<String>) = conn
                .query_row(
                    "SELECT summary, text, keywords, metadata, archived, superseded_by
                     FROM memories WHERE id = ?1",
                    [&first_id],
                    |row| {
                        Ok((
                            row.get(0)?,
                            row.get(1)?,
                            row.get(2)?,
                            row.get(3)?,
                            row.get(4)?,
                            row.get(5)?,
                        ))
                    },
                )
                .map_err(|e| e.to_string())?;
            let fts: (i64, String, String, String) = conn
                .query_row(
                    "SELECT COUNT(*), summary, text, keywords FROM memories_fts WHERE id = ?1",
                    [&first_id],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
                )
                .map_err(|e| e.to_string())?;
            let vector_count: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM memories_vec WHERE id = ?1",
                    [&first_id],
                    |row| row.get(0),
                )
                .map_err(|e| e.to_string())?;
            let derived: (String, String, String) = conn
                .query_row(
                    "SELECT text, summary, metadata FROM derived_items WHERE id = ?1",
                    [format!("derived:{first_id}")],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .map_err(|e| e.to_string())?;
            let edge_count: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM memory_edges WHERE source_id = ?1 OR target_id = ?1",
                    [&first_id],
                    |row| row.get(0),
                )
                .map_err(|e| e.to_string())?;
            let source_projection: (i64, i64) = conn
                .query_row(
                    "SELECT
                         SUM(CASE WHEN archived = 1 THEN 1 ELSE 0 END),
                         SUM(CASE WHEN superseded_by IS NOT NULL THEN 1 ELSE 0 END)
                     FROM memories WHERE id LIKE 'candidate-%'",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .map_err(|e| e.to_string())?;
            Ok(json!({
                "memory": memory,
                "fts": fts,
                "vector_count": vector_count,
                "derived": derived,
                "edge_count": edge_count,
                "source_projection": source_projection,
            }))
        })
    };
    let first_state = snapshot().expect("snapshot first committed projection set");

    let mut replay_group = group.clone();
    replay_group.entries.reverse();
    let replay_payload = GroupPayload {
        summary: "conflicting replay summary".to_string(),
        text: "conflicting replay output must not overwrite the winner".to_string(),
        keywords: vec!["replay".to_string()],
        skip_reason: None,
    };
    let replay_id = persist_distill_memory(
        &server,
        &replay_group,
        &replay_payload,
        "batch-replay",
        "raw_api",
        true,
        None,
    )
    .expect("replay returns the canonical output");

    assert_eq!(
        replay_id, first_id,
        "source ordering cannot change identity"
    );
    let replay_state = snapshot().expect("snapshot replay projection set");
    assert_eq!(
        replay_state, first_state,
        "Existing must not mutate memory, FTS, vector, derived, graph, archive, supersession, or metadata projections"
    );
}

#[test]
fn persist_distill_memory_replay_finds_an_archived_canonical_output() {
    let temp = tempfile::tempdir().expect("temp archived daily distill replay db");
    let server = crate::MemoryServer::new(
        temp.path().join("global.db"),
        Some(temp.path().join("project.db")),
    )
    .expect("server");
    let mut entries = (0..3).map(candidate_entry).collect::<Vec<_>>();
    for entry in &mut entries {
        entry.retention_policy = Some("pinned".to_string());
    }
    server
        .with_project_store(|store| {
            for entry in &entries {
                store.upsert(entry).map_err(|e| e.to_string())?;
            }
            Ok(())
        })
        .expect("seed protected source memories");
    let group = CandidateGroup {
        group_id: "archived-replay".to_string(),
        path_prefix: "/project/bounded".to_string(),
        coherence_key: "bounded-scan".to_string(),
        entries,
    };
    let payload = GroupPayload {
        summary: "archived canonical summary".to_string(),
        text: "archived canonical output".to_string(),
        keywords: vec!["archived".to_string()],
        skip_reason: None,
    };
    let first_id = persist_distill_memory(
        &server,
        &group,
        &payload,
        "batch-archived-first",
        "raw_api",
        false,
        None,
    )
    .expect("persist canonical output");
    server
        .with_project_store(|store| {
            assert!(store.archive_memory(&first_id).map_err(|e| e.to_string())?);
            Ok(())
        })
        .expect("archive canonical output");

    let replay_id = persist_distill_memory(
        &server,
        &group,
        &payload,
        "batch-archived-replay",
        "raw_api",
        false,
        None,
    )
    .expect("archived replay returns canonical output");
    assert_eq!(replay_id, first_id);
    server
        .with_project_store_read(|store| {
            let state: (i64, bool) = store
                .connection()
                .query_row(
                    "SELECT COUNT(*), archived FROM memories WHERE source = 'foundry_distill'",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .map_err(|e| e.to_string())?;
            assert_eq!(state, (1, true));
            Ok(())
        })
        .expect("archived replay must not resurrect or duplicate the winner");
}

#[test]
fn persist_distill_memory_concurrent_protected_source_set_has_one_winner() {
    use std::sync::{Arc, Barrier};

    let temp = tempfile::tempdir().expect("temp concurrent daily distill db");
    let global_db = temp.path().join("global.db");
    let project_db = temp.path().join("project.db");
    let mut entries = (0..3).map(candidate_entry).collect::<Vec<_>>();
    for entry in &mut entries {
        entry.retention_policy = Some("pinned".to_string());
    }
    {
        let server =
            crate::MemoryServer::new(global_db.clone(), Some(project_db.clone())).expect("server");
        server
            .with_project_store(|store| {
                for entry in &entries {
                    store.upsert(entry).map_err(|e| e.to_string())?;
                }
                Ok(())
            })
            .expect("seed protected source memories");
    }

    let group = CandidateGroup {
        group_id: "concurrent-protected".to_string(),
        path_prefix: "/project/bounded".to_string(),
        coherence_key: "bounded-scan".to_string(),
        entries,
    };
    let barrier = Arc::new(Barrier::new(2));
    let mut writers = Vec::new();
    for writer in 0..2 {
        let global_db = global_db.clone();
        let project_db = project_db.clone();
        let group = group.clone();
        let barrier = Arc::clone(&barrier);
        writers.push(std::thread::spawn(move || {
            let server = crate::MemoryServer::new(global_db, Some(project_db)).expect("server");
            let payload = GroupPayload {
                summary: format!("writer {writer} summary"),
                text: format!("writer {writer} output"),
                keywords: vec![format!("writer-{writer}")],
                skip_reason: None,
            };
            barrier.wait();
            persist_distill_memory(
                &server,
                &group,
                &payload,
                &format!("batch-writer-{writer}"),
                "raw_api",
                false,
                None,
            )
            .expect("concurrent writer")
        }));
    }
    let ids = writers
        .into_iter()
        .map(|writer| writer.join().expect("join concurrent writer"))
        .collect::<Vec<_>>();
    assert_eq!(ids[0], ids[1]);

    let verifier = crate::MemoryServer::new(global_db, Some(project_db)).expect("verifier");
    verifier
        .with_project_store_read(|store| {
            let conn = store.connection();
            let output_count: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM memories WHERE source = 'foundry_distill'",
                    [],
                    |row| row.get(0),
                )
                .map_err(|e| e.to_string())?;
            let derived_count: i64 = conn
                .query_row("SELECT COUNT(*) FROM derived_items", [], |row| row.get(0))
                .map_err(|e| e.to_string())?;
            let edge_count: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM memory_edges WHERE source_id = ?1",
                    [&ids[0]],
                    |row| row.get(0),
                )
                .map_err(|e| e.to_string())?;
            assert_eq!(output_count, 1);
            assert_eq!(derived_count, 1);
            assert_eq!(edge_count, 3);
            Ok(())
        })
        .expect("verify one concurrent side-effect set");
}

#[test]
fn persist_distill_memory_insert_once_bypasses_write_time_jaccard_merging() {
    let temp = tempfile::tempdir().expect("temp daily distill jaccard db");
    let server = crate::MemoryServer::new(
        temp.path().join("global.db"),
        Some(temp.path().join("project.db")),
    )
    .expect("server");
    let mut entries = (0..3).map(candidate_entry).collect::<Vec<_>>();
    for entry in &mut entries {
        entry.retention_policy = Some("pinned".to_string());
    }
    let payload = GroupPayload {
        summary: "canonical jaccard-safe summary".to_string(),
        text: "a unique distilled result that exactly matches an existing row".to_string(),
        keywords: vec!["distill-only".to_string()],
        skip_reason: None,
    };
    let mut existing = candidate_entry(99);
    existing.id = "jaccard-existing".to_string();
    existing.path = "/notes/jaccard-existing".to_string();
    existing.timestamp = "2026-01-01T00:01:39Z".to_string();
    existing.text = payload.text.clone();
    existing.summary = "existing unrelated summary".to_string();
    existing.keywords = vec!["existing-only".to_string()];
    existing.entities = vec!["existing-entity".to_string()];
    existing.importance = 0.2;
    server
        .with_project_store(|store| {
            for entry in &entries {
                store.upsert(entry).map_err(|e| e.to_string())?;
            }
            store.upsert(&existing).map_err(|e| e.to_string())?;
            Ok(())
        })
        .expect("seed source and Jaccard candidate memories");
    let group = CandidateGroup {
        group_id: "jaccard-safe".to_string(),
        path_prefix: "/project/bounded".to_string(),
        coherence_key: "bounded-scan".to_string(),
        entries,
    };

    let memory_id = persist_distill_memory(
        &server,
        &group,
        &payload,
        "batch-jaccard-safe",
        "raw_api",
        false,
        None,
    )
    .expect("persist canonical output without ordinary-memory dedupe");
    server
        .with_project_store_read(|store| {
            let conn = store.connection();
            let distill_state: (Option<String>, String) = conn
                .query_row(
                    "SELECT superseded_by, tier FROM memories WHERE id = ?1",
                    [&memory_id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .map_err(|e| e.to_string())?;
            let existing_state: (String, f64) = conn
                .query_row(
                    "SELECT keywords, importance FROM memories WHERE id = ?1",
                    [&existing.id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .map_err(|e| e.to_string())?;
            assert_eq!(distill_state, (None, "consolidated".to_string()));
            assert_eq!(existing_state.0, json!(["existing-only"]).to_string());
            assert_eq!(existing_state.1, 0.2);
            Ok(())
        })
        .expect("distill insert-once must not invoke ordinary Jaccard merging");
}

#[test]
fn persist_distill_memory_refuses_a_noncanonical_stable_id_occupant() {
    let temp = tempfile::tempdir().expect("temp daily distill identity collision db");
    let server = crate::MemoryServer::new(
        temp.path().join("global.db"),
        Some(temp.path().join("project.db")),
    )
    .expect("server");
    let mut entries = (0..3).map(candidate_entry).collect::<Vec<_>>();
    for entry in &mut entries {
        entry.retention_policy = Some("pinned".to_string());
    }
    let group = CandidateGroup {
        group_id: "identity-collision".to_string(),
        path_prefix: "/project/bounded".to_string(),
        coherence_key: "bounded-scan".to_string(),
        entries,
    };
    let source_ids = normalized_source_memory_ids(&group);
    let stable_id = stable_distill_memory_id(&group, &source_ids);
    let mut occupant = candidate_entry(99);
    occupant.id = stable_id.clone();
    occupant.path = "/notes/noncanonical-occupant".to_string();
    occupant.timestamp = "2026-01-01T00:01:39Z".to_string();
    occupant.text = "unrelated occupant text".to_string();
    occupant.source = "manual".to_string();
    server
        .with_project_store(|store| {
            for entry in &group.entries {
                store.upsert(entry).map_err(|e| e.to_string())?;
            }
            store.upsert(&occupant).map_err(|e| e.to_string())?;
            Ok(())
        })
        .expect("seed source memories and stable-id occupant");
    let payload = GroupPayload {
        summary: "collision summary".to_string(),
        text: "collision output".to_string(),
        keywords: vec!["collision".to_string()],
        skip_reason: None,
    };

    let error = persist_distill_memory(
        &server,
        &group,
        &payload,
        "batch-collision",
        "raw_api",
        false,
        None,
    )
    .expect_err("an arbitrary stable-id occupant is not a canonical replay winner");
    assert!(error.contains("source-set identity collision"), "{error}");
    server
        .with_project_store_read(|store| {
            let conn = store.connection();
            let occupant_state: (String, String) = conn
                .query_row(
                    "SELECT source, text FROM memories WHERE id = ?1",
                    [&stable_id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .map_err(|e| e.to_string())?;
            let derived_count: i64 = conn
                .query_row("SELECT COUNT(*) FROM derived_items", [], |row| row.get(0))
                .map_err(|e| e.to_string())?;
            let edge_count: i64 = conn
                .query_row("SELECT COUNT(*) FROM memory_edges", [], |row| row.get(0))
                .map_err(|e| e.to_string())?;
            assert_eq!(occupant_state, ("manual".to_string(), occupant.text));
            assert_eq!(derived_count, 0);
            assert_eq!(edge_count, 0);
            Ok(())
        })
        .expect("identity collision must leave every projection untouched");
}

#[test]
fn persist_distill_memory_refuses_malformed_self_asserted_identity_metadata() {
    let temp = tempfile::tempdir().expect("temp malformed daily distill identity db");
    let server = crate::MemoryServer::new(
        temp.path().join("global.db"),
        Some(temp.path().join("project.db")),
    )
    .expect("server");
    let mut entries = (0..3).map(candidate_entry).collect::<Vec<_>>();
    for entry in &mut entries {
        entry.retention_policy = Some("pinned".to_string());
    }
    let group = CandidateGroup {
        group_id: "malformed-identity".to_string(),
        path_prefix: "/project/bounded".to_string(),
        coherence_key: "bounded-scan".to_string(),
        entries,
    };
    let source_ids = normalized_source_memory_ids(&group);
    let stable_id = stable_distill_memory_id(&group, &source_ids);
    let mut occupant = candidate_entry(99);
    occupant.id = stable_id.clone();
    occupant.path = "/foundry/forged-occupant".to_string();
    occupant.timestamp = "2026-01-01T00:01:39Z".to_string();
    occupant.source = "foundry_distill".to_string();
    occupant.metadata = json!({
        "source_memory_ids": ["candidate-0", 17, "candidate-1", "candidate-2"],
        "source_set_contract": "daily-distill-source-set-v1",
        "source_set_identity": stable_id,
        "source_path_prefix": group.path_prefix,
        "coherence_key": group.coherence_key,
    });
    server
        .with_project_store(|store| {
            for entry in &group.entries {
                store.upsert(entry).map_err(|e| e.to_string())?;
            }
            store.upsert(&occupant).map_err(|e| e.to_string())?;
            Ok(())
        })
        .expect("seed source memories and forged occupant");
    let payload = GroupPayload {
        summary: "malformed identity summary".to_string(),
        text: "malformed identity output".to_string(),
        keywords: vec!["malformed".to_string()],
        skip_reason: None,
    };

    let error = persist_distill_memory(
        &server,
        &group,
        &payload,
        "batch-malformed-identity",
        "raw_api",
        false,
        None,
    )
    .expect_err("self-asserted metadata with a non-string source id is not canonical");
    assert!(
        error.contains("daily_distill_identity_conflict"),
        "identity refusal must expose the stable conflict marker: {error}"
    );
}

#[test]
fn persist_distill_memory_does_not_project_a_conflicted_supersession_source() {
    let temp = tempfile::tempdir().expect("temp daily distill conflict db");
    let server = crate::MemoryServer::new(
        temp.path().join("global.db"),
        Some(temp.path().join("project.db")),
    )
    .expect("server");
    let source = candidate_entry(0);
    let mut canonical = candidate_entry(1);
    canonical.id = "existing-canonical".to_string();
    canonical.path = "/project/existing-canonical".to_string();

    server
        .with_project_store(|store| {
            store.insert_if_absent(&source).map_err(|e| e.to_string())?;
            store
                .insert_if_absent(&canonical)
                .map_err(|e| e.to_string())?;
            assert!(
                store
                    .supersede_memory(&source.id, &canonical.id)
                    .map_err(|e| e.to_string())?,
                "seeded source must establish its immutable prior edge"
            );
            Ok(())
        })
        .expect("seed conflicted distill source");

    let group = CandidateGroup {
        group_id: "conflicted_source".to_string(),
        path_prefix: "/project/bounded".to_string(),
        coherence_key: "bounded-scan".to_string(),
        entries: vec![source.clone()],
    };
    let payload = GroupPayload {
        summary: "distilled conflict summary".to_string(),
        text: "distilled conflict memory".to_string(),
        keywords: vec!["conflict".to_string()],
        skip_reason: None,
    };

    let err = persist_distill_memory(
        &server,
        &group,
        &payload,
        "batch-conflict",
        "raw_api",
        false,
        None,
    )
    .expect_err("conflicted source must refuse the entire distill replacement");
    assert!(
        err.contains("immutable supersession"),
        "refusal must identify the immutable-edge conflict: {err}"
    );

    server
        .with_project_store_read(|store| {
            let source_state: (bool, Option<String>) = store
                .connection()
                .query_row(
                    "SELECT archived, superseded_by FROM memories WHERE id = ?1",
                    [&source.id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .map_err(|e| e.to_string())?;
            let distill_memories: i64 = store
                .connection()
                .query_row(
                    "SELECT COUNT(*) FROM memories WHERE source = 'foundry_distill'",
                    [],
                    |row| row.get(0),
                )
                .map_err(|e| e.to_string())?;
            let derived_items: i64 = store
                .connection()
                .query_row("SELECT COUNT(*) FROM derived_items", [], |row| row.get(0))
                .map_err(|e| e.to_string())?;
            let graph_edges: i64 = store
                .connection()
                .query_row("SELECT COUNT(*) FROM memory_edges", [], |row| row.get(0))
                .map_err(|e| e.to_string())?;
            assert_eq!(
                source_state,
                (false, Some(canonical.id.clone())),
                "failed CAS must preserve the established source edge and keep it active"
            );
            assert_eq!(
                distill_memories, 0,
                "failed CAS must roll back the candidate-success memory projection"
            );
            assert_eq!(
                derived_items, 0,
                "failed CAS must roll back the derived candidate projection"
            );
            assert_eq!(
                graph_edges, 0,
                "failed CAS must roll back every candidate-success graph projection"
            );
            Ok(())
        })
        .expect("verify conflicted distill source has no side effects");
}

// ── #1261 step 2/3: CLI fallback removed from call_claude_batch ──────

/// Before #1261, this test proved the #1087 flag-off path routed to the
/// CLI binary resolver and surfaced its "existing executable" error. With
/// the CLI fallback removed in step 2/3, the invariant flips:
/// `call_claude_batch` must NEVER touch the CLI binary resolver,
/// regardless of CLAUDE_BIN / TACHI_CLAUDE_POOL_PROVIDER_FIRST. Pointing
/// CLAUDE_BIN at a nonexistent path is now a no-op for this code path —
/// the call goes straight to the provider executor (which fails because
/// no real provider is configured in the test harness, but crucially NOT
/// with the CLI-binary-resolution error the old test required). This is
/// the discriminating guard against a CLI-fallback regression: if someone
/// re-adds the `claude_pool.call()` branch, the nonexistent CLAUDE_BIN
/// would surface "existing executable" again and this test would fail.
///
/// `CLAUDE_BIN` is process-global and mutated by other test files too
/// (`tests/dispatch_tests/board_first.rs`) — this uses the crate-wide
/// `crate::utils::global_test_lock()`, matching their convention, not a
/// locally-scoped mutex. (tachi#1288 Fix C: this doc used to also point at
/// `.../v2_smoke.rs`, an `#[ignore]`d test whose own CLAUDE_BIN-driven
/// premise no longer existed after the same #1274 removal this test guards
/// against; that dead test was deleted, not merely re-pointed here.)
#[test]
fn call_claude_batch_never_reaches_cli_binary_resolver() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());

    let prev_rollout = std::env::var("TACHI_CLAUDE_POOL_PROVIDER_FIRST").ok();
    // Explicitly set the legacy opt-OUT value: if any CLI path still
    // existed, this is the flag state that would have routed to it.
    std::env::set_var("TACHI_CLAUDE_POOL_PROVIDER_FIRST", "0");
    let prev_bin = std::env::var("CLAUDE_BIN").ok();
    std::env::set_var(
        "CLAUDE_BIN",
        "/nonexistent/__tachi_test_distill_no_cli__/claude",
    );

    let temp = tempfile::tempdir().expect("temp daily distill cli-removal db");
    let server = crate::MemoryServer::new(
        temp.path().join("global.db"),
        Some(temp.path().join("project.db")),
    )
    .expect("server");

    let chunk = vec![CandidateGroup {
        group_id: "g".to_string(),
        path_prefix: "/p".to_string(),
        coherence_key: "k".to_string(),
        entries: (0..1).map(candidate_entry).collect(),
    }];

    let result = tokio::runtime::Runtime::new()
        .expect("tokio runtime")
        .block_on(call_claude_batch(&server, "label", "prompt text", &chunk));

    // The call no longer reaches the CLI binary resolver: regardless of
    // whether the provider path succeeds or fails in the test harness, it
    // must NOT surface the CLI-binary-resolution error text the old
    // behavior produced. That error string ("existing executable") is
    // unique to `resolve_claude_binary` — its absence proves the CLI path
    // is unreachable from this call site.
    let cli_resolver_unreachable = match &result {
        Err(err) => !err.contains("existing executable"),
        Ok(_) => true,
    };
    assert!(
        cli_resolver_unreachable,
        "call_claude_batch must never reach the CLI binary resolver after #1261 step 2; \
         got: {result:?}"
    );

    match prev_rollout {
        Some(v) => std::env::set_var("TACHI_CLAUDE_POOL_PROVIDER_FIRST", v),
        None => std::env::remove_var("TACHI_CLAUDE_POOL_PROVIDER_FIRST"),
    }
    match prev_bin {
        Some(v) => std::env::set_var("CLAUDE_BIN", v),
        None => std::env::remove_var("CLAUDE_BIN"),
    }
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
        scored_count: 0,
        last_access: None,
        last_use_at: None,
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
