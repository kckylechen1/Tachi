//! Offline self-query coverage measurement for rows invisible to normal recall.
//!
//! The probe is intentionally a portable MemCore operation: it uses the same
//! hybrid-search kernel as normal recall, but keeps `record_access` false so
//! measurement cannot create the search evidence it is trying to inspect.

use std::collections::{HashMap, HashSet};

use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use crate::{
    is_namespace_search_noise, path_in_namespace, CandidateLegEvidence, MemoryEntry, MemoryError,
    MemoryStore, SearchOptions,
};

/// Default result width for the offline recall-coverage action.
pub const DEFAULT_RECALL_COVERAGE_TOP_K: usize = 6;
/// Default candidate width for each hybrid-search channel in the offline action.
pub const DEFAULT_RECALL_COVERAGE_CANDIDATES_PER_CHANNEL: usize = 20;
/// Versioned schema accepted by the optional reviewed-equivalence corpus.
pub const RECALL_COVERAGE_EQUIVALENCE_SCHEMA_VERSION: &str =
    "tachi.recall_coverage.reviewed_equivalence.v1";

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

/// One reviewed equivalence set supplied by an audited offline corpus.
///
/// IDs are the only memory data carried here. `evidence_source` names the
/// review artifact or receipt that authorized the equivalence; it must not
/// contain memory content.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RecallCoverageEquivalenceSet {
    pub canonical_id: String,
    pub equivalent_ids: Vec<String>,
    pub evidence_source: String,
}

/// Versioned file shape for `tachi recall-coverage --equivalence-file`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RecallCoverageEquivalenceCorpus {
    pub schema_version: String,
    /// Optional reviewed expected-row lane. These rows are probed independently
    /// of the legacy active self-query population, so historical superseded IDs
    /// can be evaluated without changing legacy exact totals.
    #[serde(default)]
    pub expected_ids: Vec<String>,
    pub equivalences: Vec<RecallCoverageEquivalenceSet>,
}

impl RecallCoverageEquivalenceCorpus {
    pub fn validate(&self) -> Result<(), MemoryError> {
        if self.schema_version != RECALL_COVERAGE_EQUIVALENCE_SCHEMA_VERSION {
            return Err(MemoryError::InvalidArg(format!(
                "recall coverage equivalence invariant: schema_version must be {RECALL_COVERAGE_EQUIVALENCE_SCHEMA_VERSION}, got {}",
                self.schema_version
            )));
        }
        let mut expected_ids = HashSet::new();
        for expected_id in &self.expected_ids {
            if expected_id.trim().is_empty() {
                return Err(MemoryError::InvalidArg(
                    "recall coverage expected-id invariant: expected_id must be non-empty"
                        .to_string(),
                ));
            }
            if !expected_ids.insert(expected_id) {
                return Err(MemoryError::InvalidArg(format!(
                    "recall coverage expected-id invariant: duplicate expected_id {expected_id}"
                )));
            }
        }
        validate_reviewed_equivalences(&self.equivalences).map(|_| ())
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
    /// The target was present in the executed vector candidate set and appears
    /// in the final hybrid top-k.
    Surfaced,
    /// The target failed at least one vector-qualified coverage condition: it
    /// was absent from the executed vector candidate set or from final hybrid
    /// top-k. A lexical/symbolic-only final hit remains `NotSurfaced`.
    NotSurfaced,
    Unprobeable,
    /// A textual self-query exists, but no vector-backed hybrid probe can run.
    VectorUnavailable,
}

/// Auditable authority used for a canonical fact/lineage verdict.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RecallCoverageEvidenceKind {
    ExactIdentity,
    ReviewedEquivalence,
    StoredSupersessionLineage,
}

/// Why a row was excluded from the legacy exact-ID population partition,
/// mirroring the mutually exclusive branches in the partition classification.
/// This never mutates `outcome`: it explains a miss, it does not skip the probe.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RecallCoverageFilterReason {
    Archived,
    Superseded,
    NamespaceSearchNoise,
    PathListOnly,
    AlreadySurfaced,
}

/// Content-free evidence for resolving one expected row to one canonical row.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RecallCoverageFactEvidence {
    pub kind: RecallCoverageEvidenceKind,
    pub source: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub lineage: Vec<String>,
}

/// Aggregate Recall@K and MRR for one independently reported identity metric.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct RecallCoverageMetrics {
    pub denominator: usize,
    pub hits: usize,
    pub recall_at_k: f64,
    pub mrr: f64,
}

