use std::path::PathBuf;

use memcore::{ContinuityMetrics, MemoryEdge, MemoryEntry, TachiEventQuery, TachiEventRecord};
use memory_server_runtime::EventDbRoute;

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
                .insert_tachi_event(event)
                .map_err(|e| format!("insert continuity event: {e}"))
        })
    } else if let Some(db_path) = target.db_path.as_ref() {
        server.with_path_store(db_path, |store| {
            store
                .insert_tachi_event(event)
                .map_err(|e| format!("insert continuity event: {e}"))
        })
    } else {
        server.with_store_for_scope(target.target_db, |store| {
            store
                .insert_tachi_event(event)
                .map_err(|e| format!("insert continuity event: {e}"))
        })
    }
}

pub(super) fn read_events(
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

/// #1114 (write_affinity module doc's F1 note): purpose-built call into the
/// S1 write-affinity gate's DI core for continuity projection — the
/// audited original cross-domain-drift source that bypassed
/// `handle_save_memory`'s gate entirely by resolving its own store here.
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
pub(super) fn upsert_projection_memory(
    server: &MemoryServer,
    target: &ContinuityEventTarget,
    entry: &MemoryEntry,
    project_explicit: bool,
    id_resolves_at_target: bool,
) -> Result<(), String> {
    let gated = if target.db_path.is_none() {
        let affinity = crate::memory_search_ops::save_memory::write_affinity::apply_write_affinity_for_domain(
            server,
            entry.domain.as_deref(),
            target.target_db,
            target.named_project.as_deref(),
            project_explicit,
            id_resolves_at_target,
        )?;
        Some(ContinuityEventTarget {
            target_db: affinity.target_db,
            named_project: affinity.named_project,
            db_path: None,
        })
    } else {
        None
    };
    let target = gated.as_ref().unwrap_or(target);

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

pub(super) fn add_memory_edge(
    server: &MemoryServer,
    target: &ContinuityEventTarget,
    edge: &MemoryEdge,
) -> Result<(), String> {
    if let Some(project_name) = target.named_project.as_deref() {
        server.with_named_project_store(project_name, |store| {
            store
                .add_edge(edge)
                .map_err(|e| format!("add continuity graph edge: {e}"))
        })
    } else if let Some(db_path) = target.db_path.as_ref() {
        server.with_path_store(db_path, |store| {
            store
                .add_edge(edge)
                .map_err(|e| format!("add continuity graph edge: {e}"))
        })
    } else {
        server.with_store_for_scope(target.target_db, |store| {
            store
                .add_edge(edge)
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
    use super::{upsert_projection_memory, ContinuityEventTarget};
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
            last_access: None,
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
        let project_db = home
            .join("projects")
            .join(bound_project)
            .join("memory.db");
        std::fs::create_dir_all(project_db.parent().unwrap()).expect("mkdir project");
        let global_db = home.join("global").join("memory.db");
        std::fs::create_dir_all(global_db.parent().unwrap()).expect("mkdir global");
        // `MemoryServer::new(global, project)` — the PROJECT path (second
        // arg) is what `bound_project_label` resolves the daemon's own bound
        // name from via the Plan C `projects/<name>/memory.db` convention.
        MemoryServer::new(global_db, Some(project_db)).expect("bind daemon")
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
            server
                .with_named_project_store("hapi", |_store| Ok::<(), String>(()))
                .expect("create hapi store");

            let target = ContinuityEventTarget::new(
                DbScope::Project,
                Some("quant".to_string()), // transport-injected default == bound project
                None,
            );
            let entry = projection_entry("projection-1", Some("equity_trading"));
            upsert_projection_memory(&server, &target, &entry, false, false)
                .expect("upsert projection memory");

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
            let err = upsert_projection_memory(&server, &target, &entry, false, false)
                .expect_err("must refuse, not silently write cross-domain");
            assert!(err.contains("equity_trading"));
            assert!(err.contains("hapi"));
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
            upsert_projection_memory(&server, &target, &entry, false, false)
                .expect("upsert projection memory");

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

            let target = ContinuityEventTarget::new(DbScope::Project, None, Some(pinned_db.clone()));
            let entry = projection_entry("projection-4", Some("equity_trading"));
            upsert_projection_memory(&server, &target, &entry, false, false)
                .expect("upsert projection memory");

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
