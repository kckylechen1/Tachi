use memcore::relation_ontology::is_legal_new_relation;
use memcore::{
    AuthorityLevel, MemoryEdge, MemoryEntry, ProjectionKind, TachiEventQuery, TachiEventRecord,
};
use serde_json::{json, Value};

use crate::tool_params::TachiEventParams;
use crate::MemoryServer;
use memory_server_runtime::query_limit;

use super::storage::{
    add_memory_edge, get_projection_memory, read_events, resolve_projection_write_target,
    upsert_projection_memory,
};
use super::{event_query_from_params, target_from_event_params, ContinuityEventTarget};

mod entry;

use self::entry::{
    build_projection_entry, counter_i64, event_projections, projection_event_domain,
    projection_key, projection_memory_id,
};
pub(super) use self::entry::{projected_path_prefix, projection_filters, projection_kind_metadata};

const NON_PROJECTABLE_PATTERN_EVIDENCE_ADAPTER: &str = "tachi.pattern_evidence.v1";

pub(crate) fn project_continuity_events(
    server: &MemoryServer,
    params: &TachiEventParams,
) -> Result<Value, String> {
    let target = target_from_event_params(server, params);
    let query = event_query_from_params(params);
    let filters = projection_filters(&params.projection_hints)?;
    // #1114: `params.project.is_some()` alone is not proof of a caller
    // decision — bound stdio/HTTP sessions inject the session's bound
    // project onto every project-defaulting write tool, same as
    // `SaveMemoryParams` pre-#1041-F2. `project_explicit` (stamped by
    // `session_identity::enforce_session_project`) distinguishes the two;
    // see `write_affinity`'s module doc.
    let project_explicit = params.project.is_some() && params.project_explicit;
    project_continuity_events_inner(
        server,
        &target,
        query,
        filters,
        params.dry_run,
        false,
        project_explicit,
    )
}

pub(crate) fn project_auto_continuity_events_for_target(
    server: &MemoryServer,
    target: ContinuityEventTarget,
    limit: usize,
) -> Result<Value, String> {
    let query = TachiEventQuery {
        limit: query_limit(limit),
        ..TachiEventQuery::default()
    };
    // #1114: every target this caller (the background
    // `ContinuityProjectionScheduler` sweep) builds is a `db_path`-pinned
    // visit to one specific manifest DB — `upsert_projection_memory`'s own
    // `db_path.is_some()` check skips the write-affinity gate for these
    // regardless of this flag, so `project_explicit` here is inert by
    // construction, not a live decision.
    project_continuity_events_inner(server, &target, query, Vec::new(), false, true, true)
}

/// Compute the ordinary auto-projection receipt over an in-memory event
/// overlay. `dry_run=true` reaches the real routing, merge, counter, tier,
/// graph, and promotion logic without persisting a projection or edge.
pub(super) fn preview_auto_projection_with_events(
    server: &MemoryServer,
    target: &ContinuityEventTarget,
    limit: usize,
    synthetic_events: Vec<TachiEventRecord>,
) -> Result<Value, String> {
    let query = TachiEventQuery {
        limit: query_limit(limit),
        ..TachiEventQuery::default()
    };
    let mut events = synthetic_events;
    events.extend(read_events(server, target, &query)?);
    events.truncate(query_limit(limit));
    project_continuity_event_batch(server, target, events, Vec::new(), true, true, true)
}

fn auto_projectable_event(event: &TachiEventRecord) -> bool {
    if event_projections(event).is_empty() {
        return false;
    }
    if matches!(
        event.authority,
        AuthorityLevel::Blocker | AuthorityLevel::ExecutionGate
    ) {
        return false;
    }

    let event_type = event.event_type.trim().to_ascii_lowercase();
    matches!(
        event.authority,
        AuthorityLevel::CollectOnly
            | AuthorityLevel::RawFact
            | AuthorityLevel::ReviewSignalOnly
            | AuthorityLevel::ToneAndReminderOnly
            | AuthorityLevel::Advisory
    ) || matches!(
        event_type.as_str(),
        "memory.saved"
            | "wiki.saved"
            | "session.captured"
            | "task.outcome"
            | "subagent.evaluated"
            | "session.outcome"
    )
}

