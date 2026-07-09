//! Golden synthetic recall corpus — the permanent regression net for hybrid
//! recall quality (tachi#708 Phase A, spec §4.1
//! `docs/engineering/architecture/recall-quality-architecture.md`).
//!
//! # What this is
//! ~60 fully synthetic memory entries (zh / en / mixed, across
//! `/wiki` `/guide` `/notes` `/scratch`, wiki/guide/plain categories, a
//! fresh→old timestamp gradient, and keyword/entity/importance gradients) plus
//! ~50 labeled queries in five slices (summary-derived, keyword-bag, id-like,
//! pure-CJK, wiki-scoped). Every entry's text is invented; no line here is a
//! real memory, so the file is committed (not gitignored) — the privacy line is
//! respected by construction.
//!
//! # Two assertion layers
//! - **Ratchet layer** (plain `#[test]`, green on current main): each slice's
//!   recall@10 and the overall MRR are asserted `>=` the value *measured on the
//!   code as it stands when this file landed*. These lock in current behavior so
//!   any future regression turns CI red, and the floors ratchet upward as each
//!   repair phase lands.
//! - **Target layer** (plain `#[test]` once #708 Phase C is green): records the
//!   spec's quality goal (recall@10 / MRR >= 0.9). Landed green via `rrf_k=20`
//!   + lexical-overlap precision boost (soft-stem token coverage + char 4-gram
//!   Jaccard). The historical RED baseline is preserved in git history and in
//!   the ratchet floors below.
//!
//! # Metric split (important)
//! On this corpus recall@10 has long saturated at 1.000 for every slice — defect
//! M2 (RRF flattening) left the right answer PRESENT in top-10 yet ranked low.
//! So recall@10 remains the regression *floor*, and the rank-sensitive metrics
//! (summary/CJK/overall MRR) are the quality gate. See `golden_corpus_report`.
//!
//! # Determinism
//! All timestamps derive from a fixed base instant minus a per-entry day offset
//! (never `Utc::now()`), and MMR is disabled in the eval driver. Since tachi#718
//! production ranking carries a stable `(final_score desc, timestamp desc,
//! id asc)` tie-break at every sort site, so an identical query returns a
//! byte-identical id order run to run (asserted by
//! `golden_corpus_recall_order_is_deterministic`). `rank_of` still re-sorts
//! test-side by `(final_score desc, id asc)` — now redundant but harmless — and
//! the historical floors (measured under the old HashMap-order jitter) keep
//! their margin.
//!
//! # Vector channel
//! FTS + symbolic only. No entry carries a `vector`, so the vec channel is off
//! for every query here. A deterministic hand-built vector channel is feasible
//! (`SearchOptions.query_vec` + `vec_available`) but no repo test exercises real
//! KNN yet; wiring the first one is deferred to its own issue to keep this
//! fixture on the proven FTS+symbolic ground.
//!
//! # Config override
//! Variant behavior is driven per-call through `SearchOptions.recall_config`
//! (the only override path that works in a test binary — `RecallConfig::get()`
//! is a process-wide `OnceLock` locked to `Default` under `cfg!(test)`, so env
//! vars and config files are unreachable here).

use super::*;

/// Fixed base instant. Every entry's timestamp is `BASE − days_ago`, so the
/// corpus is byte-for-byte reproducible run to run.
fn ts_days_ago(days_ago: i64) -> String {
    let base = chrono::DateTime::parse_from_rfc3339("2026-06-01T00:00:00+00:00").unwrap();
    (base - chrono::Duration::days(days_ago)).to_rfc3339()
}

/// A synthetic corpus entry. All content is invented.
struct Seed {
    id: &'static str,
    path: &'static str,
    category: &'static str,
    text: &'static str,
    keywords: &'static [&'static str],
    entities: &'static [&'static str],
    importance: f64,
    days_ago: i64,
}

