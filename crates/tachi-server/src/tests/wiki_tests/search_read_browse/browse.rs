use super::*;

#[tokio::test]
async fn wiki_browse_includes_related_entries_and_logs_operation() {
    let mut alpha = make_entry("wiki-related-alpha");
    alpha.path = "/wiki/engineering/debugging/alpha".to_string();
    alpha.summary = "Alpha debugging".to_string();
    alpha.text = "Alpha debugging lesson for MCP".to_string();
    alpha.entities = vec!["MCP".to_string()];
    alpha.importance = 0.7;

    let mut beta = make_entry("wiki-related-beta");
    beta.path = "/wiki/engineering/debugging/beta".to_string();
    beta.summary = "Beta debugging".to_string();
    beta.text = "Beta debugging lesson for MCP".to_string();
    beta.entities = vec!["MCP".to_string()];
    beta.importance = 0.9;

    let (server, _home) = seed_wiki_project_entries(vec![alpha, beta]);

    let response = server
        .tachi_browse(Parameters(WikiBrowseParams {
            category: Some("engineering/debugging".to_string()),
            limit: 10,
            project: Some("wiki".to_string()),
            lifecycle: None,
        }))
        .await
        .expect("wiki browse should succeed");
    assert!(
        response.contains("/wiki/engineering/debugging/alpha"),
        "browse markdown should contain alpha path"
    );
    assert!(
        response.contains("/wiki/engineering/debugging/beta"),
        "browse markdown should contain beta path"
    );
    assert!(response.contains("[store: named wiki]"), "{response}");

    let log = server
        .with_named_project_store_read("wiki", |store| {
            store.get("wiki-operation-log").map_err(|e| e.to_string())
        })
        .expect("read wiki log")
        .expect("wiki log should exist");
    assert!(log.text.contains("browse | /wiki/engineering/debugging"));
}

#[tokio::test]
async fn wiki_browse_hides_recall_cache_entries() {
    let mut visible = make_entry("wiki-visible-debugging");
    visible.path = "/wiki/engineering/debugging/visible".to_string();
    visible.summary = "Visible debugging lesson".to_string();
    visible.text = "Visible debugging lesson for wiki browse.".to_string();

    let mut cache = make_entry("wiki-recall-cache-pollution");
    cache.path = "/wiki/engineering/debugging/recall-cache/polluted".to_string();
    cache.summary = "RecallCacheNeedle should stay hidden".to_string();
    cache.text = "RecallCacheNeedle is an ephemeral recall projection.".to_string();
    cache.source = "foundry_recall_rerank_cache".to_string();

    let (server, _home) = seed_wiki_project_entries(vec![visible, cache]);

    let response = server
        .tachi_browse(Parameters(WikiBrowseParams {
            category: Some("engineering/debugging".to_string()),
            limit: 10,
            project: Some("wiki".to_string()),
            lifecycle: None,
        }))
        .await
        .expect("wiki browse should succeed");

    assert!(response.contains("Visible debugging lesson"));
    assert!(!response.contains("RecallCacheNeedle"));
    assert!(!response.contains("recall-cache"));
}

#[tokio::test]
async fn wiki_browse_large_limit_keeps_related_entries_empty() {
    let mut alpha = make_entry("wiki-large-limit-alpha");
    alpha.path = "/wiki/engineering/scale/alpha".to_string();
    alpha.summary = "Alpha scale".to_string();
    alpha.text = "Alpha scale lesson for MCP".to_string();
    alpha.entities = vec!["MCP".to_string()];

    let mut beta = make_entry("wiki-large-limit-beta");
    beta.path = "/wiki/engineering/scale/beta".to_string();
    beta.summary = "Beta scale".to_string();
    beta.text = "Beta scale lesson for MCP".to_string();
    beta.entities = vec!["MCP".to_string()];

    let (server, _home) = seed_wiki_project_entries(vec![alpha, beta]);

    let response = server
        .tachi_browse(Parameters(WikiBrowseParams {
            category: Some("engineering/scale".to_string()),
            limit: 21,
            project: Some("wiki".to_string()),
            lifecycle: None,
        }))
        .await
        .expect("wiki browse should succeed");
    assert!(
        response.contains("/wiki/engineering/scale/alpha"),
        "browse markdown should contain alpha path"
    );
    assert!(
        response.contains("/wiki/engineering/scale/beta"),
        "browse markdown should contain beta path"
    );
}

/// #1072 RED case 1: "Live browse currently reports a partial hard-coded
/// count while the real DB contains more path families; fixture with an
/// unlisted prefix must appear in facets/counts."
#[tokio::test]
async fn wiki_browse_stats_derive_facets_from_real_paths_including_unlisted_prefix() {
    let mut known = make_entry("wiki-facet-known");
    known.path = "/wiki/engineering/architecture/known-entry".to_string();
    known.summary = "Known category entry".to_string();
    known.text = "Known category entry body.".to_string();

    let mut unlisted = make_entry("wiki-facet-unlisted");
    unlisted.path = "/wiki/newteam-prefix/unlisted-entry".to_string();
    unlisted.summary = "Unlisted-prefix category entry".to_string();
    unlisted.text = "Unlisted-prefix category entry body.".to_string();

    let (server, _home) = seed_wiki_project_entries(vec![known, unlisted]);

    let stats = crate::wiki_ops::collect_wiki_browse_value(
        &server,
        WikiBrowseParams {
            category: None,
            limit: 50,
            project: Some("wiki".to_string()),
            lifecycle: None,
        },
    )
    .expect("browse stats should succeed");
    assert_eq!(stats["kind"], json!("stats"));
    let categories = stats["categories"]
        .as_array()
        .expect("categories array")
        .iter()
        .map(|row| row["path"].as_str().unwrap_or_default().to_string())
        .collect::<Vec<_>>();
    assert!(
        categories.contains(&"/wiki/engineering/architecture".to_string()),
        "RED-safety: known category must still be derived: {categories:?}"
    );
    assert!(
        categories.contains(&"/wiki/newteam-prefix".to_string()),
        "RED: unlisted prefix must appear in derived facets/counts: {categories:?}"
    );
}
