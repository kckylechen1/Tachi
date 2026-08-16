use chrono::{DateTime, Utc};
use rusqlite::types::Value;
use rusqlite::{params_from_iter, Connection, Transaction, TransactionBehavior};
use serde::Serialize;
use std::collections::{BTreeMap, HashMap, HashSet};

use crate::error::MemoryError;

use super::{now_utc_iso, IN_BATCH_SIZE};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AccessUpdate {
    pub access_count: i64,
    pub scored_count: i64,
    pub last_access: Option<String>,
}

/// Provenance of one `access_history` row — tachi#1446 lever 5.
///
/// The whole defect this discriminates is that one write path
/// (`record_access_with_updates`) records *the system showing a row* and
/// another read path (`get_access_times` → the ACT-R base-level-activation
/// floor in `scorer.rs`) treats those rows as evidence about the memory. The
/// two are only separable if the row says which it is.
///
/// [`AccessEventKind::Display`] is the default for stored rows and for every
/// row written before the column existed — see the `event_kind` `ensure_column`
/// in `db/schema.rs` for why that default is the honest one rather than a
/// convenience.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccessEventKind {
    /// The recall pipeline returned this memory in a result set.
    Display,
    /// A caller-initiated save cited this memory (tachi#1446 signal D).
    Use,
}

impl AccessEventKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Display => "display",
            Self::Use => "use",
        }
    }

    /// The SQL fragment appended to an `access_history` scan to restrict it to
    /// this kind. A `&'static str` chosen from a closed enum rather than a
    /// bound parameter: the value is never caller-supplied, and inlining it
    /// keeps the numbered-placeholder indices of the surrounding queries
    /// unchanged, so the two arms of `get_access_times_of_kind` differ in
    /// exactly one literal and cannot drift apart in placeholder arithmetic.
    ///
    /// There is deliberately no unfiltered *event kind*: callers that select a
    /// provenance must name one. The promotion query represents its frozen
    /// knob-OFF path as `None`, because that path must preserve the literal
    /// pre-#1446 unfiltered row set even after `use` rows begin to exist.
    ///
    /// Two readers now share this fragment: `get_access_times_of_kind` below
    /// (lever 5, ranking) and `db::daily_pipeline::count_distinct_access_days`
    /// (lever 6, the durable-promotion ratchet). Both are crate-internal;
    /// `pub(crate)` rather than `pub` keeps the closed-enum guarantee — no
    /// caller outside memcore can hand a raw predicate to either query.
    pub(crate) const fn sql_predicate(self) -> &'static str {
        match self {
            Self::Display => " AND event_kind = 'display'",
            Self::Use => " AND event_kind = 'use'",
        }
    }

    /// The arm the **durable-promotion ratchet** reads — tachi#1446 lever 6.
    ///
    /// `count_distinct_access_days` feeds `calculate_promotion_score`, and a
    /// score at or above 0.60 calls `promote_memory_to_durable`, which pins
    /// `importance = 0.7` and `retention_policy = 'durable'` permanently.
    /// Counting `display` rows there means *the system showing a memory
    /// repeatedly promotes it, irreversibly* — the same defect as levers 1-5,
    /// on the one substrate where the consequence cannot be undone by
    /// flipping the knob back.
    ///
    /// **This deliberately shares `use_provenance_recency` rather than adding
    /// a lever-6 knob.** Three reasons, in order of weight:
    ///
    /// 1. A separate knob creates a four-state config matrix of which two
    ///    states are incoherent — ranking that refuses to believe display
    ///    events while promotion still ratchets on them, or the reverse. There
    ///    is no deployment that wants either, so the second knob would exist
    ///    only to be set equal to the first.
    /// 2. The irreversibility objection points the *other* way once the sign
    ///    is checked. Turning the knob ON makes this gate strictly harder to
    ///    pass in the common case (a memory's `use` days are near zero while
    ///    its `display` days accumulate), so the ON state performs *fewer*
    ///    irreversible promotions than OFF. A withheld row remains eligible
    ///    for reconsideration whenever the bounded candidate scan selects it;
    ///    promotion itself is not reversible.
    /// 3. It is one defect with one substrate (`access_history.event_kind`).
    ///    Two knobs over one column is how the two halves drift apart.
    ///
    /// The ON row set is a strict subset of the OFF row set, so enabling the
    /// knob can only preserve or lower the count. A caller-cited memory can
    /// still earn durable retention from its `use` days; display-only days no
    /// longer help it cross the gate.
    pub fn for_promotion(recall_config: &crate::RecallConfig) -> Option<Self> {
        if recall_config.use_provenance_recency {
            Some(Self::Use)
        } else {
            None
        }
    }

    /// Every variant, for instrumentation that reports one number per kind.
    pub const ALL: [Self; 2] = [Self::Display, Self::Use];
}

/// Answer to "is signal D dense enough, or is an explicit by-id fetch (signal
/// C) required?" — tachi#1446.
///
/// D marks a memory used only when a save explicitly cites it. Whether that is
/// a usable ranking signal or a column that stays NULL forever is an empirical
/// question about a deployment, not something that can be argued from the
/// code, so this is the instrument that answers it. See
/// [`access_event_density`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AccessEventDensity {
    /// Inclusive lower bound (ISO-8601 UTC) the counts were taken over.
    pub since: String,
    /// `access_history` rows per `event_kind` at or after `since`.
    pub events_by_kind: BTreeMap<String, i64>,
    /// Distinct `memory_id`s per `event_kind` at or after `since`.
    pub memories_by_kind: BTreeMap<String, i64>,
    /// Memories carrying a non-NULL `last_use_at` (all time, not windowed —
    /// the column is last-write-wins, so a window would be meaningless).
    pub memories_with_last_use_at: i64,
    /// Total rows in `memories`, as the denominator for the line above.
    pub memories_total: i64,
}

