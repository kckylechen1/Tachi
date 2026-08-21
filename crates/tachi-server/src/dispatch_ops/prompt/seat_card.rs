//! L2 packet projection: inline a `/cards/<seat>` lane-card's counter-clause
//! section into the dispatch prompt (tachi#1202/#993).
//!
//! ## Frozen L1/L2 contract (owner-ratified, 2026-07-17)
//!
//! - **Storage (L1, NOT this module's job)**: an out-of-band sync ("ingest
//!   mirror") reads `~/.agents/dispatch-ledger/cards/<seat>.md` — the single
//!   source of truth per the owner's ruling on tachi#1202 — and mirrors each
//!   card into the GLOBAL store as a wiki-class [`memcore::MemoryEntry`] at
//!   `path = "/cards/<seat>"`, `category = "wiki"`, `metadata` carrying
//!   `{source_file, source: "dispatch-ledger", content_hash}`, an
//!   `authority: "advisory"` marker, optional typed declaration fields, and the already-extracted
//!   `counter_clauses_present`/`counter_clauses` pair this module (L2) reads
//!   from directly — see [`counter_clauses_from_metadata`]. Content changes
//!   bump `revision`;
//!   unchanged content is an idempotent no-op; a vanished source file flips
//!   the mirror row to `archived` (never deletes it). This module never
//!   touches the FS cards directly — "single source in FS, consumption in
//!   the mirror" (owner's decision ①) — and treats an `archived` mirror the
//!   same as no mirror at all (`list_by_path`'s `include_archived = false`
//!   already filters it out at read time).
//! - **Projection (L2, this module)**: at dispatch-prompt-assembly time,
//!   resolve the dispatch's seat candidates (the raw `profile` id and the
//!   normalized vendor family — see [`super::overlays::resolve_dispatch_vendor`]),
//!   match them against the seat suffixes of legacy/untyped and typed `seat`
//!   mirror rows only. Typed `model` and `harness` declarations remain
//!   queryable mirror evidence but never compete in the legacy seat matcher.
//!   Exact match is preferred over a prefix match; an ambiguous prefix
//!   match against multiple seats is treated as no match — injecting the
//!   wrong seat's countermeasures is worse than injecting none), and read
//!   the already-extracted 反制条款 (counter-clause) text straight out of the
//!   matched mirror row's `metadata.counter_clauses` field (written once, at
//!   L1 sync time, by the single heading-regex extractor that lives in
//!   `bootstrap/cli_tool/cards_ledger.rs`) — **this module never re-parses
//!   `entry.text` itself**; two independent extractors reading the same
//!   heading contract is exactly the drift the L1/L2 split was reworked to
//!   close (tachi#1202 review). The read is fail-closed:
//!   `metadata.counter_clauses_present` must be the literal `true` AND
//!   `metadata.counter_clauses` must be a string — a missing key, a `false`,
//!   or a `null`/non-string value under `true` (an inconsistent write this
//!   module has no way to repair) all fall through to "nothing to inject",
//!   never a crash and never stale/guessed text. Inline the result under a
//!   clearly marked header. No mirror row, no matching seat, or no
//!   qualifying metadata -> zero injection, zero noise (byte-identical
//!   prompt to today). `inject_card=false` on the dispatch params suppresses
//!   this overlay unconditionally.

use crate::MemoryServer;
use tachi_params::ResolvedStaffAssignment;

use crate::dispatch_profile::ResolvedDispatchProfile;

use super::budget::PromptInputBudget;
use super::overlays::resolve_dispatch_vendor;

/// Header the injected countermeasures block is always rendered under, so a
/// consumer/human can find (or strip) it deterministically.
const SEAT_CARD_HEADER: &str = "## Seat countermeasures (from lane card)";

/// 1536-character ceiling on the injected countermeasures text — a character
/// count (Unicode scalar values, per [`PromptInputBudget::admit`]), not a
/// byte count, consistent with the rest of prompt assembly's
/// character-counted budgets (see `budget.rs`). Longer sections are
/// truncated with an ellipsis marker.
const SEAT_CARD_BUDGET_CHARS: usize = 1536;