impl RecallCoverageMetrics {
    fn from_hit_ranks(denominator: usize, hit_ranks: impl Iterator<Item = usize>) -> Self {
        let (hits, reciprocal_rank_sum) = hit_ranks.fold((0, 0.0), |(hits, sum), rank| {
            (hits + 1, sum + 1.0 / rank as f64)
        });
        let recall_at_k = if denominator == 0 {
            0.0
        } else {
            hits as f64 / denominator as f64
        };
        let mrr = if denominator == 0 {
            0.0
        } else {
            reciprocal_rank_sum / denominator as f64
        };
        Self {
            denominator,
            hits,
            recall_at_k,
            mrr,
        }
    }
}

/// Content-free result detail for one legacy-selected or reviewed expected row.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RecallCoverageTarget {
    pub id: String,
    pub path: String,
    pub category: String,
    pub prior_scored_count: i64,
    pub query_source: Option<RecallCoverageQuerySource>,
    pub stored_vector_present: bool,
    pub outcome: RecallCoverageOutcome,
    /// One-based position in the final hybrid top-k, independent of vector
    /// provenance. This can be `Some` while `outcome` is `NotSurfaced` when the
    /// target entered the merged candidate union through lexical/symbolic only.
    pub rank: Option<usize>,
    /// Candidate-leg membership for the exact target when hybrid search ran.
    pub exact_candidate_legs: Option<CandidateLegEvidence>,
    /// Independent canonical fact/lineage result. This never mutates `outcome`.
    /// A canonical fact is surfaced when the authority-selected canonical row
    /// is present in final hybrid top-k, regardless of which candidate leg
    /// contributed it.
    pub canonical_fact_outcome: RecallCoverageOutcome,
    /// Stricter diagnostic retained separately from canonical fact coverage:
    /// the canonical row must be present in both the vector candidate leg and
    /// final hybrid top-k.
    pub canonical_vector_qualified_outcome: RecallCoverageOutcome,
    /// Canonical row selected only through exact identity, reviewed corpus data,
    /// or stored `superseded_by` lineage.
    pub canonical_id: Option<String>,
    /// Canonical row actually present in final hybrid top-k, if any.
    pub matched_canonical_id: Option<String>,
    pub canonical_rank: Option<usize>,
    pub canonical_candidate_legs: Option<CandidateLegEvidence>,
    pub fact_evidence: RecallCoverageFactEvidence,
    /// Why this row was excluded from the legacy exact-ID population partition,
    /// if it was. The partition classification is carried through rather than
    /// discarded so the expected-ID lane can explain a miss without ever
    /// skipping its probe.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filter_reason: Option<RecallCoverageFilterReason>,
}

/// Serializable report for an offline self-query coverage run.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
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
    pub exact_metrics: RecallCoverageMetrics,
    pub canonical_fact_metrics: RecallCoverageMetrics,
    pub canonical_vector_qualified_metrics: RecallCoverageMetrics,
    pub targets: Vec<RecallCoverageTarget>,
    /// Optional reviewed expected-ID lane, independent of legacy population
    /// selection and exact totals.
    pub expected_id_lane: RecallCoverageExpectedIdLane,
}

/// Metrics and cases for explicitly reviewed expected IDs, including archived
/// or superseded historical rows that the legacy self-query population excludes.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct RecallCoverageExpectedIdLane {
    pub requested: usize,
    pub probed: usize,
    pub unprobeable: usize,
    pub vector_unavailable: usize,
    pub exact_metrics: RecallCoverageMetrics,
    pub canonical_fact_metrics: RecallCoverageMetrics,
    pub canonical_vector_qualified_metrics: RecallCoverageMetrics,
    pub cases: Vec<RecallCoverageTarget>,
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

/// Classify one row against the same mutually exclusive partition the legacy
/// population loop uses, without discarding the branch taken: `None` means
/// the row is eligible for the legacy exact-ID population, `Some(reason)`
/// names which non-eligible bucket it falls into. Shared by the legacy
/// partition loop and the expected-ID lane so both apply an identical check.
fn recall_coverage_filter_reason(
    entry: &MemoryEntry,
    supersession_links: &HashMap<String, Option<String>>,
) -> Option<RecallCoverageFilterReason> {
    if entry.archived {
        Some(RecallCoverageFilterReason::Archived)
    } else if supersession_links
        .get(&entry.id)
        .is_some_and(Option::is_some)
    {
        Some(RecallCoverageFilterReason::Superseded)
    } else if is_namespace_search_noise(entry, None) {
        Some(RecallCoverageFilterReason::NamespaceSearchNoise)
    } else if is_recall_coverage_path_list_only(&entry.path) {
        Some(RecallCoverageFilterReason::PathListOnly)
    } else if entry.access_count > 0 {
        Some(RecallCoverageFilterReason::AlreadySurfaced)
    } else {
        None
    }
}