/// ~60 synthetic entries. Topical clusters share vocabulary on purpose so that
/// conjunctive-FTS misses on the hard slices have real distractors to lose to.
const SEEDS: &[Seed] = &[
    // ---- English: Rust / async cluster ----
    Seed { id: "gc-rust-async-01", path: "/notes/rust", category: "fact",
        text: "The tokio runtime schedules asynchronous tasks across a multi threaded worker pool",
        keywords: &["tokio", "async", "runtime"], entities: &["Tokio"], importance: 0.7, days_ago: 40 },
    Seed { id: "gc-rust-async-02", path: "/notes/rust", category: "fact",
        text: "Blocking calls inside an async task starve the tokio scheduler and cause latency spikes",
        keywords: &["blocking", "scheduler", "latency"], entities: &["Tokio"], importance: 0.6, days_ago: 35 },
    Seed { id: "gc-rust-async-03", path: "/notes/rust", category: "fact",
        text: "Spawning too many green threads exhausts the executor and increases context switching",
        keywords: &["executor", "threads"], entities: &[], importance: 0.5, days_ago: 55 },
    Seed { id: "gc-rust-async-04", path: "/notes/rust", category: "fact",
        text: "Async await desugars into a state machine polled by the runtime executor",
        keywords: &["async", "executor"], entities: &[], importance: 0.5, days_ago: 52 },
    Seed { id: "gc-rust-async-05", path: "/notes/rust", category: "fact",
        text: "A future does nothing until it is awaited or spawned onto the runtime",
        keywords: &["future", "runtime"], entities: &[], importance: 0.5, days_ago: 53 },
    Seed { id: "gc-rust-borrow-01", path: "/notes/rust", category: "fact",
        text: "The borrow checker rejects mutable aliasing to guarantee memory safety without a garbage collector",
        keywords: &["borrow", "ownership", "safety"], entities: &[], importance: 0.8, days_ago: 60 },
    Seed { id: "gc-rust-borrow-02", path: "/notes/rust", category: "fact",
        text: "Lifetimes annotate how long a reference stays valid so the compiler prevents dangling pointers",
        keywords: &["lifetime", "reference"], entities: &[], importance: 0.7, days_ago: 50 },
    // ---- English: SQLite / FTS / storage cluster ----
    Seed { id: "gc-sqlite-fts-01", path: "/notes/db", category: "fact",
        text: "SQLite FTS5 builds an inverted index over tokenised columns for fast full text search",
        keywords: &["sqlite", "fts5", "index"], entities: &["SQLite"], importance: 0.8, days_ago: 30 },
    Seed { id: "gc-sqlite-fts-02", path: "/notes/db", category: "fact",
        text: "The BM25 ranking function scores full text matches by term frequency and inverse document frequency",
        keywords: &["bm25", "ranking"], entities: &[], importance: 0.7, days_ago: 28 },
    Seed { id: "gc-sqlite-fts-03", path: "/notes/db", category: "fact",
        text: "Tokenisers like porter and unicode61 shape how fts5 splits text into terms",
        keywords: &["tokeniser", "fts5"], entities: &[], importance: 0.5, days_ago: 31 },
    Seed { id: "gc-sqlite-fts-04", path: "/notes/db", category: "fact",
        text: "The libsimple tokeniser segments chinese text for full text search inside sqlite",
        keywords: &["libsimple", "chinese"], entities: &[], importance: 0.6, days_ago: 32 },
    Seed { id: "gc-sqlite-vec-01", path: "/notes/db", category: "fact",
        text: "sqlite-vec stores dense embeddings in a virtual table and answers nearest neighbour queries",
        keywords: &["sqlite-vec", "embedding", "knn"], entities: &["sqlite-vec"], importance: 0.7, days_ago: 25 },
    Seed { id: "gc-sqlite-mig-01", path: "/notes/db", category: "fact",
        text: "Schema migrations run inside a transaction so a failed upgrade rolls back cleanly",
        keywords: &["migration", "transaction"], entities: &[], importance: 0.6, days_ago: 45 },
    // ---- English: recall / search cluster ----
    Seed { id: "gc-recall-01", path: "/notes/recall", category: "fact",
        text: "Hybrid recall merges vector fts and symbolic channels then fuses ranks with reciprocal rank fusion",
        keywords: &["recall", "hybrid", "rrf"], entities: &[], importance: 0.9, days_ago: 20 },
    Seed { id: "gc-recall-02", path: "/notes/recall", category: "fact",
        text: "Reciprocal rank fusion with a large k constant flattens the gap between rank one and rank twenty",
        keywords: &["rrf", "fusion"], entities: &[], importance: 0.8, days_ago: 18 },
    Seed { id: "gc-recall-03", path: "/notes/recall", category: "fact",
        text: "Conjunctive full text queries require every token to appear so one missing word zeroes the channel",
        keywords: &["conjunctive", "fts"], entities: &[], importance: 0.8, days_ago: 15 },
    Seed { id: "gc-recall-04", path: "/notes/recall", category: "fact",
        text: "An or fallback lexical query rewards coverage instead of demanding every single term match",
        keywords: &["fallback", "coverage"], entities: &[], importance: 0.7, days_ago: 12 },
    Seed { id: "gc-recall-05", path: "/notes/recall", category: "fact",
        text: "Symbolic bag of words scoring measures the fraction of query tokens present in an entry",
        keywords: &["symbolic", "bagofwords"], entities: &[], importance: 0.7, days_ago: 11 },
    Seed { id: "gc-recall-06", path: "/notes/recall", category: "fact",
        text: "The quality multiplier boosts wiki rows by fifteen percent in unscoped search",
        keywords: &["quality", "wiki", "multiplier"], entities: &[], importance: 0.7, days_ago: 6 },
    // ---- English: CI cluster ----
    Seed { id: "gc-ci-01", path: "/notes/ci", category: "fact",
        text: "The github actions workflow runs cargo nextest across the whole workspace on every push",
        keywords: &["ci", "nextest", "workflow"], entities: &["GitHub"], importance: 0.6, days_ago: 22 },
    Seed { id: "gc-ci-02", path: "/notes/ci", category: "fact",
        text: "A clippy lint gate with deny warnings blocks the merge until every diagnostic is resolved",
        keywords: &["clippy", "lint"], entities: &[], importance: 0.6, days_ago: 19 },
    Seed { id: "gc-ci-03", path: "/notes/ci", category: "fact",
        text: "Cargo fmt check fails the pipeline when formatting drifts from rustfmt defaults",
        keywords: &["fmt", "pipeline"], entities: &[], importance: 0.5, days_ago: 21 },
    // ---- English: MCP / daemon cluster ----
    Seed { id: "gc-mcp-01", path: "/notes/mcp", category: "fact",
        text: "The MCP daemon speaks streamable http and completes a two step initialize handshake before tool calls",
        keywords: &["mcp", "daemon", "handshake"], entities: &["MCP"], importance: 0.7, days_ago: 24 },
    Seed { id: "gc-mcp-02", path: "/notes/mcp", category: "fact",
        text: "Model context protocol tools expose typed arguments validated against a json schema",
        keywords: &["mcp", "schema"], entities: &["MCP"], importance: 0.6, days_ago: 21 },
    Seed { id: "gc-mcp-03", path: "/notes/mcp", category: "fact",
        text: "A session id header threads subsequent tool calls to the initialised mcp connection",
        keywords: &["session", "mcp"], entities: &[], importance: 0.6, days_ago: 25 },
    // ---- Wiki entries (category wiki, /wiki path) ----
    Seed { id: "gc-wiki-recall", path: "/wiki/recall-quality", category: "wiki",
        text: "Recall quality architecture gates every scoring change behind a golden corpus evaluation harness",
        keywords: &["recall", "golden", "harness"], entities: &[], importance: 0.8, days_ago: 33 },
    Seed { id: "gc-wiki-rrf", path: "/wiki/rrf", category: "wiki",
        text: "Reciprocal rank fusion combines multiple ranked lists into one consensus ordering",
        keywords: &["rrf", "fusion", "ranking"], entities: &[], importance: 0.7, days_ago: 34 },
    Seed { id: "gc-wiki-fts", path: "/wiki/fts5", category: "wiki",
        text: "FTS5 external content tables keep the index synchronised with the source rows via triggers",
        keywords: &["fts5", "triggers"], entities: &[], importance: 0.7, days_ago: 36 },
    Seed { id: "gc-wiki-embedding", path: "/wiki/embedding", category: "wiki",
        text: "Dense embeddings map text into a vector space where cosine distance approximates semantic similarity",
        keywords: &["embedding", "cosine", "semantic"], entities: &[], importance: 0.7, days_ago: 38 },
    Seed { id: "gc-wiki-tokio", path: "/wiki/tokio", category: "wiki",
        text: "Tokio is an asynchronous runtime for Rust providing an event loop and task scheduler",
        keywords: &["tokio", "async"], entities: &["Tokio"], importance: 0.7, days_ago: 41 },
    Seed { id: "gc-wiki-symbolic", path: "/wiki/symbolic", category: "wiki",
        text: "Symbolic scoring credits exact entity tokens even inside long prose entries",
        keywords: &["symbolic", "entity"], entities: &[], importance: 0.7, days_ago: 37 },
    Seed { id: "gc-wiki-daemon", path: "/wiki/daemon", category: "wiki",
        text: "The resident daemon reaps idle child processes and respawns on version skew",
        keywords: &["daemon", "reaper"], entities: &[], importance: 0.7, days_ago: 39 },
    Seed { id: "gc-wiki-harness", path: "/wiki/recall-harness", category: "wiki",
        text: "The golden corpus harness records red baselines and ratchets floors upward each phase",
        keywords: &["golden", "harness", "ratchet"], entities: &[], importance: 0.8, days_ago: 35 },
    // ---- Guide entries (category guide, /guide path) ----
    Seed { id: "gc-guide-deploy", path: "/guide/deploy", category: "guide",
        text: "To deploy a new daemon build the release binary stop the old process and verify health",
        keywords: &["deploy", "daemon", "release"], entities: &[], importance: 0.7, days_ago: 26 },
    Seed { id: "gc-guide-cleanup", path: "/guide/cleanup", category: "guide",
        text: "Run the disk cleanup playbook to reclaim shared cargo target directories before builds",
        keywords: &["cleanup", "disk", "cargo"], entities: &[], importance: 0.6, days_ago: 27 },
    Seed { id: "gc-guide-migrate", path: "/guide/migrate", category: "guide",
        text: "The migration guide walks through applying schema upgrades with a dry run preview first",
        keywords: &["migration", "dryrun"], entities: &[], importance: 0.6, days_ago: 29 },
    Seed { id: "gc-guide-review", path: "/guide/review", category: "guide",
        text: "The review guide requires a discrimination check that fails on pre fix code",
        keywords: &["review", "discrimination"], entities: &[], importance: 0.6, days_ago: 28 },
    Seed { id: "gc-guide-dispatch", path: "/guide/dispatch", category: "guide",
        text: "The dispatch doctrine separates the implementer lane from the adversarial review lane",
        keywords: &["dispatch", "doctrine"], entities: &[], importance: 0.6, days_ago: 30 },
    // ---- Scratch entries (low importance, recent, distractors) ----
    Seed { id: "gc-scratch-01", path: "/scratch/notes", category: "fact",
        text: "random scratch note about tokio async runtime latency debugging session",
        keywords: &["tokio", "scratch"], entities: &[], importance: 0.3, days_ago: 5 },
    Seed { id: "gc-scratch-02", path: "/scratch/notes", category: "fact",
        text: "temporary jotting on sqlite index rebuild during migration testing",
        keywords: &["sqlite", "scratch"], entities: &[], importance: 0.3, days_ago: 4 },
    Seed { id: "gc-scratch-03", path: "/scratch/notes", category: "fact",
        text: "scratch reminder to check reciprocal rank fusion constant tuning later",
        keywords: &["rrf", "scratch"], entities: &[], importance: 0.2, days_ago: 3 },
    Seed { id: "gc-scratch-04", path: "/scratch/notes", category: "fact",
        text: "临时便签关于 rrf 常数 k 等于六十压平排名的问题",
        keywords: &["rrf", "便签"], entities: &[], importance: 0.2, days_ago: 2 },
    Seed { id: "gc-scratch-05", path: "/scratch/notes", category: "fact",
        text: "throwaway note mentioning embedding cosine similarity experiment",
        keywords: &["embedding", "throwaway"], entities: &[], importance: 0.2, days_ago: 1 },
    // ---- Chinese cluster ----
    Seed { id: "gc-zh-mem-01", path: "/notes/zh-mem", category: "fact",
        text: "记忆系统的召回内核融合向量全文和符号三条通道并按倒数排名融合排序",
        keywords: &["记忆", "召回", "融合"], entities: &[], importance: 0.8, days_ago: 16 },
    Seed { id: "gc-zh-mem-02", path: "/notes/zh-mem", category: "fact",
        text: "合取全文检索要求每个词元都命中所以缺少一个关键词就会让整条通道归零",
        keywords: &["合取", "检索"], entities: &[], importance: 0.8, days_ago: 14 },
    Seed { id: "gc-zh-mem-03", path: "/notes/zh-mem", category: "fact",
        text: "中文查询无法触发或回退因为词元过滤只放行纯英文字符",
        keywords: &["中文", "回退"], entities: &[], importance: 0.7, days_ago: 13 },
    Seed { id: "gc-zh-mem-04", path: "/notes/zh-mem", category: "fact",
        text: "符号评分统计查询词元在条目文本关键词和实体中出现的比例",
        keywords: &["符号", "评分"], entities: &[], importance: 0.7, days_ago: 15 },
    Seed { id: "gc-zh-mem-05", path: "/notes/zh-mem", category: "fact",
        text: "质量乘数在未限定范围的搜索里把维基条目的分数提高百分之十五",
        keywords: &["质量", "维基"], entities: &[], importance: 0.7, days_ago: 6 },
    Seed { id: "gc-zh-db-01", path: "/notes/zh-db", category: "fact",
        text: "数据库迁移在事务中执行失败时会干净回滚不会留下损坏的表结构",
        keywords: &["数据库", "迁移", "事务"], entities: &[], importance: 0.7, days_ago: 42 },
    Seed { id: "gc-zh-db-02", path: "/notes/zh-db", category: "fact",
        text: "全文索引通过触发器与源数据行保持同步避免陈旧的检索结果",
        keywords: &["索引", "触发器"], entities: &[], importance: 0.6, days_ago: 43 },
    Seed { id: "gc-zh-db-03", path: "/notes/zh-db", category: "fact",
        text: "向量存储把稠密嵌入放进虚拟表并回答最近邻查询",
        keywords: &["向量", "嵌入"], entities: &[], importance: 0.6, days_ago: 44 },
    Seed { id: "gc-zh-daemon-01", path: "/notes/zh-daemon", category: "fact",
        text: "守护进程在盘中交易时段绝不重启只在收盘后停止并自动重生验证健康",
        keywords: &["守护进程", "重启"], entities: &[], importance: 0.7, days_ago: 46 },
    // ---- Mixed zh/en cluster ----
    Seed { id: "gc-mix-01", path: "/notes/mix", category: "fact",
        text: "MCP 守护进程使用 streamable http 完成两步 initialize 握手后才能调用工具",
        keywords: &["mcp", "守护进程", "握手"], entities: &["MCP"], importance: 0.7, days_ago: 23 },
    Seed { id: "gc-mix-02", path: "/notes/mix", category: "fact",
        text: "使用 recall_simulate 回放 labeled cases 评估召回质量而不改动 access 计数",
        keywords: &["recall_simulate", "召回"], entities: &[], importance: 0.8, days_ago: 17 },
    Seed { id: "gc-mix-03", path: "/notes/mix", category: "fact",
        text: "clippy 的 deny warnings 门禁会阻止合并直到所有诊断被解决",
        keywords: &["clippy", "门禁"], entities: &[], importance: 0.5, days_ago: 19 },
    Seed { id: "gc-mix-04", path: "/notes/mix", category: "fact",
        text: "or fallback 的 fts score factor 默认为零因此合取精度是默认行为",
        keywords: &["fallback", "fts"], entities: &[], importance: 0.6, days_ago: 10 },
    // ---- Id-like entries (exact uuid / slug / constant addressing) ----
    Seed { id: "11111111-2222-4333-8444-555555555555", path: "/notes/id", category: "fact",
        text: "This entry is addressed by an exact uuid and should win when the query is that uuid",
        keywords: &["uuid", "exact"], entities: &[], importance: 0.6, days_ago: 10 },
    Seed { id: "gc-probe-alpha-7f3", path: "/notes/id", category: "fact",
        text: "Probe token entry addressed by a hyphenated slug identifier for exact lookup",
        keywords: &["probe", "slug"], entities: &[], importance: 0.6, days_ago: 9 },
    Seed { id: "RECALL_PROBE_BETA_02", path: "/notes/id", category: "fact",
        text: "Uppercase probe constant used to test exact technical token retrieval",
        keywords: &["probe", "constant"], entities: &[], importance: 0.6, days_ago: 8 },
    Seed { id: "gc-ticket-708", path: "/notes/id", category: "fact",
        text: "Tracking entry for issue 708 phase a golden corpus fixture work",
        keywords: &["708", "issue"], entities: &[], importance: 0.6, days_ago: 7 },
];

