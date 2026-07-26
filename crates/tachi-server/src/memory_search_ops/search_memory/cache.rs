use crate::memory_search_ops::search_helpers::{
    infer_search_project, named_project_db_exists, resolve_workspace_named_project,
};
use crate::tool_params::SearchMemoryParams;
use crate::utils::{parse_env_bool, stable_hash};
use crate::MemoryServer;
use serde::Serialize;
#[cfg(test)]
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use super::store::{pipeline_rule_read_sources, PipelineRuleReadSource};

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
    #[cfg(test)]
    if let Some(enabled) = TEST_RECALL_CACHE_ENABLED.with(Cell::get) {
        return enabled;
    }
    parse_env_bool("TACHI_ENABLE_RECALL_CACHE").unwrap_or(false)
}

#[cfg(test)]
type RecallCacheRaceCallback = (RecallCacheRacePoint, Box<dyn FnOnce()>);

#[cfg(test)]
thread_local! {
    static TEST_RECALL_CACHE_ENABLED: Cell<Option<bool>> = const { Cell::new(None) };
    static TEST_RACE_HOOK: RefCell<Option<RecallCacheRaceCallback>> = RefCell::new(None);
}

#[cfg(test)]
pub(crate) struct RecallCacheTestOverride {
    previous: Option<bool>,
}

#[cfg(test)]
impl RecallCacheTestOverride {
    pub(crate) fn enabled() -> Self {
        let previous = TEST_RECALL_CACHE_ENABLED.with(|value| value.replace(Some(true)));
        Self { previous }
    }
}

#[cfg(test)]
impl Drop for RecallCacheTestOverride {
    fn drop(&mut self) {
        TEST_RECALL_CACHE_ENABLED.with(|value| value.set(self.previous));
    }
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RecallCacheRacePoint {
    AfterLookupBeforeValidation,
    AfterQueryBeforeValidation,
}

#[cfg(test)]
pub(crate) struct RecallCacheRaceHook;

#[cfg(test)]
impl RecallCacheRaceHook {
    pub(crate) fn install(point: RecallCacheRacePoint, action: impl FnOnce() + 'static) -> Self {
        TEST_RACE_HOOK.with(|hook| {
            let previous = hook.replace(Some((point, Box::new(action))));
            assert!(
                previous.is_none(),
                "recall-cache race hook already installed"
            );
        });
        Self
    }
}

#[cfg(test)]
impl Drop for RecallCacheRaceHook {
    fn drop(&mut self) {
        TEST_RACE_HOOK.with(|hook| {
            hook.borrow_mut().take();
        });
    }
}

#[cfg(test)]
pub(super) fn run_recall_cache_race_hook(point: RecallCacheRacePoint) {
    let action = TEST_RACE_HOOK.with(|hook| {
        let mut hook = hook.borrow_mut();
        if hook
            .as_ref()
            .is_some_and(|(hook_point, _)| *hook_point == point)
        {
            hook.take().map(|(_, action)| action)
        } else {
            None
        }
    });
    if let Some(action) = action {
        action();
    }
}

#[derive(Debug, Clone)]
pub(super) enum SearchDatabaseTarget {
    Global(PathBuf),
    BoundProject(PathBuf),
    NamedProject { name: String, path: PathBuf },
}

impl SearchDatabaseTarget {
    fn path(&self) -> &Path {
        match self {
            Self::Global(path) | Self::BoundProject(path) | Self::NamedProject { path, .. } => path,
        }
    }

    fn label(&self) -> String {
        match self {
            Self::Global(_) => "global".to_string(),
            Self::BoundProject(_) => "bound-project".to_string(),
            Self::NamedProject { name, .. } => format!("named-project:{name}"),
        }
    }

