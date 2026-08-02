//! tachi#1561 read-surface leak stopgaps (L1/L2/L3/L7).
//!
//! `search` has always filtered internal bookkeeping rows through
//! `memcore::is_namespace_search_noise`; the id-addressed and prefix-addressed
//! read surfaces did not. These tests are the discriminator: each one fails on
//! the pre-fix code, where `get`/`list_memories`/`sync_memories` returned the
//! internal row's full body verbatim.
//!
//! Every test asserts **both** directions — the internal row is gone *and* an
//! ordinary row under the same prefix still comes back — so a filter that
//! simply empties the surface cannot pass.

use super::*;

/// A recall-cache row: internal by `is_recall_cache_entry` on four independent
/// signals (id prefix, path, topic, source + `cache_key` metadata), and proven
/// writable through `store.upsert` by the existing
/// `search_facade::vector_boundaries` fixtures.
fn internal_cache_entry(id: &str, path: &str) -> memcore::MemoryEntry {
    let mut entry = make_entry(id);
    entry.path = path.to_string();
    entry.text = "internal recall-cache body that must never reach a reader".to_string();
    entry.summary = "internal recall-cache row".to_string();
    entry.topic = "recall_rerank_cache".to_string();
    entry.source = memcore::FOUNDRY_RECALL_CACHE_SOURCE.to_string();
    entry.metadata = json!({ "cache_key": memcore::FOUNDRY_RECALL_CACHE_SOURCE });
    entry
}

fn ordinary_entry(id: &str, path: &str) -> memcore::MemoryEntry {
    let mut entry = make_entry(id);
    entry.path = path.to_string();
    entry.text = "ordinary user-facing body".to_string();
    entry.summary = "ordinary row".to_string();
    entry
}

/// A reserved Wiki REM operation draft: internal by `is_reserved_wiki_rem_id`
/// on the `wiki-rem:` id prefix alone.
fn wiki_rem_entry(id: &str) -> memcore::MemoryEntry {
    let mut entry = make_entry(id);
    entry.path = "/wiki/leak/rem-draft".to_string();
    entry.text = "internal wiki-rem draft body that must never reach a reader".to_string();
    entry.summary = "internal wiki-rem row".to_string();
    entry
}

/// A Wiki operation-log row: internal by `is_wiki_log_entry` on the
/// `/wiki/_log/...` path.
fn wiki_log_entry(id: &str) -> memcore::MemoryEntry {
    let mut entry = make_entry(id);
    entry.path = "/wiki/_log/leak".to_string();
    entry.text = "internal wiki log body that must never reach a reader".to_string();
    entry.summary = "internal wiki log row".to_string();
    entry
}

/// An anchor plumbing row: internal by `is_anchor_entry` on the `anchor:` id
/// prefix.
fn anchor_entry(id: &str) -> memcore::MemoryEntry {
    let mut entry = make_entry(id);
    entry.path = "/anchors/issue/leak".to_string();
    entry.text = "internal anchor body that must never reach a reader".to_string();
    entry.summary = "internal anchor row".to_string();
    entry
}

/// A kanban card: the owner's own board content, not internal bookkeeping —
/// `is_namespace_search_noise` drops it from unaddressed listing/search
/// unless the query opts in with a `/kanban` prefix, but `get(id)` must still
/// return it verbatim to a caller who already holds the id.
fn kanban_entry(id: &str) -> memcore::MemoryEntry {
    let mut entry = make_entry(id);
    entry.path = "/kanban/leak-card".to_string();
    entry.text = "kanban card body the owner must still be able to fetch by id".to_string();
    entry.summary = "kanban row".to_string();
    entry
}

fn seed(server: &crate::tests::TestServer, entries: Vec<memcore::MemoryEntry>) {
    server
        .with_global_store(|store| {
            for entry in &entries {
                store.upsert(entry).map_err(|e| e.to_string())?;
            }
            Ok::<(), String>(())
        })
        .expect("seed read-surface leak fixtures");
}

/// Shared assertion for the internal-bookkeeping classes `get` must still
/// withhold by id.
async fn assert_get_withholds(server: &crate::tests::TestServer, id: &str, label: &str) {
    let response = crate::memory_ops::handle_get_memory(
        server,
        GetMemoryParams {
            id: id.to_string(),
            project: None,
            include_archived: false,
        },
    )
    .await
    .unwrap_or_else(|e| panic!("get of {label} must succeed as a miss, not error: {e}"));
    let response: Value = serde_json::from_str(&response).expect("get JSON");
    assert_eq!(
        response["error"], "Memory not found",
        "{label} must be indistinguishable from an absent row: {response}"
    );
    assert!(
        !response.to_string().contains("must never reach a reader"),
        "{label} body leaked through get: {response}"
    );
}