fn seed_entry(s: &Seed) -> MemoryEntry {
    let mut e = memory_entry(s.id, s.text, s.keywords);
    e.path = s.path.into();
    e.category = s.category.into();
    e.entities = s.entities.iter().map(|x| x.to_string()).collect();
    e.importance = s.importance;
    e.summary = s.text.chars().take(40).collect();
    e.timestamp = ts_days_ago(s.days_ago);
    e.metadata = json!({ "keywords": s.keywords, "entities": s.entities });
    e
}

fn seed_corpus(conn: &mut Connection) {
    for s in SEEDS {
        insert_entry(conn, seed_entry(s));
    }
}

/// The five labeled query slices (spec §4.1).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Slice {
    /// Natural-language paraphrase of an entry — the query carries words the
    /// target lacks, so conjunctive FTS zeroes and only symbolic (low, RRF-flat)
    /// keeps the target alive. The M1+M2 failure slice.
    Summary,
    /// Exact keyword tokens the target owns — conjunctive FTS hits cleanly.
    KeywordBag,
    /// The entry id itself — exact-id boost puts it at rank 1.
    IdLike,
    /// Pure-Chinese paraphrase — exercises libsimple segmentation and the CJK
    /// or-fallback exclusion (M1 CJK bug).
    Cjk,
    /// Wiki-scoped (`path_prefix=/wiki`) — scoped retrieval + the M3 wiki path.
    WikiScoped,
}

