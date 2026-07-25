//! Memory lifecycle consolidation loop (tachi#775 / #734-A1).
//!
//! Turns dry-run-only `tachi_memory(action="consolidate")` into a
//! **propose → review → apply** loop (mirror of `recall_proposals`):
//!
//! 1. **Propose** (default consolidate call): scan under `path_prefix` (default
//!    `/scratch`); emit durable proposals for:
//!    - `merge_into` — same path, high summary overlap → fold older into newer
//!    - `supersede` — same path, low overlap → newer wins without text merge
//!    - `near_dup_merge` — **cross-path** raw rows with high text-token Jaccard
//!      (RomanBath light-sleep detection). Same-path twins stay under
//!      `merge_into`/`supersede`. Connected components collapse to a star
//!      (one survivor; one proposal per non-survivor) so batch apply is safe.
//!    - `archive` — stale low-value rows
//!    - `promote_distilled` — raw rows that earned diverse recall (≥3 / ≥3)
//!
//!    Always non-mutating until apply.
//! 2. **Review**: `proposal_id` + `review_status=approved|rejected`.
//! 3. **Apply**: `proposal_id` + `confirm=true` after approval; mutates via
//!    store primitives and keeps provenance.
//!
//! Protected rows (never auto-proposed/applied as sources):
//! permanent/pinned/durable retention, wiki paths/categories, pattern tier.
//!
//! ## Near-dup scan window (provisional)
//!
//! Propose scans via `list_by_path(prefix, 500)` — path lexicographic ASC,
//! timestamp DESC — then `NEAR_DUP_RAW_SCAN_CAP` (same 500, provisional)
//! limits the raw pairwise window. This is **not** "newest 500 overall";
//! path bias can exclude recent rows under late prefixes. Recency-first
//! ordering is a follow-up; do not reorder the shared scan without updating
//! every generator that consumes it.

use super::evidence_format::{json_string, wants_json};
use crate::tool_params::TachiMemoryParams;
use crate::MemoryServer;
use chrono::{Duration, Utc};
use memcore::store::memory_lifecycle as lifecycle;
use memcore::MemoryEntry;
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap, HashSet};

const SCRATCH_PREFIX: &str = "/scratch";
const STALE_DAYS_DEFAULT: i64 = 30;
const ARCHIVE_IMPORTANCE_MAX: f64 = 0.55;
/// Same gate as `MemoryStore::promote_diversely_recalled_raw_memories`.
const PROMOTE_RECALL_MIN: i64 = 3;
const PROMOTE_DIVERSITY_MIN: i64 = 3;
/// A report should identify enough protected rows to make a zero-work scan
/// debuggable, without turning a broad maintenance response into a dump of
/// every memory body or identifier.
const SCOPE_ACCOUNTING_SAMPLE_LIMIT: usize = 20;

/// The proposal generators must not receive a bare filtered list: doing so
/// makes an all-protected prefix indistinguishable from an empty one.  Keep
/// the safety decision and its accounting together at the scan boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Ord, PartialOrd)]
enum ConsolidationExclusionReason {
    OutsideRequestedPrefix,
    AlreadySuperseded,
    AlreadyArchived,
    PatternTier,
    WikiCategory,
    WikiPath,
    RetentionPolicy,
    FutureProtection,
}

