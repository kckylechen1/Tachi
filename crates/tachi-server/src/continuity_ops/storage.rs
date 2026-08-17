use std::path::PathBuf;

use memcore::{ContinuityMetrics, MemoryEdge, MemoryEntry, TachiEventQuery, TachiEventRecord};
use memory_server_runtime::EventDbRoute;

use crate::memory_search_ops::save_memory::write_affinity::WriteAffinityError;
use crate::{DbScope, MemoryServer};

#[derive(Debug, Clone)]
pub(crate) struct ContinuityEventTarget {
    target_db: DbScope,
    named_project: Option<String>,
    db_path: Option<PathBuf>,
}

impl ContinuityEventTarget {
    pub(crate) fn new(
        target_db: DbScope,
        named_project: Option<String>,
        db_path: Option<PathBuf>,
    ) -> Self {
        Self {
            target_db,
            named_project,
            db_path,
        }
    }

    pub(crate) fn from_default_write(server: &MemoryServer, project: Option<&str>) -> Self {
        match server.event_db_route(project) {
            EventDbRoute::NamedProject(project) => Self::new(DbScope::Project, Some(project), None),
            EventDbRoute::Project => Self::new(DbScope::Project, None, None),
            EventDbRoute::Global => Self::new(DbScope::Global, None, None),
        }
    }

    pub(super) fn project_label(&self, explicit_project: Option<&str>) -> String {
        let pin = crate::memory_search_ops::explicit_workspace_project();
        let db_parent = self
            .db_path
            .as_ref()
            .and_then(|path| path.parent())
            .and_then(|parent| parent.file_name())
            .and_then(|name| name.to_str());
        if let Some(label) = pick_label(
            explicit_project,
            self.named_project.as_deref(),
            pin.as_deref(),
            db_parent,
        ) {
            return label.to_string();
        }
        // All cheap sources empty — only now pay for the git-root workspace lookup.
        crate::memory_search_ops::resolve_workspace_named_project()
            .unwrap_or_else(|| self.target_db.as_str().to_string())
    }

    /// Durable at-most-once scheduling receipt. This intentionally is not an
    /// execution-completion claim: the current pipeline is best-effort and has
    /// no recoverable worker/outbox.
    pub(super) fn claim_pipeline_schedule(
        &self,
        server: &MemoryServer,
        operation_key: &str,
    ) -> Result<bool, String> {
        let value = serde_json::json!({
            "operation_key": operation_key,
            "policy": "continuity-pipeline-at-most-once-best-effort-v1",
            "scheduled_at": chrono::Utc::now().to_rfc3339(),
        })
        .to_string();
        if let Some(project) = self.named_project.as_deref() {
            server.with_named_project_store(project, |store| {
                store
                    .insert_state_if_absent(
                        "continuity-pipeline-schedules-v1",
                        operation_key,
                        &value,
                    )
                    .map_err(|e| e.to_string())
            })
        } else if let Some(path) = self.db_path.as_ref() {
            server.with_path_store(path, |store| {
                store
                    .insert_state_if_absent(
                        "continuity-pipeline-schedules-v1",
                        operation_key,
                        &value,
                    )
                    .map_err(|e| e.to_string())
            })
        } else {
            server.with_store_for_scope(self.target_db, |store| {
                store
                    .insert_state_if_absent(
                        "continuity-pipeline-schedules-v1",
                        operation_key,
                        &value,
                    )
                    .map_err(|e| e.to_string())
            })
        }
    }
}

/// Label precedence for a continuity projection over the cheap (non-git-walk)
/// sources: explicit param > store's named project > TACHI_PROJECT pin > db-path
/// parent (git-hash) name. The pin sits ahead of the git-hash name so a pinned
/// repo's direct-daemon projections carry the same label as its pinned reads and
/// proxy-injected writes (#488). Pure — no env/git access — so the ordering is
/// unit-tested without a process-global env race.
fn pick_label<'a>(
    explicit_project: Option<&'a str>,
    named_project: Option<&'a str>,
    pin: Option<&'a str>,
    db_parent_name: Option<&'a str>,
) -> Option<&'a str> {
    [explicit_project, named_project, pin, db_parent_name]
        .into_iter()
        .flatten()
        .map(str::trim)
        .find(|value| !value.is_empty())
}

