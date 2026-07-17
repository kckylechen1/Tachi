//! tachi#1145 — W5 latency-visibility workload: auto-link's four separated
//! wall times (spawn-queue wait / pool-checkout wait / search / write) under
//! read-slot saturation, beside a foreground W2-shaped (broad-NL) load.
//!
//! `#[ignore]`d: this is a load-test harness, not a pass/fail unit test — run
//! it on demand:
//!
//!     cargo test -p tachi-server --lib auto_link_latency_w5 -- --ignored --nocapture
//!
//! # Why this lives in `src/tests/`, not `examples/` (tachi#1097 S2 record)
//!
//! `crates/tachi-server/examples/auto_link_receipt_workload.rs` is a
//! compiling BLOCKED stub: every API this workload needs (`MemoryServer`,
//! `run_auto_linking`, `AutoLinkReceipt`, `spawn_auto_linking_for_test`) was
//! `pub(crate)`, unreachable from an `examples/` crate — a separate
//! compilation unit that can only name `pub` items. The "promote to `pub`
//! for a benchmark" route was explicitly rejected (tachi#1145 contract). An
//! in-crate `#[cfg(test)]` test has full `pub(crate)` access instead:
//! `memory_search_ops::auto_link` was widened from a private `mod` to
//! `pub(crate) mod` (matching the existing `routing_config`/`save_memory`
//! precedent already in `memory_search_ops/mod.rs`) so this file — a SIBLING
//! of `memory_search_ops`, not a descendant — can reach it. That is
//! `pub(crate)` module-level visibility, the ceiling the contract names
//! ("full pub(crate) access, no visibility widening"), never `pub`.
//!
//! # The two blind spots this closes (tachi#1145)
//!
//! 1. Spawn-queue wait: `AutoLinkReceipt::spawn_queue_wait`
//!    (`memory_search_ops/auto_link.rs`) is captured BEFORE `tokio::spawn`,
//!    so it is visible here as a real, separate wall time — previously the
//!    receipt's clock only started once the task was already executing,
//!    which is blind to how long it sat queued.
//! 2. Pairing under concurrency without unredacting `entry_id`: this test
//!    drives `spawn_auto_linking_for_test`, a `#[cfg(test)]`-only
//!    correlation-token seam. `entry_id` stays the fixed redacted marker in
//!    every receipt (tachi#1097 r1 codex review ①) — pairing instead uses an
//!    opaque `u64` token this test mints itself, never logged, never
//!    touching production redaction.
//!
//! # What is and is not measured
//!
//! Four SEPARATE wall times per completed auto-link pass, never summed:
//!   * `spawn_queue_wait` — tokio dispatch delay before the task starts.
//!   * `pool_checkout_wait` — the portion of the read call spent waiting for
//!     a global read-pool slot (`LayerAvailability::Measured`, tachi#1125).
//!     Only `DbScope::Global` + no named project has this wired; this
//!     workload uses exactly that path throughout, so every receipt's value
//!     must be `Measured`, never `Unavailable`/`NotSampled` — asserted once
//!     per level below, because a silent regression to `Unavailable` would
//!     make every reported pool-wait/search percentile wrong by omission,
//!     not just missing.
//!   * `search` — computed as `read_elapsed - pool_checkout_wait`: the
//!     search-execution portion of the read call with the pool-wait portion
//!     subtracted back out. Never itself a receipt field — it is a VIEW over
//!     two fields that already exist, so it cannot drift from them.
//!   * `write_elapsed` — as already measured (tachi#1097 S1). Pool-checkout
//!     wait has no recording twin on the write path yet (writes go through a
//!     single mutex + rw_gate, not the `ReadStorePool` #1125 instruments —
//!     a future leaf's separation, not this one's).
//!
//! An auto-link pass whose completion never arrives (a hung task) is
//! unmeasured, not zero: `run_concurrency_level` panics rather than silently
//! reporting fewer samples than were requested.

use super::*;
use crate::memory_search_ops::auto_link::{spawn_auto_linking_for_test, AutoLinkReceipt};
use crate::{DbScope, MemoryServer};
use memcore::{LayerAvailability, MemoryEntry, SearchOptions};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

/// Concurrency levels the contract names: 1 / P / P+1 / 2P, where P is the
/// server's actual configured global read-pool size — never hardcoded, so
/// this stays correct under a `TACHI_MEMORY_READ_POOL_SIZE` override.
fn concurrency_levels(pool_size: usize) -> Vec<usize> {
    vec![1, pool_size, pool_size + 1, pool_size * 2]
}

/// How many auto-link passes to run per concurrency level, in waves of
/// `concurrency` (see [`run_concurrency_level`]). Small enough that the
/// whole `#[ignore]`d suite finishes in seconds, large enough that p95/p99
/// are not just "the slowest of three".
const SAMPLES_PER_LEVEL: usize = 40;