impl ConsolidationExclusionReason {
    const fn as_str(self) -> &'static str {
        match self {
            Self::OutsideRequestedPrefix => "outside_requested_prefix",
            Self::AlreadySuperseded => "already_superseded",
            Self::AlreadyArchived => "already_archived",
            Self::PatternTier => "pattern_tier",
            Self::WikiCategory => "wiki_category",
            Self::WikiPath => "wiki_path",
            Self::RetentionPolicy => "retention_policy",
            Self::FutureProtection => "future_protection",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ConsolidationExclusion {
    id: String,
    reason: ConsolidationExclusionReason,
}

struct ConsolidationScope {
    eligible: Vec<MemoryEntry>,
    exclusions: Vec<ConsolidationExclusion>,
}

impl ConsolidationScope {
    fn from_entries(
        entries: Vec<MemoryEntry>,
        path_prefix: &str,
        superseded_ids: &HashSet<String>,
    ) -> Self {
        let mut eligible = Vec::new();
        let mut exclusions = Vec::new();

        for entry in entries {
            let exclusion = if !entry.path.starts_with(path_prefix) {
                Some(ConsolidationExclusionReason::OutsideRequestedPrefix)
            } else if superseded_ids.contains(&entry.id) {
                Some(ConsolidationExclusionReason::AlreadySuperseded)
            } else {
                protection_reason(&entry)
            };
            if let Some(reason) = exclusion {
                exclusions.push(ConsolidationExclusion {
                    id: entry.id,
                    reason,
                });
            } else {
                eligible.push(entry);
            }
        }

        Self {
            eligible,
            exclusions,
        }
    }

    fn examined(&self) -> usize {
        self.eligible.len() + self.exclusions.len()
    }

    fn as_json(&self) -> Value {
        let excluded_count = self.exclusions.len();
        let by_reason = self
            .exclusions
            .iter()
            .fold(BTreeMap::new(), |mut counts, exclusion| {
                *counts.entry(exclusion.reason.as_str()).or_insert(0usize) += 1;
                counts
            });
        let samples = self
            .exclusions
            .iter()
            .take(SCOPE_ACCOUNTING_SAMPLE_LIMIT)
            .map(|exclusion| {
                json!({
                    "id": exclusion.id,
                    "reason": exclusion.reason.as_str(),
                })
            })
            .collect::<Vec<_>>();
        json!({
            "examined": self.examined(),
            "evaluated": self.eligible.len(),
            "expected_exclusions": {
                "count": excluded_count,
                "by_reason": by_reason,
                "samples": samples,
                "sample_limit": SCOPE_ACCOUNTING_SAMPLE_LIMIT,
            }
        })
    }
}
/// Minimum summary-token Jaccard to prefer `merge_into` over plain `supersede`.
const MERGE_SUMMARY_JACCARD_MIN: f64 = 0.50;
/// Minimum full-text token Jaccard for raw near-duplicate merge proposals
/// (RomanBath `run_light_sleep` parity; env-overridable).
const NEAR_DUP_TEXT_JACCARD_MIN_DEFAULT: f64 = 0.9;
const NEAR_DUP_TEXT_JACCARD_MIN_ENV: &str = "TACHI_CONSOLIDATE_NEAR_DUP_JACCARD_MIN";

fn near_dup_text_jaccard_min() -> f64 {
    std::env::var(NEAR_DUP_TEXT_JACCARD_MIN_ENV)
        .ok()
        .and_then(|value| value.trim().parse::<f64>().ok())
        .filter(|value| value.is_finite() && *value > 0.0 && *value <= 1.0)
        .unwrap_or(NEAR_DUP_TEXT_JACCARD_MIN_DEFAULT)
}

pub(crate) async fn handle_memory_consolidate(
    server: &MemoryServer,
    params: &TachiMemoryParams,
) -> Result<String, String> {
    let has_proposal = params
        .proposal_id
        .as_deref()
        .is_some_and(|id| !id.trim().is_empty());
    if has_proposal {
        // Review path: proposal_id + review_status
        if params
            .review_status
            .as_deref()
            .is_some_and(|s| !s.trim().is_empty())
        {
            return handle_review(server, params);
        }
        // Apply path: proposal_id (+ confirm gate inside)
        return handle_apply(server, params);
    }
    // Default: propose + list (always dry_run for mutations)
    handle_propose(server, params).await
}

async fn handle_propose(
    server: &MemoryServer,
    params: &TachiMemoryParams,
) -> Result<String, String> {
    let path_prefix = params
        .path_prefix
        .clone()
        .unwrap_or_else(|| SCRATCH_PREFIX.to_string());
    let generated = generate_and_persist_proposals(server, params, &path_prefix)?;
    let proposals = list_proposals(server, params)?;
    let response = json!({
        "status": "dry_run",
        "action": "consolidate",
        "kind": "memory_lifecycle",
        "requires_human_approval": true,
        "path_prefix": path_prefix,
        "generated_count": generated.proposals.len(),
        "generated": generated.proposals,
        "scope_accounting": generated.scope.as_json(),
        "count": proposals.len(),
        "proposals": proposals,
        "next_actions": [
            "tachi_memory(action='consolidate', proposal_id=..., review_status='approved')",
            "tachi_memory(action='consolidate', proposal_id=..., confirm=true) after approval"
        ],
        "note": "No rows mutated. Consolidation applies only after review + confirm=true.",
    });
    if wants_json(params.format.as_deref()) {
        return json_string(&response);
    }
    Ok(format!(
        "Tachi consolidate\nstatus: dry_run\ngenerated: {}\npending proposals: {}\nnext: review then confirm=true to apply",
        generated.proposals.len(),
        proposals
            .iter()
            .filter(|p| p.get("status").and_then(Value::as_str) == Some("pending"))
            .count()
    ))
}

fn handle_review(server: &MemoryServer, params: &TachiMemoryParams) -> Result<String, String> {
    let proposal_id = required_proposal_id(params)?;
    let decision = lifecycle::LifecycleReviewDecision::parse(
        params.review_status.as_deref().unwrap_or_default(),
    )
    .map_err(|e| e.to_string())?;
    let note = params
        .notes
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    // Delegate to the lifecycle module: v2 schema check, pending-only gate,
    // and hard_state version CAS all live there. Legacy v1 proposals are
    // refused loudly inside `review_lifecycle_proposal`.
    let updated = with_proposal_store(server, params, |store| {
        lifecycle::review_lifecycle_proposal(store, proposal_id, decision, note)
            .map_err(|e| e.to_string())
    })?;

    let status = decision.as_str();
    let response = json!({
        "status": "completed",
        "action": "consolidate",
        "sub_action": "review",
        "proposal_id": proposal_id,
        "proposal": updated,
    });
    if wants_json(params.format.as_deref()) {
        return json_string(&response);
    }
    Ok(format!(
        "Tachi consolidate review\nstatus: completed\nproposal_id: `{proposal_id}`\nreview_status: {status}"
    ))
}

fn handle_apply(server: &MemoryServer, params: &TachiMemoryParams) -> Result<String, String> {
    let proposal_id = required_proposal_id(params)?;
    if !params.confirm {
        return Err(
            "apply consolidate requires confirm=true after human approval; no memory rows mutated"
                .to_string(),
        );
    }

    // Delegate to the lifecycle module: v2 schema check, approved-only gate,
    // full hash/revision/path/protection revalidation inside one
    // BEGIN IMMEDIATE transaction, mutation + proposal-status CAS, commit.
    // Legacy v1 proposals are refused loudly inside `apply_lifecycle_proposal`.
    let result = with_memory_store(server, params, |store| {
        lifecycle::apply_lifecycle_proposal(store, proposal_id).map_err(|e| e.to_string())
    })?;

    // #1413 concern 1 / recall-cache invalidation: run AFTER
    // `apply_lifecycle_proposal` returns — its BEGIN IMMEDIATE transaction
    // is already committed at this point, so the invalidation never fires
    // on a rolled-back apply.
    let _ = crate::memory_search_ops::invalidate_recall_cache_after_write(
        server,
        "consolidate_lifecycle",
    );

    let response = json!({
        "status": "completed",
        "action": "consolidate",
        "sub_action": "apply",
        "proposal_id": proposal_id,
        "apply_result": result.apply_result,
        "proposal": result.proposal,
    });
    if wants_json(params.format.as_deref()) {
        return json_string(&response);
    }
    let action = result
        .apply_result
        .get("lifecycle_action")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    Ok(format!(
        "Tachi consolidate apply\nstatus: completed\nproposal_id: `{proposal_id}`\nlifecycle_action: {action}"
    ))
}

/// #1043 D3 (terminal-review narrowed): direct-call entry point for
/// automated (non-human-reviewed) callers that need the SAME `merge_into`
/// lifecycle mutation `tachi_memory(action='consolidate')` applies after
/// human approval — used by the distill batch's pre-selection
/// duplicate-collapse pass for byte-identical rows.
///
/// Deliberately hard-codes `action="merge_into"` rather than accepting an
/// `action: &str` parameter: this is the ONLY lifecycle mutation any
/// automated caller may reach without going through the human-reviewed
/// propose/review/apply loop. A generic `action` parameter here would open
/// every lifecycle action (`supersede`/`archive`/`promote_distilled`) to
/// bypassing human review, which the module doc above says is exactly what
/// this proposal/review/apply loop exists to prevent.
pub(crate) fn merge_into_for_project(
    server: &MemoryServer,
    project: Option<&str>,
    source_id: &str,
    target_id: &str,
) -> Result<Value, String> {
    apply_lifecycle_action(
        server,
        &minimal_params_for_project(project),
        "merge_into",
        source_id,
        Some(target_id),
    )
}

/// Bare `TachiMemoryParams` carrying only `action`/`project`, for callers
/// (see `merge_into_for_project`) that need to drive the store-resolution
/// helpers below without a real facade call's full param set. Field list
/// mirrors `tests::facade_tests::tachi_memory_params`.
fn minimal_params_for_project(project: Option<&str>) -> TachiMemoryParams {
    TachiMemoryParams {
        action: "consolidate".to_string(),
        format: None,
        query: None,
        scope: None,
        top_k: 6,
        path_prefix: None,
        file_context: None,
        error_context: None,
        category: None,
        include_archived: false,
        include_training: false,
        enable_rerank: false,
        as_of: None,
        synthesize: false,
        model: None,
        agent_role: None,
        text: None,
        title: None,
        summary: None,
        topic: None,
        keywords: Vec::new(),
        entities: Vec::new(),
        importance: None,
        retention_policy: None,
        kind: None,
        path: None,
        id: None,
        force: false,
        source: None,
        valid_from: None,
        valid_until: None,
        metadata: None,
        emit_continuity: false,
        files: Vec::new(),
        references: Vec::new(),
        flow_id: None,
        event: None,
        state: None,
        project: project.map(str::to_string),
        project_explicit: false,
        domain: None,
        compact: false,
        proposal_id: None,
        review_status: None,
        notes: None,
        confirm: false,
        state_filter: None,
        content: None,
        ingest_type: "source".to_string(),
        source_url: None,
        auto_chunk: true,
        auto_summarize: true,
        auto_link: true,
        chunk_size_chars: 1200,
        chunk_overlap_chars: 120,
        conversation_id: None,
        turn_id: None,
        event_type: None,
        messages: Vec::new(),
        issue_ref: None,
        branch: None,
        declared_file_scope: Vec::new(),
        claim_id: None,
        dispatch_id: None,
        release_reason: None,
        to: None,
        ttl_days: None,
        include_read: false,
        agent_id: None,
    }
}

fn apply_lifecycle_action(
    server: &MemoryServer,
    params: &TachiMemoryParams,
    action: &str,
    source_id: &str,
    target_id: Option<&str>,
) -> Result<Value, String> {
    let result = match action {
        "supersede" => {
            let target = target_id.ok_or_else(|| {
                "supersede proposal requires target_id (canonical survivor)".to_string()
            })?;
            with_memory_store(server, params, |store| {
                refuse_if_protected(store, source_id, "supersede")?;
                let changed = store
                    .supersede_memory(source_id, target)
                    .map_err(|e| format!("supersede_memory: {e}"))?;
                let archived = store
                    .archive_memory(source_id)
                    .map_err(|e| format!("archive after supersede: {e}"))?;
                Ok(json!({
                    "lifecycle_action": "supersede",
                    "source_id": source_id,
                    "target_id": target,
                    "superseded": changed,
                    "archived": archived,
                }))
            })
        }
        "merge_into" | "near_dup_merge" => {
            let target = target_id.ok_or_else(|| {
                format!("{action} proposal requires target_id (canonical survivor)")
            })?;
            with_memory_store(server, params, |store| {
                refuse_if_protected(store, source_id, action)?;
                let source = store
                    .get(source_id)
                    .map_err(|e| format!("load source: {e}"))?
                    .ok_or_else(|| format!("source not found: {source_id}"))?;
                let mut survivor = store
                    .get(target)
                    .map_err(|e| format!("load target: {e}"))?
                    .ok_or_else(|| format!("target not found: {target}"))?;
                // Canonical on both sides of the no-op guard below — the fold
                // is sorted+deduplicated while the stored column keeps the last
                // writer's serialization order, so a raw comparison would
                // report "changed" for any target whose array is not already
                // sorted and unique. Same helper as the reviewed apply path in
                // `memcore::store::memory_lifecycle`, so the two agree on what
                // "unchanged" means.
                let target_keywords = lifecycle::canonical_tags(&survivor.keywords);
                let target_entities = lifecycle::canonical_tags(&survivor.entities);
                let target_importance = survivor.importance;
                // Fold unique keywords/entities; keep survivor text as canonical.
                let mut merged_kw = survivor.keywords.clone();
                merged_kw.extend(source.keywords.iter().cloned());
                survivor.keywords = lifecycle::canonical_tags(&merged_kw);
                let mut merged_ents = survivor.entities.clone();
                merged_ents.extend(source.entities.iter().cloned());
                survivor.entities = lifecycle::canonical_tags(&merged_ents);
                if survivor.importance < source.importance {
                    survivor.importance = source.importance;
                }
                // An unchanged survivor must not be rewritten: upsert bumps
                // revision, which would invalidate approved sibling star
                // proposals that target the same snapshot.
                if survivor.keywords != target_keywords
                    || survivor.entities != target_entities
                    || survivor.importance != target_importance
                {
                    store
                        .upsert(&survivor)
                        .map_err(|e| format!("upsert merged survivor: {e}"))?;
                }
                let changed = store
                    .supersede_memory(source_id, target)
                    .map_err(|e| format!("supersede_memory after merge: {e}"))?;
                let archived = store
                    .archive_memory(source_id)
                    .map_err(|e| format!("archive after merge: {e}"))?;
                Ok(json!({
                    "lifecycle_action": action,
                    "source_id": source_id,
                    "target_id": target,
                    "merged_keywords": survivor.keywords.len(),
                    "merged_entities": survivor.entities.len(),
                    "superseded": changed,
                    "archived": archived,
                }))
            })
        }
        "archive" => with_memory_store(server, params, |store| {
            refuse_if_protected(store, source_id, "archive")?;
            let archived = store
                .archive_memory(source_id)
                .map_err(|e| format!("archive_memory: {e}"))?;
            Ok(json!({
                "lifecycle_action": "archive",
                "source_id": source_id,
                "archived": archived,
            }))
        }),
        "promote_distilled" => with_memory_store(server, params, |store| {
            refuse_if_protected(store, source_id, "promote_distilled")?;
            let mut entry = store
                .get(source_id)
                .map_err(|e| format!("load source: {e}"))?
                .ok_or_else(|| format!("source not found: {source_id}"))?;
            let prev_tier = entry.tier.clone();
            if !entry.tier.eq_ignore_ascii_case("raw") && !entry.tier.is_empty() {
                // Idempotent if already consolidated; still allow re-apply.
                if entry.tier.eq_ignore_ascii_case("consolidated")
                    || entry.tier.eq_ignore_ascii_case("pattern")
                {
                    return Ok(json!({
                        "lifecycle_action": "promote_distilled",
                        "source_id": source_id,
                        "tier_before": prev_tier,
                        "tier_after": entry.tier,
                        "changed": false,
                    }));
                }
            }
            entry.tier = "consolidated".to_string();
            store
                .upsert(&entry)
                .map_err(|e| format!("upsert promoted: {e}"))?;
            Ok(json!({
                "lifecycle_action": "promote_distilled",
                "source_id": source_id,
                "tier_before": prev_tier,
                "tier_after": "consolidated",
                "changed": true,
            }))
        }),
        other => Err(format!(
            "unsupported lifecycle_action '{other}'; expected supersede|merge_into|near_dup_merge|archive|promote_distilled"
        )),
    }?;

    // #1413 concern 1: every lifecycle arm above mutates memory content
    // (supersede / merge_into / near_dup_merge / archive / promote_distilled
    // all change what a subsequent search surfaces), so bust the shared
    // (global) recall cache once the store commit returned. The invalidator
    // re-takes the global write gate via `with_global_store`; calling it from
    // INSIDE any `with_memory_store` closure above would nest that gate inside
    // the project/named-project gate (or recurse on it when the store resolves
    // to global), so it must run here — after the closure returned. The
    // `other => Err(..)` arm `?`-bails before reaching this point, so an
    // unsupported action never invalidates.
    let _ = crate::memory_search_ops::invalidate_recall_cache_after_write(
        server,
        "consolidate_lifecycle",
    );
    Ok(result)
}

fn refuse_if_protected(
    store: &mut memcore::MemoryStore,
    source_id: &str,
    action: &str,
) -> Result<(), String> {
    if let Some(entry) = store
        .get(source_id)
        .map_err(|e| format!("load source: {e}"))?
    {
        if is_protected(&entry) {
            return Err(format!(
                "refusing to {action} protected memory {source_id} (retention/wiki/pattern)"
            ));
        }
    }
    Ok(())
}

struct ProposalGeneration {
    proposals: Vec<Value>,
    scope: ConsolidationScope,
}

#[cfg(test)]
struct ProposalPersistenceTestHook {
    after_build: Option<Box<dyn FnOnce(&[Value])>>,
}

#[cfg(test)]
thread_local! {
    /// One-shot, per-calling-thread synchronization seam. Unlike a process
    /// global hook, parallel tests cannot observe or consume another test's
    /// callbacks. The guard below also clears an unconsumed hook on unwind.
    static PROPOSAL_PERSISTENCE_TEST_HOOK:
        std::cell::RefCell<Option<ProposalPersistenceTestHook>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(crate) struct ProposalPersistenceTestHookGuard;

#[cfg(test)]
impl Drop for ProposalPersistenceTestHookGuard {
    fn drop(&mut self) {
        PROPOSAL_PERSISTENCE_TEST_HOOK.with(|slot| {
            slot.borrow_mut().take();
        });
    }
}

#[cfg(test)]
pub(crate) fn install_proposal_persistence_test_hook(
    after_build: impl FnOnce(&[Value]) + 'static,
) -> ProposalPersistenceTestHookGuard {
    PROPOSAL_PERSISTENCE_TEST_HOOK.with(|slot| {
        let previous = slot.borrow_mut().replace(ProposalPersistenceTestHook {
            after_build: Some(Box::new(after_build)),
        });
        assert!(
            previous.is_none(),
            "proposal persistence test hook already installed"
        );
    });
    ProposalPersistenceTestHookGuard
}

#[cfg(test)]
fn run_proposal_persistence_test_after_build(proposals: &[Value]) {
    let callback = PROPOSAL_PERSISTENCE_TEST_HOOK.with(|slot| {
        slot.borrow_mut()
            .take()
            .and_then(|mut hook| hook.after_build.take())
    });
    if let Some(callback) = callback {
        callback(proposals);
    }
}

fn generate_and_persist_proposals(
    server: &MemoryServer,
    params: &TachiMemoryParams,
    path_prefix: &str,
) -> Result<ProposalGeneration, String> {
    // One exclusive MemoryServer store gate spans list/census, endpoint
    // snapshot construction, existing-status checks, and persistence. This
    // closes the production race with same-server writers, including
    // `mark_superseded_closing_validity`, so a proposal built from A cannot
    // become visible after that writer has changed A -> C in the gap.
    //
    // This gate does not serialize arbitrary external processes that open the
    // same SQLite database independently. Apply's `BEGIN IMMEDIATE` live-row
    // revalidation remains the cross-process/database last line of defense.
    with_proposal_store(server, params, |store| {
        let entries = store
            .list_by_path(path_prefix, 500, false)
            .map_err(|e| format!("list_by_path: {e}"))?;

        // `list_by_path(..., include_archived=false)` excludes archived rows
        // but deliberately does not hide a live row whose supersession edge
        // was written without archiving it. Every lifecycle payload currently
        // records `superseded_by=None` at propose time, so remove those rows
        // from the one shared source/target pool before any generator reaches
        // `snapshot_endpoint`. Keep the exclusion in scope accounting rather
        // than making a superseded scan look empty.
        let mut superseded_ids = HashSet::new();
        for entry in &entries {
            if store
                .supersession_target(&entry.id)
                .map_err(|e| format!("load supersession state for {}: {e}", entry.id))?
                .flatten()
                .is_some()
            {
                superseded_ids.insert(entry.id.clone());
            }
        }

        let scope = ConsolidationScope::from_entries(entries, path_prefix, &superseded_ids);
        let mut proposals = Vec::new();
        proposals.extend(propose_same_path_lifecycle(&scope.eligible, path_prefix));
        proposals.extend(propose_near_dup_merge(&scope.eligible, path_prefix));
        proposals.extend(propose_stale_archives(&scope.eligible, path_prefix));
        proposals.extend(propose_promote_distilled(&scope.eligible, path_prefix));

        if proposals.is_empty() {
            return Ok(ProposalGeneration { proposals, scope });
        }

        #[cfg(test)]
        run_proposal_persistence_test_after_build(&proposals);

        for proposal in &proposals {
            let id = proposal["proposal_id"]
                .as_str()
                .ok_or_else(|| "proposal missing proposal_id".to_string())?;
            // Do not clobber approved/applied/rejected.
            if let Some((existing, _)) = store
                .get_state_kv(lifecycle::LIFECYCLE_PROPOSAL_NS, id)
                .map_err(|e| format!("load existing proposal: {e}"))?
            {
                if let Ok(existing_json) = serde_json::from_str::<Value>(&existing) {
                    let status = existing_json
                        .get("status")
                        .and_then(Value::as_str)
                        .unwrap_or("pending");
                    if status != "pending" {
                        continue;
                    }
                }
            }
            let raw =
                serde_json::to_string(proposal).map_err(|e| format!("serialize proposal: {e}"))?;
            store
                .set_state(lifecycle::LIFECYCLE_PROPOSAL_NS, id, &raw)
                .map_err(|e| format!("persist proposal: {e}"))?;
        }
        Ok(ProposalGeneration { proposals, scope })
    })
}

/// Same-path duplicates → `merge_into` when summaries overlap enough, else
/// plain `supersede` (newer wins without folding keywords).
/// Stamp a pending proposal with v2 identity fields (`schema_version`,
/// `policy_version`, `identity`, `apply_payload`). The identity is a SHA-256
/// over the typed immutable [`lifecycle::LifecycleApplyPayload`], rebuilt and
/// re-verified at apply. Volatile state (status/review/timestamps) lives
/// outside the payload and can never perturb the identity.
fn enrich_with_v2_identity(
    mut proposal: Value,
    action: &str,
    source: &MemoryEntry,
    target: Option<&MemoryEntry>,
) -> Value {
    let review_display = lifecycle::LifecycleReviewDisplay {
        kind: proposal["kind"]
            .as_str()
            .expect("lifecycle proposal kind must be a string")
            .to_string(),
        requires_human_approval: proposal["requires_human_approval"]
            .as_bool()
            .expect("lifecycle proposal approval requirement must be a boolean"),
        path: proposal["path"]
            .as_str()
            .expect("lifecycle proposal path must be a string")
            .to_string(),
        rationale: proposal["rationale"]
            .as_str()
            .expect("lifecycle proposal rationale must be a string")
            .to_string(),
        evidence: proposal["evidence"].clone(),
    };
    let payload =
        lifecycle::build_apply_payload_with_review_display(action, source, target, review_display);
    let identity = lifecycle::compute_lifecycle_identity(&payload);
    let proposal_id = lifecycle::lifecycle_proposal_id(&payload)
        .expect("proposal generator uses a known lifecycle action");
    let obj = proposal
        .as_object_mut()
        .expect("lifecycle proposal must be a JSON object");
    obj.insert(
        "schema_version".into(),
        json!(lifecycle::LIFECYCLE_SCHEMA_VERSION),
    );
    obj.insert(
        "policy_version".into(),
        json!(lifecycle::LIFECYCLE_POLICY_VERSION),
    );
    obj.insert("identity".into(), json!(identity));
    obj.insert("proposal_id".into(), json!(proposal_id));
    obj.insert(
        "apply_payload".into(),
        serde_json::to_value(&payload).unwrap_or(Value::Null),
    );
    proposal
}

/// Same-path duplicates → `merge_into` when summaries overlap enough, else
/// plain `supersede` (newer wins without folding keywords).
fn propose_same_path_lifecycle(entries: &[MemoryEntry], path_prefix: &str) -> Vec<Value> {
    let mut by_path: HashMap<String, Vec<&MemoryEntry>> = HashMap::new();
    for entry in entries {
        if is_protected(entry) {
            continue;
        }
        if !entry.path.starts_with(path_prefix) {
            continue;
        }
        by_path.entry(entry.path.clone()).or_default().push(entry);
    }

    let mut out = Vec::new();
    for (path, mut group) in by_path {
        if group.len() < 2 {
            continue;
        }
        group.sort_by(|a, b| cmp_entry_timestamp_desc(a, b).then_with(|| a.id.cmp(&b.id)));
        let survivor = group[0];
        for older in group.iter().skip(1) {
            if older.id == survivor.id {
                continue;
            }
            let jaccard = summary_token_jaccard(&older.summary, &survivor.summary);
            let (action, rationale) = if jaccard + f64::EPSILON >= MERGE_SUMMARY_JACCARD_MIN {
                (
                    "merge_into",
                    format!(
                        "Same path `{}` has near-duplicate summaries (jaccard={jaccard:.2}); merge older `{}` into newer `{}`.",
                        path, older.id, survivor.id
                    ),
                )
            } else {
                (
                    "supersede",
                    format!(
                        "Same path `{}` has multiple active rows with divergent summaries (jaccard={jaccard:.2}); supersede older `{}` with newer `{}`.",
                        path, older.id, survivor.id
                    ),
                )
            };
            out.push(enrich_with_v2_identity(
                json!({
                    "kind": "memory_lifecycle",
                    "lifecycle_action": action,
                    "status": "pending",
                    "requires_human_approval": true,
                    "created_or_refreshed_at": Utc::now().to_rfc3339(),
                    "source_id": older.id,
                    "target_id": survivor.id,
                    "path": path,
                    "rationale": rationale,
                    "evidence": {
                        "source_timestamp": older.timestamp,
                        "target_timestamp": survivor.timestamp,
                        "source_summary": older.summary,
                        "target_summary": survivor.summary,
                        "summary_token_jaccard": jaccard,
                        "source_access_count": older.access_count,
                        "target_access_count": survivor.access_count,
                    },
                }),
                action,
                older,
                Some(survivor),
            ));
        }
    }
    out
}

/// Cross-path raw-tier text near-duplicates (token Jaccard) → `near_dup_merge`.
///
/// Same-path twins are owned by [`propose_same_path_lifecycle`]; this generator
/// skips same-path edges. Pairwise hits are collapsed per connected component
/// into a star (one survivor; one proposal per non-survivor) so batch
/// approve+apply cannot orphan a source that was also a target.
/// Apply reuses the existing `merge_into` mutation path after human review.
fn propose_near_dup_merge(entries: &[MemoryEntry], path_prefix: &str) -> Vec<Value> {
    let scoped: Vec<MemoryEntry> = entries
        .iter()
        .filter(|entry| {
            entry.path.starts_with(path_prefix)
                && !is_protected(entry)
                && entry.tier.eq_ignore_ascii_case("raw")
        })
        .cloned()
        .collect();
    if scoped.len() < 2 {
        return Vec::new();
    }

    let threshold = near_dup_text_jaccard_min();
    let pairs = memcore::near_duplicate_raw_pairs(&scoped, threshold);
    // Union-find over cross-path near-dup edges only.
    let mut parent: Vec<usize> = (0..scoped.len()).collect();
    let find = |parent: &mut [usize], mut x: usize| -> usize {
        while parent[x] != x {
            parent[x] = parent[parent[x]];
            x = parent[x];
        }
        x
    };
    let mut edge_similarity: HashMap<(usize, usize), f64> = HashMap::new();
    for (left, right, similarity) in pairs {
        if left == right || scoped[left].path == scoped[right].path {
            continue;
        }
        let root_left = find(&mut parent, left);
        let root_right = find(&mut parent, right);
        if root_left != root_right {
            parent[root_right] = root_left;
        }
        let key = if left < right {
            (left, right)
        } else {
            (right, left)
        };
        edge_similarity.insert(key, similarity);
    }

    let mut components: HashMap<usize, Vec<usize>> = HashMap::new();
    for index in 0..scoped.len() {
        let root = find(&mut parent, index);
        components.entry(root).or_default().push(index);
    }

    let mut out = Vec::new();
    for mut members in components.into_values() {
        if members.len() < 2 {
            continue;
        }
        members.sort_unstable();
        let survivor_idx = pick_near_dup_survivor_index(&scoped, &members);
        let survivor = &scoped[survivor_idx];
        for &source_idx in &members {
            if source_idx == survivor_idx {
                continue;
            }
            let source = &scoped[source_idx];
            let similarity = edge_similarity
                .get(&(source_idx.min(survivor_idx), source_idx.max(survivor_idx)))
                .copied()
                .unwrap_or_else(|| memcore::text_token_jaccard(&source.text, &survivor.text));
            out.push(enrich_with_v2_identity(
                json!({
                    "kind": "memory_lifecycle",
                    "lifecycle_action": "near_dup_merge",
                    "status": "pending",
                    "requires_human_approval": true,
                    "created_or_refreshed_at": Utc::now().to_rfc3339(),
                    "source_id": source.id,
                    "target_id": survivor.id,
                    "path": source.path,
                    "rationale": format!(
                        "Raw near-duplicate text (token_jaccard={similarity:.3}) — merge lower-value `{}` into `{}`.",
                        source.id, survivor.id
                    ),
                    "evidence": {
                        "source_path": source.path,
                        "target_path": survivor.path,
                        "source_timestamp": source.timestamp,
                        "target_timestamp": survivor.timestamp,
                        "source_importance": source.importance,
                        "target_importance": survivor.importance,
                        "text_token_jaccard": similarity,
                        "near_dup_threshold": threshold,
                        "source_text_preview": source.text.chars().take(120).collect::<String>(),
                        "target_text_preview": survivor.text.chars().take(120).collect::<String>(),
                    },
                }),
                "near_dup_merge",
                source,
                Some(survivor),
            ));
        }
    }
    out
}

fn cmp_near_dup_survivor(a: &MemoryEntry, b: &MemoryEntry) -> std::cmp::Ordering {
    a.importance
        .partial_cmp(&b.importance)
        .unwrap_or(std::cmp::Ordering::Equal)
        .then_with(|| cmp_entry_timestamp_desc(a, b).reverse())
        .then_with(|| b.id.cmp(&a.id))
}

fn pick_near_dup_survivor_index(entries: &[MemoryEntry], members: &[usize]) -> usize {
    let mut best = members[0];
    for &candidate in members.iter().skip(1) {
        if cmp_near_dup_survivor(&entries[candidate], &entries[best]) == std::cmp::Ordering::Greater
        {
            best = candidate;
        }
    }
    best
}

fn propose_promote_distilled(entries: &[MemoryEntry], path_prefix: &str) -> Vec<Value> {
    let mut out = Vec::new();
    for entry in entries {
        if is_protected(entry) {
            continue;
        }
        if !entry.path.starts_with(path_prefix) {
            continue;
        }
        if !entry.tier.eq_ignore_ascii_case("raw") {
            continue;
        }
        if entry.recall_count < PROMOTE_RECALL_MIN || entry.query_diversity < PROMOTE_DIVERSITY_MIN
        {
            continue;
        }
        out.push(enrich_with_v2_identity(
            json!({
                "kind": "memory_lifecycle",
                "lifecycle_action": "promote_distilled",
                "status": "pending",
                "requires_human_approval": true,
                "created_or_refreshed_at": Utc::now().to_rfc3339(),
                "source_id": entry.id,
                "target_id": Value::Null,
                "path": entry.path,
                "rationale": format!(
                    "Raw row `{}` earned diverse recall (recall_count={}, query_diversity={}); promote to consolidated.",
                    entry.id, entry.recall_count, entry.query_diversity
                ),
                "evidence": {
                    "tier": entry.tier,
                    "recall_count": entry.recall_count,
                    "query_diversity": entry.query_diversity,
                    "access_count": entry.access_count,
                    "importance": entry.importance,
                    "summary": entry.summary,
                },
            }),
            "promote_distilled",
            entry,
            None,
        ));
    }
    out
}

fn summary_token_jaccard(a: &str, b: &str) -> f64 {
    // `tokenize` is re-exported from the scorer module path used by memcore.
    let ta: std::collections::HashSet<String> = memcore::scorer::tokenize(a).into_iter().collect();
    let tb: std::collections::HashSet<String> = memcore::scorer::tokenize(b).into_iter().collect();
    if ta.is_empty() || tb.is_empty() {
        return 0.0;
    }
    let inter = ta.intersection(&tb).count() as f64;
    let union = ta.union(&tb).count() as f64;
    inter / union.max(1.0)
}

fn propose_stale_archives(entries: &[MemoryEntry], path_prefix: &str) -> Vec<Value> {
    let cutoff = Utc::now() - Duration::days(STALE_DAYS_DEFAULT);
    let mut out = Vec::new();
    for entry in entries {
        if is_protected(entry) {
            continue;
        }
        if !entry.path.starts_with(path_prefix) {
            continue;
        }
        if entry.importance > ARCHIVE_IMPORTANCE_MAX {
            continue;
        }
        if entry.access_count > 0 || entry.recall_count > 0 {
            continue;
        }
        if !entry_timestamp_before(entry, cutoff) {
            continue;
        }
        out.push(enrich_with_v2_identity(
            json!({
                "kind": "memory_lifecycle",
                "lifecycle_action": "archive",
                "status": "pending",
                "requires_human_approval": true,
                "created_or_refreshed_at": Utc::now().to_rfc3339(),
                "source_id": entry.id,
                "target_id": Value::Null,
                "path": entry.path,
                "rationale": format!(
                    "Stale low-value row `{}` (importance={}, access=0, older than {STALE_DAYS_DEFAULT}d).",
                    entry.id, entry.importance
                ),
                "evidence": {
                    "timestamp": entry.timestamp,
                    "importance": entry.importance,
                    "access_count": entry.access_count,
                    "recall_count": entry.recall_count,
                    "summary": entry.summary,
                    "retention_policy": entry.retention_policy,
                },
            }),
            "archive",
            entry,
            None,
        ));
    }
    out
}

fn cmp_entry_timestamp_desc(a: &MemoryEntry, b: &MemoryEntry) -> std::cmp::Ordering {
    match (parse_entry_utc(&a.timestamp), parse_entry_utc(&b.timestamp)) {
        (Some(ta), Some(tb)) => tb.cmp(&ta),
        _ => b.timestamp.cmp(&a.timestamp),
    }
}

fn entry_timestamp_before(entry: &MemoryEntry, cutoff: chrono::DateTime<Utc>) -> bool {
    match parse_entry_utc(&entry.timestamp) {
        Some(ts) => ts < cutoff,
        // Fall back to lexicographic RFC3339 when parse fails (legacy/odd rows).
        None => entry.timestamp < cutoff.to_rfc3339(),
    }
}

fn parse_entry_utc(raw: &str) -> Option<chrono::DateTime<Utc>> {
    chrono::DateTime::parse_from_rfc3339(raw)
        .ok()
        .map(|dt| dt.with_timezone(&Utc))
}

fn is_protected(entry: &MemoryEntry) -> bool {
    protection_reason(entry).is_some()
}

fn protection_reason(entry: &MemoryEntry) -> Option<ConsolidationExclusionReason> {
    lifecycle::lifecycle_protection_reason(entry).map(|reason| match reason {
        "archived" => ConsolidationExclusionReason::AlreadyArchived,
        "pattern_tier" => ConsolidationExclusionReason::PatternTier,
        "wiki_category" => ConsolidationExclusionReason::WikiCategory,
        "wiki_path" => ConsolidationExclusionReason::WikiPath,
        "retention_policy" => ConsolidationExclusionReason::RetentionPolicy,
        _ => ConsolidationExclusionReason::FutureProtection,
    })
}

fn list_proposals(server: &MemoryServer, params: &TachiMemoryParams) -> Result<Vec<Value>, String> {
    // Same store resolution as propose/review/apply so named `project=` pins
    // see the proposals they just generated (Gemini #904 review).
    let rows = with_proposal_store_read(server, params, |store| {
        store
            .list_state(lifecycle::LIFECYCLE_PROPOSAL_NS)
            .map_err(|e| format!("list lifecycle proposals: {e}"))
    })?;

    let filter = params
        .state_filter
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_ascii_lowercase());
    let mut out = Vec::new();
    for row in rows {
        let Ok(value) = serde_json::from_str::<Value>(&row.value_json) else {
            continue;
        };
        if let Some(ref want) = filter {
            let status = value
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or("pending")
                .to_ascii_lowercase();
            if &status != want {
                continue;
            }
        }
        out.push(value);
    }
    Ok(out)
}

fn required_proposal_id(params: &TachiMemoryParams) -> Result<&str, String> {
    params
        .proposal_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "proposal_id is required".to_string())
}

fn with_proposal_store<T>(
    server: &MemoryServer,
    params: &TachiMemoryParams,
    f: impl FnOnce(&mut memcore::MemoryStore) -> Result<T, String>,
) -> Result<T, String> {
    // Named project pin first; else project DB; else global.
    if let Some(name) = params
        .project
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        return server.with_named_project_store(name, f);
    }
    if server.has_project_db() {
        return server.with_project_store(f);
    }
    server.with_global_store(f)
}

