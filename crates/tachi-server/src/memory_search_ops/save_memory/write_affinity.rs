//! #1041 S1 — domain-store write affinity gate.
//!
//! The write-side twin of `search_helpers::infer_search_project`'s read-side
//! domain routing: `RoutingConfig::domain_routes` (`~/.tachi/routing.json`)
//! already tells recall which named-project store owns a domain (the
//! store-allowlist invariant ratified in #770's placement rulings). Writes
//! never consulted that registry at all — an `external:mcp` save with
//! `scope=project` and no explicit `project=` always lands in whichever
//! project DB this daemon happens to be bound to, regardless of the save's
//! own domain. That's how engineering rows drift into a trading store (and
//! vice versa): the write follows the daemon binding, not the content.
//!
//! This gate only ever touches the ambiguous default path — a genuinely new
//! save (no `id` that resolves to an existing row at the target store), with
//! no CALLER-explicit `project=`, resolved to this daemon's bound project
//! store (not an explicit `scope=global`). A caller-explicit `project=`, or
//! an `id=` that actually resolves to a row already living at the target, is
//! a deliberate placement decision and is never second-guessed here.
//! Fail-safe posture: an unregistered domain is *uncertain*, not a
//! violation — it proceeds unchanged (with a warning); only a domain that IS
//! registered to a *different, currently-unbound* store is a confirmed
//! mismatch, and that either reroutes (store mounted) or refuses loudly
//! (store not mounted) — it never silently lands wrong.
//!
//! #1041 F2 (round-2 review fix): "caller-explicit `project=`" above is load
//! -bearing and is NOT the same thing as "`named_project.is_some()`", which
//! is what this gate used to check. Bound stdio/HTTP sessions inject the
//! session's bound project onto every project-defaulting write tool
//! (`session_identity::enforce_session_project`) whenever the caller
//! omitted `project=` — so the ordinary ambiguous-default save (the exact
//! case S1 exists to catch) always arrived here with `named_project.is_some()
//! == true` and skipped the gate entirely on the primary bound-transport
//! path. See `SaveMemoryParams::project_explicit`'s doc and
//! `session_identity::PROJECT_EXPLICIT_MARKER` for how a genuine caller
//! decision is now distinguished from a transport-injected default.
//!
//! #1041 F2 (round-2, `id=` exemption): similarly, a client-supplied `id`
//! used to skip this gate unconditionally, but `id` is documented as an
//! UPDATE key, not placement authority — if the id doesn't resolve to an
//! existing row at the target store, the save creates a brand-new row
//! there (not a patch), so it deserves the same domain-routing scrutiny as
//! any other new row. The gate now takes `id_resolves_at_target: bool`
//! (whether the caller's `id`, if any, was found to already exist at the
//! PRE-gate target) instead of re-deriving that from `params.id.is_some()`.
//!
//! # F1 (round-2, scope boundary — partially closed by #1114)
//!
//! `handle_save_memory` is not the only code path that persists a
//! `MemoryEntry`. Verified (read the code, not just codex's citation) direct
//! `MemoryStore::upsert` callers that never pass through this gate:
//!
//! - **Continuity/event projection** — CLOSED by #1114 via
//!   [`apply_write_affinity_for_domain`], called from
//!   `continuity_ops/storage.rs::upsert_projection_memory` for the live
//!   `tachi_event(action=project)` / `emit_memory_saved_event` path (the one
//!   that derives its own domain from the event but resolves its own store
//!   independently of `with_store_for_scope`). The background
//!   `ContinuityProjectionScheduler` sweep (`continuity_projector.rs`) is
//!   deliberately NOT gated: every target it builds is a `db_path`-pinned
//!   visit to one specific manifest DB (source store == destination store by
//!   construction), never the daemon's own ambiguous default — see the gate
//!   call site's doc for why a `db_path` target skips this gate entirely.
//! - **Pipeline ingest** (event/structured-event/source/extract) — each
//!   resolves `target_db`/`named_project` and upserts directly:
//!   `pipeline_ops/ingest/{event,structured_event,source,extract}.rs`.
//! - **Trajectory distill**: `hub_ops/call/distill.rs`.
//! - **Foundry capture** — CLOSED by #1114 via
//!   [`apply_write_affinity_for_domain`], called per-entry from
//!   `foundry_runtime_ops/handlers/capture_session.rs` before
//!   `persist_capture_entry` (the fresh-incoming-content path: bracket
//!   self-evolution notes and LLM-drafted session captures land wherever
//!   `resolve_capture_target` resolves, the exact ambiguous-default shape S1
//!   exists to catch — entries there set `domain: None` on the wire, so the
//!   gate call derives one via `repair::domain::repair_target` the same way
//!   `resolve_save_domain` does for `save_memory`).
//! - **Foundry distill** — verified NOT gated by #1114, and deliberately so:
//!   `foundry_runtime_ops/daily_distill/persist.rs::persist_distill_memory`
//!   and `foundry_runtime_ops/maintenance/distill_job.rs::process_memory_distill_job`
//!   both read their source memories from one specific store
//!   (`daily_distill/candidates.rs::collect_candidate_groups`'s
//!   `project`/`FoundryMaintenanceItem`'s `target_db`/`named_project`/
//!   `db_path`) and write the distilled summary BACK into that exact same
//!   store — source and destination are the same store by construction
//!   (`daily_distill/runner.rs::distill_one_project` threads one `project`
//!   value through both the read and the write). There is no ambiguous
//!   -default placement decision here to scrutinize: gating this the way
//!   `capture_session` is gated would risk collapsing every project's
//!   distilled output into one store the moment `domain: "foundry"`
//!   (a fixed tag, not content classification) ever gets registered as a
//!   route — exactly the "correctness regression in the name of coverage"
//!   this doc already warns about for the blanket-hook approach. Left
//!   unmodified; flagged for the next holder of this doc rather than
//!   silently dropped from the enumeration.
//! - **Other direct memory-row writers**: `component_governance_ops/mod.rs`,
//!   `copilot_ops/support/skills.rs`, `memory_search_ops/eval_capture.rs`,
//!   `handoff_ops/handlers.rs`, `kanban/handlers.rs`, `sticky_ops/handlers.rs`,
//!   `wiki_ops/ingest.rs`, `wiki_ops/log.rs`.
//!
//! Why the remaining paths above still don't close via `with_store_for_scope`:
//! EVERY one of them ultimately calls the exact same `MemoryStore::upsert`
//! (via `MemoryServer::with_store_for_scope` / `with_named_project_store`)
//! that `handle_save_memory` itself calls — but that shared choke point
//! lives BELOW where the routing policy exists. `MemoryStore::upsert`
//! (memcore) is a domain-agnostic storage primitive with no `RoutingConfig`,
//! no daemon-bound-project concept, and no `MemoryServer` reference; hooking
//! the gate there would mean plumbing tachi-server-level policy down into
//! memcore, a real layering change, not a cheap plug-in. The next candidate
//! layer up — `with_store_for_scope`/`with_named_project_store` on
//! `MemoryServer` itself — IS reachable from tachi-server, but it is shared
//! by every read AND write in the server (reads already use the separate
//! `_read` variants) across row kinds that are NOT domain-classified memory
//! content in the `SaveMemoryParams` sense: a kanban card, a sticky note, a
//! handoff memo, a component-governance record. Running THIS gate's
//! `resolve_save_domain`/`RoutingConfig::domain_routes` logic against those
//! would risk actively wrong behavior (e.g. rerouting a kanban card away
//! from the dispatch's own store because its `domain` field happens to
//! match a registered route) — a correctness regression in the name of
//! coverage, not a safe extension.
//!
//! [`apply_write_affinity_for_domain`]: apply_write_affinity_for_domain

