//! Offline self-query coverage measurement for rows invisible to normal recall.
//!
//! The probe is intentionally a portable MemCore operation: it uses the same
//! hybrid-search kernel as normal recall, but keeps `record_access` false so
//! measurement cannot create the search evidence it is trying to inspect.

use std::collections::HashSet;

use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use crate::{
    hybrid_search, is_namespace_search_noise, path_in_namespace, MemoryEntry, MemoryError,
    MemoryStore, SearchOptions,
};

/// Default result width for the offline recall-coverage action.
pub const DEFAULT_RECALL_COVERAGE_TOP_K: usize = 6;
/// Default candidate width for each hybrid-search channel in the offline action.
pub const DEFAULT_RECALL_COVERAGE_CANDIDATES_PER_CHANNEL: usize = 20;

const PATH_LIST_ONLY_NAMESPACES: [&str; 5] = [
    "/guide",
    "/cards",
    "/sticky",
    "/components/v0",
    "/agent/checkpoints",
];

/// Explicit, action-scoped options for [`run_recall_coverage_probe`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RecallCoverageOptions {
    pub top_k: usize,
    pub candidates_per_channel: usize,
    pub limit: Option<usize>,
}

impl Default for RecallCoverageOptions {
    fn default() -> Self {
        Self {
            top_k: DEFAULT_RECALL_COVERAGE_TOP_K,
            candidates_per_channel: DEFAULT_RECALL_COVERAGE_CANDIDATES_PER_CHANNEL,
            limit: None,
        }
    }
}

impl RecallCoverageOptions {
    fn validate(&self) -> Result<(), MemoryError> {
        if self.top_k == 0 {
            return Err(MemoryError::InvalidArg(
                "recall coverage invariant: top_k must be > 0".to_string(),
            ));
        }
        if self.candidates_per_channel == 0 {
            return Err(MemoryError::InvalidArg(
                "recall coverage invariant: candidates_per_channel must be > 0".to_string(),
            ));
        }
        if self.limit == Some(0) {
            return Err(MemoryError::InvalidArg(
                "recall coverage invariant: limit must be > 0 when provided".to_string(),
            ));
        }
        Ok(())
    }
}

fn validate_recall_coverage_search_invariant(
    include_superseded_env_override_active: bool,
) -> Result<(), MemoryError> {
    if include_superseded_env_override_active {
        return Err(MemoryError::InvalidArg(
            "recall coverage invariant: TACHI_SEARCH_INCLUDE_SUPERSEDED must be disabled because the probe requires superseded rows to remain excluded"
                .to_string(),
        ));
    }
    Ok(())
}

/// Counts for a mutually exclusive and exhaustive partition of every row.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct RecallCoveragePartitionCounts {
    pub total_rows: usize,
    pub archived: usize,
    pub superseded: usize,
    pub search_noise: usize,
    pub path_list_only: usize,
    pub already_surfaced: usize,
    pub eligible: usize,
}

impl RecallCoveragePartitionCounts {
    fn partition_sum(&self) -> usize {
        self.archived
            + self.superseded
            + self.search_noise
            + self.path_list_only
            + self.already_surfaced
            + self.eligible
    }

    fn validate_identity(&self) -> Result<(), MemoryError> {
        let partition_sum = self.partition_sum();
        if self.total_rows != partition_sum {
            return Err(MemoryError::InvalidArg(format!(
                "recall coverage invariant: population partition identity failed: total_rows={} partition_sum={partition_sum}",
                self.total_rows
            )));
        }
        Ok(())
    }
}

/// Split of the pre-probe scored-count state over every eligible row.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct RecallCoveragePriorScoredCountSplit {
    pub zero: usize,
    pub positive: usize,
    pub negative: usize,
}

/// Field chosen for a deterministic self-query. The query itself is never reported.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RecallCoverageQuerySource {
    Summary,
    Text,
    Topic,
    Keywords,
    Entities,
}

/// Per-target probe result without exposing source content or the generated query.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RecallCoverageOutcome {
    Surfaced,
    NotSurfaced,
    Unprobeable,
    /// A textual self-query exists, but no vector-backed hybrid probe can run.
    VectorUnavailable,
}

/// Content-free result detail for one selected eligible row.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RecallCoverageTarget {
    pub id: String,
    pub path: String,
    pub category: String,
    pub prior_scored_count: i64,
    pub query_source: Option<RecallCoverageQuerySource>,
    pub stored_vector_present: bool,
    pub outcome: RecallCoverageOutcome,
    pub rank: Option<usize>,
}

