use rusqlite::{params, Connection, OptionalExtension, Transaction};
use serde::Deserialize;
use std::collections::HashSet;

use crate::error::MemoryError;
use crate::relation_ontology::ComponentGovernanceRelation;
use crate::types::{ExpectedMemoryState, GraphExpandResult, GraphTraversalInjection, MemoryEdge};

use super::common::{normalize_utc_iso_or_now, now_utc_iso};
use super::memory_crud::{fetch_by_ids, fetch_by_ids_excluding_store_internal};

// #1558: MemCore cannot depend on tachi-llm (tachi-llm already depends on
// MemCore -- see `crates/memcore/src/store/enrichment.rs`'s doc comment on
// `EnrichmentInvocationReceipts`), so this shadow struct cannot become a
// shared type; it stays a hand-kept mirror of
// `tachi_llm::PersistedModelInvocationReceiptV1`'s wire shape, validation-
// only as the comment at its one call site below explains. `content_hash`
// / `memory_id` / `revision` are the #1558 binding fields: `#[serde(default)]`
// (not just `Option<T>`) is required on all three because pre-#1558
// receipts omit the keys entirely -- `PersistedModelInvocationReceiptV1`
// skips serializing `None` binding fields, so their absence, not merely a
// `null` value, must deserialize cleanly here.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ContradictionModelInvocationReceiptV1 {
    schema: String,
    lane: String,
    engine_kind: String,
    effective_provider: Option<String>,
    effective_model: Option<String>,
    effective_version: Option<String>,
    fallback_chain: Vec<String>,
    degraded: bool,
    completion_status: String,
    prompt_tokens: Option<u64>,
    completion_tokens: Option<u64>,
    total_tokens: Option<u64>,
    latency_ms: Option<u64>,
    #[serde(default)]
    content_hash: Option<String>,
    #[serde(default)]
    memory_id: Option<String>,
    #[serde(default)]
    revision: Option<i64>,
}

/// Append-only provenance context recorded alongside every edge write (#774).
///
/// `memory_edges` is a last-write-wins working projection; the immutable
/// `edge_observations` ledger keeps one row per observation so Layer-2
/// induction can count evidence instead of reading the single collapsed graph
/// row. Every field is best-effort — a caller with no context uses
/// [`EdgeProvenance::default`], which records a well-formed but anonymous
/// observation (`kind`/`actor` normalized to `"unknown"`, empty id/reason, no
/// evidence hash).
#[derive(Debug, Clone, Default)]
pub struct EdgeProvenance {
    /// What kind of capture event produced this observation (an event type /
    /// projector name). Empty is normalized to `"unknown"` on write.
    pub capture_event_kind: String,
    /// Id of the concrete capture event, when one exists (empty otherwise).
    pub capture_event_id: String,
    /// Who/what recorded the observation. Empty is normalized to `"unknown"`.
    pub actor: String,
    /// Why the edge was written (a short machine code); empty otherwise.
    pub reason_code: String,
    /// Optional content hash of the evidence backing this observation.
    pub evidence_hash: Option<String>,
    /// Writer-class stamp (tachi#1646 / #1460 disposition A). `None` — the
    /// [`Default`] value, and what every unclassified caller still gets by
    /// building `EdgeProvenance::default()` and going through the plain
    /// [`add_edge`] / [`add_component_governance_edge`] doors — deliberately
    /// leaves `metadata.authority` unset rather than being promoted to a
    /// fifth "unknown" enum variant a lazy writer could mint to look
    /// classified. See [`edge_authority`] for how reads treat the absence.
    pub authority: Option<EdgeAuthority>,
}

/// Authority classification for a graph edge write (tachi#1646 / #1460
/// disposition A, "the two measured worst offenders"). Every production
/// writer that has been census-reviewed states which tier it belongs to by
/// setting [`EdgeProvenance::authority`] and calling [`add_edge_with_provenance`]
/// / [`add_component_governance_edge_with_provenance`]; scoring does not yet
/// consume this (that is #1646's own explicit non-goal, left to the
/// rank-moving-allowlist follow-up leaf) — this is a recorded classification,
/// not yet an enforcement lever.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EdgeAuthority {
    /// Backed by an actual model-invocation receipt bound to specific
    /// content — the #1524 contradiction pipeline
    /// ([`persist_confirmed_contradiction_within_tx`]) is the only current
    /// producer.
    ModelReceiptBacked,
    /// A heuristic/statistical signal Tachi computed itself (vector/token
    /// similarity, symbolic overlap, keyword-substring match against free
    /// text) — no external assertion and no model receipt behind it.
    DerivedHeuristic,
    /// The relation/weight/endpoints were asserted verbatim by an external
    /// caller (an agent session's event payload, an N-API `edge_json` blob)
    /// — Tachi neither computed nor verified the claim.
    CallerAsserted,
    /// Deterministic bookkeeping the system performs as a side effect of an
    /// already-decided structural transaction (a won supersession claim, a
    /// distillation follows-chain, component-registry seeding) — not an
    /// inference about the world.
    StructuralBookkeeping,
}

impl EdgeAuthority {
    /// The stored `metadata.authority` string for this class.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ModelReceiptBacked => "model_receipt_backed",
            Self::DerivedHeuristic => "derived_heuristic",
            Self::CallerAsserted => "caller_asserted",
            Self::StructuralBookkeeping => "structural_bookkeeping",
        }
    }

    /// Parse a stored `metadata.authority` string back into a variant.
    /// Unrecognized strings — including ones a future build's enum knows
    /// that this one does not — return `None`, the same
    /// "legacy/unclassified" bucket a wholly absent key reads as (see
    /// [`edge_authority`]); this function never guesses.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "model_receipt_backed" => Some(Self::ModelReceiptBacked),
            "derived_heuristic" => Some(Self::DerivedHeuristic),
            "caller_asserted" => Some(Self::CallerAsserted),
            "structural_bookkeeping" => Some(Self::StructuralBookkeeping),
            _ => None,
        }
    }
}

