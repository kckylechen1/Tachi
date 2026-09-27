//! Skill quality guards: merge hints, skill-graph pagerank and stale-skill
//! archiving, stored under `definition.quality_guard` of each skill row.
//!
//! ### Write discipline (audit G1)
//! The refresh reads a snapshot, runs an expensive pairwise pass, and only
//! then writes. It therefore never writes a row back from that snapshot.
//! Each row is updated through
//! [`MemoryStore::hub_update_definition_with`], which re-reads the row in a
//! `BEGIN IMMEDIATE` transaction and rewrites **only** the `definition`
//! column (compare-and-set against the value it just read). Counters,
//! health/circuit-breaker state, enablement, review state and the row's
//! `updated_at` column written concurrently by feedback, `hub_call` outcomes
//! or review survive. When a row's content or `skill_path` (the inputs the
//! quality result was computed from) changed after the snapshot, the row is
//! skipped and reported in `skipped_conflicts`; its own content change
//! schedules a fresh refresh.
//!
//! A row is written only when the substantive quality result (merge hints,
//! pagerank, archive state) differs from what is stored, so an idle refresh
//! writes nothing and does not bump `quality_guard.updated_at`.
//!
//! ### Triggers
//! The refresh is not run on the `hub_feedback` request path. It runs:
//! - synchronously from `wiki_lint(include_skill_quality=true)`;
//! - in the background after a skill's content changes (`hub_register`);
//! - in the background, throttled per scope, after recorded feedback, so the
//!   time-based stale-skill archive stays reachable without a lint run.
//!
//! Background requests are coalesced by [`SkillQualityRefreshQueue`]: at most
//! one worker runs per server and each scope is refreshed at most once per
//! pending request, however many requests arrive while it runs.

use super::similarity::SimilarityCorpus;
use super::*;
use memcore::db::HubDefinitionUpdate;
use std::sync::Mutex as StdMutex;
use std::time::{Duration as StdQueueDuration, Instant};

const SKILL_QUALITY_PAIRWISE_CAP: usize = 500;
const SKILL_MERGE_HINT_THRESHOLD: f64 = 0.92;
/// Minimum spacing between feedback-triggered background refreshes of one
/// scope. Feedback does not change any quality input except activity order
/// and the 30-day stale-archive clock, so a coarse cadence is enough.
const FEEDBACK_REFRESH_MIN_INTERVAL: StdQueueDuration = StdQueueDuration::from_secs(15 * 60);

fn extract_skill_content(cap: &HubCapability) -> Option<String> {
    let def: Value = serde_json::from_str(&cap.definition).ok()?;
    def.get("content")
        .and_then(|v| v.as_str())
        .or_else(|| def.get("prompt").and_then(|v| v.as_str()))
        .map(|s| s.to_string())
}

fn extract_skill_path(cap: &HubCapability) -> Option<String> {
    let def: Value = serde_json::from_str(&cap.definition).ok()?;
    def.get("skill_path")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

fn set_skill_quality_metadata(def: &mut Value, patch: Value) {
    if !def.is_object() {
        *def = json!({});
    }
    let Some(obj) = def.as_object_mut() else {
        return;
    };
    let quality = obj.entry("quality_guard").or_insert_with(|| json!({}));
    if !quality.is_object() {
        *quality = json!({});
    }
    if let (Some(target), Some(source)) = (quality.as_object_mut(), patch.as_object()) {
        for (key, value) in source {
            target.insert(key.clone(), value.clone());
        }
    }
}

fn latest_snapshot_for_skill(store: &mut MemoryStore, skill_path: &str) -> Option<MemoryEntry> {
    let root = format!("{}/distilled", skill_path.trim_end_matches('/'));
    store
        .list_by_path(&root, 50, false)
        .ok()?
        .into_iter()
        .max_by(|a, b| a.timestamp.cmp(&b.timestamp))
}

fn skill_activity_timestamp(cap: &HubCapability) -> Option<DateTime<Utc>> {
    cap.last_used
        .as_deref()
        .and_then(parse_rfc3339_utc)
        .or_else(|| parse_rfc3339_utc(&cap.updated_at))
}

fn should_archive_skill(cap: &HubCapability, now: DateTime<Utc>) -> bool {
    let stale_cutoff = now - ChronoDuration::days(30);
    cap.avg_rating < 0.3
        && cap
            .last_used
            .as_deref()
            .and_then(parse_rfc3339_utc)
            .map(|ts| ts < stale_cutoff)
            .unwrap_or(false)
}

/// Float equality that tolerates the last-ulp drift of serde_json's default
/// (non-`float_roundtrip`) float parser when comparing a freshly computed
/// value against one read back from a stored definition.
fn quality_floats_eq(left: f64, right: f64) -> bool {
    left == right || (left - right).abs() <= 1e-12 * left.abs().max(right.abs()).max(1.0)
}

fn quality_values_eq(left: &Value, right: &Value) -> bool {
    match (left, right) {
        (Value::Number(l), Value::Number(r)) => match (l.as_f64(), r.as_f64()) {
            (Some(l), Some(r)) => quality_floats_eq(l, r),
            _ => l == r,
        },
        (Value::Array(l), Value::Array(r)) => {
            l.len() == r.len() && l.iter().zip(r).all(|(l, r)| quality_values_eq(l, r))
        }
        (Value::Object(l), Value::Object(r)) => {
            l.len() == r.len()
                && l.iter()
                    .all(|(key, l)| r.get(key).is_some_and(|r| quality_values_eq(l, r)))
        }
        _ => left == right,
    }
}

/// The substantive quality result for one skill.
struct SkillQualityResult<'a> {
    merge_hints: &'a [Value],
    pagerank: f64,
    archive: bool,
}

