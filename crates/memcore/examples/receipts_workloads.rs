//! receipts_workloads.rs — tachi#1097 S2 receipt workload harness (W1–W4).
//!
//! Measurement harness for the S1 phase-attribution receipts. S1 (merged)
//! added `hybrid_search_with_receipt` / `SearchPhaseReceipt`; this example is
//! the S2 thing that *reads the stopwatches*: it runs four frozen workloads,
//! each as two paired timing series (the unsampled production path
//! `hybrid_search` vs the sampled path `hybrid_search_with_receipt`), and
//! prints one machine-readable JSON object per workload on stdout.
//!
//! # What this measures (and what it deliberately does NOT claim)
//!
//! The off-path cost this leaf owes (#1097 S1 deferred every zero-overhead
//! claim to S2) is the delta between the two series. The harness computes the
//! two series and prints their percentiles; it does NOT editorialize about
//! whether the delta is small. No adjective appears in the output or comments
//! — numbers only. The actual numbers exist only once the build seat runs
//! this binary; nothing in the source states or implies a measured value.
//!
//! `quality_paired` is the live discriminator that observation does not change
//! behavior: the SAME query is run once unsampled and once sampled and the two
//! returned entry-id sequences are compared. `true` only when identical, in
//! order. A `false` at run time would be a production bug found — which is why
//! it is computed, never assumed.
//!
//! # One deliberate, documented deviation from literal "defaults"
//!
//! The frozen workload table says W1–W3 use "defaults" and W4 uses "graph on".
//! `SearchOptions::default().record_access == true`. This harness overrides
//! exactly that one field to `false` for every W1–W4 call (timed runs AND the
//! quality pair). Reasons, stated plainly so the contract owner can reverse
//! the call by flipping one field if they prefer literal defaults:
//!
//!   1. `record_access: true` mutates `access_count` / `recall_count` on every
//!      iteration. That corrupts `quality_paired` as a clean discriminator of
//!      "observation does not change behavior": the unsampled and sampled pair
//!      calls would run on different DB state (the first bumps the rows the
//!      second then reads), so a `false` could be the access mutation, not a
//!      sampling effect — a false "bug found".
//!   2. The same drift shifts scoring across the 500-iteration series,
//!      adding noise that obscures the very sampled/unsampled delta this leaf
//!      owes.
//!   3. The repo's own eval harness (`search/tests/golden_corpus.rs`) sets
//!      `record_access: false` for these same reasons.
//!
//! The off-path-cost delta (sampled − unsampled) stays valid under either
//! setting because `record_access` is identical in both series. `W4` adds
//! `graph_expand_hops: 1` on top of the same override.
//!
//! # How to run (build seat)
//!
//!     cargo run -p memcore --example receipts_workloads --release
//!
//! `main` runs W1-W4 (4 JSONL objects) AND, unconditionally after them, the
//! #1142 symbolic-scan grid below (72 per-cell JSONL objects + 1 kill-test
//! summary line = 73 more) — **77 JSONL records total**, not four; there is
//! no separate invocation or flag that runs W1-W4 alone. `--release` is
//! mandatory for both: the grid's own corpora reach 63k rows and W1-W4's own
//! module doc already names the S2 debug/release incident (7x skew on
//! Rust-heavy phases) as the reason no timing conclusion from this binary is
//! valid without it. Diagnostics go to stderr so the stdout pipe stays
//! machine-readable JSONL.
//!
//! # tachi#1142 (S3-A) — symbolic-scan measurement-validity grid
//!
//! Extends this harness (per #1142's own instruction: extend, do not
//! duplicate) with a second, separate measurement: not the sampled/unsampled
//! tax of W1-W4, but how the SYMBOLIC channel's own cost scales, isolated
//! from the rest of hybrid_search. It calls `db::search_symbolic_candidates`
//! directly — the exact function #1154 rewrote (relevance-first pre-cap
//! eligibility) — across a rows × byte-distribution × selectivity × cache
//! grid, printing one JSONL object per cell plus one kill-test summary line.
//! See `run_symbolic_scan_grid` below for the full design rationale.
//!
//! The grid's `#[cfg(test)]` cells are deterministic (id/rank assertions
//! only, no timing) and do not need `--release`:
//! `cargo test -p memcore --example receipts_workloads`.

use memcore::{
    hybrid_search, hybrid_search_with_receipt, MemoryEdge, MemoryEntry, MemoryStore, SearchOptions,
    SearchPhaseReceipt,
};
use rusqlite::{params, Connection};
use serde_json::json;
use std::time::Instant;

/// `search_symbolic_candidates` is not re-exported at the crate root (only
/// its in-crate relevance-aware sibling is `pub(crate)`); this is its actual
/// public path.
use memcore::db::search_symbolic_candidates;

/// Warmup iterations per path (unsampled and sampled each get this many).
const WARMUP: usize = 50;
/// Measured iterations per path.
const MEASURED: usize = 500;
/// Length of the `supports` chain used by W4 (graph expansion).
const CHAIN_LEN: usize = 30;

/// Broad natural-language phrase (8 words) overlapping many of the 20 topic
/// clusters — no single entry owns all eight tokens, so the conjunctive FTS
/// query zeros and the OR-fallback rewards coverage. This is the W2 shape.
const BROAD_QUERY: &str = "runtime database memory network policy interface storage pipeline";

/// Pure-CJK phrase overlapping the 100-entry CJK corpus (libsimple
/// segmentation). This is the W3 shape, reusing `golden_corpus::Slice::Cjk`'s
/// vocabulary themes rather than minting a parallel CJK philosophy.
const CJK_QUERY: &str = "记忆系统召回检索通道融合排序";

/// Twenty synthetic English topics, each with a small vocabulary cluster.
/// 500 entries are spread across these (25 per topic) so conjunctive-FTS
/// misses on W2 have real distractors to lose to — same intent as
/// `golden_corpus`'s topical clusters.
const TOPICS: &[(&str, &[&str])] = &[
    (
        "runtime",
        &["scheduler", "executor", "task", "worker", "pool"],
    ),
    (
        "database",
        &["index", "query", "transaction", "schema", "cursor"],
    ),
    (
        "memory",
        &["recall", "decay", "activation", "retention", "weight"],
    ),
    (
        "network",
        &["socket", "packet", "latency", "protocol", "handshake"],
    ),
    (
        "security",
        &["cipher", "key", "vault", "secret", "credential"],
    ),
    (
        "interface",
        &["button", "layout", "render", "widget", "canvas"],
    ),
    (
        "policy",
        &["rule", "gate", "adjudication", "review", "doctrine"],
    ),
    (
        "cluster",
        &["node", "replica", "shard", "partition", "quorum"],
    ),
    (
        "storage",
        &["block", "segment", "wal", "checkpoint", "page"],
    ),
    (
        "pipeline",
        &["stage", "batch", "queue", "worker", "throughput"],
    ),
    (
        "daemon",
        &["process", "signal", "respawn", "watchdog", "reaper"],
    ),
    (
        "config",
        &["profile", "override", "default", "merge", "validate"],
    ),
    ("cache", &["eviction", "ttl", "hit", "miss", "invalidate"]),
    (
        "queue",
        &["producer", "consumer", "backpressure", "offset", "ack"],
    ),
    (
        "vector",
        &["embedding", "cosine", "knn", "dimension", "normalize"],
    ),
    ("token", &["lexer", "tokenizer", "ngram", "stem", "segment"]),
    (
        "schema",
        &["migration", "column", "constraint", "fkey", "index"],
    ),
    (
        "service",
        &["handler", "endpoint", "router", "middleware", "facade"],
    ),
    (
        "kernel",
        &["syscall", "trap", "scheduler", "interrupt", "context"],
    ),
    (
        "protocol",
        &["frame", "opcode", "session", "version", "negotiate"],
    ),
];

