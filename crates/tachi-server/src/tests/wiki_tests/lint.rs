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
                    scored_count: 0,
                    last_access: None,
                    last_use_at: None,
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
                    scored_count: 0,
                    last_access: None,
                    last_use_at: None,
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
                    scored_count: 0,
                    last_access: None,
                    last_use_at: None,
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
                    scored_count: 0,
                    last_access: None,
                    last_use_at: None,
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
                    scored_count: 0,
                    last_access: None,
                    last_use_at: None,
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
                    scored_count: 0,
                    last_access: None,
                    last_use_at: None,
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
                    scored_count: 0,
                    last_access: None,
                    last_use_at: None,
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
                    scored_count: 0,
                    last_access: None,
                    last_use_at: None,
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
            persist_stale: false,
            project: None,
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
async fn wiki_lint_explicit_named_store_is_strict_and_reports_store_identity() {
    let server = make_server();
    let db_path = server
        .tachi_home_dir()
        .join("projects")
        .join("named-lint")
        .join(memcore::MEMORY_DB_FILENAME);
    std::fs::create_dir_all(db_path.parent().expect("named lint project parent"))
        .expect("create named lint project parent");
    drop(
        MemoryStore::open(db_path.to_str().expect("utf8 named lint DB"))
            .expect("create named lint DB"),
    );
    let manifest_path = server.tachi_home_dir().join("manifest.json");
    let mut manifest = crate::manifest::Manifest::load_or_empty(&manifest_path);
    manifest.dbs.push(crate::manifest::DbEntry {
        path: db_path.display().to_string(),
        role: crate::manifest::DbRole::Project,
        owner: "test".to_string(),
        schema_kind: "tachi".to_string(),
        vec_enabled: true,
        allow_write: true,
        last_doctor_at: Utc::now().to_rfc3339(),
        last_classification: "healthy".to_string(),
        scope_hint: "project:named-lint".to_string(),
        notes: String::new(),
    });
    manifest
        .save(&manifest_path)
        .expect("register named lint project");

    let mut named = make_entry("wiki-lint-named-only");
    named.path = "/wiki/lint/named-only".to_string();
    named.metadata = json!({"lifecycle": "active"});
    let mut global = make_entry("wiki-lint-global-decoy");
    global.path = "/wiki/lint/global-decoy".to_string();
    global.metadata = json!({"lifecycle": "active"});

    server
        .with_named_project_store("named-lint", |store| {
            store.upsert(&named).map_err(|error| error.to_string())
        })
        .expect("seed named lint store");
    server
        .with_global_store(|store| store.upsert(&global).map_err(|error| error.to_string()))
        .expect("seed global lint decoy");

    let raw = server
        .wiki_lint(Parameters(WikiLintParams {
            path_prefix: Some("/wiki/lint".to_string()),
            checks: vec!["orphans".to_string()],
            limit: 50,
            stale_days: 90,
            missing_edge_threshold: 0.85,
            contradiction_threshold: 0.85,
            include_skill_quality: false,
            persist_stale: false,
            project: Some("named-lint".to_string()),
        }))
        .await
        .expect("lint named store");
    let value: Value = serde_json::from_str(&raw).expect("lint JSON");
    let orphans = value["orphans"].as_array().expect("orphans array");
    assert!(
        orphans
            .iter()
            .any(|row| row["id"] == json!("wiki-lint-named-only")),
        "named-store fixture missing: {orphans:?}"
    );
    assert!(
        orphans
            .iter()
            .all(|row| row["id"] != json!("wiki-lint-global-decoy")),
        "RED: explicit named lint must not append global/workspace findings: {orphans:?}"
    );
    let named_row = orphans
        .iter()
        .find(|row| row["id"] == json!("wiki-lint-named-only"))
        .expect("named lint row");
    assert_eq!(named_row["store"]["kind"], json!("named_project"));
    assert_eq!(named_row["store"]["project"], json!("named-lint"));
}

