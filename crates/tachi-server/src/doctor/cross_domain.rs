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

/// How many example ids to keep per store — enough to spot-check, small
/// enough to stay cheap and keep the report readable.
pub(super) const SUSPECT_SAMPLE_LIMIT: usize = 5;

pub(super) fn probe(conn: &rusqlite::Connection) -> Option<memcore::db::KeywordSuspectProbe> {
    memcore::db::probe_keyword_suspects(conn, TRADING_SUSPECT_KEYWORDS, SUSPECT_SAMPLE_LIMIT).ok()
}