struct QuerySpec {
    slice: Slice,
    query: &'static str,
    expected: &'static str,
}

/// ~50 labeled queries, 10 per slice. Each `expected` names one determinate
/// target entry from `SEEDS`.
const QUERIES: &[QuerySpec] = &[
    // ---- Summary-derived (paraphrases; deliberately hard) ----
    QuerySpec {
        slice: Slice::Summary,
        query: "how does the tokio runtime schedule asynchronous worker tasks efficiently",
        expected: "gc-rust-async-01",
    },
    QuerySpec {
        slice: Slice::Summary,
        query: "building an inverted index for rapid full text search over columns",
        expected: "gc-sqlite-fts-01",
    },
    QuerySpec {
        slice: Slice::Summary,
        query: "merging vector and lexical channels using rank fusion for hybrid retrieval",
        expected: "gc-recall-01",
    },
    QuerySpec {
        slice: Slice::Summary,
        query: "every search token must be present otherwise the whole channel returns nothing",
        expected: "gc-recall-03",
    },
    QuerySpec {
        slice: Slice::Summary,
        query: "steps to release and restart the daemon then confirm it is healthy",
        expected: "gc-guide-deploy",
    },
    QuerySpec {
        slice: Slice::Summary,
        query: "database upgrades wrapped in a transaction that reverts on failure",
        expected: "gc-sqlite-mig-01",
    },
    QuerySpec {
        slice: Slice::Summary,
        query: "the daemon performs a handshake over http before accepting tool invocations",
        expected: "gc-mcp-01",
    },
    QuerySpec {
        slice: Slice::Summary,
        query: "preventing mutable aliasing gives memory safety without a garbage collector",
        expected: "gc-rust-borrow-01",
    },
    QuerySpec {
        slice: Slice::Summary,
        query: "cosine distance in vector space approximates how similar two texts are",
        expected: "gc-wiki-embedding",
    },
    QuerySpec {
        slice: Slice::Summary,
        query: "continuous integration runs the full test suite on each commit",
        expected: "gc-ci-01",
    },
    // ---- Keyword-bag (exact owned keywords; should hit on main) ----
    QuerySpec {
        slice: Slice::KeywordBag,
        query: "recall hybrid rrf",
        expected: "gc-recall-01",
    },
    QuerySpec {
        slice: Slice::KeywordBag,
        query: "sqlite fts5 index",
        expected: "gc-sqlite-fts-01",
    },
    QuerySpec {
        slice: Slice::KeywordBag,
        query: "clippy lint gate",
        expected: "gc-ci-02",
    },
    QuerySpec {
        slice: Slice::KeywordBag,
        query: "mcp daemon handshake",
        expected: "gc-mcp-01",
    },
    QuerySpec {
        slice: Slice::KeywordBag,
        query: "reciprocal rank fusion",
        expected: "gc-recall-02",
    },
    QuerySpec {
        slice: Slice::KeywordBag,
        query: "cleanup disk cargo",
        expected: "gc-guide-cleanup",
    },
    QuerySpec {
        slice: Slice::KeywordBag,
        query: "embedding cosine semantic",
        expected: "gc-wiki-embedding",
    },
    QuerySpec {
        slice: Slice::KeywordBag,
        query: "lifetime reference compiler",
        expected: "gc-rust-borrow-02",
    },
    QuerySpec {
        slice: Slice::KeywordBag,
        query: "quality wiki multiplier",
        expected: "gc-recall-06",
    },
    QuerySpec {
        slice: Slice::KeywordBag,
        query: "dispatch doctrine lane",
        expected: "gc-guide-dispatch",
    },
    // ---- Id-like (exact identifier lookup) ----
    QuerySpec {
        slice: Slice::IdLike,
        query: "11111111-2222-4333-8444-555555555555",
        expected: "11111111-2222-4333-8444-555555555555",
    },
    QuerySpec {
        slice: Slice::IdLike,
        query: "gc-probe-alpha-7f3",
        expected: "gc-probe-alpha-7f3",
    },
    QuerySpec {
        slice: Slice::IdLike,
        query: "RECALL_PROBE_BETA_02",
        expected: "RECALL_PROBE_BETA_02",
    },
    QuerySpec {
        slice: Slice::IdLike,
        query: "gc-ticket-708",
        expected: "gc-ticket-708",
    },
    QuerySpec {
        slice: Slice::IdLike,
        query: "gc-recall-01",
        expected: "gc-recall-01",
    },
    QuerySpec {
        slice: Slice::IdLike,
        query: "gc-wiki-embedding",
        expected: "gc-wiki-embedding",
    },
    QuerySpec {
        slice: Slice::IdLike,
        query: "gc-guide-deploy",
        expected: "gc-guide-deploy",
    },
    QuerySpec {
        slice: Slice::IdLike,
        query: "gc-mcp-01",
        expected: "gc-mcp-01",
    },
    QuerySpec {
        slice: Slice::IdLike,
        query: "gc-sqlite-fts-01",
        expected: "gc-sqlite-fts-01",
    },
    QuerySpec {
        slice: Slice::IdLike,
        query: "gc-zh-mem-01",
        expected: "gc-zh-mem-01",
    },
    // ---- Pure-CJK (Chinese paraphrases) ----
    QuerySpec {
        slice: Slice::Cjk,
        query: "召回内核如何融合三条检索通道",
        expected: "gc-zh-mem-01",
    },
    QuerySpec {
        slice: Slice::Cjk,
        query: "合取检索缺一个词就让通道归零",
        expected: "gc-zh-mem-02",
    },
    QuerySpec {
        slice: Slice::Cjk,
        query: "数据库迁移失败时事务回滚",
        expected: "gc-zh-db-01",
    },
    QuerySpec {
        slice: Slice::Cjk,
        query: "守护进程盘中交易时段不重启",
        expected: "gc-zh-daemon-01",
    },
    QuerySpec {
        slice: Slice::Cjk,
        query: "中文查询为何无法触发或回退",
        expected: "gc-zh-mem-03",
    },
    QuerySpec {
        slice: Slice::Cjk,
        query: "全文索引用触发器保持同步",
        expected: "gc-zh-db-02",
    },
    QuerySpec {
        slice: Slice::Cjk,
        query: "符号评分统计查询词元出现比例",
        expected: "gc-zh-mem-04",
    },
    QuerySpec {
        slice: Slice::Cjk,
        query: "质量乘数提高维基条目的分数",
        expected: "gc-zh-mem-05",
    },
    QuerySpec {
        slice: Slice::Cjk,
        query: "向量存储稠密嵌入回答最近邻",
        expected: "gc-zh-db-03",
    },
    QuerySpec {
        slice: Slice::Cjk,
        query: "回放评估召回质量不改计数",
        expected: "gc-mix-02",
    },
    // ---- Wiki-scoped (path_prefix=/wiki) ----
    QuerySpec {
        slice: Slice::WikiScoped,
        query: "recall quality golden corpus evaluation",
        expected: "gc-wiki-recall",
    },
    QuerySpec {
        slice: Slice::WikiScoped,
        query: "reciprocal rank fusion consensus ordering",
        expected: "gc-wiki-rrf",
    },
    QuerySpec {
        slice: Slice::WikiScoped,
        query: "fts5 external content synchronised triggers",
        expected: "gc-wiki-fts",
    },
    QuerySpec {
        slice: Slice::WikiScoped,
        query: "dense embedding cosine semantic similarity",
        expected: "gc-wiki-embedding",
    },
    QuerySpec {
        slice: Slice::WikiScoped,
        query: "tokio asynchronous runtime task scheduler",
        expected: "gc-wiki-tokio",
    },
    QuerySpec {
        slice: Slice::WikiScoped,
        query: "symbolic scoring exact entity tokens",
        expected: "gc-wiki-symbolic",
    },
    QuerySpec {
        slice: Slice::WikiScoped,
        query: "resident daemon reaps idle processes",
        expected: "gc-wiki-daemon",
    },
    QuerySpec {
        slice: Slice::WikiScoped,
        query: "golden corpus harness ratchet baselines",
        expected: "gc-wiki-harness",
    },
    QuerySpec {
        slice: Slice::WikiScoped,
        query: "evaluation harness gates every scoring change",
        expected: "gc-wiki-recall",
    },
    QuerySpec {
        slice: Slice::WikiScoped,
        query: "combine multiple ranked lists into one ordering",
        expected: "gc-wiki-rrf",
    },
];