/// Returns the definition with `result` applied, or `None` when the stored
/// definition already carries the same substantive result. Only
/// `quality_guard` (and `policy.visibility` when archiving) are touched;
/// `quality_guard.updated_at` / `archived_at` are stamped only when the
/// corresponding result actually changes.
fn next_skill_quality_definition(
    current_definition: &str,
    result: &SkillQualityResult<'_>,
    now: &str,
) -> Result<Option<String>, String> {
    let mut def: Value = serde_json::from_str(current_definition).unwrap_or_else(|_| json!({}));
    let quality = def.get("quality_guard");
    let merge_hints = Value::Array(result.merge_hints.to_vec());
    let hints_unchanged = quality
        .and_then(|q| q.get("merge_hints"))
        .is_some_and(|stored| quality_values_eq(stored, &merge_hints));
    let pagerank_unchanged = quality
        .and_then(|q| q.get("pagerank"))
        .and_then(Value::as_f64)
        .is_some_and(|stored| quality_floats_eq(stored, result.pagerank));
    let already_archived = quality
        .and_then(|q| q.get("status"))
        .and_then(Value::as_str)
        == Some("archived")
        && quality
            .and_then(|q| q.get("archived_reason"))
            .and_then(Value::as_str)
            == Some("stale_low_rating")
        && def
            .get("policy")
            .and_then(|p| p.get("visibility"))
            .and_then(Value::as_str)
            == Some("hidden");
    let archive_needed = result.archive && !already_archived;
    if hints_unchanged && pagerank_unchanged && !archive_needed {
        return Ok(None);
    }

    set_skill_quality_metadata(
        &mut def,
        json!({
            "merge_hints": merge_hints,
            "pagerank": result.pagerank,
            "updated_at": now,
        }),
    );
    if archive_needed {
        if let Some(obj) = def.as_object_mut() {
            let policy = obj.entry("policy").or_insert_with(|| json!({}));
            if !policy.is_object() {
                *policy = json!({});
            }
            if let Some(policy_obj) = policy.as_object_mut() {
                policy_obj.insert("visibility".to_string(), json!("hidden"));
            }
        }
        set_skill_quality_metadata(
            &mut def,
            json!({
                "status": "archived",
                "archived_reason": "stale_low_rating",
                "archived_at": now,
            }),
        );
    }
    serde_json::to_string(&def)
        .map(Some)
        .map_err(|e| format!("serialize skill quality def: {e}"))
}

#[derive(Clone)]
struct SkillQualitySnapshot {
    cap: HubCapability,
    content: String,
    skill_path: Option<String>,
    latest_snapshot: Option<MemoryEntry>,
}

/// Everything the write phase needs; computed from one read snapshot.
struct SkillQualityPlan {
    scope: DbScope,
    snapshots: Vec<SkillQualitySnapshot>,
    merge_map: HashMap<String, Vec<Value>>,
    graph_edges: Vec<memcore::MemoryEdge>,
    pagerank: HashMap<String, f64>,
    pairwise_evaluated_skills: usize,
    pairwise_skipped_skills: usize,
    now: DateTime<Utc>,
}