/// Legacy FNV-1a bucket used only by access history and query-diversity
/// telemetry. Sampled recall impressions use a separate SHA-256 fingerprint;
/// this value is neither their identity nor their cohort key.
pub(crate) fn query_hash(query: &str) -> String {
    let mut hash: u32 = 2_166_136_261;
    for byte in query.bytes() {
        hash ^= byte as u32;
        hash = hash.wrapping_mul(16_777_619);
    }
    format!("{hash:08x}")
}

fn numbered_placeholders(start: usize, count: usize) -> String {
    (start..start + count)
        .map(|idx| format!("?{idx}"))
        .collect::<Vec<_>>()
        .join(",")
}

fn values_clause(start: usize, row_count: usize, width: usize) -> String {
    (0..row_count)
        .map(|row| {
            let first = start + row * width;
            let placeholders = numbered_placeholders(first, width);
            format!("({placeholders})")
        })
        .collect::<Vec<_>>()
        .join(",")
}

fn unique_id_order(ids: &[String]) -> Vec<&str> {
    let mut seen = HashSet::with_capacity(ids.len());
    ids.iter()
        .map(String::as_str)
        .filter(|id| seen.insert(*id))
        .collect()
}

/// Test-only thin wrapper over [`record_access_with_updates`] that drops the
/// returned map. Read that function's doc for what the counters mean and, in
/// particular, for what they do not observe.
///
/// tachi#1585 D5: defaults to the pure `RecallConfig::default()`, not the
/// process-wide `RecallConfig::get()` — this wrapper has no `MemoryStore` to
/// draw a host-injected policy from (it takes a bare `Connection`), and the
/// crate's only production caller of `record_access_with_updates`
/// (`search.rs`'s `hybrid_search_inner`) already passes an explicit config,
/// so this default is test-only regardless.
#[cfg(test)]
pub(crate) fn record_access(
    conn: &Connection,
    ids: &[String],
    fts_hits: &[String],
    query: Option<&str>,
) -> Result<(), MemoryError> {
    record_access_with_updates(
        conn,
        ids,
        ids,
        fts_hits,
        query,
        &crate::RecallConfig::default(),
        None,
    )
    .map(|_| ())
}