const ALL_SLICES: [Slice; 5] = [
    Slice::Summary,
    Slice::KeywordBag,
    Slice::IdLike,
    Slice::Cjk,
    Slice::WikiScoped,
];

/// Rank of `expected` (1-based) under a deterministic ordering, or `None` if the
/// target never entered the returned pool.
///
/// Since tachi#718 production ranking is itself deterministic on ties
/// (`final_score desc, timestamp desc, id asc`). We still pull a deep pool
/// (`top_k = 40`, larger than the corpus's per-query match set) and re-sort
/// test-side by `(final_score desc, id asc)` — redundant now, but it keeps this
/// metric independent of any future ranking-key change. Touches no production
/// code — only the eval harness.
fn rank_of(
    conn: &Connection,
    spec: &QuerySpec,
    recall_config: Option<RecallConfig>,
) -> Option<usize> {
    let opts = SearchOptions {
        top_k: 40,
        // Larger than the corpus so no channel's candidate list is truncated;
        // truncation of a BM25-tied list is a second nondeterminism source.
        candidates_per_channel: 128,
        record_access: false,
        // Pure score sort — MMR reordering needs vectors we do not seed.
        mmr_threshold: None,
        path_prefix: (spec.slice == Slice::WikiScoped).then(|| "/wiki".to_string()),
        recall_config,
        ..Default::default()
    };
    let mut results = hybrid_search(conn, spec.query, &opts).unwrap();
    results.sort_by(|a, b| {
        b.score
            .final_score
            .total_cmp(&a.score.final_score)
            .then_with(|| a.entry.id.cmp(&b.entry.id))
    });
    results
        .iter()
        .position(|r| r.entry.id == spec.expected)
        .map(|i| i + 1)
}

