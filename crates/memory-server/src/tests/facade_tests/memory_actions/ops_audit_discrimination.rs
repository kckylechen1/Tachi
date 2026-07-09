//! Multi-DB operational recall discrimination (tachi#897 / epic #896 Phase 0).
//!
//! Seeds **global** + **project** libraries with synthetic fixtures (no personal
//! memory.db) and runs the production hybrid path via `recall_simulate`
//! (`record_access: false`, no recall-cache short-circuit).
//!
//! Defect class encoded here:
//! **Cross-library rank dilution** — a recent project decision is present in
//! the merged top-10 but outranked by older global roadmap/review noise for a
//! task-shaped query. Product goal is rank 1 for the project decision; current
//! main merges by `final_score` only and dilutes the project row.
//!
//! Layers:
//! - green ratchet: expected id hit@10 on the multi-DB path
//! - green shape lock: rank is not product-green (rank > 1 or documents miss)
//! - `#[ignore]` target: rank == 1 (RED on current main)

use super::*;
use chrono::{Duration, TimeZone, Utc};

fn fixed_ts_days_ago(days_ago: i64) -> String {
    let base = Utc.with_ymd_and_hms(2026, 6, 1, 0, 0, 0).unwrap();
    (base - Duration::days(days_ago)).to_rfc3339()
}

fn seed_entry(
    id: &str,
    path: &str,
    category: &str,
    text: &str,
    keywords: &[&str],
    importance: f64,
    days_ago: i64,
    tier: &str,
) -> memory_core::MemoryEntry {
    let mut e = make_entry(id);
    e.path = path.to_string();
    e.category = category.to_string();
    e.summary = text.chars().take(60).collect();
    e.text = text.to_string();
    e.keywords = keywords.iter().map(|s| s.to_string()).collect();
    e.importance = importance;
    e.timestamp = fixed_ts_days_ago(days_ago);
    e.tier = tier.to_string();
    // allow_cross_project: synthetic fixtures may use /wiki paths in the global
    // DB; path-router would otherwise reject the write.
    e.metadata = json!({
        "keywords": keywords,
        "allow_cross_project": true,
    });
    e
}

/// Global + bound project server with synthetic ops-audit cross-library seeds.
fn make_ops_audit_two_store() -> (tempfile::TempDir, crate::MemoryServer) {
    let tmp = tempfile::tempdir().expect("ops-audit temp root");
    let global_db = tmp.path().join("global/memory.db");
    let project_db = tmp.path().join("projects/ops_audit_proj/memory.db");
    if let Some(parent) = global_db.parent() {
        std::fs::create_dir_all(parent).expect("global parent");
    }
    if let Some(parent) = project_db.parent() {
        std::fs::create_dir_all(parent).expect("project parent");
    }
    // Prefer template copy when available for speed; fall back to MemoryServer::new
    // which migrates empty files.
    let server =
        crate::MemoryServer::new(global_db, Some(project_db)).expect("ops-audit two-store server");

    server
        .with_global_store(|store| {
            let seeds = [
                seed_entry(
                    "ops-xlib-global-roadmap",
                    "/wiki/memory-operating-system",
                    "wiki",
                    "Memory operating system roadmap covers bind rank digest graph and soul phases including open issue priority queues for project decisions across the campaign",
                    &["memory", "roadmap", "open", "issue", "priority", "project", "decision", "rank"],
                    0.85,
                    90,
                    "consolidated",
                ),
                seed_entry(
                    "ops-xlib-global-review",
                    "/wiki/quarterly-review",
                    "wiki",
                    "Quarterly architecture review of open issue triage prioritization and ranking quality for memory operations across every project decision surface",
                    &["review", "open", "issue", "priority", "ranking", "project", "decision"],
                    0.8,
                    60,
                    "consolidated",
                ),
                seed_entry(
                    "ops-xlib-global-guide",
                    "/guide/ranking-review",
                    "guide",
                    "Architecture review notes on open issue priority queues and ranking of project decisions in hybrid recall for the memory operating system",
                    &["architecture", "review", "open", "issue", "priority", "ranking", "project"],
                    0.75,
                    45,
                    "raw",
                ),
                seed_entry(
                    "ops-xlib-global-phase2",
                    "/wiki/phase2-precision",
                    "wiki",
                    "Phase two precision floor requires rank one for recent project decisions and open issue priority judgments without global roadmap dilution",
                    &["phase", "precision", "project", "decision", "open", "issue", "priority", "roadmap"],
                    0.8,
                    40,
                    "consolidated",
                ),
            ];
            for entry in seeds {
                store.upsert(&entry).map_err(|e| e.to_string())?;
            }
            Ok(())
        })
        .expect("seed global ops-audit rows");

    server
        .with_project_store(|store| {
            let decision = seed_entry(
                "ops-xlib-project-decision",
                "/notes/project/decisions",
                "decision",
                "Project decision for this sprint: open issue priority is binding receipts first then ranking discrimination suite before digest work",
                &["project", "decision", "open", "issue", "priority", "sprint"],
                0.95,
                2,
                "raw",
            );
            store.upsert(&decision).map_err(|e| e.to_string())
        })
        .expect("seed project ops-audit decision");

    (tmp, server)
}

