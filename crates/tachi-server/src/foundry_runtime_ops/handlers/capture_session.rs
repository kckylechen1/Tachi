use super::super::capture::queue_capture_enrichment;
use super::super::helpers::{
    build_entry_path, build_openclaw_agent_root, dedup_strings, normalize_category, normalize_scope,
};
use super::super::maintenance::enqueue_capture_maintenance_jobs;
use super::super::recall::parse_session_capture_response;
use super::bracket::{extract_bracket_self_evolution_notes, matches_agent_tag};
use super::target::resolve_capture_target;
use crate::server_state::MemoryServer;
use crate::tool_params::CaptureSessionParams;
use crate::DbScope;
use chrono::{Duration, Utc};
use memcore::MemoryEntry;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// #1301 provisional capture policy. The named constant is deliberately kept
/// beside the classifier so archive policy never grows a second flat age.
pub(crate) const CAPTURE_EPHEMERAL_TTL_DAYS: i64 = 30;
pub(crate) const CAPTURE_RETENTION_POLICY_VERSION: &str = "capture-v1";
const CAPTURE_MANIFEST_NAMESPACE: &str = "capture-session-manifest-v1";
const CAPTURE_MANIFEST_STAGING_POLICY: &str = "capture-manifest-staging-v1";
const CAPTURE_MANIFEST_COMPLETED_RETENTION_POLICY: &str = "capture-manifest-receipt-retain-v1";
const CAPTURE_MANIFEST_STAGING_TTL_DAYS: i64 = 30;
/// The fenced section contains only synchronous local SQLite writes, queue
/// inserts, and task spawning. Slow model/embedding awaits happen before it.
const CAPTURE_PROCESSING_LEASE_SECONDS: i64 = 30;

#[cfg(test)]
static CAPTURE_FAILPOINT: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

#[cfg(test)]
fn set_capture_failpoint(stage: &str) {
    let value = match stage {
        "after_manifest_admission" => 1,
        "after_artifact_0" => 2,
        "after_maintenance" => 3,
        "before_enqueue_maintenance" => 4,
        "before_first_renew" => 5,
        _ => panic!("unknown capture failpoint: {stage}"),
    };
    CAPTURE_FAILPOINT.store(value, std::sync::atomic::Ordering::SeqCst);
}

#[cfg(test)]
fn capture_failpoint(stage: &str) -> Result<(), String> {
    let value = match stage {
        "after_manifest_admission" => 1,
        "after_artifact_0" => 2,
        "after_maintenance" => 3,
        "before_enqueue_maintenance" => 4,
        "before_first_renew" => 5,
        _ => return Ok(()),
    };
    if CAPTURE_FAILPOINT
        .compare_exchange(
            value,
            0,
            std::sync::atomic::Ordering::SeqCst,
            std::sync::atomic::Ordering::SeqCst,
        )
        .is_ok()
    {
        return Err(format!("capture_session test failpoint: {stage}"));
    }
    Ok(())
}

/// #1114 (codex round-1 B3 fix): a `MemoryEntry` carried alongside its OWN
/// resolved write-affinity destination — computed once, per entry, before
/// provenance/persist/maintenance/continuity all need to agree on where the
/// row actually lives. A single `capture_session` batch's entries do NOT
/// all necessarily share one destination (a mismatched entry can reroute
/// independently of its siblings), so this is tracked per-entry rather than
/// once for the whole batch.
struct CapturedEntry {
    entry: MemoryEntry,
    target_db: DbScope,
    named_project: Option<String>,
}

#[derive(Clone, Serialize, Deserialize)]
struct CaptureManifestArtifact {
    id: String,
    replay_key: String,
    source_revision: String,
    content_digest: String,
    target_db: String,
    named_project: Option<String>,
    entry: Option<MemoryEntry>,
}

#[derive(Clone, Serialize, Deserialize)]
struct CaptureDestinationReceipts {
    target_db: String,
    named_project: Option<String>,
    memory_ids: Vec<String>,
    maintenance_job_ids: Vec<String>,
    session_event_id: String,
    pipeline_schedule_key: Option<String>,
}

#[derive(Clone, Serialize, Deserialize)]
struct CaptureManifest {
    owner: String,
    lease_until: String,
    completed: bool,
    staging_policy: String,
    expires_at: Option<String>,
    completed_retention_policy: String,
    artifacts: Vec<CaptureManifestArtifact>,
    destination_receipts: Vec<CaptureDestinationReceipts>,
}

fn manifest_artifacts(entries: &[CapturedEntry]) -> Vec<CaptureManifestArtifact> {
    entries
        .iter()
        .map(|captured| {
            let replay_key = captured.entry.metadata["capture_replay_key"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            let source_revision = captured.entry.metadata["source_revision"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            let content_digest = crate::tool_params::canonical_json_sha256(&json!({
                "text": captured.entry.text,
                "summary": captured.entry.summary,
                "keywords": captured.entry.keywords,
                "entities": captured.entry.entities,
            }))
            .expect("memory entry content is JSON serializable");
            CaptureManifestArtifact {
                id: captured.entry.id.clone(),
                replay_key,
                source_revision,
                content_digest,
                target_db: captured.target_db.as_str().to_string(),
                named_project: captured.named_project.clone(),
                entry: Some(captured.entry.clone()),
            }
        })
        .collect()
}

fn captured_entries(artifacts: &[CaptureManifestArtifact]) -> Vec<CapturedEntry> {
    artifacts
        .iter()
        .filter_map(|artifact| {
            artifact.entry.clone().map(|entry| CapturedEntry {
                entry,
                target_db: if artifact.target_db == DbScope::Global.as_str() {
                    DbScope::Global
                } else {
                    DbScope::Project
                },
                named_project: artifact.named_project.clone(),
            })
        })
        .collect()
}

fn with_manifest_store<T>(
    server: &MemoryServer,
    target_db: DbScope,
    named_project: Option<&str>,
    db_path: Option<&std::path::PathBuf>,
    f: impl FnOnce(&mut memcore::MemoryStore) -> Result<T, String>,
) -> Result<T, String> {
    if let Some(project) = named_project {
        server.with_named_project_store(project, f)
    } else if let Some(path) = db_path {
        server.with_path_store(path, f)
    } else {
        server.with_store_for_scope(target_db, f)
    }
}

fn load_manifest(
    server: &MemoryServer,
    target_db: DbScope,
    named_project: Option<&str>,
    db_path: Option<&std::path::PathBuf>,
    key: &str,
) -> Result<Option<(CaptureManifest, u32)>, String> {
    with_manifest_store(server, target_db, named_project, db_path, |store| {
        store
            .get_state_kv(CAPTURE_MANIFEST_NAMESPACE, key)
            .map_err(|e| format!("capture manifest read: {e}"))?
            .map(|(raw, version)| {
                serde_json::from_str(&raw)
                    .map(|manifest| (manifest, version))
                    .map_err(|e| format!("capture manifest parse: {e}"))
            })
            .transpose()
    })
}

fn manifest_artifacts_all_present(
    server: &MemoryServer,
    manifest: &CaptureManifest,
    db_path: Option<&std::path::PathBuf>,
) -> Result<bool, String> {
    for artifact in &manifest.artifacts {
        let target_db = if artifact.target_db == DbScope::Global.as_str() {
            DbScope::Global
        } else {
            DbScope::Project
        };
        if load_capture_entry_at_destination(
            server,
            target_db,
            artifact.named_project.as_deref(),
            db_path,
            &artifact.id,
        )?
        .is_none()
        {
            return Ok(false);
        }
    }
    Ok(true)
}

fn capture_target_identity(
    target_db: DbScope,
    named_project: Option<&str>,
    db_path: Option<&std::path::PathBuf>,
) -> String {
    if let Some(path) = db_path {
        format!("path:{}", path.display())
    } else if let Some(project) = named_project {
        format!("named-project:{project}")
    } else {
        format!("scope:{}", target_db.as_str())
    }
}

#[allow(clippy::too_many_arguments)]
fn build_capture_replay_key(
    target_identity: &str,
    agent_id: &str,
    conversation_id: &str,
    turn_id: &str,
    source_revision: &str,
    artifact_kind: &str,
    artifact_discriminator: &str,
) -> Result<String, String> {
    let basis = json!({
        "version": 1,
        "target_identity": target_identity,
        "agent_id": agent_id,
        "conversation_id": conversation_id,
        "turn_id": turn_id,
        "source_revision": source_revision,
        "artifact_kind": artifact_kind,
        "artifact_discriminator": artifact_discriminator,
    });
    let hash = crate::tool_params::canonical_json_sha256(&basis)?;
    Ok(format!(
        "capture-replay:{}",
        uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_OID, hash.as_bytes())
    ))
}

fn apply_capture_governance_metadata(
    mut metadata: Value,
    replay_key: &str,
    source_revision: &str,
    source_event_id: &str,
    artifact_kind: &str,
    retention: memcore::RetentionPolicy,
) -> Value {
    let object = metadata
        .as_object_mut()
        .expect("inject_provenance always returns an object");
    object.insert("capture_replay_key".into(), json!(replay_key));
    object.insert("capture_replay_keys".into(), json!([replay_key]));
    object.insert("source_revision".into(), json!(source_revision));
    object.insert("source_revisions".into(), json!([source_revision]));
    object.insert("source_event_id".into(), json!(source_event_id));
    object.insert("artifact_kind".into(), json!(artifact_kind));
    object.insert(
        "capture_retention".into(),
        json!({
            "class": retention.as_str(),
            "policy_version": CAPTURE_RETENTION_POLICY_VERSION,
            "ttl_days": if retention == memcore::RetentionPolicy::Ephemeral {
                Some(CAPTURE_EPHEMERAL_TTL_DAYS)
            } else {
                None
            },
        }),
    );
    metadata
}

fn entry_has_capture_replay_key(entry: &MemoryEntry, replay_key: &str) -> bool {
    entry
        .metadata
        .get("capture_replay_key")
        .and_then(Value::as_str)
        .is_some_and(|value| value == replay_key)
        || entry
            .metadata
            .get("capture_replay_keys")
            .and_then(Value::as_array)
            .is_some_and(|keys| keys.iter().any(|key| key.as_str() == Some(replay_key)))
}

#[allow(clippy::too_many_arguments)]
fn renew_capture_lease(
    server: &MemoryServer,
    target_db: DbScope,
    named_project: Option<&str>,
    db_path: Option<&std::path::PathBuf>,
    manifest_key: &str,
    manifest: &mut CaptureManifest,
    manifest_version: &mut u32,
    owner: &str,
) -> Result<(), String> {
    if manifest.owner != owner || manifest.completed {
        return Err("capture manifest ownership was lost".to_string());
    }
    manifest.lease_until =
        (Utc::now() + Duration::seconds(CAPTURE_PROCESSING_LEASE_SECONDS)).to_rfc3339();
    let raw = serde_json::to_string(manifest)
        .map_err(|e| format!("capture manifest lease serialize: {e}"))?;
    let renewed = with_manifest_store(server, target_db, named_project, db_path, |store| {
        store
            .set_state_if_version(
                CAPTURE_MANIFEST_NAMESPACE,
                manifest_key,
                &raw,
                *manifest_version,
            )
            .map_err(|e| format!("capture manifest lease renew: {e}"))
    })?;
    if !renewed {
        return Err("capture manifest ownership was lost".to_string());
    }
    *manifest_version += 1;
    Ok(())
}

/// Best-effort lease release: reloads the manifest fresh from storage
/// (rather than trusting a caller-held `&mut CaptureManifest`/`&mut u32`,
/// which a `Drop` impl can't borrow — it may be borrowed elsewhere, or the
/// caller's stack frame may already be unwinding) and clears the lease only
/// if it's still genuinely ours to clear.
///
/// This is the ONE mechanism every early exit from `handle_capture_session`
/// after a successful claim relies on (via `CaptureLeaseGuard`'s `Drop`)
/// instead of a manual `release_capture_lease(...)` call at each individual
/// `?`/`return Err(...)` site. A prior version patched failure sites one at
/// a time; a review found FOUR separate un-released-lease early-return
/// paths that had been missed that way (renew-lease failures, artifact
/// insert/replay-key failures, a malformed event receipt, the completion
/// write losing its CAS) plus a FIFTH bug in the sites that HAD been
/// patched: `release_capture_lease(...)?` let a release-phase failure
/// silently replace the original error being propagated. Tying release to
/// scope exit (via `Drop`) closes the whole class at once, including sites
/// nobody has enumerated yet.
///
/// Never returns an error: this runs from `Drop`, which cannot propagate
/// one. Every internal failure (reload error, serialize error, lost CAS
/// race) is logged and treated as "nothing more to do here", never a panic.
fn release_capture_lease_best_effort(
    server: &MemoryServer,
    target_db: DbScope,
    named_project: Option<&str>,
    db_path: Option<&std::path::PathBuf>,
    manifest_key: &str,
    owner: &str,
) {
    let loaded = match load_manifest(server, target_db, named_project, db_path, manifest_key) {
        Ok(loaded) => loaded,
        Err(err) => {
            tracing::warn!(
                "[capture_session] lease release: failed to reload manifest \
                 '{manifest_key}': {err}"
            );
            return;
        }
    };
    let Some((mut manifest, version)) = loaded else {
        // Manifest already gone — nothing to release.
        return;
    };
    if manifest.completed {
        // Legitimately finished between guard creation and drop (the normal
        // success path writes `completed = true` durably before returning).
        // A completed manifest is a durable receipt; never touch its lease
        // fields again.
        return;
    }
    if manifest.owner != owner {
        // Our lease already expired and a concurrent caller legitimately
        // re-claimed it. Releasing now would clobber a live claim that
        // isn't ours — not our lease to release anymore, not an error.
        return;
    }
    manifest.owner.clear();
    manifest.lease_until = (Utc::now() - Duration::seconds(1)).to_rfc3339();
    let raw = match serde_json::to_string(&manifest) {
        Ok(raw) => raw,
        Err(err) => {
            tracing::warn!(
                "[capture_session] lease release: failed to serialize manifest \
                 '{manifest_key}': {err}"
            );
            return;
        }
    };
    match with_manifest_store(server, target_db, named_project, db_path, |store| {
        store
            .set_state_if_version(CAPTURE_MANIFEST_NAMESPACE, manifest_key, &raw, version)
            .map_err(|e| format!("capture manifest release: {e}"))
    }) {
        Ok(true) => {}
        Ok(false) => {
            // Lost the CAS race to a concurrent renew/claim between our
            // reload and our write — again, not our lease anymore.
        }
        Err(err) => {
            tracing::warn!(
                "[capture_session] lease release: failed to write released state for \
                 '{manifest_key}': {err}"
            );
        }
    }
}

/// RAII structural gate for the capture manifest lease. Create one
/// immediately after winning the claim CAS; every subsequent exit from
/// `handle_capture_session` — success (the manifest is already durably
/// `completed` by the time this drops, so the release no-ops) or any early
/// `?`/`return Err(...)`, known or future — releases through the same
/// `release_capture_lease_best_effort` path via `Drop`, so no individual
/// failure site needs its own release call.
struct CaptureLeaseGuard<'a> {
    server: &'a MemoryServer,
    target_db: DbScope,
    named_project: Option<String>,
    db_path: Option<std::path::PathBuf>,
    manifest_key: String,
    owner: String,
}

impl Drop for CaptureLeaseGuard<'_> {
    fn drop(&mut self) {
        release_capture_lease_best_effort(
            self.server,
            self.target_db,
            self.named_project.as_deref(),
            self.db_path.as_ref(),
            &self.manifest_key,
            &self.owner,
        );
    }
}