fn compute_skill_quality_plan(
    server: &MemoryServer,
    scope: DbScope,
) -> Result<SkillQualityPlan, String> {
    let mut snapshots: Vec<SkillQualitySnapshot> =
        server.with_store_for_scope_read(scope, |store| {
            let caps = store
                .hub_list(Some("skill"), false)
                .map_err(|e| format!("hub list skills: {e}"))?;
            let mut out = Vec::new();
            for cap in caps {
                if crate::builtins::is_retired_builtin_capability_id(&cap.id) {
                    continue;
                }
                let Some(content) = extract_skill_content(&cap) else {
                    continue;
                };
                let skill_path = extract_skill_path(&cap);
                let latest_snapshot = skill_path
                    .as_deref()
                    .and_then(|path| latest_snapshot_for_skill(store, path));
                out.push(SkillQualitySnapshot {
                    cap,
                    content,
                    skill_path,
                    latest_snapshot,
                });
            }
            Ok(out)
        })?;
    snapshots.sort_by(|a, b| {
        skill_activity_timestamp(&b.cap)
            .cmp(&skill_activity_timestamp(&a.cap))
            .then_with(|| b.cap.uses.cmp(&a.cap.uses))
            .then_with(|| a.cap.id.cmp(&b.cap.id))
    });

    let now = Utc::now();
    let mut merge_map: HashMap<String, Vec<Value>> = HashMap::new();
    let mut graph_edges = Vec::<memcore::MemoryEdge>::new();
    let pairwise_evaluated_skills = snapshots.len().min(SKILL_QUALITY_PAIRWISE_CAP);
    let pairwise_skipped_skills = snapshots.len().saturating_sub(pairwise_evaluated_skills);
    let snapshots_for_pairwise = &snapshots[..pairwise_evaluated_skills];

    // Tokenize each skill once; the pairwise pass is then integer dot
    // products. Same similarity values and pair order as before.
    let mut corpus = SimilarityCorpus::new(
        snapshots_for_pairwise
            .iter()
            .map(|snapshot| snapshot.content.as_str()),
    );
    for i in 0..corpus.len() {
        corpus.for_each_later_pair(i, |j, similarity| {
            if similarity > SKILL_MERGE_HINT_THRESHOLD {
                merge_map
                    .entry(snapshots_for_pairwise[i].cap.id.clone())
                    .or_default()
                    .push(json!({
                        "skill_id": snapshots_for_pairwise[j].cap.id,
                        "similarity": similarity,
                    }));
                merge_map
                    .entry(snapshots_for_pairwise[j].cap.id.clone())
                    .or_default()
                    .push(json!({
                        "skill_id": snapshots_for_pairwise[i].cap.id,
                        "similarity": similarity,
                    }));

                if let (Some(left), Some(right)) = (
                    snapshots_for_pairwise[i].latest_snapshot.as_ref(),
                    snapshots_for_pairwise[j].latest_snapshot.as_ref(),
                ) {
                    graph_edges.push(memcore::MemoryEdge {
                        source_id: left.id.clone(),
                        target_id: right.id.clone(),
                        relation: "merge_hint".to_string(),
                        weight: similarity.clamp(0.0, 1.0),
                        metadata: json!({
                            "source": "skill_quality_guard",
                            "type": "merge_hint",
                            "similarity": similarity,
                        }),
                        created_at: now.to_rfc3339(),
                        valid_from: String::new(),
                        valid_to: None,
                    });
                }
            }
        });
    }

    let pagerank = local_pagerank(&graph_edges, 0.85);
    Ok(SkillQualityPlan {
        scope,
        snapshots,
        merge_map,
        graph_edges,
        pagerank,
        pairwise_evaluated_skills,
        pairwise_skipped_skills,
        now,
    })
}