/// Broad, multi-topic query overlapping many corpus entries — the W2 shape
/// from `crates/memcore/examples/receipts_workloads.rs` (an 8-word phrase
/// hitting the FTS OR-fallback path), reused here as the FOREGROUND load
/// that contends for global read-pool slots alongside auto-link's own reads.
const BROAD_QUERY: &str = "engine subsystem discussion release notes deployment pipeline review";

fn foreground_search_opts() -> SearchOptions {
    SearchOptions {
        record_access: false,
        ..Default::default()
    }
}

/// Seed a small multi-topic corpus so the foreground W2-shaped query and
/// auto-link's own per-entity searches have real rows to scan — an empty
/// store would make every read trivially fast and defeat the point of a
/// saturation workload.
fn seed_corpus(server: &MemoryServer) {
    const TOPICS: &[&str] = &[
        "engine",
        "pipeline",
        "release",
        "deployment",
        "review",
        "subsystem",
        "notes",
        "discussion",
    ];
    for i in 0..200usize {
        let topic = TOPICS[i % TOPICS.len()];
        let mut entry = make_entry(&format!("w5-corpus-{i:04}"));
        entry.text = format!(
            "{topic} discussion of engine subsystem release notes deployment \
             pipeline review item {i}"
        );
        entry.path = format!("/w5-corpus/{topic}");
        entry.entities = vec![topic.to_string()];
        server
            .with_global_store(|store| store.upsert(&entry).map_err(|e| format!("seed: {e}")))
            .expect("seed corpus entry");
    }
}

/// Seed one pre-existing "target" entry and return one freshly-upserted
/// "fresh" entry engineered to trigger a `supersedes` edge against it: same
/// path (hence same path root), category `"fact"` (both via `make_entry`'s
/// default), 2 shared entities, and a newer timestamp — the exact gate
/// `should_supersede` checks. Mirrors the fixture shape in
/// `memory_search_ops::auto_link::tests::run_auto_linking_reports_live_read_write_and_total_timers`,
/// including upserting `fresh` itself before returning it (mirroring what
/// `save_memory` does before it calls `spawn_auto_linking`) — so
/// `write_elapsed`/`pool_checkout_wait` are never legitimately-zero
/// artifacts of "no edge was ever attempted" or "the entry didn't exist".
fn seed_supersede_pair(server: &MemoryServer, index: usize) -> MemoryEntry {
    let shared_a = format!("w5-entity-a-{index}");
    let shared_b = format!("w5-entity-b-{index}");

    let mut target = make_entry(&format!("w5-target-{index}"));
    target.text = format!("Original note about {shared_a} and {shared_b}");
    target.entities = vec![shared_a.clone(), shared_b.clone()];
    target.path = "/w5-saturation".to_string();
    target.timestamp = "2026-01-01T00:00:00Z".to_string();
    server
        .with_global_store(|store| {
            store
                .upsert(&target)
                .map_err(|e| format!("seed target: {e}"))
        })
        .expect("seed supersede target");

    let mut fresh = make_entry(&format!("w5-fresh-{index}-{}", uuid::Uuid::new_v4()));
    fresh.text = format!("New observation about {shared_a} and {shared_b}");
    fresh.entities = vec![shared_a, shared_b];
    fresh.path = "/w5-saturation".to_string();
    fresh.timestamp = "2026-01-02T00:00:00Z".to_string();
    server
        .with_global_store(|store| store.upsert(&fresh).map_err(|e| format!("seed fresh: {e}")))
        .expect("seed fresh entry");
    fresh
}

/// p50 / p95 / p99 by sort + index (`v[len*N/100]`), matching the S2
/// harness's convention (`crates/memcore/examples/receipts_workloads.rs`).
fn percentiles_us(mut values: Vec<u64>) -> (u64, u64, u64) {
    values.sort_unstable();
    let len = values.len();
    if len == 0 {
        return (0, 0, 0);
    }
    let p50 = values[len / 2];
    let p95 = values[(len * 95 / 100).min(len - 1)];
    let p99 = values[(len * 99 / 100).min(len - 1)];
    (p50, p95, p99)
}

