use crate::memory_search_ops::search_helpers::{
    infer_search_project, named_project_db_exists, resolve_workspace_named_project,
};
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

/// Snapshot every DB that the real search routing will read. The cache key
/// carries these persisted generations, so a committed write from another
/// process naturally selects a new key even when no caller manually evicts the
/// global cache table. Any unknown, legacy, read-only, or trigger-drifted DB
/// disables caching for this query rather than serving a stale result.
///
/// This deliberately mirrors the target-selection branches in `rows.rs`; each
/// target is read exactly once through its normal read-store path. The labels
/// are stable routing identities, while the generation is the authoritative
/// freshness value in that physical DB.
pub(super) fn recall_cache_generation_fingerprint(
    server: &MemoryServer,
    params: &SearchMemoryParams,
    project_only: bool,
) -> Result<String, String> {
    let mut generations = Vec::new();
    let mut record_generation = |label: String, generation: i64| {
        if !generations.iter().any(|(known, _)| known == &label) {
            generations.push((label, generation));
        }
    };

    macro_rules! read_generation {
        ($label:expr, $read:expr) => {
            record_generation($label, $read?);
        };
    }

    let wiki_path_prefix = params
        .path_prefix
        .as_deref()
        .is_some_and(|prefix| prefix == "/wiki" || prefix.starts_with("/wiki/"));
    let mut searched_named = false;
    if let Some(project_name) = params.project.as_deref() {
        if named_project_db_exists(project_name) {
            read_generation!(
                format!("named-project:{project_name}"),
                server.with_named_project_store_read(project_name, |store| {
                    store.search_generation().map_err(|error| error.to_string())
                })
            );
            searched_named = true;
            if !project_only {
                read_generation!(
                    "global".to_string(),
                    server.with_global_store_read(|store| {
                        store.search_generation().map_err(|error| error.to_string())
                    })
                );
            }
        } else if !project_only {
            return Err(format!(
                "named project '{project_name}' is unavailable for cache validation"
            ));
        }
    }

    let mut searched_default_wiki = false;
    if params.project.is_none() && wiki_path_prefix && named_project_db_exists("wiki") {
        read_generation!(
            "named-project:wiki".to_string(),
            server.with_named_project_store_read("wiki", |store| {
                store.search_generation().map_err(|error| error.to_string())
            })
        );
        searched_default_wiki = true;
    }

    if !searched_named {
        if project_only {
            let named_project = resolve_workspace_named_project();
            if let Some(project_name) = named_project.as_deref() {
                if named_project_db_exists(project_name)
                    && (project_name != "wiki" || !searched_default_wiki)
                {
                    let workspace_path = server.project_db_path_buf();
                    let named_path = MemoryServer::resolve_named_project_db_path(project_name).ok();
                    let skip_workspace = workspace_path
                        .as_deref()
                        .zip(named_path.as_deref())
                        .map(|(workspace, named)| workspace == named)
                        .unwrap_or(false);
                    if !skip_workspace && server.has_project_db() {
                        read_generation!(
                            "bound-project".to_string(),
                            server.with_project_store_read(|store| {
                                store.search_generation().map_err(|error| error.to_string())
                            })
                        );
                    }
                    if named_path.is_some() {
                        read_generation!(
                            format!("named-project:{project_name}"),
                            server.with_named_project_store_read(project_name, |store| {
                                store.search_generation().map_err(|error| error.to_string())
                            })
                        );
                    }
                } else if server.has_project_db() {
                    read_generation!(
                        "bound-project".to_string(),
                        server.with_project_store_read(|store| {
                            store.search_generation().map_err(|error| error.to_string())
                        })
                    );
                }
            } else if server.has_project_db() {
                read_generation!(
                    "bound-project".to_string(),
                    server.with_project_store_read(|store| {
                        store.search_generation().map_err(|error| error.to_string())
                    })
                );
            }
        } else {
            let routing_config = server.routing_config().get();
            let inferred_project = infer_search_project(
                &server.tachi_home_dir(),
                &params.query,
                params.domain.as_deref(),
                &routing_config,
            );
            let inferred_db_path = inferred_project
                .as_deref()
                .and_then(|name| MemoryServer::resolve_named_project_db_path(name).ok());
            let skip_workspace = inferred_db_path.is_some()
                && server.project_db_path_buf().as_ref() == inferred_db_path.as_ref();

            read_generation!(
                "global".to_string(),
                server.with_global_store_read(|store| {
                    store.search_generation().map_err(|error| error.to_string())
                })
            );
            if let Some(project_name) = inferred_project.as_deref() {
                if project_name != "wiki" || !searched_default_wiki {
                    read_generation!(
                        format!("named-project:{project_name}"),
                        server.with_named_project_store_read(project_name, |store| {
                            store.search_generation().map_err(|error| error.to_string())
                        })
                    );
                }
            } else if server.has_project_db() && !skip_workspace {
                read_generation!(
                    "bound-project".to_string(),
                    server.with_project_store_read(|store| {
                        store.search_generation().map_err(|error| error.to_string())
                    })
                );
            }
        }
    }

    if generations.is_empty() {
        return Err("search selected no database for cache generation validation".to_string());
    }
    Ok(generations
        .into_iter()
        .map(|(label, generation)| format!("{label}={generation}"))
        .collect::<Vec<_>>()
        .join(","))
}