/// Read back a persisted edge's authority classification (tachi#1646).
///
/// `None` deliberately covers two indistinguishable-on-purpose cases: a
/// pre-#1646 row that predates the concept entirely (no schema/migration was
/// added — see [`write_edge_row`]'s doc for why authority lives in
/// `metadata` instead), and a post-#1646 write through a still-unclassified
/// caller. Both honestly mean "no authority claim was made for this edge",
/// never "verified absence of authority" — a caller that needs to
/// distinguish "never classified" from "explicitly legacy" has no way to,
/// by design, since fabricating that distinction from data that was never
/// recorded would be worse than admitting it is unknown.
pub fn edge_authority(edge: &MemoryEdge) -> Option<EdgeAuthority> {
    edge.metadata
        .get("authority")
        .and_then(serde_json::Value::as_str)
        .and_then(EdgeAuthority::parse)
}

/// A row in the append-only `edge_observations` ledger (#774).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EdgeObservation {
    pub observation_id: String,
    pub source_id: String,
    pub target_id: String,
    pub relation: String,
    pub capture_event_kind: String,
    pub capture_event_id: String,
    pub actor: String,
    pub reason_code: String,
    pub observed_at: String,
    pub evidence_hash: Option<String>,
    pub invalidated_at: Option<String>,
}

/// Generic edge write. The relation must be admissible on the **generic**
/// ontology-v1 path — this is the single choke point every dynamic string
/// caller (continuity projection, the NAPI `add_edge` surface, tools) funnels
/// through, so the #772 grandfathered relations are rejected here. Callers
/// that legitimately seed a grandfathered relation must use
/// [`add_component_governance_edge`].
///
/// Records an anonymous ([`EdgeProvenance::default`]) observation; callers with
/// provenance context should use [`add_edge_with_provenance`].
pub fn add_edge(conn: &Connection, edge: &MemoryEdge) -> Result<(), MemoryError> {
    add_edge_with_provenance(conn, edge, &EdgeProvenance::default())
}

/// [`add_edge`] plus explicit provenance for the appended observation.
pub fn add_edge_with_provenance(
    conn: &Connection,
    edge: &MemoryEdge,
    provenance: &EdgeProvenance,
) -> Result<(), MemoryError> {
    crate::relation_ontology::validate_relation_for_write(&edge.relation)?;
    write_edge_row(conn, edge, &edge.relation, provenance)
}

/// Disposition of one confirmed-contradiction commit attempt.
///
/// `StaleSkipped` is deliberately **not** an error. The contradiction pipeline
/// reads a candidate, hands it to a model, and only then opens the write
/// transaction; if the candidate changed in between, the verdict describes
/// content that is no longer stored and the only correct action is to drop it.
/// That is an ordinary outcome of racing with concurrent writers, so callers
/// count it and continue rather than failing the batch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfirmedContradictionOutcome {
    /// Both graph projections and the lifecycle transition are durable.
    Committed,
    /// The candidate no longer matches the state the model judged; nothing was
    /// written.
    StaleSkipped,
}