/// Bump `access_count` and `last_access` for a list of IDs after a non-empty
/// search when `SearchOptions::record_access` is enabled.
/// `fts_hits` are the IDs matched by the FTS channel (get `recall_count` incremented).
/// `query` is the raw query string; its FNV-1a hash is stored in `access_history` and
/// used to compute `query_diversity` (distinct queries that reached this memory).
/// Applies a promotion gate: tier -> "consolidated" when `recall_count >= 3`
/// and `query_diversity >= 3`.
///
/// **This is the only production path that increments the displayed-result
/// counters and scorer-only `scored_count`, and it has exactly one production
/// caller: `search.rs`'s `hybrid_search`**
/// (tachi#1459). `gc_tables` can later reconcile `query_diversity` downward
/// from the search-written `access_history`; it does not add non-search use.
/// So every counter it maintains observes the search path only; reads through
/// path-listing routes do not increment them — `list_by_path`,
/// `list_by_path_recent` and `list_memories_by_path_prefix` return rows without
/// coming through here, so kanban, handoffs, briefing projections, the cards
/// mirror and GC candidate scans can read a memory constantly without changing
/// any counter below. `access_count = 0` means there is currently no retained
/// access-count evidence; it does not prove that no search or other read ever
/// returned the row. Every downstream predicate that treats zero as evidence
/// of low value inherits that gap.
///
/// Two further narrowings inside the search path itself: `hybrid_search` skips
/// this call entirely when `SearchOptions::record_access` is false (the
/// internal similarity, auto-link, contradiction, auto-ingest and capture
/// searches all set it so), and `recall_count` moves only for ids present in
/// `fts_hits`.
pub(crate) fn record_access_with_updates(
    conn: &Connection,
    displayed_ids: &[String],
    scored_ids: &[String],
    fts_hits: &[String],
    query: Option<&str>,
    recall_config: &crate::RecallConfig,
    impression: Option<&crate::recall_impressions::RecallImpressionPayload>,
) -> Result<HashMap<String, AccessUpdate>, MemoryError> {
    if displayed_ids.is_empty() && scored_ids.is_empty() {
        return Ok(HashMap::new());
    }

    let now = now_utc_iso();
    let query_hash = query.map(query_hash).unwrap_or_default();
    // `record_access` is called from search paths that only hold `&Connection`.
    // The unchecked transaction keeps the access_count/history/recall updates
    // atomic without widening the public search API to require `&mut Connection`.
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;

    let mut candidate_ids = unique_id_order(displayed_ids);
    let displayed_set: HashSet<&str> = candidate_ids.iter().copied().collect();
    candidate_ids.extend(
        unique_id_order(scored_ids)
            .into_iter()
            .filter(|id| !displayed_set.contains(id)),
    );
    let mut existing_set = HashSet::with_capacity(candidate_ids.len());
    for batch in candidate_ids.chunks(IN_BATCH_SIZE) {
        let placeholders = numbered_placeholders(1, batch.len());
        let sql = format!("SELECT id FROM memories WHERE id IN ({placeholders})");
        let values = batch
            .iter()
            .map(|id| Value::Text((*id).to_string()))
            .collect::<Vec<_>>();
        let mut stmt = tx.prepare(&sql)?;
        let rows = stmt.query_map(params_from_iter(values.iter()), |row| {
            row.get::<_, String>(0)
        })?;
        for row in rows {
            existing_set.insert(row?);
        }
    }
    let existing_ids = candidate_ids
        .into_iter()
        .filter(|id| existing_set.contains(*id))
        .collect::<Vec<_>>();
    if existing_ids.is_empty() {
        tx.commit()?;
        return Ok(HashMap::new());
    }
    for id in &existing_ids {
        super::refuse_retired_sticky_row_within_tx(&tx, id, "recorded as recalled")?;
    }

    let displayed_set: HashSet<&str> = displayed_ids.iter().map(String::as_str).collect();
    let scored_set: HashSet<&str> = scored_ids.iter().map(String::as_str).collect();
    let displayed_existing_ids = existing_ids
        .iter()
        .copied()
        .filter(|id| displayed_set.contains(*id))
        .collect::<Vec<_>>();
    let displayed_scored_ids = existing_ids
        .iter()
        .copied()
        .filter(|id| displayed_set.contains(*id) && scored_set.contains(*id))
        .collect::<Vec<_>>();
    let displayed_only_ids = existing_ids
        .iter()
        .copied()
        .filter(|id| displayed_set.contains(*id) && !scored_set.contains(*id))
        .collect::<Vec<_>>();
    let scored_only_ids = existing_ids
        .iter()
        .copied()
        .filter(|id| scored_set.contains(*id) && !displayed_set.contains(*id))
        .collect::<Vec<_>>();

    // The intersection has one persistence update, so a displayed scorer hit
    // increments each diagnostic exactly once without an avoidable second
    // all-memories generation-trigger write.
    for batch in displayed_scored_ids.chunks(IN_BATCH_SIZE) {
        let placeholders = numbered_placeholders(2, batch.len());
        let sql = format!(
            "UPDATE memories
             SET scored_count = scored_count + 1,
                 access_count = access_count + 1,
                 last_access = ?1
             WHERE id IN ({placeholders})"
        );
        let mut values = Vec::with_capacity(batch.len() + 1);
        values.push(Value::Text(now.clone()));
        values.extend(batch.iter().map(|id| Value::Text((*id).to_string())));
        tx.execute(&sql, params_from_iter(values.iter()))?;
    }

    // Graph-only appended rows are displayed but never scored; preserve their
    // established access/history behavior without assigning scorer evidence.
    for batch in displayed_only_ids.chunks(IN_BATCH_SIZE) {
        let placeholders = numbered_placeholders(2, batch.len());
        let sql = format!(
            "UPDATE memories
             SET access_count = access_count + 1, last_access = ?1
             WHERE id IN ({placeholders})"
        );
        let mut values = Vec::with_capacity(batch.len() + 1);
        values.push(Value::Text(now.clone()));
        values.extend(batch.iter().map(|id| Value::Text((*id).to_string())));
        tx.execute(&sql, params_from_iter(values.iter()))?;
    }

    // Scoring-count invariant: MMR/top-k losers get one persisted scorer count
    // and no access-history row. Because scored_count lives on `memories`, this
    // UPDATE intentionally advances the DB-authoritative search generation via
    // `memory_search_generation_after_update`: record_access already invalidates
    // display searches, and generation consumers compare fingerprints/equality,
    // never numeric deltas. This adds one bounded generation write per newly
    // scored-only persisted row without changing any ranking or policy result.
    for batch in scored_only_ids.chunks(IN_BATCH_SIZE) {
        let placeholders = numbered_placeholders(1, batch.len());
        let sql = format!(
            "UPDATE memories SET scored_count = scored_count + 1 WHERE id IN ({placeholders})"
        );
        let values = batch
            .iter()
            .map(|id| Value::Text((*id).to_string()))
            .collect::<Vec<_>>();
        tx.execute(&sql, params_from_iter(values.iter()))?;
    }

    // tachi#1446 lever 5: this INSERT deliberately does NOT name `event_kind`.
    // The column's `NOT NULL DEFAULT 'display'` supplies it, which keeps this
    // statement — the hot path's third write per search — byte-for-byte what it
    // was, including its `IN_BATCH_SIZE / 3` chunking (sized for three bound
    // parameters per row). Naming the column would mean four parameters per
    // row and a re-derived chunk size for a value the schema already pins.
    // `display` is the only correct value here by construction: the sole
    // production caller of this function is `search.rs`'s `hybrid_search`,
    // recording the rows it just returned.
    for batch in displayed_existing_ids.chunks(IN_BATCH_SIZE / 3) {
        let sql = format!(
            "INSERT INTO access_history (memory_id, accessed_at, query_hash) VALUES {}",
            values_clause(1, batch.len(), 3)
        );
        let mut values = Vec::with_capacity(batch.len() * 3);
        for id in batch {
            values.push(Value::Text((*id).to_string()));
            values.push(Value::Text(now.clone()));
            values.push(Value::Text(query_hash.clone()));
        }
        tx.execute(&sql, params_from_iter(values.iter()))?;
    }

    let fts_set = fts_hits.iter().map(String::as_str).collect::<HashSet<_>>();
    let recall_ids = displayed_existing_ids
        .iter()
        .copied()
        .filter(|id| fts_set.contains(*id))
        .collect::<Vec<_>>();
    for batch in recall_ids.chunks(IN_BATCH_SIZE) {
        let placeholders = numbered_placeholders(1, batch.len());
        let sql = format!(
            "UPDATE memories SET recall_count = recall_count + 1 WHERE id IN ({placeholders})"
        );
        let values = batch
            .iter()
            .map(|id| Value::Text((*id).to_string()))
            .collect::<Vec<_>>();
        tx.execute(&sql, params_from_iter(values.iter()))?;
    }

    if !query_hash.is_empty() {
        let mut first_hash_ids = Vec::new();
        for batch in displayed_existing_ids.chunks(IN_BATCH_SIZE - 1) {
            let placeholders = numbered_placeholders(1, batch.len());
            let hash_idx = batch.len() + 1;
            let sql = format!(
                "SELECT memory_id FROM access_history
                 WHERE memory_id IN ({placeholders}) AND query_hash = ?{hash_idx}
                 GROUP BY memory_id
                 HAVING COUNT(*) = 1"
            );
            let mut values = batch
                .iter()
                .map(|id| Value::Text((*id).to_string()))
                .collect::<Vec<_>>();
            values.push(Value::Text(query_hash.clone()));
            let mut stmt = tx.prepare(&sql)?;
            let rows = stmt.query_map(params_from_iter(values.iter()), |row| {
                row.get::<_, String>(0)
            })?;
            for row in rows {
                first_hash_ids.push(row?);
            }
        }
        for batch in first_hash_ids.chunks(IN_BATCH_SIZE) {
            let placeholders = numbered_placeholders(1, batch.len());
            let sql = format!(
                "UPDATE memories SET query_diversity = query_diversity + 1 WHERE id IN ({placeholders})"
            );
            let values = batch
                .iter()
                .map(|id| Value::Text(id.clone()))
                .collect::<Vec<_>>();
            tx.execute(&sql, params_from_iter(values.iter()))?;
        }
    }

    // The tier-promotion gate. Both counters it reads observe the search path
    // only; reads through path-listing routes do not increment them
    // (tachi#1459), so this gate cannot promote on the strength of a memory
    // that is heavily used but only ever reached by path listing — it can only
    // ever under-promote, never over-promote, on that account. The promotion is
    // also one-way here: nothing in this function demotes a row whose counters
    // later fall back below the thresholds (`gc_tables`' reconciliation can
    // lower `query_diversity` after the fact).
    if !recall_config.use_provenance_recency {
        for batch in displayed_existing_ids.chunks(IN_BATCH_SIZE) {
            let placeholders = numbered_placeholders(1, batch.len());
            let sql = format!(
                "UPDATE memories SET tier = 'consolidated'
                 WHERE id IN ({placeholders})
                   AND tier = 'raw'
                   AND recall_count >= 3
                   AND query_diversity >= 3"
            );
            let values = batch
                .iter()
                .map(|id| Value::Text((*id).to_string()))
                .collect::<Vec<_>>();
            tx.execute(&sql, params_from_iter(values.iter()))?;
        }
    }

    if let Some(payload) = impression {
        crate::recall_impressions::insert_recall_impression(&tx, payload)?;
    }

    let mut updates = HashMap::with_capacity(displayed_existing_ids.len());
    for batch in displayed_existing_ids.chunks(IN_BATCH_SIZE) {
        let placeholders = numbered_placeholders(1, batch.len());
        let sql = format!(
            "SELECT id, access_count, scored_count, last_access FROM memories WHERE id IN ({placeholders})"
        );
        let values = batch
            .iter()
            .map(|id| Value::Text((*id).to_string()))
            .collect::<Vec<_>>();
        let mut stmt = tx.prepare(&sql)?;
        let rows = stmt.query_map(params_from_iter(values.iter()), |row| {
            Ok((
                row.get::<_, String>(0)?,
                AccessUpdate {
                    access_count: row.get(1)?,
                    scored_count: row.get(2)?,
                    last_access: row.get(3)?,
                },
            ))
        })?;
        for row in rows {
            let (id, update) = row?;
            updates.insert(id, update);
        }
    }

    tx.commit()?;
    Ok(updates)
}