pub(super) fn write_event(
    server: &MemoryServer,
    target: &ContinuityEventTarget,
    event: &TachiEventRecord,
) -> Result<(), String> {
    if let Some(project_name) = target.named_project.as_deref() {
        server.with_named_project_store(project_name, |store| {
            store
                .insert_tachi_event_if_absent(event)
                .map(|_| ())
                .map_err(|e| format!("insert continuity event: {e}"))
        })
    } else if let Some(db_path) = target.db_path.as_ref() {
        server.with_path_store(db_path, |store| {
            store
                .insert_tachi_event_if_absent(event)
                .map(|_| ())
                .map_err(|e| format!("insert continuity event: {e}"))
        })
    } else {
        server.with_store_for_scope(target.target_db, |store| {
            store
                .insert_tachi_event_if_absent(event)
                .map(|_| ())
                .map_err(|e| format!("insert continuity event: {e}"))
        })
    }
}

pub(super) fn write_event_if_absent(
    server: &MemoryServer,
    target: &ContinuityEventTarget,
    event: &TachiEventRecord,
) -> Result<bool, String> {
    if let Some(project_name) = target.named_project.as_deref() {
        server.with_named_project_store(project_name, |store| {
            store
                .insert_tachi_event_if_absent(event)
                .map_err(|error| format!("insert continuity event: {error}"))
        })
    } else if let Some(db_path) = target.db_path.as_ref() {
        server.with_path_store(db_path, |store| {
            store
                .insert_tachi_event_if_absent(event)
                .map_err(|error| format!("insert continuity event: {error}"))
        })
    } else {
        server.with_store_for_scope(target.target_db, |store| {
            store
                .insert_tachi_event_if_absent(event)
                .map_err(|error| format!("insert continuity event: {error}"))
        })
    }
}

pub(crate) fn read_events(
    server: &MemoryServer,
    target: &ContinuityEventTarget,
    query: &TachiEventQuery,
) -> Result<Vec<TachiEventRecord>, String> {
    if let Some(project_name) = target.named_project.as_deref() {
        server.with_named_project_store_read(project_name, |store| {
            store
                .list_tachi_events(query)
                .map_err(|e| format!("list continuity events: {e}"))
        })
    } else if let Some(db_path) = target.db_path.as_ref() {
        server.with_path_store_read(db_path, |store| {
            store
                .list_tachi_events(query)
                .map_err(|e| format!("list continuity events: {e}"))
        })
    } else {
        server.with_store_for_scope_read(target.target_db, |store| {
            store
                .list_tachi_events(query)
                .map_err(|e| format!("list continuity events: {e}"))
        })
    }
}