fn supersession_links(conn: &Connection) -> Result<HashMap<String, Option<String>>, MemoryError> {
    let mut statement = conn.prepare("SELECT id, superseded_by FROM memories")?;
    let rows = statement.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
    })?;
    rows.collect::<Result<HashMap<_, _>, _>>()
        .map_err(Into::into)
}

struct CanonicalResolution {
    canonical_id: String,
    evidence: RecallCoverageFactEvidence,
}

enum StoredLineageResolution {
    None,
    Authoritative(CanonicalResolution),
    Dangling { missing_id: String },
}

fn stored_lineage_evidence_from_links(
    links: &HashMap<String, Option<String>>,
    target_id: &str,
) -> Result<StoredLineageResolution, MemoryError> {
    let mut current = target_id.to_string();
    let mut lineage = vec![current.clone()];
    let mut seen = HashSet::from([current.clone()]);

    loop {
        match links.get(&current) {
            Some(Some(next)) => {
                if !seen.insert(next.clone()) {
                    lineage.push(next.clone());
                    return Err(MemoryError::InvalidArg(format!(
                        "recall coverage lineage invariant: superseded_by cycle while resolving target {target_id}: {}",
                        lineage.join(" -> ")
                    )));
                }
                current = next.clone();
                lineage.push(current.clone());
            }
            Some(None) => break,
            None => {
                return Ok(StoredLineageResolution::Dangling {
                    missing_id: current,
                });
            }
        }
    }

    if lineage.len() == 1 {
        return Ok(StoredLineageResolution::None);
    }
    Ok(StoredLineageResolution::Authoritative(
        CanonicalResolution {
            canonical_id: current,
            evidence: RecallCoverageFactEvidence {
                kind: RecallCoverageEvidenceKind::StoredSupersessionLineage,
                source: "memories.superseded_by".to_string(),
                lineage,
            },
        },
    ))
}

fn validate_reviewed_equivalences(
    equivalences: &[RecallCoverageEquivalenceSet],
) -> Result<HashMap<&str, &RecallCoverageEquivalenceSet>, MemoryError> {
    let mut by_equivalent_id = HashMap::new();
    let mut set_owner_by_id: HashMap<&str, &str> = HashMap::new();
    for set in equivalences {
        if set.canonical_id.trim().is_empty() {
            return Err(MemoryError::InvalidArg(
                "recall coverage equivalence invariant: canonical_id must be non-empty".to_string(),
            ));
        }
        if set.evidence_source.trim().is_empty() {
            return Err(MemoryError::InvalidArg(format!(
                "recall coverage equivalence invariant: evidence_source must be non-empty for canonical_id {}",
                set.canonical_id
            )));
        }
        if !set.evidence_source.chars().all(|character| {
            character.is_ascii_alphanumeric()
                || matches!(character, '.' | '_' | ':' | '/' | '#' | '-' | '@')
        }) {
            return Err(MemoryError::InvalidArg(format!(
                "recall coverage equivalence invariant: evidence_source must be a content-free reference identifier for canonical_id {}",
                set.canonical_id
            )));
        }
        if set.equivalent_ids.is_empty() {
            return Err(MemoryError::InvalidArg(format!(
                "recall coverage equivalence invariant: equivalent_ids must be non-empty for canonical_id {}",
                set.canonical_id
            )));
        }
        if let Some(previous_owner) =
            set_owner_by_id.insert(set.canonical_id.as_str(), set.canonical_id.as_str())
        {
            return Err(MemoryError::InvalidArg(format!(
                "recall coverage equivalence invariant: row id {} overlaps canonical sets {} and {}",
                set.canonical_id, previous_owner, set.canonical_id
            )));
        }
        let mut within_set = HashSet::new();
        for equivalent_id in &set.equivalent_ids {
            if equivalent_id.trim().is_empty() {
                return Err(MemoryError::InvalidArg(format!(
                    "recall coverage equivalence invariant: equivalent_id must be non-empty for canonical_id {}",
                    set.canonical_id
                )));
            }
            if equivalent_id == &set.canonical_id {
                return Err(MemoryError::InvalidArg(format!(
                    "recall coverage equivalence invariant: canonical_id {} must not be repeated in equivalent_ids",
                    set.canonical_id
                )));
            }
            if !within_set.insert(equivalent_id.as_str()) {
                return Err(MemoryError::InvalidArg(format!(
                    "recall coverage equivalence invariant: duplicate equivalent_id {equivalent_id} for canonical_id {}",
                    set.canonical_id
                )));
            }
            if let Some(previous_owner) =
                set_owner_by_id.insert(equivalent_id.as_str(), set.canonical_id.as_str())
            {
                return Err(MemoryError::InvalidArg(format!(
                    "recall coverage equivalence invariant: row id {equivalent_id} overlaps canonical sets {previous_owner} and {}",
                    set.canonical_id
                )));
            }
            if let Some(previous) = by_equivalent_id.insert(equivalent_id.as_str(), set) {
                return Err(MemoryError::InvalidArg(format!(
                    "recall coverage equivalence invariant: equivalent_id {equivalent_id} maps to both {} and {}",
                    previous.canonical_id, set.canonical_id
                )));
            }
        }
    }
    Ok(by_equivalent_id)
}

