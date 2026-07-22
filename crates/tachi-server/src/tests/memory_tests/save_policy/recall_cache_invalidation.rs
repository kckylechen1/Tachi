//! tachi#1435 slice 3 / #2059: save-side recall-cache invalidation contract.
//!
//! These tests exercise `handle_save_memory` at the handler level (no MCP
//! transport) plus the `search_memory` facade, with `TACHI_ENABLE_RECALL_CACHE`
//! flipped on for the duration of the test (env-guard + serial lock, matching
//! the repo's existing pattern in `enrichment.rs`'s tests — the flag is
//! process-global env state, so parallel tests must not race it).
//!
//! Row A / row B bodies below are deliberately built from disjoint word sets
//! (sharing only the needle token) rather than a single trailing-letter
//! difference: memcore's write-time near-duplicate merge (#1115,
//! `merge_into_jaccard_candidate`) tokenizes on whitespace/punctuation with a
//! `>= 2 chars` floor, so two texts differing only in a single trailing
//! letter ("row A" vs "row B") tokenize identically (`"a"`/`"b"` both get
//! filtered) and silently collapse into ONE memory row — a real, unrelated
//! repo behavior this suite must not trip over.

use super::*;
use crate::memory_search_ops::handle_save_memory;
use crate::test_support::EnvRestore;
use serde_json::Value;

fn save_params(path: &str, text: &str) -> SaveMemoryParams {
    SaveMemoryParams {
        text: text.to_string(),
        summary: String::new(),
        path: path.to_string(),
        importance: 0.7,
        category: "fact".to_string(),
        topic: String::new(),
        keywords: Vec::new(),
        persons: Vec::new(),
        entities: Vec::new(),
        location: String::new(),
        scope: "global".to_string(),
        vector: None,
        id: None,
        force: false,
        auto_link: false,
        project: None,
        project_explicit: false,
        retention_policy: None,
        domain: Some("scratch".to_string()),
        timestamp: None,
        valid_from: None,
        valid_until: None,
        metadata: None,
        emit_continuity: false,
    }
}

fn json_search_params(query: &str) -> SearchMemoryParams {
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
        as_of: None,
        include_metadata: false,
        // Explicit JSON so assertions can index rows by id/text instead of
        // parsing the markdown digest (tachi#1201 k3 default).
        format: Some("json".to_string()),
    }
}

async fn search_rows(server: &crate::server_state::MemoryServer, query: &str) -> Vec<Value> {
    let response = server
        .search_memory(Parameters(json_search_params(query)))
        .await
        .expect("search_memory");
    serde_json::from_str(&response).expect("search_memory json rows")
}

// ── T1: cache masking — pre-fix must be RED ────────────────────────────────
//
// Root cause under test: with the recall cache on, a search that already has
// a warm (non-empty) cached entry for an exact query would keep serving that
// stale answer to the very next identical query, even though a brand-new
// save just landed that the fresh (uncached) search would have returned.
// `git stash` the production diff (memcore's `recall_cache_invalidate_all` +
// its `handle_save_memory` call site) and rerun this single test to see it
// fail pre-fix — see the dispatch report for the captured red output.
#[tokio::test]
async fn new_save_is_visible_immediately_even_behind_a_warm_recall_cache() {
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let _flag = EnvRestore::set("TACHI_ENABLE_RECALL_CACHE", "true");

    let server = make_server();
    let needle = format!("RecallCacheInvalNeedle{}", uuid::Uuid::new_v4().simple());
    let query = needle.clone();

    let path_a = format!(
        "/scratch/tachi/recall-cache-inval/{}/a",
        uuid::Uuid::new_v4()
    );
    let save_a = handle_save_memory(
        &server,
        save_params(&path_a, &format!("{needle} first observation about apples")),
    )
    .await
    .expect("save A");
    let save_a_json: Value = serde_json::from_str(&save_a).expect("save A json");
    let a_id = save_a_json["id"].as_str().expect("save A id").to_string();

    // Warm the cache: this exact query, with a non-empty result, gets
    // write-through cached (see `handle_search_memory_with_access`).
    let first = search_rows(&server, &query).await;
    assert!(
        first.iter().any(|row| row["id"] == a_id),
        "seed row A must be visible on the first (cache-warming) search: {first:#?}"
    );
    // codex CONCERN: prove the cache actually engaged (a non-empty row count)
    // rather than the assertions below passing vacuously because the cache
    // was never populated in the first place.
    let warmed_entries = server
        .with_global_store_read(|store| store.recall_cache_stats().map_err(|e| e.to_string()))
        .expect("stats after warming search")
        .entries;
    assert!(
        warmed_entries > 0,
        "the warming search must have actually populated recall_cache, got {warmed_entries} rows"
    );

    // Save B: the very next identical search must see it, not a replay of
    // the just-warmed cache entry from before B existed.
    let path_b = format!(
        "/scratch/tachi/recall-cache-inval/{}/b",
        uuid::Uuid::new_v4()
    );
    let save_b = handle_save_memory(
        &server,
        save_params(
            &path_b,
            &format!("{needle} unrelated note about warehouse logistics scheduling"),
        ),
    )
    .await
    .expect("save B");
    let save_b_json: Value = serde_json::from_str(&save_b).expect("save B json");
    let b_id = save_b_json["id"].as_str().expect("save B id").to_string();

    let second = search_rows(&server, &query).await;
    assert!(
        second.iter().any(|row| row["id"] == b_id),
        "row B saved after the cache was warmed must appear in the very next \
         identical search instead of being masked by the stale cached answer: {second:#?}"
    );
}