pub(crate) async fn handle_capture_session(
    server: &MemoryServer,
    params: CaptureSessionParams,
) -> Result<String, String> {
    let combined_text = params
        .messages
        .iter()
        .map(|message| format!("{}: {}", message.role.trim(), message.content.trim()))
        .collect::<Vec<_>>()
        .join("\n");

    if combined_text.trim().is_empty() {
        return serde_json::to_string(&json!({
            "status": "skipped",
            "reason": "empty_messages",
            "captured": 0,
        }))
        .map_err(|e| format!("Failed to serialize capture_session response: {e}"));
    }

    if !params.force && combined_text.chars().count() < params.min_chars {
        return serde_json::to_string(&json!({
            "status": "skipped",
            "reason": "below_min_chars",
            "captured": 0,
        }))
        .map_err(|e| format!("Failed to serialize capture_session response: {e}"));
    }

    let requested_scope = normalize_scope(&params.scope, "project");
    let (target_db, named_project, db_path, warning) = resolve_capture_target(
        server,
        &requested_scope,
        params.project.as_deref(),
        params.project_explicit,
        &params.agent_id,
    );

    let base_path = params
        .path_prefix
        .clone()
        .unwrap_or_else(|| build_openclaw_agent_root(&params.agent_id));
    let source_ref_id = format!("{}:{}", params.conversation_id, params.turn_id);
    let source_revision = crate::tool_params::canonical_json_sha256(&json!({
        "messages": &params.messages,
    }))?;
    let target_identity =
        capture_target_identity(target_db, named_project.as_deref(), db_path.as_ref());
    let batch_hash = crate::tool_params::canonical_json_sha256(&json!({
        "version": 1,
        "target_identity": target_identity,
        "agent_id": params.agent_id,
        "conversation_id": params.conversation_id,
        "turn_id": params.turn_id,
        "source_revision": source_revision,
    }))?;
    let manifest_key = format!("capture-batch:{batch_hash}");
    // Reading the durable manifest before extraction is the invariant that
    // makes an exact replay independent of a second model sample.
    let replay_manifest = load_manifest(
        server,
        target_db,
        named_project.as_deref(),
        db_path.as_ref(),
        &manifest_key,
    )?;
    let captured_at = Utc::now();
    let self_evolution_path = format!("{}/self-evolution", base_path.trim_end_matches('/'));
    // User-preference scoping: agents with "user_memory" in their profile get
    // preference notes scoped to "user" instead of the requested scope.
    let is_user_memory_agent = matches_agent_tag(&params.agent_id, "user-memory")
        || matches_agent_tag(&params.agent_id, "jayne");

    let mut entries = Vec::<CapturedEntry>::new();
    for (note_slot, note) in
        extract_bracket_self_evolution_notes(&params.agent_id, &params.messages)
            .into_iter()
            .enumerate()
    {
        // Legacy bracket IDs were content-derived and therefore collapsed
        // distinct source events. An archived/corrected/superseded legacy row
        // is an explicit suppression tombstone and must win before routing.
        if bracket_capture_is_suppressed(
            server,
            target_db,
            named_project.as_deref(),
            db_path.as_ref(),
            &note.id,
        )? {
            continue;
        }
        let replay_key = build_capture_replay_key(
            &target_identity,
            &params.agent_id,
            &params.conversation_id,
            &params.turn_id,
            &source_revision,
            "bracket_self_evolution",
            &note.id,
        )?;
        let entry_id = replay_key.replacen("capture-replay:", "capture-session:", 1);
        // #1114 (codex round-1 B3 point ③): resolve this entry's routed
        // write-affinity destination BEFORE `inject_provenance` runs, so
        // provenance is stamped against where the row is actually going to
        // land, not the pre-gate default the gate is about to override.
        let (entry_target_db, entry_named_project) = resolve_capture_write_target(
            server,
            &entry_id,
            None,
            &self_evolution_path,
            &note.category,
            target_db,
            named_project.as_deref(),
            db_path.as_ref(),
            params.project_explicit,
        )?;
        // The invariant is checked on both sides of write affinity: a local
        // tombstone prevents routing around it, while a tombstone already at
        // the routed destination prevents recreating content there.
        if bracket_capture_is_suppressed(
            server,
            entry_target_db,
            entry_named_project.as_deref(),
            db_path.as_ref(),
            &note.id,
        )? {
            continue;
        }
        let lineage_key = crate::tool_params::canonical_json_sha256(&json!({
            "version": 1,
            "target": capture_target_identity(entry_target_db, entry_named_project.as_deref(), None),
            "agent_id": params.agent_id,
            "conversation_id": params.conversation_id,
            "turn_id": params.turn_id,
            "artifact_kind": "bracket_self_evolution",
            "note_slot": note_slot,
        }))?;
        let predecessor_id = find_lineage_predecessor(
            server,
            entry_target_db,
            entry_named_project.as_deref(),
            db_path.as_ref(),
            &self_evolution_path,
            &lineage_key,
            &entry_id,
        )?;
        let metadata = crate::provenance::inject_provenance(
            server,
            json!({
                "source_refs": [{
                    "ref_type": "turn",
                    "ref_id": source_ref_id.clone(),
                    "revision": source_revision.clone(),
                }],
                "conversation_id": params.conversation_id,
                "turn_id": params.turn_id,
                "agent_id": params.agent_id,
                "message_count": params.messages.len(),
                "artifact_kind": "bracket_self_evolution",
                "bracket_note_discriminator": note.id,
                "artifact_lineage_key": lineage_key,
                "predecessor_id": predecessor_id,
            }),
            "capture_session",
            "bracket_self_evolution",
            Some(requested_scope.as_str()),
            entry_target_db,
            json!({
                "conversation_id": params.conversation_id,
                "turn_id": params.turn_id,
                "agent_id": params.agent_id,
                "path_prefix": base_path,
            }),
        );
        let metadata = apply_capture_governance_metadata(
            metadata,
            &replay_key,
            &source_revision,
            &source_ref_id,
            "bracket_self_evolution",
            memcore::RetentionPolicy::Durable,
        );
        let strategy_keyword = if note.category == "preference" {
            "user-preference".to_string()
        } else {
            "strategy".to_string()
        };
        let entry_scope = if is_user_memory_agent && note.category == "preference" {
            "user".to_string()
        } else {
            requested_scope.clone()
        };

        entries.push(CapturedEntry {
            entry: MemoryEntry {
                id: entry_id,
                path: self_evolution_path.clone(),
                summary: note.text.chars().take(100).collect(),
                text: note.text,
                importance: 0.70,
                timestamp: captured_at.to_rfc3339(),
                valid_from: String::new(),
                valid_until: None,
                category: note.category,
                topic: "self_evolution".to_string(),
                keywords: dedup_strings(vec![
                    "self-evolution".to_string(),
                    "bracket-note".to_string(),
                    strategy_keyword,
                ]),
                persons: Vec::new(),
                entities: if is_user_memory_agent {
                    vec!["user".to_string()]
                } else {
                    Vec::new()
                },
                location: String::new(),
                source: "bracket_self_evolution".to_string(),
                scope: entry_scope,
                archived: false,
                access_count: 0,
                last_access: None,
                revision: 1,
                metadata,
                vector: None,
                retention_policy: Some(memcore::RetentionPolicy::Durable.as_str().to_string()),
                domain: None,
                recall_count: 0,
                query_diversity: 0,
                tier: "raw".to_string(),
            },
            target_db: entry_target_db,
            named_project: entry_named_project,
        });
    }

    let payload = json!({
        "conversation_id": params.conversation_id,
        "turn_id": params.turn_id,
        "agent_id": params.agent_id,
        "messages": params.messages,
    });
    let request = serde_json::to_string_pretty(&payload)
        .map_err(|e| format!("Failed to serialize session capture payload: {e}"))?;
    let drafts = if replay_manifest.is_some() {
        Vec::new()
    } else {
        match server
            .llm
            .call_extract_llm(
                crate::prompts::SESSION_CAPTURE_PROMPT,
                &request,
                None,
                0.1,
                2400,
            )
            .await
        {
            Ok(raw) => match parse_session_capture_response(&raw) {
                Ok(drafts) => drafts,
                Err(err) if entries.is_empty() => {
                    return serde_json::to_string(&json!({
                        "status": "failed",
                        "reason": "llm_capture_parse_failed",
                        "error": err,
                        "captured": 0,
                        "conversation_id": params.conversation_id,
                        "turn_id": params.turn_id,
                        "agent_id": params.agent_id,
                    }))
                    .map_err(|e| format!("Failed to serialize capture_session response: {e}"));
                }
                Err(_) => Vec::new(),
            },
            Err(err) if entries.is_empty() => {
                return serde_json::to_string(&json!({
                    "status": "failed",
                    "reason": "llm_capture_failed",
                    "error": err,
                    "captured": 0,
                    "conversation_id": params.conversation_id,
                    "turn_id": params.turn_id,
                    "agent_id": params.agent_id,
                }))
                .map_err(|e| format!("Failed to serialize capture_session response: {e}"));
            }
            Err(_) => Vec::new(),
        }
    };

    if drafts.is_empty() && entries.is_empty() && replay_manifest.is_none() {
        return serde_json::to_string(&json!({
            "status": "skipped",
            "reason": "no_durable_memories",
            "captured": 0,
        }))
        .map_err(|e| format!("Failed to serialize capture_session response: {e}"));
    }

    let draft_count = drafts.len();
    for (draft_index, draft) in drafts.into_iter().enumerate() {
        let topic = if draft.topic.trim().is_empty() {
            "session_capture".to_string()
        } else {
            draft.topic.trim().to_string()
        };
        let scope = normalize_scope(&draft.scope, &requested_scope);
        let entry_path = build_entry_path(&base_path, &topic);
        let entry_category = normalize_category(&draft.category);
        let replay_key = build_capture_replay_key(
            &target_identity,
            &params.agent_id,
            &params.conversation_id,
            &params.turn_id,
            &source_revision,
            "session_capture",
            &draft_index.to_string(),
        )?;
        let entry_id = replay_key.replacen("capture-replay:", "capture-session:", 1);

        // #1114 (codex round-1 B3 point ③): resolve BEFORE provenance, same
        // reasoning as the bracket-note loop above.
        let (entry_target_db, entry_named_project) = resolve_capture_write_target(
            server,
            &entry_id,
            None,
            &entry_path,
            &entry_category,
            target_db,
            named_project.as_deref(),
            db_path.as_ref(),
            params.project_explicit,
        )?;
        let metadata = crate::provenance::inject_provenance(
            server,
            json!({
                "source_refs": [{
                    "ref_type": "turn",
                    "ref_id": source_ref_id.clone(),
                    "revision": source_revision.clone(),
                }],
                "conversation_id": params.conversation_id,
                "turn_id": params.turn_id,
                "agent_id": params.agent_id,
                "message_count": params.messages.len(),
                "artifact_index": draft_index,
                "artifact_count": draft_count,
            }),
            "capture_session",
            "session_capture",
            Some(scope.as_str()),
            entry_target_db,
            json!({
                "conversation_id": params.conversation_id,
                "turn_id": params.turn_id,
                "agent_id": params.agent_id,
                "path_prefix": base_path,
            }),
        );
        let metadata = apply_capture_governance_metadata(
            metadata,
            &replay_key,
            &source_revision,
            &source_ref_id,
            "session_capture",
            memcore::RetentionPolicy::Ephemeral,
        );

        let summary = if draft.summary.trim().is_empty() {
            draft.text.chars().take(100).collect::<String>()
        } else {
            draft.summary.trim().to_string()
        };

        entries.push(CapturedEntry {
            entry: MemoryEntry {
                id: entry_id,
                path: entry_path,
                summary,
                text: draft.text.trim().to_string(),
                importance: draft.importance.clamp(0.0, 1.0),
                timestamp: captured_at.to_rfc3339(),
                valid_from: String::new(),
                valid_until: Some(
                    (captured_at + Duration::days(CAPTURE_EPHEMERAL_TTL_DAYS)).to_rfc3339(),
                ),
                category: entry_category,
                topic,
                keywords: dedup_strings(draft.keywords),
                persons: Vec::new(),
                entities: {
                    let mut entities = dedup_strings(draft.entities);
                    for name in draft.persons {
                        memcore::types::push_entity_name(&mut entities, &name);
                    }
                    entities
                },
                location: draft.location.trim().to_string(),
                source: "capture_session".to_string(),
                scope,
                archived: false,
                access_count: 0,
                last_access: None,
                revision: 1,
                metadata,
                vector: None,
                retention_policy: Some(memcore::RetentionPolicy::Ephemeral.as_str().to_string()),
                domain: None,
                recall_count: 0,
                query_diversity: 0,
                tier: "raw".to_string(),
            },
            target_db: entry_target_db,
            named_project: entry_named_project,
        });
    }

    let was_recovery = replay_manifest.is_some();
    let owner = uuid::Uuid::new_v4().to_string();
    let mut manifest_version;
    let mut manifest;
    if let Some((existing, version)) = replay_manifest {
        manifest = existing;
        manifest_version = version;
    } else {
        manifest = CaptureManifest {
            // Admission is not ownership. This immediately-expired, ownerless
            // row freezes the artifact payload while allowing all slow work
            // to finish before the ownership CAS below.
            owner: String::new(),
            lease_until: Utc::now().to_rfc3339(),
            completed: false,
            staging_policy: CAPTURE_MANIFEST_STAGING_POLICY.to_string(),
            expires_at: Some(
                (Utc::now() + Duration::days(CAPTURE_MANIFEST_STAGING_TTL_DAYS)).to_rfc3339(),
            ),
            completed_retention_policy: CAPTURE_MANIFEST_COMPLETED_RETENTION_POLICY.to_string(),
            artifacts: manifest_artifacts(&entries),
            destination_receipts: Vec::new(),
        };
        let raw = serde_json::to_string(&manifest)
            .map_err(|e| format!("capture manifest serialize: {e}"))?;
        let inserted = with_manifest_store(
            server,
            target_db,
            named_project.as_deref(),
            db_path.as_ref(),
            |store| {
                store
                    .insert_state_if_absent(CAPTURE_MANIFEST_NAMESPACE, &manifest_key, &raw)
                    .map_err(|e| format!("capture manifest insert: {e}"))
            },
        )?;
        if inserted {
            manifest_version = 1;
        } else {
            (manifest, manifest_version) = load_manifest(
                server,
                target_db,
                named_project.as_deref(),
                db_path.as_ref(),
                &manifest_key,
            )?
            .ok_or_else(|| "capture manifest disappeared after insert race".to_string())?;
        }
    }

    // Always use the admitted artifact set. A racing extractor may have
    // sampled a different model response, but it cannot replace this payload.
    // Completed manifests are receipts, not staging payloads. Check before
    // reconstructing entries or embedding, so exact replay performs no
    // provider work and never depends on content that completion discarded.
    if manifest.completed {
        let duplicate_ids = manifest
            .artifacts
            .iter()
            .map(|item| item.id.clone())
            .collect::<Vec<_>>();
        return serde_json::to_string(&json!({
            "status": "completed", "captured": 0, "ids": [], "merged_ids": [],
            "duplicate_ids": duplicate_ids, "duplicates_skipped": manifest.artifacts.len(),
            "recovered": 0, "maintenance_jobs": [], "db": target_db.as_str(),
            "path_prefix": base_path,
            "continuity": {"session_event":{"status":"skipped","reason":"duplicate_capture"},"pipeline":{"status":"skipped","reason":"duplicate_capture"},"by_destination":[]}
        })).map_err(|e| format!("Failed to serialize capture_session response: {e}"));
    }
    entries = captured_entries(&manifest.artifacts);
    #[cfg(test)]
    capture_failpoint("after_manifest_admission")?;
    let texts = entries
        .iter()
        .map(|captured| crate::memory_search_ops::scrub_secrets(&captured.entry.text).0)
        .collect::<Vec<_>>();
    let embeddings = match server.llm.embed_voyage_batch(&texts, "document").await {
        Ok(vectors) => Some(vectors),
        Err(err) => {
            tracing::warn!("[capture_session] embedding failed, deferring enrichment: {err}");
            None
        }
    };
    if let Some(vectors) = embeddings.as_ref() {
        for (captured, vector) in entries.iter_mut().zip(vectors.iter()) {
            captured.entry.vector = Some(vector.clone());
        }
    }

    loop {
        let all_present = manifest_artifacts_all_present(server, &manifest, db_path.as_ref())?;
        if manifest.completed && all_present {
            let duplicate_ids = manifest
                .artifacts
                .iter()
                .map(|artifact| artifact.id.clone())
                .collect::<Vec<_>>();
            return serde_json::to_string(&json!({
                "status": "completed",
                "captured": 0,
                "ids": [],
                "merged_ids": [],
                "duplicate_ids": duplicate_ids,
                "duplicates_skipped": manifest.artifacts.len(),
                "recovered": 0,
                "maintenance_jobs": [],
                "db": target_db.as_str(),
                "path_prefix": base_path,
                "continuity": {
                    "session_event": {"status": "skipped", "reason": "duplicate_capture"},
                    "pipeline": {"status": "skipped", "reason": "duplicate_capture"},
                    "by_destination": [],
                }
            }))
            .map_err(|e| format!("Failed to serialize capture_session response: {e}"));
        }

        let lease_expired = chrono::DateTime::parse_from_rfc3339(&manifest.lease_until)
            .map_err(|e| format!("capture manifest lease parse: {e}"))?
            <= Utc::now();
        if manifest.owner == owner || manifest.completed || lease_expired {
            let mut claimed = manifest.clone();
            claimed.owner = owner.clone();
            claimed.completed = false;
            claimed.lease_until =
                (Utc::now() + Duration::seconds(CAPTURE_PROCESSING_LEASE_SECONDS)).to_rfc3339();
            let raw = serde_json::to_string(&claimed)
                .map_err(|e| format!("capture manifest serialize: {e}"))?;
            let won = with_manifest_store(
                server,
                target_db,
                named_project.as_deref(),
                db_path.as_ref(),
                |store| {
                    store
                        .set_state_if_version(
                            CAPTURE_MANIFEST_NAMESPACE,
                            &manifest_key,
                            &raw,
                            manifest_version,
                        )
                        .map_err(|e| format!("capture manifest claim: {e}"))
                },
            )?;
            if won {
                manifest = claimed;
                manifest_version += 1;
                break;
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        (manifest, manifest_version) = load_manifest(
            server,
            target_db,
            named_project.as_deref(),
            db_path.as_ref(),
            &manifest_key,
        )?
        .ok_or_else(|| "capture manifest disappeared while waiting".to_string())?;
    }

    // Structural gate: from here on `manifest`/`manifest_version` represent
    // OUR successful claim. `CaptureLeaseGuard`'s `Drop` releases it on
    // every exit below — success (the completion write below durably marks
    // `completed = true` first, so the release no-ops) or any early
    // `?`/`return Err(...)` — so no individual failure site past this point
    // needs its own manual release call.
    let _capture_lease_guard = CaptureLeaseGuard {
        server,
        target_db,
        named_project: named_project.clone(),
        db_path: db_path.clone(),
        manifest_key: manifest_key.clone(),
        owner: owner.clone(),
    };

    let mut duplicate_ids = Vec::new();
    let entries = entries;

    let mut saved_ids = Vec::new();
    // #1114 (codex round-1 B3 point ④): group persisted ids by their ACTUAL
    // (post-gate) destination — a maintenance job or continuity event
    // enqueued against the pre-gate default, for an entry that reroutes
    // elsewhere, resolves memory_ids against a store that never received
    // them: the maintenance worker (`with_foundry_store`) finds nothing and
    // silently skips the job forever, and a later continuity sweep over the
    // pre-gate store references a memory id that doesn't exist there.
    let mut by_destination: std::collections::BTreeMap<(String, Option<String>), Vec<String>> =
        std::collections::BTreeMap::new();

    // Fence immediately before persistence. A stale owner whose lease was
    // taken over cannot begin artifact side effects after this CAS fails.
    // `CaptureLeaseGuard` (armed above, right after the claim) releases the
    // lease on this early return the same way it does for every other exit
    // — including this one, which a prior version of this handler left
    // completely unreleased on renew failure.
    // deliberate IIFE error-scope gate (lease release on Err); do not de-nest
    #[allow(clippy::redundant_closure_call)]
    (|| -> Result<(), String> {
        // Test-only injection point proving the structural gate covers a
        // SECOND, previously-unfixed early-return site (not just the
        // enqueue-maintenance one) without any manual release call here.
        #[cfg(test)]
        capture_failpoint("before_first_renew")?;
        renew_capture_lease(
            server,
            target_db,
            named_project.as_deref(),
            db_path.as_ref(),
            &manifest_key,
            &mut manifest,
            &mut manifest_version,
            &owner,
        )
    })()?;
    #[cfg(test)]
    let mut is_first_artifact = true;
    for captured in &entries {
        let result = if let Some(project) = captured.named_project.as_deref() {
            server.with_named_project_store(project, |store| {
                store
                    .insert_if_absent(&captured.entry)
                    .map_err(|e| e.to_string())
            })
        } else if let Some(path) = db_path.as_ref() {
            server.with_path_store(path, |store| {
                store
                    .insert_if_absent(&captured.entry)
                    .map_err(|e| e.to_string())
            })
        } else {
            server.with_store_for_scope(captured.target_db, |store| {
                store
                    .insert_if_absent(&captured.entry)
                    .map_err(|e| e.to_string())
            })
        }?;
        if result == memcore::InsertMemoryResult::Existing {
            let existing = load_capture_entry_at_destination(
                server,
                captured.target_db,
                captured.named_project.as_deref(),
                db_path.as_ref(),
                &captured.entry.id,
            )?
            .ok_or_else(|| "capture insert winner disappeared".to_string())?;
            let replay_key = captured
                .entry
                .metadata
                .get("capture_replay_key")
                .and_then(Value::as_str)
                .ok_or_else(|| "capture entry missing replay key".to_string())?;
            if !entry_has_capture_replay_key(&existing, replay_key) {
                return Err(format!(
                    "capture replay id collision for '{}'",
                    captured.entry.id
                ));
            }
            duplicate_ids.push(captured.entry.id.clone());
        } else {
            saved_ids.push(captured.entry.id.clone());
        }
        if result == memcore::InsertMemoryResult::Inserted && embeddings.is_none() {
            queue_capture_enrichment(
                server,
                captured.target_db,
                captured.named_project.clone(),
                db_path.clone(),
                &captured.entry,
                false,
                Some(&params.agent_id),
                Some(&base_path),
            );
        }
        by_destination
            .entry((
                captured.target_db.as_str().to_string(),
                captured.named_project.clone(),
            ))
            .or_default()
            .push(captured.entry.id.clone());
        // The manual lease-release inline here was removed when the
        // release mechanism moved to `CaptureLeaseGuard`'s `Drop` (armed
        // right after this caller's claim, above) — this early return now
        // releases the same way every other exit does.
        #[cfg(test)]
        if is_first_artifact && capture_failpoint("after_artifact_0").is_err() {
            return Err("capture_session test failpoint: after_artifact_0".into());
        }
        #[cfg(test)]
        {
            is_first_artifact = false;
        }
    }

    let saved_ids = dedup_strings(saved_ids);

    // The pre-gate default destination — preferred, when present among the
    // ACTUAL destinations reached, to populate the response's top-level
    // (pre-#1114-shaped) `maintenance_jobs`/`continuity` fields, so existing
    // callers parsing this response see NO shape change in the common
    // (nothing rerouted) case. When every entry rerouted AWAY from this
    // default (codex round-2 item 3 point ④), it will not appear in
    // `by_destination` at all — falling back to whichever destination
    // actually exists (below) instead of a hardcoded "skipped" placeholder
    // is what keeps the top-level `continuity.session_event` truthful in
    // that case.
    let default_destination_key = (target_db.as_str().to_string(), named_project.clone());

    let mut maintenance_jobs = Vec::new();
    let mut destination_receipts = Vec::new();
    let mut continuity_by_destination = Vec::new();
    // (destination_key, session_event, pipeline) tuples, in the same order
    // as `continuity_by_destination` — kept alongside the JSON so picking
    // the "primary" entry after the loop doesn't need to round-trip
    // through JSON value comparisons.
    let mut continuity_results_by_destination: Vec<((String, Option<String>), Value, Value)> =
        Vec::new();

    // Maintenance and continuity are separately fenced from persistence.
    // No await occurs between this renewal and these local queue/spawn calls.
    renew_capture_lease(
        server,
        target_db,
        named_project.as_deref(),
        db_path.as_ref(),
        &manifest_key,
        &mut manifest,
        &mut manifest_version,
        &owner,
    )?;

    for (destination_key, raw_ids) in &by_destination {
        let ids = dedup_strings(raw_ids.clone());
        let ids = &ids;
        let (db_str, group_named_project) = destination_key;
        let group_target_db = if db_str.as_str() == DbScope::Global.as_str() {
            DbScope::Global
        } else {
            DbScope::Project
        };
        // deliberate IIFE error-scope gate (lease release on Err); do not de-nest
        #[allow(clippy::redundant_closure_call)]
        let mut jobs = match (|| -> Result<Vec<memcore::FoundryJobSpec>, String> {
            // Test-only injection point so the lease-release-on-enqueue-
            // failure path below is exercised without depending on a real
            // foundry-queue write failure. Compiled out (and a no-op) on
            // non-test builds.
            #[cfg(test)]
            capture_failpoint("before_enqueue_maintenance")?;
            enqueue_capture_maintenance_jobs(
                server,
                group_target_db,
                group_named_project.clone(),
                db_path.clone(),
                &params.agent_id,
                &base_path,
                ids,
                0,
                0,
                Some(&format!(
                    "{manifest_key}:{db_str}:{}",
                    group_named_project.as_deref().unwrap_or("default")
                )),
            )
        })() {
            // `CaptureLeaseGuard` (armed after the claim, above) releases
            // the lease on this return the same way it does for every
            // other exit — no manual release call needed here.
            Ok(jobs) => jobs,
            Err(error) => return Err(error),
        };
        let maintenance_job_ids = jobs.iter().map(|job| job.id.clone()).collect();
        maintenance_jobs.append(&mut jobs);

        let group_continuity_target = crate::continuity_ops::ContinuityEventTarget::new(
            group_target_db,
            group_named_project.clone(),
            db_path.clone(),
        );
        let session_event = match crate::continuity_ops::emit_session_captured_event(
            server,
            &group_continuity_target,
            &params.conversation_id,
            &params.turn_id,
            &params.agent_id,
            &base_path,
            ids,
            params.messages.len(),
            // #1114 (codex round-2 item 3 point ②): label this event with
            // the destination it was ACTUALLY written to, not the stale
            // pre-gate `params.project` — `ContinuityEventTarget::
            // project_label`'s own precedence puts an "explicit_project"
            // argument ahead of the target's own `named_project`, so
            // passing the stale value here made the label lie about where
            // the row landed whenever `params.project` was `Some` (which,
            // since #1114's B2 fix, is nearly always true for a bound
            // session).
            group_named_project.as_deref(),
        ) {
            Ok(receipt) => receipt,
            // `CaptureLeaseGuard` releases the lease on this return the
            // same way it does for every other exit.
            Err(error) => return Err(error),
        };
        let session_event_id = session_event["event_id"]
            .as_str()
            .ok_or_else(|| "session.captured receipt missing event_id".to_string())?
            .to_string();

        // #1114 (codex round-2 item 3 point ③): the session-continuity
        // PIPELINE analyzes the whole conversation (not specific memory
        // ids), but it still writes its OWN continuity events — those must
        // land where this destination group's rows actually are, not at
        // the pre-gate default. Running it once per destination that
        // actually received entries (rather than once, unconditionally, at
        // the pre-gate default) means a fully-rerouted capture no longer
        // writes pipeline events into a store none of its content ever
        // reached; a batch split across multiple destinations pays for
        // duplicate background analysis, which is the acceptable side of
        // this trade-off (a background LLM pass repeating is far cheaper
        // than a durable row landing in the wrong store).
        let pipeline_target = crate::continuity_ops::ContinuityEventTarget::new(
            group_target_db,
            group_named_project.clone(),
            db_path.clone(),
        );
        let pipeline_operation_key = format!(
            "{manifest_key}:{db_str}:{}:continuity-pipeline-v1",
            group_named_project.as_deref().unwrap_or("default")
        );
        let pipeline = crate::continuity_ops::maybe_spawn_session_continuity_pipeline(
            server,
            pipeline_target,
            &pipeline_operation_key,
            params.conversation_id.clone(),
            params.turn_id.clone(),
            params.agent_id.clone(),
            group_named_project.clone(),
            params.messages.clone(),
        );

        destination_receipts.push(CaptureDestinationReceipts {
            target_db: group_target_db.as_str().to_string(),
            named_project: group_named_project.clone(),
            memory_ids: ids.clone(),
            maintenance_job_ids,
            session_event_id,
            pipeline_schedule_key: matches!(
                pipeline["status"].as_str(),
                Some("scheduled_best_effort" | "already_scheduled")
            )
            .then_some(pipeline_operation_key),
        });

        continuity_results_by_destination.push((
            destination_key.clone(),
            session_event.clone(),
            pipeline.clone(),
        ));
        continuity_by_destination.push(json!({
            "target_db": group_target_db.as_str(),
            "named_project": group_named_project,
            "memory_ids": ids,
            "session_event": session_event,
            "pipeline": pipeline,
        }));
    }

    // #1114 (codex round-2 item 3 point ④): prefer the pre-gate default
    // destination's session event/pipeline when it's among the ones
    // actually reached; otherwise (every entry rerouted away from it) fall
    // back to whichever destination DID receive entries, rather than
    // reporting a hardcoded "skipped: no_captured_entries" placeholder that
    // contradicts `by_destination`'s own (truthful) contents.
    let primary = continuity_results_by_destination
        .iter()
        .find(|(key, _, _)| key == &default_destination_key)
        .or_else(|| continuity_results_by_destination.first());
    let primary_session_event = primary
        .map(|(_, event, _)| event.clone())
        .unwrap_or_else(|| json!({"status": "skipped", "reason": "no_captured_entries"}));
    let primary_pipeline = primary
        .map(|(_, _, pipeline)| pipeline.clone())
        .unwrap_or_else(|| json!({"status": "skipped", "reason": "no_captured_entries"}));

    // `CaptureLeaseGuard` releases the lease on this return the same way
    // it does for every other exit.
    #[cfg(test)]
    if capture_failpoint("after_maintenance").is_err() {
        return Err("capture_session test failpoint: after_maintenance".into());
    }

    manifest.completed = true;
    manifest.owner.clear();
    manifest.lease_until.clear();
    manifest.expires_at = None;
    manifest.destination_receipts = destination_receipts;
    // Atomic content drop: completed receipts retain only identity,
    // destination, digests, and durable side-effect receipt identifiers.
    for artifact in &mut manifest.artifacts {
        artifact.entry = None;
    }
    let completed_manifest = serde_json::to_string(&manifest)
        .map_err(|e| format!("capture manifest serialize completion: {e}"))?;
    let completed = with_manifest_store(
        server,
        target_db,
        named_project.as_deref(),
        db_path.as_ref(),
        |store| {
            store
                .set_state_if_version(
                    CAPTURE_MANIFEST_NAMESPACE,
                    &manifest_key,
                    &completed_manifest,
                    manifest_version,
                )
                .map_err(|e| format!("capture manifest complete: {e}"))
        },
    )?;
    if !completed {
        return Err("capture manifest lease was lost before completion".to_string());
    }

    let mut response = serde_json::Map::new();
    response.insert("status".into(), json!("completed"));
    response.insert("captured".into(), json!(saved_ids.len()));
    response.insert("ids".into(), json!(saved_ids));
    response.insert("merged_ids".into(), json!(Vec::<String>::new()));
    let duplicate_count = duplicate_ids.len();
    response.insert("duplicate_ids".into(), json!(duplicate_ids));
    response.insert("duplicates_skipped".into(), json!(duplicate_count));
    response.insert(
        "recovered".into(),
        json!(if was_recovery { saved_ids.len() } else { 0 }),
    );
    response.insert("maintenance_jobs".into(), json!(maintenance_jobs));
    response.insert("db".into(), json!(target_db.as_str()));
    response.insert("path_prefix".into(), json!(base_path));
    response.insert(
        "continuity".into(),
        json!({
            "session_event": primary_session_event,
            "pipeline": primary_pipeline,
            "by_destination": continuity_by_destination,
        }),
    );
    if let Some(warning) = warning {
        response.insert("warning".into(), json!(warning));
    }

    serde_json::to_string(&Value::Object(response))
        .map_err(|e| format!("Failed to serialize capture_session response: {e}"))
}

/// #1114 (write_affinity module doc's F1 note): purpose-built write-affinity
/// scrutiny for the fresh-incoming-content path — bracket self-evolution
/// notes and LLM-drafted session-capture drafts land wherever
/// `resolve_capture_target` resolved, the exact ambiguous-default shape S1
/// exists to catch. Takes `path`/`category`/`domain` directly (not a built
/// `MemoryEntry`) so the routed destination can be resolved BEFORE the entry
/// (and its provenance) is constructed at all — see `handle_capture_session`'s
/// entry-building loops, where the ROUTED destination now feeds
/// `provenance::inject_provenance` directly instead of the pre-gate default
/// (codex round-1 B3 point ③: provenance must not be stamped against a
/// destination the write-affinity gate is about to override).
///
/// A `db_path` target (the manifest agent-pinned branch of
/// `resolve_capture_target`) is a deliberate per-agent DB assignment and is
/// never scrutinized here, same posture as continuity's own `db_path` skip
/// in `continuity_ops::storage::upsert_projection_memory`.
///
/// `id` (codex round-1 B4 fix): bracket self-evolution notes use a
/// deterministic `UUIDv5` (`build_bracket_self_evolution_id`, hashed from
/// `agent_id` + note text), not a fresh random id — a repeat capture of the
/// SAME note text is an update-in-place at wherever it already lives, not a
/// fresh row. `id_resolves_at_target` used to be hard-coded `false`, so a
/// repeat bracket capture whose domain routes elsewhere would reroute AGAIN
/// on every call, landing a duplicate copy at BOTH the original and the
/// rerouted store instead of updating the one row in place. Checking
/// existence at the PRE-gate target first (mirrors continuity's own
/// `get_projection_memory` pre-check in `projection.rs`) makes a second
/// capture of the same note skip the gate and update where it already is.
///
/// KNOWN LIMITATION (codex round-2 item 4③, deferred, not solved here):
/// this only catches the note sitting at the PRE-gate default. If the
/// registry routes this domain to store A on one capture and later (a
/// SECOND registry edit) to a DIFFERENT store B, this check never looks at
/// A — the note already there is invisible to it, and a second,
/// independent copy gets created at B. Fully closing that requires
/// persistent per-id "last known location" tracking (a schema-level
/// change), the same shape of gap #1115's check-then-insert atomicity work
/// is already scoped to address — deliberately not solved with an ad-hoc
/// migration in this leaf. Concurrent/racing inserts against the SAME
/// pre-gate id have the identical exposure for the same reason (this
/// lookup-then-decide sequence has no transactional atomicity).
fn resolve_capture_write_target(
    server: &MemoryServer,
    id: &str,
    domain: Option<&str>,
    path: &str,
    category: &str,
    target_db: DbScope,
    named_project: Option<&str>,
    db_path: Option<&std::path::PathBuf>,
    project_explicit: bool,
) -> Result<(DbScope, Option<String>), String> {
    if db_path.is_some() {
        return Ok((target_db, named_project.map(str::to_string)));
    }
    // #1114 (codex round-2 item 4 fix): `?` propagates a genuine read
    // failure instead of silently treating it as "doesn't exist" — a false
    // negative here (store unreadable right now, NOT actually empty) risks
    // creating exactly the duplicate-across-two-stores outcome this
    // existence check exists to prevent.
    let already_exists_at_target = capture_entry_exists_at(server, target_db, named_project, id)?;
    // `repair_target` returns `None` both when nothing needs repairing (an
    // already-clean domain is unchanged) and when there was truly nothing to
    // infer — same `.or_else` fallback `resolve_save_domain` (save_memory)
    // and `projection_domain_label` (continuity) both apply, so an already
    // -valid `domain` is never dropped just because it didn't need a repair.
    let resolved_domain =
        crate::repair::domain::repair_target(domain, path, category, "foundry_capture")
            .or_else(|| domain.map(str::to_string));
    let affinity =
        crate::memory_search_ops::save_memory::write_affinity::apply_write_affinity_for_domain(
            server,
            resolved_domain.as_deref(),
            target_db,
            named_project,
            project_explicit,
            already_exists_at_target,
        )?;
    Ok((affinity.target_db, affinity.named_project))
}

/// #1114 (codex round-1 B4 fix, round-2 item 4 fix): does a row with this
/// id already exist at the PRE-gate target? A read FAILURE now propagates
/// as a real `Err` — treating it as "doesn't exist" (the original B4 shape,
/// matching `continuity_ops::projection::persist_timeline_graph_edges`'s
/// own tolerant endpoint-existence checks) is safe for THAT call site
/// because a false negative there only skips an optional graph edge; here a
/// false negative risks creating a duplicate row at the write-affinity
/// gate's proposed reroute destination while a genuine copy already exists
/// at the target this call couldn't read.
fn capture_entry_exists_at(
    server: &MemoryServer,
    target_db: DbScope,
    named_project: Option<&str>,
    id: &str,
) -> Result<bool, String> {
    let result = if let Some(project_name) = named_project {
        server.with_named_project_store_read(project_name, |store| {
            store.get_with_options(id, true).map_err(|e| e.to_string())
        })
    } else {
        server.with_store_for_scope_read(target_db, |store| {
            store.get_with_options(id, true).map_err(|e| e.to_string())
        })
    };
    Ok(result?.is_some())
}

fn load_capture_entry_at_destination(
    server: &MemoryServer,
    target_db: DbScope,
    named_project: Option<&str>,
    db_path: Option<&std::path::PathBuf>,
    id: &str,
) -> Result<Option<MemoryEntry>, String> {
    if let Some(project_name) = named_project {
        server.with_named_project_store_read(project_name, |store| {
            store.get_with_options(id, true).map_err(|e| e.to_string())
        })
    } else if let Some(path) = db_path {
        server.with_path_store_read(path, |store| {
            store.get_with_options(id, true).map_err(|e| e.to_string())
        })
    } else {
        server.with_store_for_scope_read(target_db, |store| {
            store.get_with_options(id, true).map_err(|e| e.to_string())
        })
    }
}

fn find_lineage_predecessor(
    server: &MemoryServer,
    target_db: DbScope,
    named_project: Option<&str>,
    db_path: Option<&std::path::PathBuf>,
    path: &str,
    lineage_key: &str,
    current_id: &str,
) -> Result<Option<String>, String> {
    let query = |store: &mut memcore::MemoryStore| {
        store
            .connection()
            .query_row(
                "SELECT id FROM memories
                 WHERE path = ?1 AND id <> ?2
                   AND json_extract(metadata, '$.artifact_lineage_key') = ?3
                 ORDER BY timestamp DESC, created_at DESC LIMIT 1",
                rusqlite::params![path, current_id, lineage_key],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(|e| format!("query bracket lineage predecessor: {e}"))
    };
    use rusqlite::OptionalExtension;
    if let Some(project) = named_project {
        server.with_named_project_store_read(project, query)
    } else if let Some(path) = db_path {
        server.with_path_store_read(path, query)
    } else {
        server.with_store_for_scope_read(target_db, query)
    }
}

fn bracket_capture_is_suppressed(
    server: &MemoryServer,
    target_db: DbScope,
    named_project: Option<&str>,
    db_path: Option<&std::path::PathBuf>,
    discriminator: &str,
) -> Result<bool, String> {
    let find = |store: &mut memcore::MemoryStore| {
        store
            .connection()
            .query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM memories
                    WHERE (
                        id = ?1
                        OR json_extract(metadata, '$.bracket_note_discriminator') = ?1
                    )
                    AND (
                        archived = 1
                        OR superseded_by IS NOT NULL
                        OR json_extract(metadata, '$.lifecycle') IN ('corrected', 'superseded')
                        OR json_type(metadata, '$.superseded_by') IS NOT NULL
                    )
                 )",
                [discriminator],
                |row| row.get::<_, bool>(0),
            )
            .map_err(|e| format!("query bracket suppression tombstone: {e}"))
    };
    if let Some(project) = named_project {
        server.with_named_project_store_read(project, find)
    } else if let Some(path) = db_path {
        server.with_path_store_read(path, find)
    } else {
        server.with_store_for_scope_read(target_db, find)
    }
}

#[cfg(test)]
mod affinity_tests {
    use super::resolve_capture_write_target;
    use crate::server_state::MemoryServer;
    use crate::DbScope;
    use memcore::MemoryEntry;
    use serde_json::json;

    fn entry_with_domain(id: &str, domain: Option<&str>) -> MemoryEntry {
        MemoryEntry {
            id: id.to_string(),
            path: "/openclaw/agent/self-evolution".to_string(),
            summary: String::new(),
            text: "captured note".to_string(),
            importance: 0.7,
            timestamp: "2026-07-14T00:00:00Z".to_string(),
            valid_from: String::new(),
            valid_until: None,
            category: "preference".to_string(),
            topic: String::new(),
            keywords: Vec::new(),
            persons: Vec::new(),
            entities: Vec::new(),
            location: String::new(),
            source: "capture_session".to_string(),
            scope: "project".to_string(),
            archived: false,
            access_count: 0,
            last_access: None,
            revision: 1,
            metadata: json!({}),
            vector: None,
            retention_policy: None,
            domain: domain.map(str::to_string),
            recall_count: 0,
            query_diversity: 0,
            tier: "raw".to_string(),
        }
    }

    fn two_project_server(home: &std::path::Path, bound_project: &str) -> MemoryServer {
        let quant_db = home.join("projects").join(bound_project).join("memory.db");
        std::fs::create_dir_all(quant_db.parent().unwrap()).expect("mkdir bound project");
        let global_db = home.join("global").join("memory.db");
        std::fs::create_dir_all(global_db.parent().unwrap()).expect("mkdir global");
        // `MemoryServer::new(global, project)` — the project path (not the
        // first/global one) is what `bound_project_label` resolves the bound
        // name from via the Plan C `projects/<name>/memory.db` convention.
        MemoryServer::new(global_db, Some(quant_db)).expect("bind daemon")
    }

    /// #1114 (Oz r3 fixture fix): mount a real, schema-initialized named
    /// -project DB rather than a zero-byte placeholder file — see
    /// `continuity_ops::storage::tests::mount_named_project_db`'s doc for
    /// why a zero-byte file only survives a WRITE-first touch, not a READ
    /// -first one (`with_named_project_store_read` does not run schema
    /// init the way the write path's `MemoryStore::open_with_label` does).
    fn mount_named_project_db(home: &std::path::Path, name: &str) -> std::path::PathBuf {
        let db_path = home.join("projects").join(name).join("memory.db");
        std::fs::create_dir_all(db_path.parent().unwrap()).expect("mkdir named project");
        memcore::MemoryStore::open_with_label(db_path.to_str().expect("utf-8 db path"), name)
            .expect("init named project schema");
        db_path
    }

    /// #1114 discriminating test (red before this PR): capture content whose
    /// domain is registered to a DIFFERENT, mounted store than the daemon's
    /// own bound project must be rerouted there, not silently land in the
    /// bound project just because that's where `resolve_capture_target`
    /// pointed by default. Before this change, `capture_session.rs` called
    /// `persist_capture_entry` directly with the pre-gate target — this is
    /// exactly the cross-domain-drift shape #1041/#1114 exist to catch.
    #[test]
    fn cross_domain_capture_entry_reroutes_to_registered_mounted_store() {
        crate::test_support::with_tachi_home(|home| {
            std::fs::write(
                home.join("routing.json"),
                r#"{"domain_routes":[{"project":"hapi","domains":["equity_trading"]}]}"#,
            )
            .expect("write routing.json");
            let server = two_project_server(home, "quant");
            let _hapi_db = mount_named_project_db(home, "hapi");

            let (target_db, named_project) = resolve_capture_write_target(
                &server,
                "cap-1",
                Some("equity_trading"),
                "/openclaw/agent/self-evolution",
                "preference",
                DbScope::Project,
                Some("quant"), // transport-injected default == daemon's own bound project
                None,
                false, // project_explicit: NOT a caller decision
            )
            .expect("resolve write target");

            assert_eq!(target_db, DbScope::Project);
            assert_eq!(
                named_project.as_deref(),
                Some("hapi"),
                "equity_trading content on an unrelated (quant) daemon must reroute to hapi"
            );
        });
    }

    /// Same mismatch, but the registered store is NOT mounted — must refuse
    /// loudly rather than silently landing the entry in the bound store.
    #[test]
    fn cross_domain_capture_entry_refuses_when_registered_store_unmounted() {
        crate::test_support::with_tachi_home(|home| {
            std::fs::write(
                home.join("routing.json"),
                r#"{"domain_routes":[{"project":"hapi","domains":["equity_trading"]}]}"#,
            )
            .expect("write routing.json");
            let server = two_project_server(home, "quant");
            // "hapi" is never mounted here.

            let err = resolve_capture_write_target(
                &server,
                "cap-2",
                Some("equity_trading"),
                "/openclaw/agent/self-evolution",
                "preference",
                DbScope::Project,
                Some("quant"),
                None,
                false,
            )
            .expect_err("must refuse, not silently write cross-domain");
            assert!(err.contains("equity_trading"));
            assert!(err.contains("hapi"));
        });
    }

    /// Same-domain (or unregistered-domain) content on its own daemon is
    /// unaffected — the common case must not be disturbed by this gate.
    #[test]
    fn same_domain_capture_entry_is_unaffected() {
        crate::test_support::with_tachi_home(|home| {
            let server = two_project_server(home, "quant");

            let (target_db, named_project) = resolve_capture_write_target(
                &server,
                "cap-3",
                None,
                "/openclaw/agent/self-evolution",
                "preference",
                DbScope::Project,
                Some("quant"),
                None,
                false,
            )
            .expect("resolve write target");

            assert_eq!(target_db, DbScope::Project);
            assert_eq!(named_project.as_deref(), Some("quant"));
        });
    }

    /// An explicit `db_path` target (the manifest agent-pinned capture
    /// branch) is never scrutinized — it passes through unchanged even for
    /// mismatched, registered domain content.
    #[test]
    fn db_path_target_skips_the_gate_entirely() {
        crate::test_support::with_tachi_home(|home| {
            std::fs::write(
                home.join("routing.json"),
                r#"{"domain_routes":[{"project":"hapi","domains":["equity_trading"]}]}"#,
            )
            .expect("write routing.json");
            let server = two_project_server(home, "quant");

            let pinned = home
                .join("projects")
                .join("pinned-agent-db")
                .join("memory.db");
            let (target_db, named_project) = resolve_capture_write_target(
                &server,
                "cap-4",
                Some("equity_trading"),
                "/openclaw/agent/self-evolution",
                "preference",
                DbScope::Project,
                None,
                Some(&pinned),
                false,
            )
            .expect("resolve write target");

            assert_eq!(target_db, DbScope::Project);
            assert!(named_project.is_none());
        });
    }

    /// #1114 codex round-1 B4 discriminating test: bracket self-evolution
    /// notes hash to a stable, deterministic `UUIDv5` (same agent + same
    /// note text -> same id every time) — NOT a fresh random id each
    /// capture. Scenario: a note was captured back when `equity_trading` had
    /// no registered route (landed at the daemon's own bound "quant" store,
    /// the ordinary passthrough case), and the SAME note text gets captured
    /// AGAIN later, after `equity_trading` has since been registered to
    /// route to "hapi". The repeat capture must update the row that's
    /// ALREADY at "quant" in place, not reroute to "hapi" and create a
    /// SECOND, independent copy of the identical note split across two
    /// stores. Before the B4 fix, `id_resolves_at_target` was hard-coded
    /// `false`, so every repeat capture blindly re-evaluated domain routing
    /// from scratch regardless of where the row already lived.
    #[test]
    fn repeat_capture_of_same_deterministic_id_updates_in_place_not_rerouted() {
        crate::test_support::with_tachi_home(|home| {
            let server = two_project_server(home, "quant");
            // The note's FIRST capture landed here, back before
            // `equity_trading` had any registered route at all.
            server
                .with_project_store(|store| {
                    store
                        .upsert(&entry_with_domain(
                            "bracket-self-evolution:stable-hash",
                            Some("equity_trading"),
                        ))
                        .map_err(|e| e.to_string())
                })
                .expect("seed the note's original row at the pre-gate target");

            // `equity_trading` is now registered to route to "hapi", and
            // "hapi" is mounted — if the gate did not check for an existing
            // row first, a repeat capture would reroute there.
            std::fs::write(
                home.join("routing.json"),
                r#"{"domain_routes":[{"project":"hapi","domains":["equity_trading"]}]}"#,
            )
            .expect("write routing.json");
            let _hapi_db = mount_named_project_db(home, "hapi");

            let (round2_db, round2_project) = resolve_capture_write_target(
                &server,
                "bracket-self-evolution:stable-hash",
                Some("equity_trading"),
                "/openclaw/agent/self-evolution",
                "preference",
                DbScope::Project,
                Some("quant"),
                None,
                false,
            )
            .expect("repeat capture resolve");
            assert_eq!(
                round2_db,
                DbScope::Project,
                "the repeat capture must update in place, not reroute"
            );
            assert_eq!(
                round2_project.as_deref(),
                Some("quant"),
                "the repeat capture must target wherever the row ALREADY \
                 lives (quant), not re-evaluate domain routing and split it \
                 across two stores"
            );
        });
    }

    /// #1114 codex round-2 item 4② discriminating test: a genuine read
    /// FAILURE at the pre-gate target must propagate as a real `Err`, not
    /// be silently treated as "this id doesn't exist here" — see
    /// `continuity_ops::storage::tests::read_failure_at_pretarget_propagates_instead_of_being_treated_as_absence`
    /// for the fuller reasoning (same fix, same shape, applied to
    /// `capture_entry_exists_at` instead of `get_projection_memory`).
    #[test]
    fn read_failure_at_pretarget_propagates_instead_of_being_treated_as_absence() {
        crate::test_support::with_tachi_home(|home| {
            let server = two_project_server(home, "quant");
            // "corrupt" exists per the filesystem but is not a valid SQLite
            // database at all — genuinely unreadable, not merely empty.
            let corrupt_db = home.join("projects").join("corrupt").join("memory.db");
            std::fs::create_dir_all(corrupt_db.parent().unwrap()).expect("mkdir corrupt");
            std::fs::write(&corrupt_db, b"this is not a sqlite database file at all")
                .expect("write garbage bytes");

            let err = resolve_capture_write_target(
                &server,
                "cap-read-failure",
                Some("equity_trading"),
                "/openclaw/agent/self-evolution",
                "preference",
                DbScope::Project,
                Some("corrupt"),
                None,
                false,
            )
            .expect_err(
                "a genuinely unreadable pre-gate target must propagate an error, \
                 not silently proceed as if the row didn't exist",
            );
            assert!(!err.is_empty());
        });
    }

    // #1114 codex round-2 item 4①/③ KNOWN LIMITATION: NOT reproduced as an
    // integration test in this module. A provider caches its first
    // successful load for the lifetime of its server, so a `routing.json`
    // rewrite does not affect that same live daemon; this scenario happens
    // across a restart. The underlying gate mechanism's lack of memory
    // across two calls with DIFFERENT configs is characterized at the DI
    // level instead:
    // `memory_search_ops::save_memory::write_affinity::tests::
    // config_change_between_calls_reroutes_a_stable_id_to_a_different_store`.
    // Closing this fully requires persistent per-id location tracking,
    // deferred alongside #1115's atomicity work, not solved here.
}

/// #1114 (codex round-2 item 5 fix): a handler-LEVEL discriminating test —
/// every other #1114 capture test in this file drives `resolve_capture_
/// write_target` (or the affinity DI core) directly, hand-injecting
/// `Some("equity_trading")` as the domain. Production never does that:
/// `handle_capture_session` always calls the gate with `domain: None` and
/// lets it get INFERRED from the entry's own `path`/`category` via
/// `repair::domain::repair_target`. A test that injects the domain
/// directly would stay green even if the gate call were deleted from the
/// handler entirely — it never proves the WIRING inside the handler itself.
/// This test goes through the real `handle_capture_session` entry point
/// with a bracket-self-evolution note (the only capture path that needs no
/// live LLM call) whose `path_prefix` drives `infer_domain_from_row` to
/// "equity_trading" the same way a real caller's path would, and asserts
/// the captured row actually lands in the registered "hapi" store — not
/// the daemon's own bound "quant". Deleting the handler's gate call turns
/// this from green to red.
#[cfg(test)]
mod handler_tests {
    use super::{
        build_capture_replay_key, extract_bracket_self_evolution_notes, handle_capture_session,
        set_capture_failpoint, CAPTURE_EPHEMERAL_TTL_DAYS,
        CAPTURE_MANIFEST_COMPLETED_RETENTION_POLICY, CAPTURE_MANIFEST_NAMESPACE,
        CAPTURE_MANIFEST_STAGING_POLICY, CAPTURE_RETENTION_POLICY_VERSION,
    };
    use crate::server_state::MemoryServer;
    use crate::tool_params::{CaptureSessionParams, Message};

    static FAILPOINT_TEST_LOCK: std::sync::OnceLock<std::sync::Mutex<()>> =
        std::sync::OnceLock::new();

    async fn spawn_capture_llm(
        contents: Vec<String>,
    ) -> (
        u16,
        std::sync::Arc<std::sync::atomic::AtomicUsize>,
        tokio::task::JoinHandle<()>,
    ) {
        use axum::{routing::post, Json, Router};

        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let handler_calls = calls.clone();
        let app = Router::new().route(
            "/chat/completions",
            post(move || {
                let index = handler_calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let content = contents
                    .get(index)
                    .or_else(|| contents.last())
                    .expect("fake LLM requires one payload")
                    .clone();
                async move {
                    Json(serde_json::json!({
                        "choices": [{
                            "message": {"role": "assistant", "content": content},
                            "finish_reason": "stop"
                        }],
                        "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
                    }))
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind capture mock LLM");
        let port = listener.local_addr().expect("mock LLM address").port();
        let handle = tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("serve capture mock LLM");
        });
        tokio::task::yield_now().await;
        (port, calls, handle)
    }

    #[test]
    fn capture_replay_key_distinguishes_source_revision_and_event() {
        let key = |conversation: &str, turn: &str, revision: &str| {
            build_capture_replay_key(
                "named-project:alpha",
                "agent-1",
                conversation,
                turn,
                revision,
                "session_capture",
                "0",
            )
            .expect("capture replay key")
        };

        assert_eq!(
            key("conv-1", "turn-1", "rev-a"),
            key("conv-1", "turn-1", "rev-a")
        );
        assert_ne!(
            key("conv-1", "turn-1", "rev-a"),
            key("conv-1", "turn-1", "rev-b")
        );
        assert_ne!(
            key("conv-1", "turn-1", "rev-a"),
            key("conv-1", "turn-2", "rev-a")
        );
    }

    /// #1301 discrimination: exact replay of one durable bracket artifact is
    /// a no-op. Before capture write governance, both calls reported a save,
    /// the second unconditional upsert incremented the row revision, and the
    /// row had no explicit retention class.
    #[test]
    fn exact_bracket_capture_replay_is_duplicate_and_keeps_one_durable_revision() {
        crate::test_support::with_tachi_home(|home| {
            let global_db = home.join("global").join("memory.db");
            let project_db = home.join("projects").join("capture").join("memory.db");
            std::fs::create_dir_all(global_db.parent().unwrap()).expect("mkdir global");
            std::fs::create_dir_all(project_db.parent().unwrap()).expect("mkdir project");

            let params = CaptureSessionParams {
                conversation_id: "conv-replay".to_string(),
                turn_id: "turn-replay".to_string(),
                agent_id: "capture-replay-agent".to_string(),
                messages: vec![Message {
                    role: "assistant".to_string(),
                    content: "（记住了先验证真实对象再相信报告）".to_string(),
                }],
                path_prefix: None,
                scope: "project".to_string(),
                project: None,
                project_explicit: false,
                min_chars: 1,
                force: true,
            };

            let _workers =
                crate::test_support::EnvRestore::set("TACHI_TEST_ENABLE_BACKGROUND_WORKERS", "1");
            let _persist = crate::test_support::EnvRestore::set(
                "TACHI_TEST_DISABLE_PROVIDER_KEY_HEALTH_PERSIST",
                "1",
            );
            let _voyage = crate::test_support::EnvRestore::remove("VOYAGE_API_KEY");

            let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
            let (first, replay, server) = rt.block_on(async move {
                let (port, _calls, mock) = spawn_capture_llm(vec!["[]".to_string()]).await;
                let _base = crate::test_support::EnvRestore::set(
                    "EXTRACT_BASE_URL",
                    &format!("http://127.0.0.1:{port}/chat/completions"),
                );
                let _model = crate::test_support::EnvRestore::set("EXTRACT_MODEL", "capture-mock");
                let _key = crate::test_support::EnvRestore::set("EXTRACT_API_KEY", "test-key");
                let server =
                    MemoryServer::new(global_db, Some(project_db)).expect("capture server");
                let first = handle_capture_session(&server, params.clone())
                    .await
                    .expect("first capture");
                let replay = handle_capture_session(&server, params)
                    .await
                    .expect("replayed capture");
                mock.abort();
                (first, replay, server)
            });
            rt.shutdown_timeout(std::time::Duration::from_millis(500));

            let first: serde_json::Value = serde_json::from_str(&first).expect("first receipt");
            let replay: serde_json::Value = serde_json::from_str(&replay).expect("replay receipt");
            assert_eq!(first["captured"], 1, "first receipt: {first}");
            assert_eq!(replay["captured"], 0, "replay receipt: {replay}");
            assert_eq!(replay["duplicates_skipped"], 1, "replay receipt: {replay}");

            let id = first["ids"][0].as_str().expect("captured id");
            let stored = server
                .with_project_store_read(|store| store.get(id).map_err(|e| e.to_string()))
                .expect("read captured row")
                .expect("captured row exists");
            assert_eq!(stored.revision, 1, "duplicate replay must not upsert");
            assert_eq!(stored.retention_policy.as_deref(), Some("durable"));
            assert!(stored.valid_until.is_none());
        });
    }

    #[test]
    fn bracket_artifacts_are_source_event_specific_and_preserve_lineage() {
        crate::test_support::with_tachi_home(|home| {
            let global_db = home.join("global/memory.db");
            let project_db = home.join("projects/bracket-source/memory.db");
            std::fs::create_dir_all(global_db.parent().unwrap()).unwrap();
            std::fs::create_dir_all(project_db.parent().unwrap()).unwrap();
            let _workers =
                crate::test_support::EnvRestore::set("TACHI_TEST_ENABLE_BACKGROUND_WORKERS", "1");
            let _voyage = crate::test_support::EnvRestore::remove("VOYAGE_API_KEY");
            let rt = tokio::runtime::Runtime::new().unwrap();
            let server = rt.block_on(async move {
                let (port, _, mock) = spawn_capture_llm(vec!["[]".into()]).await;
                let _base = crate::test_support::EnvRestore::set(
                    "EXTRACT_BASE_URL",
                    &format!("http://127.0.0.1:{port}/chat/completions"),
                );
                let _model = crate::test_support::EnvRestore::set("EXTRACT_MODEL", "mock");
                let _key = crate::test_support::EnvRestore::set("EXTRACT_API_KEY", "test-key");
                let server = MemoryServer::new(global_db, Some(project_db)).unwrap();
                let params = |turn: &str, content: &str| CaptureSessionParams {
                    conversation_id: "source-conversation".into(),
                    turn_id: turn.into(),
                    agent_id: "source-agent".into(),
                    messages: vec![Message {
                        role: "assistant".into(),
                        content: content.into(),
                    }],
                    path_prefix: None,
                    scope: "project".into(),
                    project: None,
                    project_explicit: false,
                    min_chars: 1,
                    force: true,
                };
                let first: serde_json::Value = serde_json::from_str(
                    &handle_capture_session(&server, params("turn-1", "（记住了先核验来源）"))
                        .await
                        .unwrap(),
                )
                .unwrap();
                let first_id = first["ids"][0].as_str().unwrap().to_string();
                let before = server
                    .with_project_store_read(|s| s.get(&first_id).map_err(|e| e.to_string()))
                    .unwrap()
                    .unwrap();
                let changed: serde_json::Value = serde_json::from_str(
                    &handle_capture_session(&server, params("turn-1", "（记住了先核验原始来源）"))
                        .await
                        .unwrap(),
                )
                .unwrap();
                let changed_id = changed["ids"][0].as_str().unwrap();
                assert_ne!(first_id, changed_id);
                let changed_row = server
                    .with_project_store_read(|s| s.get(changed_id).map_err(|e| e.to_string()))
                    .unwrap()
                    .unwrap();
                assert_eq!(changed_row.metadata["predecessor_id"], first_id);
                let after = server
                    .with_project_store_read(|s| s.get(&first_id).map_err(|e| e.to_string()))
                    .unwrap()
                    .unwrap();
                assert_eq!(
                    serde_json::to_value(before).unwrap(),
                    serde_json::to_value(after).unwrap()
                );

                let other: serde_json::Value = serde_json::from_str(
                    &handle_capture_session(&server, params("turn-2", "（记住了先核验来源）"))
                        .await
                        .unwrap(),
                )
                .unwrap();
                let other_id = other["ids"][0].as_str().unwrap();
                assert_ne!(first_id, other_id);
                for (id, turn) in [(first_id.as_str(), "turn-1"), (other_id, "turn-2")] {
                    let row = server
                        .with_project_store_read(|s| s.get(id).map_err(|e| e.to_string()))
                        .unwrap()
                        .unwrap();
                    assert_eq!(
                        row.metadata["source_event_id"],
                        format!("source-conversation:{turn}")
                    );
                }
                mock.abort();
                server
            });
            rt.shutdown_timeout(std::time::Duration::from_millis(500));
            drop(server);
        });
    }

    #[test]
    fn concurrent_identical_capture_futures_have_one_receipt_and_one_side_effect_set() {
        crate::test_support::with_tachi_home(|home| {
            let global_db = home.join("global").join("memory.db");
            let project_db = home
                .join("projects")
                .join("capture-concurrent")
                .join("memory.db");
            std::fs::create_dir_all(global_db.parent().unwrap()).expect("mkdir global");
            std::fs::create_dir_all(project_db.parent().unwrap()).expect("mkdir project");
            let params = CaptureSessionParams {
                conversation_id: "conv-concurrent-replay".into(),
                turn_id: "turn-concurrent-replay".into(),
                agent_id: "capture-concurrent-agent".into(),
                messages: vec![Message {
                    role: "assistant".into(),
                    content: "（记住了并发重放只能产生一份持久副作用）".into(),
                }],
                path_prefix: None,
                scope: "project".into(),
                project: None,
                project_explicit: false,
                min_chars: 1,
                force: true,
            };
            let _workers =
                crate::test_support::EnvRestore::set("TACHI_TEST_ENABLE_BACKGROUND_WORKERS", "1");
            let _voyage = crate::test_support::EnvRestore::remove("VOYAGE_API_KEY");
            let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
            let project_db_for_query = project_db.clone();
            let (left, right, replay, server, calls, calls_before_replay) =
                rt.block_on(async move {
                    let (port, calls, mock) = spawn_capture_llm(vec!["[]".into()]).await;
                    let _base = crate::test_support::EnvRestore::set(
                        "EXTRACT_BASE_URL",
                        &format!("http://127.0.0.1:{port}/chat/completions"),
                    );
                    let _model = crate::test_support::EnvRestore::set("EXTRACT_MODEL", "mock");
                    let _key = crate::test_support::EnvRestore::set("EXTRACT_API_KEY", "test-key");
                    let server = MemoryServer::new(global_db, Some(project_db)).expect("server");
                    let (left, right) = tokio::join!(
                        handle_capture_session(&server, params.clone()),
                        handle_capture_session(&server, params.clone())
                    );
                    let calls_before_replay = calls.load(std::sync::atomic::Ordering::SeqCst);
                    assert!((1..=2).contains(&calls_before_replay));
                    let replay = handle_capture_session(&server, params)
                        .await
                        .expect("completed exact replay");
                    assert_eq!(
                        calls.load(std::sync::atomic::Ordering::SeqCst),
                        calls_before_replay
                    );
                    mock.abort();
                    (
                        left.expect("left"),
                        right.expect("right"),
                        replay,
                        server,
                        calls,
                        calls_before_replay,
                    )
                });
            let left: serde_json::Value = serde_json::from_str(&left).expect("left JSON");
            let right: serde_json::Value = serde_json::from_str(&right).expect("right JSON");
            assert_eq!(
                left["captured"].as_u64().unwrap() + right["captured"].as_u64().unwrap(),
                1
            );
            let winner = if left["captured"] == 1 { &left } else { &right };
            let duplicate = if left["captured"] == 0 { &left } else { &right };
            assert_eq!(duplicate["maintenance_jobs"], serde_json::json!([]));
            assert_eq!(
                duplicate["continuity"]["by_destination"],
                serde_json::json!([])
            );
            let id = winner["ids"][0].as_str().expect("winner id");
            let row = server
                .with_project_store_read(|store| store.get(id).map_err(|e| e.to_string()))
                .expect("read row")
                .expect("row");
            assert_eq!(row.revision, 1);
            let replay: serde_json::Value = serde_json::from_str(&replay).expect("replay JSON");
            assert_eq!(replay["captured"], 0);
            assert_eq!(
                calls.load(std::sync::atomic::Ordering::SeqCst),
                calls_before_replay
            );
            let events = server
                .with_project_store_read(|store| {
                    store
                        .list_tachi_events(&memcore::TachiEventQuery {
                            event_type: Some("session.captured".into()),
                            limit: 100,
                            ..Default::default()
                        })
                        .map_err(|e| e.to_string())
                })
                .expect("query session capture events");
            assert_eq!(events.len(), 1);
            let conn = rusqlite::Connection::open(project_db_for_query).expect("open project DB");
            let (job_count, distinct_job_count): (i64, i64) = conn
                .query_row(
                    "SELECT COUNT(*), COUNT(DISTINCT id) FROM foundry_jobs",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .expect("query foundry jobs");
            assert!(job_count > 0);
            assert_eq!(job_count, distinct_job_count);
        });
    }

    #[test]
    fn required_session_event_failure_keeps_manifest_recoverable() {
        let _guard = FAILPOINT_TEST_LOCK
            .get_or_init(|| std::sync::Mutex::new(()))
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        crate::test_support::with_tachi_home(|home| {
            let global_db = home.join("global").join("event-failure.db");
            let project_db = home
                .join("projects")
                .join("event-failure")
                .join("memory.db");
            std::fs::create_dir_all(global_db.parent().unwrap()).expect("mkdir global");
            std::fs::create_dir_all(project_db.parent().unwrap()).expect("mkdir project");
            let params = CaptureSessionParams {
                conversation_id: "event-failure-conversation".into(),
                turn_id: "turn-1".into(),
                agent_id: "event-failure-agent".into(),
                messages: vec![Message {
                    role: "user".into(),
                    content: "Capture one event-failure recovery observation.".into(),
                }],
                path_prefix: Some("/capture/event-failure".into()),
                scope: "project".into(),
                project: None,
                project_explicit: false,
                min_chars: 1,
                force: true,
            };
            let draft = serde_json::json!([{
                "text": "The required session event must be durable before completion.",
                "summary": "Required session event",
                "topic": "recovery",
                "category": "fact",
                "scope": "project",
                "importance": 0.8
            }])
            .to_string();
            let _workers =
                crate::test_support::EnvRestore::set("TACHI_TEST_ENABLE_BACKGROUND_WORKERS", "1");
            let _voyage = crate::test_support::EnvRestore::remove("VOYAGE_API_KEY");
            let rt = tokio::runtime::Runtime::new().expect("runtime");
            rt.block_on(async move {
                let (port, calls, mock) = spawn_capture_llm(vec![draft]).await;
                let _base = crate::test_support::EnvRestore::set(
                    "EXTRACT_BASE_URL",
                    &format!("http://127.0.0.1:{port}/chat/completions"),
                );
                let _model = crate::test_support::EnvRestore::set("EXTRACT_MODEL", "mock");
                let _key = crate::test_support::EnvRestore::set("EXTRACT_API_KEY", "test-key");
                let server = MemoryServer::new(global_db, Some(project_db)).expect("server");

                set_capture_failpoint("after_artifact_0");
                handle_capture_session(&server, params.clone())
                    .await
                    .expect_err("artifact failpoint must leave an incomplete manifest");
                let artifact_id = server
                    .with_project_store_read(|store| {
                        store
                            .connection()
                            .query_row(
                                "SELECT id FROM memories
                                 WHERE path LIKE '/capture/event-failure/%' LIMIT 1",
                                [],
                                |row| row.get::<_, String>(0),
                            )
                            .map_err(|e| e.to_string())
                    })
                    .expect("read admitted artifact id");
                let event_basis = [
                    "session.captured",
                    params.conversation_id.as_str(),
                    params.turn_id.as_str(),
                    params.agent_id.as_str(),
                    artifact_id.as_str(),
                ]
                .join("|");
                let event_id = format!("event-{}", crate::utils::stable_hash(&event_basis));
                let conflicting = memcore::TachiEventRecord {
                    id: event_id.clone(),
                    source_repo: "tachi".into(),
                    adapter: "conflicting-test-fixture".into(),
                    project: "event-failure".into(),
                    domain: "session".into(),
                    session_id: params.conversation_id.clone(),
                    actor: params.agent_id.clone(),
                    event_type: "session.captured".into(),
                    authority: memcore::AuthorityLevel::RawFact,
                    effects: vec![memcore::EffectScope::MemoryWrite],
                    projection_hints: vec![memcore::ProjectionKind::Timeline],
                    payload: serde_json::json!({"conflict": true}),
                    provenance: serde_json::json!({"fixture": true}),
                    created_at: chrono::Utc::now().to_rfc3339(),
                };
                server
                    .with_project_store(|store| {
                        store
                            .insert_tachi_event(&conflicting)
                            .map_err(|e| e.to_string())
                    })
                    .expect("seed deterministic event-id collision");

                let error = handle_capture_session(&server, params.clone())
                    .await
                    .expect_err("required event collision must block completion");
                assert!(error.contains("collision"), "unexpected error: {error}");
                let incomplete = server
                    .with_project_store_read(|store| {
                        store
                            .list_state(CAPTURE_MANIFEST_NAMESPACE)
                            .map_err(|e| e.to_string())
                    })
                    .expect("read incomplete manifest");
                let incomplete: serde_json::Value =
                    serde_json::from_str(&incomplete[0].value_json).expect("manifest JSON");
                assert_eq!(incomplete["completed"], false);
                assert!(incomplete["artifacts"][0]["entry"].is_object());

                server
                    .with_project_store(|store| {
                        store
                            .connection()
                            .execute("DELETE FROM tachi_events WHERE id = ?1", [&event_id])
                            .map(|_| ())
                            .map_err(|e| e.to_string())
                    })
                    .expect("remove transient collision");
                let recovered: serde_json::Value = serde_json::from_str(
                    &handle_capture_session(&server, params)
                        .await
                        .expect("retry after event failure"),
                )
                .expect("recovery receipt");
                assert_eq!(recovered["status"], "completed");
                assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
                let (events, manifests) = server
                    .with_project_store_read(|store| {
                        let events = store
                            .list_tachi_events(&memcore::TachiEventQuery {
                                event_type: Some("session.captured".into()),
                                limit: 10,
                                ..Default::default()
                            })
                            .map_err(|e| e.to_string())?;
                        let manifests = store
                            .list_state(CAPTURE_MANIFEST_NAMESPACE)
                            .map_err(|e| e.to_string())?;
                        Ok((events, manifests))
                    })
                    .expect("read recovered durable state");
                assert_eq!(events.len(), 1);
                let completed: serde_json::Value =
                    serde_json::from_str(&manifests[0].value_json).expect("completed manifest");
                assert_eq!(completed["completed"], true);
                assert!(completed["artifacts"][0]["entry"].is_null());
                mock.abort();
            });
        });
    }

    #[test]
    fn capture_failpoints_recover_idempotently_at_every_durable_stage() {
        let _guard = FAILPOINT_TEST_LOCK
            .get_or_init(|| std::sync::Mutex::new(()))
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        for stage in [
            "after_manifest_admission",
            "after_artifact_0",
            "after_maintenance",
        ] {
            crate::test_support::with_tachi_home(|home| {
                let global_db = home.join("global").join(format!("{stage}-global.db"));
                let project_db = home.join("projects").join(stage).join("memory.db");
                std::fs::create_dir_all(global_db.parent().unwrap()).expect("mkdir global");
                std::fs::create_dir_all(project_db.parent().unwrap()).expect("mkdir project");
                let params = CaptureSessionParams {
                    conversation_id: format!("conv-{stage}"),
                    turn_id: "turn-1".into(),
                    agent_id: "failpoint-agent".into(),
                    messages: vec![Message {
                        role: "user".into(),
                        content: "Capture the durable recovery observation.".into(),
                    }],
                    path_prefix: Some(format!("/capture/{stage}")),
                    scope: "project".into(),
                    project: None,
                    project_explicit: false,
                    min_chars: 1,
                    force: true,
                };
                let draft = serde_json::json!([{
                    "text": "Failpoint recovery preserves exactly one artifact.",
                    "summary": "Failpoint recovery",
                    "topic": "recovery",
                    "category": "fact",
                    "scope": "project",
                    "importance": 0.8
                }])
                .to_string();
                let _workers = crate::test_support::EnvRestore::set(
                    "TACHI_TEST_ENABLE_BACKGROUND_WORKERS",
                    "1",
                );
                let _persist = crate::test_support::EnvRestore::set(
                    "TACHI_TEST_DISABLE_PROVIDER_KEY_HEALTH_PERSIST",
                    "1",
                );
                let _voyage = crate::test_support::EnvRestore::remove("VOYAGE_API_KEY");
                let rt = tokio::runtime::Runtime::new().expect("runtime");
                let project_db_for_query = project_db.clone();
                let (second, third, server, calls) = rt.block_on(async move {
                    let (port, calls, mock) = spawn_capture_llm(vec![draft]).await;
                    let _base = crate::test_support::EnvRestore::set(
                        "EXTRACT_BASE_URL",
                        &format!("http://127.0.0.1:{port}/chat/completions"),
                    );
                    let _model = crate::test_support::EnvRestore::set("EXTRACT_MODEL", "mock");
                    let _key = crate::test_support::EnvRestore::set("EXTRACT_API_KEY", "test-key");
                    let server = MemoryServer::new(global_db, Some(project_db)).expect("server");
                    set_capture_failpoint(stage);
                    let first = handle_capture_session(&server, params.clone()).await;
                    assert!(
                        first
                            .expect_err("armed failpoint must fail once")
                            .contains(stage),
                        "stage {stage} must return an explicit error"
                    );
                    let second = handle_capture_session(&server, params.clone())
                        .await
                        .expect("recovery");
                    let calls_after_recovery = calls.load(std::sync::atomic::Ordering::SeqCst);
                    let third = handle_capture_session(&server, params)
                        .await
                        .expect("exact replay");
                    assert_eq!(
                        calls.load(std::sync::atomic::Ordering::SeqCst),
                        calls_after_recovery,
                        "completed fast path must precede extraction/embedding work"
                    );
                    assert_eq!(
                        calls_after_recovery, 1,
                        "retry must reuse admitted manifest"
                    );
                    mock.abort();
                    (second, third, server, calls)
                });
                rt.shutdown_timeout(std::time::Duration::from_millis(500));

                let second: serde_json::Value = serde_json::from_str(&second).unwrap();
                let third: serde_json::Value = serde_json::from_str(&third).unwrap();
                assert_eq!(second["status"], "completed");
                assert_eq!(third["captured"], 0);
                assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
                let id = second["ids"]
                    .as_array()
                    .and_then(|ids| ids.first())
                    .or_else(|| {
                        third["duplicate_ids"]
                            .as_array()
                            .and_then(|ids| ids.first())
                    })
                    .and_then(serde_json::Value::as_str)
                    .expect("artifact id");
                let row = server
                    .with_project_store_read(|store| store.get(id).map_err(|e| e.to_string()))
                    .expect("query artifact")
                    .expect("artifact exists");
                assert_eq!(row.revision, 1);
                let (events, manifest) = server
                    .with_project_store_read(|store| {
                        let events = store
                            .list_tachi_events(&memcore::TachiEventQuery {
                                event_type: Some("session.captured".into()),
                                limit: 100,
                                ..Default::default()
                            })
                            .map_err(|e| e.to_string())?;
                        let states = store
                            .list_state(CAPTURE_MANIFEST_NAMESPACE)
                            .map_err(|e| e.to_string())?;
                        Ok((events, states))
                    })
                    .expect("query durable side effects");
                assert_eq!(events.len(), 1);
                let conn = rusqlite::Connection::open(&project_db_for_query)
                    .expect("open project DB for durable job query");
                let artifact_count: i64 = conn
                    .query_row("SELECT COUNT(*) FROM memories WHERE id = ?1", [id], |row| {
                        row.get(0)
                    })
                    .expect("count capture memory IDs");
                assert_eq!(artifact_count, 1);
                let mut statement = conn
                    .prepare("SELECT id, kind FROM foundry_jobs ORDER BY id")
                    .expect("prepare foundry job query");
                let jobs = statement
                    .query_map([], |row| {
                        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                    })
                    .expect("query foundry jobs")
                    .collect::<Result<Vec<_>, _>>()
                    .expect("read foundry jobs");
                let mut ids = jobs.iter().map(|(id, _)| id.as_str()).collect::<Vec<_>>();
                ids.sort_unstable();
                ids.dedup();
                assert_eq!(
                    ids.len(),
                    jobs.len(),
                    "deterministic jobs must not duplicate"
                );
                let mut by_kind = std::collections::BTreeMap::new();
                for (_, kind) in &jobs {
                    *by_kind.entry(kind).or_insert(0usize) += 1;
                }
                assert!(by_kind.values().all(|count| *count == 1));
                assert!(
                    !jobs.is_empty(),
                    "one destination group must enqueue maintenance"
                );
                assert_eq!(manifest.len(), 1);
                let serialized = manifest[0].value_json.clone();
                let manifest: serde_json::Value =
                    serde_json::from_str(&serialized).expect("manifest JSON");
                assert_eq!(manifest["completed"], true);
                assert_eq!(manifest["artifacts"][0]["entry"], serde_json::Value::Null);
                let receipt = &manifest["destination_receipts"][0];
                assert!(receipt["memory_ids"]
                    .as_array()
                    .expect("receipt memory ids")
                    .iter()
                    .any(|memory_id| memory_id == id));
                for job_id in receipt["maintenance_job_ids"]
                    .as_array()
                    .expect("receipt job ids")
                {
                    assert!(jobs
                        .iter()
                        .any(|(found, _)| Some(found.as_str()) == job_id.as_str()));
                }
                assert!(events.iter().any(|event| {
                    Some(event.id.as_str()) == receipt["session_event_id"].as_str()
                }));
                assert_eq!(manifest["expires_at"], serde_json::Value::Null);
                assert_eq!(manifest["staging_policy"], CAPTURE_MANIFEST_STAGING_POLICY);
                assert_eq!(
                    manifest["completed_retention_policy"],
                    CAPTURE_MANIFEST_COMPLETED_RETENTION_POLICY
                );
                for sensitive in [
                    "Failpoint recovery preserves exactly one artifact.",
                    "Failpoint recovery",
                    "entities",
                    "keywords",
                ] {
                    assert!(
                        !serialized.contains(sensitive),
                        "completed receipt leaked {sensitive}"
                    );
                }
            });
        }
    }

    /// #1301-adjacent lease-leak fix: `enqueue_capture_maintenance_jobs`
    /// failing must release the manifest lease immediately — same
    /// obligation `emit_session_captured_event`'s failure path already
    /// honors a few lines below it. Before this fix, an enqueue failure
    /// propagated the error with `?` and skipped `release_capture_lease`
    /// entirely, leaving `owner` set and `lease_until` ~30s in the future;
    /// concurrent/retrying callers would spin-wait on a lease nobody was
    /// still using. This asserts the manifest's durable state directly
    /// (rather than timing a retry) so the proof doesn't depend on the
    /// spin-wait loop's cadence.
    #[test]
    fn enqueue_maintenance_failure_releases_the_manifest_lease_immediately() {
        let _guard = FAILPOINT_TEST_LOCK
            .get_or_init(|| std::sync::Mutex::new(()))
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        crate::test_support::with_tachi_home(|home| {
            let stage = "before_enqueue_maintenance";
            let global_db = home.join("global").join(format!("{stage}-global.db"));
            let project_db = home.join("projects").join(stage).join("memory.db");
            std::fs::create_dir_all(global_db.parent().unwrap()).expect("mkdir global");
            std::fs::create_dir_all(project_db.parent().unwrap()).expect("mkdir project");
            let params = CaptureSessionParams {
                conversation_id: format!("conv-{stage}"),
                turn_id: "turn-1".into(),
                agent_id: "failpoint-agent".into(),
                messages: vec![Message {
                    role: "user".into(),
                    content: "Capture the lease-release-on-enqueue-failure observation.".into(),
                }],
                path_prefix: Some(format!("/capture/{stage}")),
                scope: "project".into(),
                project: None,
                project_explicit: false,
                min_chars: 1,
                force: true,
            };
            let draft = serde_json::json!([{
                "text": "Enqueue failure must not leak the manifest lease.",
                "summary": "Lease release on enqueue failure",
                "topic": "recovery",
                "category": "fact",
                "scope": "project",
                "importance": 0.8
            }])
            .to_string();
            let _workers =
                crate::test_support::EnvRestore::set("TACHI_TEST_ENABLE_BACKGROUND_WORKERS", "1");
            let _persist = crate::test_support::EnvRestore::set(
                "TACHI_TEST_DISABLE_PROVIDER_KEY_HEALTH_PERSIST",
                "1",
            );
            let _voyage = crate::test_support::EnvRestore::remove("VOYAGE_API_KEY");
            let rt = tokio::runtime::Runtime::new().expect("runtime");
            rt.block_on(async move {
                let (port, _calls, mock) = spawn_capture_llm(vec![draft]).await;
                let _base = crate::test_support::EnvRestore::set(
                    "EXTRACT_BASE_URL",
                    &format!("http://127.0.0.1:{port}/chat/completions"),
                );
                let _model = crate::test_support::EnvRestore::set("EXTRACT_MODEL", "mock");
                let _key = crate::test_support::EnvRestore::set("EXTRACT_API_KEY", "test-key");
                let server = MemoryServer::new(global_db, Some(project_db)).expect("server");
                set_capture_failpoint(stage);
                let err = handle_capture_session(&server, params)
                    .await
                    .expect_err("armed failpoint must fail the enqueue step");
                assert!(
                    err.contains(stage),
                    "error must name the failpoint stage, got: {err}"
                );

                let manifests = server
                    .with_project_store_read(|store| {
                        store
                            .list_state(CAPTURE_MANIFEST_NAMESPACE)
                            .map_err(|e| e.to_string())
                    })
                    .expect("read manifest state after failed enqueue");
                assert_eq!(
                    manifests.len(),
                    1,
                    "capture admission must have written exactly one manifest row"
                );
                let manifest: serde_json::Value =
                    serde_json::from_str(&manifests[0].value_json).expect("manifest JSON");
                assert_eq!(
                    manifest["owner"], "",
                    "enqueue failure must clear the manifest owner immediately, not leave it \
                     held until the 30s processing lease expires on its own"
                );
                let lease_until = manifest["lease_until"]
                    .as_str()
                    .expect("lease_until is a string");
                let lease_until_dt = chrono::DateTime::parse_from_rfc3339(lease_until)
                    .expect("lease_until parses as RFC3339");
                assert!(
                    lease_until_dt <= chrono::Utc::now(),
                    "lease_until must already be in the past — proof the lease was actively \
                     released rather than left to expire naturally"
                );
                mock.abort();
            });
            rt.shutdown_timeout(std::time::Duration::from_millis(500));
        });
    }

    /// Proves `CaptureLeaseGuard`'s structural release covers a SECOND,
    /// previously-unfixed early-return site — the first `renew_capture_lease`
    /// call, right after the entries loop's post-claim fence — not just the
    /// one enumerated call site the prior (per-site) patch happened to cover.
    /// Before the `Drop`-based guard, this exact site propagated a renew
    /// failure with a bare `?` and never released anything.
    #[test]
    fn renew_lease_failure_releases_the_manifest_lease_via_the_structural_gate() {
        let _guard = FAILPOINT_TEST_LOCK
            .get_or_init(|| std::sync::Mutex::new(()))
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        crate::test_support::with_tachi_home(|home| {
            let stage = "before_first_renew";
            let global_db = home.join("global").join(format!("{stage}-global.db"));
            let project_db = home.join("projects").join(stage).join("memory.db");
            std::fs::create_dir_all(global_db.parent().unwrap()).expect("mkdir global");
            std::fs::create_dir_all(project_db.parent().unwrap()).expect("mkdir project");
            let params = CaptureSessionParams {
                conversation_id: format!("conv-{stage}"),
                turn_id: "turn-1".into(),
                agent_id: "failpoint-agent".into(),
                messages: vec![Message {
                    role: "user".into(),
                    content: "Capture the lease-release-on-renew-failure observation.".into(),
                }],
                path_prefix: Some(format!("/capture/{stage}")),
                scope: "project".into(),
                project: None,
                project_explicit: false,
                min_chars: 1,
                force: true,
            };
            let draft = serde_json::json!([{
                "text": "Renew failure must not leak the manifest lease.",
                "summary": "Lease release on renew failure",
                "topic": "recovery",
                "category": "fact",
                "scope": "project",
                "importance": 0.8
            }])
            .to_string();
            let _workers =
                crate::test_support::EnvRestore::set("TACHI_TEST_ENABLE_BACKGROUND_WORKERS", "1");
            let _persist = crate::test_support::EnvRestore::set(
                "TACHI_TEST_DISABLE_PROVIDER_KEY_HEALTH_PERSIST",
                "1",
            );
            let _voyage = crate::test_support::EnvRestore::remove("VOYAGE_API_KEY");
            let rt = tokio::runtime::Runtime::new().expect("runtime");
            rt.block_on(async move {
                let (port, _calls, mock) = spawn_capture_llm(vec![draft]).await;
                let _base = crate::test_support::EnvRestore::set(
                    "EXTRACT_BASE_URL",
                    &format!("http://127.0.0.1:{port}/chat/completions"),
                );
                let _model = crate::test_support::EnvRestore::set("EXTRACT_MODEL", "mock");
                let _key = crate::test_support::EnvRestore::set("EXTRACT_API_KEY", "test-key");
                let server = MemoryServer::new(global_db, Some(project_db)).expect("server");
                set_capture_failpoint(stage);
                let err = handle_capture_session(&server, params)
                    .await
                    .expect_err("armed failpoint must fail the first renew step");
                assert!(
                    err.contains(stage),
                    "error must name the failpoint stage, got: {err}"
                );

                let manifests = server
                    .with_project_store_read(|store| {
                        store
                            .list_state(CAPTURE_MANIFEST_NAMESPACE)
                            .map_err(|e| e.to_string())
                    })
                    .expect("read manifest state after failed renew");
                assert_eq!(
                    manifests.len(),
                    1,
                    "capture admission must have written exactly one manifest row"
                );
                let manifest: serde_json::Value =
                    serde_json::from_str(&manifests[0].value_json).expect("manifest JSON");
                assert_eq!(
                    manifest["owner"], "",
                    "renew failure must clear the manifest owner immediately via the \
                     structural gate, even though this exact site never had its own \
                     manual release call"
                );
                let lease_until = manifest["lease_until"]
                    .as_str()
                    .expect("lease_until is a string");
                let lease_until_dt = chrono::DateTime::parse_from_rfc3339(lease_until)
                    .expect("lease_until parses as RFC3339");
                assert!(
                    lease_until_dt <= chrono::Utc::now(),
                    "lease_until must already be in the past — proof the structural gate \
                     released it, not just the one previously-patched enqueue site"
                );
                mock.abort();
            });
            rt.shutdown_timeout(std::time::Duration::from_millis(500));
        });
    }

    /// Discrimination test for the reported "embed short-batch silently
    /// drops the tail" concern: the embedding-assignment code right after
    /// `embed_voyage_batch` is called (`entries.iter_mut().zip(vectors.iter())`,
    /// this file, immediately below the `server.llm.embed_voyage_batch(...)`
    /// call near the top of `handle_capture_session`) would silently
    /// under-embed the tail of a batch IF `embed_voyage_batch` could ever
    /// return `Ok(vectors)` with `vectors.len() < texts.len()`.
    ///
    /// It cannot, by construction: `parse_voyage_batch_embeddings`
    /// (`crates/tachi-llm/src/llm/embedding.rs:25-31`) rejects any chunk
    /// whose returned item count doesn't match the requested count, and
    /// `embed_voyage_batch` only ever returns `Ok` after every chunk has
    /// passed that check (`crates/tachi-llm/src/llm/embedding.rs:197,200`)
    /// — a short response always becomes `Err`, which this handler already
    /// treats as "defer the whole batch" (the `Err(err) => { ...; None }`
    /// arm right next to the zip). This test proves that guarantee
    /// end-to-end against a real (mocked) short HTTP response rather than
    /// resting on a reading of the tachi-llm source, so a future change
    /// that weakens the tachi-llm-side check would turn this test red
    /// instead of silently reintroducing the reported bug.
    #[test]
    fn short_voyage_batch_response_is_rejected_not_silently_truncated() {
        let _guard = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let rt = tokio::runtime::Runtime::new().expect("runtime");
        rt.block_on(async move {
            use axum::{routing::post, Json, Router};

            // Responds to every /v1/embeddings call with exactly ONE
            // embedding, regardless of how many inputs were requested — the
            // shape a provider would produce if it silently dropped part of
            // a batch.
            let app = Router::new().route(
                "/v1/embeddings",
                post(|| async move {
                    Json(serde_json::json!({
                        "data": [{
                            "index": 0,
                            "embedding": vec![0.0_f64; 1024],
                        }]
                    }))
                }),
            );
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .expect("bind short-batch voyage mock");
            let port = listener.local_addr().expect("voyage mock address").port();
            let mock = tokio::spawn(async move {
                axum::serve(listener, app)
                    .await
                    .expect("serve short-batch voyage mock");
            });
            tokio::task::yield_now().await;

            let _base = crate::test_support::EnvRestore::set(
                "VOYAGE_BASE_URL",
                &format!("http://127.0.0.1:{port}"),
            );
            let _key = crate::test_support::EnvRestore::set("VOYAGE_API_KEY", "test-voyage-key");
            let _attempts =
                crate::test_support::EnvRestore::set("TACHI_RECALL_PROVIDER_ATTEMPTS", "1");

            let client = tachi_llm::LlmClient::new().expect("client should initialize");
            let texts = vec!["first entry".to_string(), "second entry".to_string()];
            let err = client
                .embed_voyage_batch(&texts, "document")
                .await
                .expect_err(
                    "a 1-embedding response for a 2-text request must be rejected, not \
                     silently accepted as a short Ok batch",
                );
            assert!(
                err.contains("returned") && err.contains("embeddings") && err.contains("inputs"),
                "error should name the count mismatch, got: {err}"
            );

            mock.abort();
        });
    }

    #[test]
    fn legacy_archived_bracket_fixture_is_preserved_field_for_field_on_replay() {
        crate::test_support::with_tachi_home(|home| {
            let global_db = home.join("global").join("memory.db");
            let project_db = home
                .join("projects")
                .join("capture-legacy")
                .join("memory.db");
            std::fs::create_dir_all(global_db.parent().unwrap()).expect("mkdir global");
            std::fs::create_dir_all(project_db.parent().unwrap()).expect("mkdir project");
            let mut params = CaptureSessionParams {
                conversation_id: "legacy-conv-a".into(),
                turn_id: "legacy-turn-a".into(),
                agent_id: "legacy-agent".into(),
                messages: vec![Message {
                    role: "assistant".into(),
                    content: "（记住了旧规则已被纠正）".into(),
                }],
                path_prefix: None,
                scope: "project".into(),
                project: None,
                project_explicit: false,
                min_chars: 1,
                force: true,
            };
            let _workers =
                crate::test_support::EnvRestore::set("TACHI_TEST_ENABLE_BACKGROUND_WORKERS", "1");
            let _voyage = crate::test_support::EnvRestore::remove("VOYAGE_API_KEY");
            let rt = tokio::runtime::Runtime::new().expect("runtime");
            rt.block_on(async {
                let (port, _calls, mock) = spawn_capture_llm(vec!["[]".into()]).await;
                let _base = crate::test_support::EnvRestore::set("EXTRACT_BASE_URL", &format!("http://127.0.0.1:{port}/chat/completions"));
                let _model = crate::test_support::EnvRestore::set("EXTRACT_MODEL", "mock");
                let _key = crate::test_support::EnvRestore::set("EXTRACT_API_KEY", "test-key");
                let server = MemoryServer::new(global_db, Some(project_db)).expect("server");
                let receipt: serde_json::Value = serde_json::from_str(&handle_capture_session(&server, params.clone()).await.expect("seed")).unwrap();
                let id = receipt["ids"][0].as_str().unwrap().to_string();
                let mut fixture = server.with_project_store_read(|s| s.get(&id).map_err(|e| e.to_string())).unwrap().unwrap();
                let legacy_id = extract_bracket_self_evolution_notes(&params.agent_id, &params.messages)[0].id.clone();
                server.with_project_store(|s| s.delete(&id).map_err(|e| e.to_string())).expect("remove modern seed");
                fixture.id = legacy_id.clone();
                fixture.archived = true;
                fixture.metadata = serde_json::json!({"lifecycle":"corrected","authority":"legacy-owner","provenance":{"source":"archive"},"superseded_by":"replacement-7"});
                server.with_project_store(|s| s.upsert(&fixture).map_err(|e| e.to_string())).expect("install legacy fixture");
                let before = server.with_project_store_read(|s| s.get_with_options(&legacy_id, true).map_err(|e| e.to_string())).unwrap().unwrap();
                params.conversation_id = "legacy-conv-b".into();
                params.turn_id = "legacy-turn-b".into();
                let replay: serde_json::Value = serde_json::from_str(&handle_capture_session(&server, params).await.expect("replay")).unwrap();
                assert_eq!(replay["captured"], 0);
                let after = server.with_project_store_read(|s| s.get_with_options(&legacy_id, true).map_err(|e| e.to_string())).unwrap().unwrap();
                assert_eq!(serde_json::to_value(&before).unwrap(), serde_json::to_value(&after).unwrap());
                assert!(after.archived);
                mock.abort();
            });
        });
    }

    #[test]
    fn routed_db_supersession_tombstone_blocks_new_bracket_artifact() {
        crate::test_support::with_tachi_home(|home| {
            std::fs::write(
                home.join("routing.json"),
                r#"{"domain_routes":[{"project":"hapi","domains":["equity_trading"]}]}"#,
            )
            .expect("write routing");
            let global_db = home.join("global").join("memory.db");
            let quant_db = home.join("projects").join("quant").join("memory.db");
            let hapi_db = home.join("projects").join("hapi").join("memory.db");
            for path in [&global_db, &quant_db, &hapi_db] {
                std::fs::create_dir_all(path.parent().unwrap()).expect("mkdir DB parent");
            }
            memcore::MemoryStore::open_with_label(hapi_db.to_str().unwrap(), "hapi")
                .expect("mount hapi");
            let params = CaptureSessionParams {
                conversation_id: "routed-tombstone-conv".into(),
                turn_id: "turn-1".into(),
                agent_id: "routed-tombstone-agent".into(),
                messages: vec![Message {
                    role: "assistant".into(),
                    content: "（记住了这次交易复盘的重要经验，下次要更谨慎）".into(),
                }],
                path_prefix: Some("/trading/equity".into()),
                scope: "project".into(),
                project: None,
                project_explicit: false,
                min_chars: 1,
                force: true,
            };
            let discriminator =
                extract_bracket_self_evolution_notes(&params.agent_id, &params.messages)[0]
                    .id
                    .clone();
            let tombstone_id = "routed-bracket-tombstone";
            let tombstone = memcore::MemoryEntry {
                id: tombstone_id.into(),
                path: "/trading/equity/self-evolution".into(),
                summary: "superseded bracket evidence".into(),
                text: "superseded bracket evidence".into(),
                importance: 0.7,
                timestamp: "2026-07-19T00:00:00Z".into(),
                valid_from: String::new(),
                valid_until: None,
                category: "preference".into(),
                topic: "self_evolution".into(),
                keywords: vec!["bracket-note".into()],
                persons: Vec::new(),
                entities: Vec::new(),
                location: String::new(),
                source: "bracket_self_evolution".into(),
                scope: "project".into(),
                archived: false,
                access_count: 0,
                last_access: None,
                revision: 1,
                metadata: serde_json::json!({
                    "bracket_note_discriminator": discriminator,
                    "authority": "historical",
                }),
                vector: None,
                retention_policy: Some("durable".into()),
                domain: Some("equity_trading".into()),
                recall_count: 0,
                query_diversity: 0,
                tier: "raw".into(),
            };
            let server = MemoryServer::new(global_db, Some(quant_db)).expect("server");
            server
                .with_named_project_store("hapi", |store| {
                    store.upsert(&tombstone).map_err(|e| e.to_string())?;
                    store
                        .connection()
                        .execute(
                            "UPDATE memories SET superseded_by = 'replacement-9' WHERE id = ?1",
                            [tombstone_id],
                        )
                        .map_err(|e| e.to_string())?;
                    Ok(())
                })
                .expect("seed routed DB-column tombstone");
            let before: (String, i64, String) = server
                .with_named_project_store_read("hapi", |store| {
                    store
                        .connection()
                        .query_row(
                            "SELECT metadata, revision, superseded_by FROM memories WHERE id = ?1",
                            [tombstone_id],
                            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                        )
                        .map_err(|e| e.to_string())
                })
                .expect("snapshot tombstone");
            let _workers =
                crate::test_support::EnvRestore::set("TACHI_TEST_ENABLE_BACKGROUND_WORKERS", "1");
            let _voyage = crate::test_support::EnvRestore::remove("VOYAGE_API_KEY");
            let rt = tokio::runtime::Runtime::new().expect("runtime");
            rt.block_on(async {
                let (port, _calls, mock) = spawn_capture_llm(vec!["[]".into()]).await;
                let _base = crate::test_support::EnvRestore::set(
                    "EXTRACT_BASE_URL",
                    &format!("http://127.0.0.1:{port}/chat/completions"),
                );
                let _model = crate::test_support::EnvRestore::set("EXTRACT_MODEL", "mock");
                let _key = crate::test_support::EnvRestore::set("EXTRACT_API_KEY", "test-key");
                let receipt: serde_json::Value = serde_json::from_str(
                    &handle_capture_session(&server, params)
                        .await
                        .expect("suppressed capture"),
                )
                .expect("receipt");
                assert_eq!(receipt["captured"], 0);
                mock.abort();
            });
            let after: (String, i64, String) = server
                .with_named_project_store_read("hapi", |store| {
                    store
                        .connection()
                        .query_row(
                            "SELECT metadata, revision, superseded_by FROM memories WHERE id = ?1",
                            [tombstone_id],
                            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                        )
                        .map_err(|e| e.to_string())
                })
                .expect("snapshot routed tombstone after capture");
            assert_eq!(after, before, "suppression must not mutate the tombstone");
            for project in ["quant", "hapi"] {
                let active_new: i64 = server
                    .with_named_project_store_read(project, |store| {
                        store
                            .connection()
                            .query_row(
                                "SELECT COUNT(*) FROM memories
                                 WHERE id LIKE 'capture-session:%'
                                   AND archived = 0 AND superseded_by IS NULL",
                                [],
                                |row| row.get(0),
                            )
                            .map_err(|e| e.to_string())
                    })
                    .expect("count active bracket copies");
                assert_eq!(active_new, 0, "new active copy appeared in {project}");
            }
        });
    }

    /// #1301 LLM-draft discrimination: a source revision owns deterministic
    /// artifact slots. Completed receipts do not retain content and therefore
    /// do not restore an out-of-scope later deletion; the failpoint test above
    /// covers recovery while the admitted manifest is still incomplete.
    /// Exact replay is a no-op without resampling the model, and a different
    /// source event remains separately attributable.
    #[test]
    fn session_capture_replay_is_noop_after_completion_and_classifies_ephemeral() {
        crate::test_support::with_tachi_home(|home| {
            let global_db = home.join("global").join("memory.db");
            let project_db = home.join("projects").join("capture").join("memory.db");
            std::fs::create_dir_all(global_db.parent().unwrap()).expect("mkdir global");
            std::fs::create_dir_all(project_db.parent().unwrap()).expect("mkdir project");

            let params = CaptureSessionParams {
                conversation_id: "conv-drafts".to_string(),
                turn_id: "turn-1".to_string(),
                agent_id: "capture-draft-agent".to_string(),
                messages: vec![Message {
                    role: "user".to_string(),
                    content: "Record the two machine-generated session observations.".to_string(),
                }],
                path_prefix: Some("/capture/replay".to_string()),
                scope: "project".to_string(),
                project: None,
                project_explicit: false,
                min_chars: 1,
                force: true,
            };
            let drafts = serde_json::json!([
                {
                    "text": "The first extracted observation remains atomic.",
                    "summary": "First observation",
                    "topic": "session",
                    "category": "fact",
                    "scope": "project",
                    "importance": 0.7
                },
                {
                    "text": "The second extracted observation remains atomic.",
                    "summary": "Second observation",
                    "topic": "session",
                    "category": "fact",
                    "scope": "project",
                    "importance": 0.7
                }
            ])
            .to_string();

            let _workers =
                crate::test_support::EnvRestore::set("TACHI_TEST_ENABLE_BACKGROUND_WORKERS", "1");
            let _persist = crate::test_support::EnvRestore::set(
                "TACHI_TEST_DISABLE_PROVIDER_KEY_HEALTH_PERSIST",
                "1",
            );
            let _voyage = crate::test_support::EnvRestore::remove("VOYAGE_API_KEY");

            let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
            let (first, completed_replay, replay, changed_event, removed_id, server, calls) =
                rt.block_on(async move {
                    let mutated = serde_json::json!([{
                        "text": "A later model sample changed",
                        "summary": "changed",
                        "topic": "other",
                        "category": "fact",
                        "scope": "project",
                        "importance": 0.9
                    }])
                    .to_string();
                    let (port, calls, mock) = spawn_capture_llm(vec![drafts, mutated]).await;
                    let _base = crate::test_support::EnvRestore::set(
                        "EXTRACT_BASE_URL",
                        &format!("http://127.0.0.1:{port}/chat/completions"),
                    );
                    let _model =
                        crate::test_support::EnvRestore::set("EXTRACT_MODEL", "capture-draft-mock");
                    let _key = crate::test_support::EnvRestore::set("EXTRACT_API_KEY", "test-key");
                    let server =
                        MemoryServer::new(global_db, Some(project_db)).expect("capture server");

                    let first = handle_capture_session(&server, params.clone())
                        .await
                        .expect("first capture");
                    let first_json: serde_json::Value =
                        serde_json::from_str(&first).expect("first receipt JSON");
                    let removed_id = first_json["ids"][1]
                        .as_str()
                        .expect("second captured id")
                        .to_string();
                    server
                        .with_project_store(|store| {
                            store.delete(&removed_id).map_err(|e| e.to_string())
                        })
                        .expect("create a missing-artifact recovery fixture");

                    let completed_replay = handle_capture_session(&server, params.clone())
                        .await
                        .expect("completed replay after external deletion");
                    let replay = handle_capture_session(&server, params.clone())
                        .await
                        .expect("exact replay");
                    assert_eq!(
                        calls.load(std::sync::atomic::Ordering::SeqCst),
                        1,
                        "exact replay must not call LLM; its next payload is intentionally different"
                    );
                    let mut changed_params = params;
                    changed_params.turn_id = "turn-2".to_string();
                    let changed_event = handle_capture_session(&server, changed_params)
                        .await
                        .expect("different source event");
                    mock.abort();
                    (
                        first,
                        completed_replay,
                        replay,
                        changed_event,
                        removed_id,
                        server,
                        calls,
                    )
                });
            rt.shutdown_timeout(std::time::Duration::from_millis(500));

            let first: serde_json::Value = serde_json::from_str(&first).expect("first receipt");
            let completed_replay: serde_json::Value =
                serde_json::from_str(&completed_replay).expect("completed replay receipt");
            let replay: serde_json::Value = serde_json::from_str(&replay).expect("replay receipt");
            let changed_event: serde_json::Value =
                serde_json::from_str(&changed_event).expect("changed-event receipt");
            assert_eq!(
                calls.load(std::sync::atomic::Ordering::SeqCst),
                2,
                "exact replay/recovery must not resample; only first and changed event call LLM"
            );

            assert_eq!(first["captured"], 2, "first receipt: {first}");
            assert_eq!(
                completed_replay["captured"], 0,
                "completed replay receipt: {completed_replay}"
            );
            assert_eq!(
                completed_replay["duplicates_skipped"], 2,
                "completed replay receipt: {completed_replay}"
            );
            assert_eq!(replay["captured"], 0, "exact replay receipt: {replay}");
            assert_eq!(
                replay["duplicates_skipped"], 2,
                "exact replay receipt: {replay}"
            );
            assert_eq!(
                changed_event["captured"], 1,
                "new source event must remain attributable: {changed_event}"
            );
            assert_ne!(
                first["ids"], changed_event["ids"],
                "source event identity must participate in artifact ids"
            );

            for id in first["ids"].as_array().expect("first ids") {
                let id = id.as_str().expect("string id");
                if id == removed_id {
                    assert!(
                        server
                            .with_project_store_read(|store| {
                                store.get(id).map_err(|e| e.to_string())
                            })
                            .expect("read externally deleted row")
                            .is_none(),
                        "completed replay must not resurrect an out-of-scope deletion"
                    );
                    continue;
                }
                let stored = server
                    .with_project_store_read(|store| store.get(id).map_err(|e| e.to_string()))
                    .expect("read capture row")
                    .expect("capture row exists");
                assert_eq!(stored.revision, 1, "replay must not update {id}");
                assert_eq!(stored.retention_policy.as_deref(), Some("ephemeral"));
                let timestamp = chrono::DateTime::parse_from_rfc3339(&stored.timestamp)
                    .expect("capture timestamp");
                let expires = chrono::DateTime::parse_from_rfc3339(
                    stored.valid_until.as_deref().expect("ephemeral expiry"),
                )
                .expect("capture expiry");
                assert_eq!((expires - timestamp).num_days(), CAPTURE_EPHEMERAL_TTL_DAYS);
                assert_eq!(
                    stored.metadata["capture_retention"]["policy_version"],
                    CAPTURE_RETENTION_POLICY_VERSION
                );
                assert!(stored.metadata["source_revision"].as_str().is_some());
                assert_eq!(stored.metadata["source_refs"][0]["ref_type"], "turn");
            }
        });
    }

    // `with_tachi_home` is a plain sync closure (it restores TACHI_HOME the
    // instant the closure returns) — not `#[tokio::test]`-compatible
    // directly. Every other capture_session test in this file relies on
    // that sync-closure shape, so rather than reach for a different
    // env-var pattern for just this one test, this test stays `#[test]`
    // and blocks on the handler's async body from INSIDE the closure, where
    // TACHI_HOME is still set.
    #[test]
    fn handle_capture_session_infers_domain_from_entry_and_reroutes() {
        crate::test_support::with_tachi_home(|home| {
            std::fs::write(
                home.join("routing.json"),
                r#"{"domain_routes":[{"project":"hapi","domains":["equity_trading"]}]}"#,
            )
            .expect("write routing.json");

            let quant_db = home.join("projects").join("quant").join("memory.db");
            std::fs::create_dir_all(quant_db.parent().unwrap()).expect("mkdir quant");
            let global_db = home.join("global").join("memory.db");
            std::fs::create_dir_all(global_db.parent().unwrap()).expect("mkdir global");

            let hapi_db = home.join("projects").join("hapi").join("memory.db");
            std::fs::create_dir_all(hapi_db.parent().unwrap()).expect("mkdir hapi");
            memcore::MemoryStore::open_with_label(hapi_db.to_str().expect("utf-8 db path"), "hapi")
                .expect("init hapi schema");

            let params = CaptureSessionParams {
                conversation_id: "conv-1".to_string(),
                turn_id: "turn-1".to_string(),
                agent_id: "handler-test-agent".to_string(),
                messages: vec![Message {
                    role: "assistant".to_string(),
                    // A bracket self-evolution note: matches the "记住了"
                    // strategy pattern `extract_bracket_self_evolution_notes`
                    // requires — this path needs NO live LLM call, unlike
                    // the session-capture draft path.
                    content: "（记住了这次交易复盘的重要经验，下次要更谨慎）".to_string(),
                }],
                // #1114: this path prefix is what drives the HANDLER's own
                // domain inference (`infer_domain_from_row`'s `/trading/
                // equity` prefix rule) to "equity_trading" — the test never
                // tells the gate what domain to use.
                path_prefix: Some("/trading/equity".to_string()),
                scope: "project".to_string(),
                project: None,
                project_explicit: false,
                min_chars: 1,
                force: true,
            };

            // Oz r5 fixture fix: `handle_capture_session` unconditionally
            // calls `enqueue_capture_maintenance_jobs`, which `try_send`s
            // onto the foundry-maintenance mpsc channel — under `#[cfg(test)]`,
            // `background_workers_enabled()` (server_state/init.rs:37-46)
            // defaults OFF unless `TACHI_TEST_ENABLE_BACKGROUND_WORKERS` is
            // set, so `MemoryServer::new` never spawns the worker that would
            // hold the receiver open; the sender side is immediately
            // disconnected and the real handler call fails with "foundry
            // maintenance worker unavailable". This is a TEST-fixture gap,
            // not a production one — the fix is enabling the real worker for
            // this test, NOT teaching the handler to tolerate a missing one
            // (that would mask a genuinely dead worker in production, the
            // same "拒必有声" reasoning the write-affinity gate itself
            // follows). `MemoryServer::new` spawns via `tokio::spawn` when
            // workers are enabled, so it — and everything else — now runs
            // INSIDE the same `block_on`'d runtime, not constructed before
            // it: calling `tokio::spawn` with no active runtime context
            // panics.
            let _background_workers =
                crate::test_support::EnvRestore::set("TACHI_TEST_ENABLE_BACKGROUND_WORKERS", "1");

            // The spawned foundry-maintenance worker's `while let Some(item)
            // = rx.recv().await` loop (maintenance/worker.rs:120) only ever
            // exits when its Sender is dropped — but `server` (which owns
            // it) is kept alive below for the post-capture read assertions,
            // so that task genuinely never finishes on its own within this
            // test's lifetime. Explicitly bounding shutdown here (rather
            // than trusting `Runtime`'s implicit `Drop` to reap a
            // still-running spawned task within some unstated time budget)
            // is what guarantees this test can never hang instead of
            // failing fast.
            let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
            let (response, server) = rt.block_on(async move {
                let server =
                    MemoryServer::new(global_db, Some(quant_db)).expect("bind quant daemon");
                let response = handle_capture_session(&server, params)
                    .await
                    .expect("capture_session should complete");
                (response, server)
            });
            rt.shutdown_timeout(std::time::Duration::from_millis(500));
            let parsed: serde_json::Value = serde_json::from_str(&response).expect("response JSON");
            assert_eq!(
                parsed["captured"].as_u64(),
                Some(1),
                "the bracket note must be captured: {parsed}"
            );

            let in_hapi = server
                .with_named_project_store_read("hapi", |store| {
                    store
                        .list_by_path("/trading/equity", 10, false)
                        .map_err(|e| e.to_string())
                })
                .expect("read hapi");
            assert_eq!(
                in_hapi.len(),
                1,
                "equity_trading content (inferred by the HANDLER from the entry's \
                 own path, not injected by this test) must reroute into hapi: {in_hapi:?}"
            );

            let in_quant = server
                .with_project_store_read(|store| {
                    store
                        .list_by_path("/trading/equity", 10, false)
                        .map_err(|e| e.to_string())
                })
                .expect("read quant");
            assert!(
                in_quant.is_empty(),
                "must NOT silently land in the daemon's own bound (quant) store: {in_quant:?}"
            );
        });
    }

    // #1114 codex round-3 item 3 point ③, capture_session-side (label +
    // by_destination fallback): DELIBERATELY NOT covered by a new runtime
    // test this round. `emit_session_captured_event` (what such a test
    // would check) runs AFTER `enqueue_capture_maintenance_jobs` within the
    // SAME per-destination-group loop iteration — unlike
    // `handle_capture_session_infers_domain_from_entry_and_reroutes` above
    // (which only needs state written BEFORE that call), there is no way to
    // observe this fix without the maintenance-enqueue call actually
    // succeeding, which needs `TACHI_TEST_ENABLE_BACKGROUND_WORKERS`
    // enabled. That would reintroduce EXACTLY the process-env-var
    // concurrency race item 5 was fixed for (`EnvRestore` prevents the
    // value LEAKING after this test ends; it does not prevent OTHER,
    // concurrently-running tests that don't hold `global_test_lock` from
    // OBSERVING it while set — and per codex's own grep,
    // `server_state/init.rs`'s tests around lines 371-379 are exactly such
    // tests) — multiplying the exact class of risk this round exists to
    // eliminate, not adding a new instance of it. A safe test would need
    // either (a) reordering `capture_session.rs` so the continuity event is
    // written before maintenance-enqueue, or (b) extracting the
    // destination-group loop body into a directly-testable seam — both are
    // production changes, out of scope for a test-quality-only round. The
    // fix itself (verified correct by codex's own production-logic review
    // this round) stays confirmed by code inspection only:
    // `emit_session_captured_event`'s call now passes `group_named_project
    // .as_deref()` instead of the stale `params.project.as_deref()`, and
    // `primary_session_event`/`primary_pipeline` fall back to whichever
    // destination group actually exists instead of a hardcoded "skipped"
    // placeholder — see capture_session.rs's own inline doc comments at
    // those two call sites for the reasoning.
}
