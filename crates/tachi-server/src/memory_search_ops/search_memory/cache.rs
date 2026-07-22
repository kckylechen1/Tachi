use crate::tool_params::SearchMemoryParams;
use crate::utils::{parse_env_bool, stable_hash};
use crate::MemoryServer;
use std::sync::atomic::{AtomicU64, Ordering};

pub(super) fn recall_cache_recall_opted_in(path_prefix: Option<&str>) -> bool {
    memcore::path_prefix_opts_into_recall_cache(path_prefix)
}

/// Master gate for the recall-cache read short-circuit + write-through.
/// Off by default; enabled per-deployment via `~/.tachi/config.env`. Only
/// consulted from within this module now (tachi#1435 slice 4 / #2059 codex
/// round 2): every writer that needs to bust the cache goes through
/// [`invalidate_recall_cache_after_write`] below instead of re-reading this
/// flag independently, so there is exactly one place that can drift from the
/// read/write-through gate.
pub(super) fn recall_cache_read_enabled() -> bool {
    parse_env_bool("TACHI_ENABLE_RECALL_CACHE").unwrap_or(false)
}

// ─── Write-side invalidation + epoch guard (tachi#1435 slice 3/4 / #2059) ──
//
// `invalidate_recall_cache_after_write` is the single choke point every
// content-changing writer calls after its own commit: a fresh save
// (`save_memory::handler`), an enrichment-batch flush that adds
// embeddings/keywords/summary (`enrichment.rs`), and a contradiction
// supersede that closes an old memory's validity
// (`memory_search_ops::contradiction`). All three change what a subsequent
// search would surface, so all three must invalidate — once per commit/batch,
// never per-row inside a batch.
//
// `RECALL_CACHE_EPOCH` closes a race the DELETE alone does not: an in-flight
// cache MISS search that already computed its (now-stale) result set before
// a concurrent save committed + invalidated must not resurrect that stale
// result via its own write-through landing *after* the invalidation. A
// search snapshots the epoch before doing any store work
// (`recall_cache_epoch()`); its write-through compares that snapshot against
// the current epoch immediately before writing and discards (does not write)
// on a mismatch — discarding is always safe, the next miss just recomputes.
//
// **Cross-process safety boundary**: this counter is process-local
// (`static AtomicU64`, not persisted). That is safe ONLY because of two
// facts that must keep holding:
//   1. The underlying `DELETE FROM recall_cache` this function issues IS a
//      real, cross-process-visible SQL commit — any *other* process reading
//      the same SQLite file after that commit sees the cache empty.
//   2. In production there is exactly ONE process that ever reads or
//      write-throughs this cache: the single-instance `tachi-server` daemon.
//      `portable-server` (the other MCP server binary in this workspace)
//      has zero references to `recall_cache` anywhere in its crate or its
//      `portable-kernel` dependency (verified by grep — see tachi#1435 slice
//      3's dispatch report) — it never engages this cache at all, so it
//      cannot observe or race this in-memory epoch.
// If a second cache-*consuming* process (one that calls `recall_cache_lookup`
// / `recall_cache_store`, not merely one that writes memories) is ever
// introduced, this in-memory epoch stops being sufficient — it would need to
// become a persisted (DB-backed) generation/version column that every
// process reads and compares, not an `AtomicU64`.
static RECALL_CACHE_EPOCH: AtomicU64 = AtomicU64::new(0);

/// Snapshot the current cache epoch. Callers take this BEFORE doing any
/// store work for a cache-miss search, so a concurrent invalidation that
/// lands after the snapshot is detectable at write-through time.
pub(super) fn recall_cache_epoch() -> u64 {
    RECALL_CACHE_EPOCH.load(Ordering::SeqCst)
}

/// The write-through race guard itself, extracted into its own testable
/// function (tachi#1435 slice 4 / #2059 codex round 2, "tooth 2" discriminating
/// test): `true` iff no invalidation has landed since `epoch_at_read` was
/// snapshotted, i.e. it is still safe to commit this miss-path search's
/// computed rows into the cache. `false` means discard — a concurrent
/// save/enrichment/contradiction invalidated in between, so writing now would
/// resurrect stale content. The caller (`search_memory::handlers`) never
/// writes when this returns `false`; discarding is always safe, the next
/// miss just recomputes.
pub(super) fn recall_cache_write_through_is_safe(epoch_at_read: u64) -> bool {
    recall_cache_epoch() == epoch_at_read
}

/// Write-side cache bust, shared by every content-changing writer. Gated on
/// [`recall_cache_read_enabled`] so a deployment with the cache off never
/// pays for a `DELETE` against a table it never populates. Bumps
/// [`RECALL_CACHE_EPOCH`] exactly once per successful `DELETE` — see the
/// module doc above for why that bump exists and its cross-process boundary.
///
/// Returns the same three-state fence string `save_memory`'s receipt
/// surfaces as `recall_fence` (`"cleared"` / `"unconfirmed"` / `"disabled"`);
/// non-receipt callers (enrichment flush, contradiction supersede) log the
/// context and continue rather than surfacing it anywhere.
///
/// Failure degrades loudly instead of pretending the cache is clean or
/// rolling back the write that triggered it: the DELETE failing after a
/// save/enrichment/contradiction already committed does not undo that
/// commit, it only means a stale cached search result *might* survive until
/// its TTL — logged, never silent.
pub(crate) fn invalidate_recall_cache_after_write(server: &MemoryServer, context: &str) -> &'static str {
    if !recall_cache_read_enabled() {
        return "disabled";
    }
    match server.with_global_store(|store| {
        store
            .recall_cache_invalidate_all()
            .map_err(|e| e.to_string())
    }) {
        Ok(_) => {
            RECALL_CACHE_EPOCH.fetch_add(1, Ordering::SeqCst);
            "cleared"
        }
        Err(err) => {
            tracing::warn!("[recall_cache] invalidation failed after {context}: {err}");
            "unconfirmed"
        }
    }
}

/// Freshness window for a cached entry, in seconds. A short default bounds how
/// long a just-added memory can stay hidden behind a stale entry; write-through
/// keeps actually-run queries fresh.
pub(super) fn recall_cache_ttl_secs() -> i64 {
    std::env::var("TACHI_RECALL_CACHE_TTL_SECS")
        .ok()
        .and_then(|v| v.trim().parse::<i64>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(900)
}

fn normalize_cache_query(query: &str) -> String {
    query
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// Build the opaque recall-cache key from every `SearchMemoryParams` field that
/// changes which rows are returned. Keep this in sync with the read-side
/// filters below — a result-affecting field missing here would let one query
/// serve another's cached rows. `enable_rerank` is deliberately excluded so the
/// background rerank job can upgrade the same entry in place; rerank intent is
/// reconciled against the stored `reranked` flag at read time.
pub(super) fn recall_cache_key(
    params: &SearchMemoryParams,
    top_k: usize,
    project_only: bool,
) -> String {
    let seed = format!(
        "rcv1|{q}|{proj}|{prefix}|{domain}|{top_k}|{po}|{tr}|{ar}|{meta}|{role}",
        q = normalize_cache_query(&params.query),
        proj = params.project.as_deref().unwrap_or(""),
        prefix = params.path_prefix.as_deref().unwrap_or(""),
        domain = params.domain.as_deref().unwrap_or(""),
        top_k = top_k,
        po = project_only as u8,
        tr = params.include_training as u8,
        ar = params.include_archived as u8,
        meta = params.include_metadata as u8,
        role = params.agent_role.as_deref().unwrap_or(""),
    );
    format!("rc:{}", stable_hash(&seed))
}