// ─── Optional local eviction fast path ────────────────────────────────────
//
// SQLite's trigger-maintained generation is the cache authority across every
// process. This epoch plus best-effort whole-table eviction only avoids local
// stale write-through work; missing a manual caller or a process-local race
// cannot cause a stale hit because the next lookup reads the DB generation.
// The epoch recheck and write-through remain inside one global-store write
// closure so an in-process eviction cannot be undone by an older miss result.
static RECALL_CACHE_EPOCH: AtomicU64 = AtomicU64::new(0);

/// Snapshot the current cache epoch. Callers take this BEFORE doing any
/// store work for a cache-miss search, so a concurrent invalidation that
/// lands after the snapshot is detectable at write-through time. This alone
/// is only a hint, not the safety mechanism — see the module doc's
/// mutual-exclusion invariant for why the actual recheck must happen inside
/// the same `with_global_store` critical section as the write it gates.
pub(super) fn recall_cache_epoch() -> u64 {
    RECALL_CACHE_EPOCH.load(Ordering::SeqCst)
}

/// The write-through race guard itself, extracted into its own testable
/// function (tachi#1435 slice 4 / #2059 codex round 2, "tooth 2" discriminating
/// test): `true` iff no invalidation has landed since `epoch_at_read` was
/// snapshotted, i.e. it is still safe to commit this miss-path search's
/// computed rows into the cache. `false` means discard — a concurrent
/// save/enrichment/contradiction/auto-link invalidated in between, so
/// writing now would resurrect stale content.
///
/// **Callers MUST invoke this from inside the same `with_global_store`
/// closure as the write it gates** (see the module doc's mutual-exclusion
/// invariant) — calling it beforehand, outside any lock, and then writing
/// inside the lock reopens the TOCTOU this function exists to close.
pub(super) fn recall_cache_write_through_is_safe(epoch_at_read: u64) -> bool {
    recall_cache_epoch() == epoch_at_read
}

/// The full miss-path write-through attempt, recheck and write together
/// inside ONE `with_global_store` critical section (tachi#1435 slice 6 /
/// #2059 codex round 3, "tooth B" TOCTOU fix) — see the module doc's
/// mutual-exclusion invariant. This is the actual production call site
/// (`search_memory::handlers`) delegates to, not a parallel
/// reimplementation, so a test exercising this function is exercising the
/// real race-closed path, not a stand-in for it.
///
/// Returns `Ok(true)` when the row was written, `Ok(false)` when discarded
/// because a concurrent invalidation bumped the epoch since `epoch_at_read`
/// was snapshotted (safe: the next miss just recomputes), or `Err` on a
/// genuine store error.
pub(super) fn recall_cache_write_through(
    server: &MemoryServer,
    epoch_at_read: u64,
    cache_id: &str,
    query: &str,
    rows_json: &str,
    result_count: i64,
    reranked: bool,
) -> Result<bool, String> {
    server.with_global_store(|store| {
        if !recall_cache_write_through_is_safe(epoch_at_read) {
            return Ok(false);
        }
        store
            .recall_cache_store(cache_id, query, rows_json, result_count, reranked)
            .map_err(|e| e.to_string())?;
        Ok(true)
    })
}

/// Write-side cache bust, shared by every content-changing writer. Gated on
/// [`recall_cache_read_enabled`] so a deployment with the cache off never
/// pays for a `DELETE` against a table it never populates.
///
/// The epoch bump happens INSIDE the same `with_global_store` closure as the
/// `DELETE`, in the success branch, before the closure returns — see the
/// module doc's mutual-exclusion invariant for why this must not move
/// outside the closure (a bump after `with_global_store` returns would let a
/// concurrent write-through's recheck, running in its OWN later critical
/// section, observe a not-yet-bumped epoch and treat a should-be-stale write
/// as safe).
///
/// Returns the same three-state fence string `save_memory`'s receipt
/// surfaces as `recall_fence` (`"cleared"` / `"unconfirmed"` / `"disabled"`);
/// non-receipt callers (enrichment flush, contradiction supersede, auto-link
/// supersede) log the context and continue rather than surfacing it
/// anywhere.
///
/// Failure degrades loudly instead of pretending the cache is clean or
/// rolling back the write that triggered it: the DELETE failing after a
/// save/enrichment/contradiction/auto-link already committed does not undo
/// that commit, it only means a stale cached search result *might* survive
/// until its TTL — logged, never silent.
pub(crate) fn invalidate_recall_cache_after_write(
    server: &MemoryServer,
    context: &str,
) -> &'static str {
    if !recall_cache_read_enabled() {
        return "disabled";
    }
    match server.with_global_store(|store| {
        store
            .recall_cache_invalidate_all()
            .map_err(|e| e.to_string())?;
        // Bump INSIDE this closure — still holding `global_rw_gate`'s write
        // lock — so this DELETE+bump is one atomic unit against any
        // concurrent write-through's recheck+write critical section.
        RECALL_CACHE_EPOCH.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }) {
        Ok(_) => "cleared",
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
    generation_fingerprint: &str,
) -> String {
    let seed = format!(
        "rcv2|{q}|{proj}|{prefix}|{domain}|{top_k}|{po}|{tr}|{ar}|{meta}|{role}|{generation_fingerprint}",
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