/// Record that `ids` were **used** — tachi#1446 signal D.
///
/// "Used" here is deliberately narrow, and this function does not decide it:
/// the caller decides, and the only production caller that does is
/// `save_memory::persist::mark_save_target_used` in `tachi-server`, reached
/// only when an MCP-tool save named an existing memory's id (see that
/// function's doc for the channel inventory and why nothing else qualifies).
/// It is the complement of `record_access_with_updates`, which records the
/// system *showing* a row.
///
/// What this writes:
/// * `memories.last_use_at = at` — the recency anchor `scorer.rs` reads when
///   `RecallConfig::use_provenance_recency` is on;
/// * one `access_history` row per id with `event_kind = 'use'` — the ACT-R
///   base-level-activation evidence [`get_use_access_times`] reads, and the
///   per-candidate use count levers 2 and 4 derive from (`search/ranking.rs`
///   counts the rows that same read already returned, so the counts cost no
///   extra query).
///
/// **Unconditional on `RecallConfig::use_provenance_recency`.** The knob
/// governs *reads*, not this write: a deployment has to be able to accumulate
/// use provenance and measure its density ([`access_event_density`]) before
/// deciding to switch ranking onto it, and a knob-gated writer would make the
/// first day after the flip look identical to the defect it repairs. Nothing
/// written here can reach the default scorer — [`get_access_times`] filters to
/// `display`, so with the knob off these rows are invisible to ranking.
///
/// What it deliberately does NOT write: `access_count`, `last_access`,
/// `recall_count`, `query_diversity`, or the tier promotion gate. Those are
/// the display-side counters; a use event must not be able to reach them or
/// the two provenances re-merge and the whole discriminator is decorative.
///
/// The existence scan, retired-row preflight, timestamp update, and history
/// append share one `BEGIN IMMEDIATE` transaction. Ids with no row in
/// `memories` are skipped (same existence-filtered contract as
/// `record_access_with_updates`); the return value is the number of ids that
/// actually existed and were marked.
pub fn record_memory_use(
    conn: &Connection,
    ids: &[String],
    at: &str,
) -> Result<usize, MemoryError> {
    if ids.is_empty() {
        return Ok(0);
    }

    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
    let unique_ids = unique_id_order(ids);
    let mut existing_set = HashSet::with_capacity(unique_ids.len());
    for batch in unique_ids.chunks(IN_BATCH_SIZE) {
        let placeholders = numbered_placeholders(1, batch.len());
        let sql = format!("SELECT id FROM memories WHERE id IN ({placeholders})");
        let values = batch
            .iter()
            .map(|id| Value::Text((*id).to_string()))
            .collect::<Vec<_>>();
        let mut stmt = tx.prepare(&sql)?;
        let rows = stmt.query_map(params_from_iter(values.iter()), |row| {
            row.get::<_, String>(0)
        })?;
        for row in rows {
            existing_set.insert(row?);
        }
    }
    let existing_ids = unique_ids
        .into_iter()
        .filter(|id| existing_set.contains(*id))
        .collect::<Vec<_>>();
    if existing_ids.is_empty() {
        tx.commit()?;
        return Ok(0);
    }
    for id in &existing_ids {
        super::refuse_retired_sticky_row_within_tx(&tx, id, "recorded as used")?;
    }

    for batch in existing_ids.chunks(IN_BATCH_SIZE) {
        let placeholders = numbered_placeholders(2, batch.len());
        let sql = format!("UPDATE memories SET last_use_at = ?1 WHERE id IN ({placeholders})");
        let mut values = Vec::with_capacity(batch.len() + 1);
        values.push(Value::Text(at.to_string()));
        values.extend(batch.iter().map(|id| Value::Text((*id).to_string())));
        tx.execute(&sql, params_from_iter(values.iter()))?;
    }

    // Four bound parameters per row, so the chunk divisor is 4 (the display
    // writer above uses 3 because it lets `event_kind` default).
    for batch in existing_ids.chunks(IN_BATCH_SIZE / 4) {
        let sql = format!(
            "INSERT INTO access_history (memory_id, accessed_at, query_hash, event_kind) VALUES {}",
            values_clause(1, batch.len(), 4)
        );
        let mut values = Vec::with_capacity(batch.len() * 4);
        for id in batch {
            values.push(Value::Text((*id).to_string()));
            values.push(Value::Text(at.to_string()));
            // Empty query hash on purpose: a use event has no query behind it,
            // and `query_diversity` counts distinct NON-empty hashes
            // (`stats_gc.rs`'s reconciliation excludes `query_hash = ''`), so
            // this cannot inflate the promotion gate.
            values.push(Value::Text(String::new()));
            values.push(Value::Text(AccessEventKind::Use.as_str().to_string()));
        }
        tx.execute(&sql, params_from_iter(values.iter()))?;
    }

    let marked = existing_ids.len();
    tx.commit()?;
    Ok(marked)
}