// ── T2: handler-level save→search visibility + receipt contract ───────────
#[tokio::test]
async fn save_then_immediate_search_hits_and_receipt_declares_lexical_immediate() {
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let _flag = EnvRestore::set("TACHI_ENABLE_RECALL_CACHE", "true");

    let server = make_server();
    let needle = format!("RecallCacheT2Needle{}", uuid::Uuid::new_v4().simple());
    let path = format!(
        "/scratch/tachi/recall-cache-inval/{}/t2",
        uuid::Uuid::new_v4()
    );

    let saved = handle_save_memory(&server, save_params(&path, &needle))
        .await
        .expect("save");
    let saved_json: Value = serde_json::from_str(&saved).expect("save json");
    assert_eq!(
        saved_json["visibility"]["lexical"], "immediate",
        "{saved_json:#}"
    );
    let id = saved_json["id"].as_str().expect("save id").to_string();

    let rows = search_rows(&server, &needle).await;
    assert!(
        rows.iter().any(|row| row["id"] == id),
        "handler-level save must be immediately lexically searchable: {rows:#?}"
    );
}

// ── T3: rerouted save's read_target matches where the row actually landed ──
#[tokio::test]
async fn save_receipt_read_target_matches_actual_landing_scope() {
    let server = make_server();
    // No named project on this fixture (global-only server, see `make_server`)
    // — target_db resolves to global regardless of the requested scope, and
    // `read_target.scope` must say so rather than echoing the request.
    let path = format!(
        "/scratch/tachi/recall-cache-inval/{}/t3",
        uuid::Uuid::new_v4()
    );
    let mut params = save_params(&path, "read_target contract probe");
    params.scope = "project".to_string();

    let saved = handle_save_memory(&server, params).await.expect("save");
    let saved_json: Value = serde_json::from_str(&saved).expect("save json");

    assert_eq!(
        saved_json["db"], saved_json["read_target"]["scope"],
        "{saved_json:#}"
    );
    assert_eq!(saved_json["read_target"]["scope"], "global", "{saved_json:#}");
    assert!(
        saved_json["read_target"].get("project").is_none(),
        "no named project on this fixture: {saved_json:#}"
    );
    assert_eq!(saved_json["confirm"]["tool"], "get_memory", "{saved_json:#}");
    assert_eq!(saved_json["confirm"]["id"], saved_json["id"], "{saved_json:#}");
}

// ── T4: dedupe short-circuit does not invalidate (cache is preserved) ──────
#[tokio::test]
async fn exact_duplicate_save_does_not_bust_the_recall_cache() {
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let _flag = EnvRestore::set("TACHI_ENABLE_RECALL_CACHE", "true");

    let server = make_server();
    let path = format!(
        "/scratch/tachi/recall-cache-inval/{}/t4",
        uuid::Uuid::new_v4()
    );
    let text = "Exact duplicate dedupe must not clear the recall cache.".to_string();

    let first = handle_save_memory(&server, save_params(&path, &text))
        .await
        .expect("first save");
    let first_json: Value = serde_json::from_str(&first).expect("first json");
    assert!(first_json["status"]
        .as_str()
        .is_some_and(|s| s.starts_with("saved")));

    // Warm a recall-cache row directly (bypassing an actual search call, so
    // this test only depends on the cache table, not on unrelated ranking
    // behavior of a real hybrid search).
    server
        .with_global_store(|store| {
            store
                .recall_cache_store(
                    "rc:t4-probe",
                    "t4 probe query",
                    "[{\"id\":\"seed\"}]",
                    1,
                    false,
                )
                .map_err(|e| e.to_string())
        })
        .expect("seed recall cache row");
    let entries_before = server
        .with_global_store_read(|store| store.recall_cache_stats().map_err(|e| e.to_string()))
        .expect("stats before")
        .entries;
    assert!(
        entries_before >= 1,
        "seed row must be present before the duplicate save"
    );

    // The identical path+text save again (id-less) must hit the dedupe
    // short-circuit — not the upsert path — and therefore must NOT clear the
    // cache row seeded above.
    let second = handle_save_memory(&server, save_params(&path, &text))
        .await
        .expect("second save");
    let second_json: Value = serde_json::from_str(&second).expect("second json");
    assert_eq!(second_json["status"], "duplicate", "{second_json:#}");

    let entries_after = server
        .with_global_store_read(|store| store.recall_cache_stats().map_err(|e| e.to_string()))
        .expect("stats after")
        .entries;
    assert_eq!(
        entries_after, entries_before,
        "dedupe short-circuit must not invalidate the recall cache"
    );
}