/// Path prefix every lane-card mirror row lives under.
const SEAT_CARD_PATH_PREFIX: &str = "/cards";

pub(crate) fn complete_counter_clause_projection(section: &str) -> bool {
    let mut budget = PromptInputBudget::new(SEAT_CARD_BUDGET_CHARS);
    budget.admit(section).as_deref() == Some(section)
}

/// Readiness describes the exact mirror row resolved for a future projection;
/// it does not claim that any prompt has already been injected.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SeatCardReadinessReceipt {
    pub seat: String,
    pub source_hash: String,
    pub mirror_revision: i64,
    pub counter_clause_hash: String,
    pub counter_clauses: String,
    pub source_file: String,
    pub complete_projection: bool,
}

/// Resolve one exact seat using the same metadata parser as prompt rendering.
pub(crate) fn resolve_exact_seat_card_readiness(
    server: &MemoryServer,
    seat: &str,
) -> Option<SeatCardReadinessReceipt> {
    let entries = server
        .with_global_store_read(|store| {
            store
                .list_by_path(&format!("{SEAT_CARD_PATH_PREFIX}/{seat}"), 10, false)
                .map_err(|e| e.to_string())
        })
        .ok()?;
    let exact: Vec<_> = entries
        .iter()
        .filter(|entry| entry.path == format!("{SEAT_CARD_PATH_PREFIX}/{seat}") && entry.is_wiki())
        .collect();
    if exact.len() != 1 {
        return None;
    }
    let entry = exact[0];
    if !participates_in_seat_projection(entry) {
        return None;
    }
    readiness_from_entry(seat, entry)
}

fn readiness_from_entry(
    seat: &str,
    entry: &memcore::MemoryEntry,
) -> Option<SeatCardReadinessReceipt> {
    let section = counter_clauses_from_metadata(&entry.metadata)?;
    let source_file = entry.metadata.get("source_file")?.as_str()?;
    if entry.metadata.get("source")?.as_str()? != "dispatch-ledger"
        || entry.metadata.get("authority")?.as_str()? != "advisory"
    {
        return None;
    }
    let complete_projection = complete_counter_clause_projection(section);
    Some(SeatCardReadinessReceipt {
        seat: seat.to_string(),
        source_hash: entry.metadata.get("content_hash")?.as_str()?.to_string(),
        mirror_revision: entry.revision,
        counter_clause_hash: tachi_params::hash_bytes(section.as_bytes()),
        counter_clauses: section.to_string(),
        source_file: source_file.to_string(),
        complete_projection,
    })
}

/// Read the already-extracted counter-clause text out of a `/cards/<seat>`
/// mirror row's `metadata`, fail-closed. The L1 writer
/// (`bootstrap/cli_tool/cards_ledger.rs`) is the single source of the
/// heading-regex extraction; this reads its output, never re-derives it.
///
/// Returns `Some` only when `counter_clauses_present` is literally `true`
/// AND `counter_clauses` is a string. Any other shape — the key missing
/// entirely, `counter_clauses_present: false`, or a `null`/non-string
/// `counter_clauses` under a `true` present flag (a write-side
/// inconsistency this reader cannot repair) — returns `None`: zero
/// injection, never a guess and never a panic.
fn counter_clauses_from_metadata(metadata: &serde_json::Value) -> Option<&str> {
    let present = metadata
        .get("counter_clauses_present")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    if !present {
        return None;
    }
    metadata
        .get("counter_clauses")
        .and_then(serde_json::Value::as_str)
}

fn participates_in_seat_projection(entry: &memcore::MemoryEntry) -> bool {
    card_kind_participates_in_seat_projection(
        entry
            .metadata
            .get("card_kind")
            .and_then(serde_json::Value::as_str),
    )
}

pub(crate) fn card_kind_participates_in_seat_projection(kind: Option<&str>) -> bool {
    match kind {
        None | Some("seat") => true,
        Some("model" | "harness" | "crew") => false,
        Some(_) => false,
    }
}