/// Per-`memory_id` cap on rows `get_access_times` will read from
/// `access_history`. ACT-R base-level activation (`base_level_activation` in
/// `scorer.rs`) sums `t_j^(-d)` over every returned access age — a plain sum,
/// so it is order-independent and dominated by the most recent (least-decayed)
/// accesses; older accesses beyond this cap contribute a vanishingly small
/// share of the sum. This mirrors `GcConfig::access_history_keep_per_memory`
/// (default 256): in steady state GC already prunes each memory_id down to
/// this many rows, so the cap changes nothing for GC'd data and only bounds
/// the query's worst case for a memory_id whose history has grown past that
/// (e.g. between GC runs) — access_history is the fastest-growing table
/// (15.6k rows observed on a single live DB) and this query was previously
/// unbounded per id.
///
/// tachi#1446: the cap is applied *after* the `event_kind` filter, so each
/// kind gets its own window of this size. That is deliberately the same shape
/// as the GC quota, which now partitions by `(memory_id, event_kind)` — a cap
/// applied before the filter would let 256 display rows starve the read of the
/// rare use rows exactly as an un-partitioned GC would starve their storage.
const ACCESS_TIMES_MAX_PER_MEMORY: i64 = 256;

/// Fetch **display** access timestamps for a set of memory IDs (for ACT-R
/// base-level activation).
/// Returns a map from memory_id -> sorted list of seconds-since-epoch (age in seconds).
/// Handles batching internally to stay under SQLite's 999 parameter limit.
/// Caps each memory_id to its [`ACCESS_TIMES_MAX_PER_MEMORY`] most recent
/// accesses (see that constant's doc comment for why this preserves ACT-R
/// semantics).
///
/// tachi#1446 lever 5: this is the `RecallConfig::use_provenance_recency` =
/// **off** arm, and it is byte-identical in result to the pre-#1446 unfiltered
/// query — every row that existed before the `event_kind` column carries
/// `display` by migration default, and the only writer of any other value is
/// [`record_memory_use`], which did not exist. The filter is what keeps that
/// true *going forward*: without it, a use event would silently raise the ACT-R
/// floor at default config, i.e. the new signal would leak into the exact
/// channel #1446 is repairing. The on arm is [`get_use_access_times`].
///
/// tachi#1459: the `display` rows this reads observe recorded search-path
/// accesses only; reads through path-listing routes do not write them. An empty
/// vector here means no retained `display` event exists, not that nothing ever
/// read or returned the memory. The ACT-R floor this feeds is therefore silent
/// about any memory whose only consumer is `list_by_path` / `list_by_path_recent` /
/// `list_memories_by_path_prefix`.
pub fn get_access_times(
    conn: &Connection,
    ids: &[String],
) -> Result<HashMap<String, Vec<f64>>, MemoryError> {
    get_access_times_of_kind(conn, ids, AccessEventKind::Display)
}