/// CJK sentence templates (themes reused from `golden_corpus`'s Chinese
/// cluster). `{j}` is substituted with the entry index so every entry is
/// byte-distinct while sharing vocabulary.
const CJK_TEMPLATES: &[&str] = &[
    "记忆系统的召回内核融合第{j}条通道并按倒数排名融合排序",
    "全文检索在第{j}组索引上要求每个词元都命中否则通道归零",
    "数据库迁移在事务中执行第{j}步失败时干净回滚不留损坏表",
    "守护进程在第{j}个交易时段绝不重启只在收盘后停止并重生",
    "向量存储把第{j}个稠密嵌入放进虚拟表并回答最近邻查询",
    "符号评分统计第{j}条查询词元在条目关键词实体中的出现比例",
    "质量乘数在第{j}次未限定范围的搜索里提高维基条目分数",
    "合取检索缺第{j}个关键词就让整条通道立刻归零无法召回",
    "中文查询第{j}号无法触发或回退因为词元过滤只放行纯英文",
    "倒排索引在第{j}个列上为快速全文检索建立映射与触发器",
];

fn main() {
    let mut store = MemoryStore::open_in_memory().expect("open in-memory store");
    seed_corpus(&mut store);
    // All timed calls are read-only over a stable corpus, so a shared
    // immutable borrow of the underlying connection is held for the rest of
    // the run.
    let conn: &Connection = store.connection();

    // W1 — short exact: one rare token hitting exactly one entry.
    run_workload(conn, "W1", "raretok0042", defaults_opts);
    // W2 — broad NL: 8-word phrase overlapping many topics (OR-fallback path).
    run_workload(conn, "W2", BROAD_QUERY, defaults_opts);
    // W3 — CJK phrase over the 100-entry Chinese corpus.
    run_workload(conn, "W3", CJK_QUERY, defaults_opts);
    // W4 — graph expansion: query hits the chain head, graph_expand_hops: 1.
    run_workload(conn, "W4", "chainhead", graph_opts);

    // Store-level wrapper consumption (see consumption checklist): the bare
    // hybrid_search_with_receipt receipt carries operation=HybridSearch /
    // database_scope=Unknown; MemoryStore::search_with_receipt upgrades both.
    // Diagnostics to stderr so stdout stays clean JSONL.
    demonstrate_store_wrapper(conn, &store);

    // tachi#1142 (S3-A) — symbolic-scan measurement-validity grid. Builds its
    // own fresh in-memory stores per cell (the fixed 500-English/100-CJK/
    // 30-chain corpus above can't host 0.63k-63k row tiers), independent of
    // `conn`/`store`.
    run_symbolic_scan_grid();
}

// ---------------------------------------------------------------------------
// Option makers. `SearchOptions` is NOT `Clone`, so every call site builds a
// fresh value. Each maker bakes in the documented `record_access: false`
// override (see module docs).
// ---------------------------------------------------------------------------

fn defaults_opts() -> SearchOptions {
    SearchOptions {
        record_access: false,
        ..Default::default()
    }
}

fn graph_opts() -> SearchOptions {
    SearchOptions {
        record_access: false,
        graph_expand_hops: 1,
        ..Default::default()
    }
}

// ---------------------------------------------------------------------------
// Workload driver
// ---------------------------------------------------------------------------

/// Run one frozen workload end-to-end and print its JSONL result.
///
/// `make_opts` is invoked once per search call (the struct is not `Clone`),
/// so warmup / measured / quality all build identical, fresh options.
fn run_workload(conn: &Connection, name: &str, query: &str, make_opts: fn() -> SearchOptions) {
    // Warmup both paths so caches / prepared statements stabilize before the
    // measured series. 50 per path.
    for _ in 0..WARMUP {
        let opts = make_opts();
        hybrid_search(conn, query, &opts).expect("warmup unsampled");
    }
    for _ in 0..WARMUP {
        let opts = make_opts();
        hybrid_search_with_receipt(conn, query, &opts).expect("warmup sampled");
    }

    // Measured series — unsampled production path. Per-iteration wall time of
    // the `hybrid_search` call only.
    let mut unsampled_us: Vec<u64> = Vec::with_capacity(MEASURED);
    for _ in 0..MEASURED {
        let opts = make_opts();
        let start = Instant::now();
        hybrid_search(conn, query, &opts).expect("measured unsampled");
        unsampled_us.push(start.elapsed().as_micros() as u64);
    }

    // Measured series — sampled path. Per-iteration wall time of the
    // `hybrid_search_with_receipt` call (receipt construction included — that
    // inclusion IS the off-path cost being measured). The last receipt is
    // retained for the digest.
    let mut sampled_us: Vec<u64> = Vec::with_capacity(MEASURED);
    let mut last_receipt: Option<SearchPhaseReceipt> = None;
    for _ in 0..MEASURED {
        let opts = make_opts();
        let start = Instant::now();
        let (_, receipt) =
            hybrid_search_with_receipt(conn, query, &opts).expect("measured sampled");
        sampled_us.push(start.elapsed().as_micros() as u64);
        last_receipt = Some(receipt);
    }

    let (u_p50, u_p95) = percentiles(&mut unsampled_us);
    let (s_p50, s_p95) = percentiles(&mut sampled_us);
    let quality_paired = quality_paired(conn, query, make_opts);
    let digest = build_receipt_digest(last_receipt.as_ref().expect("sampled receipt exists"));

    println!(
        "{{\"workload\":\"{name}\",\"iterations\":{MEASURED},\
         \"unsampled\":{{\"p50_us\":{u_p50},\"p95_us\":{u_p95}}},\
         \"sampled\":{{\"p50_us\":{s_p50},\"p95_us\":{s_p95}}},\
         \"quality_paired\":{quality_paired},\
         \"receipt_digest\":{digest}}}"
    );
}

/// p50 / p95 by sort + index, exactly as the contract specifies
/// (`v[len/2]`, `v[len*95/100]`). No external stats dependency.
fn percentiles(values: &mut [u64]) -> (u64, u64) {
    values.sort_unstable();
    let len = values.len();
    // len == MEASURED (500) -> indices 250 and 475. Defensive against an empty
    // slice only because this is a library-shaped helper.
    if len == 0 {
        return (0, 0);
    }
    let p50 = values[len / 2];
    let p95 = values[len * 95 / 100];
    (p50, p95)
}

/// Run the SAME query once unsampled and once sampled; `true` only when the
/// two returned entry-id sequences are identical, in order. Uses
/// `record_access: false` (via the maker) so neither call mutates state — the
/// two calls therefore run over identical DB state, isolating the sampling
/// effect (the thing this field discriminates) from access-count mutation.
fn quality_paired(conn: &Connection, query: &str, make_opts: fn() -> SearchOptions) -> bool {
    let opts = make_opts();
    let unsampled = hybrid_search(conn, query, &opts).expect("quality unsampled");
    let (sampled, _) = hybrid_search_with_receipt(conn, query, &opts).expect("quality sampled");
    let a: Vec<&str> = unsampled.iter().map(|r| r.entry.id.as_str()).collect();
    let b: Vec<&str> = sampled.iter().map(|r| r.entry.id.as_str()).collect();
    a == b
}