/// Re-read a row inside the writer's transaction and compare it to the state
/// the caller froze before the model was consulted.
///
/// Used for both sides of a confirmed contradiction: the candidate (the older
/// fact being judged) and, since tachi#1563, the entry (the newer fact that
/// triggered detection) can each be rewritten or archived during the LLM
/// round-trip, which happens outside any transaction.
///
/// The comparison itself is [`ExpectedMemoryState::matches`] — the same
/// field-by-field comparator the tachi#1551 migration writes go through — so a
/// contradiction commit and a migration commit can never disagree about what
/// "unchanged" means. Only the row load is restated here: the equivalent loader
/// in `db::memory_crud::update` is private to that module.
///
/// A missing row reads as a mismatch, which is the fail-closed direction: a
/// verdict about a row that no longer exists must not write edges. An
/// archived row is likewise a mismatch whenever the snapshot was taken before
/// archival, because `archived` is one of the compared fields.
fn row_matches_expected_state(
    tx: &Transaction<'_>,
    row_id: &str,
    expected: &ExpectedMemoryState,
) -> Result<bool, MemoryError> {
    let ids = vec![row_id.to_string()];
    let mut current = fetch_by_ids(tx, &ids, true)?;
    let Some(current) = current.remove(row_id) else {
        return Ok(false);
    };
    let superseded_by = tx
        .query_row(
            "SELECT superseded_by FROM memories WHERE id = ?1",
            [row_id],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()?
        .flatten();
    Ok(expected.matches(&current, superseded_by.as_deref()))
}

/// Persist one model-confirmed contradiction inside the caller's transaction.
///
/// This is deliberately narrower than a generic transaction surface: the two
/// graph projections must describe the same directed pair, carry the same
/// model-invocation provenance, and use the fixed `contradicts`/`supersedes`
/// relations. The lifecycle update must win its unsuperseded-row CAS or the
/// caller rolls the whole transaction back.
///
/// `expected_candidate` binds the write to the exact candidate state the model
/// judged (tachi#1551 `ExpectedMemoryState`). `superseded_by IS NULL` alone
/// cannot carry that binding, and neither can `revision`: enrichment rewrites
/// `summary`, `keywords`, and `metadata` without bumping it, so a candidate can
/// be rewritten in place during the model round-trip. `expected_entry` binds
/// the same write to the entry (the newer fact) that triggered detection —
/// the entry can equally be rewritten or archived while the model call is in
/// flight, and nothing upstream of this function re-reads it (tachi#1563).
/// Both snapshots are verified after `BEGIN IMMEDIATE` and before any edge
/// write, so a stale verdict about either side leaves the database untouched.
pub(crate) fn persist_confirmed_contradiction_within_tx(
    tx: &Transaction<'_>,
    contradicts_edge: &MemoryEdge,
    supersedes_edge: &MemoryEdge,
    superseded_at: &str,
    expected_entry: &ExpectedMemoryState,
    expected_candidate: &ExpectedMemoryState,
) -> Result<ConfirmedContradictionOutcome, MemoryError> {
    validate_confirmed_contradiction(contradicts_edge, supersedes_edge, superseded_at)?;

    if !row_matches_expected_state(tx, &contradicts_edge.source_id, expected_entry)? {
        // A batch shares one `expected_entry` snapshot across every candidate
        // (`apply_auto_contradiction_detection` freezes it once), so once the
        // entry drifts, this line fires once per remaining candidate — the
        // candidate id below is what turns those repeats into a distinguishable
        // per-candidate diagnostic instead of duplicate noise.
        eprintln!(
            "[confirmed-contradiction] entry-side snapshot mismatch for {} (candidate {}): \
             the triggering memory changed (or was archived/deleted) between the read \
             that fed the model and the write",
            contradicts_edge.source_id, contradicts_edge.target_id
        );
        return Ok(ConfirmedContradictionOutcome::StaleSkipped);
    }
    if !row_matches_expected_state(tx, &contradicts_edge.target_id, expected_candidate)? {
        return Ok(ConfirmedContradictionOutcome::StaleSkipped);
    }

    // tachi#1646: the only writer census-classified `ModelReceiptBacked` —
    // this function's whole contract (validated above) is that both edges
    // carry a bound, schema-checked `provenance.model_invocation` receipt.
    let receipt_provenance = EdgeProvenance {
        authority: Some(EdgeAuthority::ModelReceiptBacked),
        ..EdgeProvenance::default()
    };
    write_edge_row(tx, contradicts_edge, "contradicts", &receipt_provenance)?;
    write_edge_row(tx, supersedes_edge, "supersedes", &receipt_provenance)?;

    let superseded_at = super::normalize_utc_iso(superseded_at)?;
    // `revision` advances here (unlike `mark_superseded_closing_validity`,
    // which intentionally does not) so revision-scoped observers — the
    // `update_enrichment_fields` CAS chief among them — see the lifecycle
    // transition: an enrichment write in flight against the pre-supersession
    // revision must lose its CAS rather than land on a row that has already
    // been superseded (tachi#1563).
    let affected = tx.execute(
        "UPDATE memories SET superseded_by = ?1, updated_at = ?2, \
         valid_until = COALESCE(valid_until, ?2), revision = revision + 1 \
         WHERE id = ?3 AND superseded_by IS NULL",
        params![
            contradicts_edge.source_id,
            superseded_at,
            contradicts_edge.target_id
        ],
    )?;
    if affected != 1 {
        return Err(MemoryError::InvalidArg(format!(
            "confirmed contradiction lifecycle CAS refused for {} -> {}",
            contradicts_edge.target_id, contradicts_edge.source_id
        )));
    }
    Ok(ConfirmedContradictionOutcome::Committed)
}

fn validate_confirmed_contradiction(
    contradicts_edge: &MemoryEdge,
    supersedes_edge: &MemoryEdge,
    superseded_at: &str,
) -> Result<(), MemoryError> {
    if contradicts_edge.relation != "contradicts" || supersedes_edge.relation != "supersedes" {
        return Err(MemoryError::InvalidArg(
            "confirmed contradiction requires contradicts and supersedes edges".to_string(),
        ));
    }
    crate::relation_ontology::validate_relation_for_write(&contradicts_edge.relation)?;
    crate::relation_ontology::validate_relation_for_write(&supersedes_edge.relation)?;

    let source_id = contradicts_edge.source_id.trim();
    let target_id = contradicts_edge.target_id.trim();
    if source_id.is_empty() || target_id.is_empty() || source_id == target_id {
        return Err(MemoryError::InvalidArg(
            "confirmed contradiction endpoints must be non-empty and distinct".to_string(),
        ));
    }
    if supersedes_edge.source_id != contradicts_edge.source_id
        || supersedes_edge.target_id != contradicts_edge.target_id
    {
        return Err(MemoryError::InvalidArg(
            "confirmed contradiction edges must share identical endpoints".to_string(),
        ));
    }
    if !contradicts_edge.weight.is_finite()
        || !(0.0..=1.0).contains(&contradicts_edge.weight)
        || supersedes_edge.weight != contradicts_edge.weight
    {
        return Err(MemoryError::InvalidArg(
            "confirmed contradiction edges require one finite confidence in [0,1]".to_string(),
        ));
    }
    if contradicts_edge.metadata != supersedes_edge.metadata {
        return Err(MemoryError::InvalidArg(
            "confirmed contradiction edges must carry identical metadata".to_string(),
        ));
    }
    if contradicts_edge.metadata.get("auto_contradiction") != Some(&serde_json::Value::Bool(true))
        || contradicts_edge.metadata.get("llm_verified") != Some(&serde_json::Value::Bool(true))
    {
        return Err(MemoryError::InvalidArg(
            "confirmed contradiction metadata must identify an LLM-verified auto contradiction"
                .to_string(),
        ));
    }

    let receipt_value = contradicts_edge
        .metadata
        .pointer("/provenance/model_invocation")
        .ok_or_else(|| {
            MemoryError::InvalidArg(
                "confirmed contradiction metadata requires provenance.model_invocation".to_string(),
            )
        })?;
    // This is validation-only: the transaction persists the caller's original
    // metadata after this function returns. The typed shadow rejects unknown
    // or unsafe receipt shapes, while canonical serialization remains the
    // producer boundary's responsibility.
    let receipt: ContradictionModelInvocationReceiptV1 =
        serde_json::from_value(receipt_value.clone()).map_err(|error| {
            MemoryError::InvalidArg(format!(
                "confirmed contradiction model-invocation receipt is malformed: {error}"
            ))
        })?;
    if receipt.schema != "model-invocation-v1"
        || receipt.lane != "extract"
        || receipt.engine_kind != "provider_http"
    {
        return Err(MemoryError::InvalidArg(
            "confirmed contradiction requires an extract-lane model-invocation-v1 receipt"
                .to_string(),
        ));
    }
    if !matches!(receipt.completion_status.as_str(), "complete" | "unknown") {
        return Err(MemoryError::InvalidArg(
            "confirmed contradiction receipt must not be truncated or malformed".to_string(),
        ));
    }
    if receipt.fallback_chain.len() > 4
        || receipt.fallback_chain.iter().any(|step| {
            !matches!(
                step.as_str(),
                "provider_http_fallback" | "claude_cli_to_provider_http"
            )
        })
        || [
            &receipt.effective_provider,
            &receipt.effective_model,
            &receipt.effective_version,
        ]
        .into_iter()
        .flatten()
        .any(|value| value.trim().is_empty())
    {
        return Err(MemoryError::InvalidArg(
            "confirmed contradiction receipt contains invalid persisted provenance".to_string(),
        ));
    }
    // #1558: the binding fields are optional (a receipt minted before #1558,
    // or one whose producer has not wired binding yet, carries none of
    // them) but must not be blank/negative garbage when present. This does
    // NOT check the binding against the edge's actual target row content --
    // this function has no row content in view, only the caller-supplied
    // metadata blob -- so a present-but-wrong binding is not caught here;
    // that check belongs to `PersistedModelInvocationReceiptV1::binding_matches`
    // at the write site that has the real content in hand.
    if [&receipt.content_hash, &receipt.memory_id]
        .into_iter()
        .flatten()
        .any(|value| value.trim().is_empty())
        || receipt.revision.is_some_and(|revision| revision < 0)
    {
        return Err(MemoryError::InvalidArg(
            "confirmed contradiction receipt has a malformed content binding".to_string(),
        ));
    }
    let _typed_receipt_metrics = (
        receipt.degraded,
        receipt.prompt_tokens,
        receipt.completion_tokens,
        receipt.total_tokens,
        receipt.latency_ms,
    );

    let superseded_at = super::normalize_utc_iso(superseded_at)?;
    if super::normalize_utc_iso(&contradicts_edge.created_at)? != superseded_at
        || super::normalize_utc_iso(&supersedes_edge.created_at)? != superseded_at
    {
        return Err(MemoryError::InvalidArg(
            "confirmed contradiction edges and lifecycle transition must share one timestamp"
                .to_string(),
        ));
    }
    Ok(())
}

/// Typed, caller-scoped write door for the #772 component-governance
/// grandfathered relations (`owns` / `consumes` / `backflow_candidate` /
/// `blocked_by`). The `relation` argument is the closed
/// [`ComponentGovernanceRelation`] enum, not a string, so these four relations
/// can only enter the graph through this single call — the generic [`add_edge`]
/// rejects them. The enum is authoritative for the stored `relation` column;
/// `edge.relation` is ignored. Bypassing the generic ontology check here is
/// deliberate: the enum type *is* the validation.
///
/// Records an anonymous ([`EdgeProvenance::default`]) observation; callers with
/// provenance context should use [`add_component_governance_edge_with_provenance`].
pub fn add_component_governance_edge(
    conn: &Connection,
    edge: &MemoryEdge,
    relation: ComponentGovernanceRelation,
) -> Result<(), MemoryError> {
    add_component_governance_edge_with_provenance(conn, edge, relation, &EdgeProvenance::default())
}

/// [`add_component_governance_edge`] plus explicit provenance for the appended
/// observation.
pub fn add_component_governance_edge_with_provenance(
    conn: &Connection,
    edge: &MemoryEdge,
    relation: ComponentGovernanceRelation,
    provenance: &EdgeProvenance,
) -> Result<(), MemoryError> {
    write_edge_row(conn, edge, relation.as_str(), provenance)
}

/// Normalize an incoming edge weight into the `[0.0, 1.0]` band the graph
/// scorer already assumes (#1460).
///
/// Non-finite input (`NaN`, `±inf`) is **malformed, not merely large**: it
/// collapses to `0.0` rather than saturating at the upper bound. `+inf` is
/// therefore stored as zero influence, not as maximum influence — this gate
/// exists because untrusted writers reach this field, so the malformed case
/// fails closed. It is also the rule the shipped seed-weight path already
/// uses (`scorer/graph.rs:99`), and one subsystem must not carry two
/// answers for the same malformed value.
///
/// The finiteness check must also come *before* the clamp: `f64::clamp`
/// propagates `NaN` rather than pinning it to a bound, so a bare `clamp`
/// would let `NaN` reach the column and, from there,
/// `graph_spreading_activation_with_seed_weights`' edge term
/// (`scorer/graph.rs`), which multiplies `edge.weight` without a finiteness
/// guard of its own and whose `propagated <= 0.0` skip is false for `NaN`.
///
/// This is a write-time gate only — it does not retire the read-time clamp.
/// Legacy rows predate any clamp, and the raw-SQL edge writers listed on
/// [`write_edge_row`] never pass through here, so reads still cannot assume a
/// stored weight was governed.
fn clamp_edge_weight(weight: f64) -> f64 {
    if weight.is_finite() {
        weight.clamp(0.0, 1.0)
    } else {
        0.0
    }
}

/// Merge the writer's authority classification (tachi#1646) into a clone of
/// the edge's metadata.
///
/// `authority == None` **scrubs** any `"authority"` key already present in
/// `metadata` before returning the clone — it does not pass `metadata`
/// through untouched. `metadata.authority` is a reserved key: `edge_authority`
/// (above) trusts whatever string sits there, so if an unclassified caller's
/// `add_edge` handed a pre-baked `{"authority":"model_receipt_backed"}`
/// straight through, that string would round-trip as a trusted
/// classification no writer ever actually made — authority spoofing via the
/// unclassified door (tachi#1646 round-2 MUST-FIX 1). An unclassified write
/// carries *no* authority claim, full stop, so the key must be **absent**,
/// never merely "whatever the caller happened to put there". Legacy rows
/// (pre-#1646, no migration) and post-#1646 unclassified rows both read back
/// `None` from `edge_authority` because the key is absent — never because we
/// trusted a caller-supplied string. This is a behavior change from the
/// pre-round-2 "clone only" version: metadata is no longer guaranteed
/// byte-for-byte identical when the caller's own payload happened to contain
/// the reserved key, but it *is* guaranteed byte-for-byte identical for every
/// caller that never touches `metadata.authority`, which is every legitimate
/// caller of the plain (unclassified) doors.
///
/// When `authority == Some(_)`, the reserved key is **overwritten**, not
/// merged with whatever the caller supplied — `insert` on an existing key
/// replaces its value, so a caller-asserted `metadata.authority` cannot
/// survive a writer that explicitly classifies the edge either.
///
/// A non-object `metadata` (e.g. `component_governance_ops` seeds
/// `Value::Null`) is replaced with a fresh object when `authority` is
/// `Some`, since a caller that explicitly asked for a classification must
/// get one; when `authority` is `None` a non-object `metadata` has no
/// `"authority"` key to scrub in the first place and is returned unchanged.
fn stamp_authority(
    metadata: &serde_json::Value,
    authority: Option<EdgeAuthority>,
) -> serde_json::Value {
    let Some(authority) = authority else {
        let Some(obj) = metadata.as_object() else {
            return metadata.clone();
        };
        if !obj.contains_key("authority") {
            return metadata.clone();
        }
        let mut scrubbed = obj.clone();
        scrubbed.remove("authority");
        return serde_json::Value::Object(scrubbed);
    };
    let mut stamped = if metadata.is_object() {
        metadata.clone()
    } else {
        serde_json::Value::Object(serde_json::Map::new())
    };
    if let Some(obj) = stamped.as_object_mut() {
        obj.insert(
            "authority".to_string(),
            serde_json::Value::String(authority.as_str().to_string()),
        );
    }
    stamped
}

/// Shared INSERT/UPSERT for the edge write doors above. `relation` is the
/// (already-validated) relation string to persist; all timestamp normalization
/// is identical across every entry point, and the weight is clamped into
/// `[0.0, 1.0]` by [`clamp_edge_weight`] (#1460) so semi-trusted callers (e.g.
/// the continuity timeline projection, which lifts `weight` straight out of an
/// event payload) cannot seed an out-of-band activation multiplier.
///
/// This is the choke point for *typed* edge writes, not for every statement
/// that touches the table. Known raw-SQL writers that do **not** funnel
/// through here (verified 2026-07-26): `store::exact_dedupe`'s
/// `transfer_edges_to_winner` (re-keys existing rows, carrying their stored
/// weight verbatim), `tachi-server`'s `repair::memory_hygiene` R12 rules and
/// `repair::plan_c`'s `copy_common_rows` alias merge. None of them mints a new
/// weight from caller input, but they are why the read-side clamp stays.
///
/// Invariant (#774): the `memory_edges` upsert (mutable working projection) and
/// the append-only `edge_observations` row land together — either both persist
/// or neither. A `SAVEPOINT` nests cleanly whether or not the caller already
/// holds a transaction (see `migrate_v9_relocate_and_drop_location`), unlike a
/// raw `BEGIN`; when there is no enclosing transaction the savepoint starts one
/// and `RELEASE` commits it, so the two writes are always atomic.
///
/// `provenance.authority` (tachi#1646) is stamped into `metadata.authority`,
/// not a new column — this follows the #1524 contradiction-receipt precedent
/// (`metadata.provenance.model_invocation`, validated in
/// [`validate_confirmed_contradiction`] above), which landed the *previous*
/// edge-provenance addition through the same JSON channel without a schema
/// migration. `memory_edges.metadata` is an unindexed free-form JSON blob
/// already read generically by every edge consumer, so a new key is
/// additive and legacy rows with no `metadata.authority` key simply read
/// back `None` from [`edge_authority`] — no `CHECK` constraint, no `NOT
/// NULL`, no backfill. A `None` authority (every caller that still builds
/// `EdgeProvenance::default()`) leaves `metadata` byte-for-byte unstamped
/// *unless* the caller's own payload already carried an `"authority"` key —
/// [`stamp_authority`] scrubs that reserved key on the unclassified path so a
/// plain `add_edge` cannot be used to spoof a trusted classification no
/// writer actually made (tachi#1646 round-2 MUST-FIX 1).
fn write_edge_row(
    conn: &Connection,
    edge: &MemoryEdge,
    relation: &str,
    provenance: &EdgeProvenance,
) -> Result<(), MemoryError> {
    let created = if edge.created_at.is_empty() {
        now_utc_iso()
    } else {
        normalize_utc_iso_or_now(&edge.created_at)
    };
    let valid_from = if edge.valid_from.is_empty() {
        created.clone()
    } else {
        normalize_utc_iso_or_now(&edge.valid_from)
    };
    // Freeze normalized UTC half-open interval [valid_from, valid_to) semantics:
    // valid_to must be normalized to the same RFC3339-with-millis format as
    // valid_from/created_at so it stays comparable with the read-side's
    // format-agnostic datetime() comparison (see get_edges / get_edges_batch /
    // get_contradiction_count). Storing it raw let same-day RFC3339 values
    // remain lexically "active" forever against SQLite's differently
    // formatted datetime('now') text (#773 Sol correction 4).
    let valid_to = edge
        .valid_to
        .as_deref()
        .filter(|s| !s.is_empty())
        .map(normalize_utc_iso_or_now);
    let stamped_metadata = stamp_authority(&edge.metadata, provenance.authority);
    let meta_str = serde_json::to_string(&stamped_metadata).unwrap_or_else(|_| "{}".to_string());
    let weight = clamp_edge_weight(edge.weight);

    conn.execute_batch("SAVEPOINT write_edge_row")?;
    let result = (|| -> Result<(), MemoryError> {
        conn.execute(
            r#"INSERT INTO memory_edges (source_id, target_id, relation, weight, metadata, created_at, valid_from, valid_to)
               VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
               ON CONFLICT(source_id, target_id, relation)
               DO UPDATE SET weight = ?4, metadata = ?5, created_at = ?6, valid_from = ?7, valid_to = ?8"#,
            params![edge.source_id, edge.target_id, relation, weight, meta_str, created, valid_from, valid_to],
        )?;
        append_edge_observation(conn, edge, relation, provenance)?;
        Ok(())
    })();
    match result {
        Ok(()) => {
            conn.execute_batch("RELEASE write_edge_row")?;
            Ok(())
        }
        Err(e) => {
            let _ = conn.execute_batch("ROLLBACK TO write_edge_row");
            let _ = conn.execute_batch("RELEASE write_edge_row");
            Err(e)
        }
    }
}

/// Normalize an empty/blank provenance text field to `"unknown"` so the ledger
/// never stores a blank in a slot the DDL defaults to `"unknown"`.
fn or_unknown(value: &str) -> &str {
    if value.trim().is_empty() {
        "unknown"
    } else {
        value
    }
}

/// Append exactly one immutable observation row for an edge write (#774).
/// Called only from inside `write_edge_row`'s savepoint, so it shares the edge
/// upsert's atomicity.
fn append_edge_observation(
    conn: &Connection,
    edge: &MemoryEdge,
    relation: &str,
    provenance: &EdgeProvenance,
) -> Result<(), MemoryError> {
    let observation_id = uuid::Uuid::new_v4().to_string();
    let observed_at = now_utc_iso();
    conn.execute(
        r#"INSERT INTO edge_observations
           (observation_id, source_id, target_id, relation, capture_event_kind,
            capture_event_id, actor, reason_code, observed_at, evidence_hash, invalidated_at)
           VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, NULL)"#,
        params![
            observation_id,
            edge.source_id,
            edge.target_id,
            relation,
            or_unknown(&provenance.capture_event_kind),
            provenance.capture_event_id,
            or_unknown(&provenance.actor),
            provenance.reason_code,
            observed_at,
            provenance.evidence_hash,
        ],
    )?;
    Ok(())
}