/// Fetch **use** access timestamps for a set of memory IDs — tachi#1446
/// lever 5, the `use_provenance_recency` = on arm of [`get_access_times`].
///
/// Same shape, same cap, same batching; only `event_kind` differs. What the
/// ACT-R base-level-activation floor (`scorer::default_decay_score_actr_with_config`)
/// sums over is then evidence that callers *used* these memories, never
/// evidence that the recall pipeline displayed them.
///
/// `search/ranking.rs` also takes the per-candidate **use count** from the
/// length of these vectors (levers 2 and 4), so a candidate's use count is
/// capped by [`ACCESS_TIMES_MAX_PER_MEMORY`] exactly like its ages are.
///
/// tachi#1459: this read has the complementary blind spot to
/// [`get_access_times`], not the same one. Its rows come from
/// [`record_memory_use`], whose only production caller is a save that cited an
/// existing memory's id — so this observes the *save* path only, and a memory
/// read through search or through a path-listing route without ever being cited
/// stays absent here. Neither read observes path listing.
pub fn get_use_access_times(
    conn: &Connection,
    ids: &[String],
) -> Result<HashMap<String, Vec<f64>>, MemoryError> {
    get_access_times_of_kind(conn, ids, AccessEventKind::Use)
}

fn get_access_times_of_kind(
    conn: &Connection,
    ids: &[String],
    kind: AccessEventKind,
) -> Result<HashMap<String, Vec<f64>>, MemoryError> {
    if ids.is_empty() {
        return Ok(HashMap::new());
    }

    let now = Utc::now();
    let mut result: HashMap<String, Vec<f64>> = HashMap::new();
    let kind_predicate = kind.sql_predicate();

    for batch in ids.chunks(IN_BATCH_SIZE) {
        let placeholders: Vec<String> = batch
            .iter()
            .enumerate()
            .map(|(i, _)| format!("?{}", i + 1))
            .collect();
        let hash_idx = batch.len() + 1;
        let sql = format!(
            "SELECT memory_id, accessed_at FROM (
                 SELECT memory_id, accessed_at,
                        ROW_NUMBER() OVER (
                            PARTITION BY memory_id
                            ORDER BY accessed_at DESC
                        ) AS rn
                 FROM access_history
                 WHERE memory_id IN ({}){kind_predicate}
             ) ranked
             WHERE rn <= ?{hash_idx}
             ORDER BY accessed_at DESC",
            placeholders.join(", ")
        );
        let mut stmt = conn.prepare(&sql)?;
        let mut params_vec: Vec<&dyn rusqlite::ToSql> =
            batch.iter().map(|s| s as &dyn rusqlite::ToSql).collect();
        params_vec.push(&ACCESS_TIMES_MAX_PER_MEMORY);
        let rows = stmt.query_map(params_vec.as_slice(), |row| {
            let mem_id: String = row.get(0)?;
            let at: String = row.get(1)?;
            Ok((mem_id, at))
        })?;

        for row in rows {
            let (mem_id, at_str) = row?;
            if let Ok(dt) = at_str.parse::<DateTime<Utc>>() {
                let age_secs = (now - dt).num_seconds().max(1) as f64;
                result.entry(mem_id).or_default().push(age_secs);
            }
        }
    }

    Ok(result)
}

/// Measure how dense the tachi#1446 signal-D use provenance actually is —
/// the instrument [`AccessEventDensity`] documents.
///
/// `since` is an inclusive ISO-8601 UTC lower bound compared as a string,
/// which is sound here and only here because every `accessed_at` this table
/// holds is written by `record_access_with_updates` or
/// [`record_memory_use`] from `now_utc_iso()` / an RFC-3339 UTC instant — one
/// fixed-width, zero-padded, UTC-normalised format, so lexicographic order is
/// chronological order. Do not copy this comparison to a column that mixes
/// offsets.
///
/// Every kind in [`AccessEventKind::ALL`] is present in both maps, zero
/// included: "no use events at all" is the single most important answer this
/// instrument can give, and a missing key would report it as a gap in the
/// instrument rather than as the measurement it is.
pub fn access_event_density(
    conn: &Connection,
    since: &str,
) -> Result<AccessEventDensity, MemoryError> {
    let mut events_by_kind: BTreeMap<String, i64> = AccessEventKind::ALL
        .iter()
        .map(|kind| (kind.as_str().to_string(), 0))
        .collect();
    let mut memories_by_kind = events_by_kind.clone();

    let mut stmt = conn.prepare(
        "SELECT event_kind, COUNT(*), COUNT(DISTINCT memory_id)
           FROM access_history
          WHERE accessed_at >= ?1
          GROUP BY event_kind",
    )?;
    let rows = stmt.query_map([since], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, i64>(2)?,
        ))
    })?;
    for row in rows {
        let (kind, events, memories) = row?;
        events_by_kind.insert(kind.clone(), events);
        memories_by_kind.insert(kind, memories);
    }

    let memories_with_last_use_at: i64 = conn.query_row(
        "SELECT COUNT(*) FROM memories WHERE last_use_at IS NOT NULL",
        [],
        |row| row.get(0),
    )?;
    let memories_total: i64 =
        conn.query_row("SELECT COUNT(*) FROM memories", [], |row| row.get(0))?;

    Ok(AccessEventDensity {
        since: since.to_string(),
        events_by_kind,
        memories_by_kind,
        memories_with_last_use_at,
        memories_total,
    })
}