/// Serializable report for an offline self-query coverage run.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RecallCoverageReport {
    pub partition: RecallCoveragePartitionCounts,
    pub partition_invariant_holds: bool,
    pub top_k: usize,
    pub candidates_per_channel: usize,
    pub limit: Option<usize>,
    pub coverage_complete: bool,
    pub selected_eligible_rows: usize,
    pub unprobed_due_to_limit: usize,
    pub probed: usize,
    pub surfaced: usize,
    pub not_surfaced: usize,
    pub unprobeable: usize,
    pub vector_unavailable: usize,
    pub eligible_prior_scored_count: RecallCoveragePriorScoredCountSplit,
    pub targets: Vec<RecallCoverageTarget>,
}

/// True only for an exact namespace path or one of its descendants.
///
/// This is deliberately separate from the general unscoped-search noise
/// predicate: these namespaces are confirmed path-list-only populations, not
/// generic search noise, so the report can account for them separately.
pub fn is_recall_coverage_path_list_only(path: &str) -> bool {
    PATH_LIST_ONLY_NAMESPACES
        .iter()
        .any(|namespace| path_in_namespace(path, namespace))
}

fn superseded_ids(conn: &Connection) -> Result<HashSet<String>, MemoryError> {
    let mut statement = conn.prepare("SELECT id FROM memories WHERE superseded_by IS NOT NULL")?;
    let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
    rows.collect::<Result<HashSet<_>, _>>().map_err(Into::into)
}

fn is_non_empty(value: &str) -> bool {
    !value.trim().is_empty()
}

fn join_non_empty(values: &[String]) -> Option<String> {
    let selected: Vec<&str> = values
        .iter()
        .map(String::as_str)
        .filter(|value| is_non_empty(value))
        .collect();
    (!selected.is_empty()).then(|| selected.join(" "))
}

fn deterministic_self_query(entry: &MemoryEntry) -> Option<(RecallCoverageQuerySource, String)> {
    if is_non_empty(&entry.summary) {
        return Some((RecallCoverageQuerySource::Summary, entry.summary.clone()));
    }
    if is_non_empty(&entry.text) {
        return Some((RecallCoverageQuerySource::Text, entry.text.clone()));
    }
    if is_non_empty(&entry.topic) {
        return Some((RecallCoverageQuerySource::Topic, entry.topic.clone()));
    }
    if let Some(query) = join_non_empty(&entry.keywords) {
        return Some((RecallCoverageQuerySource::Keywords, query));
    }
    join_non_empty(&entry.entities).map(|query| (RecallCoverageQuerySource::Entities, query))
}

fn sort_eligible(entries: &mut [MemoryEntry]) {
    entries.sort_by(|left, right| {
        left.id
            .cmp(&right.id)
            .then_with(|| left.path.cmp(&right.path))
            .then_with(|| left.category.cmp(&right.category))
    });
}