/// Hand-built JSON for one sampled receipt. The receipt family derives only
/// `Debug + Clone` (not `Serialize`), so it cannot go through `serde_json`;
/// the digest is assembled field by field. Durations render as integer
/// microseconds. Phase fields render as `present` flags + their elapsed; the
/// FTS group count is the per-workload receipt-digest signal the contract
/// names for W1–W4.
fn build_receipt_digest(r: &SearchPhaseReceipt) -> String {
    let total_us = r.total_elapsed.as_micros() as u64;
    let candidates_present = r.candidates.is_some();
    let candidates_us = r
        .candidates
        .as_ref()
        .map(|c| c.total_elapsed.as_micros() as u64)
        .unwrap_or(0);
    let fts_group_count = r
        .candidates
        .as_ref()
        .map(|c| c.fts_groups.len())
        .unwrap_or(0);
    let fts_candidate_count = r
        .candidates
        .as_ref()
        .map(|c| c.fts_candidate_count)
        .unwrap_or(0);
    let merged_candidate_count = r
        .candidates
        .as_ref()
        .map(|c| c.merged_candidate_count)
        .unwrap_or(0);
    let fetch_present = r.fetch.is_some();
    let fetch_us = r
        .fetch
        .as_ref()
        .map(|f| f.elapsed.as_micros() as u64)
        .unwrap_or(0);
    let rank_present = r.rank.is_some();
    let rank_us = r
        .rank
        .as_ref()
        .map(|rk| rk.total_elapsed.as_micros() as u64)
        .unwrap_or(0);
    let graph_present = r.graph_expansion.is_some();
    let graph_enabled = r
        .graph_expansion
        .as_ref()
        .map(|g| g.enabled)
        .unwrap_or(false);
    let graph_us = r
        .graph_expansion
        .as_ref()
        .map(|g| g.elapsed.as_micros() as u64)
        .unwrap_or(0);
    let access_present = r.access_recording.is_some();

    format!(
        "{{\"sampled\":{sampled},\"total_us\":{total_us},\
         \"candidates_present\":{candidates_present},\"candidates_us\":{candidates_us},\
         \"fts_group_count\":{fts_group_count},\"fts_candidate_count\":{fts_candidate_count},\
         \"merged_candidate_count\":{merged_candidate_count},\
         \"fetch_present\":{fetch_present},\"fetch_us\":{fetch_us},\
         \"rank_present\":{rank_present},\"rank_us\":{rank_us},\
         \"graph_present\":{graph_present},\"graph_enabled\":{graph_enabled},\"graph_us\":{graph_us},\
         \"access_recording_present\":{access_present},\
         \"pool_wait\":\"Unavailable\",\"sqlite_retry\":\"NotApplicable\"}}",
        sampled = r.sampled
    )
}

// ---------------------------------------------------------------------------
// Store-level wrapper consumption (API list item 3)
// ---------------------------------------------------------------------------

/// Exercises `MemoryStore::search_with_receipt` once and prints (to stderr)
/// how its receipt differs from the bare-connection receipt: the wrapper
/// upgrades `operation` to `MemoryStoreSearch` and stamps the store's
/// `db_label` into `database_scope`. This is a consumption check, not a
/// timed workload.
fn demonstrate_store_wrapper(conn: &Connection, store: &MemoryStore) {
    let bare = {
        let opts = SearchOptions {
            top_k: 3,
            record_access: false,
            ..Default::default()
        };
        let (_, receipt) =
            hybrid_search_with_receipt(conn, "raretok0042", &opts).expect("bare wrapper receipt");
        receipt
    };
    let wrapped = {
        let opts = SearchOptions {
            top_k: 3,
            record_access: false,
            ..Default::default()
        };
        let (_, receipt) = store
            .search_with_receipt("raretok0042", Some(opts))
            .expect("store wrapper receipt");
        receipt
    };
    eprintln!(
        "store-wrapper-check: bare op={:?} scope={:?} | wrapped op={:?} scope={:?}",
        bare.operation, bare.database_scope, wrapped.operation, wrapped.database_scope
    );
}

// ---------------------------------------------------------------------------
// Deterministic corpus (no files, no network, no Utc::now)
// ---------------------------------------------------------------------------

/// Seed the full corpus: 500 English entries across 20 topics, 100 CJK
/// entries, and a 30-entry `supports` chain for graph expansion. All ids /
/// texts derive from the index, so the corpus is byte-identical run to run.
fn seed_corpus(store: &mut MemoryStore) {
    // 500 English entries across 20 synthetic topics.
    for i in 0..500usize {
        let (name, vocab) = TOPICS[i % TOPICS.len()];
        let w1 = vocab[i % vocab.len()];
        let w2 = vocab[(i / TOPICS.len()) % vocab.len()];
        let raretok = format!("raretok{i:04}");
        let id = format!("entry-{i:04}");
        let text = format!("{name} discussion of {w1} and {w2} subsystem details {raretok}");
        let keywords = vec![name.to_string(), w1.to_string(), raretok];
        let entities = vec![name.to_string()];
        let entry = make_entry(
            &id,
            &text,
            &keywords,
            &entities,
            "/notes",
            ts_days_ago((i % 60) as i64 + 1),
        );
        store.upsert(&entry).expect("seed english entry");
    }

    // 100 CJK entries (themes reused from golden_corpus::Slice::Cjk).
    for j in 0..100usize {
        let tmpl = CJK_TEMPLATES[j % CJK_TEMPLATES.len()];
        let text = tmpl.replace("{j}", &j.to_string());
        let id = format!("cjk-{j:03}");
        let keywords = vec![format!("cjk-{j:03}")];
        let entry = make_entry(
            &id,
            &text,
            &keywords,
            &[],
            "/notes/zh",
            ts_days_ago((j % 50) as i64 + 1),
        );
        store.upsert(&entry).expect("seed cjk entry");
    }

    // 30-entry chain; chain[k] --supports--> chain[k+1]. The head owns the
    // rare token W4 queries so FTS surfaces it, then graph_expand_hops: 1
    // pulls in chain[1] (shape reused from tests/graph.rs).
    for k in 0..CHAIN_LEN {
        let id = format!("chain-{k:02}");
        let text = if k == 0 {
            "chainhead origin node start of the supports chain link zero".to_string()
        } else {
            format!("chain link node {k} reachable by graph expansion only")
        };
        let entry = make_entry(
            &id,
            &text,
            &["chain".to_string()],
            &[],
            "/notes/chain",
            ts_days_ago(k as i64 + 1),
        );
        store.upsert(&entry).expect("seed chain entry");
    }
    for k in 0..(CHAIN_LEN - 1) {
        let edge = MemoryEdge {
            source_id: format!("chain-{k:02}"),
            target_id: format!("chain-{:02}", k + 1),
            relation: "supports".to_string(),
            weight: 1.0,
            metadata: json!({}),
            created_at: String::new(),
            valid_from: String::new(),
            valid_to: None,
        };
        store.add_edge(&edge).expect("seed chain edge");
    }
}

/// Build a `MemoryEntry` with the workload-neutral defaults. Mirrors the
/// shape used by `search/tests.rs::memory_entry` and the auto_link test
/// `test_entry`, so the corpus matches existing fixture conventions.
fn make_entry(
    id: &str,
    text: &str,
    keywords: &[String],
    entities: &[String],
    path: &str,
    timestamp: String,
) -> MemoryEntry {
    MemoryEntry {
        id: id.to_string(),
        path: path.to_string(),
        summary: text.chars().take(40).collect(),
        text: text.to_string(),
        importance: 0.7,
        timestamp,
        valid_from: String::new(),
        valid_until: None,
        category: "fact".to_string(),
        topic: String::new(),
        keywords: keywords.to_vec(),
        persons: vec![],
        entities: entities.to_vec(),
        location: String::new(),
        source: "workload".to_string(),
        scope: "general".to_string(),
        archived: false,
        access_count: 0,
        last_access: None,
        revision: 1,
        vector: None,
        retention_policy: None,
        domain: None,
        metadata: json!({}),
        recall_count: 0,
        query_diversity: 0,
        tier: "raw".to_string(),
    }
}

