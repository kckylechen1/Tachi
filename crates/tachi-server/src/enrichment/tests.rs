use super::*;
use crate::test_support::EnvRestore;
use crate::tests::make_server;
use crate::tool_params::SearchMemoryParams;
use chrono::Utc;
use memcore::MemoryEntry;
use rmcp::handler::server::wrapper::Parameters;
use serde_json::json;

#[test]
fn external_llm_input_scrubs_secret_patterns() {
    let input = "extract metadata for api_key=sk-abcdefghijklmnopqrstuvwxyz123456";
    let output = external_llm_input(input);

    assert!(output.contains("[REDACTED]"));
    assert!(!output.contains("sk-abcdefghijklmnopqrstuvwxyz123456"));
}

#[test]
fn transient_provider_enrichment_errors_are_deferred() {
    assert!(should_defer_enrichment_failure(
        "embedding",
        "Missing API key. Add one to Tachi Vault or set the appropriate env var."
    ));
    assert!(should_defer_enrichment_failure(
        "summary",
        "secret materialization failed: Vault is locked"
    ));
    assert!(should_defer_enrichment_failure(
        "metadata",
        "API key unavailable: all configured provider keys are temporarily unavailable; retry after about 30s"
    ));
}

#[test]
fn durable_enrichment_failures_are_recorded() {
    assert!(!should_defer_enrichment_failure(
        "embedding",
        "Voyage batch auth failure 401 Unauthorized"
    ));
    assert!(!should_defer_enrichment_failure(
        "embedding",
        "API key unavailable for [VOYAGE_API_KEY]: all configured provider keys are unusable (auth_failed: 2)"
    ));
    assert!(!should_defer_enrichment_failure(
        "db_update",
        "retry after lock contention"
    ));
}

#[test]
fn merge_enriched_keywords_dedupes_and_caps() {
    let existing = vec!["Recall".into(), "pipeline".into()];
    let expanded = vec![
        "recall".into(), // case-insensitive dupe
        "检索".into(),
        "synapse".into(),
    ];
    let merged = merge_enriched_keywords(&existing, &expanded);
    assert_eq!(merged, vec!["Recall", "pipeline", "检索", "synapse"]);

    let many: Vec<String> = (0..(MAX_ENRICHED_KEYWORDS + 5))
        .map(|i| format!("kw{i}"))
        .collect();
    let capped = merge_enriched_keywords(&[], &many);
    assert_eq!(capped.len(), MAX_ENRICHED_KEYWORDS);
}

#[test]
fn accepted_metadata_keywords_keeps_caller_tags_first() {
    assert_eq!(
        accepted_metadata_keywords(&[], vec!["sccache".into()]),
        Some(vec!["sccache".to_string()])
    );
    assert_eq!(
        accepted_metadata_keywords(&["pr-1".into()], vec!["PR-1".into(), "sccache".into()]),
        Some(vec!["pr-1".to_string(), "sccache".to_string()])
    );
    assert_eq!(
        accepted_metadata_keywords(&["pr-1".into()], vec!["PR-1".into()]),
        None
    );
    assert_eq!(accepted_metadata_keywords(&["pr-1".into()], vec![]), None);
}

#[test]
fn sanitize_enriched_keyword_strips_controls_bounds_length_rejects_punct() {
    assert_eq!(
        sanitize_enriched_keyword("  hello\u{0001}world  ").as_deref(),
        Some("helloworld")
    );
    let oversized: String = "a".repeat(MAX_KEYWORD_LEN + 20);
    let truncated = sanitize_enriched_keyword(&oversized).expect("keep truncated");
    assert_eq!(truncated.chars().count(), MAX_KEYWORD_LEN);
    assert!(sanitize_enriched_keyword("   ").is_none());
    assert!(sanitize_enriched_keyword("!!!???").is_none());
    assert!(sanitize_enriched_keyword("\u{0007}\u{009F}").is_none());
    assert_eq!(sanitize_enriched_keyword("双语").as_deref(), Some("双语"));
}

#[test]
fn merge_enriched_keywords_sanitizes_hostile_llm_output() {
    let existing = vec!["keep-me".into()];
    let expanded = vec![
        "a".repeat(MAX_KEYWORD_LEN + 10),
        "bad\u{0000}ctrl".into(),
        "!!!".into(),
        "  ".into(),
        "keep-me".into(), // dupe of existing
        "new-ok".into(),
    ];
    let merged = merge_enriched_keywords(&existing, &expanded);
    assert_eq!(
        merged,
        vec![
            "keep-me".to_string(),
            "a".repeat(MAX_KEYWORD_LEN),
            "badctrl".to_string(),
            "new-ok".to_string(),
        ]
    );
    assert!(!merged.iter().any(|k| k.contains('\0')));
    assert!(!merged.iter().any(|k| k == "!!!"));
}

#[test]
fn write_keyword_enrichment_flag_defaults_off() {
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let _unset = EnvRestore::remove(WRITE_ENRICH_KEYWORDS_ENV);
    assert!(!write_keyword_enrichment_enabled());
    let _set = EnvRestore::set(WRITE_ENRICH_KEYWORDS_ENV, "true");
    assert!(write_keyword_enrichment_enabled());
}