/// #1114 (codex round-1 B3 fix): resolve ONE routed destination for a
/// continuity projection's write-affinity gate — called ONCE, BEFORE any
/// existing-row lookup, row write, or graph-edge work for this projection,
/// so `get_projection_memory` (existing-row lookup), `upsert_projection_memory`
/// (the write), and `add_memory_edge`/timeline endpoint checks all target the
/// SAME store. Before this split, the gate ran INSIDE `upsert_projection_memory`
/// itself, after the caller had already looked up "does this row exist" and
/// resolved graph edges against the STALE pre-gate `target` — so a rerouted
/// projection's second run silently reset its aggregation state (seen/hit/miss
/// counters never found the row that had actually moved) and self-referencing
/// timeline edges got dropped as "endpoint missing" (the endpoint existed,
/// just not at the store being checked).
///
/// Only in scope when `target.db_path.is_none()` — a `db_path` target (the
/// background `ContinuityProjectionScheduler` sweep) is a pinned visit to
/// one specific manifest DB, source store == destination store by
/// construction, never the daemon's ambiguous default; gating it would risk
/// rerouting a scheduled per-DB pass away from the exact DB it's sweeping.
/// When `target.named_project` is `Some`, `project_explicit` (threaded from
/// `TachiEventParams::project_explicit`) decides whether that's a genuine
/// caller placement decision (skip) or a transport-injected session default
/// (still scrutinized) — same semantics as `SaveMemoryParams::project_explicit`.
///
/// `id_resolves_at_target` (codex round-1 B4-class fix — B4 itself named
/// `capture_session.rs`, but continuity projection has the identical
/// exposure and is closed here proactively): continuity projection ids are
/// internally deterministic (a stable hash of the projection key), never
/// caller-placement-authority — there is no caller id to second-guess a
/// reroute against (mirrors `apply_write_affinity_for_domain`'s
/// `client_id: None` contract for every #1114 caller). But a STABLE
/// deterministic id is exactly the shape that can get split across two
/// stores if the routing registry changes between two projection runs of
/// the SAME event: a projection captured before its domain had a
/// registered route lands at the pre-gate default; if that domain is later
/// registered, blindly re-evaluating routing on every subsequent run would
/// reroute the SAME id to the newly-registered store, creating a second,
/// independent copy instead of updating the row that's already there.
/// Callers check "does this row already exist at the PRE-gate target" and
/// pass the result in here — a genuine update-in-place is never
/// second-guessed by a registry change that happened after the fact, same
/// as `apply_write_affinity_with`'s own `id_resolves_at_target` contract.
/// #1114 (codex round-2 item 3 fix): returns the TYPED `WriteAffinityError`
/// rather than an already-stringified `String` — a refusal must stay
/// typed all the way up to `projection.rs`'s own call site, which decides
/// (based on whether this is a live explicit call or the background
/// auto-sweep) whether to hard-abort or soft-continue, and which needs
/// `WriteAffinityError::kind()` to tag the JSON response distinctly from an
/// ordinary persistence error. Converting to `String` this early erased
/// that distinction — every refusal looked identical to every other error
/// once it reached the JSON `errors` array.
pub(super) fn resolve_projection_write_target(
    server: &MemoryServer,
    target: &ContinuityEventTarget,
    domain: Option<&str>,
    project_explicit: bool,
    id_resolves_at_target: bool,
) -> Result<ContinuityEventTarget, WriteAffinityError> {
    if target.db_path.is_some() {
        return Ok(target.clone());
    }
    let affinity =
        crate::memory_search_ops::save_memory::write_affinity::apply_write_affinity_for_domain(
            server,
            domain,
            target.target_db,
            target.named_project.as_deref(),
            project_explicit,
            id_resolves_at_target,
        )?;
    Ok(ContinuityEventTarget {
        target_db: affinity.target_db,
        named_project: affinity.named_project,
        db_path: None,
    })
}

pub(super) fn upsert_projection_memory(
    server: &MemoryServer,
    target: &ContinuityEventTarget,
    entry: &MemoryEntry,
) -> Result<(), String> {
    if let Some(project_name) = target.named_project.as_deref() {
        server.with_named_project_store(project_name, |store| {
            store
                .upsert(entry)
                .map_err(|e| format!("upsert projection memory: {e}"))
        })
    } else if let Some(db_path) = target.db_path.as_ref() {
        server.with_path_store(db_path, |store| {
            store
                .upsert(entry)
                .map_err(|e| format!("upsert projection memory: {e}"))
        })
    } else {
        server.with_store_for_scope(target.target_db, |store| {
            store
                .upsert(entry)
                .map_err(|e| format!("upsert projection memory: {e}"))
        })
    }
}

pub(super) fn get_projection_memory(
    server: &MemoryServer,
    target: &ContinuityEventTarget,
    id: &str,
) -> Result<Option<MemoryEntry>, String> {
    if let Some(project_name) = target.named_project.as_deref() {
        server.with_named_project_store_read(project_name, |store| {
            store
                .get(id)
                .map_err(|e| format!("get projection memory: {e}"))
        })
    } else if let Some(db_path) = target.db_path.as_ref() {
        server.with_path_store_read(db_path, |store| {
            store
                .get(id)
                .map_err(|e| format!("get projection memory: {e}"))
        })
    } else {
        server.with_store_for_scope_read(target.target_db, |store| {
            store
                .get(id)
                .map_err(|e| format!("get projection memory: {e}"))
        })
    }
}

