//! Recall-rerank cache job.
//!
//! Builds the small population of `recall-cache/{topic}` ephemeral memory
//! entries that the recall path consults to skip a full hybrid-search +
//! rerank round trip on the next equivalent query. Split out of
//! `maintenance.rs` (PR #2) so the cache lifecycle — query generation,
//! candidate fetch, rerank, persist — lives in one focused file.
//!
//! Design notes pinned here so the next contributor doesn't relitigate:
//!
//!   * Cache writes are deterministic by `(named_project, path_prefix,
//!     top_k, query)` — re-running the same job overwrites the same
//!     `foundry:recall-cache:{stable_hash}` row instead of growing the
//!     table. Durable cache writes are disabled by default after the DB
//!     hygiene audit showed they leaked across every long-lived memory DB.
//!     Set `TACHI_ENABLE_DURABLE_RECALL_CACHE=1` to opt back in.
//!
//!   * We deliberately do NOT enqueue an enrichment job for cache rows
//!     (PR #2 / Q4). The cache entry already carries the query string,
//!     `result_ids`, and per-result scores in `metadata`; running the
//!     enrichment pipeline would (a) burn Voyage embedding budget on
//!     transient text, (b) trigger graph auto-link from a synthetic
//!     entry, and (c) feed the recall search itself with cache rows as
//!     candidates. The on-write `retain` filter excludes cache rows
//!     from candidates regardless, but skipping enrichment removes the
//!     temptation entirely.
//!
//!   * Query selection (PR #2 / Q2): operator-supplied `metadata.queries`
//!     wins. Otherwise we ask the reasoning lane LLM for one short query
//!     summarizing the source memories' shared topic. Only if that fails
//!     (no API key, network error, empty output) do we fall back to the
//!     keyword-bag heuristic — the heuristic is now a safety net rather
//!     than the primary path. This keeps recall-cache queries phrased
//!     the way a real user would ask, which improves rerank hit rate.

mod config;
mod process;
mod queries;
mod search;
mod text;

pub(super) use config::durable_recall_cache_enabled;
pub(super) use process::process_recall_rerank_cache_job;