/// recall@`cut` for one slice under a given config override.
fn slice_recall_at(
    conn: &Connection,
    slice: Slice,
    cut: usize,
    recall_config: Option<RecallConfig>,
) -> f64 {
    let specs: Vec<&QuerySpec> = QUERIES.iter().filter(|q| q.slice == slice).collect();
    let hits = specs
        .iter()
        .filter(|q| matches!(rank_of(conn, q, recall_config.clone()), Some(r) if r <= cut))
        .count();
    hits as f64 / specs.len() as f64
}

/// recall@10 for one slice under a given config override.
fn slice_recall(conn: &Connection, slice: Slice, recall_config: Option<RecallConfig>) -> f64 {
    slice_recall_at(conn, slice, 10, recall_config)
}

/// Mean reciprocal rank for one slice.
fn slice_mrr(conn: &Connection, slice: Slice, recall_config: Option<RecallConfig>) -> f64 {
    let specs: Vec<&QuerySpec> = QUERIES.iter().filter(|q| q.slice == slice).collect();
    let sum: f64 = specs
        .iter()
        .map(|q| rank_of(conn, q, recall_config.clone()).map_or(0.0, |r| 1.0 / r as f64))
        .sum();
    sum / specs.len() as f64
}

/// Mean reciprocal rank over all queries (rank capped at top-10; 0 if absent).
fn overall_mrr(conn: &Connection, recall_config: Option<RecallConfig>) -> f64 {
    let sum: f64 = QUERIES
        .iter()
        .map(|q| rank_of(conn, q, recall_config.clone()).map_or(0.0, |r| 1.0 / r as f64))
        .sum();
    sum / QUERIES.len() as f64
}