/// tachi#1646: this is `persist_timeline_graph_edges`'s only edge-write
/// door, and that caller lifts relation/weight straight out of an agent
/// session's event payload — hard-coded `CallerAsserted` here, not a
/// parameter, because there is exactly one caller and its authority class is
/// not a per-call decision. A second caller with a different authority
/// class must not reuse this function unmodified.
fn continuity_edge_provenance() -> memcore::db::EdgeProvenance {
    memcore::db::EdgeProvenance {
        authority: Some(memcore::db::EdgeAuthority::CallerAsserted),
        ..Default::default()
    }
}

pub(super) fn add_memory_edge(
    server: &MemoryServer,
    target: &ContinuityEventTarget,
    edge: &MemoryEdge,
) -> Result<(), String> {
    if edge.relation == "supersedes" {
        return Err(
            "supersedes is reserved for canonical immutable-supersession claims".to_string(),
        );
    }
    let provenance = continuity_edge_provenance();
    if let Some(project_name) = target.named_project.as_deref() {
        server.with_named_project_store(project_name, |store| {
            store
                .add_edge_with_provenance(edge, &provenance)
                .map_err(|e| format!("add continuity graph edge: {e}"))
        })
    } else if let Some(db_path) = target.db_path.as_ref() {
        server.with_path_store(db_path, |store| {
            store
                .add_edge_with_provenance(edge, &provenance)
                .map_err(|e| format!("add continuity graph edge: {e}"))
        })
    } else {
        server.with_store_for_scope(target.target_db, |store| {
            store
                .add_edge_with_provenance(edge, &provenance)
                .map_err(|e| format!("add continuity graph edge: {e}"))
        })
    }
}

pub(super) fn list_projection_memories(
    server: &MemoryServer,
    target: &ContinuityEventTarget,
    path_prefix: &str,
    limit: usize,
) -> Result<Vec<MemoryEntry>, String> {
    if let Some(project_name) = target.named_project.as_deref() {
        server.with_named_project_store_read(project_name, |store| {
            store
                .list_by_path(path_prefix, limit, false)
                .map_err(|e| format!("list projected memories: {e}"))
        })
    } else if let Some(db_path) = target.db_path.as_ref() {
        server.with_path_store_read(db_path, |store| {
            store
                .list_by_path(path_prefix, limit, false)
                .map_err(|e| format!("list projected memories: {e}"))
        })
    } else {
        server.with_store_for_scope_read(target.target_db, |store| {
            store
                .list_by_path(path_prefix, limit, false)
                .map_err(|e| format!("list projected memories: {e}"))
        })
    }
}