fn project_continuity_events_inner(
    server: &MemoryServer,
    target: &ContinuityEventTarget,
    query: TachiEventQuery,
    filters: Vec<ProjectionKind>,
    dry_run: bool,
    auto_only: bool,
    project_explicit: bool,
) -> Result<Value, String> {
    let events = read_events(server, target, &query)?;
    project_continuity_event_batch(
        server,
        target,
        events,
        filters,
        dry_run,
        auto_only,
        project_explicit,
    )
}

fn project_continuity_event_batch(
    server: &MemoryServer,
    target: &ContinuityEventTarget,
    events: Vec<TachiEventRecord>,
    filters: Vec<ProjectionKind>,
    dry_run: bool,
    auto_only: bool,
    project_explicit: bool,
) -> Result<Value, String> {
    let mut projected = Vec::new();
    let mut skipped = Vec::new();
    let mut errors = Vec::new();
    let mut promotion_candidates = Vec::new();

    for event in events {
        if event.adapter == NON_PROJECTABLE_PATTERN_EVIDENCE_ADAPTER {
            skipped.push(json!({
                "event_id": event.id,
                "event_type": event.event_type,
                "reason": "append-only pattern evidence is not projectable",
            }));
            continue;
        }
        if auto_only && !auto_projectable_event(&event) {
            skipped.push(json!({
                "event_id": event.id,
                "event_type": event.event_type,
                "reason": "not auto-projectable",
            }));
            continue;
        }
        let projections = event_projections(&event);
        if projections.is_empty() {
            skipped.push(json!({
                "event_id": event.id,
                "reason": "no projection hint or inferable event type",
            }));
            continue;
        }
        for projection in projections {
            if !filters.is_empty() && !filters.contains(&projection) {
                continue;
            }
            let key = projection_key(&event, projection);
            let memory_id = projection_memory_id(projection, &key);

            // #1114 (codex round-1 B3 fix): resolve ONE routed destination
            // for this projection BEFORE any existing-row lookup, write, or
            // graph-edge work — `projection_event_domain` computes the SAME
            // domain `build_projection_entry` would (without building the
            // rest of the entry, which needs `existing` to merge correctly).
            // Row lookup, row write, and this projection's timeline graph
            // edges all use `routed_target`, never the pre-gate `target`:
            // before this split, a rerouted projection's SECOND run looked
            // up "does this exist" at the stale pre-gate store, found
            // nothing, and silently reset its aggregation counters instead
            // of updating the row that had actually moved — and
            // self-referencing timeline edges got dropped as "endpoint
            // missing" for the same reason.
            let domain = projection_event_domain(&event, projection, &key);
            // #1114 (codex round-2 item 4 fix): does this row already exist
            // at the PRE-gate target? If a prior run placed it there (e.g.
            // before this domain had a registered route), a LATER route
            // registration must not reroute the SAME deterministic id to a
            // different store and split it across two — it must update the
            // row that's already there. `?` propagates a genuine read
            // failure as a real error instead of the `.ok().flatten()`
            // idiom this used to use, which conflated "the store is
            // unreadable right now" with "this id doesn't exist" — a false
            // negative here is exactly as dangerous as the bug this check
            // exists to prevent (it would proceed to create a fresh,
            // duplicate row instead of updating the one that's there).
            //
            // KNOWN LIMITATION (deferred, not solved here): this only
            // catches the row sitting at the PRE-gate default. If the
            // registry routes this domain to store A on one run and later
            // (a SECOND registry edit) to a DIFFERENT store B, neither the
            // pre-gate check here nor the post-gate check below (against
            // whatever B resolves to) ever look at A — the row already at A
            // is invisible to both, and a second, independent copy gets
            // created at B. Closing that fully requires persistent per-id
            // "last known location" tracking (a schema-level change), the
            // same shape of gap #1115's check-then-insert atomicity work is
            // already scoped to address — deliberately not solved with an
            // ad-hoc migration in this leaf.
            let id_resolves_at_pretarget =
                get_projection_memory(server, target, &memory_id)?.is_some();
            let routed_target = match resolve_projection_write_target(
                server,
                target,
                domain.as_deref(),
                project_explicit,
                id_resolves_at_pretarget,
            ) {
                Ok(routed) => routed,
                Err(error) => {
                    // #1114 (codex round-2 item 3 point ①): a write-affinity
                    // refusal is a loud, TYPED failure ("拒必有声") — never
                    // silently absorbed into an overall `Ok({"status":
                    // "partial"})`. A live, explicit `action=project` call
                    // (auto_only == false) hard-aborts: the caller asked for
                    // this materialization and deserves a real error
                    // response, not a 200 they have to inspect an `errors`
                    // array to notice. The background auto-sweep
                    // (auto_only == true) still soft-continues to the next
                    // row for an ordinary per-domain refusal (aborting the
                    // WHOLE sweep over one misconfigured route is a worse
                    // operational outcome than skipping that one row) — but
                    // `RoutingConfigUnavailable` hard-aborts even there,
                    // because a broken config fails EVERY remaining row in
                    // this same batch identically; continuing serves no
                    // purpose. The JSON error entry (when soft-continuing)
                    // carries `error.kind()` — a stable, typed discriminant
                    // — not just the stringified `Display` message, so a
                    // refusal stays distinguishable from an ordinary
                    // persistence error downstream.
                    tracing::warn!(
                        event_id = %event.id,
                        projection = projection.as_str(),
                        kind = error.kind(),
                        error = %error,
                        "continuity projection: write-affinity gate refused this row"
                    );
                    let hard_abort = !auto_only
                        || matches!(
                            error,
                            crate::memory_search_ops::save_memory::write_affinity::WriteAffinityError::RoutingConfigUnavailable(_)
                        );
                    if hard_abort {
                        return Err(format!(
                            "continuity projection refused (event {}, projection {}): {error}",
                            event.id,
                            projection.as_str()
                        ));
                    }
                    errors.push(json!({
                        "event_id": event.id,
                        "projection": projection.as_str(),
                        "memory_id": memory_id,
                        "error_kind": error.kind(),
                        "error": error.to_string(),
                        "reason": "write_affinity_refused",
                    }));
                    continue;
                }
            };

            let existing = get_projection_memory(server, &routed_target, &memory_id)?;
            let (entry, already_projected) = build_projection_entry(existing, &event, projection);
            if !dry_run {
                if let Err(error) = upsert_projection_memory(server, &routed_target, &entry) {
                    errors.push(json!({
                        "event_id": event.id,
                        "projection": projection.as_str(),
                        "memory_id": entry.id,
                        "error": error,
                    }));
                    continue;
                }
            }
            let graph_edges = if projection == ProjectionKind::Timeline {
                persist_timeline_graph_edges(server, &routed_target, &entry, &event, dry_run)
            } else {
                json!({
                    "saved_count": 0,
                    "skipped_count": 0,
                    "edges": [],
                    "skipped": [],
                })
            };
            if let Some(reason) = projection_promotion_reason(&entry) {
                if !promotion_candidates.iter().any(|candidate: &Value| {
                    candidate.get("memory_id").and_then(Value::as_str) == Some(entry.id.as_str())
                }) {
                    promotion_candidates.push(json!({
                        "memory_id": entry.id,
                        "projection": projection.as_str(),
                        "path": entry.path,
                        "summary": entry.summary,
                        "tier": entry.tier,
                        "reason": reason,
                        "counters": entry.metadata.get("counters").cloned().unwrap_or_else(|| json!({})),
                        "review_artifacts": maturity_review_artifacts(&entry, reason),
                    }));
                }
            }
            projected.push(json!({
                "event_id": event.id,
                "event_type": event.event_type,
                "projection": projection.as_str(),
                "memory_id": entry.id,
                "path": entry.path,
                "summary": entry.summary,
                "tier": entry.tier,
                "already_projected": already_projected,
                "graph_edges": graph_edges,
                "dry_run": dry_run,
            }));
        }
    }

    Ok(json!({
        "status": if errors.is_empty() { "completed" } else { "partial" },
        "dry_run": dry_run,
        "auto_only": auto_only,
        "projected_count": projected.len(),
        "skipped_count": skipped.len(),
        "promotion_candidate_count": promotion_candidates.len(),
        "promotion_candidates": promotion_candidates,
        "error_count": errors.len(),
        "projections": projected,
        "skipped": skipped,
        "errors": errors,
    }))
}