fn canonical_resolution(
    links: &HashMap<String, Option<String>>,
    reviewed: &HashMap<&str, &RecallCoverageEquivalenceSet>,
    target_id: &str,
) -> Result<CanonicalResolution, MemoryError> {
    let reviewed_resolution = |set: &RecallCoverageEquivalenceSet| CanonicalResolution {
        canonical_id: set.canonical_id.clone(),
        evidence: RecallCoverageFactEvidence {
            kind: RecallCoverageEvidenceKind::ReviewedEquivalence,
            source: set.evidence_source.clone(),
            lineage: vec![target_id.to_string(), set.canonical_id.clone()],
        },
    };

    match stored_lineage_evidence_from_links(links, target_id)? {
        StoredLineageResolution::Authoritative(lineage) => {
            if let Some(set) = reviewed.get(target_id) {
                if set.canonical_id != lineage.canonical_id {
                    return Err(MemoryError::InvalidArg(format!(
                        "recall coverage authority_conflict: stored lineage resolves target {target_id} to {}, but reviewed evidence {} resolves it to {}",
                        lineage.canonical_id, set.evidence_source, set.canonical_id
                    )));
                }
            }
            return Ok(lineage);
        }
        StoredLineageResolution::Dangling { missing_id } => {
            if let Some(set) = reviewed.get(target_id) {
                return Ok(reviewed_resolution(set));
            }
            return Err(MemoryError::InvalidArg(format!(
                "recall coverage lineage invariant: dangling superseded_by while resolving target {target_id}: missing terminal row {missing_id}"
            )));
        }
        StoredLineageResolution::None => {}
    }
    if let Some(set) = reviewed.get(target_id) {
        return Ok(reviewed_resolution(set));
    }
    Ok(CanonicalResolution {
        canonical_id: target_id.to_string(),
        evidence: RecallCoverageFactEvidence {
            kind: RecallCoverageEvidenceKind::ExactIdentity,
            source: "target.id".to_string(),
            lineage: Vec::new(),
        },
    })
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

fn vector_qualified_outcome(
    candidate_legs: Option<CandidateLegEvidence>,
    rank: Option<usize>,
) -> RecallCoverageOutcome {
    if candidate_legs.is_some_and(|legs| legs.vector) && rank.is_some() {
        RecallCoverageOutcome::Surfaced
    } else {
        RecallCoverageOutcome::NotSurfaced
    }
}

fn surface_outcome(rank: Option<usize>) -> RecallCoverageOutcome {
    if rank.is_some() {
        RecallCoverageOutcome::Surfaced
    } else {
        RecallCoverageOutcome::NotSurfaced
    }
}

fn probe_entry(
    conn: &Connection,
    store: &MemoryStore,
    options: &RecallCoverageOptions,
    entry: &MemoryEntry,
    planned_canonical: CanonicalResolution,
    filter_reason: Option<RecallCoverageFilterReason>,
) -> Result<RecallCoverageTarget, MemoryError> {
    let stored_vector_present = entry.vector.is_some();
    let Some((query_source, query)) = deterministic_self_query(entry) else {
        return Ok(RecallCoverageTarget {
            id: entry.id.clone(),
            path: entry.path.clone(),
            category: entry.category.clone(),
            prior_scored_count: entry.scored_count,
            query_source: None,
            stored_vector_present,
            outcome: RecallCoverageOutcome::Unprobeable,
            rank: None,
            exact_candidate_legs: None,
            canonical_fact_outcome: RecallCoverageOutcome::Unprobeable,
            canonical_vector_qualified_outcome: RecallCoverageOutcome::Unprobeable,
            canonical_id: Some(planned_canonical.canonical_id),
            matched_canonical_id: None,
            canonical_rank: None,
            canonical_candidate_legs: None,
            fact_evidence: planned_canonical.evidence,
            filter_reason,
        });
    };

    // Exact and canonical results share one executed self-query. A reviewed or
    // lineage-selected canonical row does not lend its vector to a historical
    // expected row: doing so would change the query representation and make
    // the two metrics incomparable. The expected-ID lane therefore reports
    // both outcomes as vector-unavailable until the expected row itself can
    // supply the frozen query vector.
    if !store.vec_available || entry.vector.is_none() {
        return Ok(RecallCoverageTarget {
            id: entry.id.clone(),
            path: entry.path.clone(),
            category: entry.category.clone(),
            prior_scored_count: entry.scored_count,
            query_source: Some(query_source),
            stored_vector_present,
            outcome: RecallCoverageOutcome::VectorUnavailable,
            rank: None,
            exact_candidate_legs: None,
            canonical_fact_outcome: RecallCoverageOutcome::VectorUnavailable,
            canonical_vector_qualified_outcome: RecallCoverageOutcome::VectorUnavailable,
            canonical_id: Some(planned_canonical.canonical_id),
            matched_canonical_id: None,
            canonical_rank: None,
            canonical_candidate_legs: None,
            fact_evidence: planned_canonical.evidence,
            filter_reason,
        });
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
    let mut observed_ids = vec![entry.id.clone()];
    if planned_canonical.canonical_id != entry.id {
        observed_ids.push(planned_canonical.canonical_id.clone());
    }
    let (results, candidate_legs) = crate::search::hybrid_search_with_candidate_leg_evidence(
        conn,
        &query,
        &search_options,
        &observed_ids,
    )?;
    let rank = results
        .iter()
        .position(|result| result.entry.id == entry.id)
        .map(|index| index + 1);
    let exact_candidate_legs = candidate_legs.get(&entry.id).copied();
    let outcome = vector_qualified_outcome(exact_candidate_legs, rank);
    let canonical = if outcome == RecallCoverageOutcome::Surfaced {
        CanonicalResolution {
            canonical_id: entry.id.clone(),
            evidence: RecallCoverageFactEvidence {
                kind: RecallCoverageEvidenceKind::ExactIdentity,
                source: "target.id".to_string(),
                lineage: Vec::new(),
            },
        }
    } else {
        planned_canonical
    };
    let canonical_rank = results
        .iter()
        .position(|result| result.entry.id == canonical.canonical_id)
        .map(|index| index + 1);
    let canonical_candidate_legs = candidate_legs.get(&canonical.canonical_id).copied();
    let canonical_fact_outcome = surface_outcome(canonical_rank);
    let canonical_vector_qualified_outcome =
        vector_qualified_outcome(canonical_candidate_legs, canonical_rank);

    Ok(RecallCoverageTarget {
        id: entry.id.clone(),
        path: entry.path.clone(),
        category: entry.category.clone(),
        prior_scored_count: entry.scored_count,
        query_source: Some(query_source),
        stored_vector_present,
        outcome,
        rank,
        exact_candidate_legs,
        canonical_fact_outcome,
        canonical_vector_qualified_outcome,
        matched_canonical_id: canonical_rank.map(|_| canonical.canonical_id.clone()),
        canonical_id: Some(canonical.canonical_id),
        canonical_rank,
        canonical_candidate_legs,
        fact_evidence: canonical.evidence,
        filter_reason,
    })
}

fn run_expected_id_lane(
    conn: &Connection,
    store: &MemoryStore,
    options: &RecallCoverageOptions,
    expected_ids: &[String],
    supersession_links: &HashMap<String, Option<String>>,
    reviewed_equivalences: &HashMap<&str, &RecallCoverageEquivalenceSet>,
) -> Result<RecallCoverageExpectedIdLane, MemoryError> {
    if expected_ids.is_empty() {
        return Ok(RecallCoverageExpectedIdLane::default());
    }

    let mut ordered_ids = expected_ids.to_vec();
    ordered_ids.sort();
    let hydrated = crate::db::fetch_by_ids(conn, &ordered_ids, true)?;
    let mut cases = Vec::with_capacity(ordered_ids.len());
    for expected_id in &ordered_ids {
        let entry = hydrated.get(expected_id).ok_or_else(|| {
            MemoryError::InvalidArg(format!(
                "recall coverage expected-id invariant: reviewed expected row is absent from the inspected database: {expected_id}"
            ))
        })?;
        let planned_canonical =
            canonical_resolution(supersession_links, reviewed_equivalences, expected_id)?;
        // The legacy population loop applies this same partition check before
        // deciding eligibility; the expected-ID lane skipped it entirely. A
        // filtered expected id still probes below — the reason explains a
        // miss, it never substitutes for one.
        let filter_reason = recall_coverage_filter_reason(entry, supersession_links);
        cases.push(probe_entry(
            conn,
            store,
            options,
            entry,
            planned_canonical,
            filter_reason,
        )?);
    }

    let probed = cases
        .iter()
        .filter(|case| {
            matches!(
                case.outcome,
                RecallCoverageOutcome::Surfaced | RecallCoverageOutcome::NotSurfaced
            )
        })
        .count();
    let unprobeable = cases
        .iter()
        .filter(|case| case.outcome == RecallCoverageOutcome::Unprobeable)
        .count();
    let vector_unavailable = cases
        .iter()
        .filter(|case| case.outcome == RecallCoverageOutcome::VectorUnavailable)
        .count();
    let exact_metrics = RecallCoverageMetrics::from_hit_ranks(
        probed,
        cases
            .iter()
            .filter(|case| case.outcome == RecallCoverageOutcome::Surfaced)
            .filter_map(|case| case.rank),
    );
    let canonical_fact_metrics = RecallCoverageMetrics::from_hit_ranks(
        probed,
        cases
            .iter()
            .filter(|case| case.canonical_fact_outcome == RecallCoverageOutcome::Surfaced)
            .filter_map(|case| case.canonical_rank),
    );
    let canonical_vector_qualified_metrics = RecallCoverageMetrics::from_hit_ranks(
        probed,
        cases
            .iter()
            .filter(|case| {
                case.canonical_vector_qualified_outcome == RecallCoverageOutcome::Surfaced
            })
            .filter_map(|case| case.canonical_rank),
    );

    Ok(RecallCoverageExpectedIdLane {
        requested: ordered_ids.len(),
        probed,
        unprobeable,
        vector_unavailable,
        exact_metrics,
        canonical_fact_metrics,
        canonical_vector_qualified_metrics,
        cases,
    })
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
    run_recall_coverage_probe_internal(store, options, &[], &[])
}

/// Run the offline probe with optional, explicitly reviewed equivalence sets.
///
/// The equivalence sets can only affect the separate canonical fact/lineage
/// verdict. Exact target selection, exact outcome accounting, and exact ranks
/// remain on the original path.
pub fn run_recall_coverage_probe_with_equivalences(
    store: &MemoryStore,
    options: RecallCoverageOptions,
    equivalences: &[RecallCoverageEquivalenceSet],
) -> Result<RecallCoverageReport, MemoryError> {
    run_recall_coverage_probe_internal(store, options, equivalences, &[])
}

/// Run both the legacy self-query lane and an independent reviewed expected-ID lane.
pub fn run_recall_coverage_probe_with_corpus(
    store: &MemoryStore,
    options: RecallCoverageOptions,
    corpus: &RecallCoverageEquivalenceCorpus,
) -> Result<RecallCoverageReport, MemoryError> {
    corpus.validate()?;
    run_recall_coverage_probe_internal(store, options, &corpus.equivalences, &corpus.expected_ids)
}

fn run_recall_coverage_probe_internal(
    store: &MemoryStore,
    options: RecallCoverageOptions,
    equivalences: &[RecallCoverageEquivalenceSet],
    expected_ids: &[String],
) -> Result<RecallCoverageReport, MemoryError> {
    options.validate()?;
    let reviewed_equivalences = validate_reviewed_equivalences(equivalences)?;
    validate_recall_coverage_search_invariant(
        crate::search::include_superseded_env_override_active(),
    )?;

    // `unchecked_transaction` begins a single SQLite read snapshot. The
    // store may itself be read-only; hybrid search stays read-only because the
    // options below set `record_access` false.
    // In-crate field access rather than `MemoryStore::connection()`: that
    // accessor is gated out of non-test portable builds (#1585 review round 2
    // — it is a raw-SQL bypass of the store_identity write-once guards), while
    // this probe is a legitimate portable read path.
    let transaction = store.conn.unchecked_transaction()?;
    let all_entries = crate::db::get_all(&transaction, i64::MAX as usize, true, false)?;
    let supersession_links = supersession_links(&transaction)?;

    let mut partition = RecallCoveragePartitionCounts {
        total_rows: all_entries.len(),
        ..Default::default()
    };
    let mut eligible = Vec::new();
    for entry in all_entries {
        // Carried through `RecallCoverageFilterReason` rather than discarded
        // as a bare counter increment: `None` here is exactly the condition
        // that admits the row to `eligible`, and `Some(reason)` is the same
        // value the expected-ID lane attaches to a filtered row below.
        match recall_coverage_filter_reason(&entry, &supersession_links) {
            Some(RecallCoverageFilterReason::Archived) => partition.archived += 1,
            Some(RecallCoverageFilterReason::Superseded) => partition.superseded += 1,
            Some(RecallCoverageFilterReason::NamespaceSearchNoise) => partition.search_noise += 1,
            Some(RecallCoverageFilterReason::PathListOnly) => partition.path_list_only += 1,
            Some(RecallCoverageFilterReason::AlreadySurfaced) => partition.already_surfaced += 1,
            None => {
                partition.eligible += 1;
                eligible.push(entry);
            }
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
        let planned_canonical =
            canonical_resolution(&supersession_links, &reviewed_equivalences, &entry.id)?;
        // `entry` is drawn from `eligible`, so this is always `None` by
        // construction (see the partition loop above); passed explicitly
        // rather than omitted so both lanes populate the same field the
        // same way instead of one silently leaving it unset.
        let filter_reason = recall_coverage_filter_reason(entry, &supersession_links);
        let target = probe_entry(
            &transaction,
            store,
            &options,
            entry,
            planned_canonical,
            filter_reason,
        )?;
        match target.outcome {
            RecallCoverageOutcome::Surfaced => {
                probed += 1;
                surfaced += 1;
            }
            RecallCoverageOutcome::NotSurfaced => {
                probed += 1;
                not_surfaced += 1;
            }
            RecallCoverageOutcome::Unprobeable => unprobeable += 1,
            RecallCoverageOutcome::VectorUnavailable => vector_unavailable += 1,
        }
        targets.push(target);
    }

    if probed != surfaced + not_surfaced
        || selected_eligible_rows != probed + unprobeable + vector_unavailable
    {
        return Err(MemoryError::InvalidArg(format!(
            "recall coverage invariant: outcome accounting identity failed: selected={selected_eligible_rows} probed={probed} surfaced={surfaced} not_surfaced={not_surfaced} unprobeable={unprobeable} vector_unavailable={vector_unavailable}"
        )));
    }

    let exact_metrics = RecallCoverageMetrics::from_hit_ranks(
        probed,
        targets
            .iter()
            .filter(|target| target.outcome == RecallCoverageOutcome::Surfaced)
            .filter_map(|target| target.rank),
    );
    let canonical_fact_metrics = RecallCoverageMetrics::from_hit_ranks(
        probed,
        targets
            .iter()
            .filter(|target| target.canonical_fact_outcome == RecallCoverageOutcome::Surfaced)
            .filter_map(|target| target.canonical_rank),
    );
    let canonical_vector_qualified_metrics = RecallCoverageMetrics::from_hit_ranks(
        probed,
        targets
            .iter()
            .filter(|target| {
                target.canonical_vector_qualified_outcome == RecallCoverageOutcome::Surfaced
            })
            .filter_map(|target| target.canonical_rank),
    );
    let expected_id_lane = run_expected_id_lane(
        &transaction,
        store,
        &options,
        expected_ids,
        &supersession_links,
        &reviewed_equivalences,
    )?;

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
        exact_metrics,
        canonical_fact_metrics,
        canonical_vector_qualified_metrics,
        targets,
        expected_id_lane,
    })
}

fn format_candidate_legs(legs: Option<CandidateLegEvidence>) -> String {
    match legs {
        Some(legs) => format!(
            "vector={} fts={} symbolic={} exact_id={}",
            legs.vector, legs.fts, legs.symbolic, legs.exact_id
        ),
        None => "not_executed".to_string(),
    }
}

/// Render a deterministic, content-free operator summary.
pub fn format_recall_coverage_human(report: &RecallCoverageReport) -> String {
    use std::fmt::Write;

    let mut output = String::new();
    writeln!(
        output,
        "Exact-ID Recall: {}/{} Recall@K={:.6} MRR={:.6}",
        report.exact_metrics.hits,
        report.exact_metrics.denominator,
        report.exact_metrics.recall_at_k,
        report.exact_metrics.mrr
    )
    .expect("writing to String cannot fail");
    writeln!(
        output,
        "Canonical Fact/Lineage Recall: {}/{} Recall@K={:.6} MRR={:.6}",
        report.canonical_fact_metrics.hits,
        report.canonical_fact_metrics.denominator,
        report.canonical_fact_metrics.recall_at_k,
        report.canonical_fact_metrics.mrr
    )
    .expect("writing to String cannot fail");
    writeln!(
        output,
        "Canonical Vector-Qualified Recall: {}/{} Recall@K={:.6} MRR={:.6}",
        report.canonical_vector_qualified_metrics.hits,
        report.canonical_vector_qualified_metrics.denominator,
        report.canonical_vector_qualified_metrics.recall_at_k,
        report.canonical_vector_qualified_metrics.mrr
    )
    .expect("writing to String cannot fail");
    if report.expected_id_lane.requested > 0 {
        writeln!(
            output,
            "Reviewed Expected-ID Exact Recall: {}/{} Recall@K={:.6} MRR={:.6}",
            report.expected_id_lane.exact_metrics.hits,
            report.expected_id_lane.exact_metrics.denominator,
            report.expected_id_lane.exact_metrics.recall_at_k,
            report.expected_id_lane.exact_metrics.mrr
        )
        .expect("writing to String cannot fail");
        writeln!(
            output,
            "Reviewed Expected-ID Canonical Fact/Lineage Recall: {}/{} Recall@K={:.6} MRR={:.6}",
            report.expected_id_lane.canonical_fact_metrics.hits,
            report.expected_id_lane.canonical_fact_metrics.denominator,
            report.expected_id_lane.canonical_fact_metrics.recall_at_k,
            report.expected_id_lane.canonical_fact_metrics.mrr
        )
        .expect("writing to String cannot fail");
        writeln!(
            output,
            "Reviewed Expected-ID Canonical Vector-Qualified Recall: {}/{} Recall@K={:.6} MRR={:.6}",
            report.expected_id_lane.canonical_vector_qualified_metrics.hits,
            report.expected_id_lane.canonical_vector_qualified_metrics.denominator,
            report.expected_id_lane.canonical_vector_qualified_metrics.recall_at_k,
            report.expected_id_lane.canonical_vector_qualified_metrics.mrr
        )
        .expect("writing to String cannot fail");
    }

    for target in report
        .targets
        .iter()
        .filter(|target| target.outcome != RecallCoverageOutcome::Surfaced)
    {
        writeln!(
            output,
            "miss id={} exact_outcome={:?} exact_rank={:?} exact_legs=[{}] canonical_outcome={:?} canonical_vector_qualified_outcome={:?} canonical_id={:?} matched_canonical_id={:?} canonical_rank={:?} canonical_legs=[{}] evidence_kind={:?} evidence_source={}",
            target.id,
            target.outcome,
            target.rank,
            format_candidate_legs(target.exact_candidate_legs),
            target.canonical_fact_outcome,
            target.canonical_vector_qualified_outcome,
            target.canonical_id,
            target.matched_canonical_id,
            target.canonical_rank,
            format_candidate_legs(target.canonical_candidate_legs),
            target.fact_evidence.kind,
            target.fact_evidence.source,
        )
        .expect("writing to String cannot fail");
    }
    for target in &report.expected_id_lane.cases {
        writeln!(
            output,
            "expected_id_case id={} exact_outcome={:?} exact_rank={:?} exact_legs=[{}] canonical_outcome={:?} canonical_vector_qualified_outcome={:?} canonical_id={:?} matched_canonical_id={:?} canonical_rank={:?} canonical_legs=[{}] evidence_kind={:?} evidence_source={}",
            target.id,
            target.outcome,
            target.rank,
            format_candidate_legs(target.exact_candidate_legs),
            target.canonical_fact_outcome,
            target.canonical_vector_qualified_outcome,
            target.canonical_id,
            target.matched_canonical_id,
            target.canonical_rank,
            format_candidate_legs(target.canonical_candidate_legs),
            target.fact_evidence.kind,
            target.fact_evidence.source,
        )
        .expect("writing to String cannot fail");
    }
    output
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

    #[test]
    fn reviewed_equivalence_requires_versioned_content_free_evidence() {
        let wrong_version = RecallCoverageEquivalenceCorpus {
            schema_version: "unreviewed".to_string(),
            expected_ids: Vec::new(),
            equivalences: Vec::new(),
        };
        assert!(wrong_version
            .validate()
            .expect_err("wrong schema must fail")
            .to_string()
            .contains("schema_version"));

        let content_shaped_source = [RecallCoverageEquivalenceSet {
            canonical_id: "canonical".to_string(),
            equivalent_ids: vec!["expected".to_string()],
            evidence_source: "this looks like memory content".to_string(),
        }];
        assert!(validate_reviewed_equivalences(&content_shaped_source)
            .expect_err("free-text evidence source must fail")
            .to_string()
            .contains("content-free reference identifier"));
    }
}
