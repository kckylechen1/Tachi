use chrono::Utc;
use memcore::{MemoryEntry, MemoryStore};
use serde_json::json;

use crate::memory_search_ops::{scrub_secrets, scrub_think_tags};

use super::claim::try_claim_sticky;
use super::memo::{
    sticky_from_entry, sticky_row_is_unread, sticky_ttl_expired, sticky_visible_to, StickyMemo,
};
use super::STICKY_PATH;

/// #1016: the single choke point where a delivered sticky's `from_agent`
/// gets rendered into every downstream surface (briefing markdown's
/// `row.get("from_agent")` and `sticky_check`'s raw JSON both consume this
/// value verbatim, with no further transform) — so marking a
/// caller-asserted identity here, once, covers both without either renderer
/// needing its own awareness of `identity_assurance`. Mirrors the existing
/// `scrub_sticky_text_for_read` choke-point pattern above for `text`.
fn sticky_display_from_agent(memo: &StickyMemo) -> String {
    if memo.identity_assurance == "caller_asserted" {
        format!("{} (自报)", memo.from_agent)
    } else {
        memo.from_agent.clone()
    }
}

/// Round-3 fix (codex final review of #964/PR #1003, BUG CP4): the single
/// row-load choke point every sticky-reading route flows through
/// (`claim_unread_stickies_for_briefing`'s `delivered.push` below, and
/// `list_or_claim_stickies`'s `include_read` branch) — scrubbing text HERE
/// means the markdown renderer (`agent_markdown::briefing::format_briefing`)
/// AND both JSON routes (briefing JSON compact/full in
/// `facade_memory_ops::briefing_ops`, and the `sticky_check` JSON response
/// in `handlers::handle_sticky_check`) all inherit the scrub for free,
/// instead of each caller needing its own belt-and-suspenders re-scrub.
/// `sticky_leave` already scrubs before persisting (see
/// `handlers::handle_sticky_leave`), so this is defense-in-depth for a row
/// that somehow bypassed write-time scrubbing (hand-inserted, migrated from
/// an older build, etc.) — mirrors the exact `scrub_think_tags` ->
/// `scrub_secrets` order used there.
fn scrub_sticky_text_for_read(text: &str) -> String {
    let without_think_tags = scrub_think_tags(text);
    let (safe_text, _redactions) = scrub_secrets(&without_think_tags);
    safe_text
}

/// All persisted sticky rows under `/sticky` (both broadcast and addressed
/// buckets), newest first. Callers filter by visibility/unread separately.
pub(super) fn all_sticky_entries(store: &mut MemoryStore) -> Result<Vec<MemoryEntry>, String> {
    let mut entries = store
        .list_by_path_recent(STICKY_PATH, super::STICKY_DB_LIMIT, true)
        .map_err(|e| format!("Failed to list sticky memories: {e}"))?;
    entries.retain(|entry| entry.category == "sticky");
    entries.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
    Ok(entries)
}

/// Unread stickies visible to `agent_id` (`None` = leader), newest first,
/// capped to `limit`. Each row returned here has ALREADY been atomically
/// claimed (read-once) as a side effect — calling this twice for the same
/// caller will only return a given sticky on the first call, by design
/// (briefing inclusion IS the read for the "surfaces once" delivery model).
pub(crate) fn claim_unread_stickies_for_briefing(
    server: &crate::MemoryServer,
    agent_id: Option<&str>,
    limit: usize,
) -> Result<Vec<serde_json::Value>, String> {
    let limit = limit.max(1).min(20);
    let claimed_by = agent_id.unwrap_or("leader").to_string();
    let now = Utc::now();

    server.with_global_store(|store| {
        let entries = all_sticky_entries(store)?;
        let mut delivered = Vec::new();
        for entry in entries {
            if delivered.len() >= limit {
                break;
            }
            if !sticky_row_is_unread(&entry) {
                continue;
            }
            let Some(memo) = sticky_from_entry(&entry) else {
                continue;
            };
            if !sticky_visible_to(&memo, agent_id) {
                continue;
            }
            // TTL (frozen semantics #6): past ttl_days -> archived as expired,
            // never delivered, visible only via include_read. Idempotent
            // write (no claim race needed — converges regardless of how many
            // concurrent readers observe the same expiry).
            if sticky_ttl_expired(&memo, now) {
                mark_expired(store, entry)?;
                continue;
            }
            // CP3 (opus xhigh review of #964/PR #1003): claim + mark are two
            // separate non-transactional writes on two different stores
            // (hard_state KV vs. the memories row). `MemoryStore` has no
            // public cross-write transaction primitive today — `db::upsert`
            // takes `&mut Connection` and opens its OWN transaction
            // internally, so it cannot be composed inside an outer
            // transaction on the same connection without a larger API
            // change — so a true single-transaction wrap across both stores
            // is impractical here without expanding memcore's surface. We
            // take the INVERTED-ORDER option the review offered instead,
            // reordered from a literal swap so it stays correct: the
            // `hard_state` CAS (`try_claim_sticky`) MUST still run first and
            // stay authoritative for "who won" — that's the only atomic
            // primitive in this pair, so it cannot move without breaking
            // CP1's single-winner guarantee. What moves is: this caller is
            // queued into `delivered` (this request's own return value)
            // immediately after winning the CAS, BEFORE `mark_claimed` runs,
            // and `mark_claimed`'s own failure is now non-fatal — logged,
            // not propagated with `?` — so it can never take down the
            // current caller's already-decided delivery or the rest of this
            // batch's already-collected `delivered` rows.
            //
            // Why this changes the failure mode from permanent silent loss
            // to at-most a stale display flag: previously,
            // `mark_claimed(..)?` propagated its error out of the WHOLE
            // closure — a crash there discarded every sticky already queued
            // in `delivered` for THIS request (not just the one that failed
            // to mark), on top of leaving hard_state permanently "claimed"
            // while the row still read "unread": every future caller would
            // pass the row's unread gate, lose the CAS (`Ok(false)`), and
            // silently skip forever — nobody delivered, nobody warned, no
            // way to ever retry. Now, a `mark_claimed` failure after a
            // successful CAS win only means the row's display/TTL-sweep
            // mirror lags (`status` may still read "unread" in
            // `include_read`/GC views even though the sticky was genuinely
            // delivered) — a cosmetic inconsistency, not a delivery loss —
            // and the CURRENT caller still receives the content, and the
            // REST of this batch's already-collected `delivered` rows are
            // unaffected by one entry's mark failure.
            let sticky_id = memo.id.clone();
            let won = try_claim_sticky(store, &sticky_id, Some(&claimed_by))?;
            if !won {
                continue;
            }
            delivered.push(json!({
                "id": sticky_id,
                "from_agent": sticky_display_from_agent(&memo),
                "to": memo.to,
                "text": scrub_sticky_text_for_read(&memo.text),
                "created_at": memo.created_at,
                "kind": "sticky",
            }));
            if let Err(e) = mark_claimed(store, entry, &claimed_by) {
                eprintln!(
                    "warning: sticky {sticky_id} claimed but failed to mirror status onto \
                     memory row (delivery already committed via hard_state CAS; row may show \
                     stale 'unread' until GC/manual repair): {e}"
                );
            }
        }
        Ok(delivered)
    })
}