/// Map a full `edge_observations` row to [`EdgeObservation`].
fn row_to_observation(row: &rusqlite::Row) -> rusqlite::Result<EdgeObservation> {
    Ok(EdgeObservation {
        observation_id: row.get(0)?,
        source_id: row.get(1)?,
        target_id: row.get(2)?,
        relation: row.get(3)?,
        capture_event_kind: row.get(4)?,
        capture_event_id: row.get(5)?,
        actor: row.get(6)?,
        reason_code: row.get(7)?,
        observed_at: row.get(8)?,
        evidence_hash: row.get(9)?,
        invalidated_at: row.get(10)?,
    })
}

/// All observations for one `(source, target, relation)` edge, oldest first
/// (`observed_at` ascending, `observation_id` as a stable tie-break). Includes
/// invalidated rows — history is never removed from this read.
pub fn list_observations_for_edge(
    conn: &Connection,
    source_id: &str,
    target_id: &str,
    relation: &str,
) -> Result<Vec<EdgeObservation>, MemoryError> {
    let mut stmt = conn.prepare(
        "SELECT observation_id, source_id, target_id, relation, capture_event_kind, \
         capture_event_id, actor, reason_code, observed_at, evidence_hash, invalidated_at \
         FROM edge_observations \
         WHERE source_id = ?1 AND target_id = ?2 AND relation = ?3 \
         ORDER BY observed_at ASC, observation_id ASC",
    )?;
    let rows = stmt.query_map(params![source_id, target_id, relation], row_to_observation)?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

/// Count the still-active (not invalidated) observations for one edge — this is
/// the evidence count Layer-2 induction reads instead of the single collapsed
/// `memory_edges` row (#774).
pub fn count_active_observations(
    conn: &Connection,
    source_id: &str,
    target_id: &str,
    relation: &str,
) -> Result<u32, MemoryError> {
    let count: u32 = conn.query_row(
        "SELECT COUNT(*) FROM edge_observations \
         WHERE source_id = ?1 AND target_id = ?2 AND relation = ?3 AND invalidated_at IS NULL",
        params![source_id, target_id, relation],
        |row| row.get(0),
    )?;
    Ok(count)
}

/// THE single write path that invalidates an observation (#774): soft-stamp
/// `invalidated_at` on a still-active row. History is never deleted — the row
/// stays in the ledger and in [`list_observations_for_edge`], it just stops
/// counting toward [`count_active_observations`]. Idempotent: an already-
/// invalidated (or missing) observation is left untouched. Returns whether a
/// row transitioned active -> invalidated on this call. Mirrors the
/// single-writer discipline of `reclaim_exec_env`.
pub fn invalidate_observation(
    conn: &Connection,
    observation_id: &str,
    at: &str,
) -> Result<bool, MemoryError> {
    let stamped = normalize_utc_iso_or_now(at);
    let changed = conn.execute(
        "UPDATE edge_observations SET invalidated_at = ?2 \
         WHERE observation_id = ?1 AND invalidated_at IS NULL",
        params![observation_id, stamped],
    )?;
    Ok(changed > 0)
}

/// Close `valid_to` on every still-open `related_to` edge (tachi#773 item 3:
/// legacy fog retirement). Idempotent: only rows with `valid_to IS NULL` are
/// touched, so re-running after a first pass (or a crash mid-pass) closes
/// exactly the rows that are still open and no others — safe to call from a
/// maintenance sweep on every tick.
///
/// `related_to` rows are not deleted (they stay for audit / historical
/// reads), only closed so `get_edges` / `graph_expand`'s
/// `valid_to IS NULL OR datetime(valid_to) > datetime('now')` filter excludes
/// them from traversal from this point forward.
///
/// Uses the same normalized-UTC-millis format `add_edge` writes for
/// `created_at`/`valid_from` (`now_utc_iso`, RFC3339 with millis, matching
/// PR #1013's fix/773-edge-valid-to-normalization convention so a single
/// comparison format is used everywhere valid_to is read).
///
/// Returns the number of rows closed by this call (0 on a fully-idempotent
/// re-run once the fog is already closed).
pub fn close_related_to_fog(conn: &Connection) -> Result<usize, MemoryError> {
    let now = now_utc_iso();
    let closed = conn.execute(
        "UPDATE memory_edges SET valid_to = ?1 WHERE relation = 'related_to' AND valid_to IS NULL",
        params![now],
    )?;
    Ok(closed)
}

/// Remove an edge from the memory graph.
pub fn remove_edge(
    conn: &Connection,
    source_id: &str,
    target_id: &str,
    relation: &str,
) -> Result<bool, MemoryError> {
    let count = conn.execute(
        "DELETE FROM memory_edges WHERE source_id = ?1 AND target_id = ?2 AND relation = ?3",
        params![source_id, target_id, relation],
    )?;
    Ok(count > 0)
}

/// Get all edges connected to a memory ID.
/// direction: "outgoing" (source_id match), "incoming" (target_id match), or "both"
pub fn get_edges(
    conn: &Connection,
    memory_id: &str,
    direction: &str,
    relation_filter: Option<&str>,
) -> Result<Vec<MemoryEdge>, MemoryError> {
    get_edges_limited(conn, memory_id, direction, relation_filter, usize::MAX)
}

/// Get edges connected to a memory ID with the row ceiling applied by SQLite.
pub fn get_edges_limited(
    conn: &Connection,
    memory_id: &str,
    direction: &str,
    relation_filter: Option<&str>,
    limit: usize,
) -> Result<Vec<MemoryEdge>, MemoryError> {
    let base_sql = match direction {
        "incoming" =>
            "SELECT source_id, target_id, relation, weight, metadata, created_at, valid_from, valid_to FROM memory_edges WHERE target_id = ?1
             AND (valid_to IS NULL OR datetime(valid_to) > datetime('now'))",
        "outgoing" =>
            "SELECT source_id, target_id, relation, weight, metadata, created_at, valid_from, valid_to FROM memory_edges WHERE source_id = ?1
             AND (valid_to IS NULL OR datetime(valid_to) > datetime('now'))",
        _ =>
            "SELECT source_id, target_id, relation, weight, metadata, created_at, valid_from, valid_to FROM memory_edges WHERE (source_id = ?1 OR target_id = ?1)
             AND (valid_to IS NULL OR datetime(valid_to) > datetime('now'))",
    };

    // Use parameterized query for relation_filter to prevent SQL injection
    let full_sql = if relation_filter.is_some() {
        format!(
            "{} AND relation = ?2 ORDER BY source_id ASC, target_id ASC, relation ASC LIMIT ?3",
            base_sql
        )
    } else {
        format!(
            "{} ORDER BY source_id ASC, target_id ASC, relation ASC LIMIT ?2",
            base_sql
        )
    };

    let mut stmt = conn.prepare(&full_sql)?;
    let row_mapper = |row: &rusqlite::Row| {
        let meta_str: String = row.get(4)?;
        let metadata = serde_json::from_str(&meta_str).unwrap_or_default();
        Ok(MemoryEdge {
            source_id: row.get(0)?,
            target_id: row.get(1)?,
            relation: row.get(2)?,
            weight: row.get(3)?,
            metadata,
            created_at: row.get(5)?,
            valid_from: row.get(6)?,
            valid_to: row.get(7)?,
        })
    };

    let mut edges = Vec::new();
    let sql_limit = i64::try_from(limit).unwrap_or(i64::MAX);
    if let Some(rel) = relation_filter {
        for row in stmt.query_map(params![memory_id, rel, sql_limit], row_mapper)? {
            edges.push(row?);
        }
    } else {
        for row in stmt.query_map(params![memory_id, sql_limit], row_mapper)? {
            edges.push(row?);
        }
    }

    Ok(edges)
}

fn get_edges_batch(
    conn: &Connection,
    ids: &[String],
    relation_filter: Option<&str>,
    limit: usize,
) -> Result<Vec<MemoryEdge>, MemoryError> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }

    let placeholders = (0..ids.len()).map(|_| "?").collect::<Vec<_>>().join(", ");

    let base_sql = format!(
        "SELECT source_id, target_id, relation, weight, metadata, created_at, valid_from, valid_to \
         FROM memory_edges \
         WHERE (source_id IN ({ph}) OR target_id IN ({ph})) \
         AND (valid_to IS NULL OR datetime(valid_to) > datetime('now'))",
        ph = placeholders
    );

    let full_sql = if relation_filter.is_some() {
        format!(
            "{base_sql} AND relation = ? \
             ORDER BY source_id ASC, target_id ASC, relation ASC LIMIT ?"
        )
    } else {
        format!("{base_sql} ORDER BY source_id ASC, target_id ASC, relation ASC LIMIT ?")
    };

    let mut stmt = conn.prepare(&full_sql)?;
    let row_mapper = |row: &rusqlite::Row| {
        let meta_str: String = row.get(4)?;
        let metadata = serde_json::from_str(&meta_str).unwrap_or_default();
        Ok(MemoryEdge {
            source_id: row.get(0)?,
            target_id: row.get(1)?,
            relation: row.get(2)?,
            weight: row.get(3)?,
            metadata,
            created_at: row.get(5)?,
            valid_from: row.get(6)?,
            valid_to: row.get(7)?,
        })
    };

    let mut param_values: Vec<Box<dyn rusqlite::types::ToSql>> =
        Vec::with_capacity(ids.len() * 2 + 2);
    for id in ids {
        param_values.push(Box::new(id.clone()));
    }
    for id in ids {
        param_values.push(Box::new(id.clone()));
    }
    if let Some(rel) = relation_filter {
        param_values.push(Box::new(rel.to_string()));
    }
    param_values.push(Box::new(i64::try_from(limit).unwrap_or(i64::MAX)));

    let params_refs: Vec<&dyn rusqlite::types::ToSql> =
        param_values.iter().map(|value| value.as_ref()).collect();
    let rows = stmt.query_map(params_refs.as_slice(), row_mapper)?;
    let mut result = Vec::new();
    for edge in rows {
        result.push(edge?);
    }

    Ok(result)
}