// ---------------------------------------------------------------------------
// Measured baselines on current main (this fixture's own numbers, produced by
// the `golden_corpus_report` #[ignore] test below and confirmed over 20 runs).
//
// Metric split — WHY recall@10 is the floor but not the target:
//   recall@10 SATURATES at 1.000 for every slice on current main. That is not
//   the code being healthy — it is defect M2 (RRF k=60 flattening): the right
//   answer is PRESENT but RANKED LOW, not missing. So recall@10 makes the best
//   *regression floor* (any future drop = a real miss) while the rank-sensitive
//   metrics (recall@3, MRR) carry the *discrimination* — they are the ones the
//   known defects push below the spec target of 0.9.
//
// Determinism note: since tachi#718 production ties break stably (score desc,
// timestamp desc, id asc), so these metrics no longer flap. The floors were
// measured under the old HashMap-order jitter and retain their margin. If a
// future phase legitimately changes behavior, re-run the report and RAISE these
// floors (never lower one — that hides a regression).
// ---------------------------------------------------------------------------

/// recall@10 floor per slice (20-run stable value = 1.000 everywhere).
fn recall10_floor(slice: Slice) -> f64 {
    match slice {
        Slice::Summary => 1.0,
        Slice::KeywordBag => 1.0,
        Slice::IdLike => 1.0,
        Slice::Cjk => 1.0,
        Slice::WikiScoped => 1.0,
    }
}
// ---- Spec quality target (spec §4.1 / §7): recall@10 / MRR >= 0.9. ----
// Green after #708 Phase C (`rrf_k=20` + lexical-overlap boost). Pre-fix
// 20-run maxima that made this red: summary MRR <= 0.540, CJK MRR <= 0.864,
// overall MRR <= 0.868.
const TARGET_QUALITY: f64 = 0.9;

/// Overall-MRR regression floor. Raised after Phase C (measured ~0.947). Keep a
/// margin under the live number so tiny score jitter cannot flap CI; never lower.
const BASELINE_MRR_FLOOR: f64 = 0.90;

/// RATCHET LAYER — green on current main; the regression net. No slice's
/// recall@10 and no overall MRR may drop below the measured floor.
#[test]
fn golden_corpus_slices_do_not_regress() {
    let mut conn = setup();
    seed_corpus(&mut conn);
    for slice in ALL_SLICES {
        let recall = slice_recall(&conn, slice, None);
        let floor = recall10_floor(slice);
        assert!(
            recall >= floor,
            "slice {slice:?} recall@10 regressed: {recall:.3} < floor {floor:.3}"
        );
    }
    let mrr = overall_mrr(&conn, None);
    assert!(
        mrr >= BASELINE_MRR_FLOOR,
        "overall MRR regressed: {mrr:.3} < floor {BASELINE_MRR_FLOOR:.3}"
    );
}

/// TARGET LAYER — the spec's quality goal (recall@10 / MRR >= 0.9).
/// Green after #708 Phase C; remains a CI gate so rank regressions cannot hide
/// behind saturated recall@10.
#[test]
fn golden_corpus_meets_spec_targets() {
    let mut conn = setup();
    seed_corpus(&mut conn);

    let summary_r3 = slice_recall_at(&conn, Slice::Summary, 3, None);
    let summary_mrr = slice_mrr(&conn, Slice::Summary, None);
    let cjk_mrr = slice_mrr(&conn, Slice::Cjk, None);
    let overall = overall_mrr(&conn, None);

    assert!(
        summary_r3 >= TARGET_QUALITY,
        "summary slice recall@3 {summary_r3:.3} < spec target {TARGET_QUALITY:.3} \
         (M2: right answer present in top-10 but buried below rank 3)"
    );
    assert!(
        summary_mrr >= TARGET_QUALITY,
        "summary slice MRR {summary_mrr:.3} < spec target {TARGET_QUALITY:.3}"
    );
    assert!(
        cjk_mrr >= TARGET_QUALITY,
        "pure-CJK slice MRR {cjk_mrr:.3} < spec target {TARGET_QUALITY:.3}"
    );
    assert!(
        overall >= TARGET_QUALITY,
        "overall MRR {overall:.3} < spec target {TARGET_QUALITY:.3}"
    );
}