/// Fixed-base timestamp `BASE − days`, identical run to run (never
/// `Utc::now()`). Same scheme as `golden_corpus::ts_days_ago`.
fn ts_days_ago(days: i64) -> String {
    let base = chrono::DateTime::parse_from_rfc3339("2026-06-01T00:00:00+00:00").expect("base ts");
    (base - chrono::Duration::days(days)).to_rfc3339()
}

// =============================================================================
// tachi#1142 (S3-A) — symbolic-scan measurement-validity grid
// =============================================================================
//
// The S2 evidence table's "396μs at 630 rows -> tens of ms at 63k" claim was
// downgraded to hypothesis: the timestamp-DESC LIMIT permits early exit on
// dense recent matches, and the channel materializes full `MemoryEntry` rows
// before ID-reduction, so the real cost axis may be bytes inspected, not row
// count. This grid measures `db::search_symbolic_candidates` directly (the
// public function #1154 rewrote for relevance-first pre-cap eligibility —
// this grid exercises exactly that new code path, not the pre-#1154 shape)
// across rows x byte-distribution x selectivity x cache-state, printing one
// JSONL object per cell plus a kill-test summary line.
//
// # The four axes
//
// - rows: 630 / 6,300 / 63,000 (the #1142 contract's 0.63k/6.3k/63k).
// - byte distribution: one-line (this file's existing ~80-byte shape),
//   production quantiles (a synthetic 60/30/10 short/medium/long mix — NOT
//   measured production telemetry, see `production_quantile_target_bytes`),
//   KB paragraphs (~1-3KB, uniform-ish).
// - selectivity: absent term (zero matches — the worst-case full-scan
//   baseline), newest-dense (small tied-relevance cohort at the head of
//   recency), oldest-target (single strongest match at the tail of
//   recency, behind >SYMBOLIC_LIMIT weaker decoys — this is #1144's own
//   kill-test re-run at grid scale), 200-recent-hit (a cohort of
//   relevance-TIED matches larger than SYMBOLIC_LIMIT, so the cap must bind
//   on the `julianday(timestamp) DESC` tie-break alone).
// - cache state: fresh (the literal first call ever made for that exact
//   query text on that connection — genuinely cannot be sampled more than
//   once by definition) vs warmed (`GRID_WARMUP` uninstrumented calls first,
//   matching this file's existing W1-W4 warmup convention, then
//   `GRID_MEASURED` timed calls with p50/p95).
//
// # Per-cell JSONL schema
//
// `{"workload","rows","byte_distribution","selectivity","cache_state",
//   "iterations","elapsed_us","p50_us","p95_us","candidate_count",
//   "expected_id","expected_present","hit","rank","query_plan"}`.
// `elapsed_us` is set only for `cache_state=fresh` (n=1, a percentile is
// undefined for one sample); `p50_us`/`p95_us` are set only for `warmed`.
// `expected_id` is the row this cell has a specific prediction about (null
// for absent_term, whose prediction is `candidate_count == 0`).
// `expected_present` states the prediction (true = must appear, false = must
// be evicted, null = not applicable); `hit`/`rank` state what was actually
// observed (`hit` = whether `expected_id` appeared, `rank` = its 0-indexed
// position if so). A cell is correct iff `hit == expected_present` (or, for
// absent_term, iff `candidate_count == 0`) — the grid reports every input,
// it does not editorialize about which cells are correct.
//
// # Kill-test
//
// One extra JSONL line after the 72 per-cell records: for the
// `production_quantiles` byte distribution (the grid's "realistic bytes"
// cross-section) and `cache_state=warmed`, the absent_term and
// recent_hit_200 warmed p50 at each of the three row tiers. If absent_term
// grows with rows while recent_hit_200 stays roughly flat (plateaus at
// `SYMBOLIC_LIMIT`-bound cost), the flat O(rows) extrapolation from the S2
// evidence table is disproved for the dense-recent shape — read directly off
// this line, not asserted by the harness.

/// Row-count tier axis: 0.63k / 6.3k / 63k, per the #1142 contract.
const GRID_ROW_COUNTS: [usize; 3] = [630, 6_300, 63_000];

/// Warmup iterations for a `CacheState::Warmed` cell. Smaller than this
/// file's W1-W4 `WARMUP` (50): this grid characterizes cost SHAPE across 72
/// cells, not W1-W4's sampling-tax precision goal — at the top row-count
/// tier, a 50-call warmup budget per cell multiplies the KB-paragraph
/// corpus's per-call table-scan cost 72 times over for no shape-relevant
/// benefit.
const GRID_WARMUP: usize = 5;
/// Measured iterations for a `CacheState::Warmed` cell.
const GRID_MEASURED: usize = 15;

/// Candidate cap passed to `search_symbolic_candidates`. Mirrors the REAL
/// production default: `SearchOptions::default().candidates_per_channel` is
/// 20 (`search.rs`), and `search/candidates.rs::collect_candidates` calls
/// the symbolic channel with
/// `n.saturating_mul(SYMBOLIC_CANDIDATE_MULTIPLIER=10).max(opts.top_k=6)` =
/// 200. This is also where the "200-recent-hit" selectivity name in the
/// #1142 contract comes from — it names this exact cap.
const SYMBOLIC_LIMIT: usize = 200;

/// Count of "oldest-target" decoy rows: newer than the target, sharing its
/// raw LIKE substring but not its tokenized word (see `seed_grid_corpus`).
/// Fixed above `SYMBOLIC_LIMIT` (200) so the pre-#1154/#1144 timestamp-first
/// cap would have evicted the target entirely.
const OLDTARGET_DECOY_COUNT: usize = 220;

/// Count of rows sharing the "densehit" token, seeded across the MOST
/// RECENT rows of the corpus. Exceeds `SYMBOLIC_LIMIT` so the cap must
/// truncate a group of rows that are relevance-TIED (a single shared token
/// scores every member 1.0 under `symbolic_score_fields`'s presence-only,
/// not field-count-weighted, formula — `scorer/text.rs`) and can only be
/// ordered by the `julianday(timestamp) DESC` tie-break.
const DENSE_HIT_COUNT: usize = 250;

/// Count of rows sharing the "newesthit" token — a small tied-relevance
/// cohort at the very head of the recency ordering, never truncated by
/// `SYMBOLIC_LIMIT`. Exists to give the grid a "recent and unambiguous"
/// selectivity distinct from `DENSE_HIT_COUNT`'s "recent and cap-boundary"
/// shape.
const NEWEST_HIT_COUNT: usize = 5;

/// A literal token never seeded anywhere in the grid corpus.
const ABSENT_TERM_QUERY: &str = "zqxabsentneverseeded9999";

/// Entry-byte-size axis. See the module-level doc for what each variant
/// means and how it is generated.
#[derive(Clone, Copy, Debug)]
enum ByteDistribution {
    OneLine,
    ProductionQuantiles,
    KbParagraphs,
}

impl ByteDistribution {
    const ALL: [ByteDistribution; 3] = [
        ByteDistribution::OneLine,
        ByteDistribution::ProductionQuantiles,
        ByteDistribution::KbParagraphs,
    ];

    fn label(self) -> &'static str {
        match self {
            ByteDistribution::OneLine => "one_line",
            ByteDistribution::ProductionQuantiles => "production_quantiles",
            ByteDistribution::KbParagraphs => "kb_paragraphs",
        }
    }
}