/// Project the seat-matched lane card's countermeasures section into the
/// dispatch prompt. Returns `None` (never an error) when the overlay is
/// disabled, no vendor/profile is derivable, no mirror row matches, or the
/// matched card's metadata carries no qualifying countermeasures text —
/// every one of these is a legitimate "nothing to inject" outcome, not a
/// fault (mirrors the swallow-and-degrade discipline
/// `render_vendor_vaccination_overlay` already uses for #735).
pub(super) fn render_seat_countermeasures_overlay(
    server: &MemoryServer,
    assignment: &ResolvedStaffAssignment,
    profile: &ResolvedDispatchProfile,
    inject_card: bool,
) -> Option<String> {
    if !inject_card {
        return None;
    }

    let readiness = resolve_seat_card_readiness(server, assignment, profile, inject_card)?;
    let mut budget = PromptInputBudget::new(SEAT_CARD_BUDGET_CHARS);
    let admitted = budget.admit(&readiness.counter_clauses)?;

    Some(format!(
        "{SEAT_CARD_HEADER}\n- seat: {} (source: {})\n{admitted}",
        readiness.seat, readiness.source_file
    ))
}

/// Resolve source/readiness from the same exact mirror row used by prompt
/// rendering. Suppression and fail-closed matching semantics are shared.
pub(super) fn resolve_seat_card_readiness(
    server: &MemoryServer,
    assignment: &ResolvedStaffAssignment,
    profile: &ResolvedDispatchProfile,
    inject_card: bool,
) -> Option<SeatCardReadinessReceipt> {
    if !inject_card {
        return None;
    }
    let profile_id = profile
        .selected_profile
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .or(assignment
            .selected_profile
            .as_deref()
            .filter(|s| !s.trim().is_empty()))
        .map(str::to_string);
    let vendor = resolve_dispatch_vendor(assignment, profile);
    if profile_id.is_none() && vendor.is_none() {
        return None;
    }

    let entries = match server.with_global_store_read(|store| {
        store
            .list_by_path(SEAT_CARD_PATH_PREFIX, 500, false)
            .map_err(|e| e.to_string())
    }) {
        Ok(entries) => entries,
        Err(err) => {
            eprintln!(
                "[dispatch] seat-card projection skipped (packet assembles without lane-card countermeasures): {err}"
            );
            return None;
        }
    };

    let seats: Vec<(String, &memcore::MemoryEntry)> = entries
        .iter()
        // "wiki-class" per `MemoryEntry::is_wiki()`'s canonical definition
        // (category == "wiki" OR domain == "wiki" OR metadata.wiki == true) —
        // whichever signal the L1 mirror-sync writer uses to mark it.
        .filter(|entry| entry.is_wiki())
        // Pre-frontmatter rows remain legacy seat cards. Typed model/harness
        // declarations are visible in the mirror but cannot create prefix
        // ambiguity in this seat-only overlay.
        .filter(|entry| participates_in_seat_projection(entry))
        .filter_map(|entry| {
            entry
                .path
                .strip_prefix("/cards/")
                .filter(|seat| !seat.is_empty())
                .map(|seat| (seat.to_string(), entry))
        })
        .collect();
    if seats.is_empty() {
        return None;
    }

    let seat_names: Vec<&str> = seats.iter().map(|(name, _)| name.as_str()).collect();
    let matched_seat = resolve_seat(&seat_names, profile_id.as_deref(), vendor.as_deref())?;
    let entry = seats
        .iter()
        .find(|(name, _)| name == matched_seat)
        .map(|(_, entry)| *entry)?;

    readiness_from_entry(matched_seat, entry)
}