    fn read_generation(&self, server: &MemoryServer) -> Result<i64, String> {
        match self {
            Self::Global(_) => server.with_global_store_read(|store| {
                store.search_generation().map_err(|error| error.to_string())
            }),
            Self::BoundProject(_) => server.with_project_store_read(|store| {
                store.search_generation().map_err(|error| error.to_string())
            }),
            Self::NamedProject { name, .. } => server
                .with_named_project_store_read(name, |store| {
                    store.search_generation().map_err(|error| error.to_string())
                }),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) struct DatabaseFileIdentity {
    canonical_path: PathBuf,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
}

impl DatabaseFileIdentity {
    fn resolve(path: &Path) -> Result<Self, String> {
        let canonical_path = std::fs::canonicalize(path).map_err(|error| {
            format!(
                "database identity cannot canonicalize {}: {error}",
                path.display()
            )
        })?;
        let metadata = std::fs::metadata(&canonical_path).map_err(|error| {
            format!(
                "database identity cannot stat {}: {error}",
                canonical_path.display()
            )
        })?;
        if !metadata.is_file() {
            return Err(format!(
                "database identity is not a regular file: {}",
                canonical_path.display()
            ));
        }

        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            Ok(Self {
                canonical_path,
                device: metadata.dev(),
                inode: metadata.ino(),
            })
        }
        #[cfg(not(unix))]
        {
            Ok(Self { canonical_path })
        }
    }

    fn physical_key(&self) -> String {
        #[cfg(unix)]
        {
            format!("unix:{}:{}", self.device, self.inode)
        }
        #[cfg(not(unix))]
        {
            format!(
                "path:{}",
                stable_hash(&self.canonical_path.to_string_lossy())
            )
        }
    }
}

pub(super) fn unique_database_targets(
    targets: Vec<SearchDatabaseTarget>,
) -> Result<Vec<(SearchDatabaseTarget, DatabaseFileIdentity)>, String> {
    let mut by_physical_key: HashMap<String, PathBuf> = HashMap::new();
    let mut unique = Vec::new();
    for target in targets {
        let identity = DatabaseFileIdentity::resolve(target.path())?;
        let physical_key = identity.physical_key();
        if let Some(first_path) = by_physical_key.get(&physical_key) {
            if first_path != &identity.canonical_path {
                return Err(format!(
                    "database identity is a hard-link alias between {} and {}; SQLite WAL sidecars are path-bound, so recall cache is bypassed",
                    first_path.display(),
                    identity.canonical_path.display()
                ));
            }
            continue;
        }
        by_physical_key.insert(physical_key, identity.canonical_path.clone());
        unique.push((target, identity));
    }
    Ok(unique)
}

fn search_database_targets(
    server: &MemoryServer,
    params: &SearchMemoryParams,
    project_only: bool,
) -> Result<Vec<SearchDatabaseTarget>, String> {
    let mut targets = Vec::new();
    let global = || SearchDatabaseTarget::Global(server.global_db_path_buf());
    let bound = || {
        server
            .project_db_path_buf()
            .map(SearchDatabaseTarget::BoundProject)
    };
    let named = |name: &str| -> Result<SearchDatabaseTarget, String> {
        Ok(SearchDatabaseTarget::NamedProject {
            name: name.to_string(),
            path: server.resolve_server_named_project_db_path(name)?,
        })
    };

    let wiki_path_prefix = params
        .path_prefix
        .as_deref()
        .is_some_and(|prefix| prefix == "/wiki" || prefix.starts_with("/wiki/"));
    let mut searched_named = false;
    if let Some(project_name) = params.project.as_deref() {
        if named_project_db_exists(server, project_name) {
            targets.push(named(project_name)?);
            searched_named = true;
            if !project_only {
                targets.push(global());
            }
        } else if !project_only {
            return Err(format!(
                "named project '{project_name}' is unavailable for cache validation"
            ));
        }
    }

    let mut searched_default_wiki = false;
    if params.project.is_none() && wiki_path_prefix && named_project_db_exists(server, "wiki") {
        targets.push(named("wiki")?);
        searched_default_wiki = true;
    }

    if !searched_named {
        if project_only {
            let named_project = resolve_workspace_named_project();
            if let Some(project_name) = named_project.as_deref() {
                if named_project_db_exists(server, project_name)
                    && (project_name != "wiki" || !searched_default_wiki)
                {
                    let named_target = named(project_name)?;
                    let skip_workspace = server
                        .project_db_path_buf()
                        .as_deref()
                        .is_some_and(|workspace| workspace == named_target.path());
                    if !skip_workspace {
                        if let Some(bound) = bound() {
                            targets.push(bound);
                        }
                    }
                    targets.push(named_target);
                } else if let Some(bound) = bound() {
                    targets.push(bound);
                }
            } else if let Some(bound) = bound() {
                targets.push(bound);
            }
        } else {
            let routing_config = server.routing_config().get();
            let inferred_project = infer_search_project(
                &server.tachi_home_dir(),
                &params.query,
                params.domain.as_deref(),
                &routing_config,
            );
            let inferred_target = inferred_project.as_deref().map(named).transpose()?;
            let skip_workspace = inferred_target.as_ref().is_some_and(|inferred| {
                server.project_db_path_buf().as_deref() == Some(inferred.path())
            });

            targets.push(global());
            if let Some(inferred_target) = inferred_target {
                if inferred_project.as_deref() != Some("wiki") || !searched_default_wiki {
                    targets.push(inferred_target);
                }
            } else if !skip_workspace {
                if let Some(bound) = bound() {
                    targets.push(bound);
                }
            }
        }
    }

    // Pipeline row augmentation reads these stores independently of request
    // routing. Consume the same source enumeration as rows.rs so explicit or
    // inferred named-project searches cannot omit bound/global rules from the
    // authoritative generation vector.
    for source in pipeline_rule_read_sources(server) {
        match source {
            PipelineRuleReadSource::BoundProject => {
                if let Some(bound) = bound() {
                    targets.push(bound);
                }
            }
            PipelineRuleReadSource::Global => targets.push(global()),
        }
    }

    if targets.is_empty() {
        return Err("search selected no database for cache generation validation".to_string());
    }
    Ok(targets)
}

#[derive(Serialize)]
struct DatabaseGeneration {
    identity: String,
    generation: i64,
}

#[derive(Serialize)]
struct GenerationFingerprint {
    version: u8,
    databases: Vec<DatabaseGeneration>,
}

/// Snapshot every physical DB that the real search routing will read. Target
/// paths are canonicalized and deduplicated before reads, so aliases cannot
/// cause duplicate work or inconsistent labels. A hard-link alias is rejected:
/// SQLite derives WAL sidecars from the opened path, so two names for one inode
/// are not a cache-safe database identity.
pub(super) fn recall_cache_generation_fingerprint(
    server: &MemoryServer,
    params: &SearchMemoryParams,
    project_only: bool,
) -> Result<String, String> {
    let targets = unique_database_targets(search_database_targets(server, params, project_only)?)?;
    let databases = targets
        .into_iter()
        .map(|(target, identity)| {
            target
                .read_generation(server)
                .map(|generation| DatabaseGeneration {
                    identity: identity.physical_key(),
                    generation,
                })
                .map_err(|error| format!("{} generation read failed: {error}", target.label()))
        })
        .collect::<Result<Vec<_>, _>>()?;
    serde_json::to_string(&GenerationFingerprint {
        version: 1,
        databases,
    })
    .map_err(|error| format!("serialize generation fingerprint: {error}"))
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
    generation_fingerprint: &str,
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
            .recall_cache_store(
                cache_id,
                generation_fingerprint,
                query,
                rows_json,
                result_count,
                reranked,
            )
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

fn require_finite_f64(field: &str, value: f64) -> Result<(), String> {
    if value.is_finite() {
        Ok(())
    } else {
        Err(format!("search cache field '{field}' must be finite"))
    }
}

fn validate_cache_request_floats(params: &SearchMemoryParams) -> Result<(), String> {
    if let Some(query_vec) = &params.query_vec {
        for (index, value) in query_vec.iter().enumerate() {
            if !value.is_finite() {
                return Err(format!(
                    "search cache field 'query_vec[{index}]' must be finite"
                ));
            }
        }
    }
    if let Some(value) = params.mmr_threshold {
        require_finite_f64("mmr_threshold", value)?;
    }
    if let Some(weights) = &params.weights {
        require_finite_f64("weights.semantic", weights.semantic)?;
        require_finite_f64("weights.fts", weights.fts)?;
        require_finite_f64("weights.symbolic", weights.symbolic)?;
        require_finite_f64("weights.decay", weights.decay)?;
    }
    Ok(())
}

#[derive(Serialize)]
struct RecallCacheRequest<'a> {
    version: u8,
    query: &'a str,
    query_vec: &'a Option<Vec<f32>>,
    top_k_requested: usize,
    top_k_effective: usize,
    path_prefix: &'a Option<String>,
    include_training: bool,
    include_archived: bool,
    candidates_per_channel: usize,
    mmr_threshold: Option<f64>,
    graph_expand_hops: u32,
    graph_relation_filter: &'a Option<String>,
    weights: &'a Option<crate::tool_params::HybridWeightsParam>,
    context_symbols: &'a [String],
    agent_role: &'a Option<String>,
    project: &'a Option<String>,
    domain: &'a Option<String>,
    file_context: &'a Option<String>,
    error_context: &'a Option<String>,
    enable_rerank: bool,
    as_of: &'a Option<String>,
    include_metadata: bool,
    format: &'a Option<String>,
    project_only: bool,
    pipeline_enabled: bool,
}

/// Build the opaque recall-cache key from deterministic structured
/// serialization of every request/handler option that can change rows or their
/// representation. `Option` values remain JSON null versus a concrete value,
/// and numeric zero remains zero, so absent/empty/zero cannot collapse through
/// delimiter tricks or ad-hoc defaults.
pub(super) fn recall_cache_key(
    params: &SearchMemoryParams,
    top_k: usize,
    project_only: bool,
    pipeline_enabled: bool,
) -> Result<String, String> {
    validate_cache_request_floats(params)?;
    let request = RecallCacheRequest {
        version: 3,
        query: &params.query,
        query_vec: &params.query_vec,
        top_k_requested: params.top_k,
        top_k_effective: top_k,
        path_prefix: &params.path_prefix,
        include_training: params.include_training,
        include_archived: params.include_archived,
        candidates_per_channel: params.candidates_per_channel,
        mmr_threshold: params.mmr_threshold,
        graph_expand_hops: params.graph_expand_hops,
        graph_relation_filter: &params.graph_relation_filter,
        weights: &params.weights,
        context_symbols: &params.context_symbols,
        agent_role: &params.agent_role,
        project: &params.project,
        domain: &params.domain,
        file_context: &params.file_context,
        error_context: &params.error_context,
        enable_rerank: params.enable_rerank,
        as_of: &params.as_of,
        include_metadata: params.include_metadata,
        format: &params.format,
        project_only,
        pipeline_enabled,
    };
    let serialized = serde_json::to_string(&request)
        .map_err(|error| format!("serialize recall cache request: {error}"))?;
    Ok(format!("rc:{}", stable_hash(&serialized)))
}
