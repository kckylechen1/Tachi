use super::*;

fn vector_health_entry(id: &str, source: &str, vector: Option<Vec<f32>>) -> MemoryEntry {
    MemoryEntry {
        id: id.to_string(),
        path: format!("/scratch/status/{id}"),
        summary: "summary".to_string(),
        text: "status vector health test memory".to_string(),
        importance: 0.7,
        timestamp: Utc::now().to_rfc3339(),
        valid_from: String::new(),
        valid_until: None,
        category: "fact".to_string(),
        topic: "status".to_string(),
        keywords: Vec::new(),
        persons: Vec::new(),
        entities: Vec::new(),
        location: String::new(),
        source: source.to_string(),
        scope: "general".to_string(),
        archived: false,
        access_count: 0,
        last_access: None,
        last_use_at: None,
        revision: 1,
        metadata: json!({}),
        vector,
        retention_policy: None,
        domain: None,
        recall_count: 0,
        query_diversity: 0,
        tier: "raw".to_string(),
    }
}

#[test]
fn vector_health_excludes_recall_cache_rows_from_coverage() {
    let dir = tempfile::tempdir().expect("temp db dir");
    let db = dir.path().join("memory.db");
    let mut store = MemoryStore::open(db.to_str().expect("db path")).expect("open store");

    store
        .upsert(&vector_health_entry(
            "normal-with-vector",
            "manual",
            Some(vec![0.1; EXPECTED_EMBEDDING_DIM]),
        ))
        .expect("insert vector row");
    store
        .upsert(&vector_health_entry("normal-missing", "manual", None))
        .expect("insert missing row");
    store
        .upsert(&vector_health_entry(
            "cache-missing",
            FOUNDRY_RECALL_CACHE_SOURCE,
            None,
        ))
        .expect("insert cache row");
    let mut cache_by_path = vector_health_entry("cache-by-path", "manual", None);
    cache_by_path.path = "/scratch/recall-cache/vector-health".to_string();
    cache_by_path.topic = "recall_rerank_cache".to_string();
    store.upsert(&cache_by_path).expect("insert path cache row");

    let health = vector_health(store.connection()).expect("vector health");
    assert_eq!(health.total, 2);
    assert_eq!(health.with_vec, 1);
    assert_eq!(health.missing, 1);
    assert_eq!(health.pending_enrichment, 1);
}

#[test]
fn collect_snapshot_surfaces_plan_c_split_brain_warning() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let saved = std::env::var_os("TACHI_HOME");
    let dir = tempfile::tempdir().expect("temp db dir");
    let app_home = dir.path().join("home");
    std::env::set_var("TACHI_HOME", &app_home);

    let global_db = app_home.join("global/memory.db");
    std::fs::create_dir_all(global_db.parent().expect("global parent"))
        .expect("create global parent");
    MemoryStore::open(global_db.to_str().expect("global path")).expect("open global");

    let repo = dir.path().join("Split Brain Repo");
    let local_db = repo.join(".tachi/memory.db");
    std::fs::create_dir_all(local_db.parent().expect("local parent")).expect("create local parent");
    MemoryStore::open(local_db.to_str().expect("local path")).expect("open local");

    let alias_db = crate::path_utils::plan_c_global_db_path("Split_Brain_Repo");
    std::fs::create_dir_all(alias_db.parent().expect("alias parent")).expect("create alias parent");
    MemoryStore::open(alias_db.to_str().expect("alias path")).expect("open alias");

    let snapshot = collect_snapshot(&app_home, &global_db, Some(&local_db));
    assert_eq!(snapshot.plan_c_split_brain.len(), 1);
    assert_eq!(
        snapshot.plan_c_split_brain[0].project_name,
        "Split_Brain_Repo"
    );

    let warnings = build_status_warnings(&snapshot, &daemon_running());
    assert!(
        warnings
            .iter()
            .any(|warning| warning.contains("Plan C split-brain detected")),
        "expected Plan C warning in {warnings:?}"
    );

    if let Some(value) = saved {
        std::env::set_var("TACHI_HOME", value);
    } else {
        std::env::remove_var("TACHI_HOME");
    }
}

#[test]
fn namespace_health_counts_cache_wiki_derived_and_graph_rows() {
    let dir = tempfile::tempdir().expect("temp db dir");
    let db = dir.path().join("memory.db");
    let mut store = MemoryStore::open(db.to_str().expect("db path")).expect("open store");

    store
        .upsert(&vector_health_entry("normal", "manual", None))
        .expect("insert normal row");

    let mut cache = vector_health_entry(
        "foundry:recall-cache:noise",
        FOUNDRY_RECALL_CACHE_SOURCE,
        None,
    );
    cache.path = "/scratch/recall-cache/noise".to_string();
    cache.topic = "recall_rerank_cache".to_string();
    store.upsert(&cache).expect("insert cache row");

    let mut wiki = vector_health_entry("wiki-legacy", "manual", None);
    wiki.path = "/wiki/engineering/legacy".to_string();
    wiki.category = "experience".to_string();
    wiki.domain = None;
    store.upsert(&wiki).expect("insert wiki row");

    store
        .connection()
        .execute(
            "INSERT INTO memory_edges
                 (source_id, target_id, relation, weight, metadata, created_at)
                 VALUES ('normal', 'missing-target', 'related_to', 1.0, '{}', ?1)",
            [Utc::now().to_rfc3339()],
        )
        .expect("insert orphan edge");

    let health = namespace_health(store.connection()).expect("namespace health");
    assert_eq!(health.recall_cache_rows, 1);
    assert_eq!(health.wiki_rows, 1);
    assert_eq!(health.wiki_non_source_rows, 1);
    assert_eq!(health.wiki_non_category_rows, 1);
    assert_eq!(health.derived_items, 0);
    assert_eq!(health.graph_edges, 1);
    assert_eq!(health.graph_orphan_edges, 1);
    assert_eq!(health.graph_relation_types[0].relation, "related_to");
    assert_eq!(health.graph_relation_types[0].count, 1);
}

#[test]
fn checkpoint_fixture_classifier_recognizes_only_real_fixtures() {
    let sep = std::path::MAIN_SEPARATOR;
    let cases = [
        (
            format!("{sep}Users{sep}u{sep}.tachi{sep}global{sep}memory.db"),
            false,
        ),
        (
            format!("{sep}home{sep}u{sep}.openclaw{sep}agents{sep}main{sep}memory.db"),
            false,
        ),
        (
            format!("{sep}srv{sep}checkpointed{sep}prod{sep}memory.db"),
            false,
        ),
        (
            format!("{sep}tmp{sep}feature-daemon-global.db.checkpointed.20260430T012609Z.sqlite"),
            false,
        ),
        (
            format!("{sep}tmp{sep}feature-daemon-global.db.checkpointed.20260430T012609Z.db"),
            true,
        ),
        (
            format!("{sep}tmp{sep}feature-daemon-project.db.checkpointed.20260430T015747Z.db"),
            true,
        ),
        (
            format!("{sep}tmp{sep}foo.db.CHECKPOINTED.20260430T015747Z.DB"),
            true,
        ),
    ];
    for (path, expected) in cases {
        assert_eq!(
            status_cli::is_checkpoint_fixture_path(&path),
            expected,
            "is_checkpoint_fixture_path({path:?}) misclassified"
        );
    }
}