/// Query-selectivity axis. See the module-level doc for what each variant
/// means and how it is seeded.
#[derive(Clone, Copy, Debug)]
enum Selectivity {
    AbsentTerm,
    NewestDense,
    OldestTarget,
    RecentHit200,
}

impl Selectivity {
    const ALL: [Selectivity; 4] = [
        Selectivity::AbsentTerm,
        Selectivity::NewestDense,
        Selectivity::OldestTarget,
        Selectivity::RecentHit200,
    ];

    fn label(self) -> &'static str {
        match self {
            Selectivity::AbsentTerm => "absent_term",
            Selectivity::NewestDense => "newest_dense",
            Selectivity::OldestTarget => "oldest_target",
            Selectivity::RecentHit200 => "recent_hit_200",
        }
    }

    fn query(self) -> &'static str {
        match self {
            Selectivity::AbsentTerm => ABSENT_TERM_QUERY,
            Selectivity::NewestDense => "newesthit",
            Selectivity::OldestTarget => "oldtargetterm",
            Selectivity::RecentHit200 => "densehit",
        }
    }

    /// The specific row this selectivity has a prediction about. `None` for
    /// `AbsentTerm`, whose prediction (`candidate_count == 0`) is not about
    /// any one row.
    fn expected_id(self, markers: &GridMarkers) -> Option<String> {
        match self {
            Selectivity::AbsentTerm => None,
            Selectivity::NewestDense => Some(markers.newest_dense_id.clone()),
            Selectivity::OldestTarget => Some(markers.oldest_target_id.clone()),
            Selectivity::RecentHit200 => Some(markers.dense_hit_oldest_id.clone()),
        }
    }

    /// Whether `expected_id` is predicted to survive the cap. `None` for
    /// `AbsentTerm` (not applicable — see `expected_id`).
    fn expected_present(self) -> Option<bool> {
        match self {
            Selectivity::AbsentTerm => None,
            Selectivity::NewestDense => Some(true),
            Selectivity::OldestTarget => Some(true),
            Selectivity::RecentHit200 => Some(false),
        }
    }
}

/// Cache-state axis. See the module-level doc for the exact operationalization.
#[derive(Clone, Copy, Debug)]
enum CacheState {
    Fresh,
    Warmed,
}

impl CacheState {
    const ALL: [CacheState; 2] = [CacheState::Fresh, CacheState::Warmed];

    fn label(self) -> &'static str {
        match self {
            CacheState::Fresh => "fresh",
            CacheState::Warmed => "warmed",
        }
    }
}

/// Ids of the marker rows `seed_grid_corpus` planted, so the grid can check
/// `expected_id`/`expected_present` against what `search_symbolic_candidates`
/// actually returns.
struct GridMarkers {
    oldest_target_id: String,
    newest_dense_id: String,
    dense_hit_oldest_id: String,
    /// Observed distribution of the `text` column's byte length across the
    /// corpus just seeded — #1142's invariant requires searchable bytes to
    /// be stated, not just the `byte_distribution` label bucket name
    /// (`text` dominates the LIKE-scanned column set: `summary` is a
    /// 40-char truncation of it, `keywords`/`entities`/`path`/`topic` are
    /// short and near-constant per row — see `make_entry`).
    searchable_bytes: SearchableByteStats,
}

/// `count`/`min`/`max`/`p50`/`p95` of per-row `text`-column byte length for
/// one seeded corpus. `p50`/`p95` reuse the same nearest-rank method as the
/// timing `percentiles` helper (sorted index `len/2` / `len*95/100`) — same
/// shape, different unit (bytes, not microseconds).
struct SearchableByteStats {
    count: usize,
    min_bytes: u64,
    max_bytes: u64,
    p50_bytes: u64,
    p95_bytes: u64,
}

/// Build one grid corpus: `rows` entries at the given `dist`, with marker
/// rows for all four `Selectivity` variants seeded in. Filler rows reuse
/// this file's `TOPICS` vocabulary (same intent as the W1-W4 corpus: real
/// topical distractors, not an isolated synthetic vocabulary).
fn seed_grid_corpus(store: &mut MemoryStore, rows: usize, dist: ByteDistribution) -> GridMarkers {
    assert!(
        rows > OLDTARGET_DECOY_COUNT + DENSE_HIT_COUNT,
        "grid row-count tier must be large enough to hold non-overlapping marker zones"
    );
    let dense_hit_start = rows - DENSE_HIT_COUNT;
    let newest_hit_start = rows - NEWEST_HIT_COUNT;
    let mut text_bytes: Vec<u64> = Vec::with_capacity(rows);

    for i in 0..rows {
        let (name, vocab) = TOPICS[i % TOPICS.len()];
        let w1 = vocab[i % vocab.len()];
        let w2 = vocab[(i / TOPICS.len()) % vocab.len()];
        let raretok = format!("gridtok{i:06}");
        let id = format!("grid-{i:06}");
        let mut text = filler_text(dist, i, name, w1, w2, &raretok);
        let mut keywords = vec![name.to_string(), w1.to_string()];
        let entities = vec![name.to_string()];

        // Ordinary filler recency: safely between the oldest-target's
        // far-past marker and the dense/newest zones' true-recent markers.
        // Ordering among filler rows themselves is not load-bearing for any
        // of the four selectivity predictions below.
        let mut days_ago: i64 = 1_000 + (i % 400) as i64;

        if i == 0 {
            // oldest-target: a unique token, far in the past. See
            // `OLDTARGET_DECOY_COUNT`'s doc for why this must survive the cap
            // under the current (post-#1154) relevance-first ordering.
            text.push_str(" oldtargetterm");
            keywords.push("oldtargetterm".to_string());
            days_ago = 100_000;
        } else if i <= OLDTARGET_DECOY_COUNT {
            // Decoy: shares the raw LIKE substring but not the tokenized
            // word — `scorer::tokenize` splits only on non-alphanumeric
            // boundaries, so "oldtargettermish" is one token, distinct from
            // "oldtargetterm". All strictly newer than the target.
            text.push_str(" oldtargettermish");
            days_ago = i as i64;
        }

        if i >= dense_hit_start {
            text.push_str(" densehit");
            keywords.push("densehit".to_string());
            days_ago = (rows - i) as i64;
        }

        if i >= newest_hit_start {
            text.push_str(" newesthit");
            keywords.push("newesthit".to_string());
            days_ago = (rows - i) as i64;
        }

        text_bytes.push(text.len() as u64);

        let entry = make_entry(
            &id,
            &text,
            &keywords,
            &entities,
            "/grid",
            ts_days_ago(days_ago),
        );
        store.upsert(&entry).expect("seed grid entry");
    }

    let searchable_bytes = summarize_byte_lengths(&mut text_bytes);

    GridMarkers {
        oldest_target_id: "grid-000000".to_string(),
        newest_dense_id: format!("grid-{:06}", rows - 1),
        dense_hit_oldest_id: format!("grid-{dense_hit_start:06}"),
        searchable_bytes,
    }
}

/// `min`/`max`/`p50`/`p95` of `values` (per-row `text` byte lengths). Reuses
/// `percentiles`'s nearest-rank method so the byte-length percentiles are
/// computed identically to the timing ones. `values` is non-empty here: the
/// same `assert!` above (`rows > OLDTARGET_DECOY_COUNT + DENSE_HIT_COUNT`)
/// guarantees at least one row was seeded.
fn summarize_byte_lengths(values: &mut [u64]) -> SearchableByteStats {
    let count = values.len();
    let (p50_bytes, p95_bytes) = percentiles(values);
    // `percentiles` already sorted `values` in place.
    let min_bytes = *values.first().unwrap_or(&0);
    let max_bytes = *values.last().unwrap_or(&0);
    SearchableByteStats {
        count,
        min_bytes,
        max_bytes,
        p50_bytes,
        p95_bytes,
    }
}