/// Measure whether every selected never-search-surfaced row can retrieve itself.
///
/// The whole operation runs inside one read transaction. It uses the existing
/// Rust hybrid-search kernel with an explicit `record_access = false`, no path
/// scope, no archived/superseded visibility, and graph expansion disabled.
/// The report deliberately contains identifiers and metadata only, never a
/// content field or the transient query constructed from one.
pub fn run_recall_coverage_probe(
    store: &MemoryStore,
    options: RecallCoverageOptions,
) -> Result<RecallCoverageReport, MemoryError> {
    options.validate()?;
    validate_recall_coverage_search_invariant(
        crate::search::include_superseded_env_override_active(),
    )?;

    // `unchecked_transaction` begins a single SQLite read snapshot. The
    // store may itself be read-only; hybrid search stays read-only because the
    // options below set `record_access` false.
    let transaction = store.connection().unchecked_transaction()?;
    let all_entries = crate::db::get_all(&transaction, i64::MAX as usize, true)?;
    let superseded = superseded_ids(&transaction)?;

    let mut partition = RecallCoveragePartitionCounts {
        total_rows: all_entries.len(),
        ..Default::default()
    };
    let mut eligible = Vec::new();
    for entry in all_entries {
        if entry.archived {
            partition.archived += 1;
        } else if superseded.contains(&entry.id) {
            partition.superseded += 1;
        } else if is_namespace_search_noise(&entry, None) {
            partition.search_noise += 1;
        } else if is_recall_coverage_path_list_only(&entry.path) {
            partition.path_list_only += 1;
        } else if entry.access_count > 0 {
            partition.already_surfaced += 1;
        } else {
            partition.eligible += 1;
            eligible.push(entry);
        }
    }
    partition.validate_identity()?;

    let mut eligible_prior_scored_count = RecallCoveragePriorScoredCountSplit::default();
    for entry in &eligible {
        match entry.scored_count.cmp(&0) {
            std::cmp::Ordering::Less => eligible_prior_scored_count.negative += 1,
            std::cmp::Ordering::Equal => eligible_prior_scored_count.zero += 1,
            std::cmp::Ordering::Greater => eligible_prior_scored_count.positive += 1,
        }
    }

    sort_eligible(&mut eligible);
    let selected_eligible_rows = options.limit.unwrap_or(eligible.len()).min(eligible.len());
    let unprobed_due_to_limit = eligible.len() - selected_eligible_rows;
    let selected = &eligible[..selected_eligible_rows];
    let selected_ids: Vec<String> = selected.iter().map(|entry| entry.id.clone()).collect();
    let hydrated = crate::db::fetch_by_ids(&transaction, &selected_ids, true)?;

    let mut targets = Vec::with_capacity(selected.len());
    let mut probed = 0;
    let mut surfaced = 0;
    let mut not_surfaced = 0;
    let mut unprobeable = 0;
    let mut vector_unavailable = 0;

    for listed_entry in selected {
        let entry = hydrated.get(&listed_entry.id).ok_or_else(|| {
            MemoryError::InvalidArg(format!(
                "recall coverage invariant: stable population target disappeared during vector hydration: {}",
                listed_entry.id
            ))
        })?;
        let stored_vector_present = entry.vector.is_some();
        let Some((query_source, query)) = deterministic_self_query(entry) else {
            unprobeable += 1;
            targets.push(RecallCoverageTarget {
                id: entry.id.clone(),
                path: entry.path.clone(),
                category: entry.category.clone(),
                prior_scored_count: entry.scored_count,
                query_source: None,
                stored_vector_present,
                outcome: RecallCoverageOutcome::Unprobeable,
                rank: None,
            });
            continue;
        };

        // Construct the textual query before checking vector availability so a
        // content-free target stays Unprobeable even if its vector leg is also
        // unavailable. A query-bearing target without a usable vector is not
        // lexical-only coverage evidence and must not enter hybrid_search.
        if !store.vec_available || entry.vector.is_none() {
            vector_unavailable += 1;
            targets.push(RecallCoverageTarget {
                id: entry.id.clone(),
                path: entry.path.clone(),
                category: entry.category.clone(),
                prior_scored_count: entry.scored_count,
                query_source: Some(query_source),
                stored_vector_present,
                outcome: RecallCoverageOutcome::VectorUnavailable,
                rank: None,
            });
            continue;
        }

        let search_options = SearchOptions {
            top_k: options.top_k,
            candidates_per_channel: options.candidates_per_channel,
            path_prefix: None,
            query_vec: entry.vector.clone(),
            vec_available: store.vec_available,
            record_access: false,
            include_archived: false,
            include_superseded: false,
            graph_expand_hops: 0,
            graph_relation_filter: None,
            ..Default::default()
        };
        let results = hybrid_search(&transaction, &query, &search_options)?;
        probed += 1;
        let rank = results
            .iter()
            .position(|result| result.entry.id == entry.id)
            .map(|index| index + 1);
        let outcome = if rank.is_some() {
            surfaced += 1;
            RecallCoverageOutcome::Surfaced
        } else {
            not_surfaced += 1;
            RecallCoverageOutcome::NotSurfaced
        };
        targets.push(RecallCoverageTarget {
            id: entry.id.clone(),
            path: entry.path.clone(),
            category: entry.category.clone(),
            prior_scored_count: entry.scored_count,
            query_source: Some(query_source),
            stored_vector_present,
            outcome,
            rank,
        });
    }

    if probed != surfaced + not_surfaced
        || selected_eligible_rows != probed + unprobeable + vector_unavailable
    {
        return Err(MemoryError::InvalidArg(format!(
            "recall coverage invariant: outcome accounting identity failed: selected={selected_eligible_rows} probed={probed} surfaced={surfaced} not_surfaced={not_surfaced} unprobeable={unprobeable} vector_unavailable={vector_unavailable}"
        )));
    }

    Ok(RecallCoverageReport {
        partition,
        partition_invariant_holds: true,
        top_k: options.top_k,
        candidates_per_channel: options.candidates_per_channel,
        limit: options.limit,
        coverage_complete: unprobed_due_to_limit == 0,
        selected_eligible_rows,
        unprobed_due_to_limit,
        probed,
        surfaced,
        not_surfaced,
        unprobeable,
        vector_unavailable,
        eligible_prior_scored_count,
        targets,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recall_coverage_rejects_active_superseded_env_override_without_mutating_process_env() {
        assert!(validate_recall_coverage_search_invariant(false).is_ok());
        let error = validate_recall_coverage_search_invariant(true)
            .expect_err("active override must reject the probe");
        assert!(
            error
                .to_string()
                .contains("recall coverage invariant: TACHI_SEARCH_INCLUDE_SUPERSEDED"),
            "guard must name the invariant and environment override: {error}"
        );
    }
}