use super::entry::resolve_save_domain;
use crate::memory_search_ops::routing_config::{RoutingConfig, RoutingConfigError};
use crate::memory_search_ops::search_helpers::{bound_project_label, named_project_db_exists};
use crate::tool_params::SaveMemoryParams;
use crate::{DbScope, MemoryServer};
use thiserror::Error;

/// #1041 F4: the write-affinity refusal is now a typed error (was a bare
/// `String`) so callers can distinguish "this is the S1 gate refusing a
/// cross-domain write" from an ordinary save failure without string
/// -matching the message. `impl From<WriteAffinityError> for String` keeps
/// the conversion transparent at every existing `?` call site — the crate
/// -wide save_memory error type stays `String` end-to-end, this only adds a
/// typed intermediate that can't be silently downgraded to a generic retry.
#[derive(Debug, Error)]
pub(crate) enum WriteAffinityError {
    #[error(
        "save refused: domain '{domain}' is registered to project store '{store}', which is not mounted on this daemon (bound store: {bound}). Refusing to silently write cross-domain into the bound store. Mount/register '{store}', or pass an explicit project= to override."
    )]
    UnmountedRoute {
        domain: String,
        store: String,
        bound: String,
    },
    /// #1041 F4: `RoutingConfig::load` used to collapse an unreadable file or
    /// invalid JSON into an empty route table with only a `tracing::warn!` —
    /// a broken config silently disabled the S1 gate rather than being
    /// treated as "can't evaluate this write's affinity, so don't risk it".
    #[error(
        "save refused: {0} — refusing to evaluate domain-store write affinity rather than risk a silent cross-domain write (a broken/unreadable routing config is treated as unsafe-to-proceed, not as \"no routes registered\"). Fix or remove the file and retry — no daemon restart required, the fix takes effect on the next save."
    )]
    RoutingConfigUnavailable(RoutingConfigError),
    /// #1041 B2 (codex round-4 TOCTOU): a caller-supplied `id` that does NOT
    /// resolve at the pre-gate target is not proof there's no concurrent
    /// writer — a racing save can insert that same id at the pre-gate store
    /// between the pre-gate lookup and this decision. Silently rerouting the
    /// id to a *different* store would then split one caller-chosen "update
    /// key" across two stores. `id=` is only meaningful as an update key
    /// against the store the caller actually meant, so an id-bearing save
    /// whose domain routes elsewhere refuses rather than reroutes — the
    /// caller must say which store they mean (drop `id=` for a fresh row, or
    /// pass an explicit `project=` to place this exact id deliberately).
    #[error(
        "save refused: client-supplied id '{id}' was not found at the daemon-bound store, and domain '{domain}' routes new rows to a different store ('{store}'). Refusing to silently create this id there — a caller-chosen id is an update key, and rerouting it risks splitting the same id across two stores under concurrent writes. Pass an explicit project='{store}' to place this id deliberately, or omit id to let a fresh row route normally."
    )]
    RerouteRefusedForClientId {
        id: String,
        domain: String,
        store: String,
    },
}

