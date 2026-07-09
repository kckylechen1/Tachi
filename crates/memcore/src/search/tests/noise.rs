use super::*;

#[test]
fn hybrid_hides_operation_logs() {
    let mut conn = setup();
    insert(
        &mut conn,
        "knowledge",
        "TrendLock durable decision rule for agents",
        &["trendlock"],
    );
    let mut log = memory_entry(
        "wiki-operation-log",
        "TrendLock write operation log should not be recalled",
        &["trendlock", "log"],
    );
    log.path = "/wiki/_log".to_string();
    log.topic = "wiki_log".to_string();
    log.metadata = json!({"wiki_log": true});
    upsert(&mut conn, &log, false).unwrap();

    let opts = SearchOptions {
        top_k: 5,
        record_access: false,
        ..Default::default()
    };
    let results = hybrid_search(&conn, "TrendLock", &opts).unwrap();
    assert!(results.iter().any(|result| result.entry.id == "knowledge"));
    assert!(!results
        .iter()
        .any(|result| result.entry.id == "wiki-operation-log"));
}

#[test]
fn quality_multiplier_demotes_sft_training_samples() {
    let mut sample = memory_entry(
        "sft-sample",
        "DaemonAdapterTimeoutFix root cause and verified production fix",
        &["daemon", "timeout", "fix"],
    );
    sample.importance = 0.95;
    sample.path = "/sft/v4/strict/engineering/123".to_string();
    sample.topic = "sft-memory".to_string();
    sample.metadata = json!({"training_sample": true});
    assert_eq!(quality_multiplier(&sample), 0.45);

    let mut handoff = memory_entry(
        "handoff",
        "DaemonAdapterTimeoutFix operational handoff",
        &["daemon", "timeout", "fix"],
    );
    handoff.category = "handoff".to_string();
    handoff.importance = 0.95;
    assert_eq!(quality_multiplier(&handoff), 1.0);
}

#[test]
fn quality_multiplier_demotes_openclaw_low_signal_entries() {
    let mut legacy = memory_entry(
        "openclaw-legacy",
        "Legacy migrated raw session note",
        &["openclaw", "legacy"],
    );
    legacy.importance = 0.95;
    legacy.path = "/openclaw/legacy".to_string();
    assert_eq!(quality_multiplier(&legacy), 0.55);

    let mut unnamed = memory_entry(
        "openclaw-unnamed",
        "Unnamed migrated memory should not dominate recall",
        &["openclaw", "unnamed"],
    );
    unnamed.importance = 0.95;
    unnamed.path = "/openclaw/agent-main/unnamed".to_string();
    assert_eq!(quality_multiplier(&unnamed), 0.55);
}

#[test]
fn recall_cache_variants_are_search_noise_by_default() {
    let mut cache = memory_entry(
        "openclaw-recall-cache",
        "Recall rerank cache for query: Scout pipeline fixes",
        &["recall", "cache"],
    );
    cache.path = "/openclaw/agent-main/recall-cache/Scout_pipeline".to_string();
    cache.topic = "recall_rerank_cache".to_string();
    assert!(is_search_noise_entry(&cache, None));
    assert!(is_search_noise_entry(&cache, Some("/openclaw/agent-main")));
    assert!(!is_search_noise_entry(
        &cache,
        Some("/openclaw/agent-main/recall-cache")
    ));
}