#[tokio::test]
async fn wiki_lint_ignores_operation_log_rows() {
    let server = make_server();
    server
        .with_global_store(|store| {
            let mut log = make_entry("wiki-operation-log");
            log.path = "/wiki/_log".to_string();
            log.topic = "wiki_log".to_string();
            log.domain = Some("wiki".to_string());
            log.metadata = json!({"wiki_log": true});
            store
                .upsert_wiki_operation_log(&log)
                .map_err(|e| e.to_string())?;

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
            persist_stale: false,
            project: None,
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
            persist_stale: false,
            project: None,
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

/// #1072 fix-round (cross-vendor review, #1215): "Discrimination: provide
/// unchanged-behavior RED-on-main proof for the retrieval-exclusion
/// property (a stale/unreviewed entry is excluded from truthful retrieval)
/// — not just 'lint output exists'." The pre-fix `wiki_lint` "stale" check
/// only ever appended a diagnostic row (`stale_nodes`); the entry's
/// persisted `metadata.lifecycle` never changed, so it stayed `active` and
/// fully retrievable through the exact same gate (`derive_wiki_lifecycle` /
/// `apply_wiki_lifecycle_gate`) this leaf's own truthful-retrieval fix
/// relies on. This test proves the causal chain end to end: RED (before
/// `persist_stale`) the contradicted entry is still default-retrievable;
/// GREEN (after `persist_stale=true`) it is not — using the exact predicate
/// (`derive_wiki_lifecycle(..).is_default_retrievable()`) and the exact
/// search entry point (`tachi_wiki_search`) truthful retrieval depends on,
/// not a bespoke assertion.
#[tokio::test]
async fn wiki_lint_persist_stale_makes_contradicted_entry_retrieval_excluded() {
    let server = make_server();
    server
        .with_global_store(|store| {
            let mut contradicted = make_entry("wiki-persist-stale-target");
            contradicted.path = "/wiki/test/persist-stale/target".to_string();
            contradicted.summary = "SemanticStalePersistNeedle target entry".to_string();
            contradicted.text =
                "SemanticStalePersistNeedle documents policy a newer entry contradicts."
                    .to_string();
            store.upsert(&contradicted).map_err(|e| e.to_string())?;

            let mut newer = make_entry("wiki-persist-stale-newer");
            newer.path = "/wiki/test/persist-stale/newer".to_string();
            newer.text = "Newer entry that contradicts the persist-stale target.".to_string();
            store.upsert(&newer).map_err(|e| e.to_string())?;

            let edge = memcore::MemoryEdge {
                source_id: "wiki-persist-stale-newer".to_string(),
                target_id: "wiki-persist-stale-target".to_string(),
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
        .expect("seed persist-stale fixtures");

    async fn target_lifecycle_retrievable(server: &MemoryServer) -> bool {
        let fetched = server
            .get_memory(Parameters(GetMemoryParams {
                id: "wiki-persist-stale-target".to_string(),
                include_archived: false,
                project: None,
            }))
            .await
            .expect("get_memory should succeed");
        let entry: Value = serde_json::from_str(&fetched).expect("entry json");
        let metadata = entry.get("metadata").cloned().unwrap_or_else(|| json!({}));
        let path = entry
            .get("path")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        crate::tool_params::derive_wiki_lifecycle(&metadata, &path).is_default_retrievable()
    }

    // RED: before any persist_stale lint run, the contradicted entry is
    // still `active` (unchanged pre-#1072-fix-round behavior) and therefore
    // default-retrievable — this is the exact bug the review flagged.
    assert!(
        target_lifecycle_retrievable(&server).await,
        "RED baseline: contradicted entry must start default-retrievable (unchanged behavior)"
    );

    let lint_response = server
        .wiki_lint(Parameters(WikiLintParams {
            path_prefix: Some("/wiki/test/persist-stale".to_string()),
            checks: vec!["stale".to_string()],
            limit: 50,
            stale_days: 90,
            missing_edge_threshold: 0.6,
            contradiction_threshold: 0.6,
            include_skill_quality: false,
            persist_stale: true,
            project: None,
        }))
        .await
        .expect("wiki_lint with persist_stale should succeed");
    let lint_parsed: Value = serde_json::from_str(&lint_response).expect("wiki_lint json");
    assert_eq!(lint_parsed["stale_persisted"], json!(true));
    assert_eq!(
        lint_parsed["stale_persist_errors"].as_array().map(Vec::len),
        Some(0)
    );
    let stale_rows = lint_parsed["stale_nodes"]
        .as_array()
        .expect("stale_nodes array");
    assert!(stale_rows
        .iter()
        .any(|row| row["id"] == json!("wiki-persist-stale-target")));

    // GREEN: after the persist_stale write-back, the SAME predicate the
    // retrieval gate calls now excludes the entry.
    assert!(
        !target_lifecycle_retrievable(&server).await,
        "GREEN: persisted semantic-stale entry must no longer be default-retrievable"
    );

    // End-to-end proof through the real MCP search entry point: the entry
    // must have been default-retrievable by exact-needle search before the
    // lint run and excluded after it.
    let search_params = WikiSearchParams {
        query: "SemanticStalePersistNeedle".to_string(),
        path_prefix: Some("/wiki/test/persist-stale".to_string()),
        category: None,
        top_k: 10,
        include_archived: false,
        agent_role: None,
        project: None,
        domain: None,
        file_context: None,
        error_context: None,
        weights: None,
        lifecycle: None,
    };
    let search_markdown = server
        .tachi_wiki_search(Parameters(search_params))
        .await
        .expect("post-persist search should succeed");
    assert!(
        !search_markdown.contains("/wiki/test/persist-stale/target"),
        "GREEN: default-scope search must exclude the now-stale entry: {search_markdown}"
    );
}

#[tokio::test]
async fn wiki_lint_migration_audit_never_applies_edges_across_store_identity() {
    let (server, _project_db) = crate::tests::make_server_with_project_fixture("bound-project");
    let mut bound = make_entry("wiki-lint-cross-store-same-id");
    bound.path = "/wiki/lint/cross-store/bound".to_string();
    bound.metadata = json!({"lifecycle": "active"});
    let mut global_target = make_entry("wiki-lint-cross-store-same-id");
    global_target.path = "/wiki/lint/cross-store/global-target".to_string();
    global_target.metadata = json!({"lifecycle": "active"});
    let mut global_source = make_entry("wiki-lint-cross-store-source");
    global_source.path = "/wiki/lint/cross-store/global-source".to_string();
    global_source.metadata = json!({"lifecycle": "active"});

    server
        .with_project_store(|store| store.upsert(&bound).map_err(|error| error.to_string()))
        .expect("seed bound same-id row");
    server
        .with_global_store(|store| {
            store
                .upsert(&global_target)
                .map_err(|error| error.to_string())?;
            store
                .upsert(&global_source)
                .map_err(|error| error.to_string())?;
            store
                .add_edge(&memcore::MemoryEdge {
                    source_id: global_source.id.clone(),
                    target_id: global_target.id.clone(),
                    relation: "supersedes".to_string(),
                    weight: 0.9,
                    metadata: json!({"source": "test"}),
                    created_at: Utc::now().to_rfc3339(),
                    valid_from: String::new(),
                    valid_to: None,
                })
                .map_err(|error| error.to_string())
        })
        .expect("seed global edge");

    server
        .wiki_lint(Parameters(WikiLintParams {
            path_prefix: Some("/wiki/lint/cross-store".to_string()),
            checks: vec!["stale".to_string()],
            limit: 50,
            stale_days: 90,
            missing_edge_threshold: 0.85,
            contradiction_threshold: 0.85,
            include_skill_quality: false,
            persist_stale: true,
            project: None,
        }))
        .await
        .expect("migration audit lint");

    let bound_after = server
        .with_project_store_read(|store| {
            store
                .get("wiki-lint-cross-store-same-id")
                .map_err(|error| error.to_string())
        })
        .expect("read bound row")
        .expect("bound row exists");
    let global_after = server
        .with_global_store_read(|store| {
            store
                .get("wiki-lint-cross-store-same-id")
                .map_err(|error| error.to_string())
        })
        .expect("read global row")
        .expect("global row exists");
    assert_eq!(bound_after.metadata["lifecycle"], json!("active"));
    assert_eq!(
        global_after.metadata["lifecycle"],
        json!("stale"),
        "the edge must affect only its own physical store"
    );
}

#[tokio::test]
async fn wiki_lint_rejects_unknown_check_names() {
    let server = make_server();
    let error = server
        .wiki_lint(Parameters(WikiLintParams {
            path_prefix: Some("/wiki".to_string()),
            checks: vec!["orphan".to_string()],
            limit: 10,
            stale_days: 90,
            missing_edge_threshold: 0.85,
            contradiction_threshold: 0.85,
            include_skill_quality: false,
            persist_stale: false,
            project: None,
        }))
        .await
        .expect_err("unknown check must fail closed");
    assert!(
        error.contains("invalid wiki_lint check 'orphan'"),
        "{error}"
    );
}

/// tachi#1561 (L6): `wiki_lint` is wiki-corpus hygiene, but its `path_prefix`
/// went straight into `list_wiki_entries_for_plan` with no root check — so
/// `path_prefix="/"` (or `/anchors`, `/user/affect`, ...) turned a lint call
/// into a whole-store walker that reports id, path and timestamp for every row
/// it touches. Clamp to the same knowledge roots `browse`/`read` accept, and
/// fail loudly rather than silently narrowing to `/wiki` (a silent narrowing
/// would make the report claim coverage it never had).
#[tokio::test]
async fn wiki_lint_clamps_path_prefix_to_the_knowledge_artifact_roots() {
    let server = make_server();

    let lint_params = |prefix: &str| WikiLintParams {
        path_prefix: Some(prefix.to_string()),
        checks: vec!["orphans".to_string()],
        limit: 10,
        stale_days: 90,
        missing_edge_threshold: 0.85,
        contradiction_threshold: 0.85,
        include_skill_quality: false,
        persist_stale: false,
        project: None,
    };

    for out_of_root in ["/", "/anchors", "/user/affect", "/scratch"] {
        let error = server
            .wiki_lint(Parameters(lint_params(out_of_root)))
            .await
            .expect_err("out-of-root lint scope must fail closed");
        assert!(
            error.contains("invalid wiki_lint path_prefix"),
            "prefix {out_of_root} must be rejected as a parameter error: {error}"
        );
    }

    for in_root in ["/wiki", "/wiki/engineering", "/guide", "/guide/tachi"] {
        server
            .wiki_lint(Parameters(lint_params(in_root)))
            .await
            .unwrap_or_else(|error| panic!("{in_root} must stay lintable: {error}"));
    }
}
