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
//!   `{source_file, source: "dispatch-ledger", content_hash}` and an
//!   `authority: "advisory"` marker. Content changes bump `revision`;
//!   unchanged content is an idempotent no-op; a vanished source file flips
//!   the mirror row to `archived` (never deletes it). This module never
//!   touches the FS cards directly — "single source in FS, consumption in
//!   the mirror" (owner's decision ①) — and treats an `archived` mirror the
//!   same as no mirror at all (`list_by_path`'s `include_archived = false`
//!   already filters it out at read time).
//! - **Projection (L2, this module)**: at dispatch-prompt-assembly time,
//!   resolve the dispatch's seat candidates (the raw `profile` id and the
//!   normalized vendor family — see [`super::overlays::resolve_dispatch_vendor`]),
//!   match them against the seat suffixes of every `/cards/<seat>` mirror
//!   row (exact match preferred over a prefix match; an ambiguous prefix
//!   match against multiple seats is treated as no match — injecting the
//!   wrong seat's countermeasures is worse than injecting none), extract
//!   that card's 反制条款 (counter-clause) section(s), and inline them under
//!   a clearly marked header. No mirror row, no matching seat, or no
//!   matching section inside it -> zero injection, zero noise (byte-identical
//!   prompt to today). `inject_card=false` on the dispatch params suppresses
//!   this overlay unconditionally.

use regex::Regex;
use std::sync::OnceLock;

use crate::tool_params::TachiDispatchParams;
use crate::MemoryServer;

use super::budget::PromptInputBudget;
use super::overlays::resolve_dispatch_vendor;

/// Header the injected countermeasures block is always rendered under, so a
/// consumer/human can find (or strip) it deterministically.
const SEAT_CARD_HEADER: &str = "## Seat countermeasures (from lane card)";

/// 1.5 KB (1536 bytes treated as chars, consistent with the rest of prompt
/// assembly's character-counted budgets — see `budget.rs`) ceiling on the
/// injected countermeasures text. Longer sections are truncated with an
/// ellipsis marker by [`PromptInputBudget::admit`].
const SEAT_CARD_BUDGET_CHARS: usize = 1536;

/// Path prefix every lane-card mirror row lives under.
const SEAT_CARD_PATH_PREFIX: &str = "/cards";

/// Section-heading matcher for the "反制条款 section" the frozen contract
/// names: an ATX (`#`…) heading whose title contains 反制, 必带, or
/// case-insensitive "Counter". A card with no such heading has no
/// countermeasures to project (zero injection, not a fallback to some other
/// section).
fn counter_clause_heading_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"反制|必带|(?i)Counter").expect("static regex is valid"))
}

/// Project the seat-matched lane card's countermeasures section into the
/// dispatch prompt. Returns `None` (never an error) when the overlay is
/// disabled, no vendor/profile is derivable, no mirror row matches, or the
/// matched card has no countermeasures section — every one of these is a
/// legitimate "nothing to inject" outcome, not a fault (mirrors the
/// swallow-and-degrade discipline `render_vendor_vaccination_overlay` already
/// uses for #735).
pub(super) fn render_seat_countermeasures_overlay(
    server: &MemoryServer,
    params: &TachiDispatchParams,
) -> Option<String> {
    if params.inject_card == Some(false) {
        return None;
    }

    let profile_id = params
        .profile
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .map(str::to_string);
    let vendor = resolve_dispatch_vendor(params);
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

    let section = extract_counter_clause_sections(&entry.text)?;
    let mut budget = PromptInputBudget::new(SEAT_CARD_BUDGET_CHARS);
    let admitted = budget.admit(&section)?;

    let source_file = entry
        .metadata
        .get("source_file")
        .and_then(|v| v.as_str())
        .unwrap_or("dispatch-ledger");

    Some(format!(
        "{SEAT_CARD_HEADER}\n- seat: {matched_seat} (source: {source_file})\n{admitted}"
    ))
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
        let mut matches = seats.iter().copied().filter(|seat| seat.starts_with(*candidate));
        if let Some(first) = matches.next() {
            if matches.next().is_none() {
                return Some(first);
            }
        }
    }

    None
}

/// ATX heading level (number of leading `#`) if `line` (already left-trimmed)
/// is a valid markdown heading, else `None`. Requires a space (or end of
/// line) after the hashes so `#comment`-shaped lines never misparse as
/// headings.
fn heading_level(line: &str) -> Option<usize> {
    if !line.starts_with('#') {
        return None;
    }
    let hashes = line.chars().take_while(|&c| c == '#').count();
    let rest = &line[hashes..];
    (rest.is_empty() || rest.starts_with(' ')).then_some(hashes)
}

/// Extract every section (heading line inclusive, through the next heading
/// of equal-or-shallower depth, exclusive) whose title matches
/// [`counter_clause_heading_re`]. Concatenated in document order, `\n\n`
/// separated. `None` when the card has no matching heading at all — the
/// frozen contract's "no matching section -> zero injection, never borrow
/// from another section."
fn extract_counter_clause_sections(card_text: &str) -> Option<String> {
    let lines: Vec<&str> = card_text.lines().collect();
    let mut sections: Vec<String> = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let trimmed = lines[i].trim_start();
        let Some(level) = heading_level(trimmed) else {
            i += 1;
            continue;
        };
        let title = trimmed.trim_start_matches('#').trim();
        if !counter_clause_heading_re().is_match(title) {
            i += 1;
            continue;
        }

        let mut section_lines = vec![lines[i]];
        let mut j = i + 1;
        while j < lines.len() {
            let next_trimmed = lines[j].trim_start();
            if let Some(next_level) = heading_level(next_trimmed) {
                if next_level <= level {
                    break;
                }
            }
            section_lines.push(lines[j]);
            j += 1;
        }
        while section_lines
            .last()
            .map(|l| l.trim().is_empty())
            .unwrap_or(false)
        {
            section_lines.pop();
        }
        sections.push(section_lines.join("\n"));
        i = j;
    }

    (!sections.is_empty()).then(|| sections.join("\n\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const GLM_CARD: &str = "详见 Claude 记忆.\n\n## 反制条款(派单必带,2026-07-06)\n- 只给窄单。\n- 自报永不可信。\n\n## 流量定向\n- 量上去。\n";

    #[test]
    fn extracts_matching_section_only() {
        let section = extract_counter_clause_sections(GLM_CARD).expect("section found");
        assert!(section.contains("## 反制条款"));
        assert!(section.contains("只给窄单"));
        assert!(!section.contains("流量定向"));
    }

    #[test]
    fn no_matching_heading_yields_none() {
        let card = "## 定位\n一句话定位。\n\n## 病谱\n- 一条病。\n";
        assert!(extract_counter_clause_sections(card).is_none());
    }

    #[test]
    fn multiple_matching_sections_are_concatenated_in_order() {
        let card = "## 反制条款 A\n- one\n\n## unrelated\n- skip\n\n## 继承反制条款 B\n- two\n";
        let section = extract_counter_clause_sections(card).expect("sections found");
        assert!(section.contains("反制条款 A"));
        assert!(section.contains("- one"));
        assert!(section.contains("继承反制条款 B"));
        assert!(section.contains("- two"));
        assert!(!section.contains("unrelated"));
        // Order preserved: A before B.
        assert!(section.find("A").unwrap() < section.find("B").unwrap());
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

    #[test]
    fn heading_level_rejects_non_atx_hash_lines() {
        assert_eq!(heading_level("#nospace"), None);
        assert_eq!(heading_level("## 反制条款"), Some(2));
        assert_eq!(heading_level("#"), Some(1));
    }
}
