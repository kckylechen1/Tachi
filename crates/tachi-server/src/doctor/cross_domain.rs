//! #1041 S4 — per-store cross-domain suspect tripwire.
//!
//! `tachi doctor` already flags `none_domain_count` per store. This adds an
//! informational-only counterpart: rows whose text/summary/path contain an
//! obvious trading-vocabulary keyword, surfaced per store so an engineering
//! store slowly absorbing trading content (or vice versa) shows up in the
//! doctor report before it grows into hundreds of rows, instead of only
//! being caught by a bulk audit after the fact.
//!
//! This is a heuristic, never a classifier and never a gate: it never blocks
//! a scan, never renames/moves anything, and a false positive costs nothing
//! but an operator glancing at a sample id. The keyword set intentionally
//! overlaps `memory-server-rescue`'s `classify::kw_trading` (the re-homing
//! migration's own trading-vocabulary heuristic) but is NOT wired to that
//! crate: `doctor` is unconditionally compiled (`tachi-server/src/lib.rs`'s
//! `mod doctor;` carries no feature gate) while `memory-server-rescue` is an
//! optional `full`-profile-only operator crate — hard-depending on it here
//! would make doctor, a basic DB-hygiene tool, un-compilable once the
//! `portable` profile's source gating (#924) actually lands.

/// Trading-vocabulary substrings used to flag suspect rows. Deliberately
/// narrow (mirrors the "narrow to avoid misrouting" comment on
/// `memory-server-rescue::rescue::classify`'s own keyword fallback) — this
/// is a tripwire, not a domain classifier, so a missed hit is far cheaper
/// than a false-positive flood burying real signal.
pub(super) const TRADING_SUSPECT_KEYWORDS: &[&str] = &[
    "持仓",
    "买入",
    "卖出",
    "止损",
    "止盈",
    "回测",
    "trading agent",
    "stock symbol",
    "ticker",
];

/// #1041 F7: the mirror-image tripwire for a store registered to a
/// NON-engineering domain (today, only "trading" routes exist) — content
/// that shouldn't be there is engineering vocabulary, not trading
/// vocabulary. Kept narrow for the same reason as `TRADING_SUSPECT_KEYWORDS`.
pub(super) const ENGINEERING_SUSPECT_KEYWORDS: &[&str] = &[
    "cargo build",
    "rustc",
    "git commit",
    "pull request",
    "clippy",
    "unit test",
    "stack trace",
];

/// How many example ids to keep per store — enough to spot-check, small
/// enough to stay cheap and keep the report readable.
pub(super) const SUSPECT_SAMPLE_LIMIT: usize = 5;

/// #1041 F7 fix: which keyword vocabulary is the CROSS-domain tripwire for
/// `scope_hint`'s store. Before this, every store — a healthy trading
/// store, a healthy engineering store, global, all of them — was scanned
/// with the SAME trading-only wordlist, so a healthy trading store flagged
/// its own normal rows as "suspect" while an engineering store slowly
/// absorbing trading content stayed invisible (nothing was ever scanned
/// FOR engineering vocabulary). This looks up the store's REGISTERED domain
/// via `RoutingConfig::domain_routes` (the same registry the write-affinity
/// gate itself reads) and picks the OPPOSITE vocabulary — trading-registered
/// stores get scanned for engineering leakage and vice versa. A store with
/// no registered domain at all (the common case for a generic, no-routing
/// -config install) has no defined "foreign" side, so `None` here means
/// "skip", not "scan with the trading default" — the caller surfaces that
/// as `cross_domain_suspect_count: None` ("not evaluated"), never a
/// misleading `Some(0)` ("evaluated, clean").
/// Config-injectable core (mirrors `write_affinity::apply_write_affinity_with`'s
/// DI shape) so this stays unit-testable without touching a process-global
/// cache or `~/.tachi/routing.json`.
fn suspect_keywords_for_with(
    scope_hint: &str,
    config: &crate::memory_search_ops::routing_config::RoutingConfig,
) -> Option<&'static [&'static str]> {
    let project = scope_hint.strip_prefix("project:")?;
    let mut registered_domains = config
        .domain_routes
        .iter()
        .filter(|route| route.project.eq_ignore_ascii_case(project))
        .flat_map(|route| route.domains.iter().map(String::as_str))
        .peekable();
    registered_domains.peek()?;
    let is_trading_domain = registered_domains.any(|d| d.to_ascii_lowercase().contains("trad"));
    Some(if is_trading_domain {
        ENGINEERING_SUSPECT_KEYWORDS
    } else {
        TRADING_SUSPECT_KEYWORDS
    })
}

pub(super) fn probe(
    conn: &rusqlite::Connection,
    scope_hint: &str,
    config: &crate::memory_search_ops::routing_config::RoutingConfig,
) -> Option<memcore::db::KeywordSuspectProbe> {
    let keywords = suspect_keywords_for_with(scope_hint, config)?;
    memcore::db::probe_keyword_suspects(conn, keywords, SUSPECT_SAMPLE_LIMIT).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory_search_ops::routing_config::{DomainRoute, RoutingConfig};

    fn config_with(project: &str, domains: &[&str]) -> RoutingConfig {
        RoutingConfig {
            domain_routes: vec![DomainRoute {
                project: project.to_string(),
                domains: domains.iter().map(|d| d.to_string()).collect(),
            }],
            ..Default::default()
        }
    }

    #[test]
    fn unregistered_project_skips_the_probe_entirely() {
        assert_eq!(
            suspect_keywords_for_with("project:sigil", &RoutingConfig::default()),
            None,
            "no registered domain -> no defined foreign side -> skip, not the trading default"
        );
    }

    #[test]
    fn non_project_scope_hint_skips_the_probe() {
        assert_eq!(
            suspect_keywords_for_with("global", &config_with("global", &["equity_trading"])),
            None,
            "domain_routes keys by named-project, not the shared global store"
        );
    }

    #[test]
    fn trading_registered_store_is_scanned_for_engineering_leakage() {
        let config = config_with("hapi", &["equity_trading"]);
        let keywords = suspect_keywords_for_with("project:hapi", &config)
            .expect("registered domain must select a vocabulary");
        assert_eq!(keywords, ENGINEERING_SUSPECT_KEYWORDS);
    }

    #[test]
    fn non_trading_registered_store_is_scanned_for_trading_leakage() {
        let config = config_with("sigil", &["engineering"]);
        let keywords = suspect_keywords_for_with("project:sigil", &config)
            .expect("registered domain must select a vocabulary");
        assert_eq!(keywords, TRADING_SUSPECT_KEYWORDS);
    }
}