/// Entry text for one filler row at the given byte distribution.
fn filler_text(
    dist: ByteDistribution,
    i: usize,
    name: &str,
    w1: &str,
    w2: &str,
    raretok: &str,
) -> String {
    let base = format!("{name} discussion of {w1} and {w2} subsystem details {raretok}");
    match dist {
        ByteDistribution::OneLine => base,
        ByteDistribution::ProductionQuantiles => {
            pad_to_bytes(&base, production_quantile_target_bytes(i), i)
        }
        ByteDistribution::KbParagraphs => pad_to_bytes(&base, kb_paragraph_target_bytes(i), i),
    }
}

/// Synthetic three-bucket size mix (60% short / 30% medium / 10% long).
/// This is NOT sourced from measured production telemetry — this repo has
/// none to cite for entry-text length — it is chosen to give the grid's
/// "production quantiles" cell a realistic mixed-size shape instead of a
/// uniform one. Flagged here so a reader does not mistake it for a measured
/// distribution.
fn production_quantile_target_bytes(i: usize) -> usize {
    match bucket_pct(i) {
        0..=59 => 180,
        60..=89 => 650,
        _ => 2_600,
    }
}

/// ~1-3KB, spread deterministically by row index so the corpus is not one
/// uniform byte count.
fn kb_paragraph_target_bytes(i: usize) -> usize {
    1_024 + (bucket_pct(i) as usize) * 20
}

/// Deterministic 0..100 spread from a row index (Knuth multiplicative hash,
/// `2_654_435_761 = 2^32 / golden ratio`) — no RNG dependency, byte-identical
/// run to run.
fn bucket_pct(i: usize) -> u32 {
    ((i as u64).wrapping_mul(2_654_435_761) >> 16) as u32 % 100
}

/// Pad `base` up to (at least) `target_bytes` by repeating an index-salted
/// filler sentence. Byte-identical run to run; never shrinks `base`.
fn pad_to_bytes(base: &str, target_bytes: usize, i: usize) -> String {
    if base.len() >= target_bytes {
        return base.to_string();
    }
    let filler_sentence = format!(
        " additional context sentence {i} elaborates further on the subsystem \
         behavior and edge cases observed during review"
    );
    let mut out = String::with_capacity(target_bytes + filler_sentence.len());
    out.push_str(base);
    while out.len() < target_bytes {
        out.push_str(&filler_sentence);
    }
    out
}

