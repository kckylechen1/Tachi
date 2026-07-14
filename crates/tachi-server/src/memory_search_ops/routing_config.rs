//! Project-routing and cross-domain filter rules, as data.
//!
//! The generic Tachi package must not ship product-specific recall routes.
//! Domain packs or forks can opt into routes by writing `~/.tachi/routing.json`;
//! the built-in defaults stay empty so coding-agent recall is domain-agnostic.

use serde::Deserialize;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

/// #1041 F4: a REAL routing-config load failure (file exists but is
/// unreadable, or contains invalid JSON) — distinct from "no override file",
/// which is the documented, behavior-preserving empty-table default (see
/// module doc). Surfaced so the write-affinity gate can refuse a write
/// rather than silently treat a broken config as "no routes registered"
/// (that used to fail OPEN: a corrupt `routing.json` disabled the S1 gate
/// entirely, permissively, with only a `tracing::warn!`).
#[derive(Debug, Clone)]
pub(crate) enum RoutingConfigError {
    Unreadable { path: PathBuf, detail: String },
    Invalid { path: PathBuf, detail: String },
}

impl std::fmt::Display for RoutingConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RoutingConfigError::Unreadable { path, detail } => write!(
                f,
                "routing config {} is unreadable: {detail}",
                path.display()
            ),
            RoutingConfigError::Invalid { path, detail } => write!(
                f,
                "routing config {} is invalid JSON: {detail}",
                path.display()
            ),
        }
    }
}

/// Route a query to `project` when it contains one of `terms` (lowercased
/// substrings), provided that project's DB exists.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct ProjectRoute {
    pub project: String,
    pub terms: Vec<String>,
}

