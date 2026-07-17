use super::*;

#[tokio::test]
async fn wiki_lint_reports_memory_health_and_skill_quality_guards() {
    let server = make_server();
    let old_ts = (Utc::now() - chrono::Duration::days(120)).to_rfc3339();

    server
        .with_global_store(|store| {
            let entries = vec![
                MemoryEntry {
                    id: "wiki-orphan".to_string(),
                    path: "/wiki/test/orphan".to_string(),
                    summary: "orphan".to_string(),
                    text: "Standalone old note".to_string(),
                    importance: 0.4,
                    timestamp: old_ts.clone(),
                    valid_from: String::new(),
                    valid_until: None,
                    category: "fact".to_string(),
                    topic: "orphan".to_string(),
                    keywords: vec![],
                    persons: vec![],
                    entities: vec![],
                    location: String::new(),
                    source: "test".to_string(),
                    scope: "global".to_string(),
                    archived: false,
                    access_count: 0,
                    last_access: None,
                    revision: 1,
                    metadata: json!({}),
                    vector: None,
                    // Explicit `durable` opts out of the `/wiki*` → permanent
                    // default retention applied by `normalize_for_write`, so
                    // the stale check still flags this fixture.
                    retention_policy: Some("durable".to_string()),
                    domain: Some("general".to_string()),
                    recall_count: 0,
                    query_diversity: 0,
                    tier: "raw".to_string(),
                },
                MemoryEntry {
                    id: "wiki-always".to_string(),
                    path: "/wiki/test/policy-a".to_string(),
                    summary: "policy a".to_string(),
                    text: "Always use a feature flag for rollout safety.".to_string(),
                    importance: 0.7,
                    timestamp: Utc::now().to_rfc3339(),
                    valid_from: String::new(),
                    valid_until: None,
                    category: "fact".to_string(),
                    topic: "policy".to_string(),
                    keywords: vec![],
                    persons: vec![],
                    entities: vec![],
                    location: String::new(),
                    source: "test".to_string(),
                    scope: "global".to_string(),
                    archived: false,
                    access_count: 0,
                    last_access: None,
                    revision: 1,
                    metadata: json!({}),
                    vector: None,
                    retention_policy: None,
                    domain: Some("general".to_string()),
                    recall_count: 0,
                    query_diversity: 0,
                    tier: "raw".to_string(),
                },
                MemoryEntry {
                    id: "wiki-never".to_string(),
                    path: "/wiki/test/policy-b".to_string(),
                    summary: "policy b".to_string(),
                    text: "Do not use a feature flag for rollout safety.".to_string(),
                    importance: 0.7,
                    timestamp: Utc::now().to_rfc3339(),
                    valid_from: String::new(),
                    valid_until: None,
                    category: "fact".to_string(),
                    topic: "policy".to_string(),
                    keywords: vec![],
                    persons: vec![],
                    entities: vec![],
                    location: String::new(),
                    source: "test".to_string(),
                    scope: "global".to_string(),
                    archived: false,
                    access_count: 0,
                    last_access: None,
                    revision: 1,
                    metadata: json!({}),
                    vector: None,
                    retention_policy: None,
                    domain: Some("general".to_string()),
                    recall_count: 0,
                    query_diversity: 0,
                    tier: "raw".to_string(),
                },
                MemoryEntry {
                    id: "wiki-dirty".to_string(),
                    path: "/wiki/test/dirty".to_string(),
                    summary: "dirty <think）leak".to_string(),
                    text: "A leaked <think） tag should be reported.".to_string(),
                    importance: 0.7,
                    timestamp: Utc::now().to_rfc3339(),
                    valid_from: String::new(),
                    valid_until: None,
                    category: "fact".to_string(),
                    topic: "dirty".to_string(),
                    keywords: vec![],
                    persons: vec![],
                    entities: vec![],
                    location: String::new(),
                    source: "test".to_string(),
                    scope: "global".to_string(),
                    archived: false,
                    access_count: 0,
                    last_access: None,
                    revision: 1,
                    metadata: json!({}),
                    vector: None,
                    retention_policy: None,
                    domain: Some("general".to_string()),
                    recall_count: 0,
                    query_diversity: 0,
                    tier: "raw".to_string(),
                },
                MemoryEntry {
                    id: "wiki-duplicate-a".to_string(),
                    path: "/wiki/test/duplicate-a".to_string(),
                    summary: "duplicate a".to_string(),
                    text: "Duplicate token sequence exact match for wiki lint duplicate detection."
                        .to_string(),
                    importance: 0.7,
                    timestamp: Utc::now().to_rfc3339(),
                    valid_from: String::new(),
                    valid_until: None,
                    category: "fact".to_string(),
                    topic: "duplicate".to_string(),
                    keywords: vec![],
                    persons: vec![],
                    entities: vec![],
                    location: String::new(),
                    source: "test".to_string(),
                    scope: "global".to_string(),
                    archived: false,
                    access_count: 0,
                    last_access: None,
                    revision: 1,
                    metadata: json!({}),
                    vector: None,
                    retention_policy: None,
                    domain: Some("general".to_string()),
                    recall_count: 0,
                    query_diversity: 0,
                    tier: "raw".to_string(),
                },
                MemoryEntry {
                    id: "wiki-duplicate-b".to_string(),
                    path: "/wiki/test/duplicate-b".to_string(),
                    summary: "duplicate b".to_string(),
                    text: "Duplicate token sequence exact match for wiki lint duplicate detection."
                        .to_string(),
                    importance: 0.7,
                    timestamp: Utc::now().to_rfc3339(),
                    valid_from: String::new(),
                    valid_until: None,
                    category: "fact".to_string(),
                    topic: "duplicate".to_string(),
                    keywords: vec![],
                    persons: vec![],
                    entities: vec![],
                    location: String::new(),
                    source: "test".to_string(),
                    scope: "global".to_string(),
                    archived: false,
                    access_count: 0,
                    last_access: None,
                    revision: 1,
                    metadata: json!({}),
                    vector: None,
                    retention_policy: None,
                    domain: Some("general".to_string()),
                    recall_count: 0,
                    query_diversity: 0,
                    tier: "raw".to_string(),
                },
                MemoryEntry {
                    id: "skill-snapshot-a".to_string(),
                    path: "/skills/coding/merge-a/distilled/20260406T000000".to_string(),
                    summary: "merge a".to_string(),
                    text: "Follow SOP: inspect logs, isolate failure, add regression test."
                        .to_string(),
                    importance: 0.9,
                    timestamp: Utc::now().to_rfc3339(),
                    valid_from: String::new(),
                    valid_until: None,
                    category: "decision".to_string(),
                    topic: "merge_a".to_string(),
                    keywords: vec![],
                    persons: vec![],
                    entities: vec!["skill:merge-a".to_string()],
                    location: String::new(),
                    source: "test".to_string(),
                    scope: "global".to_string(),
                    archived: false,
                    access_count: 0,
                    last_access: None,
                    revision: 1,
                    metadata: json!({}),
                    vector: None,
                    retention_policy: Some("permanent".to_string()),
                    domain: Some("coding".to_string()),
                    recall_count: 0,
                    query_diversity: 0,
                    tier: "raw".to_string(),
                },
                MemoryEntry {
                    id: "skill-snapshot-b".to_string(),
                    path: "/skills/coding/merge-b/distilled/20260406T000100".to_string(),
                    summary: "merge b".to_string(),
                    text: "Follow SOP: inspect logs, isolate failure, add regression test."
                        .to_string(),
                    importance: 0.9,
                    timestamp: Utc::now().to_rfc3339(),
                    valid_from: String::new(),
                    valid_until: None,
                    category: "decision".to_string(),
                    topic: "merge_b".to_string(),
                    keywords: vec![],
                    persons: vec![],
                    entities: vec!["skill:merge-b".to_string()],
                    location: String::new(),
                    source: "test".to_string(),
                    scope: "global".to_string(),
                    archived: false,
                    access_count: 0,
                    last_access: None,
                    revision: 1,
                    metadata: json!({}),
                    vector: None,
                    retention_policy: Some("permanent".to_string()),
                    domain: Some("coding".to_string()),
                    recall_count: 0,
                    query_diversity: 0,
                    tier: "raw".to_string(),
                },
            ];
            for entry in entries {
                store.upsert(&entry).map_err(|e| e.to_string())?;
            }

            let skill_a = HubCapability {
                id: "skill:merge-a".to_string(),
                cap_type: "skill".to_string(),
                name: "merge-a".to_string(),
                version: 1,
                description: "merge skill a".to_string(),
                definition: json!({
                    "content": "Follow SOP: inspect logs, isolate failure, add regression test.",
                    "prompt": "Follow SOP: inspect logs, isolate failure, add regression test.",
                    "policy": {"visibility": "listed"},
                    "skill_path": "/skills/coding/merge-a"
                })
                .to_string(),
                enabled: true,
                review_status: "approved".to_string(),
                health_status: "healthy".to_string(),
                last_error: None,
                last_success_at: None,
                last_failure_at: None,
                fail_streak: 0,
                active_version: None,
                exposure_mode: "direct".to_string(),
                uses: 3,
                successes: 3,
                failures: 0,
                avg_rating: 0.2,
                last_used: Some((Utc::now() - chrono::Duration::days(40)).to_rfc3339()),
                created_at: Utc::now().to_rfc3339(),
                updated_at: Utc::now().to_rfc3339(),
            };
            let skill_b = HubCapability {
                id: "skill:merge-b".to_string(),
                cap_type: "skill".to_string(),
                name: "merge-b".to_string(),
                version: 1,
                description: "merge skill b".to_string(),
                definition: json!({
                    "content": "Follow SOP: inspect logs, isolate failure, add regression test.",
                    "prompt": "Follow SOP: inspect logs, isolate failure, add regression test.",
                    "policy": {"visibility": "listed"},
                    "skill_path": "/skills/coding/merge-b"
                })
                .to_string(),
                enabled: true,
                review_status: "approved".to_string(),
                health_status: "healthy".to_string(),
                last_error: None,
                last_success_at: None,
                last_failure_at: None,
                fail_streak: 0,
                active_version: None,
                exposure_mode: "direct".to_string(),
                uses: 5,
                successes: 5,
                failures: 0,
                avg_rating: 4.5,
                last_used: Some(Utc::now().to_rfc3339()),
                created_at: Utc::now().to_rfc3339(),
                updated_at: Utc::now().to_rfc3339(),
            };
            store.hub_register(&skill_a).map_err(|e| e.to_string())?;
            store.hub_register(&skill_b).map_err(|e| e.to_string())?;
            Ok(())
        })
        .expect("seed wiki lint fixtures");

    let response = server
        .wiki_lint(Parameters(WikiLintParams {
            path_prefix: Some("/wiki/test".to_string()),
            checks: vec![
                "orphans".to_string(),
                "contradictions".to_string(),
                "stale".to_string(),
                "missing_edges".to_string(),
                "dirty_data".to_string(),
                "duplicates".to_string(),
            ],
            limit: 50,
            stale_days: 90,
            missing_edge_threshold: 0.6,
            contradiction_threshold: 0.6,
            include_skill_quality: true,
        }))
        .await
        .expect("wiki_lint should succeed");
    let json: Value = serde_json::from_str(&response).expect("wiki_lint json");
    assert_eq!(json["skill_quality"]["global"]["pairwise_cap"], json!(500));
    assert!(json["skill_quality"]["global"]["pairwise_evaluated_skills"]
        .as_u64()
        .is_some_and(|count| count >= 2));
    assert_eq!(
        json["skill_quality"]["global"]["pairwise_skipped_skills"],
        json!(0)
    );
    assert!(
        json["orphans"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v["id"] == "wiki-orphan"),
        "expected orphan node in wiki_lint output"
    );
    assert!(
        json["stale_nodes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v["id"] == "wiki-orphan"),
        "expected stale node in wiki_lint output"
    );
    assert!(
        !json["missing_edge_hints"].as_array().unwrap().is_empty(),
        "expected missing edge hints"
    );
    assert!(
        !json["contradiction_candidates"]
            .as_array()
            .unwrap()
            .is_empty(),
        "expected contradiction candidates"
    );
    assert!(
        json["dirty_data"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v["id"] == "wiki-dirty"),
        "expected dirty data finding"
    );
    assert!(
        json["duplicates"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| { v["left_id"] == "wiki-duplicate-a" && v["right_id"] == "wiki-duplicate-b" }),
        "expected duplicate finding"
    );

    let archived = server
        .with_global_store_read(|store| store.hub_get("skill:merge-a").map_err(|e| e.to_string()))
        .expect("load archived skill")
        .expect("archived skill should exist");
    let archived_def: Value =
        serde_json::from_str(&archived.definition).expect("archived skill def json");
    assert_eq!(archived_def["quality_guard"]["status"], "archived");
    assert_eq!(archived_def["policy"]["visibility"], "hidden");
    assert!(
        archived_def["quality_guard"]["merge_hints"]
            .as_array()
            .map(|arr| !arr.is_empty())
            .unwrap_or(false),
        "expected merge hints on archived skill"
    );
    let related_edges = server
        .with_global_store_read(|store| {
            store
                .get_edges("skill-snapshot-a", "both", Some("merge_hint"))
                .map_err(|e| e.to_string())
        })
        .expect("load related skill edges");
    assert!(
        !related_edges.is_empty(),
        "expected skill graph merge_hint edge from quality guard"
    );
}

#[tokio::test]
async fn wiki_lint_ignores_operation_log_rows() {
    let server = make_server();
    server
        .with_global_store(|store| {
            let mut log = make_entry("wiki-log-noise");
            log.path = "/wiki/_log".to_string();
            log.topic = "wiki_log".to_string();
            log.domain = Some("wiki".to_string());
            log.metadata = json!({"wiki_log": true});
            store.upsert(&log).map_err(|e| e.to_string())?;

            let mut orphan = make_entry("wiki-real-orphan");
            orphan.path = "/wiki/test/real-orphan".to_string();
            orphan.domain = Some("wiki".to_string());
            orphan.metadata = json!({"wiki": true});
            store.upsert(&orphan).map_err(|e| e.to_string())?;

            Ok(())
        })
        .expect("seed wiki lint log fixture");

    let response = server
        .wiki_lint(Parameters(WikiLintParams {
            path_prefix: Some("/wiki".to_string()),
            checks: vec!["orphans".to_string()],
            limit: 50,
            stale_days: 90,
            missing_edge_threshold: 0.6,
            contradiction_threshold: 0.6,
            include_skill_quality: false,
        }))
        .await
        .expect("wiki_lint should succeed");
    let parsed: Value = serde_json::from_str(&response).expect("wiki_lint json");
    let orphan_ids = parsed["orphans"]
        .as_array()
        .expect("orphans array")
        .iter()
        .map(|row| row["id"].as_str().unwrap_or_default())
        .collect::<Vec<_>>();
    assert!(orphan_ids.contains(&"wiki-real-orphan"));
    assert!(!orphan_ids.contains(&"wiki-log-noise"));
}

/// #1072 RED case 4: "Permanent stale wiki that contradicts a changed
/// trusted source must become semantic-stale." Scoped honestly (see
/// `wiki_ops::lint`'s inline comment and the `knowledge_artifact` module
/// doc): this leaf's concrete trigger is the `supersedes`/`contradicts`
/// graph-edge signal canon doc §7 lists, not full external trusted-doc
/// blob-SHA drift detection (a separate leaf).
#[tokio::test]
async fn wiki_lint_stale_check_ignores_retention_policy_for_contradicted_permanent_entries() {
    let server = make_server();
    let old_ts = (Utc::now() - chrono::Duration::days(120)).to_rfc3339();
    server
        .with_global_store(|store| {
            let mut permanent_contradicted = make_entry("wiki-semantic-stale-permanent");
            permanent_contradicted.path = "/wiki/test/semantic-stale/permanent".to_string();
            permanent_contradicted.text =
                "Permanent policy note that a newer entry contradicts.".to_string();
            permanent_contradicted.timestamp = old_ts.clone();
            permanent_contradicted.retention_policy = Some("permanent".to_string());
            store
                .upsert(&permanent_contradicted)
                .map_err(|e| e.to_string())?;

            let mut newer_contradictor = make_entry("wiki-semantic-stale-newer");
            newer_contradictor.path = "/wiki/test/semantic-stale/newer".to_string();
            newer_contradictor.text =
                "Newer entry that contradicts the permanent policy note.".to_string();
            store
                .upsert(&newer_contradictor)
                .map_err(|e| e.to_string())?;

            // RED-safety control: a permanent entry with NO contradicts/
            // supersedes edge must stay exempt from the retention-age check
            // (unchanged pre-#1072 behavior) — proves this fix does not
            // simply delete the permanent/pinned exemption outright.
            let mut permanent_untouched = make_entry("wiki-semantic-stale-untouched");
            permanent_untouched.path = "/wiki/test/semantic-stale/untouched".to_string();
            permanent_untouched.text = "Permanent policy note nothing contradicts.".to_string();
            permanent_untouched.timestamp = old_ts.clone();
            permanent_untouched.retention_policy = Some("permanent".to_string());
            store
                .upsert(&permanent_untouched)
                .map_err(|e| e.to_string())?;

            let edge = memcore::MemoryEdge {
                source_id: "wiki-semantic-stale-newer".to_string(),
                target_id: "wiki-semantic-stale-permanent".to_string(),
                relation: "contradicts".to_string(),
                weight: 0.9,
                metadata: json!({"source": "test"}),
                created_at: Utc::now().to_rfc3339(),
                valid_from: String::new(),
                valid_to: None,
            };
            store.add_edge(&edge).map_err(|e| e.to_string())?;
            Ok(())
        })
        .expect("seed semantic staleness fixtures");

    let response = server
        .wiki_lint(Parameters(WikiLintParams {
            path_prefix: Some("/wiki/test/semantic-stale".to_string()),
            checks: vec!["stale".to_string()],
            limit: 50,
            stale_days: 90,
            missing_edge_threshold: 0.6,
            contradiction_threshold: 0.6,
            include_skill_quality: false,
        }))
        .await
        .expect("wiki_lint should succeed");
    let parsed: Value = serde_json::from_str(&response).expect("wiki_lint json");
    let stale_rows = parsed["stale_nodes"].as_array().expect("stale_nodes array");
    let stale = stale_rows
        .iter()
        .find(|row| row["id"] == json!("wiki-semantic-stale-permanent"))
        .expect("RED: contradicted permanent entry must be flagged stale despite retention_policy=permanent");
    assert_eq!(
        stale["reason"],
        json!("semantic_stale_contradicted_or_superseded")
    );
    assert!(
        !stale_rows
            .iter()
            .any(|row| row["id"] == json!("wiki-semantic-stale-untouched")),
        "an untouched permanent entry must stay exempt from the stale check: {stale_rows:?}"
    );
}
