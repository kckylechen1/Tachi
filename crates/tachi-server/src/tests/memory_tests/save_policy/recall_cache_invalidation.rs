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
use crate::facade_memory_ops::consolidate_ops::merge_into_for_project;
use crate::memory_ops::{handle_archive_memory, handle_delete_memory, handle_memory_gc};
use crate::memory_search_ops::handle_save_memory;
use crate::test_support::EnvRestore;
use crate::tool_params::{ArchiveMemoryParams, DeleteMemoryParams};
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
    search_rows_with_params(server, json_search_params(query)).await
}

async fn search_rows_with_params(
    server: &crate::server_state::MemoryServer,
    params: SearchMemoryParams,
) -> Vec<Value> {
    let response = server
        .search_memory(Parameters(params))
        .await
        .expect("search_memory");
    serde_json::from_str(&response).expect("search_memory json rows")
}

async fn wait_for_cache_hit(server: &crate::server_state::MemoryServer, previous_hits: i64) {
    for _ in 0..50 {
        let hits = server
            .with_global_store_read(|store| {
                store
                    .recall_cache_stats()
                    .map_err(|error| error.to_string())
            })
            .expect("cache stats")
            .total_hits;
        if hits > previous_hits {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("unchanged search did not record a recall-cache hit");
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
#[allow(clippy::await_holding_lock)]
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
#[allow(clippy::await_holding_lock)]
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
    assert_eq!(
        saved_json["read_target"]["scope"], "global",
        "{saved_json:#}"
    );
    assert!(
        saved_json["read_target"].get("project").is_none(),
        "no named project on this fixture: {saved_json:#}"
    );
    assert_eq!(
        saved_json["confirm"]["tool"], "get_memory",
        "{saved_json:#}"
    );
    assert_eq!(
        saved_json["confirm"]["id"], saved_json["id"],
        "{saved_json:#}"
    );
}

// ── T4: dedupe short-circuit does not invalidate (cache is preserved) ──────
#[tokio::test]
#[allow(clippy::await_holding_lock)]
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
                    "test-generation",
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
#[allow(clippy::await_holding_lock)]
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

    let saved = handle_save_memory(
        &server,
        save_params(&path, "recall_fence literal contract probe"),
    )
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
                    "test-generation",
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

// ── #1413 concern 1: invalidation after non-save content mutations ─────────
//
// `handle_save_memory` (T1–T6 above) already busts the shared recall cache.
// These tests cover the OTHER content-changing paths cited in #1413 concern 1
// — delete, archive, gc, and a consolidate lifecycle action — each of which
// must likewise clear the GLOBAL recall cache only AFTER its store commit
// returned, never from inside an active store closure (the invalidator
// re-enters `with_global_store`, which would recurse on / nest the
// non-reentrant `global_rw_gate`). Every test follows the same discriminating
// shape: seed a cache row directly, run the mutation, assert the cache is now
// empty. Pre-fix (no invalidation call in the cited path) the seeded row
// survives, so `entries == 0` is RED; post-fix it is GREEN.

fn seed_global_recall_cache_row(server: &crate::server_state::MemoryServer, cache_id: &str) {
    server
        .with_global_store(|store| {
            store
                .recall_cache_store(
                    cache_id,
                    "test-generation",
                    "1413 seed query",
                    "[{\"id\":\"seed\"}]",
                    1,
                    false,
                )
                .map_err(|e| e.to_string())
        })
        .expect("seed global recall cache row");
}

fn global_recall_cache_entries(server: &crate::server_state::MemoryServer) -> i64 {
    server
        .with_global_store_read(|store| store.recall_cache_stats().map_err(|e| e.to_string()))
        .expect("global recall cache stats")
        .entries
}

async fn save_and_get_id(
    server: &crate::server_state::MemoryServer,
    path: &str,
    text: &str,
) -> String {
    let saved = handle_save_memory(server, save_params(path, text))
        .await
        .expect("save");
    let saved_json: Value = serde_json::from_str(&saved).expect("save json");
    saved_json["id"].as_str().expect("save id").to_string()
}

// Like `save_and_get_id` but stamps `retention_policy = "ephemeral"`. Needed
// for lifecycle-merge tests: `is_protected` (consolidate_ops) shields any row
// whose retention is durable/permanent/pinned from supersede/merge/archive,
// and `save_params` defaults retention to `durable` (None →
// `resolve_save_retention_policy` → durable), so a default-saved fixture is
// refused by `refuse_if_protected`. Ephemeral rows are genuinely non-protected
// (same retention `/ghost/run` rows get), so this exercises the REAL merge
// path without weakening production protection (durable/permanent/pinned stay
// fully shielded).
async fn save_ephemeral_and_get_id(
    server: &crate::server_state::MemoryServer,
    path: &str,
    text: &str,
) -> String {
    let mut params = save_params(path, text);
    params.retention_policy = Some("ephemeral".to_string());
    let saved = handle_save_memory(server, params).await.expect("save");
    let saved_json: Value = serde_json::from_str(&saved).expect("save json");
    saved_json["id"].as_str().expect("save id").to_string()
}

// ── D1: delete clears the recall cache after the store commit ──────────────
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn delete_memory_clears_recall_cache_after_commit() {
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let _flag = EnvRestore::set("TACHI_ENABLE_RECALL_CACHE", "true");

    let server = make_server();
    let path = format!("/scratch/tachi/1413-delete/{}", uuid::Uuid::new_v4());
    let id = save_and_get_id(&server, &path, "1413 delete inval probe apples").await;

    seed_global_recall_cache_row(&server, "rc:1413-d1");
    assert!(
        global_recall_cache_entries(&server) >= 1,
        "seed row must be present before delete"
    );

    let raw = handle_delete_memory(
        &server,
        DeleteMemoryParams {
            id: id.clone(),
            project: None,
        },
    )
    .await
    .expect("delete");
    let deleted_json: Value = serde_json::from_str(&raw).expect("delete json");
    assert!(
        deleted_json["deleted"].as_bool().unwrap_or(false),
        "delete must actually remove the row (else the cache assert is vacuous): {deleted_json:#}"
    );

    assert_eq!(
        global_recall_cache_entries(&server),
        0,
        "a successful delete must clear the recall cache after its store commit"
    );
}

// ── D2: archive clears the recall cache after the store commit ─────────────
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn archive_memory_clears_recall_cache_after_commit() {
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let _flag = EnvRestore::set("TACHI_ENABLE_RECALL_CACHE", "true");

    let server = make_server();
    let path = format!("/scratch/tachi/1413-archive/{}", uuid::Uuid::new_v4());
    let id = save_and_get_id(&server, &path, "1413 archive inval probe bananas").await;

    seed_global_recall_cache_row(&server, "rc:1413-d2");
    assert!(
        global_recall_cache_entries(&server) >= 1,
        "seed row must be present before archive"
    );

    let raw = handle_archive_memory(
        &server,
        ArchiveMemoryParams {
            id: id.clone(),
            project: None,
        },
    )
    .await
    .expect("archive");
    let archived_json: Value = serde_json::from_str(&raw).expect("archive json");
    assert!(
        archived_json["archived"].as_bool().unwrap_or(false),
        "archive must actually archive the row (else the cache assert is vacuous): {archived_json:#}"
    );

    assert_eq!(
        global_recall_cache_entries(&server),
        0,
        "a successful archive must clear the recall cache after its store commit"
    );
}

// ── D3: gc clears the recall cache after the batch commits ─────────────────
//
// `gc_common_store` / `gc_expired_kanban_cards` run inside the gc closures and
// only borrow a `&mut MemoryStore`, so the bust MUST be the caller's job after
// those closures return — this test proves it lands there (pre-fix there is no
// invalidation in `handle_memory_gc`, so the seeded row survives → RED).
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn memory_gc_clears_recall_cache_after_commit() {
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let _flag = EnvRestore::set("TACHI_ENABLE_RECALL_CACHE", "true");

    let server = make_server();
    // Seed content so gc sweeps a populated store (whether it reclaims this row
    // is irrelevant to the cache assertion below).
    let path = format!("/scratch/tachi/1413-gc/{}", uuid::Uuid::new_v4());
    let _ = save_and_get_id(&server, &path, "1413 gc inval probe cherries").await;

    seed_global_recall_cache_row(&server, "rc:1413-d3");
    assert!(
        global_recall_cache_entries(&server) >= 1,
        "seed row must be present before gc"
    );

    handle_memory_gc(&server).await.expect("gc");

    assert_eq!(
        global_recall_cache_entries(&server),
        0,
        "gc must clear the recall cache after its batch store commits"
    );
}

// ── D4: a consolidate lifecycle action (merge_into) clears the cache ───────
// Drives `apply_lifecycle_action` via its automated `merge_into_for_project`
// entry. NOT the same code path as the human-reviewed propose/review/apply
// loop: `handle_apply` delegates to
// `memcore::store::memory_lifecycle::apply_lifecycle_proposal` and runs its own
// post-commit invalidation, so `apply_lifecycle_action` now has exactly two
// callers, both automated (`merge_into_for_project` and, through it,
// `foundry_runtime_ops::daily_distill::consolidate_prepass`). What this test
// covers is that automated arm's real mutation path — the reviewed arm's
// invalidation is a separate obligation and needs its own coverage. The
// invalidation must fire AFTER the `with_memory_store` closure returns.
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn consolidate_lifecycle_clears_recall_cache_after_commit() {
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let _flag = EnvRestore::set("TACHI_ENABLE_RECALL_CACHE", "true");

    let server = make_server();
    // Disjoint word sets beyond the shared needle so write-time near-dup
    // merge (Jaccard) does not collapse A and B into one row before this test
    // gets to merge them explicitly. Both rows are saved `ephemeral` so they
    // are NOT protected (durable/permanent/pinned are shielded from merge by
    // `is_protected`); see `save_ephemeral_and_get_id`.
    let needle = format!("ConsolidateMergeNeedle{}", uuid::Uuid::new_v4().simple());
    let path_a = format!("/scratch/tachi/1413-merge/{}/a", uuid::Uuid::new_v4());
    let path_b = format!("/scratch/tachi/1413-merge/{}/b", uuid::Uuid::new_v4());
    let id_a =
        save_ephemeral_and_get_id(&server, &path_a, &format!("{needle} alpha source row")).await;
    let id_b =
        save_ephemeral_and_get_id(&server, &path_b, &format!("{needle} bravo survivor row")).await;

    seed_global_recall_cache_row(&server, "rc:1413-d4");
    assert!(
        global_recall_cache_entries(&server) >= 1,
        "seed row must be present before merge"
    );

    // Merge source (A) into survivor (B): fold keywords/entities, supersede +
    // archive A. `merge_into_for_project` is the only non-human-reviewed entry
    // to the lifecycle mutation choke point.
    let merged = merge_into_for_project(&server, None, &id_a, &id_b).expect("merge_into");
    assert_eq!(
        merged["lifecycle_action"].as_str(),
        Some("merge_into"),
        "merge must report its action: {merged:#}"
    );

    assert_eq!(
        global_recall_cache_entries(&server),
        0,
        "a consolidate lifecycle action must clear the recall cache after its store commit"
    );
}

// The cache key must derive freshness from SQLite itself, not from the
// writer-facing invalidation calls above. This models two server processes:
// B warms the cache, then A performs a raw memcore upsert that never calls a
// tachi-server cache helper. B's next identical read must observe the changed
// database generation and compute fresh rows.
#[tokio::test]
async fn raw_memcore_write_in_one_server_invalidates_another_servers_warm_cache() {
    let (writer, _temp_home) = make_server_with_temp_home();
    let _flag = EnvRestore::set("TACHI_ENABLE_RECALL_CACHE", "true");
    let reader = crate::server_state::MemoryServer::new(writer.global_db_path_buf(), None)
        .expect("independent reader server");
    let needle = format!(
        "CrossProcessGenerationNeedle{}",
        uuid::Uuid::new_v4().simple()
    );
    let first = handle_save_memory(
        &writer,
        save_params(
            "/scratch/cache-generation/first",
            &format!("{needle} first cacheable memory"),
        ),
    )
    .await
    .expect("seed first memory");
    let first_id = serde_json::from_str::<Value>(&first).expect("seed json")["id"]
        .as_str()
        .expect("seed id")
        .to_string();

    let warmed = search_rows(&reader, &needle).await;
    assert!(
        warmed.iter().any(|row| row["id"] == first_id),
        "reader must see the first row while warming the cache: {warmed:#?}"
    );
    let cache_entries_before_raw_write = global_recall_cache_entries(&reader);
    assert!(
        cache_entries_before_raw_write > 0,
        "the reader search must populate the shared recall cache before the raw write"
    );
    let hits_before_unchanged = reader
        .with_global_store_read(|store| {
            store
                .recall_cache_stats()
                .map_err(|error| error.to_string())
        })
        .expect("cache stats before unchanged query")
        .total_hits;
    let unchanged = search_rows(&reader, &needle).await;
    assert!(
        unchanged.iter().any(|row| row["id"] == first_id),
        "the unchanged database must keep serving its warm cache entry: {unchanged:#?}"
    );
    wait_for_cache_hit(&reader, hits_before_unchanged).await;

    let mut raw_entry = writer
        .with_global_store_read(|store| {
            store
                .get(&first_id)
                .map_err(|error| error.to_string())?
                .ok_or_else(|| "seed memory disappeared".to_string())
        })
        .expect("load raw entry template");
    let second_id = format!("raw-generation-{}", uuid::Uuid::new_v4());
    raw_entry.id = second_id.clone();
    raw_entry.path = "/scratch/cache-generation/raw-memcore".to_string();
    raw_entry.text = format!("{needle} second row written by raw memcore upsert");
    raw_entry.summary = raw_entry.text.clone();
    writer
        .with_global_store(|store| store.upsert(&raw_entry).map_err(|error| error.to_string()))
        .expect("raw memcore upsert in independent writer");

    assert_eq!(
        global_recall_cache_entries(&reader),
        cache_entries_before_raw_write,
        "this path must prove SQLite generation validation, not a manual cache-table delete"
    );
    let refreshed = search_rows(&reader, &needle).await;
    assert!(
        refreshed.iter().any(|row| row["id"] == second_id),
        "reader B must not replay its warm pre-write cache after raw writer A committed: {refreshed:#?}"
    );
}

// A missing trigger means the generation can no longer prove freshness. The
// cache stays physically populated, but the reader must bypass it and search
// rather than presenting a clean-looking stale hit.
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn missing_generation_trigger_bypasses_a_warm_cache_instead_of_serving_stale_rows() {
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let _flag = EnvRestore::set("TACHI_ENABLE_RECALL_CACHE", "true");
    let server = make_server();
    let needle = format!("GenerationDriftNeedle{}", uuid::Uuid::new_v4().simple());
    let first = handle_save_memory(
        &server,
        save_params(
            "/scratch/cache-generation/drift-first",
            &format!("{needle} first cacheable memory"),
        ),
    )
    .await
    .expect("seed first memory");
    let first_id = serde_json::from_str::<Value>(&first).expect("seed json")["id"]
        .as_str()
        .expect("seed id")
        .to_string();
    assert!(
        search_rows(&server, &needle)
            .await
            .iter()
            .any(|row| row["id"] == first_id),
        "first search must warm a non-empty cache"
    );
    let cache_entries_before_drift = global_recall_cache_entries(&server);
    assert!(cache_entries_before_drift > 0, "warm cache must exist");

    let second_id = format!("drift-generation-{}", uuid::Uuid::new_v4());
    let mut raw_entry = server
        .with_global_store_read(|store| {
            store
                .get(&first_id)
                .map_err(|error| error.to_string())?
                .ok_or_else(|| "seed memory disappeared".to_string())
        })
        .expect("load raw entry template");
    raw_entry.id = second_id.clone();
    raw_entry.path = "/scratch/cache-generation/drift-second".to_string();
    raw_entry.text = format!("{needle} second row after trigger drift");
    raw_entry.summary = raw_entry.text.clone();
    server
        .with_global_store(|store| {
            store
                .connection()
                .execute_batch("DROP TRIGGER memory_search_generation_after_insert")
                .map_err(|error| error.to_string())?;
            store
                .upsert(&raw_entry)
                .map_err(|error| error.to_string())?;
            Ok(())
        })
        .expect("simulate trigger drift plus out-of-band insert");

    assert_eq!(
        global_recall_cache_entries(&server),
        cache_entries_before_drift,
        "trigger drift must not rely on an unrelated cache eviction to look fresh"
    );
    let refreshed = search_rows(&server, &needle).await;
    assert!(
        refreshed.iter().any(|row| row["id"] == second_id),
        "an invalid generation contract must bypass the warm cache, not hide the new row: {refreshed:#?}"
    );
}

#[tokio::test]
async fn graph_edge_write_in_one_server_invalidates_another_servers_warm_expansion() {
    let (writer, _temp_home) = make_server_with_temp_home();
    let _flag = EnvRestore::set("TACHI_ENABLE_RECALL_CACHE", "true");
    let reader = crate::server_state::MemoryServer::new(writer.global_db_path_buf(), None)
        .expect("independent reader server");
    let needle = format!("GraphGenerationNeedle{}", uuid::Uuid::new_v4().simple());
    let source = handle_save_memory(
        &writer,
        save_params(
            "/scratch/cache-generation/graph-source",
            &format!("{needle} graph expansion source"),
        ),
    )
    .await
    .expect("save graph source");
    let source_id = serde_json::from_str::<Value>(&source).expect("source json")["id"]
        .as_str()
        .expect("source id")
        .to_string();
    let target = handle_save_memory(
        &writer,
        save_params(
            "/scratch/cache-generation/graph-target",
            "Graph target only reachable through a newly committed edge",
        ),
    )
    .await
    .expect("save graph target");
    let target_id = serde_json::from_str::<Value>(&target).expect("target json")["id"]
        .as_str()
        .expect("target id")
        .to_string();

    let mut graph_search = json_search_params(&needle);
    graph_search.graph_expand_hops = 1;
    let warmed = search_rows_with_params(&reader, graph_search.clone()).await;
    assert!(warmed.iter().any(|row| row["id"] == source_id));
    assert!(!warmed.iter().any(|row| row["id"] == target_id));
    let cache_entries = global_recall_cache_entries(&reader);
    assert!(cache_entries > 0, "graph search must warm the cache");

    writer
        .with_global_store(|store| {
            store
                .add_edge(&memcore::MemoryEdge {
                    source_id: source_id.clone(),
                    target_id: target_id.clone(),
                    relation: "references".to_string(),
                    weight: 1.0,
                    metadata: serde_json::json!({}),
                    created_at: chrono::Utc::now().to_rfc3339(),
                    valid_from: String::new(),
                    valid_to: None,
                })
                .map_err(|error| error.to_string())
        })
        .expect("raw graph edge write");

    assert_eq!(
        global_recall_cache_entries(&reader),
        cache_entries,
        "edge trigger proof must not rely on manual cache eviction"
    );
    let refreshed = search_rows_with_params(&reader, graph_search).await;
    assert!(
        refreshed.iter().any(|row| row["id"] == target_id),
        "graph generation change must expose the newly reachable row: {refreshed:#?}"
    );
}

#[tokio::test]
async fn fts_backfill_in_one_server_invalidates_another_servers_warm_lexical_cache() {
    let (writer, _temp_home) = make_server_with_temp_home();
    let _flag = EnvRestore::set("TACHI_ENABLE_RECALL_CACHE", "true");
    let reader = crate::server_state::MemoryServer::new(writer.global_db_path_buf(), None)
        .expect("independent reader server");
    let needle = format!("FtsRepairGenerationNeedle{}", uuid::Uuid::new_v4().simple());
    let first = handle_save_memory(
        &writer,
        save_params(
            "/scratch/cache-generation/fts-first",
            &format!("{needle} indexed row"),
        ),
    )
    .await
    .expect("save indexed row");
    let first_id = serde_json::from_str::<Value>(&first).expect("first json")["id"]
        .as_str()
        .expect("first id")
        .to_string();
    let mut missing_projection = writer
        .with_global_store_read(|store| {
            store
                .get(&first_id)
                .map_err(|error| error.to_string())?
                .ok_or_else(|| "indexed row disappeared".to_string())
        })
        .expect("load row template");
    let repaired_id = format!("fts-repair-{}", uuid::Uuid::new_v4());
    missing_projection.id = repaired_id.clone();
    missing_projection.path = "/scratch/cache-generation/fts-repaired".to_string();
    missing_projection.text = format!("{needle} row absent from both FTS projections");
    missing_projection.summary = missing_projection.text.clone();
    writer
        .with_global_store(|store| {
            store
                .upsert(&missing_projection)
                .map_err(|error| error.to_string())?;
            store
                .connection()
                .execute("DELETE FROM memories_fts WHERE id = ?1", [&repaired_id])
                .map_err(|error| error.to_string())?;
            store
                .connection()
                .execute(
                    "DELETE FROM memories_symbolic_fts WHERE id = ?1",
                    [&repaired_id],
                )
                .map_err(|error| error.to_string())?;
            Ok(())
        })
        .expect("seed missing projection row");

    let warmed = search_rows(&reader, &needle).await;
    assert!(warmed.iter().any(|row| row["id"] == first_id));
    assert!(!warmed.iter().any(|row| row["id"] == repaired_id));
    let cache_entries = global_recall_cache_entries(&reader);
    assert!(cache_entries > 0, "lexical search must warm the cache");

    let inserted = writer
        .with_global_store(|store| {
            store
                .backfill_fts_missing()
                .map_err(|error| error.to_string())
        })
        .expect("production FTS backfill");
    assert!(inserted > 0, "repair must mutate the FTS projection");
    assert_eq!(
        global_recall_cache_entries(&reader),
        cache_entries,
        "FTS generation proof must not rely on manual cache eviction"
    );

    let refreshed = search_rows(&reader, &needle).await;
    assert!(
        refreshed.iter().any(|row| row["id"] == repaired_id),
        "FTS-only repair must invalidate the warm cross-store result: {refreshed:#?}"
    );
}