/// Match `profile_id`/`vendor` against the available seat names, exact match
/// preferred over prefix match, `profile_id` checked before `vendor` at each
/// precision tier. A prefix match that is ambiguous (more than one seat
/// shares the same candidate prefix) is treated as no match under that
/// candidate — it falls through to the next candidate rather than guessing.
fn resolve_seat<'a>(
    seats: &[&'a str],
    profile_id: Option<&str>,
    vendor: Option<&str>,
) -> Option<&'a str> {
    let candidates: Vec<&str> = [profile_id, vendor]
        .into_iter()
        .flatten()
        .filter(|c| !c.trim().is_empty())
        .collect();
    if candidates.is_empty() {
        return None;
    }

    // Tier 1: exact match, profile id before vendor.
    for candidate in &candidates {
        if let Some(seat) = seats.iter().copied().find(|seat| seat == candidate) {
            return Some(seat);
        }
    }

    // Tier 2: unambiguous prefix match, profile id before vendor.
    for candidate in &candidates {
        let mut matches = seats
            .iter()
            .copied()
            .filter(|seat| seat.starts_with(*candidate));
        if let Some(first) = matches.next() {
            if matches.next().is_none() {
                return Some(first);
            }
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn metadata_present_true_with_string_injects_verbatim() {
        let metadata = json!({
            "counter_clauses_present": true,
            "counter_clauses": "## 反制条款\n- 只给窄单。",
        });
        assert_eq!(
            counter_clauses_from_metadata(&metadata),
            Some("## 反制条款\n- 只给窄单。")
        );
    }

    #[test]
    fn metadata_null_counter_clauses_yields_none() {
        // L1's explicit-Null-over-omission stale-clear write (tachi#1202
        // 499dbe10): present is false, and the value itself is Null too.
        let metadata = json!({
            "counter_clauses_present": false,
            "counter_clauses": serde_json::Value::Null,
        });
        assert_eq!(counter_clauses_from_metadata(&metadata), None);
    }

    #[test]
    fn metadata_missing_key_yields_none() {
        let metadata = json!({
            "source_file": "glm-5.2.md",
            "source": "dispatch-ledger",
        });
        assert_eq!(counter_clauses_from_metadata(&metadata), None);
    }

    #[test]
    fn metadata_present_true_but_null_value_yields_none() {
        // Defensive: an inconsistent write (present=true, value still Null)
        // must fail closed rather than inject a Null/garbage placeholder.
        let metadata = json!({
            "counter_clauses_present": true,
            "counter_clauses": serde_json::Value::Null,
        });
        assert_eq!(counter_clauses_from_metadata(&metadata), None);
    }

    #[test]
    fn metadata_present_true_but_non_string_value_yields_none() {
        let metadata = json!({
            "counter_clauses_present": true,
            "counter_clauses": 42,
        });
        assert_eq!(counter_clauses_from_metadata(&metadata), None);
    }

    #[test]
    fn typed_non_seat_kinds_are_not_apply_readiness_targets() {
        assert!(card_kind_participates_in_seat_projection(None));
        assert!(card_kind_participates_in_seat_projection(Some("seat")));
        for kind in ["model", "harness", "crew", "unknown"] {
            assert!(
                !card_kind_participates_in_seat_projection(Some(kind)),
                "typed non-seat kind {kind} must never be reported as seat projection-ready"
            );
        }
    }

    #[test]
    fn resolve_seat_prefers_exact_over_prefix() {
        let seats = vec!["codex", "codex-gpt56-sol"];
        assert_eq!(
            resolve_seat(&seats, Some("codex"), None),
            Some("codex"),
            "exact match on profile id must win over a prefix match"
        );
    }

    #[test]
    fn resolve_seat_falls_back_to_vendor_when_profile_is_ambiguous() {
        let seats = vec!["codex-gpt55", "codex-gpt56-sol"];
        // profile_id "codex" prefix-matches BOTH seats -> ambiguous, must not
        // guess; vendor "codex-gpt55" (exact) should still resolve.
        assert_eq!(
            resolve_seat(&seats, Some("codex"), Some("codex-gpt55")),
            Some("codex-gpt55")
        );
    }

    #[test]
    fn resolve_seat_none_when_nothing_matches() {
        let seats = vec!["glm-5.2", "grok-4.5"];
        assert_eq!(resolve_seat(&seats, Some("kimi_arch"), Some("kimi")), None);
    }
}