/// Count the number of 'contradicts' edges for a given memory ID.
/// Used by surprise scoring to detect controversial/surprising memories.
pub fn get_contradiction_count(conn: &Connection, memory_id: &str) -> Result<u32, MemoryError> {
    let count: u32 = conn.query_row(
        "SELECT COUNT(*) FROM memory_edges WHERE (source_id = ?1 OR target_id = ?1) AND relation = 'contradicts' AND (valid_to IS NULL OR datetime(valid_to) > datetime('now'))",
        params![memory_id],
        |row| row.get(0),
    )?;
    Ok(count)
}

/// Count how many memories share the same topic as the given entry.
/// Used by surprise scoring for topic novelty.
pub fn count_same_topic(conn: &Connection, topic: &str) -> Result<u32, MemoryError> {
    if topic.is_empty() {
        return Ok(0);
    }
    let count: u32 = conn.query_row(
        "SELECT COUNT(*) FROM memories WHERE topic = ?1 AND archived = 0",
        params![topic],
        |row| row.get(0),
    )?;
    Ok(count)
}

/// Get the average importance across all non-archived memories.
/// Used by surprise scoring.
pub fn avg_importance(conn: &Connection) -> Result<f64, MemoryError> {
    let avg: f64 = conn.query_row(
        "SELECT COALESCE(AVG(importance), 0.7) FROM memories WHERE archived = 0",
        [],
        |row| row.get(0),
    )?;
    Ok(avg)
}

