//! Non-gating full-path timing fixture for the default-off impression ledger.
//!
//! The corpus, query, options, and timed loop intentionally use APIs available
//! at the reviewed #1504 base so this file can be applied there for an A/B run.
//! The small `BASE-AB` observation blocks below are the only #1447-only lines.

use super::*;
use crate::{db::upsert, RecallConfig};
use std::hint::black_box;

const ITERATIONS: usize = 250;
const VECTOR_DIM: usize = 1024;
const QUERY: &str = "sqlite recall ranking migration authority replay";

const CORPUS: &[(&str, &str, &str, &[&str], usize)] = &[
    (
        "architecture-impression-ledger",
        "/guide/architecture",
        "Recall impression ledger records pre-boost channel evidence for deterministic replay without storing query content.",
        &["recall", "ranking", "replay", "ledger"],
        960,
    ),
    (
        "migration-authority",
        "/guide/database",
        "SQLite schema migration authority must reject an older stamped database before persistent tables or indexes are created.",
        &["sqlite", "migration", "authority", "schema"],
        930,
    ),
    (
        "fusion-replay",
        "/notes/scoring",
        "Exact replay reuses the production fusion helper and preserves bit-identical pre-boost ranking scores.",
        &["fusion", "pre-boost", "ranking", "replay"],
        900,
    ),
    (
        "retention-policy",
        "/guide/database",
        "Impression retention has an independent age and quota policy with group cascade semantics.",
        &["retention", "impression", "cascade"],
        820,
    ),
    (
        "access-transaction",
        "/notes/database",
        "Access updates and optional recall evidence commit in the same SQLite transaction and roll back atomically.",
        &["sqlite", "transaction", "recall", "atomic"],
        850,
    ),
    (
        "hybrid-search",
        "/guide/search",
        "Hybrid search merges vector full text and symbolic candidates before applying precision and diversity boosts.",
        &["hybrid", "search", "ranking", "vector"],
        880,
    ),
    (
        "query-sampling",
        "/guide/search",
        "Deterministic SHA-256 query-fingerprint sampling remains disabled at the default zero rate and constructs no payload.",
        &["query", "sampling", "default", "payload"],
        780,
    ),
    (
        "mmr-order",
        "/notes/scoring",
        "Maximum marginal relevance changes displayed order after score boosts while preserving scored candidate evidence.",
        &["mmr", "ranking", "display"],
        740,
    ),
    (
        "graph-expansion",
        "/guide/search",
        "Graph expansion appends related memories after ranked results and records the actual displayed set.",
        &["graph", "expansion", "display"],
        710,
    ),
    (
        "privacy-contract",
        "/guide/security",
        "Telemetry rows exclude query text content entities paths vectors propensity and cache identifiers.",
        &["privacy", "telemetry", "content-free"],
        680,
    ),
    (
        "sqlite-index-health",
        "/notes/database",
        "SQLite index inventory verification detects missing persistent schema objects during migration.",
        &["sqlite", "index", "migration"],
        800,
    ),
    (
        "release-verification",
        "/handoff/testing",
        "Release verification runs focused schema tests full library tests formatting and clippy checks.",
        &["release", "verification", "tests"],
        610,
    ),
    (
        "deployment-runbook",
        "/guide/operations",
        "Service deployment uses a staged rollout health probes and a reversible traffic switch.",
        &["deployment", "operations", "health"],
        420,
    ),
    (
        "incident-response",
        "/notes/operations",
        "Incident response preserves logs identifies the failing boundary and records recovery evidence.",
        &["incident", "recovery", "evidence"],
        390,
    ),
    (
        "agent-work-claims",
        "/guide/dispatch",
        "Work claims bind an admitted agent identity to a bounded task and observable execution environment.",
        &["agent", "claims", "dispatch"],
        320,
    ),
    (
        "memory-lifecycle",
        "/guide/memory",
        "Memory lifecycle review can consolidate supersede archive or retain evidence according to policy.",
        &["memory", "lifecycle", "review"],
        360,
    ),
    (
        "portfolio-routing",
        "/guide/issues",
        "Issue portfolio routing attaches implementation leaves below one canonical architecture owner.",
        &["issues", "portfolio", "routing"],
        280,
    ),
    (
        "credential-boundary",
        "/guide/security",
        "Credentials remain outside memory identity and never follow a model carrier across sessions.",
        &["credentials", "identity", "security"],
        210,
    ),
];

fn benchmark_vector(positive_components: usize) -> Vec<f32> {
    (0..VECTOR_DIM)
        .map(|index| {
            if index < positive_components {
                1.0
            } else {
                -1.0
            }
        })
        .collect()
}

fn fixed_benchmark_store() -> Connection {
    let mut conn = setup();
    for (ordinal, (id, path, text, keywords, positive_components)) in CORPUS.iter().enumerate() {
        let mut entry = memory_entry(id, text, keywords);
        entry.path = (*path).to_string();
        entry.topic = "memcore benchmark fixture".to_string();
        entry.timestamp = format!("2026-07-{:02}T12:00:00Z", ordinal + 1);
        entry.valid_from = entry.timestamp.clone();
        entry.vector = Some(benchmark_vector(*positive_components));
        upsert(&mut conn, &entry, true).expect("insert fixed benchmark memory");
    }
    conn
}

fn update_checksum(mut checksum: u64, results: &[SearchResult]) -> u64 {
    for result in results {
        for byte in result.entry.id.as_bytes() {
            checksum = checksum.rotate_left(5) ^ u64::from(*byte);
        }
    }
    checksum ^ results.len() as u64
}

#[test]
#[ignore = "non-gating full hybrid_search timing report"]
fn full_hybrid_search_default_zero_timing_report() {
    let conn = fixed_benchmark_store();
    let recall_config = RecallConfig::default();
    // BASE-AB #1447-only invariant: remove this assertion on 71aa58bc.
    assert_eq!(recall_config.impression_sample_rate_bps, 0);
    let options = SearchOptions {
        query_vec: Some(vec![1.0; VECTOR_DIM]),
        vec_available: true,
        recall_config: Some(recall_config),
        ..SearchOptions::default()
    };

    // BASE-AB #1447-only observation: remove this line on 71aa58bc.
    let payloads_before = super::ranking::impression_payload_constructions();

    let mut checksum = 0_u64;
    let started = std::time::Instant::now();
    for _ in 0..ITERATIONS {
        let results = hybrid_search(black_box(&conn), black_box(QUERY), black_box(&options))
            .expect("full hybrid_search benchmark iteration");
        checksum = update_checksum(checksum, black_box(&results));
    }
    let total = started.elapsed();

    // BASE-AB #1447-only observations: remove this block on 71aa58bc.
    let ledger_row_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM recall_impression_groups", [], |row| {
            row.get(0)
        })
        .expect("read impression ledger count");
    let payload_construction_count =
        super::ranking::impression_payload_constructions() - payloads_before;
    assert_eq!(ledger_row_count, 0);
    assert_eq!(payload_construction_count, 0);

    println!(
        "benchmark=full_hybrid_search_default0 total_ns={} iterations={} ns_per_call={:.3} result_checksum={} ledger_row_count={} payload_construction_count={}",
        total.as_nanos(),
        ITERATIONS,
        total.as_nanos() as f64 / ITERATIONS as f64,
        black_box(checksum),
        ledger_row_count,
        payload_construction_count,
    );
}