/// Route a query to `project` when the caller-supplied `domain` matches one of
/// `domains` (case-insensitive).
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct DomainRoute {
    pub project: String,
    pub domains: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub(crate) struct RoutingConfig {
    /// Domain -> project routing (e.g. `domain_pack` -> `domain_pack_project`).
    pub domain_routes: Vec<DomainRoute>,
    /// When set, a bare 6-digit token in the query routes to this project
    /// (the A-share ticker heuristic). `None` disables ticker routing entirely.
    pub ticker_route_project: Option<String>,
    /// Term → project routing for query text.
    pub project_routes: Vec<ProjectRoute>,
    /// Whole-word terms that opt a query INTO foreign-domain recall for the
    /// sigil project (otherwise foreign-domain rows are filtered out of sigil
    /// searches). Matched against alphanumeric word splits.
    pub foreign_domain_word_terms: Vec<String>,
    /// CJK / numeric substrings that opt a query into foreign-domain recall
    /// (CJK is not whitespace-delimited, so these match as raw substrings).
    pub foreign_domain_substring_terms: Vec<String>,
    /// `entry.domain` values that mark a memory as foreign to the sigil project.
    pub foreign_domains: Vec<String>,
    /// `entry.path` prefixes that mark a memory as foreign to the sigil project.
    pub foreign_path_prefixes: Vec<String>,
}

impl Default for RoutingConfig {
    fn default() -> Self {
        Self {
            domain_routes: Vec::new(),
            ticker_route_project: None,
            project_routes: Vec::new(),
            foreign_domain_word_terms: Vec::new(),
            foreign_domain_substring_terms: Vec::new(),
            foreign_domains: Vec::new(),
            foreign_path_prefixes: Vec::new(),
        }
    }
}

/// #1041 B4 (codex round-4): `get()` and `get_checked()` used to hold
/// SEPARATE `OnceLock`s. Both permanently cache a SUCCESSFUL load, but only
/// `get_checked()` retried on failure — `get()` permanently cached
/// `RoutingConfig::default()` the first time it observed a broken file. If a
/// write happened while the file was broken (via `get_checked()`, which
/// doesn't cache the failure) and then the file got fixed, the NEXT write
/// picked up the fix immediately (through `get_checked()`'s own cache) and
/// started correctly rerouting content to its registered domain store — but
/// every READ (`get()`, used by search/recall routing) stayed stuck on the
/// empty-defaults snapshot cached during the broken window, for the rest of
/// the process lifetime. Content silently became unfindable: written to the
/// correct store, searched for in the wrong one, with no daemon restart able
/// to fix it short of a full process restart.
///
/// The fix: ONE shared cache for both callers. Only a SUCCESSFUL load is
/// ever written to it (matching `get_checked()`'s pre-B4 contract); an error
/// is never cached, by either caller, so the very next call — whichever side
/// makes it — retries the load and, on success, becomes the fixed value both
/// sides see from then on. `get()` (read-side) always falls back to
/// `last_good` on a still-broken file (there is no "last good" until the
/// first success, so that's `RoutingConfig::default()` until then);
/// `get_checked()` (write-side) surfaces the error instead, per the #1041 F4
/// fail-loud contract.
fn cached_or_reload_with(
    cache: &OnceLock<Arc<RoutingConfig>>,
    load: impl FnOnce() -> Result<RoutingConfig, RoutingConfigError>,
) -> Result<Arc<RoutingConfig>, RoutingConfigError> {
    if let Some(cfg) = cache.get() {
        return Ok(cfg.clone());
    }
    let cfg = Arc::new(load()?);
    // Another thread may have raced us and already set it — that's a
    // harmless double-read of the identical file, not a correctness issue.
    // Either way, use whatever actually landed in the cache.
    let _ = cache.set(cfg.clone());
    Ok(cache.get().cloned().unwrap_or(cfg))
}

/// #1041 B4: the ONE process-wide cache slot shared by both [`RoutingConfig::get`]
/// and [`RoutingConfig::get_checked`] — a module-level `static`, not one
/// nested inside each function, is exactly what makes them share it. Only a
/// successful load is ever stored here; see `cached_or_reload_with`'s doc for
/// why a shared cache (versus each caller's own) is the actual fix.
static ROUTING_CONFIG_CACHE: OnceLock<Arc<RoutingConfig>> = OnceLock::new();

impl RoutingConfig {
    /// Process-wide config, sharing [`ROUTING_CONFIG_CACHE`] with
    /// [`Self::get_checked`] (see that static's doc for why). Falls back to
    /// the behavior-preserving defaults on a missing file, or degrades a
    /// real load error to defaults with a warning — read-side callers, e.g.
    /// recall routing, must keep working even if the file is momentarily
    /// broken. Write-side callers that need the #1041 F4 fail-loud contract
    /// (a broken file must refuse a write, not silently disable the
    /// affinity gate) use [`Self::get_checked`] instead.
    pub(crate) fn get() -> Arc<RoutingConfig> {
        cached_or_reload_with(&ROUTING_CONFIG_CACHE, Self::load).unwrap_or_else(|err| {
            tracing::warn!("{err}; using defaults (not cached — retried on the next call)");
            Arc::new(Self::default())
        })
    }

    /// Like [`Self::get`], but surfaces a real load error instead of
    /// silently degrading to an empty route table. A missing file is still
    /// not an error (see [`Self::load`]). Errors are never cached: a load
    /// failure is retried on the very next call (e.g. once the file is
    /// repaired), so recovering does not require a daemon restart — only a
    /// SUCCESSFUL load is cached for the process lifetime, and shared with
    /// [`Self::get`] via [`ROUTING_CONFIG_CACHE`] (#1041 B4).
    pub(crate) fn get_checked() -> Result<Arc<RoutingConfig>, RoutingConfigError> {
        cached_or_reload_with(&ROUTING_CONFIG_CACHE, Self::load)
    }

    fn load() -> Result<RoutingConfig, RoutingConfigError> {
        let Some(path) = Self::config_path() else {
            return Ok(Self::default());
        };
        match std::fs::read_to_string(&path) {
            Ok(body) => serde_json::from_str(&body).map_err(|err| RoutingConfigError::Invalid {
                path,
                detail: err.to_string(),
            }),
            // No override file -> generic, domain-agnostic defaults. This is
            // the ONLY io::Error kind treated as "nothing configured" —
            // every other error (permission denied, I/O failure) is a real
            // problem the caller must know about, not silence.
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(err) => Err(RoutingConfigError::Unreadable {
                path,
                detail: err.to_string(),
            }),
        }
    }

    fn config_path() -> Option<PathBuf> {
        // Only resolve home_dir() when actually needed, so an absolute
        // TACHI_HOME still works in sandboxed/headless environments where
        // home_dir() returns None.
        let app_home = match std::env::var("TACHI_HOME") {
            Ok(home) => match home.strip_prefix("~/") {
                Some(rest) => dirs::home_dir()?.join(rest),
                None => PathBuf::from(home),
            },
            Err(_) => dirs::home_dir()?.join(".tachi"),
        };
        Some(app_home.join("routing.json"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_domain_agnostic() {
        let config = RoutingConfig::default();
        assert!(config.domain_routes.is_empty());
        assert!(config.ticker_route_project.is_none());
        assert!(config.project_routes.is_empty());
        assert!(config.foreign_domain_word_terms.is_empty());
        assert!(config.foreign_domain_substring_terms.is_empty());
        assert!(config.foreign_domains.is_empty());
        assert!(config.foreign_path_prefixes.is_empty());
    }

    #[test]
    fn empty_config_disables_domain_routing() {
        let json = r#"{"ticker_route_project": null}"#;
        let config: RoutingConfig = serde_json::from_str(json).unwrap();
        // serde(default) fills the rest, but an explicit override here proves the
        // ticker heuristic can be turned off entirely via config.
        assert!(config.ticker_route_project.is_none());
    }

    // #1096 leaf-2a: local `with_test_tachi_home` replaced by the shared,
    // panic-safe `crate::test_support::with_tachi_home`.
    use crate::test_support::with_tachi_home as with_test_tachi_home;

    // #1041 F4 regression: `load()` must distinguish "no override file" (a
    // legitimate empty table) from a REAL load failure (unreadable /
    // invalid JSON) — the write-affinity gate refuses on the latter instead
    // of silently treating a broken config as "no routes registered".
    #[test]
    fn load_missing_file_is_not_an_error() {
        with_test_tachi_home(|_home| {
            let config = RoutingConfig::load().expect("missing file must be Ok(default)");
            assert!(config.domain_routes.is_empty());
        });
    }

    #[test]
    fn load_invalid_json_is_a_loud_error() {
        with_test_tachi_home(|home| {
            std::fs::write(home.join("routing.json"), b"{ not valid json").expect("write");
            let err = RoutingConfig::load().expect_err("invalid JSON must be Err, not defaults");
            assert!(matches!(err, RoutingConfigError::Invalid { .. }));
        });
    }

    #[cfg(unix)]
    #[test]
    fn load_unreadable_file_is_a_loud_error() {
        use std::os::unix::fs::PermissionsExt;
        with_test_tachi_home(|home| {
            let path = home.join("routing.json");
            std::fs::write(&path, br#"{"domain_routes": []}"#).expect("write");
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).expect("chmod");
            // Root (common in CI containers) ignores unix perm bits entirely —
            // skip rather than false-fail on a permission model that can't
            // reproduce the unreadable case at all.
            let is_root = std::fs::read_to_string(&path).is_ok();
            if !is_root {
                let err =
                    RoutingConfig::load().expect_err("unreadable file must be Err, not defaults");
                assert!(matches!(err, RoutingConfigError::Unreadable { .. }));
            }
            let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644));
        });
    }

    #[test]
    fn load_valid_config_succeeds() {
        with_test_tachi_home(|home| {
            std::fs::write(
                home.join("routing.json"),
                br#"{"domain_routes": [{"project": "hapi", "domains": ["equity_trading"]}]}"#,
            )
            .expect("write");
            let config = RoutingConfig::load().expect("valid JSON must load");
            assert_eq!(config.domain_routes.len(), 1);
            assert_eq!(config.domain_routes[0].project, "hapi");
        });
    }

    // ── #1041 B4: shared read/write cache consistency ───────────────────────
    //
    // These exercise `cached_or_reload_with` directly against a LOCAL
    // `OnceLock`, not the real process-global `ROUTING_CONFIG_CACHE` —
    // `cargo test` runs every test in one process, so asserting on the real
    // static would be poisoned by whichever other test in this binary
    // happens to touch `RoutingConfig::get`/`get_checked` first. The
    // injectable core is exactly what makes the invariant testable at all
    // (same DI shape as `apply_write_affinity`/`apply_write_affinity_with`).

    #[test]
    fn error_is_never_cached_and_retries_next_call() {
        let cache: OnceLock<Arc<RoutingConfig>> = OnceLock::new();
        let bad = || {
            Err(RoutingConfigError::Invalid {
                path: PathBuf::from("/fake/routing.json"),
                detail: "still broken".to_string(),
            })
        };
        assert!(cached_or_reload_with(&cache, bad).is_err());
        assert!(
            cache.get().is_none(),
            "a failed load must not be cached — the next call must retry"
        );
        assert!(
            cached_or_reload_with(&cache, bad).is_err(),
            "still broken -> still an error on the very next call"
        );
    }

    /// #1041 B4 core regression: a bad file, then a fix, must be visible to
    /// BOTH a `get()`-shaped caller (never errors, falls back to defaults
    /// while broken) and a `get_checked()`-shaped caller (surfaces the
    /// error while broken) — and once fixed, both must see the IDENTICAL
    /// repaired config, with no restart, because they share one cache.
    #[test]
    fn bad_file_then_fixed_file_is_visible_to_both_read_and_write_without_restart() {
        let cache: OnceLock<Arc<RoutingConfig>> = OnceLock::new();
        let bad = || {
            Err(RoutingConfigError::Invalid {
                path: PathBuf::from("/fake/routing.json"),
                detail: "broken".to_string(),
            })
        };
        let fixed = || {
            Ok(RoutingConfig {
                domain_routes: vec![DomainRoute {
                    project: "hapi".to_string(),
                    domains: vec!["equity_trading".to_string()],
                }],
                ..RoutingConfig::default()
            })
        };

        // While broken: the write-side shape (get_checked) sees the error...
        assert!(cached_or_reload_with(&cache, bad).is_err());
        // ...and the read-side shape (get) falls back to defaults, exactly
        // like `RoutingConfig::get`'s own `unwrap_or_else`.
        let read_during_break = cached_or_reload_with(&cache, bad)
            .unwrap_or_else(|_| Arc::new(RoutingConfig::default()));
        assert!(read_during_break.domain_routes.is_empty());

        // File gets fixed. The next call from EITHER side reloads and caches
        // the fix — simulate the write-side calling first.
        let write_after_fix = cached_or_reload_with(&cache, fixed).expect("fixed file must load");
        assert_eq!(write_after_fix.domain_routes.len(), 1);
        assert_eq!(write_after_fix.domain_routes[0].project, "hapi");

        // The read-side, calling AFTER, must see the SAME cached fix — not
        // re-read (this closure would panic if called: `unreachable!()`),
        // proving it came from the shared cache, not a fresh load.
        let read_after_fix = cached_or_reload_with(&cache, || {
            unreachable!("cache must already be populated by the write-side's successful load")
        })
        .expect("shared cache must already hold the fixed config");
        assert_eq!(read_after_fix.domain_routes.len(), 1);
        assert_eq!(read_after_fix.domain_routes[0].project, "hapi");
        assert!(
            Arc::ptr_eq(&write_after_fix, &read_after_fix),
            "read and write must observe the literal same cached Arc, not \
             two independently-loaded copies"
        );
    }
}