/// Run `SAMPLES_PER_LEVEL` auto-link passes in waves of `concurrency` (never
/// more than `concurrency` in flight at once, since each wave is fully
/// drained before the next is spawned), beside `concurrency` foreground
/// W2-shaped readers contending for the same global read pool for the whole
/// duration. Returns every completed receipt; panics rather than silently
/// truncating if a spawned task never reports back — an unmeasured pass must
/// never collapse into "fewer samples than requested" being mistaken for "the
/// workload was smaller than it was".
async fn run_concurrency_level(server: &MemoryServer, concurrency: usize) -> Vec<AutoLinkReceipt> {
    let concurrency = concurrency.max(1);
    let stop = Arc::new(AtomicBool::new(false));
    let foreground_handles: Vec<_> = (0..concurrency)
        .map(|_| {
            let server = server.clone();
            let stop = Arc::clone(&stop);
            tokio::spawn(async move {
                while !stop.load(Ordering::Relaxed) {
                    let _ = server.with_global_store_read(|store| {
                        store
                            .search(BROAD_QUERY, Some(foreground_search_opts()))
                            .map_err(|e| e.to_string())
                    });
                    // Yield so the runtime can interleave with other ready
                    // tasks (including the measured auto-link tasks below)
                    // even when `concurrency` foreground readers exceed the
                    // worker thread count (e.g. concurrency == 2P).
                    tokio::task::yield_now().await;
                }
            })
        })
        .collect();

    let mut receipts = Vec::with_capacity(SAMPLES_PER_LEVEL);
    let mut minted = 0usize;
    while receipts.len() < SAMPLES_PER_LEVEL {
        let batch_size = concurrency.min(SAMPLES_PER_LEVEL - receipts.len());
        let (tx, rx) = std::sync::mpsc::channel();
        for _ in 0..batch_size {
            let entry = seed_supersede_pair(server, minted);
            spawn_auto_linking_for_test(
                server,
                &entry,
                DbScope::Global,
                None,
                minted as u64,
                tx.clone(),
            );
            minted += 1;
        }
        drop(tx);
        for _ in 0..batch_size {
            let (_, receipt) = rx.recv_timeout(std::time::Duration::from_secs(30)).expect(
                "every spawned auto-link task must report back — a missing \
                     receipt is an UNMEASURED pass, not a smaller sample size",
            );
            receipts.push(receipt);
        }
    }

    stop.store(true, Ordering::Relaxed);
    for handle in foreground_handles {
        let _ = handle.await;
    }

    receipts
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "load-test harness (tachi#1145 W5) — run explicitly with --ignored"]
async fn w5_auto_link_latency_under_saturation() {
    // `spawn_auto_linking_for_test` reads this env var (via
    // `spawn_and_run_auto_linking`) to decide whether to sample at all —
    // without it every receipt would be `None` and this workload would hang
    // waiting on a channel nothing ever sends to. `EnvRestore` holds no lock
    // (just owned data), so it is safe to keep alive across this test's many
    // `.await` points; see `spawn_queue_wait_measures_dispatch_delay_under_worker_contention`
    // in `memory_search_ops/auto_link.rs` for why a `std::sync::Mutex`-based
    // cross-test lock is NOT used here (`!Send` guard across `.await`).
    let _env = crate::test_support::EnvRestore::set("TACHI_AUTO_LINK_PHASE_RECEIPTS", "1");

    let server = make_server();
    seed_corpus(&server);
    let pool_size = server.global_read_pool_size_for_tests();

    for concurrency in concurrency_levels(pool_size) {
        let started = Instant::now();
        let receipts = run_concurrency_level(&server, concurrency).await;
        let wall = started.elapsed();

        let mut spawn_queue_us = Vec::with_capacity(receipts.len());
        let mut pool_wait_us = Vec::with_capacity(receipts.len());
        let mut search_us = Vec::with_capacity(receipts.len());
        let mut write_us = Vec::with_capacity(receipts.len());
        let mut unmeasured_pool_wait = 0usize;

        for receipt in &receipts {
            spawn_queue_us.push(receipt.spawn_queue_wait.as_micros() as u64);
            write_us.push(receipt.write_elapsed.as_micros() as u64);
            match receipt.pool_checkout_wait {
                LayerAvailability::Measured(pool_wait) => {
                    pool_wait_us.push(pool_wait.as_micros() as u64);
                    let search = receipt.read_elapsed.saturating_sub(pool_wait);
                    search_us.push(search.as_micros() as u64);
                }
                _ => unmeasured_pool_wait += 1,
            }
        }

        assert_eq!(
            unmeasured_pool_wait,
            0,
            "every receipt in this workload uses DbScope::Global with no \
             named project — the one path wired to the recording read call \
             — so pool_checkout_wait must be Measured on all {} samples; {} \
             came back unmeasured, which would silently undercount the \
             reported pool-wait/search percentiles below",
            receipts.len(),
            unmeasured_pool_wait
        );

        let (spawn_p50, spawn_p95, spawn_p99) = percentiles_us(spawn_queue_us);
        let (pool_p50, pool_p95, pool_p99) = percentiles_us(pool_wait_us);
        let (search_p50, search_p95, search_p99) = percentiles_us(search_us);
        let (write_p50, write_p95, write_p99) = percentiles_us(write_us);

        println!(
            "{{\"workload\":\"W5\",\"pool_size\":{pool_size},\"concurrency\":{concurrency},\
             \"samples\":{sample_count},\"wall_ms\":{wall_ms},\
             \"spawn_queue_wait_us\":{{\"p50\":{spawn_p50},\"p95\":{spawn_p95},\"p99\":{spawn_p99}}},\
             \"pool_checkout_wait_us\":{{\"p50\":{pool_p50},\"p95\":{pool_p95},\"p99\":{pool_p99}}},\
             \"search_us\":{{\"p50\":{search_p50},\"p95\":{search_p95},\"p99\":{search_p99}}},\
             \"write_us\":{{\"p50\":{write_p50},\"p95\":{write_p95},\"p99\":{write_p99}}}}}",
            sample_count = receipts.len(),
            wall_ms = wall.as_millis(),
        );
    }
}
