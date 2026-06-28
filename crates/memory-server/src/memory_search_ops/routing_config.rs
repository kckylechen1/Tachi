//! Project-routing and cross-domain filter rules, as data.
//!
//! The generic Tachi package must not ship product-specific recall routes.
//! Domain packs or forks can opt into routes by writing `~/.tachi/routing.json`;
//! the built-in defaults stay empty so coding-agent recall is domain-agnostic.

use serde::Deserialize;
use std::path::PathBuf;
use std::sync::OnceLock;

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

impl RoutingConfig {
    /// Process-wide config, loaded once from `~/.tachi/routing.json` (falling
    /// back to the behavior-preserving defaults on a missing or invalid file).
    pub(crate) fn get() -> &'static RoutingConfig {
        static CONFIG: OnceLock<RoutingConfig> = OnceLock::new();
        CONFIG.get_or_init(Self::load)
    }

    fn load() -> RoutingConfig {
        let Some(path) = Self::config_path() else {
            return Self::default();
        };
        match std::fs::read_to_string(&path) {
            Ok(body) => serde_json::from_str(&body).unwrap_or_else(|err| {
                tracing::warn!(
                    "invalid routing config {}: {err}; using defaults",
                    path.display()
                );
                Self::default()
            }),
            // No override file -> generic, domain-agnostic defaults.
            Err(_) => Self::default(),
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
}