fn nested_event_payload(event: &TachiEventRecord) -> &Value {
    event.payload.get("candidate").unwrap_or(&event.payload)
}

fn edge_endpoint(raw: &Value, keys: &[&str], projection_id: &str) -> Option<String> {
    keys.iter()
        .find_map(|key| raw.get(*key).and_then(Value::as_str))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| {
            if matches!(value, "self" | "$self" | "projection" | "$projection") {
                projection_id.to_string()
            } else {
                value.to_string()
            }
        })
}

/// tachi#1646 offender 1 (#1460 measurement): before this leaf, a payload
/// that omitted `weight` defaulted to `1.0`, and one that supplied an
/// explicit weight was bound only by the write-time `[0.0, 1.0]` clamp
/// (`memcore::db::graph::clamp_edge_weight`) — either way an untrusted
/// caller could reach the ceiling. `0.6` is a blanket cap, not just a new
/// default: it applies whether the payload omits `weight` or asserts one.
/// It sits below the product of a full-trust weight and every relation
/// multiplier a `CollectOnly`-tier event can reach post-restriction
/// (`follows`/`references` at 0.70, `scorer::graph_relation_activation_weight`
/// @ scorer/graph.rs:77: `0.6 * 0.70 = 0.42`) and, for the higher-authority
/// tiers this function also serves, below every named relation multiplier
/// up to and including `supports` at 0.90 (scorer/graph.rs:73) times a
/// full-trust weight of 1.0 — a caller-asserted edge can no longer reach the
/// ceiling a `ModelReceiptBacked` or `StructuralBookkeeping` writer could.
const CALLER_ASSERTED_WEIGHT_CAP: f64 = 0.6;