fn apply_skill_quality_plan(
    server: &MemoryServer,
    plan: SkillQualityPlan,
) -> Result<Value, String> {
    let SkillQualityPlan {
        scope,
        snapshots,
        merge_map,
        graph_edges,
        pagerank,
        pairwise_evaluated_skills,
        pairwise_skipped_skills,
        now,
    } = plan;

    if !graph_edges.is_empty() {
        let _ = server.with_store_for_scope(scope, |store| {
            for edge in &graph_edges {
                // tachi#1646: `merge_hint` edges are a token-similarity
                // heuristic Tachi computed itself.
                store
                    .add_edge_with_provenance(
                        edge,
                        &memcore::db::EdgeProvenance {
                            authority: Some(memcore::db::EdgeAuthority::DerivedHeuristic),
                            ..Default::default()
                        },
                    )
                    .map_err(|e| format!("skill graph edge: {e}"))?;
            }
            Ok(())
        });
    }

    let now_rfc3339 = now.to_rfc3339();
    let mut archived_skills = Vec::<String>::new();
    let mut changed_caps = Vec::<HubCapability>::new();
    let mut skipped_conflicts = Vec::<String>::new();

    server.with_store_for_scope(scope, |store| {
        for snapshot in &snapshots {
            let merge_hints = merge_map
                .get(&snapshot.cap.id)
                .map(Vec::as_slice)
                .unwrap_or_default();
            let pagerank_score = snapshot
                .latest_snapshot
                .as_ref()
                .and_then(|memory| pagerank.get(&memory.id).copied())
                .unwrap_or(0.0);
            let mut conflict = false;
            let mut archive = false;
            let mut build_error = None;
            let outcome = store
                .hub_update_definition_with(&snapshot.cap.id, |current| {
                    // Compare-and-set on the inputs the result was computed
                    // from. Other definition fields may have moved; the
                    // patch is applied to the current definition.
                    if extract_skill_content(current).as_deref() != Some(snapshot.content.as_str())
                        || extract_skill_path(current) != snapshot.skill_path
                    {
                        conflict = true;
                        return None;
                    }
                    // Archive on the current counters, not the snapshot's.
                    archive = should_archive_skill(current, now);
                    let result = SkillQualityResult {
                        merge_hints,
                        pagerank: pagerank_score,
                        archive,
                    };
                    match next_skill_quality_definition(&current.definition, &result, &now_rfc3339)
                    {
                        Ok(next) => next,
                        Err(error) => {
                            build_error = Some(error);
                            None
                        }
                    }
                })
                .map_err(|e| format!("hub update skill quality definition: {e}"))?;
            if let Some(error) = build_error {
                return Err(error);
            }
            if conflict {
                skipped_conflicts.push(snapshot.cap.id.clone());
                continue;
            }
            if archive {
                archived_skills.push(snapshot.cap.id.clone());
            }
            if let HubDefinitionUpdate::Updated(cap) = outcome {
                changed_caps.push(*cap);
            }
        }
        Ok(())
    })?;

    for cap in &changed_caps {
        if capability_callable(cap) && should_expose_skill_tool(cap) {
            let _ = server.register_skill_tool(cap);
        } else {
            let _ = server.unregister_skill_tool(&cap.id);
        }
    }

    Ok(json!({
        "scope": scope.as_str(),
        "archived_skills": archived_skills,
        "merge_hints": merge_map,
        "pairwise_cap": SKILL_QUALITY_PAIRWISE_CAP,
        "pairwise_evaluated_skills": pairwise_evaluated_skills,
        "pairwise_skipped_skills": pairwise_skipped_skills,
        "pagerank": pagerank,
        "updated_caps": changed_caps.iter().map(|cap| cap.id.clone()).collect::<Vec<_>>(),
        "skipped_conflicts": skipped_conflicts,
    }))
}

fn run_skill_quality_guards_for_scope(
    server: &MemoryServer,
    scope: DbScope,
) -> Result<Value, String> {
    let plan = compute_skill_quality_plan(server, scope)?;
    apply_skill_quality_plan(server, plan)
}

/// Test seam: runs one scope's refresh with `between` executed after the
/// snapshot/pairwise phase and before any write, to exercise interleavings.
#[cfg(test)]
pub(crate) fn refresh_skill_quality_scope_interleaved_for_test(
    server: &MemoryServer,
    scope: DbScope,
    between: impl FnOnce(),
) -> Result<Value, String> {
    let plan = compute_skill_quality_plan(server, scope)?;
    between();
    apply_skill_quality_plan(server, plan)
}

pub(crate) fn refresh_skill_quality_guards(server: &MemoryServer) -> Result<Value, String> {
    let global = run_skill_quality_guards_for_scope(server, DbScope::Global)?;
    let project = if server.has_project_db() {
        Some(run_skill_quality_guards_for_scope(
            server,
            DbScope::Project,
        )?)
    } else {
        None
    };
    Ok(json!({"global": global, "project": project}))
}