#[cfg(test)]
mod get_access_times_tests {
    use super::*;
    use crate::{MemoryEntry, MemoryStore};
    use chrono::Duration as ChronoDuration;

    fn seed_memory(store: &mut MemoryStore, id: &str) {
        let entry = MemoryEntry {
            id: id.to_string(),
            path: "/facts/readonly".to_string(),
            summary: String::new(),
            text: String::new(),
            importance: 0.5,
            timestamp: "2026-01-01T00:00:00Z".to_string(),
            valid_from: "2026-01-01T00:00:00Z".to_string(),
            valid_until: None,
            category: "fact".to_string(),
            topic: String::new(),
            keywords: Vec::new(),
            persons: Vec::new(),
            entities: Vec::new(),
            location: String::new(),
            source: "manual".to_string(),
            scope: "general".to_string(),
            archived: false,
            access_count: 0,
            scored_count: 0,
            last_access: None,
            last_use_at: None,
            revision: 1,
            vector: None,
            retention_policy: None,
            domain: None,
            metadata: serde_json::json!({}),
            recall_count: 0,
            query_diversity: 0,
            tier: "raw".to_string(),
        };
        store
            .insert_if_absent(&entry)
            .expect("seed memory row through typed store API");
    }

    fn retire_existing_fixture(store: &MemoryStore, id: &str) {
        let _authorization =
            crate::db::authorize_reserved_reference_write(&store.reserved_reference_write)
                .expect("authorize raw legacy sticky fixture");
        store
            .connection()
            .execute(
                "UPDATE memories SET path='/sticky/legacy', category='sticky' WHERE id=?1",
                [id],
            )
            .expect("turn ordinary seed into raw legacy sticky fixture");
    }

    #[derive(Debug, PartialEq)]
    struct AccessRow {
        id: String,
        access_count: i64,
        scored_count: i64,
        last_access: Option<String>,
        last_use_at: Option<String>,
    }

    #[derive(Debug, PartialEq)]
    struct AccessSnapshot {
        memories: Vec<AccessRow>,
        history: i64,
    }

    fn access_snapshot(conn: &Connection) -> AccessSnapshot {
        let memories = conn
            .prepare(
                "SELECT id,access_count,scored_count,last_access,last_use_at \
                 FROM memories ORDER BY id",
            )
            .expect("prepare access snapshot")
            .query_map([], |row| {
                Ok(AccessRow {
                    id: row.get(0)?,
                    access_count: row.get(1)?,
                    scored_count: row.get(2)?,
                    last_access: row.get(3)?,
                    last_use_at: row.get(4)?,
                })
            })
            .expect("read access snapshot")
            .collect::<Result<Vec<_>, _>>()
            .expect("collect access snapshot");
        let history = conn
            .query_row("SELECT COUNT(*) FROM access_history", [], |row| row.get(0))
            .expect("count access history");
        AccessSnapshot { memories, history }
    }

    /// Inserts `count` `access_history` rows for `id`, each one second older
    /// than the last (row 0 = most recent = `now`), so the returned ages are
    /// deterministic and ordering is unambiguous.
    fn seed_access_history(conn: &Connection, id: &str, count: i64) {
        let now = Utc::now();
        for i in 0..count {
            let at = (now - ChronoDuration::seconds(i)).to_rfc3339();
            conn.execute(
                "INSERT INTO access_history (memory_id, accessed_at, query_hash) VALUES (?1, ?2, '')",
                rusqlite::params![id, at],
            )
            .expect("seed access_history row");
        }
    }

    #[test]
    fn caps_at_max_per_memory_and_keeps_most_recent() {
        let mut store = MemoryStore::open_in_memory().expect("open in-memory store");
        seed_memory(&mut store, "busy");
        let conn = store.connection();
        // One more row than the cap — the oldest single row must be dropped.
        seed_access_history(conn, "busy", ACCESS_TIMES_MAX_PER_MEMORY + 1);

        let times = get_access_times(conn, &["busy".to_string()]).expect("get_access_times");
        let ages = times.get("busy").expect("busy has access history");

        assert_eq!(
            ages.len(),
            ACCESS_TIMES_MAX_PER_MEMORY as usize,
            "must truncate to the cap, not return all {}+1 rows",
            ACCESS_TIMES_MAX_PER_MEMORY
        );
        // Row `count-1` (age ~= count-1 seconds, the OLDEST inserted row) must
        // be the one dropped; the youngest row (age ~= 0s) must survive.
        let max_age = ages.iter().cloned().fold(0.0_f64, f64::max);
        assert!(
            max_age < ACCESS_TIMES_MAX_PER_MEMORY as f64,
            "oldest surviving access must be younger than the row that got dropped, got max_age={max_age}"
        );
    }

    #[test]
    fn under_cap_is_unaffected() {
        let mut store = MemoryStore::open_in_memory().expect("open in-memory store");
        seed_memory(&mut store, "quiet");
        let conn = store.connection();
        seed_access_history(conn, "quiet", 3);

        let times = get_access_times(conn, &["quiet".to_string()]).expect("get_access_times");
        assert_eq!(times.get("quiet").map(Vec::len), Some(3));
    }