fn seed_entry(id: &str, text: &str, keywords: Vec<String>) -> MemoryEntry {
    MemoryEntry {
        id: id.into(),
        // Ordinary-content provenance: `/notes` is not a reserved
        // `/test`/`/tests` namespace, so the namespace search-noise
        // classifier does not withhold these ordinary recall fixtures from
        // the warm-cache search this file's flush tests exercise.
        path: "/notes/keyword-enrich".into(),
        summary: "pre-seeded summary".into(),
        text: text.into(),
        importance: 0.8,
        timestamp: Utc::now().to_rfc3339(),
        valid_from: String::new(),
        valid_until: None,
        category: "fact".into(),
        topic: "keyword-enrich".into(),
        keywords,
        persons: vec![],
        entities: vec!["tachi".into()],
        location: String::new(),
        source: "test".into(),
        scope: "general".into(),
        archived: false,
        access_count: 0,
        scored_count: 0,
        last_access: None,
        last_use_at: None,
        revision: 1,
        metadata: json!({}),
        // Present so flush does not attempt Voyage embed.
        vector: Some(vec![0.0_f32; 1024]),
        retention_policy: None,
        domain: None,
        recall_count: 0,
        query_diversity: 0,
        tier: "raw".into(),
    }
}

async fn spawn_recording_mock_extract_llm(
    body: serde_json::Value,
) -> (
    u16,
    tokio::task::JoinHandle<()>,
    std::sync::Arc<std::sync::Mutex<Vec<serde_json::Value>>>,
) {
    spawn_recording_mock_extract_llm_with_finish(body, "stop").await
}

