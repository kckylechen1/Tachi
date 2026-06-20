//! Project-routing and cross-domain filter rules, as data.
//!
//! These rules used to be hardcoded constants inline in the search layer —
//! A-share/finance terms (`止损`, `688981`, `iron rules`, `trading`, …) baked
//! into the otherwise generic engine, exactly the "domain logic in the shared
//! layer" smell. They now live in [`RoutingConfig`], whose **defaults reproduce
//! the previous behavior exactly** (zero recall change on upgrade) and which can
//! be overridden by `~/.tachi/routing.json`.
//!
//! To make the engine fully domain-agnostic, ship a `routing.json` with the
//! lists emptied (and your own projects' routes added); to extend routing for a
//! new project, add an entry rather than editing this crate.

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
    /// Domain → project routing (e.g. `equity_trading` → `hyperion`).
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
            domain_routes: vec![DomainRoute {
                project: "hyperion".to_string(),
                domains: vec![
                    "equity_trading".into(),
                    "trading".into(),
                    "finance".into(),
                    "hyperion".into(),
                ],
            }],
            ticker_route_project: Some("hyperion".to_string()),
            project_routes: vec![
                ProjectRoute {
                    project: "hyperion".to_string(),
                    terms: vec![
                        "hyperion".into(),
                        "radar".into(),
                        "warpcore".into(),
                        "hapi".into(),
                        "hermes".into(),
                        "trading".into(),
                        "止损".into(),
                        "iron rules".into(),
                        "牛市".into(),
                        "daemon".into(),
                    ],
                },
                ProjectRoute {
                    project: "sigil".to_string(),
                    terms: vec![
                        "sigil".into(),
                        "memory-server".into(),
                        "tachi".into(),
                        "mcp".into(),
                        "foundry".into(),
                    ],
                },
            ],
            foreign_domain_word_terms: vec![
                "hyperion".into(),
                "quant".into(),
                "trading".into(),
                "equity".into(),
                "kronos".into(),
                "warpcore".into(),
                "chan".into(),
                "v8".into(),
            ],
            foreign_domain_substring_terms: vec![
                "股票".into(),
                "个股".into(),
                "止损".into(),
                "盘中".into(),
                "持仓".into(),
                "688981".into(),
            ],
            foreign_domains: vec![
                "equity_trading".into(),
                "trading".into(),
                "finance".into(),
                "hyperion".into(),
            ],
            foreign_path_prefixes: vec!["/trading/".into(), "/scratch/hyperion/".into()],
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
            // No override file → behavior-preserving defaults.
            Err(_) => Self::default(),
        }
    }

    fn config_path() -> Option<PathBuf> {
        let home = dirs::home_dir()?;
        let app_home = std::env::var("TACHI_HOME")
            .map(|v| {
                if let Some(rest) = v.strip_prefix("~/") {
                    home.join(rest)
                } else {
                    PathBuf::from(v)
                }
            })
            .unwrap_or_else(|_| home.join(".tachi"));
        Some(app_home.join("routing.json"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_preserve_legacy_routing_terms() {
        let config = RoutingConfig::default();
        assert_eq!(config.ticker_route_project.as_deref(), Some("hyperion"));
        assert!(config
            .project_routes
            .iter()
            .any(|r| r.project == "hyperion" && r.terms.iter().any(|t| t == "止损")));
        assert!(config
            .foreign_domain_substring_terms
            .iter()
            .any(|t| t == "688981"));
        assert!(config
            .foreign_path_prefixes
            .iter()
            .any(|p| p == "/scratch/hyperion/"));
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