impl From<WriteAffinityError> for String {
    fn from(err: WriteAffinityError) -> String {
        err.to_string()
    }
}

impl WriteAffinityError {
    /// #1114 (codex round-2 item 3 fix): a stable, machine-checkable
    /// discriminant for callers that surface this error into a JSON
    /// response and need to tell "this domain's registered store isn't
    /// mounted" apart from "the whole routing config is unusable right
    /// now" without string-matching `Display`'s prose. Kept alongside
    /// (not instead of) the `Display` message — callers that just want the
    /// full text still get it via `.to_string()`/`{err}`.
    pub(crate) fn kind(&self) -> &'static str {
        match self {
            WriteAffinityError::UnmountedRoute { .. } => "unmounted_route",
            WriteAffinityError::RoutingConfigUnavailable(_) => "routing_config_unavailable",
            WriteAffinityError::RerouteRefusedForClientId { .. } => {
                "reroute_refused_for_client_id"
            }
        }
    }
}

/// What the gate did, surfaced back to the caller for transparency (never
/// itself an error — an `Err` result is the separate, loud-refusal path).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AffinityNote {
    /// The domain has no registered route — nothing to compare against, so
    /// the requested target was left untouched (uncertain, not wrong).
    Unregistered { domain: String },
    /// The domain is registered to a different, mounted store; the save was
    /// rerouted there instead of the originally-resolved target.
    Rerouted { domain: String, project: String },
}

#[derive(Debug)]
pub(crate) struct AffinityOutcome {
    pub target_db: DbScope,
    pub named_project: Option<String>,
    pub note: Option<AffinityNote>,
}

/// Apply the write-affinity gate against the live process state (real
/// `RoutingConfig::get_checked()`, the real daemon-bound project label, and
/// a real filesystem `named_project_db_exists` check). See
/// [`apply_write_affinity_with`] for the injectable core used by tests.
///
/// `id_resolves_at_target`: whether `params.id` (if the caller supplied one)
/// was found, by the caller, to already exist at the PRE-gate
/// `target_db`/`named_project` — see the module doc's F2 note on why
/// `params.id.is_some()` alone is no longer the skip signal.
pub(in crate::memory_search_ops::save_memory) fn apply_write_affinity(
    server: &MemoryServer,
    params: &SaveMemoryParams,
    target_db: DbScope,
    named_project: Option<&str>,
    id_resolves_at_target: bool,
) -> Result<AffinityOutcome, WriteAffinityError> {
    // #1041 B4: `get_checked()` now returns a shared `Arc<RoutingConfig>` (see
    // `routing_config::ROUTING_CONFIG_CACHE`'s doc) rather than a `&'static
    // RoutingConfig` — `&config` derefs it for `apply_write_affinity_with`.
    let config =
        RoutingConfig::get_checked().map_err(WriteAffinityError::RoutingConfigUnavailable)?;
    apply_write_affinity_with(
        params,
        target_db,
        named_project,
        id_resolves_at_target,
        &config,
        bound_project_label(server),
        named_project_db_exists,
    )
}

/// Config-injectable core (mirrors `search_helpers::infer_search_project_with_available`'s
/// DI shape so both the read-side and write-side domain routers stay
/// independently unit-testable without touching `~/.tachi/routing.json` or
/// the filesystem).
fn apply_write_affinity_with<F>(
    params: &SaveMemoryParams,
    target_db: DbScope,
    named_project: Option<&str>,
    id_resolves_at_target: bool,
    config: &RoutingConfig,
    current_label: Option<String>,
    project_exists: F,
) -> Result<AffinityOutcome, WriteAffinityError>
where
    F: Fn(&str) -> bool,
{
    let passthrough = || AffinityOutcome {
        target_db,
        named_project: named_project.map(str::to_string),
        note: None,
    };

    // Only the ambiguous default path is in scope: no CALLER-explicit
    // project= (see module doc's F2 note — `named_project.is_some()` alone
    // is not proof of that anymore), no `id` that resolves to an existing
    // row at the target (a genuine patch, not a new row), and resolved to
    // the bound project store rather than an explicit scope=global.
    let explicit_project_override = named_project.is_some() && params.project_explicit;
    if explicit_project_override || id_resolves_at_target || target_db != DbScope::Project {
        return Ok(passthrough());
    }

    let domain = match resolve_save_domain(params.domain.clone(), &params.path, &params.category) {
        Some(d) if !d.trim().is_empty() => d,
        _ => return Ok(passthrough()),
    };

    route_decision(
        domain,
        target_db,
        named_project,
        params.id.as_deref(),
        config,
        current_label,
        project_exists,
        "save_memory",
    )
}