const CASE_QUERY: &str = "what is the current open issue priority project decision for this sprint";
const CASE_EXPECTED: &str = "ops-xlib-project-decision";

async fn run_recall_simulate(server: &crate::MemoryServer) -> Value {
    let mut params = tachi_memory_params("recall_simulate");
    params.format = Some("json".to_string());
    params.scope = Some("memory".to_string());
    params.top_k = 10;
    params.enable_rerank = false;
    params.metadata = Some(json!({
        "cases": [
            {
                "name": "cross-library-rank-dilution",
                "query": CASE_QUERY,
                "expected_id": CASE_EXPECTED
            }
        ]
    }));

    let body = crate::facade_memory_ops::handle_tachi_memory(server, params)
        .await
        .expect("recall_simulate should succeed");
    serde_json::from_str(&body).expect("recall_simulate JSON")
}

/// RATCHET — green on current main: project decision is findable in merged top-10.
#[tokio::test]
async fn ops_audit_cross_library_dilution_hit_at_10_ratchet() {
    let (_tmp, server) = make_ops_audit_two_store();
    let parsed = run_recall_simulate(&server).await;

    assert_eq!(parsed["status"], json!("completed"));
    assert_eq!(parsed["action"], json!("recall_simulate"));
    assert_eq!(parsed["case_count"], json!(1));

    let case0 = &parsed["cases"][0];
    assert_eq!(case0["hit"], json!(true), "expected hit@10: {case0}");
    let rank = case0["rank"].as_u64().expect("rank present on hit");
    assert!(
        (1..=10).contains(&rank),
        "rank must be in 1..=10, got {rank}: {case0}"
    );

    // No access mutation on the project decision.
    let access: i64 = server
        .with_project_store_read(|store| {
            store
                .get(CASE_EXPECTED)
                .map(|e| e.map(|m| m.access_count).unwrap_or(-1))
                .map_err(|e| e.to_string())
        })
        .expect("read access_count");
    assert_eq!(access, 0, "recall_simulate must not record access");
}

/// SHAPE LOCK — green while the defect is live: merged path does **not** put
/// the project decision at rank 1 (global noise dilutes). When Phase 2 cures
/// this, flip to assert rank==1 and retire the ignored target.
#[tokio::test]
async fn ops_audit_cross_library_dilution_documents_red_rank_shape() {
    let (_tmp, server) = make_ops_audit_two_store();
    let parsed = run_recall_simulate(&server).await;
    let case0 = &parsed["cases"][0];
    let rank = case0["rank"].as_u64();
    let returned = case0["returned_ids"]
        .as_array()
        .cloned()
        .unwrap_or_default();

    // Product-green would be rank == 1. While the defect is live we require
    // either miss or rank > 1 so the suite keeps discriminating.
    let product_green = rank == Some(1);
    assert!(
        !product_green,
        "cross-library dilution no longer observable (rank=1). \
         Raise the product target out of #[ignore] and drop this shape lock. \
         returned={returned:?}"
    );

    // Prefer the measured audit shape: present but buried (rank in 2..=10).
    if let Some(r) = rank {
        assert!(
            r > 1,
            "expected buried rank (>1), got rank={r}; returned={returned:?}"
        );
    }
}

/// TARGET — RED on current main. Product goal for Phase 2 cross-library policy.
#[tokio::test]
#[ignore = "ops-audit target — red on main; cross-library rank-1 goal for tachi#897 / #896 Phase 2"]
async fn ops_audit_cross_library_dilution_meets_rank1_target() {
    let (_tmp, server) = make_ops_audit_two_store();
    let parsed = run_recall_simulate(&server).await;
    let case0 = &parsed["cases"][0];
    assert_eq!(
        case0["rank"],
        json!(1),
        "project decision must be rank 1 on bound multi-DB search: {case0}"
    );
}