pub(super) fn continuity_metrics(
    server: &MemoryServer,
    target: &ContinuityEventTarget,
    limit: usize,
) -> Result<ContinuityMetrics, String> {
    if let Some(project_name) = target.named_project.as_deref() {
        server.with_named_project_store_read(project_name, |store| {
            store
                .continuity_metrics(limit)
                .map_err(|e| format!("compute continuity metrics: {e}"))
        })
    } else if let Some(db_path) = target.db_path.as_ref() {
        server.with_path_store_read(db_path, |store| {
            store
                .continuity_metrics(limit)
                .map_err(|e| format!("compute continuity metrics: {e}"))
        })
    } else {
        server.with_store_for_scope_read(target.target_db, |store| {
            store
                .continuity_metrics(limit)
                .map_err(|e| format!("compute continuity metrics: {e}"))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::pick_label;
    use super::{resolve_projection_write_target, upsert_projection_memory, ContinuityEventTarget};
    use crate::server_state::MemoryServer;
    use crate::DbScope;
    use memcore::MemoryEntry;
    use serde_json::json;

    fn projection_entry(id: &str, domain: Option<&str>) -> MemoryEntry {
        MemoryEntry {
            id: id.to_string(),
            path: "/timeline/projected".to_string(),
            summary: "projected".to_string(),
            text: "projected content".to_string(),
            importance: 0.6,
            timestamp: "2026-07-14T00:00:00Z".to_string(),
            valid_from: "2026-07-14T00:00:00Z".to_string(),
            valid_until: None,
            category: "experience".to_string(),
            topic: "timeline".to_string(),
            keywords: Vec::new(),
            persons: Vec::new(),
            entities: Vec::new(),
            location: String::new(),
            source: "external:tachi_event_projection".to_string(),
            scope: "project".to_string(),
            archived: false,
            access_count: 0,
            scored_count: 0,
            last_access: None,
            last_use_at: None,
            revision: 1,
            metadata: json!({}),
            vector: None,
            retention_policy: None,
            domain: domain.map(str::to_string),
            recall_count: 0,
            query_diversity: 0,
            tier: "raw".to_string(),
        }
    }

    fn bound_server(home: &std::path::Path, bound_project: &str) -> MemoryServer {
        let project_db = home.join("projects").join(bound_project).join("memory.db");
        std::fs::create_dir_all(project_db.parent().unwrap()).expect("mkdir project");
        let global_db = home.join("global").join("memory.db");
        std::fs::create_dir_all(global_db.parent().unwrap()).expect("mkdir global");
        // `MemoryServer::new(global, project)` — the PROJECT path (second
        // arg) is what `bound_project_label` resolves the daemon's own bound
        // name from via the Plan C `projects/<name>/memory.db` convention.
        MemoryServer::new(global_db, Some(project_db)).expect("bind daemon")
    }

    /// #1114 (Oz r3 fixture fix): mount a real, schema-initialized named
    /// -project DB rather than a zero-byte placeholder file. `with_named_
    /// project_store` (the WRITE path) resolves via `resolve_named_project_
    /// db_path`, which only checks `.exists()` — a zero-byte file satisfies
    /// that, and a WRITE against it succeeds because `MemoryStore::open_
    /// with_label` runs schema init on first open. But `with_named_project_
    /// store_read` (the READ path a pre-gate/existing-row check goes
    /// through) does NOT run that init — reading a schema-less file fails
    /// outright ("no such table: memories"), not "no rows found". Any test
    /// whose FIRST touch of a mounted store is a read (not a write) needs a
    /// REAL schema, not a placeholder — `memcore::MemoryStore::open_with_label`
    /// (the same call the write path itself makes) gives it one.
    fn mount_named_project_db(home: &std::path::Path, name: &str) -> std::path::PathBuf {
        let db_path = home.join("projects").join(name).join("memory.db");
        std::fs::create_dir_all(db_path.parent().unwrap()).expect("mkdir named project");
        memcore::MemoryStore::open_with_label(db_path.to_str().expect("utf-8 db path"), name)
            .expect("init named project schema");
        db_path
    }

    /// #1114 discriminating test (red before this PR): a continuity
    /// projection whose event domain is registered to a DIFFERENT, mounted
    /// store than the daemon's own bound project — via a transport-injected
    /// `named_project` (bound session default, NOT a caller's `project=`) —
    /// must be rerouted there, not silently upserted into the bound
    /// project's own store. Before this change, `upsert_projection_memory`
    /// called `with_named_project_store`/`with_store_for_scope` directly
    /// with no domain-affinity check at all: this is the exact
    /// cross-domain-drift shape #1041/#1114 exist to catch.
    #[test]
    fn cross_domain_projection_reroutes_to_registered_mounted_store() {
        crate::test_support::with_tachi_home(|home| {
            std::fs::write(
                home.join("routing.json"),
                r#"{"domain_routes":[{"project":"hapi","domains":["equity_trading"]}]}"#,
            )
            .expect("write routing.json");
            let server = bound_server(home, "quant");
            let _hapi_db = mount_named_project_db(home, "hapi");

            let target = ContinuityEventTarget::new(
                DbScope::Project,
                Some("quant".to_string()), // transport-injected default == bound project
                None,
            );
            let entry = projection_entry("projection-1", Some("equity_trading"));
            let routed = resolve_projection_write_target(
                &server,
                &target,
                entry.domain.as_deref(),
                false,
                false,
            )
            .expect("resolve routed target");
            upsert_projection_memory(&server, &routed, &entry).expect("upsert projection memory");

            let in_hapi = server
                .with_named_project_store_read("hapi", |store| {
                    store.get(&entry.id).map_err(|e| e.to_string())
                })
                .expect("read hapi");
            assert!(
                in_hapi.is_some(),
                "equity_trading projection content must reroute into hapi"
            );
            let in_quant = server
                .with_project_store_read(|store| store.get(&entry.id).map_err(|e| e.to_string()))
                .expect("read quant");
            assert!(
                in_quant.is_none(),
                "must NOT silently land in the daemon's own bound (quant) store"
            );
        });
    }

    /// Same mismatch, but the registered store is not mounted — must refuse
    /// loudly (typed `WriteAffinityError` surfaced as a `String`), never
    /// silently write cross-domain.
    #[test]
    fn cross_domain_projection_refuses_when_registered_store_unmounted() {
        crate::test_support::with_tachi_home(|home| {
            std::fs::write(
                home.join("routing.json"),
                r#"{"domain_routes":[{"project":"hapi","domains":["equity_trading"]}]}"#,
            )
            .expect("write routing.json");
            let server = bound_server(home, "quant");
            // "hapi" is never mounted here.

            let target =
                ContinuityEventTarget::new(DbScope::Project, Some("quant".to_string()), None);
            let entry = projection_entry("projection-2", Some("equity_trading"));
            let err = resolve_projection_write_target(
                &server,
                &target,
                entry.domain.as_deref(),
                false,
                false,
            )
            .expect_err("must refuse, not silently write cross-domain");
            assert_eq!(err.kind(), "unmounted_route");
            let message = err.to_string();
            assert!(message.contains("equity_trading"));
            assert!(message.contains("hapi"));
        });
    }

    /// Same-domain (unregistered-domain) projection content is unaffected —
    /// the ordinary case must not be disturbed by this gate.
    #[test]
    fn same_domain_projection_is_unaffected() {
        crate::test_support::with_tachi_home(|home| {
            let server = bound_server(home, "quant");
            let target =
                ContinuityEventTarget::new(DbScope::Project, Some("quant".to_string()), None);
            let entry = projection_entry("projection-3", Some("engineering"));
            let routed = resolve_projection_write_target(
                &server,
                &target,
                entry.domain.as_deref(),
                false,
                false,
            )
            .expect("resolve routed target");
            upsert_projection_memory(&server, &routed, &entry).expect("upsert projection memory");

            let in_quant = server
                .with_named_project_store_read("quant", |store| {
                    store.get(&entry.id).map_err(|e| e.to_string())
                })
                .expect("read quant");
            assert!(in_quant.is_some());
        });
    }

    /// A `db_path`-targeted projection (the background
    /// `ContinuityProjectionScheduler` sweep) is never scrutinized — it
    /// passes through unchanged even for mismatched, registered domain
    /// content, since source store == destination store by construction.
    #[test]
    fn db_path_target_skips_the_gate_entirely() {
        crate::test_support::with_tachi_home(|home| {
            std::fs::write(
                home.join("routing.json"),
                r#"{"domain_routes":[{"project":"hapi","domains":["equity_trading"]}]}"#,
            )
            .expect("write routing.json");
            let server = bound_server(home, "quant");
            let pinned_db = home.join("pinned").join("memory.db");
            std::fs::create_dir_all(pinned_db.parent().unwrap()).expect("mkdir pinned");

            let target =
                ContinuityEventTarget::new(DbScope::Project, None, Some(pinned_db.clone()));
            let entry = projection_entry("projection-4", Some("equity_trading"));
            let routed = resolve_projection_write_target(
                &server,
                &target,
                entry.domain.as_deref(),
                false,
                false,
            )
            .expect("resolve routed target (db_path skips unchanged)");
            upsert_projection_memory(&server, &routed, &entry).expect("upsert projection memory");

            let in_pinned = server
                .with_path_store_read(&pinned_db, |store| {
                    store.get(&entry.id).map_err(|e| e.to_string())
                })
                .expect("read pinned db");
            assert!(
                in_pinned.is_some(),
                "db_path target must land exactly where pinned, ungated"
            );
        });
    }

    /// #1114 codex round-1 B3 point ① discriminating test: a SECOND run over
    /// the SAME rerouted projection must find its own row at the ROUTED
    /// destination (via `resolve_projection_write_target` + `get_
    /// projection_memory` sharing the SAME resolved target), not silently
    /// treat it as brand-new every time. Before the B3 fix,
    /// `get_projection_memory` in `projection.rs`'s loop always read the
    /// STALE pre-gate `target` — a rerouted projection's aggregation state
    /// (seen/hit/miss counters) reset on every subsequent run because the
    /// existing-row lookup never found the row that had actually moved.
    #[test]
    fn rerouted_projection_is_found_on_a_second_run_not_recreated() {
        crate::test_support::with_tachi_home(|home| {
            std::fs::write(
                home.join("routing.json"),
                r#"{"domain_routes":[{"project":"hapi","domains":["equity_trading"]}]}"#,
            )
            .expect("write routing.json");
            let server = bound_server(home, "quant");
            let _hapi_db = mount_named_project_db(home, "hapi");

            let target =
                ContinuityEventTarget::new(DbScope::Project, Some("quant".to_string()), None);
            let entry = projection_entry("projection-5", Some("equity_trading"));

            // Round 1: fresh row, reroutes into hapi.
            let routed_1 = resolve_projection_write_target(
                &server,
                &target,
                entry.domain.as_deref(),
                false,
                false,
            )
            .expect("round 1 routed target");
            assert_eq!(routed_1.named_project.as_deref(), Some("hapi"));
            let existing_1 = super::get_projection_memory(&server, &routed_1, &entry.id)
                .expect("round 1 existing-row lookup");
            assert!(
                existing_1.is_none(),
                "round 1 is a genuinely fresh row: {existing_1:?}"
            );
            upsert_projection_memory(&server, &routed_1, &entry).expect("round 1 upsert");

            // Round 2: SAME event/projection/key -> same deterministic id.
            // The existing-row lookup must use the SAME routed target as
            // round 1 (hapi), and must find the row round 1 just wrote.
            let routed_2 = resolve_projection_write_target(
                &server,
                &target,
                entry.domain.as_deref(),
                false,
                false,
            )
            .expect("round 2 routed target");
            assert_eq!(
                routed_2.named_project.as_deref(),
                Some("hapi"),
                "round 2 must resolve to the SAME destination as round 1"
            );
            let existing_2 = super::get_projection_memory(&server, &routed_2, &entry.id)
                .expect("round 2 existing-row lookup");
            assert!(
                existing_2.is_some(),
                "round 2 must find round 1's row at the routed destination, \
                 not silently treat it as a fresh row"
            );
        });
    }

    /// #1114 codex round-1 B4-class discriminating test (proactive for
    /// continuity, same bug class B4 named for `capture_session.rs`): a
    /// projection whose row ALREADY exists at the pre-gate default (e.g.
    /// projected before its domain had any registered route) must be
    /// updated in place when that domain is LATER registered — not
    /// rerouted to the newly-registered store, which would split the same
    /// deterministic id across two stores.
    #[test]
    fn preexisting_projection_at_pretarget_updates_in_place_when_route_added_later() {
        crate::test_support::with_tachi_home(|home| {
            let server = bound_server(home, "quant");
            let target =
                ContinuityEventTarget::new(DbScope::Project, Some("quant".to_string()), None);
            let entry = projection_entry("projection-6", Some("equity_trading"));
            // The row already lives at "quant" — projected back when
            // `equity_trading` had no registered route at all.
            upsert_projection_memory(&server, &target, &entry)
                .expect("seed pre-existing row at the pre-gate target");

            // `equity_trading` is now registered to route to "hapi", and
            // "hapi" is mounted.
            std::fs::write(
                home.join("routing.json"),
                r#"{"domain_routes":[{"project":"hapi","domains":["equity_trading"]}]}"#,
            )
            .expect("write routing.json");
            let _hapi_db = mount_named_project_db(home, "hapi");

            let id_resolves_at_pretarget =
                super::get_projection_memory(&server, &target, &entry.id)
                    .expect("pretarget existence check")
                    .is_some();
            assert!(
                id_resolves_at_pretarget,
                "the seeded row must be found at the pre-gate target"
            );
            let routed = resolve_projection_write_target(
                &server,
                &target,
                entry.domain.as_deref(),
                false,
                id_resolves_at_pretarget,
            )
            .expect("resolve routed target");

            assert_eq!(
                routed.named_project.as_deref(),
                Some("quant"),
                "must update the row already at quant, not split it into a \
                 second copy at the newly-registered hapi"
            );
        });
    }

    // #1114 codex round-3 item 4 fix: the test that used to live here
    // (asserting `get_projection_memory` itself returns `Err` on a read
    // failure) was HOLLOW — that helper already propagated read errors at
    // the base level, unaffected by the #1114 fix, so the test passed
    // identically before and after. The line actually changed was one layer
    // up, in `projection.rs`'s loop (`id_resolves_at_pretarget = ...ok()
    // .flatten().is_some()` -> `?`) — moved to a real discriminating test
    // driving that layer's actual entry point:
    // `continuity_ops::tests::project_auto_continuity_events_propagates_a_downstream_read_failure`.

    // #1114 codex round-2 item 4①/③ KNOWN LIMITATION (registry changes its
    // route for a domain between two projection runs of the SAME
    // deterministic id): NOT reproduced as an integration test in THIS
    // module. A server-bound provider caches a successful config for that
    // daemon's lifetime, so a SECOND `routing.json` rewrite has no effect
    // on a SECOND `resolve_projection_write_target` call from that daemon.
    // The "domain routes to A, then later to B" scenario happens ACROSS a
    // restart. The underlying GATE MECHANISM'S lack of memory
    // across two calls with DIFFERENT configs — the actual thing that
    // would let the same id split across two stores — IS characterized at
    // the DI level, where config is a plain parameter with no caching
    // involved at all:
    // `memory_search_ops::save_memory::write_affinity::tests::
    // config_change_between_calls_reroutes_a_stable_id_to_a_different_store`.
    // Closing the gap for real requires persistent per-id "last known
    // location" tracking (a schema-level change), deferred alongside
    // #1115's check-then-insert atomicity work, not solved here.

    // #488: the TACHI_PROJECT pin must win over the git-hash db-path parent name
    // so direct-daemon continuity projections align with pinned reads/writes.
    #[test]
    fn pick_label_prefers_pin_over_git_hash_db_parent() {
        assert_eq!(
            pick_label(
                None,
                None,
                Some("trading"),
                Some("Quant_Analyzer_2026-b4773587"),
            ),
            Some("trading")
        );
    }

    #[test]
    fn pick_label_full_precedence_order() {
        // explicit param > named project > pin > db-path parent.
        assert_eq!(
            pick_label(
                Some("explicit"),
                Some("named"),
                Some("pin"),
                Some("dbparent")
            ),
            Some("explicit")
        );
        assert_eq!(
            pick_label(None, Some("named"), Some("pin"), Some("dbparent")),
            Some("named")
        );
        assert_eq!(
            pick_label(None, None, Some("pin"), Some("dbparent")),
            Some("pin")
        );
        // No pin -> db-path parent (git-hash) name — behavior unchanged.
        assert_eq!(
            pick_label(None, None, None, Some("Quant_Analyzer_2026-b4773587")),
            Some("Quant_Analyzer_2026-b4773587")
        );
        assert_eq!(pick_label(None, None, None, None), None);
    }

    #[test]
    fn pick_label_skips_blank_sources() {
        assert_eq!(
            pick_label(Some("   "), None, Some("trading"), None),
            Some("trading")
        );
        assert_eq!(pick_label(None, None, None, Some("  ")), None);
    }
}