/// #1114: entrypoint for callers outside `save_memory` that already resolved
/// their own domain and target store independently of `with_store_for_scope`
/// (continuity/event projection, foundry capture) — see the module doc's F1
/// note. This is NOT the blanket choke-point hook that doc rejects: it is
/// opt-in per write, and the caller supplies its own already-derived
/// `domain` and target instead of a `SaveMemoryParams`. Same
/// Unregistered/Rerouted/UnmountedRoute posture as [`apply_write_affinity`].
///
/// `explicit_project`: mirrors `SaveMemoryParams::project_explicit` — true
/// only when the CALLER (not a transport-injected session default) chose
/// `named_project`, in which case (like `apply_write_affinity`'s own
/// passthrough) it is never second-guessed. Callers whose target shape has
/// no ambiguous-default concept at all (e.g. a background sweep visiting one
/// specific `db_path`) should not call this function in the first place —
/// see the call sites in `continuity_ops::storage` and
/// `foundry_runtime_ops::handlers::capture_session` for the guard.
///
/// `id_resolves_at_target`: whether this exact row already exists at the
/// PRE-gate target — a genuine update-in-place is never second-guessed,
/// same as `apply_write_affinity`'s `id_resolves_at_target`.
pub(crate) fn apply_write_affinity_for_domain(
    server: &MemoryServer,
    domain: Option<&str>,
    target_db: DbScope,
    named_project: Option<&str>,
    explicit_project: bool,
    id_resolves_at_target: bool,
) -> Result<AffinityOutcome, WriteAffinityError> {
    let config =
        RoutingConfig::get_checked().map_err(WriteAffinityError::RoutingConfigUnavailable)?;
    apply_write_affinity_for_domain_with(
        domain,
        target_db,
        named_project,
        explicit_project,
        id_resolves_at_target,
        &config,
        bound_project_label(server),
        named_project_db_exists,
    )
}

#[allow(clippy::too_many_arguments)]
fn apply_write_affinity_for_domain_with<F>(
    domain: Option<&str>,
    target_db: DbScope,
    named_project: Option<&str>,
    explicit_project: bool,
    id_resolves_at_target: bool,
    config: &RoutingConfig,
    current_label: Option<String>,
    project_exists: F,
) -> Result<AffinityOutcome, WriteAffinityError>
where
    F: Fn(&str) -> bool,
{
    let passthrough = || AffinityOutcome {
        target_db,
        named_project: named_project.map(str::to_string),
        note: None,
    };

    let explicit_project_override = named_project.is_some() && explicit_project;
    if explicit_project_override || id_resolves_at_target || target_db != DbScope::Project {
        return Ok(passthrough());
    }

    let domain = match domain.map(str::trim).filter(|d| !d.is_empty()) {
        Some(d) => d.to_string(),
        None => return Ok(passthrough()),
    };

    // Continuity/foundry ids are internally generated (a stable hash or a
    // fresh uuid), never a caller-chosen placement authority the way
    // `SaveMemoryParams::id` can be — so a domain mismatch always reroutes,
    // matching `apply_write_affinity_with`'s id-LESS case. There is no
    // `RerouteRefusedForClientId` branch reachable from this entrypoint.
    route_decision(
        domain,
        target_db,
        named_project,
        None,
        config,
        current_label,
        project_exists,
        "write_affinity",
    )
}

