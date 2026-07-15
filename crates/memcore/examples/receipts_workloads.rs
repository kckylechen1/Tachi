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
//!     cargo run -p memcore --example receipts_workloads
//!
//! Prints exactly four JSONL objects to stdout (W1, W2, W3, W4). Diagnostics
//! go to stderr so the stdout pipe stays machine-readable.

use memcore::{
    hybrid_search, hybrid_search_with_receipt, MemoryEdge, MemoryEntry, MemoryStore, SearchOptions,
    SearchPhaseReceipt,
};
use rusqlite::Connection;
use serde_json::json;
use std::time::Instant;

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
    ("runtime", &["scheduler", "executor", "task", "worker", "pool"]),
    ("database", &["index", "query", "transaction", "schema", "cursor"]),
    ("memory", &["recall", "decay", "activation", "retention", "weight"]),
    ("network", &["socket", "packet", "latency", "protocol", "handshake"]),
    ("security", &["cipher", "key", "vault", "secret", "credential"]),
    ("interface", &["button", "layout", "render", "widget", "canvas"]),
    ("policy", &["rule", "gate", "adjudication", "review", "doctrine"]),
    ("cluster", &["node", "replica", "shard", "partition", "quorum"]),
    ("storage", &["block", "segment", "wal", "checkpoint", "page"]),
    ("pipeline", &["stage", "batch", "queue", "worker", "throughput"]),
    ("daemon", &["process", "signal", "respawn", "watchdog", "reaper"]),
    ("config", &["profile", "override", "default", "merge", "validate"]),
    ("cache", &["eviction", "ttl", "hit", "miss", "invalidate"]),
    ("queue", &["producer", "consumer", "backpressure", "offset", "ack"]),
    ("vector", &["embedding", "cosine", "knn", "dimension", "normalize"]),
    ("token", &["lexer", "tokenizer", "ngram", "stem", "segment"]),
    ("schema", &["migration", "column", "constraint", "fkey", "index"]),
    ("service", &["handler", "endpoint", "router", "middleware", "facade"]),
    ("kernel", &["syscall", "trap", "scheduler", "interrupt", "context"]),
    ("protocol", &["frame", "opcode", "session", "version", "negotiate"]),
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
    let graph_enabled = r.graph_expansion.as_ref().map(|g| g.enabled).unwrap_or(false);
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
        let (_, receipt) = hybrid_search_with_receipt(conn, "raretok0042", &opts)
            .expect("bare wrapper receipt");
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