    /// tachi#1446 signal D + lever 5, in one assertion set: the use write
    /// lands, and the two reads are genuinely separated in both directions.
    ///
    /// The negative half is the load-bearing one. If `get_access_times` did
    /// not filter, the use row would raise the ACT-R base-level-activation
    /// floor at **default** config — the new signal leaking straight back into
    /// the channel #1446 exists to repair, and invisible to any test that only
    /// checked the knob-on path.
    #[test]
    fn record_memory_use_writes_use_provenance_the_display_read_cannot_see() {
        let mut store = MemoryStore::open_in_memory().expect("open in-memory store");
        seed_memory(&mut store, "cited");
        seed_memory(&mut store, "shown");
        let conn = store.connection();
        seed_access_history(conn, "shown", 2);
        seed_access_history(conn, "cited", 1);

        let marked = record_memory_use(
            conn,
            &[
                "cited".to_string(),
                "cited".to_string(),
                "ghost".to_string(),
            ],
            "2026-07-26T00:00:00Z",
        )
        .expect("record_memory_use");
        assert_eq!(
            marked, 1,
            "duplicate ids collapse and an id with no memories row is skipped"
        );

        let ids = vec!["cited".to_string(), "shown".to_string()];
        let display = get_access_times(conn, &ids).expect("display read");
        assert_eq!(
            display.get("cited").map(Vec::len),
            Some(1),
            "the display read must still see the display row, and ONLY it — a use event must \
             not reach the ACT-R floor at default config"
        );
        assert_eq!(display.get("shown").map(Vec::len), Some(2));

        let uses = get_use_access_times(conn, &ids).expect("use read");
        assert_eq!(
            uses.get("cited").map(Vec::len),
            Some(1),
            "the use read must see the recorded use event"
        );
        assert!(
            !uses.contains_key("shown"),
            "a row that was only ever displayed has no use history — the use read must omit \
             the id entirely, not return an empty vector for it"
        );

        let last_use_at: Option<String> = conn
            .query_row(
                "SELECT last_use_at FROM memories WHERE id = 'cited'",
                [],
                |row| row.get(0),
            )
            .expect("read last_use_at");
        assert_eq!(last_use_at.as_deref(), Some("2026-07-26T00:00:00Z"));

        let shown_last_use_at: Option<String> = conn
            .query_row(
                "SELECT last_use_at FROM memories WHERE id = 'shown'",
                [],
                |row| row.get(0),
            )
            .expect("read last_use_at");
        assert_eq!(
            shown_last_use_at, None,
            "being displayed must never set the use timestamp"
        );

        let display_counters: (i64, Option<String>) = conn
            .query_row(
                "SELECT access_count, last_access FROM memories WHERE id = 'cited'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("read display counters");
        assert_eq!(
            display_counters,
            (0, None),
            "a use event must not touch the display-side counters, or the two provenances \
             re-merge and the discriminator is decorative"
        );
    }

    #[test]
    fn access_batches_refuse_retired_sticky_without_partially_mutating_ordinary_rows() {
        for operation in ["display", "use"] {
            let mut store = MemoryStore::open_in_memory().expect("open in-memory store");
            seed_memory(&mut store, "ordinary-first");
            seed_memory(&mut store, "sticky-second");
            retire_existing_fixture(&store, "sticky-second");
            let ids = vec!["ordinary-first".to_string(), "sticky-second".to_string()];
            let before = access_snapshot(store.connection());

            let error = match operation {
                "display" => record_access_with_updates(
                    store.connection(),
                    &ids,
                    &ids,
                    &ids,
                    Some("sticky batch"),
                    &crate::RecallConfig::default(),
                    None,
                )
                .map(|_| ()),
                "use" => {
                    record_memory_use(store.connection(), &ids, "2026-08-13T00:00:00Z").map(|_| ())
                }
                _ => unreachable!(),
            }
            .expect_err("a sticky member must refuse the complete access batch");
            assert!(error.to_string().contains("tachi_a2a"), "{error}");
            assert_eq!(
                access_snapshot(store.connection()),
                before,
                "{operation} refusal must not partially mutate the ordinary first row"
            );
        }
    }

    /// tachi#1446: the density instrument returns a number on a fixture,
    /// including the number that matters most — zero use events.
    #[test]
    fn access_event_density_reports_a_number_for_every_kind() {
        let mut store = MemoryStore::open_in_memory().expect("open in-memory store");
        seed_memory(&mut store, "a");
        seed_memory(&mut store, "b");
        let conn = store.connection();

        let empty = access_event_density(conn, "1970-01-01T00:00:00Z").expect("density");
        assert_eq!(empty.events_by_kind.get("use"), Some(&0));
        assert_eq!(empty.events_by_kind.get("display"), Some(&0));
        assert_eq!(empty.memories_with_last_use_at, 0);
        assert_eq!(empty.memories_total, 2);

        seed_access_history(conn, "a", 3);
        seed_access_history(conn, "b", 1);
        record_memory_use(conn, &["a".to_string()], "2026-07-26T00:00:00Z").expect("mark used");

        let density = access_event_density(conn, "1970-01-01T00:00:00Z").expect("density");
        assert_eq!(density.events_by_kind.get("display"), Some(&4));
        assert_eq!(density.memories_by_kind.get("display"), Some(&2));
        assert_eq!(density.events_by_kind.get("use"), Some(&1));
        assert_eq!(density.memories_by_kind.get("use"), Some(&1));
        assert_eq!(density.memories_with_last_use_at, 1);
        assert_eq!(density.memories_total, 2);

        // A window that starts after every seeded row still answers with
        // numbers, not with missing keys.
        let future = access_event_density(conn, "2099-01-01T00:00:00Z").expect("density");
        assert_eq!(future.events_by_kind.get("display"), Some(&0));
        assert_eq!(future.events_by_kind.get("use"), Some(&0));
        assert_eq!(
            future.memories_with_last_use_at, 1,
            "the last_use_at coverage counter is all-time by contract, not windowed"
        );
    }
}