/// Shared decision tail once a domain string, target, and scrutiny
/// -eligibility have already been established by the caller's own preamble
/// (`apply_write_affinity_with`'s `SaveMemoryParams`-shaped shortcuts, or
/// `apply_write_affinity_for_domain_with`'s domain-generic ones).
/// `client_id`: a caller-supplied placement-authority id (see
/// `WriteAffinityError::RerouteRefusedForClientId`'s doc) — `None` from every
/// #1114 caller, since continuity/foundry ids are never placement authority.
#[allow(clippy::too_many_arguments)]
fn route_decision<F>(
    domain: String,
    target_db: DbScope,
    named_project: Option<&str>,
    client_id: Option<&str>,
    config: &RoutingConfig,
    current_label: Option<String>,
    project_exists: F,
    log_target: &'static str,
) -> Result<AffinityOutcome, WriteAffinityError>
where
    F: Fn(&str) -> bool,
{
    let route = config.domain_routes.iter().find(|route| {
        route
            .domains
            .iter()
            .any(|d| d.eq_ignore_ascii_case(&domain))
    });

    let Some(route) = route else {
        tracing::warn!(
            domain = %domain,
            target = log_target,
            "write-affinity: domain has no registered store route; proceeding with the requested target (uncertain classification, fail-safe permissive)"
        );
        // #1041 round-3 regression fix (see `apply_write_affinity_with`'s
        // history): this branch is pass-through — "uncertain classification,
        // proceeds unchanged" — and must preserve whatever `named_project`
        // was passed in rather than forcing it to `None`, or a bound
        // transport's default target silently gets dropped.
        return Ok(AffinityOutcome {
            target_db,
            named_project: named_project.map(str::to_string),
            note: Some(AffinityNote::Unregistered { domain }),
        });
    };

    if current_label
        .as_deref()
        .is_some_and(|c| c.eq_ignore_ascii_case(&route.project))
    {
        // Already the registered store — no mismatch.
        return Ok(AffinityOutcome {
            target_db,
            named_project: named_project.map(str::to_string),
            note: None,
        });
    }

    // #1041 F8 (round-2 review, CONCERN — not fixed here): `project_exists`
    // (real impl: `named_project_db_exists`) is a `path.exists()` snapshot;
    // the store can vanish (or get recreated) between this check and the
    // eventual open in `persist.rs`/`server_methods/db.rs`, which resolves
    // the name again and uses open-or-create semantics — a genuinely
    // -missing store gets its parent directory created and a fresh empty
    // DB, not a refusal. Verified this is NOT introduced by the reroute
    // path: every named-project write (including a pre-#1041 explicit
    // `project=`) already goes through this identical
    // exists-check-then-later-reopen shape (e.g.
    // `session_identity`'s alias resolution checks existence at
    // normalization time, then the store opens fresh at persist time) — the
    // reroute decision here doesn't introduce a NEW race window, it reuses
    // the one every named-project path already has. A real fix (passing an
    // already-opened handle atomically from this check through to
    // persistence) is a cross-cutting change to the whole named-project
    // open path, not a local one; out of scope for this PR.
    if project_exists(&route.project) {
        // #1041 B2: a caller-supplied `id` that didn't resolve at the
        // pre-gate target is not proof no row exists there — a concurrent
        // writer can insert that exact id at the pre-gate store between the
        // pre-gate lookup and this decision. Silently rerouting would then
        // let the SAME caller-chosen id exist at two stores. Refuse instead
        // of reroute when `id` was genuinely supplied by the caller (an id
        // skipped this branch entirely, via the earlier
        // `id_resolves_at_target` passthrough, if it already resolved).
        if let Some(id) = client_id {
            return Err(WriteAffinityError::RerouteRefusedForClientId {
                id: id.to_string(),
                domain,
                store: route.project.clone(),
            });
        }
        tracing::warn!(
            domain = %domain,
            target = log_target,
            from = current_label.as_deref().unwrap_or("<unbound>"),
            to = %route.project,
            "write-affinity: domain-store affinity mismatch — rerouting to the registered store"
        );
        return Ok(AffinityOutcome {
            target_db: DbScope::Project,
            named_project: Some(route.project.clone()),
            note: Some(AffinityNote::Rerouted {
                domain,
                project: route.project.clone(),
            }),
        });
    }

    Err(WriteAffinityError::UnmountedRoute {
        domain,
        store: route.project.clone(),
        bound: current_label.unwrap_or_else(|| "<none>".to_string()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_params() -> SaveMemoryParams {
        SaveMemoryParams {
            text: "text".to_string(),
            summary: String::new(),
            path: "/notes/x".to_string(),
            importance: 0.7,
            category: "fact".to_string(),
            topic: String::new(),
            keywords: Vec::new(),
            persons: Vec::new(),
            entities: Vec::new(),
            location: String::new(),
            scope: "project".to_string(),
            vector: None,
            id: None,
            force: false,
            auto_link: false,
            project: None,
            project_explicit: false,
            retention_policy: None,
            domain: None,
            timestamp: None,
            valid_from: None,
            valid_until: None,
            metadata: None,
            emit_continuity: false,
        }
    }

    fn trading_routed_config() -> RoutingConfig {
        RoutingConfig {
            domain_routes: vec![crate::memory_search_ops::routing_config::DomainRoute {
                project: "hapi".to_string(),
                domains: vec!["equity_trading".to_string()],
            }],
            ..Default::default()
        }
    }

    #[test]
    fn skips_when_explicit_project_given() {
        let mut params = base_params();
        params.project = Some("hapi".to_string());
        params.project_explicit = true;
        params.domain = Some("equity_trading".to_string());
        let outcome = apply_write_affinity_with(
            &params,
            DbScope::Project,
            Some("hapi"),
            false,
            &trading_routed_config(),
            Some("quant".to_string()),
            |_| true,
        )
        .unwrap();
        assert!(outcome.note.is_none());
        assert_eq!(outcome.named_project.as_deref(), Some("hapi"));
    }

    /// #1041 F2 regression: the exact bug the round-2 review caught. A
    /// transport (bound stdio/HTTP session) injected `named_project` because
    /// the caller omitted `project=` — `project_explicit` stays `false`.
    /// Before the fix, `named_project.is_some()` alone would have skipped
    /// the gate here; now it must still evaluate (and reroute) the
    /// mismatched trading content away from the injected engineering-bound
    /// default.
    #[test]
    fn transport_injected_project_does_not_skip_the_gate() {
        let mut params = base_params();
        params.project = Some("quant".to_string()); // transport-injected value
        params.project_explicit = false; // NOT a caller decision
        params.domain = Some("equity_trading".to_string());
        let outcome = apply_write_affinity_with(
            &params,
            DbScope::Project,
            Some("quant"),
            false,
            &trading_routed_config(),
            Some("quant".to_string()),
            |project| project == "hapi",
        )
        .unwrap();
        assert_eq!(
            outcome.named_project.as_deref(),
            Some("hapi"),
            "a transport default must not be treated as an override — the \
             gate must still reroute mismatched content"
        );
        assert!(matches!(outcome.note, Some(AffinityNote::Rerouted { .. })));
    }

    #[test]
    fn skips_when_id_resolves_at_target() {
        let mut params = base_params();
        params.id = Some("existing-id".to_string());
        params.domain = Some("equity_trading".to_string());
        let outcome = apply_write_affinity_with(
            &params,
            DbScope::Project,
            None,
            true, // id_resolves_at_target: this really is a patch
            &trading_routed_config(),
            Some("quant".to_string()),
            |_| true,
        )
        .unwrap();
        assert!(outcome.note.is_none());
        assert_eq!(outcome.target_db, DbScope::Project);
        assert!(outcome.named_project.is_none());
    }

    /// #1041 F2 regression: an `id` that does NOT resolve at the target is
    /// not a patch — it still gets the same domain-routing SCRUTINY as an
    /// id-less save (does not skip the gate outright). Proven here via the
    /// unregistered-domain passthrough path, which doesn't trip the B2
    /// refusal below (nothing to refuse — there's no mounted mismatched
    /// store to reroute to).
    #[test]
    fn id_that_does_not_resolve_at_target_does_not_skip_the_gate() {
        let mut params = base_params();
        params.id = Some("brand-new-id".to_string());
        params.domain = Some("some_totally_unregistered_domain".to_string());
        let outcome = apply_write_affinity_with(
            &params,
            DbScope::Project,
            None,
            false, // id_resolves_at_target: false — this id doesn't exist yet
            &RoutingConfig::default(),
            Some("quant".to_string()),
            |_| false,
        )
        .unwrap();
        assert!(
            matches!(outcome.note, Some(AffinityNote::Unregistered { .. })),
            "an id that isn't actually an update must still reach domain \
             evaluation, not skip the gate outright"
        );
    }

    /// #1041 B2 core regression (codex round-4 TOCTOU): an `id` that does NOT
    /// resolve at the pre-gate target and WOULD have been rerouted to a
    /// different, mounted store must now be REFUSED, not silently rerouted.
    /// Before the fix, this reroute + a racing concurrent write inserting the
    /// same id at the pre-gate store could split one caller-chosen id across
    /// two stores. `id`-less saves are unaffected — they still reroute (see
    /// `trading_domain_on_engineering_daemon_reroutes_when_trading_store_mounted`).
    #[test]
    fn id_bearing_save_refuses_reroute_instead_of_splitting_the_id_across_stores() {
        let mut params = base_params();
        params.id = Some("brand-new-id".to_string());
        params.domain = Some("equity_trading".to_string());
        let result = apply_write_affinity_with(
            &params,
            DbScope::Project,
            None,
            false, // id_resolves_at_target: false — this id doesn't exist yet
            &trading_routed_config(),
            Some("quant".to_string()),
            |project| project == "hapi", // the reroute target IS mounted
        );
        let err = result.expect_err(
            "a caller-supplied id must refuse a domain reroute, never silently split across stores",
        );
        assert!(matches!(
            err,
            WriteAffinityError::RerouteRefusedForClientId { .. }
        ));
        let message = err.to_string();
        assert!(message.contains("brand-new-id"));
        assert!(message.contains("equity_trading"));
        assert!(message.contains("hapi"));
    }

    #[test]
    fn skips_when_scope_is_global() {
        let mut params = base_params();
        params.domain = Some("equity_trading".to_string());
        let outcome = apply_write_affinity_with(
            &params,
            DbScope::Global,
            None,
            false,
            &trading_routed_config(),
            Some("quant".to_string()),
            |_| true,
        )
        .unwrap();
        assert!(outcome.note.is_none());
        assert_eq!(outcome.target_db, DbScope::Global);
    }

    #[test]
    fn unregistered_domain_is_uncertain_not_blocking() {
        let mut params = base_params();
        params.domain = Some("some_totally_unregistered_domain".to_string());
        let outcome = apply_write_affinity_with(
            &params,
            DbScope::Project,
            None,
            false,
            &RoutingConfig::default(),
            Some("quant".to_string()),
            |_| false,
        )
        .unwrap();
        assert_eq!(outcome.target_db, DbScope::Project);
        assert!(outcome.named_project.is_none());
        assert!(matches!(
            outcome.note,
            Some(AffinityNote::Unregistered { .. })
        ));
    }

    /// #1041 round-3 regression: a transport-injected `named_project` (bound
    /// stdio/HTTP session, `project_explicit == false`) whose domain has NO
    /// registered route must still pass the bound project through unchanged
    /// — "uncertain classification" means "don't touch the target", not
    /// "drop the target". Before the fix, this branch forced `named_project:
    /// None`, which sent the write down the untargeted `DbScope::Project`
    /// path instead of the actually-bound named store (a hard failure on a
    /// global-only daemon, since it has no unnamed project DB at all).
    #[test]
    fn transport_injected_project_survives_unregistered_domain() {
        let mut params = base_params();
        params.project = Some("quant".to_string()); // transport-injected value
        params.project_explicit = false; // NOT a caller decision
        params.domain = Some("some_totally_unregistered_domain".to_string());
        let outcome = apply_write_affinity_with(
            &params,
            DbScope::Project,
            Some("quant"),
            false,
            &RoutingConfig::default(),
            Some("quant".to_string()),
            |_| false,
        )
        .unwrap();
        assert_eq!(
            outcome.named_project.as_deref(),
            Some("quant"),
            "an unregistered domain must pass the bound project through, not drop it"
        );
        assert!(matches!(
            outcome.note,
            Some(AffinityNote::Unregistered { .. })
        ));
    }

    #[test]
    fn matching_current_store_is_a_noop() {
        let mut params = base_params();
        params.domain = Some("equity_trading".to_string());
        let outcome = apply_write_affinity_with(
            &params,
            DbScope::Project,
            None,
            false,
            &trading_routed_config(),
            Some("hapi".to_string()),
            |_| true,
        )
        .unwrap();
        assert!(outcome.note.is_none());
        assert_eq!(outcome.named_project, None);
    }

    /// The packet's core judgement test: trading-domain content, an
    /// engineering-bound daemon, and the trading store IS mounted -> reroute.
    #[test]
    fn trading_domain_on_engineering_daemon_reroutes_when_trading_store_mounted() {
        let mut params = base_params();
        params.domain = Some("equity_trading".to_string());
        let outcome = apply_write_affinity_with(
            &params,
            DbScope::Project,
            None,
            false,
            &trading_routed_config(),
            Some("quant".to_string()),
            |project| project == "hapi",
        )
        .unwrap();
        assert_eq!(outcome.target_db, DbScope::Project);
        assert_eq!(outcome.named_project.as_deref(), Some("hapi"));
        assert!(matches!(outcome.note, Some(AffinityNote::Rerouted { .. })));
    }

    /// Same scenario, but the trading store is NOT mounted -> loud refusal,
    /// never a silent cross-domain write.
    #[test]
    fn trading_domain_on_engineering_daemon_refuses_loudly_when_trading_store_unmounted() {
        let mut params = base_params();
        params.domain = Some("equity_trading".to_string());
        let result = apply_write_affinity_with(
            &params,
            DbScope::Project,
            None,
            false,
            &trading_routed_config(),
            Some("quant".to_string()),
            |_| false,
        );
        let err = result.expect_err("must refuse, not silently save cross-domain");
        let message = err.to_string();
        assert!(message.contains("equity_trading"));
        assert!(message.contains("hapi"));
        assert!(matches!(err, WriteAffinityError::UnmountedRoute { .. }));
    }

    /// Engineering content on the same engineering daemon proceeds
    /// unaffected regardless of what routes are registered.
    #[test]
    fn engineering_content_proceeds_normally() {
        let mut params = base_params();
        params.domain = Some("engineering".to_string());
        let outcome = apply_write_affinity_with(
            &params,
            DbScope::Project,
            None,
            false,
            &trading_routed_config(),
            Some("quant".to_string()),
            |_| true,
        )
        .unwrap();
        assert_eq!(outcome.target_db, DbScope::Project);
        assert!(outcome.named_project.is_none());
        assert!(matches!(
            outcome.note,
            Some(AffinityNote::Unregistered { .. })
        ));
    }

    // ─── #1114: `apply_write_affinity_for_domain_with` (continuity/foundry) ───

    /// #1114 core judgement test: continuity-projected trading content on an
    /// engineering-bound daemon, with the trading store mounted -> reroute.
    /// Mirrors `trading_domain_on_engineering_daemon_reroutes_when_trading_store_mounted`
    /// but through the domain-generic entrypoint continuity/foundry call.
    #[test]
    fn for_domain_reroutes_mismatched_domain_when_store_mounted() {
        let outcome = apply_write_affinity_for_domain_with(
            Some("equity_trading"),
            DbScope::Project,
            None,
            false,
            false,
            &trading_routed_config(),
            Some("quant".to_string()),
            |project| project == "hapi",
        )
        .unwrap();
        assert_eq!(outcome.target_db, DbScope::Project);
        assert_eq!(outcome.named_project.as_deref(), Some("hapi"));
        assert!(matches!(outcome.note, Some(AffinityNote::Rerouted { .. })));
    }

    /// Same mismatch, but the registered store isn't mounted -> loud refusal,
    /// never a silent cross-domain write — same fail-safe posture as
    /// `apply_write_affinity_with`.
    #[test]
    fn for_domain_refuses_loudly_when_store_unmounted() {
        let result = apply_write_affinity_for_domain_with(
            Some("equity_trading"),
            DbScope::Project,
            None,
            false,
            false,
            &trading_routed_config(),
            Some("quant".to_string()),
            |_| false,
        );
        let err = result.expect_err("must refuse, not silently write cross-domain");
        assert!(matches!(err, WriteAffinityError::UnmountedRoute { .. }));
        let message = err.to_string();
        assert!(message.contains("equity_trading"));
        assert!(message.contains("hapi"));
    }

    /// A transport-injected `named_project` (continuity's own `event_db_route`
    /// resolving the bound session's project, `explicit_project == false`)
    /// must NOT skip the gate — same regression class as
    /// `transport_injected_project_does_not_skip_the_gate` for `save_memory`.
    #[test]
    fn for_domain_transport_injected_project_does_not_skip_the_gate() {
        let outcome = apply_write_affinity_for_domain_with(
            Some("equity_trading"),
            DbScope::Project,
            Some("quant"),
            false, // NOT a caller-explicit project= — transport-injected default
            false,
            &trading_routed_config(),
            Some("quant".to_string()),
            |project| project == "hapi",
        )
        .unwrap();
        assert_eq!(
            outcome.named_project.as_deref(),
            Some("hapi"),
            "a transport default must not be treated as an override — the \
             gate must still reroute mismatched content"
        );
        assert!(matches!(outcome.note, Some(AffinityNote::Rerouted { .. })));
    }

    /// A genuinely caller-explicit `project=` is never second-guessed, even
    /// against a mismatched, mounted domain route.
    #[test]
    fn for_domain_skips_when_explicit_project_given() {
        let outcome = apply_write_affinity_for_domain_with(
            Some("equity_trading"),
            DbScope::Project,
            Some("quant"),
            true, // caller-explicit
            false,
            &trading_routed_config(),
            Some("quant".to_string()),
            |project| project == "hapi",
        )
        .unwrap();
        assert!(outcome.note.is_none());
        assert_eq!(outcome.named_project.as_deref(), Some("quant"));
    }

    /// `id_resolves_at_target` (an update-in-place at the pre-gate target,
    /// e.g. a continuity projection memory that already exists there) is
    /// never second-guessed even for a mismatched, mounted domain route.
    #[test]
    fn for_domain_skips_when_id_resolves_at_target() {
        let outcome = apply_write_affinity_for_domain_with(
            Some("equity_trading"),
            DbScope::Project,
            None,
            false,
            true, // id_resolves_at_target
            &trading_routed_config(),
            Some("quant".to_string()),
            |_| true,
        )
        .unwrap();
        assert!(outcome.note.is_none());
        assert!(outcome.named_project.is_none());
    }

    /// Same-domain content proceeds unaffected, matching
    /// `engineering_content_proceeds_normally`.
    #[test]
    fn for_domain_unregistered_domain_proceeds_uncertain_not_blocking() {
        let outcome = apply_write_affinity_for_domain_with(
            Some("engineering"),
            DbScope::Project,
            None,
            false,
            false,
            &trading_routed_config(),
            Some("quant".to_string()),
            |_| true,
        )
        .unwrap();
        assert_eq!(outcome.target_db, DbScope::Project);
        assert!(outcome.named_project.is_none());
        assert!(matches!(
            outcome.note,
            Some(AffinityNote::Unregistered { .. })
        ));
    }

    /// A `Global` scope target is never second-guessed — same as
    /// `skips_when_scope_is_global`.
    #[test]
    fn for_domain_skips_when_scope_is_global() {
        let outcome = apply_write_affinity_for_domain_with(
            Some("equity_trading"),
            DbScope::Global,
            None,
            false,
            false,
            &trading_routed_config(),
            Some("quant".to_string()),
            |_| true,
        )
        .unwrap();
        assert!(outcome.note.is_none());
        assert_eq!(outcome.target_db, DbScope::Global);
    }

    /// No domain at all (e.g. `repair::domain::repair_target` was somehow
    /// bypassed and the caller genuinely has nothing) passes through
    /// unchanged rather than panicking or defaulting to a route.
    #[test]
    fn for_domain_none_passes_through() {
        let outcome = apply_write_affinity_for_domain_with(
            None,
            DbScope::Project,
            None,
            false,
            false,
            &trading_routed_config(),
            Some("quant".to_string()),
            |_| true,
        )
        .unwrap();
        assert!(outcome.note.is_none());
    }

    /// #1114 codex round-2 item 4①/③ CHARACTERIZATION test (documents a
    /// KNOWN, ACCEPTED limitation, not a target this PR claims to hit): the
    /// gate has no memory of a PRIOR call's decision across two calls with
    /// DIFFERENT `RoutingConfig`s for the SAME domain/id. In production
    /// this happens across a daemon restart after a `routing.json` edit
    /// (see `continuity_ops::storage::tests`' note on why
    /// `RoutingConfig::get_checked()`'s process-wide cache makes this
    /// untestable as a same-process integration test — that cache is
    /// exactly what makes it possible to characterize ONLY at this DI
    /// level, where `config` is a plain parameter, not a cached global).
    /// A caller can only ever check "does this id exist at the PRE-gate
    /// target" (never at wherever a PRIOR run under the OLD config actually
    /// placed it) — so when the registered route changes, the SAME
    /// deterministic id resolves to a DIFFERENT store on the next call,
    /// with nothing here (or in any of this PR's callers) aware that a
    /// copy may already exist at the OLD destination. Two independent rows
    /// under the same id, split across two stores, is the direct
    /// consequence. Closing this for real requires persistent per-id
    /// "last known location" tracking (a schema-level change), deferred
    /// alongside #1115's check-then-insert atomicity work — not solved by
    /// this PR.
    #[test]
    fn config_change_between_calls_reroutes_a_stable_id_to_a_different_store() {
        let config_a = RoutingConfig {
            domain_routes: vec![crate::memory_search_ops::routing_config::DomainRoute {
                project: "store-a".to_string(),
                domains: vec!["equity_trading".to_string()],
            }],
            ..Default::default()
        };
        let config_b = RoutingConfig {
            domain_routes: vec![crate::memory_search_ops::routing_config::DomainRoute {
                project: "store-b".to_string(),
                domains: vec!["equity_trading".to_string()],
            }],
            ..Default::default()
        };
        let both_mounted = |project: &str| project == "store-a" || project == "store-b";

        // "Before a routing.json edit + daemon restart": the id doesn't
        // exist anywhere yet (`id_resolves_at_target: false`) — routes to A.
        let before_restart = apply_write_affinity_for_domain_with(
            Some("equity_trading"),
            DbScope::Project,
            Some("quant"),
            false,
            false,
            &config_a,
            Some("quant".to_string()),
            both_mounted,
        )
        .unwrap();
        assert_eq!(before_restart.named_project.as_deref(), Some("store-a"));

        // "After the restart, with routing.json now pointing this domain at
        // B": a caller can only ever check the pre-gate target for
        // existence (never store-a, where the prior run actually placed
        // it) — id_resolves_at_target is STILL false here, honestly
        // reflecting what any real caller could know.
        let after_restart = apply_write_affinity_for_domain_with(
            Some("equity_trading"),
            DbScope::Project,
            Some("quant"),
            false,
            false,
            &config_b,
            Some("quant".to_string()),
            both_mounted,
        )
        .unwrap();
        assert_eq!(
            after_restart.named_project.as_deref(),
            Some("store-b"),
            "documents the known limitation: the SAME deterministic id resolves \
             to a DIFFERENT store once the registry changes, with nothing here \
             aware a copy may already exist at store-a from before the change — \
             this is what lets a row split across two stores. If this assertion \
             ever needs to change because the gate gained cross-call memory of \
             prior placements, that's a genuine improvement, not a regression."
        );
    }
}