/// tachi#1646 offender 1 (#1460 measurement, "auto-projects even
/// `CollectOnly`-tier events"): `auto_projectable_event` admits
/// `AuthorityLevel::CollectOnly` — the lowest-trust tier — into projection
/// unconditionally, yet before this leaf that tier's `causal_edges` payload
/// could still assert any of the 14 ontology-v1 relations, including
/// `supports` at activation weight 0.90 (scorer/graph.rs:73). Restricting
/// the writable relation SET (this list) rather than adding a second,
/// tier-specific weight cap is the smaller honest change here: the general
/// `CALLER_ASSERTED_WEIGHT_CAP` above already bounds every continuity edge's
/// weight regardless of tier, so the only gap left for `CollectOnly`
/// specifically is which relation multiplier it can reach — closing that
/// needs one restriction, not a second cap dimension.
const COLLECT_ONLY_ALLOWED_RELATIONS: [&str; 2] = ["follows", "references"];

/// Remap an out-of-allowlist relation to the lowest-multiplier legal
/// substitute for `CollectOnly`-tier events. Applied by authority tier, not
/// by whether this call came from the background auto-sweep or an explicit
/// `action=project` request: a `CollectOnly` event's `causal_edges` payload
/// is exactly as untrusted either way, so gating on `auto_only` as well
/// would add a second condition without closing any additional exposure —
/// the tier alone is the correct and simpler predicate.
///
/// The remap is gated on [`is_legal_new_relation`] — "would this relation
/// validate for a generic writer via the `add_edge` choke point" — not on
/// mere allowlist-membership. Only ontology-legal-but-not-`CollectOnly`
/// relations (e.g. `supports`, `causes`, `elaborates`) get downgraded to
/// `references`. Relations that are illegal on the generic path — the #772
/// `COMPONENT_GOVERNANCE_GRANDFATHERED` set (`owns`/`consumes`/
/// `backflow_candidate`/`blocked_by`, caller-scoped to the typed
/// `add_component_governance_edge` door) and any unknown string — must NOT
/// be remapped: laundering them into a legal `references` edge here would
/// let a dynamic string caller forge a governance relation by relabeling it,
/// bypassing the ontology's generic-path rejection entirely
/// (tachi#1646 kill-test `continuity_projection_rejects_grandfathered_relation_fail_soft`).
/// Leaving them unchanged routes them to `add_memory_edge` /
/// `validate_relation_for_write`, which rejects and fail-soft-drops them —
/// the same outcome as before this remap existed.
fn restrict_relation_for_authority(relation: String, authority: AuthorityLevel) -> String {
    if authority == AuthorityLevel::CollectOnly
        && is_legal_new_relation(&relation)
        && !COLLECT_ONLY_ALLOWED_RELATIONS.contains(&relation.as_str())
    {
        "references".to_string()
    } else {
        relation
    }
}