/// Structural mirror of the WHERE/ORDER BY shape in
/// `crates/memcore/src/db/memory_crud/search.rs`
/// (`search_symbolic_candidates_with_relevance`, as of this commit — hand-
/// check both if that function's shape moves; it is `pub(crate)` and its SQL
/// is built inline, so an example cannot call into it directly to extract
/// the real statement). Two deliberate simplifications from the real
/// statement, neither of which changes SQLite's chosen access strategy (SCAN
/// vs SEARCH; sorted vs unsorted), which is all `EXPLAIN QUERY PLAN` reports:
///
///   1. `SELECT id` instead of the full column list — column selection does
///      not affect the WHERE/ORDER BY strategy on a table with no relevant
///      secondary index over these text columns.
///   2. One representative term OR-clause (`?5`) instead of up to 12 —
///      SQLite's strategy for N OR'd unanchored `LIKE '%...%'` predicates
///      across non-indexed columns is the same for any N >= 1; more terms
///      change per-row work, not per-row access strategy.
///
/// `tachi_symbolic_score` must already be registered on `conn` — it is a
/// side effect of any prior `search_symbolic_candidates` call, and every
/// grid cell makes one before this runs.
const SYMBOLIC_SCAN_MIRROR_SQL: &str = r#"SELECT id FROM memories
 WHERE (?1 = 1 OR archived = 0)
   AND (?2 = 1 OR superseded_by IS NULL)
   AND (?3 IS NULL OR path LIKE ?3)
   AND (?4 IS NULL OR (COALESCE(NULLIF(valid_from, ''), timestamp) <= ?4 AND (valid_until IS NULL OR valid_until > ?4)))
   AND id NOT LIKE 'anchor:%'
   AND (id LIKE ?5 ESCAPE '\' OR path LIKE ?5 ESCAPE '\' OR summary LIKE ?5 ESCAPE '\' OR text LIKE ?5 ESCAPE '\' OR keywords LIKE ?5 ESCAPE '\' OR entities LIKE ?5 ESCAPE '\' OR topic LIKE ?5 ESCAPE '\')
 ORDER BY tachi_symbolic_score(?6, id, path, topic, summary, text, keywords, entities) DESC, julianday(timestamp) DESC, id ASC
 LIMIT ?7"#;

/// `EXPLAIN QUERY PLAN` rows (the `detail` column only) for
/// `SYMBOLIC_SCAN_MIRROR_SQL`. Same extraction pattern as
/// `crates/memcore/src/db/migrations.rs`'s `query_plan` test helper
/// (`row.get::<_, String>(3)` — modern SQLite's `EXPLAIN QUERY PLAN` result
/// columns are `id, parent, notused, detail`).
///
/// `SYMBOLIC_SCAN_MIRROR_SQL` declares 7 positional placeholders (`?1..?7`)
/// mirroring `search_symbolic_candidates_with_relevance`'s real bind list
/// (`crates/memcore/src/db/memory_crud/search.rs:259-285`); rusqlite 0.38
/// rejects a placeholder-count mismatch, so this binds one representative,
/// correctly-typed value per slot (matching that function's actual types:
/// `?1`/`?2` archived/superseded flags as `i64`, `?3`/`?4` optional
/// path/as-of filters left `NULL` to exercise the `IS NULL OR ...`
/// short-circuit branch, `?5` a LIKE pattern, `?6` the relevance-scorer
/// query text, `?7` the `LIMIT`). `EXPLAIN QUERY PLAN` reports the access
/// strategy SQLite would choose for this shape; it does not execute the
/// query body, so the bound values only need to type-check, not encode a
/// real query.
fn symbolic_scan_mirror_query_plan(conn: &Connection) -> Vec<String> {
    let mut stmt = conn
        .prepare(&format!("EXPLAIN QUERY PLAN {SYMBOLIC_SCAN_MIRROR_SQL}"))
        .expect("prepare mirror EXPLAIN QUERY PLAN");
    let rows = stmt
        .query_map(
            params![
                0i64,
                0i64,
                Option::<String>::None,
                Option::<String>::None,
                "%x%",
                "x",
                1i64,
            ],
            |row| row.get::<_, String>(3),
        )
        .expect("run mirror EXPLAIN QUERY PLAN");
    rows.collect::<Result<Vec<_>, _>>()
        .expect("collect mirror EXPLAIN QUERY PLAN rows")
}

/// Run the full 3x3x4x2 grid; prints 72 per-cell JSONL objects plus one
/// kill-test summary line. See the module-level doc for the full design.
fn run_symbolic_scan_grid() {
    // Growth tracking for the kill-test cross-section: warmed p50, at
    // byte_distribution == production_quantiles (the grid's "realistic
    // bytes" cell), across the three row-count tiers.
    let mut absent_term_growth: [Option<u64>; 3] = [None; 3];
    let mut recent_hit_growth: [Option<u64>; 3] = [None; 3];

    for (row_idx, &rows) in GRID_ROW_COUNTS.iter().enumerate() {
        for dist in ByteDistribution::ALL {
            eprintln!(
                "grid: building corpus rows={rows} byte_distribution={}",
                dist.label()
            );
            let mut store = MemoryStore::open_in_memory().expect("open grid store");
            let markers = seed_grid_corpus(&mut store, rows, dist);
            let conn: &Connection = store.connection();

            let mut cells: Vec<serde_json::Value> = Vec::with_capacity(8);
            for selectivity in Selectivity::ALL {
                let query = selectivity.query();
                let expected_id = selectivity.expected_id(&markers);
                let expected_present = selectivity.expected_present();

                for cache in CacheState::ALL {
                    let (iterations, elapsed_us, p50_us, p95_us, entries) =
                        measure_symbolic_cell(conn, query, cache);

                    let ids: Vec<&str> = entries.iter().map(|e| e.id.as_str()).collect();
                    let (hit, rank) = match &expected_id {
                        None => (false, None),
                        Some(target) => {
                            let rank = ids.iter().position(|id| *id == target.as_str());
                            (rank.is_some(), rank)
                        }
                    };

                    if matches!(dist, ByteDistribution::ProductionQuantiles)
                        && matches!(cache, CacheState::Warmed)
                    {
                        if let Some(p50) = p50_us {
                            match selectivity {
                                Selectivity::AbsentTerm => absent_term_growth[row_idx] = Some(p50),
                                Selectivity::RecentHit200 => recent_hit_growth[row_idx] = Some(p50),
                                _ => {}
                            }
                        }
                    }

                    cells.push(json!({
                        "workload": "S3-A-grid",
                        "rows": rows,
                        "byte_distribution": dist.label(),
                        // #1142's invariant: "searchable bytes" must be
                        // observable per conclusion, not just the
                        // distribution-bucket label above — the actual
                        // per-row `text` byte-length distribution this
                        // corpus was seeded with.
                        "searchable_bytes": {
                            "count": markers.searchable_bytes.count,
                            "min": markers.searchable_bytes.min_bytes,
                            "max": markers.searchable_bytes.max_bytes,
                            "p50": markers.searchable_bytes.p50_bytes,
                            "p95": markers.searchable_bytes.p95_bytes,
                        },
                        "selectivity": selectivity.label(),
                        "cache_state": cache.label(),
                        "iterations": iterations,
                        "elapsed_us": elapsed_us,
                        "p50_us": p50_us,
                        "p95_us": p95_us,
                        "candidate_count": ids.len(),
                        "expected_id": expected_id,
                        "expected_present": expected_present,
                        "hit": hit,
                        "rank": rank,
                    }));
                }
            }

            // Captured once per corpus, not once per cell: the mirror SQL is
            // selectivity/cache-agnostic (unbound placeholders), so the plan
            // it reports is the same for every cell in this corpus. Re-run
            // per (rows, byte_distribution) rather than truly globally,
            // because SQLite's row-count estimate CAN in principle steer its
            // strategy at very different table sizes even without ANALYZE.
            let plan = symbolic_scan_mirror_query_plan(conn);
            for mut cell in cells {
                cell["query_plan"] = json!(plan);
                println!("{cell}");
            }
        }
    }

    println!(
        "{}",
        json!({
            "kill_test": "rows_scaling_at_production_quantiles_warmed_p50",
            "rows": GRID_ROW_COUNTS,
            "absent_term_p50_us": absent_term_growth,
            "recent_hit_200_p50_us": recent_hit_growth,
        })
    );
}

/// Run one (query, cache_state) cell. Returns
/// `(iterations, elapsed_us, p50_us, p95_us, last_result)` — `elapsed_us` is
/// `Some` only for `Fresh` (n=1), `p50_us`/`p95_us` only for `Warmed`.
fn measure_symbolic_cell(
    conn: &Connection,
    query: &str,
    cache: CacheState,
) -> (
    usize,
    Option<u64>,
    Option<u64>,
    Option<u64>,
    Vec<MemoryEntry>,
) {
    match cache {
        CacheState::Fresh => {
            // `conn` is reused across all 4 selectivities x 2 cache states
            // for this (rows, byte_distribution) corpus (opening a fresh
            // in-memory `MemoryStore` per selectivity would multiply corpus
            // -seeding cost 4x, including at the 63k-row tier). Without this,
            // only the very first selectivity's Fresh cell in a corpus is
            // genuinely fresh: every later "fresh" call inherits SQLite's
            // page cache warmed by the prior selectivity's own Warmed loop
            // (both scan the same `memories` table). `PRAGMA shrink_memory`
            // releases the connection's cached pages
            // (`sqlite3_db_release_memory`) with no live cursor held open at
            // this point (the previous cell's `query_map` fully drained and
            // returned owned `MemoryEntry` rows), giving each Fresh cell an
            // empty page cache regardless of what ran before it on this
            // connection.
            conn.execute_batch("PRAGMA shrink_memory;")
                .expect("shrink_memory before fresh symbolic scan");
            let start = Instant::now();
            let entries =
                search_symbolic_candidates(conn, query, SYMBOLIC_LIMIT, false, false, None, None)
                    .expect("fresh symbolic scan");
            let elapsed_us = start.elapsed().as_micros() as u64;
            (1, Some(elapsed_us), None, None, entries)
        }
        CacheState::Warmed => {
            for _ in 0..GRID_WARMUP {
                search_symbolic_candidates(conn, query, SYMBOLIC_LIMIT, false, false, None, None)
                    .expect("warmup symbolic scan");
            }
            let mut samples: Vec<u64> = Vec::with_capacity(GRID_MEASURED);
            let mut last_entries = Vec::new();
            for _ in 0..GRID_MEASURED {
                let start = Instant::now();
                let entries = search_symbolic_candidates(
                    conn,
                    query,
                    SYMBOLIC_LIMIT,
                    false,
                    false,
                    None,
                    None,
                )
                .expect("measured symbolic scan");
                samples.push(start.elapsed().as_micros() as u64);
                last_entries = entries;
            }
            let (p50, p95) = percentiles(&mut samples);
            (GRID_MEASURED, None, Some(p50), Some(p95), last_entries)
        }
    }
}

#[cfg(test)]
mod grid_tests {
    use super::*;

    /// Discriminating check for tachi#1142's oldest-target selectivity.
    ///
    /// Structural-discrimination justification (this lane cannot run
    /// `cargo` to demonstrate red-then-green against a reverted #1154/#1144;
    /// AGENTS.md's frozen-assertion law permits a stated structural
    /// justification in that case): pre-#1154/#1144,
    /// `search_symbolic_candidates` capped by `ORDER BY timestamp DESC
    /// LIMIT` BEFORE any relevance scoring. This corpus seeds the sole
    /// "oldtargetterm" match at the single OLDEST row (`days_ago =
    /// 100_000`) behind `OLDTARGET_DECOY_COUNT` (220, > the 200 cap)
    /// strictly-newer decoy rows that share the LIKE substring but not the
    /// tokenized word. Under the old timestamp-first cap, the 200 most
    /// recent decoys would fill the entire LIMIT and the target would never
    /// enter the result set (a false miss — exactly #1144's bug). Under the
    /// current relevance-first cap (`search.rs`'s
    /// `search_symbolic_candidates_with_relevance`, `ORDER BY
    /// tachi_symbolic_score(...) DESC, julianday(timestamp) DESC, id ASC`),
    /// the target's nonzero token-overlap score (1.0) strictly beats every
    /// decoy's zero score (`oldtargettermish` tokenizes as one word, via
    /// `scorer::tokenize`'s split-on-non-alphanumeric-only rule, distinct
    /// from `oldtargetterm`), so it survives regardless of recency.
    #[test]
    fn oldest_target_survives_the_relevance_first_cap() {
        let rows = GRID_ROW_COUNTS[0];
        let mut store = MemoryStore::open_in_memory().expect("open grid test store");
        let markers = seed_grid_corpus(&mut store, rows, ByteDistribution::OneLine);
        let conn = store.connection();

        let results = search_symbolic_candidates(
            conn,
            Selectivity::OldestTarget.query(),
            SYMBOLIC_LIMIT,
            false,
            false,
            None,
            None,
        )
        .expect("oldest-target symbolic scan");

        assert!(
            !results.is_empty(),
            "oldest-target query returned zero candidates; the target row was evicted"
        );
        assert_eq!(
            results[0].id, markers.oldest_target_id,
            "oldest-target's unique token match must outrank all {OLDTARGET_DECOY_COUNT} \
             decoys (zero token overlap each) regardless of recency"
        );
    }

    /// Structural-discrimination justification (cross-vendor review
    /// checkpoint 3): this does NOT discriminate #1154's relevance-first
    /// ordering — an always-empty or otherwise-broken symbolic scan would
    /// also pass. What it does discriminate: a WHERE-clause regression that
    /// makes the LIKE match-set too broad (e.g. an accidentally-satisfied
    /// predicate, or a term-extraction bug that treats an absent term as
    /// present) — a real bug class shared by both #1144's cap and #1154's
    /// relevance ordering, since both operate on this query's match-set.
    #[test]
    fn absent_term_returns_no_candidates() {
        let rows = GRID_ROW_COUNTS[0];
        let mut store = MemoryStore::open_in_memory().expect("open grid test store");
        seed_grid_corpus(&mut store, rows, ByteDistribution::OneLine);
        let conn = store.connection();

        let results = search_symbolic_candidates(
            conn,
            Selectivity::AbsentTerm.query(),
            SYMBOLIC_LIMIT,
            false,
            false,
            None,
            None,
        )
        .expect("absent-term symbolic scan");

        assert!(
            results.is_empty(),
            "a token seeded nowhere in the corpus must not match any row, got {} rows",
            results.len()
        );
    }

    /// Structural-discrimination justification (cross-vendor review
    /// checkpoint 3): every `NEWEST_HIT_COUNT` row ties at relevance score
    /// 1.0 for this single-token query, and this fixture's row IDs increase
    /// monotonically with recency (`grid-{i:06}`, `days_ago` decreasing in
    /// `i`) — so a naive "timestamp-DESC only, no relevance scoring at all"
    /// implementation would produce the identical top result and this test
    /// would NOT catch its absence of #1154's relevance-first ordering. What
    /// it does discriminate: that ties are broken by `julianday(timestamp)
    /// DESC` (parsed-instant comparison, not raw-TEXT string comparison —
    /// tachi#718 CP2, tachi#1144) and that `symbolic_score_fields` scores
    /// token PRESENCE, not field-count (`scorer/text.rs`) — both are real,
    /// independently-breakable behaviors this query's match-set exercises.
    /// `oldest_target_survives_the_relevance_first_cap` above is this file's
    /// one test that actually discriminates #1154's relevance-first cap
    /// itself (decoys are strictly more recent than the target, so recency
    /// alone would evict it).
    #[test]
    fn newest_dense_ranks_first_by_recency_tiebreak() {
        let rows = GRID_ROW_COUNTS[0];
        let mut store = MemoryStore::open_in_memory().expect("open grid test store");
        let markers = seed_grid_corpus(&mut store, rows, ByteDistribution::OneLine);
        let conn = store.connection();

        let results = search_symbolic_candidates(
            conn,
            Selectivity::NewestDense.query(),
            SYMBOLIC_LIMIT,
            false,
            false,
            None,
            None,
        )
        .expect("newest-dense symbolic scan");

        assert!(
            !results.is_empty(),
            "newest-dense query returned zero candidates"
        );
        assert_eq!(
            results[0].id, markers.newest_dense_id,
            "all NEWEST_HIT_COUNT rows tie at score 1.0 for a single-token query \
             (symbolic_score_fields counts token PRESENCE, not which/how many \
             fields carry it — scorer/text.rs); the most recent must win the \
             julianday(timestamp) DESC tie-break"
        );
    }

    /// Structural-discrimination justification (cross-vendor review
    /// checkpoint 3): `DENSE_HIT_COUNT` rows all tie at relevance score 1.0
    /// for this single-token query, so — same confound as
    /// `newest_dense_ranks_first_by_recency_tiebreak` above — a naive
    /// "timestamp-DESC only" implementation evicts the same oldest member
    /// this test expects, and would NOT be caught by this assertion as
    /// missing #1154's relevance-first ordering. What it does discriminate:
    /// that `SYMBOLIC_LIMIT` (#1144's cap) is actually enforced
    /// (`results.len() == SYMBOLIC_LIMIT`, not the unbounded match-set) and
    /// that the tie-break evicts the LEAST-, not most-, recent tied member
    /// (a reversed-comparison regression in the `julianday(timestamp) DESC`
    /// ordering would flip which end of the tied group survives).
    #[test]
    fn recent_hit_200_evicts_the_least_recent_tied_member() {
        let rows = GRID_ROW_COUNTS[0];
        let mut store = MemoryStore::open_in_memory().expect("open grid test store");
        let markers = seed_grid_corpus(&mut store, rows, ByteDistribution::OneLine);
        let conn = store.connection();

        let results = search_symbolic_candidates(
            conn,
            Selectivity::RecentHit200.query(),
            SYMBOLIC_LIMIT,
            false,
            false,
            None,
            None,
        )
        .expect("recent-hit-200 symbolic scan");

        assert_eq!(
            results.len(),
            SYMBOLIC_LIMIT,
            "DENSE_HIT_COUNT ({DENSE_HIT_COUNT}) exceeds SYMBOLIC_LIMIT \
             ({SYMBOLIC_LIMIT}); the cap must bind"
        );
        assert!(
            !results.iter().any(|e| e.id == markers.dense_hit_oldest_id),
            "the least-recent of {DENSE_HIT_COUNT} relevance-tied rows must lose the \
             julianday(timestamp) DESC tie-break and fall outside the {SYMBOLIC_LIMIT} cap"
        );
    }

    /// Scope note (cross-vendor review checkpoint 2): this only asserts the
    /// SCAN-vs-SEARCH access strategy (`EXPLAIN QUERY PLAN`'s `detail`
    /// column contains `"SCAN memories"`), not the full `ORDER BY` sort
    /// order or the real statement's up-to-12-term OR-clause shape. If
    /// `search.rs`'s WHERE/ORDER BY shape drifts in a way that changes the
    /// sort strategy but not the SCAN-vs-SEARCH choice, this test stays
    /// green on the stale mirror — see `SYMBOLIC_SCAN_MIRROR_SQL`'s doc for
    /// why the two known simplifications (single-column SELECT, one
    /// representative term) don't themselves change either.
    #[test]
    fn mirror_query_plan_shows_no_index_can_serve_this_query() {
        let mut store = MemoryStore::open_in_memory().expect("open grid test store");
        seed_grid_corpus(&mut store, GRID_ROW_COUNTS[0], ByteDistribution::OneLine);
        let conn = store.connection();
        // Registers `tachi_symbolic_score` as a side effect (see
        // `SYMBOLIC_SCAN_MIRROR_SQL`'s doc).
        search_symbolic_candidates(
            conn,
            "oldtargetterm",
            SYMBOLIC_LIMIT,
            false,
            false,
            None,
            None,
        )
        .expect("register scorer fn via a real symbolic scan");

        let plan = symbolic_scan_mirror_query_plan(conn);
        assert!(
            plan.iter().any(|line| line.contains("SCAN memories")),
            "expected a full table scan (unanchored LIKE across non-indexed \
             columns cannot use an index), got: {plan:?}"
        );
    }
}