/// Return IDs whose memory row has been superseded by a newer memory.
pub fn get_superseded_ids(
    conn: &Connection,
    ids: &[String],
) -> Result<HashSet<String>, MemoryError> {
    if ids.is_empty() {
        return Ok(HashSet::new());
    }

    let mut out = HashSet::new();
    for batch in ids.chunks(500) {
        let placeholders = (1..=batch.len())
            .map(|i| format!("?{i}"))
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "SELECT id FROM memories WHERE id IN ({}) AND superseded_by IS NOT NULL",
            placeholders
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(batch.iter()), |row| {
            row.get::<_, String>(0)
        })?;
        for row in rows {
            out.insert(row?);
        }
    }
    Ok(out)
}

/// BFS graph expansion from seed memory IDs.
/// Returns entries found within `max_hops` of any seed, plus the edges traversed.
pub fn graph_expand(
    conn: &Connection,
    seed_ids: &[String],
    max_hops: u32,
    relation_filter: Option<&str>,
    wiki_corpus_store: bool,
) -> Result<GraphExpandResult, MemoryError> {
    graph_expand_limited(
        conn,
        seed_ids,
        max_hops,
        relation_filter,
        usize::MAX,
        wiki_corpus_store,
    )
}

/// BFS graph expansion with a global edge ceiling enforced in each SQLite batch.
pub fn graph_expand_limited(
    conn: &Connection,
    seed_ids: &[String],
    max_hops: u32,
    relation_filter: Option<&str>,
    edge_limit: usize,
    wiki_corpus_store: bool,
) -> Result<GraphExpandResult, MemoryError> {
    use std::collections::{HashMap, HashSet, VecDeque};

    let mut visited: HashSet<String> = HashSet::new();
    let mut distances: HashMap<String, u32> = HashMap::new();
    let mut injection_edges: HashMap<String, GraphTraversalInjection> = HashMap::new();
    let mut all_edges: Vec<MemoryEdge> = Vec::new();
    let mut seen_edges: HashSet<(String, String, String)> = HashSet::new();
    let mut queue: VecDeque<(String, u32)> = VecDeque::new();

    for id in seed_ids {
        if visited.insert(id.clone()) {
            distances.insert(id.clone(), 0);
            queue.push_back((id.clone(), 0));
        }
    }

    const MAX_NODES: usize = 50;

    while !queue.is_empty() {
        let mut frontier: Vec<String> = Vec::new();
        let mut current_depth = 0u32;

        while let Some((id, depth)) = queue.front() {
            if *depth >= max_hops || visited.len() >= MAX_NODES {
                queue.pop_front();
                continue;
            }
            current_depth = *depth;
            frontier.push(id.clone());
            queue.pop_front();
            if visited.len() >= MAX_NODES {
                break;
            }
        }

        if frontier.is_empty() {
            break;
        }

        let remaining_edges = edge_limit.saturating_sub(all_edges.len());
        if remaining_edges == 0 {
            break;
        }
        // A later frontier can encounter every edge already accumulated. Allow
        // those rows within this bounded query without reducing the unseen-edge
        // budget, while never fetching more than the global edge ceiling.
        let batch_limit = remaining_edges.saturating_add(seen_edges.len());
        let frontier_ids: HashSet<&str> = frontier.iter().map(String::as_str).collect();
        let edges_batch = get_edges_batch(conn, &frontier, relation_filter, batch_limit)?;
        for edge in edges_batch {
            if all_edges.len() >= edge_limit {
                break;
            }
            let edge_key = (
                edge.source_id.clone(),
                edge.target_id.clone(),
                edge.relation.clone(),
            );
            if !seen_edges.insert(edge_key) {
                continue;
            }

            for neighbor in [
                frontier_ids
                    .contains(edge.source_id.as_str())
                    .then_some(&edge.target_id),
                frontier_ids
                    .contains(edge.target_id.as_str())
                    .then_some(&edge.source_id),
            ]
            .into_iter()
            .flatten()
            {
                if visited.insert(neighbor.clone()) {
                    let new_depth = current_depth + 1;
                    distances.insert(neighbor.clone(), new_depth);
                    injection_edges.insert(
                        neighbor.clone(),
                        GraphTraversalInjection {
                            source_id: edge.source_id.clone(),
                            target_id: edge.target_id.clone(),
                            relation: edge.relation.clone(),
                            weight: edge.weight,
                            depth: new_depth,
                        },
                    );
                    queue.push_back((neighbor.clone(), new_depth));
                }
            }
            all_edges.push(edge);
        }

        if visited.len() >= MAX_NODES {
            break;
        }
    }

    // Fetch all discovered entries (exclude seeds — caller already has those)
    let non_seed_ids: Vec<String> = distances
        .keys()
        .filter(|id| !seed_ids.contains(id))
        .cloned()
        .collect();

    // tachi#1569 (cross-vendor review): these rows are the *system's* choice,
    // not ids the caller named, so on the Wiki store they carry the same
    // internal-row exclusion search results do. Seeds keep their existing
    // semantics — they are the caller's own ids and are excluded from
    // `entries` anyway. `edges` and `distances` still reference internal ids
    // where the graph really connects to one: withholding an edge would
    // change hop counts and spreading-activation weights, which is a
    // different decision from withholding a row's content.
    let entries = if non_seed_ids.is_empty() {
        Vec::new()
    } else {
        let map =
            fetch_by_ids_excluding_store_internal(conn, &non_seed_ids, false, wiki_corpus_store)?;
        map.into_values().collect()
    };

    // Keep the stable ordering the legacy result exposed after deduplication.
    all_edges.sort_by(|a, b| {
        (&a.source_id, &a.target_id, &a.relation).cmp(&(&b.source_id, &b.target_id, &b.relation))
    });

    Ok(GraphExpandResult {
        entries,
        edges: all_edges,
        distances,
        injection_edges,
    })
}