fn persist_timeline_graph_edges(
    server: &MemoryServer,
    target: &ContinuityEventTarget,
    entry: &MemoryEntry,
    event: &TachiEventRecord,
    dry_run: bool,
) -> Value {
    let payload = nested_event_payload(event);
    let Some(edges) = payload.get("causal_edges").and_then(Value::as_array) else {
        return json!({
            "saved_count": 0,
            "skipped_count": 0,
            "edges": [],
            "skipped": [],
        });
    };
    let mut saved = Vec::new();
    let mut skipped = Vec::new();
    for raw in edges {
        let Some(source_id) =
            edge_endpoint(raw, &["source_id", "from_memory_id", "from_id"], &entry.id)
        else {
            skipped.push(json!({"edge": raw, "reason": "missing source_id"}));
            continue;
        };
        let Some(target_id) =
            edge_endpoint(raw, &["target_id", "to_memory_id", "to_id"], &entry.id)
        else {
            skipped.push(json!({"edge": raw, "reason": "missing target_id"}));
            continue;
        };
        let requested_relation = raw
            .get("relation")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("causes")
            .to_string();
        if requested_relation == "supersedes" {
            skipped.push(json!({
                "edge": raw,
                "source_id": source_id,
                "target_id": target_id,
                "relation": requested_relation,
                "reason": "supersedes is reserved for canonical immutable-supersession claims",
            }));
            continue;
        }
        let relation = restrict_relation_for_authority(requested_relation, event.authority);
        let source_exists = get_projection_memory(server, target, &source_id)
            .ok()
            .flatten()
            .is_some();
        let target_exists = get_projection_memory(server, target, &target_id)
            .ok()
            .flatten()
            .is_some();
        if !source_exists || !target_exists {
            skipped.push(json!({
                "edge": raw,
                "source_id": source_id,
                "target_id": target_id,
                "reason": "endpoint memory missing",
            }));
            continue;
        }
        let edge = MemoryEdge {
            source_id: source_id.clone(),
            target_id: target_id.clone(),
            relation: relation.clone(),
            // tachi#1646: CallerAsserted blanket cap (see
            // CALLER_ASSERTED_WEIGHT_CAP) — applies whether the payload
            // omits weight or asserts one.
            weight: raw
                .get("weight")
                .and_then(Value::as_f64)
                .unwrap_or(CALLER_ASSERTED_WEIGHT_CAP)
                .min(CALLER_ASSERTED_WEIGHT_CAP),
            metadata: json!({
                "source_event_id": event.id,
                "timeline_projection_id": entry.id,
                "raw_edge": raw,
            }),
            created_at: event.created_at.clone(),
            valid_from: raw
                .get("valid_from")
                .and_then(Value::as_str)
                .unwrap_or(event.created_at.as_str())
                .to_string(),
            valid_to: raw
                .get("valid_to")
                .and_then(Value::as_str)
                .map(str::to_string),
        };
        if !dry_run {
            if let Err(error) = add_memory_edge(server, target, &edge) {
                skipped.push(json!({
                    "edge": raw,
                    "source_id": source_id,
                    "target_id": target_id,
                    "relation": relation,
                    "reason": error,
                }));
                continue;
            }
        }
        saved.push(json!({
            "source_id": source_id,
            "target_id": target_id,
            "relation": relation,
            "dry_run": dry_run,
        }));
    }
    json!({
        "saved_count": saved.len(),
        "skipped_count": skipped.len(),
        "edges": saved,
        "skipped": skipped,
    })
}

