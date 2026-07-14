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
//! save (no client-supplied `id`), with no explicit `project=`, resolved to
//! this daemon's bound project store (not an explicit `scope=global`). An
//! explicit `project=` or `id=` is a deliberate placement decision and is
//! never second-guessed here. Fail-safe posture: an unregistered domain is
//! *uncertain*, not a violation — it proceeds unchanged (with a warning);
//! only a domain that IS registered to a *different, currently-unbound*
//! store is a confirmed mismatch, and that either reroutes (store mounted)
//! or refuses loudly (store not mounted) — it never silently lands wrong.

use super::entry::resolve_save_domain;
use crate::memory_search_ops::routing_config::RoutingConfig;
use crate::memory_search_ops::search_helpers::{bound_project_label, named_project_db_exists};
use crate::tool_params::SaveMemoryParams;
use crate::{DbScope, MemoryServer};

/// What the gate did, surfaced back to the caller for transparency (never
/// itself an error — an `Err` result is the separate, loud-refusal path).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::memory_search_ops::save_memory) enum AffinityNote {
    /// The domain has no registered route — nothing to compare against, so
    /// the requested target was left untouched (uncertain, not wrong).
    Unregistered { domain: String },
    /// The domain is registered to a different, mounted store; the save was
    /// rerouted there instead of the originally-resolved target.
    Rerouted { domain: String, project: String },
}

#[derive(Debug)]
pub(in crate::memory_search_ops::save_memory) struct AffinityOutcome {
    pub target_db: DbScope,
    pub named_project: Option<String>,
    pub note: Option<AffinityNote>,
}

/// Apply the write-affinity gate against the live process state (real
/// `RoutingConfig::get()`, the real daemon-bound project label, and a real
/// filesystem `named_project_db_exists` check). See
/// [`apply_write_affinity_with`] for the injectable core used by tests.
pub(in crate::memory_search_ops::save_memory) fn apply_write_affinity(
    server: &MemoryServer,
    params: &SaveMemoryParams,
    target_db: DbScope,
    named_project: Option<&str>,
) -> Result<AffinityOutcome, String> {
    apply_write_affinity_with(
        params,
        target_db,
        named_project,
        RoutingConfig::get(),
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
    config: &RoutingConfig,
    current_label: Option<String>,
    project_exists: F,
) -> Result<AffinityOutcome, String>
where
    F: Fn(&str) -> bool,
{
    let passthrough = || AffinityOutcome {
        target_db,
        named_project: named_project.map(str::to_string),
        note: None,
    };

    // Only the ambiguous default path is in scope: no explicit project=, no
    // client-supplied id (a genuinely new save, not a patch to a row that
    // already lives wherever it lives), and resolved to the bound project
    // store rather than an explicit scope=global.
    if named_project.is_some() || params.id.is_some() || target_db != DbScope::Project {
        return Ok(passthrough());
    }

    let domain = match resolve_save_domain(params.domain.clone(), &params.path, &params.category) {
        Some(d) if !d.trim().is_empty() => d,
        _ => return Ok(passthrough()),
    };

    let route = config.domain_routes.iter().find(|route| {
        route
            .domains
            .iter()
            .any(|d| d.eq_ignore_ascii_case(&domain))
    });

    let Some(route) = route else {
        tracing::warn!(
            domain = %domain,
            "save_memory: domain has no registered store route; proceeding with the requested target (uncertain classification, fail-safe permissive)"
        );
        return Ok(AffinityOutcome {
            target_db,
            named_project: None,
            note: Some(AffinityNote::Unregistered { domain }),
        });
    };

    if current_label
        .as_deref()
        .is_some_and(|c| c.eq_ignore_ascii_case(&route.project))
    {
        // Already the registered store — no mismatch.
        return Ok(passthrough());
    }

    if project_exists(&route.project) {
        tracing::warn!(
            domain = %domain,
            from = current_label.as_deref().unwrap_or("<unbound>"),
            to = %route.project,
            "save_memory: domain-store affinity mismatch — rerouting to the registered store"
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

    Err(format!(
        "save refused: domain '{domain}' is registered to project store '{store}', which is not mounted on this daemon (bound store: {bound}). Refusing to silently write cross-domain into the bound store. Mount/register '{store}', or pass an explicit project= to override.",
        domain = domain,
        store = route.project,
        bound = current_label.as_deref().unwrap_or("<none>"),
    ))
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
        params.domain = Some("equity_trading".to_string());
        let outcome = apply_write_affinity_with(
            &params,
            DbScope::Project,
            Some("hapi"),
            &trading_routed_config(),
            Some("quant".to_string()),
            |_| true,
        )
        .unwrap();
        assert!(outcome.note.is_none());
        assert_eq!(outcome.named_project.as_deref(), Some("hapi"));
    }

    #[test]
    fn skips_when_explicit_id_given() {
        let mut params = base_params();
        params.id = Some("existing-id".to_string());
        params.domain = Some("equity_trading".to_string());
        let outcome = apply_write_affinity_with(
            &params,
            DbScope::Project,
            None,
            &trading_routed_config(),
            Some("quant".to_string()),
            |_| true,
        )
        .unwrap();
        assert!(outcome.note.is_none());
        assert_eq!(outcome.target_db, DbScope::Project);
        assert!(outcome.named_project.is_none());
    }

    #[test]
    fn skips_when_scope_is_global() {
        let mut params = base_params();
        params.domain = Some("equity_trading".to_string());
        let outcome = apply_write_affinity_with(
            &params,
            DbScope::Global,
            None,
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

    #[test]
    fn matching_current_store_is_a_noop() {
        let mut params = base_params();
        params.domain = Some("equity_trading".to_string());
        let outcome = apply_write_affinity_with(
            &params,
            DbScope::Project,
            None,
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
            &trading_routed_config(),
            Some("quant".to_string()),
            |_| false,
        );
        let err = result.expect_err("must refuse, not silently save cross-domain");
        assert!(err.contains("equity_trading"));
        assert!(err.contains("hapi"));
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
}