/// Explicit check/list without a briefing wrapper (`action='check'`).
/// `include_read=false` (default) claims+delivers unread stickies exactly
/// like the briefing hook. `include_read=true` shows the archive (claimed +
/// expired) WITHOUT claiming anything — a pure read-only view.
pub(crate) fn list_or_claim_stickies(
    server: &crate::MemoryServer,
    agent_id: Option<&str>,
    include_read: bool,
    limit: usize,
) -> Result<Vec<serde_json::Value>, String> {
    if include_read {
        let limit = limit.max(1).min(50);
        return server.with_global_store_read(|store| {
            let entries = all_sticky_entries(store)?;
            let rows = entries
                .into_iter()
                .filter_map(|entry| {
                    let memo = sticky_from_entry(&entry)?;
                    if !sticky_visible_to(&memo, agent_id) {
                        return None;
                    }
                    let status = entry
                        .metadata
                        .get("status")
                        .and_then(|v| v.as_str())
                        .unwrap_or("unread")
                        .to_string();
                    let from_agent = sticky_display_from_agent(&memo);
                    Some(json!({
                        "id": memo.id,
                        "from_agent": from_agent,
                        "to": memo.to,
                        "text": scrub_sticky_text_for_read(&memo.text),
                        "created_at": memo.created_at,
                        "status": status,
                        "archived": entry.archived,
                        "kind": "sticky",
                    }))
                })
                .take(limit)
                .collect();
            Ok(rows)
        });
    }

    claim_unread_stickies_for_briefing(server, agent_id, limit)
}

/// Mirror a successful claim onto the memory row's metadata (for display /
/// TTL sweep / include_read archive). This write happens AFTER the atomic
/// `hard_state` claim already succeeded, so it never itself needs to be a
/// CAS — at most one caller ever reaches this function for a given sticky.
/// `pub(super)` (rather than private) only so the CP3 crash-safety unit test
/// in `tests.rs` can exercise its error branch directly — production code
/// only ever reaches it through `claim_unread_stickies_for_briefing` above.
pub(super) fn mark_claimed(
    store: &mut MemoryStore,
    mut entry: MemoryEntry,
    claimed_by: &str,
) -> Result<(), String> {
    let claimed_at = Utc::now().to_rfc3339();
    let metadata = entry
        .metadata
        .as_object_mut()
        .ok_or_else(|| "sticky metadata must be an object".to_string())?;
    metadata.insert("status".into(), json!("claimed"));
    metadata.insert("claimed_by".into(), json!(claimed_by));
    metadata.insert("claimed_at".into(), json!(claimed_at));
    entry.archived = true;
    entry.vector = None;
    store
        .upsert(&entry)
        .map_err(|e| format!("Failed to mark sticky claimed: {e}"))
}

/// Archive a TTL-expired sticky (frozen semantics #6). Idempotent — safe to
/// call redundantly if multiple readers observe the same expiry.
fn mark_expired(store: &mut MemoryStore, mut entry: MemoryEntry) -> Result<(), String> {
    let metadata = entry
        .metadata
        .as_object_mut()
        .ok_or_else(|| "sticky metadata must be an object".to_string())?;
    metadata.insert("status".into(), json!("expired"));
    entry.archived = true;
    entry.vector = None;
    store
        .upsert(&entry)
        .map_err(|e| format!("Failed to mark sticky expired: {e}"))
}