fn projection_promotion_reason(entry: &MemoryEntry) -> Option<&'static str> {
    let projection = projection_kind_metadata(entry)?;
    if !matches!(projection, "pattern" | "bonding" | "world_book") {
        return None;
    }
    let seen = counter_i64(&entry.metadata, "seen");
    let hit = counter_i64(&entry.metadata, "hit");
    let miss = counter_i64(&entry.metadata, "miss");
    if seen >= 3 && hit > 0 && hit > miss {
        return Some("hit_threshold");
    }
    if entry.tier == "pattern" {
        return Some("reviewed_pattern_tier");
    }
    None
}

fn promotion_slug(entry: &MemoryEntry) -> String {
    let raw = entry
        .metadata
        .get("projection_key")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or(entry.id.as_str());
    let mut out = String::new();
    let mut previous_sep = false;
    for ch in raw.trim().chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            previous_sep = false;
        } else if !previous_sep && !out.is_empty() {
            out.push('-');
            previous_sep = true;
        }
        if out.len() >= 72 {
            break;
        }
    }
    let out = out.trim_matches('-').to_string();
    if out.is_empty() {
        entry.id.clone()
    } else {
        out
    }
}

fn non_empty_array_at<'a>(value: &'a Value, path: &[&str]) -> Option<&'a Vec<Value>> {
    let mut cursor = value;
    for key in path {
        cursor = cursor.get(*key)?;
    }
    cursor.as_array().filter(|items| !items.is_empty())
}

fn bool_at(value: &Value, path: &[&str]) -> bool {
    let mut cursor = value;
    for key in path {
        let Some(next) = cursor.get(*key) else {
            return false;
        };
        cursor = next;
    }
    cursor.as_bool().unwrap_or(false)
}

fn promotion_gate(entry: &MemoryEntry) -> Value {
    let external_validation =
        non_empty_array_at(&entry.metadata, &["timeline", "external_validations"]).is_some()
            || non_empty_array_at(&entry.metadata, &["external_validations"]).is_some()
            || bool_at(&entry.metadata, &["promotion_gate", "external_validation"]);
    let cold_seat_review = bool_at(&entry.metadata, &["promotion_gate", "cold_seat_review"])
        || bool_at(&entry.metadata, &["cold_seat", "reviewed"])
        || non_empty_array_at(&entry.metadata, &["cold_seat", "checks"]).is_some();
    let final_ready = external_validation && cold_seat_review;
    let mut missing = Vec::new();
    if !external_validation {
        missing.push("external_validation");
    }
    if !cold_seat_review {
        missing.push("cold_seat_review");
    }
    json!({
        "final_ready": final_ready,
        "review_required": !final_ready,
        "external_validation": external_validation,
        "cold_seat_review": cold_seat_review,
        "missing": missing,
        "rule": "final promotion requires external validation and cold-seat review",
    })
}

fn maturity_review_artifacts(entry: &MemoryEntry, reason: &str) -> Value {
    let slug = promotion_slug(entry);
    let pattern_ref = crate::continuity_ops::pattern_ref_json(entry);
    let gate = promotion_gate(entry);
    json!({
        "review_required": true,
        "auto_promote": false,
        "gate": gate,
        "wiki_draft": {
            "tool": "tachi_wiki_write",
            "path": format!("/wiki/drafts/patterns/{slug}"),
            "title": format!("Pattern Review: {}", entry.summary),
            "include_patterns": true,
            "pattern_query": entry.metadata.get("projection_key").and_then(Value::as_str).unwrap_or(entry.id.as_str()),
            "review_status": "pending",
            "reason": reason,
            "gate": gate,
            "pattern_ref": pattern_ref,
        },
        "skill_candidate": {
            "tool": "tachi_event",
            "action": "promote",
            "id": entry.id,
            "payload": {
                "pattern_ref": entry.id,
                "skill_id": format!("skill:pattern-{slug}"),
                "name": format!("Pattern: {}", entry.summary),
            },
            "enabled": false,
            "review_status": "pending",
            "gate": gate,
        },
        "agent_profile_proposal": {
            "tool": "tachi_event",
            "action": "promote",
            "id": entry.id,
            "payload": {
                "pattern_ref": pattern_ref,
                "reason": reason,
                "write": false,
            },
            "write": false,
            "review_status": "pending",
        },
    })
}