/// DISCRIMINATION TEST (tachi#718) — production recall order must be byte-stable
/// across reruns of an identical query over an identical corpus.
///
/// Pre-fix, `final_score` ties broke by HashMap iteration order at three sites
/// (`scorer::rank_map` RRF per-channel ranks, `ranking` final sort,
/// `filtering::newest_by_shared_entity` recency-boost pick), so the same query
/// returned different id sequences run to run. `rank_map` is the deepest source:
/// two docs with an equal per-channel score get adjacent RRF ranks in a
/// nondeterministic order, so their *final scores themselves* swap between runs.
///
/// This asserts each query's RAW production-returned id order — not the
/// `(score desc, id asc)` test-side re-sort in `rank_of`, which deliberately
/// masks the defect — is byte-identical across N consecutive runs. RED pre-fix
/// (some summary/CJK query jitters), GREEN once every sort site carries the
/// stable `(score desc, timestamp desc, id asc)` tie-break.
#[test]
fn golden_corpus_recall_order_is_deterministic() {
    let mut conn = setup();
    seed_corpus(&mut conn);

    const N: usize = 10;
    for spec in QUERIES {
        let opts = SearchOptions {
            top_k: 40,
            candidates_per_channel: 128,
            record_access: false,
            mmr_threshold: None,
            path_prefix: (spec.slice == Slice::WikiScoped).then(|| "/wiki".to_string()),
            ..Default::default()
        };
        let ids_at = || -> Vec<String> {
            hybrid_search(&conn, spec.query, &opts)
                .unwrap()
                .into_iter()
                .map(|r| r.entry.id)
                .collect()
        };
        let baseline = ids_at();
        for run in 1..N {
            let ids = ids_at();
            assert_eq!(
                ids, baseline,
                "query {:?} returned a different id order on run {run} of {N} \
                 (nondeterministic score-tie break — tachi#718)",
                spec.query
            );
        }
    }
}

/// VARIANT HOOK — documents that `or_fallback` is a live per-call lever and
/// that tachi#708 Gate 1 intentionally turned the factory default on. The
/// assertion is on the FTS *channel score* (a deterministic value, unlike
/// rank): a partial-coverage target scores 0 when explicitly disabled and a
/// positive score under the default eval-calibrated 0.55 factor.
#[test]
fn golden_corpus_or_fallback_override_is_a_live_lever() {
    let mut conn = setup();
    seed_corpus(&mut conn);

    // gc-recall-04 owns keywords {fallback, coverage}; the query adds ASCII
    // tokens it lacks ("missing", "zeroed", "channel") so the conjunctive
    // primary FTS query zeroes for it while >= 2 terms still cover it under OR.
    let query = "coverage fallback missing zeroed channel";
    let target = "gc-recall-04";

    let fts_channel_score = |cfg: Option<RecallConfig>| -> f64 {
        let opts = SearchOptions {
            top_k: 40,
            candidates_per_channel: 128,
            record_access: false,
            mmr_threshold: None,
            recall_config: cfg,
            ..Default::default()
        };
        hybrid_search(&conn, query, &opts)
            .unwrap()
            .iter()
            .find(|r| r.entry.id == target)
            .map(|r| r.score.fts)
            .expect("symbolic candidates keep the partial-coverage target in the pool")
    };

    // Explicit off: conjunctive-only FTS → the target has no FTS-channel signal.
    assert_eq!(
        fts_channel_score(Some(RecallConfig {
            or_fallback_fts_score_factor: 0.0,
            ..RecallConfig::default()
        })),
        0.0,
        "or_fallback=0.0 keeps the conjunctive-AND precision"
    );

    // tachi#708 Gate 1: default 0.55 rewards coverage with no mechanical harm
    // on the adversarial corpus (0 hit→miss, 1 miss→hit, 30 unchanged).
    assert_eq!(RecallConfig::default().or_fallback_fts_score_factor, 0.55);
    assert!(
        fts_channel_score(None) > 0.0,
        "default or_fallback=0.55 should give the partial-coverage target FTS signal"
    );
}

/// REPORT — not a gate. Prints per-slice recall@1/@3/@10 + MRR (default and
/// `or_fallback=0.55`) and overall MRR so the baseline constants above can be
/// refreshed after a legitimate behavior change. `#[ignore]`; run with
/// `cargo test -p memcore golden_corpus_report -- --ignored --nocapture`.
#[test]
#[ignore = "reporting helper — run with --nocapture to refresh baseline constants"]
fn golden_corpus_report() {
    let mut conn = setup();
    seed_corpus(&mut conn);
    let tuned = RecallConfig {
        or_fallback_fts_score_factor: 0.55,
        ..RecallConfig::default()
    };
    eprintln!(
        "--- golden corpus report ({} entries, {} queries) ---",
        SEEDS.len(),
        QUERIES.len()
    );
    for slice in ALL_SLICES {
        let r1 = slice_recall_at(&conn, slice, 1, None);
        let r3 = slice_recall_at(&conn, slice, 3, None);
        let r10 = slice_recall_at(&conn, slice, 10, None);
        let mrr = slice_mrr(&conn, slice, None);
        let r3o = slice_recall_at(&conn, slice, 3, Some(tuned.clone()));
        eprintln!(
            "slice {slice:?}: r@1={r1:.3} r@3={r3:.3} r@10={r10:.3} mrr={mrr:.3} | orf r@3={r3o:.3}"
        );
    }
    eprintln!(
        "overall MRR default={:.3} or_fallback0.55={:.3}",
        overall_mrr(&conn, None),
        overall_mrr(&conn, Some(tuned))
    );
}