/// Why a background skill-quality refresh is requested.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SkillQualityRefreshReason {
    /// A skill's definition/content changed. Always scheduled (coalesced).
    ContentChanged,
    /// Feedback was recorded. Throttled per scope by
    /// [`FEEDBACK_REFRESH_MIN_INTERVAL`].
    Feedback,
}

#[derive(Default)]
struct RefreshQueueState {
    /// Indexed by [`scope_slot`].
    pending: [bool; 2],
    last_feedback_request: [Option<Instant>; 2],
    worker_running: bool,
}

/// Per-server coalescing queue for background skill-quality refreshes.
pub(crate) struct SkillQualityRefreshQueue {
    /// Whether requests may start a background worker. False for embedded
    /// MCP facades and unit tests (owner duties stay with the daemon); the
    /// request is then only recorded as pending.
    spawn_worker: bool,
    state: StdMutex<RefreshQueueState>,
}

fn scope_slot(scope: DbScope) -> usize {
    match scope {
        DbScope::Global => 0,
        DbScope::Project => 1,
    }
}

impl SkillQualityRefreshQueue {
    pub(crate) fn new(spawn_worker: bool) -> Self {
        Self {
            spawn_worker,
            state: StdMutex::new(RefreshQueueState::default()),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, RefreshQueueState> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Records a request. Returns true when `scope` is now pending (newly or
    /// already), false when a feedback request was throttled.
    fn record(&self, scope: DbScope, reason: SkillQualityRefreshReason, now: Instant) -> bool {
        let slot = scope_slot(scope);
        let mut state = self.lock();
        if state.pending[slot] {
            return true;
        }
        if reason == SkillQualityRefreshReason::Feedback {
            if state.last_feedback_request[slot]
                .is_some_and(|last| now.duration_since(last) < FEEDBACK_REFRESH_MIN_INTERVAL)
            {
                return false;
            }
            state.last_feedback_request[slot] = Some(now);
        }
        state.pending[slot] = true;
        true
    }

    /// Claims the worker slot if a worker may start and none is running.
    fn try_claim_worker(&self) -> bool {
        if !self.spawn_worker {
            return false;
        }
        let mut state = self.lock();
        if state.worker_running || !state.pending.iter().any(|pending| *pending) {
            return false;
        }
        state.worker_running = true;
        true
    }

    /// Takes the next pending scope, or releases the worker slot when none
    /// is left (under the same lock, so a concurrent request either is seen
    /// here or claims a new worker).
    fn next_pending(&self) -> Option<DbScope> {
        let mut state = self.lock();
        for scope in [DbScope::Global, DbScope::Project] {
            let slot = scope_slot(scope);
            if state.pending[slot] {
                state.pending[slot] = false;
                return Some(scope);
            }
        }
        state.worker_running = false;
        None
    }

    fn release_worker(&self) {
        self.lock().worker_running = false;
    }

    #[cfg(test)]
    pub(crate) fn pending_scopes(&self) -> Vec<DbScope> {
        let state = self.lock();
        [DbScope::Global, DbScope::Project]
            .into_iter()
            .filter(|scope| state.pending[scope_slot(*scope)])
            .collect()
    }
}

/// Releases the worker slot if the worker unwinds mid-refresh.
struct WorkerSlotGuard<'a>(&'a SkillQualityRefreshQueue);

impl Drop for WorkerSlotGuard<'_> {
    fn drop(&mut self) {
        if std::thread::panicking() {
            self.0.release_worker();
        }
    }
}

impl MemoryServer {
    /// Requests a deferred skill-quality refresh of `scope` without doing
    /// the work on the caller's path. See the module docs for triggers.
    pub(crate) fn request_skill_quality_refresh(
        &self,
        scope: DbScope,
        reason: SkillQualityRefreshReason,
    ) {
        let queue = &self.skill_quality_refresh;
        if !queue.record(scope, reason, Instant::now()) {
            return;
        }
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        if !queue.try_claim_worker() {
            return;
        }
        let server = self.clone();
        runtime.spawn_blocking(move || server.run_pending_skill_quality_refreshes());
    }