/// L1/L3: `get` is id-addressed and had no namespace filter at all — knowing
/// the id was enough to read an internal row's full body.
///
/// wave2 follow-up: the original fix reused `is_namespace_search_noise`
/// wholesale, which also withheld kanban/handoff/continuity-projection rows
/// from `get` — the owner's own content, not internal bookkeeping, and with
/// no `path_prefix` opt-in available on an id-addressed read. `get` now
/// filters on the narrower `is_internal_only_row` instead, so this test
/// asserts all three outcomes: every internal-bookkeeping class is still
/// withheld by id, a kanban row (the class that motivated the narrowing) is
/// still fetchable by id, and an ordinary wiki row is unaffected.
#[tokio::test]
async fn get_memory_withholds_internal_rows_but_still_serves_wiki_rows() {
    let server = make_server();
    seed(
        &server,
        vec![
            internal_cache_entry(
                "foundry:recall-cache:get-leak",
                "/scratch/leak/recall-cache/get",
            ),
            wiki_rem_entry("wiki-rem:get-leak"),
            wiki_log_entry("wiki-log-get-leak"),
            anchor_entry("anchor:get-leak"),
            ordinary_entry("wiki-user-facing-get", "/wiki/leak/user-facing"),
            kanban_entry("kanban-get-leak"),
        ],
    );

    // The four internal-bookkeeping classes stay withheld by id.
    assert_get_withholds(&server, "foundry:recall-cache:get-leak", "recall-cache row").await;
    assert_get_withholds(&server, "wiki-rem:get-leak", "wiki-rem draft").await;
    assert_get_withholds(&server, "wiki-log-get-leak", "wiki log row").await;
    assert_get_withholds(&server, "anchor:get-leak", "anchor row").await;

    // Ordinary wiki row: unaffected by either predicate.
    let ordinary = crate::memory_ops::handle_get_memory(
        &server,
        GetMemoryParams {
            id: "wiki-user-facing-get".to_string(),
            project: None,
            include_archived: false,
        },
    )
    .await
    .expect("get of an ordinary wiki row");
    let ordinary: Value = serde_json::from_str(&ordinary).expect("get JSON");
    assert_eq!(
        ordinary["id"], "wiki-user-facing-get",
        "ordinary wiki rows must stay readable by id: {ordinary}"
    );

    // Kanban row: the class that motivated narrowing `readable_entry` off
    // `is_namespace_search_noise` — the owner's own content must remain
    // fetchable by id even though it has no `path_prefix` to opt in with,
    // and even though it is excluded from unaddressed listing/search.
    let kanban = crate::memory_ops::handle_get_memory(
        &server,
        GetMemoryParams {
            id: "kanban-get-leak".to_string(),
            project: None,
            include_archived: false,
        },
    )
    .await
    .expect("get of a kanban row by id");
    let kanban: Value = serde_json::from_str(&kanban).expect("get JSON");
    assert_eq!(
        kanban["id"], "kanban-get-leak",
        "kanban rows must stay readable by id even though they are namespace \
         search noise on unaddressed surfaces: {kanban}"
    );
}

/// L2: `list_memories` returned raw `list_by_path` rows. The explicit
/// `path_prefix` opt-in (`memcore::path_prefix_opts_into_recall_cache`) must
/// survive the fix — scoped browsing of the cache namespace still works.
#[tokio::test]
async fn list_memories_drops_internal_rows_unless_the_prefix_opts_in() {
    let server = make_server();
    seed(
        &server,
        vec![
            internal_cache_entry(
                "foundry:recall-cache:list-leak",
                "/scratch/leak/recall-cache/list",
            ),
            ordinary_entry("list-ordinary", "/scratch/leak/ordinary"),
        ],
    );

    let unscoped = crate::memory_ops::handle_list_memories(
        &server,
        ListMemoriesParams {
            path_prefix: "/scratch/leak".to_string(),
            limit: 50,
            include_archived: false,
            project: None,
        },
    )
    .await
    .expect("list under the shared prefix");
    let unscoped: Vec<Value> = serde_json::from_str(&unscoped).expect("list JSON");
    let ids = unscoped
        .iter()
        .map(|row| row["id"].as_str().unwrap_or_default().to_string())
        .collect::<Vec<_>>();
    assert!(
        ids.iter().any(|id| id == "list-ordinary"),
        "ordinary row must still list: {unscoped:#?}"
    );
    assert!(
        !ids.iter().any(|id| id == "foundry:recall-cache:list-leak"),
        "internal row leaked through list_memories: {unscoped:#?}"
    );

    let opted_in = crate::memory_ops::handle_list_memories(
        &server,
        ListMemoriesParams {
            path_prefix: "/scratch/leak/recall-cache".to_string(),
            limit: 50,
            include_archived: false,
            project: None,
        },
    )
    .await
    .expect("list scoped into the recall-cache namespace");
    let opted_in: Vec<Value> = serde_json::from_str(&opted_in).expect("list JSON");
    assert!(
        opted_in
            .iter()
            .any(|row| row["id"] == json!("foundry:recall-cache:list-leak")),
        "explicit recall-cache scope must keep opting in: {opted_in:#?}"
    );
}

/// L7: `sync_memories` defaults to `path_prefix = "/"`, so a bare sync shipped
/// every internal row in the store to the calling agent.
#[tokio::test]
async fn sync_memories_does_not_ship_internal_rows_to_agents() {
    let server = make_server();
    seed(
        &server,
        vec![
            internal_cache_entry(
                "foundry:recall-cache:sync-leak",
                "/scratch/leak/recall-cache/sync",
            ),
            ordinary_entry("sync-ordinary", "/scratch/leak/sync-ordinary"),
        ],
    );

    let response = server
        .sync_memories(Parameters(SyncMemoriesParams {
            agent_id: "agent-1561-leak".to_string(),
            path_prefix: Some("/".to_string()),
            limit: 100,
        }))
        .await
        .expect("sync should succeed");
    let response: Value = serde_json::from_str(&response).expect("sync JSON");
    let entries = response["entries"]
        .as_array()
        .cloned()
        .expect("sync entries array");
    assert!(
        entries
            .iter()
            .any(|row| row["id"] == json!("sync-ordinary")),
        "ordinary row must still sync: {response}"
    );
    assert!(
        !entries
            .iter()
            .any(|row| row["id"] == json!("foundry:recall-cache:sync-leak")),
        "internal row leaked through sync_memories: {response}"
    );
}