fn with_proposal_store_read<T>(
    server: &MemoryServer,
    params: &TachiMemoryParams,
    f: impl FnOnce(&mut memcore::MemoryStore) -> Result<T, String>,
) -> Result<T, String> {
    // *_store_read still takes &mut MemoryStore (shared lock style).
    if let Some(name) = params
        .project
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        return server.with_named_project_store_read(name, f);
    }
    if server.has_project_db() {
        return server.with_project_store_read(f);
    }
    server.with_global_store_read(f)
}

fn with_memory_store<T>(
    server: &MemoryServer,
    params: &TachiMemoryParams,
    f: impl FnOnce(&mut memcore::MemoryStore) -> Result<T, String>,
) -> Result<T, String> {
    with_proposal_store(server, params, f)
}

fn with_memory_store_read<T>(
    server: &MemoryServer,
    params: &TachiMemoryParams,
    f: impl FnOnce(&mut memcore::MemoryStore) -> Result<T, String>,
) -> Result<T, String> {
    with_proposal_store_read(server, params, f)
}

#[cfg(test)]
mod near_dup_threshold_tests {
    use super::{
        near_dup_text_jaccard_min, NEAR_DUP_TEXT_JACCARD_MIN_DEFAULT, NEAR_DUP_TEXT_JACCARD_MIN_ENV,
    };
    use crate::test_support::EnvRestore;
    use std::sync::Mutex;

    /// Serialize env mutation — parallel tests racing on the same key flake.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn near_dup_threshold_env_edge_cases() {
        let _lock = ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        {
            let _guard = EnvRestore::remove(NEAR_DUP_TEXT_JACCARD_MIN_ENV);
            assert_eq!(
                near_dup_text_jaccard_min(),
                NEAR_DUP_TEXT_JACCARD_MIN_DEFAULT
            );
        }
        {
            let _guard = EnvRestore::set(NEAR_DUP_TEXT_JACCARD_MIN_ENV, "0.85");
            assert!((near_dup_text_jaccard_min() - 0.85).abs() < f64::EPSILON);
        }
        for bad in ["not-a-number", "0", "0.0", "1.01", "-0.1", ""] {
            let _guard = EnvRestore::set(NEAR_DUP_TEXT_JACCARD_MIN_ENV, bad);
            assert_eq!(
                near_dup_text_jaccard_min(),
                NEAR_DUP_TEXT_JACCARD_MIN_DEFAULT,
                "bad env {bad:?} must fall back to default"
            );
        }
    }
}