// ── T5: successful save's receipt literally says recall_fence=="cleared" ──
#[tokio::test]
async fn successful_save_receipt_recall_fence_is_cleared() {
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let _flag = EnvRestore::set("TACHI_ENABLE_RECALL_CACHE", "true");

    let server = make_server();
    let path = format!(
        "/scratch/tachi/recall-cache-inval/{}/t5",
        uuid::Uuid::new_v4()
    );

    let saved = handle_save_memory(&server, save_params(&path, "recall_fence literal contract probe"))
        .await
        .expect("save");
    let saved_json: Value = serde_json::from_str(&saved).expect("save json");
    assert_eq!(
        saved_json["recall_fence"], "cleared",
        "a successful save with the cache enabled must report recall_fence==\"cleared\" verbatim: {saved_json:#}"
    );
}

// ── T6: a named-project save still clears the GLOBAL recall cache ─────────
// (cross-store face — the cache lives in the global store regardless of
// where write-affinity routes the entry itself).
#[tokio::test]
async fn named_project_save_clears_global_recall_cache() {
    // `make_server_with_temp_home()` returns a `TempHomeGuard` that itself
    // holds `crate::utils::global_test_lock()` for its lifetime (see
    // `TempHomeGuard::new()` — `home_test_lock()` IS that same mutex) — do
    // NOT also acquire it here, that std::sync::Mutex is not reentrant and
    // a second `.lock()` on this thread would deadlock the test.
    let _flag = EnvRestore::set("TACHI_ENABLE_RECALL_CACHE", "true");

    let (server, _temp_home) = make_server_with_temp_home();
    let project_name = format!("t6proj-{}", uuid::Uuid::new_v4().simple());
    let project_db = crate::path_utils::plan_c_global_db_path(&project_name);
    std::fs::create_dir_all(project_db.parent().expect("project db parent"))
        .expect("create named project db dir");
    // Touch a schema-initialized store at the named-project alias path so
    // `resolve_named_project_db_path` finds it.
    memcore::MemoryStore::open(project_db.to_str().expect("utf8 project db path"))
        .expect("open named project db");

    // Seed the GLOBAL recall cache directly — this is what the named-project
    // save below must clear even though the entry itself lands elsewhere.
    server
        .with_global_store(|store| {
            store
                .recall_cache_store(
                    "rc:t6-probe",
                    "t6 probe query",
                    "[{\"id\":\"seed\"}]",
                    1,
                    false,
                )
                .map_err(|e| e.to_string())
        })
        .expect("seed global recall cache row");
    let entries_before = server
        .with_global_store_read(|store| store.recall_cache_stats().map_err(|e| e.to_string()))
        .expect("stats before")
        .entries;
    assert!(
        entries_before >= 1,
        "seed row must exist in the global cache before the named-project save"
    );

    let mut params = save_params(
        "/scratch/t6/named-project-probe",
        "named project save must clear the global cache",
    );
    params.project = Some(project_name.clone());
    params.project_explicit = true;

    let saved = handle_save_memory(&server, params)
        .await
        .expect("save to named project");
    let saved_json: Value = serde_json::from_str(&saved).expect("save json");
    assert_eq!(
        saved_json["read_target"]["project"], project_name,
        "entry must actually land in the named project, not global: {saved_json:#}"
    );
    assert_eq!(saved_json["recall_fence"], "cleared", "{saved_json:#}");

    let entries_after = server
        .with_global_store_read(|store| store.recall_cache_stats().map_err(|e| e.to_string()))
        .expect("stats after")
        .entries;
    assert_eq!(
        entries_after, 0,
        "a named-project save must still clear the GLOBAL recall cache (cross-store invalidation)"
    );
}