async fn spawn_recording_mock_extract_llm_with_finish(
    body: serde_json::Value,
    finish_reason: &'static str,
) -> (
    u16,
    tokio::task::JoinHandle<()>,
    std::sync::Arc<std::sync::Mutex<Vec<serde_json::Value>>>,
) {
    use axum::{routing::post, Json, Router};

    let requests = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let app = Router::new().route(
        "/chat/completions",
        post({
            let body = body.clone();
            let requests = std::sync::Arc::clone(&requests);
            move |Json(request): Json<serde_json::Value>| {
                let body = body.clone();
                let requests = std::sync::Arc::clone(&requests);
                async move {
                    requests.lock().expect("record mock request").push(request);
                    Json(serde_json::json!({
                        "choices": [{
                            "message": {
                                "role": "assistant",
                                "content": body.to_string()
                            },
                            "finish_reason": finish_reason
                        }],
                        "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
                    }))
                }
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind mock extract llm");
    let port = listener.local_addr().expect("addr").port();
    let handle = tokio::spawn(async move {
        axum::serve(listener, app).await.expect("serve mock llm");
    });
    // Give the server a tick to accept.
    tokio::task::yield_now().await;
    (port, handle, requests)
}

async fn spawn_mock_extract_llm(body: serde_json::Value) -> (u16, tokio::task::JoinHandle<()>) {
    let (port, handle, _requests) = spawn_recording_mock_extract_llm(body).await;
    (port, handle)
}

fn assert_mock_saw_keyword_enrichment_request(
    requests: &std::sync::Arc<std::sync::Mutex<Vec<serde_json::Value>>>,
) {
    let requests = requests.lock().expect("read mock requests");
    assert!(
        requests
            .iter()
            .any(|request| request.to_string().contains("Existing keywords:")),
        "extract mock must receive a keyword-enrichment request; got {requests:?}"
    );
}

/// (a) Generated keywords are retrievable through the normal FTS search path.
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn write_side_keyword_enrichment_hits_search_via_synonym() {
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let _flag = EnvRestore::set(WRITE_ENRICH_KEYWORDS_ENV, "true");
    let _persist = EnvRestore::set("TACHI_TEST_DISABLE_PROVIDER_KEY_HEALTH_PERSIST", "1");

    let (port, mock, requests) = spawn_recording_mock_extract_llm(json!({
        "keywords": ["synapse-recall", "双语检索", "write-side-enrichment"]
    }))
    .await;
    let _base = EnvRestore::set(
        "EXTRACT_BASE_URL",
        &format!("http://127.0.0.1:{port}/chat/completions"),
    );
    let _model = EnvRestore::set("EXTRACT_MODEL", "mock-keyword-model");
    let _key = EnvRestore::set("EXTRACT_API_KEY", "test-key");

    let llm = tachi_llm::LlmClient::new().expect("construct mock extract lane");
    assert_eq!(
        llm.expand_search_keywords("provider/parser probe", &[])
            .await
            .expect("mock keyword response must parse"),
        vec![
            "synapse-recall".to_string(),
            "双语检索".to_string(),
            "write-side-enrichment".to_string(),
        ]
    );

    // Construct server AFTER extract env is pointed at the mock, then
    // inject the probed client so the flush uses the same extract lane.
    let mut server = make_server();
    server.replace_llm(llm);
    let id = format!("kw-search-{}", uuid::Uuid::new_v4());
    // Text deliberately omits the synonym; only enrichment adds it.
    let entry = seed_entry(
        &id,
        "Write-side enrichment widens FTS surface at save time without query-time LLM cost.",
        vec!["fts".into(), "enrichment".into()],
    );
    server
        .with_global_store(|store| store.upsert(&entry).map_err(|e| format!("upsert: {e}")))
        .expect("seed");

    // Precondition: synonym not in text/keywords → FTS miss.
    let before = server
        .with_global_store(|store| {
            memcore::db::search_fts(
                store.connection(),
                "synapse-recall",
                10,
                false,
                false,
                None,
                None,
                None,
                false,
            )
            .map_err(|e| format!("fts before: {e}"))
        })
        .expect("fts before");
    assert!(
        !before.contains_key(&id),
        "pre-enrichment search must miss synonym; got {before:?}"
    );

    let mut batch = vec![build_enrichment_item(
        &entry,
        false,
        false,
        DbScope::Global,
        None,
        None,
        None,
        None,
        entry.revision,
    )];
    assert!(
        batch[0].needs_keyword_enrichment,
        "flag-on seed must request keyword enrichment"
    );
    server.flush_enrichment_batch(&mut batch).await;

    assert_mock_saw_keyword_enrichment_request(&requests);

    let loaded = server
        .with_global_store(|store| {
            store
                .get(&id)
                .map_err(|e| format!("get: {e}"))
                .map(|e| e.expect("entry exists"))
        })
        .expect("load");
    assert_eq!(
        loaded.metadata["enrichment"]["keywords_status"],
        json!("enriched"),
        "mock response must parse and the keyword stage must succeed: {:?}",
        loaded.metadata
    );
    assert_eq!(
        loaded.metadata["enrichment"]["invocations"]["keywords"]["schema"],
        json!("model-invocation-v1"),
        "accepted keyword write must atomically retain its #1521 receipt: {:?}",
        loaded.metadata
    );
    assert_eq!(
        loaded.metadata["enrichment"]["invocations"]["keywords"]["lane"],
        json!("extract"),
        "receipt must describe the actual keyword lane: {:?}",
        loaded.metadata
    );
    assert!(
        loaded
            .keywords
            .iter()
            .any(|k| k.eq_ignore_ascii_case("synapse-recall")),
        "keywords must include expanded synonym: {:?}",
        loaded.keywords
    );

    let after = server
        .with_global_store(|store| {
            memcore::db::search_fts(
                store.connection(),
                "synapse-recall",
                10,
                false,
                false,
                None,
                None,
                None,
                false,
            )
            .map_err(|e| format!("fts after: {e}"))
        })
        .expect("fts after");
    assert!(
        after.contains_key(&id),
        "post-enrichment FTS must hit via generated synonym; got {after:?}"
    );

    mock.abort();
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn truncated_keyword_output_never_persists_a_field_or_receipt() {
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let _flag = EnvRestore::set(WRITE_ENRICH_KEYWORDS_ENV, "true");
    let _persist = EnvRestore::set("TACHI_TEST_DISABLE_PROVIDER_KEY_HEALTH_PERSIST", "1");
    let (port, mock, _requests) = spawn_recording_mock_extract_llm_with_finish(
        json!({"keywords": ["must-not-land"]}),
        "length",
    )
    .await;
    let _base = EnvRestore::set(
        "EXTRACT_BASE_URL",
        &format!("http://127.0.0.1:{port}/chat/completions"),
    );
    let _model = EnvRestore::set("EXTRACT_MODEL", "mock-keyword-model");
    let _key = EnvRestore::set("EXTRACT_API_KEY", "test-key");

    let llm = tachi_llm::LlmClient::new().expect("construct mock extract lane");
    let mut server = make_server();
    server.replace_llm(llm);
    let id = format!("kw-truncated-{}", uuid::Uuid::new_v4());
    let entry = seed_entry(
        &id,
        "A truncated keyword response must never become durable provenance.",
        vec!["baseline".into()],
    );
    server
        .with_global_store(|store| store.upsert(&entry).map_err(|e| format!("upsert: {e}")))
        .expect("seed");

    let mut batch = vec![build_enrichment_item(
        &entry,
        false,
        false,
        DbScope::Global,
        None,
        None,
        None,
        None,
        entry.revision,
    )];
    assert!(batch[0].needs_keyword_enrichment);
    server.flush_enrichment_batch(&mut batch).await;

    let loaded = server
        .with_global_store(|store| {
            store
                .get(&id)
                .map_err(|e| format!("get: {e}"))
                .map(|entry| entry.expect("entry exists"))
        })
        .expect("load after truncated output");
    assert_eq!(loaded.keywords, vec!["baseline".to_string()]);
    assert!(
        loaded
            .metadata
            .pointer("/enrichment/invocations/keywords")
            .is_none(),
        "truncated output must not leave a success receipt: {:?}",
        loaded.metadata
    );
    assert_eq!(loaded.metadata["enrichment"]["failed_stage"], "keywords");

    mock.abort();
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn empty_keyword_output_is_skipped_without_a_success_receipt() {
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let _flag = EnvRestore::set(WRITE_ENRICH_KEYWORDS_ENV, "true");
    let _persist = EnvRestore::set("TACHI_TEST_DISABLE_PROVIDER_KEY_HEALTH_PERSIST", "1");
    let (port, mock, _requests) = spawn_recording_mock_extract_llm(json!({"keywords": []})).await;
    let _base = EnvRestore::set(
        "EXTRACT_BASE_URL",
        &format!("http://127.0.0.1:{port}/chat/completions"),
    );
    let _model = EnvRestore::set("EXTRACT_MODEL", "mock-keyword-model");
    let _key = EnvRestore::set("EXTRACT_API_KEY", "test-key");

    let llm = tachi_llm::LlmClient::new().expect("construct mock extract lane");
    let mut server = make_server();
    server.replace_llm(llm);
    let id = format!("kw-empty-{}", uuid::Uuid::new_v4());
    let entry = seed_entry(
        &id,
        "An empty keyword response is a skip, never a durable success.",
        vec!["baseline".into()],
    );
    server
        .with_global_store(|store| store.upsert(&entry).map_err(|e| format!("upsert: {e}")))
        .expect("seed");

    let mut batch = vec![build_enrichment_item(
        &entry,
        false,
        false,
        DbScope::Global,
        None,
        None,
        None,
        None,
        entry.revision,
    )];
    server.flush_enrichment_batch(&mut batch).await;

    let loaded = server
        .with_global_store(|store| {
            store
                .get(&id)
                .map_err(|e| format!("get: {e}"))
                .map(|entry| entry.expect("entry exists"))
        })
        .expect("load after empty output");
    assert_eq!(loaded.keywords, vec!["baseline".to_string()]);
    assert_eq!(loaded.metadata["enrichment"]["keywords_status"], "skipped");
    assert!(
        loaded
            .metadata
            .pointer("/enrichment/invocations/keywords")
            .is_none(),
        "empty output must not leave a success receipt: {:?}",
        loaded.metadata
    );

    mock.abort();
}

fn search_params_for(query: &str) -> SearchMemoryParams {
    SearchMemoryParams {
        query: query.to_string(),
        query_vec: None,
        top_k: 10,
        path_prefix: None,
        include_training: false,
        include_archived: false,
        candidates_per_channel: 20,
        mmr_threshold: None,
        graph_expand_hops: 0,
        graph_relation_filter: None,
        weights: None,
        context_symbols: Vec::new(),
        agent_role: None,
        project: None,
        domain: None,
        file_context: None,
        error_context: None,
        enable_rerank: false,
        conversation_scope: None,
        auto_conversation_scope: true,
        multi_evidence_pack: true,
        as_of: None,
        include_superseded: false,
        include_test: false,
        include_metadata: false,
        // Explicit JSON so assertions can index rows by id instead of
        // parsing the markdown digest (tachi#1201 k3 default).
        format: Some("json".to_string()),
    }
}

/// tachi#1435 slice 4 / #2059 codex round 2, "tooth 1" (BUG, pre-fix RED):
/// enrichment flush write-back can add FTS-visible content (a synonym
/// keyword here) to an entry that was already searchable — a query this
/// change newly matches must not keep serving a cached answer computed
/// before the flush. Reuses this file's existing keyword-enrichment mock
/// harness (see `write_side_keyword_enrichment_hits_search_via_synonym`
/// above) rather than reinventing it.
///
/// `git stash` this file's `any_field_write` tracking + its
/// `invalidate_recall_cache_after_write` call at the end of
/// `flush_enrichment_batch` and rerun this single test to see it fail
/// pre-fix (the post-flush search still shows only the decoy row).
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn enrichment_flush_keyword_write_back_busts_a_warm_recall_cache() {
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let _flag = EnvRestore::set(WRITE_ENRICH_KEYWORDS_ENV, "true");
    let _cache_flag = EnvRestore::set("TACHI_ENABLE_RECALL_CACHE", "true");
    let _persist = EnvRestore::set("TACHI_TEST_DISABLE_PROVIDER_KEY_HEALTH_PERSIST", "1");

    let sentinel = format!("EnrichFlushSentinel{}", uuid::Uuid::new_v4().simple());

    let (port, mock, requests) =
        spawn_recording_mock_extract_llm(json!({ "keywords": [sentinel.clone()] })).await;
    let _base = EnvRestore::set(
        "EXTRACT_BASE_URL",
        &format!("http://127.0.0.1:{port}/chat/completions"),
    );
    let _model = EnvRestore::set("EXTRACT_MODEL", "mock-keyword-model");
    let _key = EnvRestore::set("EXTRACT_API_KEY", "test-key");

    let llm = tachi_llm::LlmClient::new().expect("construct mock extract lane");
    assert_eq!(
        llm.expand_search_keywords("provider/parser probe", &[])
            .await
            .expect("mock keyword response must parse"),
        vec![sentinel.clone()]
    );

    // Construct server AFTER extract env is pointed at the mock, then
    // inject the probed client so the flush uses the same extract lane.
    let mut server = make_server();
    server.replace_llm(llm);

    // Decoy: an unrelated, already-searchable memory that literally
    // contains the sentinel token — warms the cache for `sentinel`
    // pre-enrichment with a non-empty (thus cacheable) result.
    let decoy_id = format!("decoy-{}", uuid::Uuid::new_v4());
    let decoy = seed_entry(
        &decoy_id,
        &format!("{sentinel} already appears in this unrelated memory."),
        vec![],
    );
    server
        .with_global_store(|store| {
            store
                .upsert(&decoy)
                .map_err(|e| format!("upsert decoy: {e}"))
        })
        .expect("seed decoy");

    // Target: does NOT contain the sentinel yet — the keyword
    // enrichment stage will add it.
    let target_id = format!("target-{}", uuid::Uuid::new_v4());
    let target = seed_entry(
        &target_id,
        "Enrichment flush write-back cache-bust probe body.",
        // Keep the metadata extractor out of this keyword-only cache
        // test. Otherwise its mock response consumes the sentinel first,
        // and the keyword expansion is correctly classified as a
        // duplicate-only skip rather than the write this test covers.
        vec!["preexisting-cache-keyword".into()],
    );
    server
        .with_global_store(|store| {
            store
                .upsert(&target)
                .map_err(|e| format!("upsert target: {e}"))
        })
        .expect("seed target");

    // Warm the cache: pre-enrichment, the sentinel query hits only the
    // decoy.
    let first = server
        .search_memory(Parameters(search_params_for(&sentinel)))
        .await
        .expect("warm search");
    let first_rows: serde_json::Value = serde_json::from_str(&first).expect("warm search json");
    let first_rows = first_rows.as_array().expect("warm search rows array");
    assert!(
        first_rows.iter().any(|r| r["id"] == decoy_id),
        "decoy must be visible pre-enrichment: {first_rows:#?}"
    );
    assert!(
        !first_rows.iter().any(|r| r["id"] == target_id),
        "target must NOT match before enrichment: {first_rows:#?}"
    );

    // Flush enrichment on the target: the keyword stage adds `sentinel`.
    let mut batch = vec![build_enrichment_item(
        &target,
        false,
        false,
        DbScope::Global,
        None,
        None,
        None,
        None,
        target.revision,
    )];
    assert!(
        batch[0].needs_keyword_enrichment,
        "flag-on target must request keyword enrichment"
    );
    server.flush_enrichment_batch(&mut batch).await;

    assert_mock_saw_keyword_enrichment_request(&requests);
    let loaded = server
        .with_global_store(|store| {
            store
                .get(&target_id)
                .map_err(|e| format!("get enriched target: {e}"))
                .map(|entry| entry.expect("enriched target exists"))
        })
        .expect("load enriched target");
    assert_eq!(
        loaded.metadata["enrichment"]["keywords_status"],
        json!("enriched"),
        "cache assertion requires keyword stage success: {:?}",
        loaded.metadata
    );
    assert!(
        loaded.keywords.iter().any(|keyword| keyword == &sentinel),
        "cache assertion requires a successful keyword write: {:?}",
        loaded.keywords
    );

    // Post-fix: the SAME query must now surface BOTH rows, not the
    // stale decoy-only answer cached before the flush.
    let second = server
        .search_memory(Parameters(search_params_for(&sentinel)))
        .await
        .expect("post-enrichment search");
    let second_rows: serde_json::Value = serde_json::from_str(&second).expect("post search json");
    let second_rows = second_rows.as_array().expect("post search rows array");
    assert!(
        second_rows.iter().any(|r| r["id"] == target_id),
        "the just-enriched target must appear in the very next identical \
         search instead of being masked by the pre-enrichment cached \
         answer: {second_rows:#?}"
    );

    mock.abort();
}

/// (b) Enrichment failure leaves the save intact with failed status.
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn write_side_keyword_enrichment_failure_leaves_save_intact() {
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let _flag = EnvRestore::set(WRITE_ENRICH_KEYWORDS_ENV, "true");
    let _persist = EnvRestore::set("TACHI_TEST_DISABLE_PROVIDER_KEY_HEALTH_PERSIST", "1");

    // Bind a port with no accept loop → connection errors → durable failure.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind closed provider");
    let port = listener.local_addr().expect("addr").port();
    drop(listener);

    let _base = EnvRestore::set(
        "EXTRACT_BASE_URL",
        &format!("http://127.0.0.1:{port}/chat/completions"),
    );
    let _model = EnvRestore::set("EXTRACT_MODEL", "mock-keyword-model");
    let _key = EnvRestore::set("EXTRACT_API_KEY", "test-key");

    let server = make_server();
    let id = format!("kw-fail-{}", uuid::Uuid::new_v4());
    let original_text = "Save must survive keyword enrichment provider failures without data loss.";
    let entry = seed_entry(&id, original_text, vec!["intact".into()]);
    server
        .with_global_store(|store| store.upsert(&entry).map_err(|e| format!("upsert: {e}")))
        .expect("seed");

    let mut batch = vec![build_enrichment_item(
        &entry,
        false,
        false,
        DbScope::Global,
        None,
        None,
        None,
        None,
        entry.revision,
    )];
    server.flush_enrichment_batch(&mut batch).await;

    let loaded = server
        .with_global_store(|store| {
            store
                .get(&id)
                .map_err(|e| format!("get: {e}"))
                .map(|e| e.expect("entry must still exist"))
        })
        .expect("load after failure");
    assert_eq!(
        loaded.text, original_text,
        "save payload must remain intact"
    );
    assert_eq!(loaded.keywords, vec!["intact".to_string()]);
    assert_eq!(
        loaded.metadata["enrichment"]["status"],
        json!("failed"),
        "overall enrichment status must be failed: {:?}",
        loaded.metadata
    );
    assert_eq!(
        loaded.metadata["enrichment"]["failed_stage"],
        json!("keywords")
    );
    assert_eq!(
        loaded.metadata["enrichment"]["keywords_status"],
        json!("failed"),
        "keywords_status must distinguish failed: {:?}",
        loaded.metadata
    );
}

/// (c) Flag off = zero behavior change (no keyword expansion, no status field).
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn write_side_keyword_enrichment_flag_off_is_noop() {
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let _flag = EnvRestore::remove(WRITE_ENRICH_KEYWORDS_ENV);
    let _persist = EnvRestore::set("TACHI_TEST_DISABLE_PROVIDER_KEY_HEALTH_PERSIST", "1");

    // Mock that would inject a synonym if the job ran.
    let (port, mock) = spawn_mock_extract_llm(json!({
        "keywords": ["should-not-appear-when-flag-off"]
    }))
    .await;
    let _base = EnvRestore::set(
        "EXTRACT_BASE_URL",
        &format!("http://127.0.0.1:{port}/chat/completions"),
    );
    let _model = EnvRestore::set("EXTRACT_MODEL", "mock-keyword-model");
    let _key = EnvRestore::set("EXTRACT_API_KEY", "test-key");

    let server = make_server();
    let id = format!("kw-off-{}", uuid::Uuid::new_v4());
    let entry = seed_entry(
        &id,
        "Flag-off path must not expand keywords or write keywords_status.",
        vec!["baseline".into()],
    );
    server
        .with_global_store(|store| store.upsert(&entry).map_err(|e| format!("upsert: {e}")))
        .expect("seed");

    let mut batch = vec![build_enrichment_item(
        &entry,
        false,
        false,
        DbScope::Global,
        None,
        None,
        None,
        None,
        entry.revision,
    )];
    assert!(
        !batch[0].needs_keyword_enrichment,
        "flag-off must not request keyword enrichment"
    );
    server.flush_enrichment_batch(&mut batch).await;

    let loaded = server
        .with_global_store(|store| {
            store
                .get(&id)
                .map_err(|e| format!("get: {e}"))
                .map(|e| e.expect("entry exists"))
        })
        .expect("load");
    assert_eq!(loaded.keywords, vec!["baseline".to_string()]);
    assert!(
        loaded
            .metadata
            .get("enrichment")
            .and_then(|e| e.get("keywords_status"))
            .is_none(),
        "flag-off must not write keywords_status: {:?}",
        loaded.metadata
    );

    let hits = server
        .with_global_store(|store| {
            memcore::db::search_fts(
                store.connection(),
                "should-not-appear-when-flag-off",
                10,
                false,
                false,
                None,
                None,
                None,
                false,
            )
            .map_err(|e| format!("fts: {e}"))
        })
        .expect("fts");
    assert!(
        !hits.contains_key(&id),
        "flag-off must not index mock synonym"
    );

    mock.abort();
}

/// Caller tags must not suppress metadata extraction (#2176): extracted
/// keywords merge after the tags, entities fill, and the recorded
/// invocation stops a second extraction.
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn caller_tags_do_not_suppress_metadata_extraction() {
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let _flag = EnvRestore::remove(WRITE_ENRICH_KEYWORDS_ENV);
    let _persist = EnvRestore::set("TACHI_TEST_DISABLE_PROVIDER_KEY_HEALTH_PERSIST", "1");

    let (port, mock) = spawn_mock_extract_llm(json!({
        "keywords": ["sccache", "pr-2133"],
        "entities": ["zeroclaw"]
    }))
    .await;
    let _base = EnvRestore::set(
        "EXTRACT_BASE_URL",
        &format!("http://127.0.0.1:{port}/chat/completions"),
    );
    let _model = EnvRestore::set("EXTRACT_MODEL", "mock-metadata-model");
    let _key = EnvRestore::set("EXTRACT_API_KEY", "test-key");

    let server = make_server();
    let id = format!("tagged-{}", uuid::Uuid::new_v4());
    let mut entry = seed_entry(
        &id,
        "zeroclaw build hung on a stale sccache server; restarting it fixed the build.",
        vec!["eng-checkpoint".into(), "pr-2133".into()],
    );
    entry.entities = vec![];
    server
        .with_global_store(|store| store.upsert(&entry).map_err(|e| format!("upsert: {e}")))
        .expect("seed");

    let mut batch = vec![build_enrichment_item(
        &entry,
        false,
        false,
        DbScope::Global,
        None,
        None,
        None,
        None,
        entry.revision,
    )];
    assert!(
        batch[0].needs_metadata,
        "caller tags must not suppress extraction"
    );
    server.flush_enrichment_batch(&mut batch).await;

    let loaded = server
        .with_global_store(|store| {
            store
                .get(&id)
                .map_err(|e| format!("get: {e}"))
                .map(|e| e.expect("entry exists"))
        })
        .expect("load");
    assert_eq!(
        loaded.keywords,
        vec![
            "eng-checkpoint".to_string(),
            "pr-2133".to_string(),
            "sccache".to_string()
        ],
        "caller tags first, extracted keywords merged after: {:?}",
        loaded.metadata
    );
    assert_eq!(loaded.entities, vec!["zeroclaw".to_string()]);
    assert!(
        loaded.metadata["enrichment"]["invocations"]["metadata"].is_object(),
        "accepted extraction must retain its receipt: {:?}",
        loaded.metadata
    );
    assert!(
        !needs_metadata_enrichment(&loaded),
        "a recorded extraction must not be requested again"
    );

    mock.abort();
}

/// A tagged row whose extraction adds nothing is marked attempted, so it
/// is not extracted again and an admitted intent can retire (#2176).
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn tagged_noop_metadata_extraction_is_marked_attempted() {
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let _flag = EnvRestore::remove(WRITE_ENRICH_KEYWORDS_ENV);
    let _persist = EnvRestore::set("TACHI_TEST_DISABLE_PROVIDER_KEY_HEALTH_PERSIST", "1");

    let (port, mock) = spawn_mock_extract_llm(json!({
        "keywords": ["PR-2133"],
        "entities": []
    }))
    .await;
    let _base = EnvRestore::set(
        "EXTRACT_BASE_URL",
        &format!("http://127.0.0.1:{port}/chat/completions"),
    );
    let _model = EnvRestore::set("EXTRACT_MODEL", "mock-metadata-model");
    let _key = EnvRestore::set("EXTRACT_API_KEY", "test-key");

    let server = make_server();
    let id = format!("tagged-noop-{}", uuid::Uuid::new_v4());
    let mut entry = seed_entry(&id, "merged", vec!["pr-2133".into()]);
    entry.entities = vec![];
    server
        .with_global_store(|store| store.upsert(&entry).map_err(|e| format!("upsert: {e}")))
        .expect("seed");

    let mut batch = vec![build_enrichment_item(
        &entry,
        false,
        false,
        DbScope::Global,
        None,
        None,
        None,
        None,
        entry.revision,
    )];
    assert!(batch[0].needs_metadata);
    server.flush_enrichment_batch(&mut batch).await;

    let loaded = server
        .with_global_store(|store| {
            store
                .get(&id)
                .map_err(|e| format!("get: {e}"))
                .map(|e| e.expect("entry exists"))
        })
        .expect("load");
    assert_eq!(loaded.keywords, vec!["pr-2133".to_string()]);
    assert!(loaded.entities.is_empty());
    assert!(
        loaded.metadata["enrichment"]["invocations"]
            .get("metadata")
            .is_none(),
        "no field accepted, so no receipt: {:?}",
        loaded.metadata
    );
    assert_eq!(
        loaded.metadata["enrichment"]["metadata_status"],
        json!("attempted")
    );
    assert!(!needs_metadata_enrichment(&loaded));
    let intent = AdmittedEnrichmentIntentV1 {
        version: ADMITTED_ENRICHMENT_INTENT_VERSION,
        job_id: "job".into(),
        idempotency_key: "key".into(),
        entry_id: id.clone(),
        revision: loaded.revision,
        target_db: "global".into(),
        named_project: None,
        needs_embedding: false,
        needs_summary: false,
        needs_metadata: true,
        owner_token: None,
    };
    assert!(admitted_enrichment_is_terminal(&loaded, &intent));

    mock.abort();
}

/// Hostile LLM keyword payload is sanitized at the write boundary before FTS.
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn write_side_keyword_enrichment_sanitizes_hostile_llm_keywords() {
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let _flag = EnvRestore::set(WRITE_ENRICH_KEYWORDS_ENV, "true");
    let _persist = EnvRestore::set("TACHI_TEST_DISABLE_PROVIDER_KEY_HEALTH_PERSIST", "1");

    let oversized = "x".repeat(MAX_KEYWORD_LEN + 32);
    let (port, mock, requests) = spawn_recording_mock_extract_llm(json!({
        "keywords": [
            oversized,
            "good\u{0001}keyword",
            "!!!",
            "  ",
            "dup",
            "DUP",
            "safe-term"
        ]
    }))
    .await;
    let _base = EnvRestore::set(
        "EXTRACT_BASE_URL",
        &format!("http://127.0.0.1:{port}/chat/completions"),
    );
    let _model = EnvRestore::set("EXTRACT_MODEL", "mock-keyword-model");
    let _key = EnvRestore::set("EXTRACT_API_KEY", "test-key");

    let llm = tachi_llm::LlmClient::new().expect("construct mock extract lane");
    assert_eq!(
        llm.expand_search_keywords("provider/parser probe", &[])
            .await
            .expect("mock keyword response must parse"),
        vec![
            "x".repeat(MAX_KEYWORD_LEN),
            "goodkeyword".to_string(),
            "dup".to_string(),
            "DUP".to_string(),
            "safe-term".to_string(),
        ]
    );

    let mut server = make_server();
    server.replace_llm(llm);
    let id = format!("kw-sanitize-{}", uuid::Uuid::new_v4());
    let entry = seed_entry(
        &id,
        "Sanitization must drop controls, pure punct, and bound length.",
        vec!["dup".into()],
    );
    server
        .with_global_store(|store| store.upsert(&entry).map_err(|e| format!("upsert: {e}")))
        .expect("seed");

    let mut batch = vec![build_enrichment_item(
        &entry,
        false,
        false,
        DbScope::Global,
        None,
        None,
        None,
        None,
        entry.revision,
    )];
    server.flush_enrichment_batch(&mut batch).await;

    assert_mock_saw_keyword_enrichment_request(&requests);

    let loaded = server
        .with_global_store(|store| {
            store
                .get(&id)
                .map_err(|e| format!("get: {e}"))
                .map(|e| e.expect("entry exists"))
        })
        .expect("load");

    assert_eq!(
        loaded.metadata["enrichment"]["keywords_status"],
        json!("enriched"),
        "mock response must parse and the keyword stage must succeed: {:?}",
        loaded.metadata
    );

    assert!(
        loaded
            .keywords
            .iter()
            .all(|k| k.chars().count() <= MAX_KEYWORD_LEN),
        "no keyword may exceed max len: {:?}",
        loaded.keywords
    );
    assert!(
        loaded.keywords.iter().all(|k| !k.chars().any(|c| {
            let u = c as u32;
            u <= 0x1F || (0x7F..=0x9F).contains(&u)
        })),
        "control chars must be stripped: {:?}",
        loaded.keywords
    );
    assert!(
        !loaded.keywords.iter().any(|k| k == "!!!"),
        "pure punctuation must be dropped: {:?}",
        loaded.keywords
    );
    assert!(
        loaded.keywords.iter().any(|k| k == "goodkeyword"),
        "control-stripped token kept: {:?}",
        loaded.keywords
    );
    assert!(
        loaded.keywords.iter().any(|k| k == "safe-term"),
        "safe term kept: {:?}",
        loaded.keywords
    );
    // case-insensitive dedupe of dup/DUP
    assert_eq!(
        loaded
            .keywords
            .iter()
            .filter(|k| k.eq_ignore_ascii_case("dup"))
            .count(),
        1,
        "duplicates collapsed: {:?}",
        loaded.keywords
    );
    assert_eq!(
        loaded.metadata["enrichment"]["keywords_status"],
        json!("enriched")
    );

    mock.abort();
}

/// existing_keywords with secret-shaped tokens are scrubbed before LLM egress.
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn write_side_keyword_enrichment_scrubs_existing_keywords_in_prompt() {
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let _flag = EnvRestore::set(WRITE_ENRICH_KEYWORDS_ENV, "true");
    let _persist = EnvRestore::set("TACHI_TEST_DISABLE_PROVIDER_KEY_HEALTH_PERSIST", "1");

    use axum::{body::Bytes, routing::post, Json, Router};
    use std::sync::{Arc, Mutex};

    let captured: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let captured_for_handler = captured.clone();
    let app = Router::new().route(
        "/chat/completions",
        post(move |body: Bytes| {
            let captured = captured_for_handler.clone();
            async move {
                let text = String::from_utf8_lossy(&body).into_owned();
                captured.lock().expect("lock").push(text);
                Json(serde_json::json!({
                    "choices": [{
                        "message": {
                            "role": "assistant",
                            "content": "{\"keywords\":[\"synonym-ok\"]}"
                        },
                        "finish_reason": "stop"
                    }],
                    "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
                }))
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind mock");
    let port = listener.local_addr().expect("addr").port();
    let mock = tokio::spawn(async move {
        axum::serve(listener, app).await.expect("serve");
    });
    tokio::task::yield_now().await;

    let _base = EnvRestore::set(
        "EXTRACT_BASE_URL",
        &format!("http://127.0.0.1:{port}/chat/completions"),
    );
    let _model = EnvRestore::set("EXTRACT_MODEL", "mock-keyword-model");
    let _key = EnvRestore::set("EXTRACT_API_KEY", "test-key");

    let server = make_server();
    let id = format!("kw-scrub-{}", uuid::Uuid::new_v4());
    let secret = "sk-abcdefghijklmnopqrstuvwxyz123456";
    let entry = seed_entry(
        &id,
        "Memory text without the secret token in body.",
        vec![format!("api_key={secret}"), "safe-seed".into()],
    );
    server
        .with_global_store(|store| store.upsert(&entry).map_err(|e| format!("upsert: {e}")))
        .expect("seed");

    let mut batch = vec![build_enrichment_item(
        &entry,
        false,
        false,
        DbScope::Global,
        None,
        None,
        None,
        None,
        entry.revision,
    )];
    server.flush_enrichment_batch(&mut batch).await;

    let bodies = captured.lock().expect("lock").clone();
    assert!(
        !bodies.is_empty(),
        "mock must have received at least one extract request"
    );
    for body in &bodies {
        assert!(
            !body.contains(secret),
            "secret must not egress in prompt body: {body}"
        );
        assert!(
            body.contains("[REDACTED]") || body.contains("safe-seed"),
            "scrubbed seed or safe keyword expected in body: {body}"
        );
    }

    mock.abort();
}