    /// Drains pending refresh requests on the current thread. This is the
    /// background worker body; tests call it directly.
    pub(crate) fn run_pending_skill_quality_refreshes(&self) -> Vec<Result<Value, String>> {
        let queue = &self.skill_quality_refresh;
        let _guard = WorkerSlotGuard(queue);
        let mut results = Vec::new();
        while let Some(scope) = queue.next_pending() {
            if scope == DbScope::Project && !self.has_project_db() {
                continue;
            }
            let result = run_skill_quality_guards_for_scope(self, scope);
            if let Err(error) = &result {
                tracing::warn!(
                    scope = scope.as_str(),
                    error = %error,
                    "background skill quality refresh failed"
                );
            }
            results.push(result);
        }
        results
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stored(definition: Value) -> String {
        serde_json::to_string(&definition).unwrap()
    }

    #[test]
    fn unchanged_quality_result_is_not_rewritten() {
        let hints = vec![json!({"skill_id": "skill:b", "similarity": 0.9312345678901234})];
        let result = SkillQualityResult {
            merge_hints: &hints,
            pagerank: 0.15,
            archive: false,
        };
        let first = next_skill_quality_definition(
            &stored(json!({"content": "x", "policy": {"visibility": "listed"}})),
            &result,
            "2026-01-01T00:00:00+00:00",
        )
        .unwrap()
        .expect("first refresh writes quality_guard");
        assert_eq!(
            next_skill_quality_definition(&first, &result, "2026-02-01T00:00:00+00:00").unwrap(),
            None,
            "same result must not rewrite or re-stamp updated_at"
        );

        let archived_result = SkillQualityResult {
            archive: true,
            ..result
        };
        let archived =
            next_skill_quality_definition(&first, &archived_result, "2026-03-01T00:00:00+00:00")
                .unwrap()
                .expect("archiving is a substantive change");
        let archived_def: Value = serde_json::from_str(&archived).unwrap();
        assert_eq!(archived_def["policy"]["visibility"], "hidden");
        assert_eq!(archived_def["quality_guard"]["status"], "archived");
        assert_eq!(
            archived_def["quality_guard"]["archived_at"],
            "2026-03-01T00:00:00+00:00"
        );
        assert_eq!(
            next_skill_quality_definition(&archived, &archived_result, "2026-04-01T00:00:00+00:00")
                .unwrap(),
            None,
            "an already archived skill keeps its archived_at"
        );

        let changed_hints = vec![];
        let changed = next_skill_quality_definition(
            &archived,
            &SkillQualityResult {
                merge_hints: &changed_hints,
                ..archived_result
            },
            "2026-05-01T00:00:00+00:00",
        )
        .unwrap()
        .expect("changed merge hints are rewritten");
        let changed_def: Value = serde_json::from_str(&changed).unwrap();
        assert_eq!(changed_def["quality_guard"]["merge_hints"], json!([]));
        assert_eq!(
            changed_def["quality_guard"]["updated_at"],
            "2026-05-01T00:00:00+00:00"
        );
        assert_eq!(
            changed_def["quality_guard"]["archived_at"],
            "2026-03-01T00:00:00+00:00"
        );
    }

    #[test]
    fn refresh_queue_coalesces_and_throttles_feedback() {
        let queue = SkillQualityRefreshQueue::new(true);
        let start = Instant::now();
        assert!(queue.record(DbScope::Global, SkillQualityRefreshReason::Feedback, start));
        assert!(queue.try_claim_worker());
        assert!(!queue.try_claim_worker(), "one worker at a time");
        assert!(queue.record(
            DbScope::Global,
            SkillQualityRefreshReason::ContentChanged,
            start
        ));
        assert_eq!(queue.next_pending(), Some(DbScope::Global));
        assert_eq!(queue.next_pending(), None, "coalesced into one run");
        assert!(!queue.try_claim_worker(), "nothing pending");

        assert!(
            !queue.record(
                DbScope::Global,
                SkillQualityRefreshReason::Feedback,
                start + StdQueueDuration::from_secs(60)
            ),
            "feedback within the interval is throttled"
        );
        assert!(queue.record(
            DbScope::Project,
            SkillQualityRefreshReason::Feedback,
            start + StdQueueDuration::from_secs(60)
        ));
        assert!(queue.record(
            DbScope::Global,
            SkillQualityRefreshReason::ContentChanged,
            start + StdQueueDuration::from_secs(60)
        ));
        assert_eq!(
            queue.pending_scopes(),
            vec![DbScope::Global, DbScope::Project]
        );
        assert!(queue.record(
            DbScope::Global,
            SkillQualityRefreshReason::Feedback,
            start + FEEDBACK_REFRESH_MIN_INTERVAL
        ));
    }
}
