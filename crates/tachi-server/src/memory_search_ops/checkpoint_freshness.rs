//! Read-time hints for dated agent checkpoints; durable lifecycle stays intact.

use crate::tool_params::SearchMemoryParams;
use chrono::{NaiveDate, Utc};
use memcore::{MemoryEntry, MemoryStore, SearchResult};
use rusqlite::{params, OptionalExtension};
use serde_json::{json, Value};

// SearchResult has no response-only extension map. This key is temporary,
// replaced on every search and removed when slim_search_result emits the hint.
pub(crate) const TRANSIENT_KEY: &str = "_checkpoint_read_freshness";
const PREFIX: &str = "/agent/checkpoints/";

pub(crate) fn is_checkpoint_path(path: &str) -> bool {
    path == PREFIX.trim_end_matches('/') || path.starts_with(PREFIX)
}

fn checkpoint_date(path: &str) -> Option<&str> {
    let date = path.strip_prefix(PREFIX)?;
    (date.len() == 10 && NaiveDate::parse_from_str(date, "%Y-%m-%d").is_ok()).then_some(date)
}

/// Compare only attributed, canonical dated checkpoints in this physical
/// store, scope, domain and authoring project/workspace. Legacy unattributed
/// rows and custom checkpoint paths do not imply a shared agent or timeline.
/// Path dates win over ingestion time: importing an old checkpoint is not a
/// newer observation. Same-day snapshots remain peers, not replacements.
pub(crate) fn annotation(
    store: &MemoryStore,
    entry: &MemoryEntry,
    include_test: bool,
    as_of: Option<&str>,
) -> Result<Option<Value>, String> {
    let Some(date) = checkpoint_date(&entry.path) else {
        return Ok(None);
    };
    let provenance = &entry.metadata["provenance"];
    let agent = provenance["agent_identity_id"]
        .as_str()
        .filter(|value| !value.is_empty())
        .or_else(|| {
            provenance
                .pointer("/agent/agent_id")
                .and_then(Value::as_str)
        })
        .filter(|value| !value.trim().is_empty());
    let Some(agent) = agent else {
        return Ok(None);
    };
    let cutoff = as_of
        .map(str::to_owned)
        .unwrap_or_else(|| Utc::now().to_rfc3339());
    // This exact namespace cannot overlap reserved test paths; only the
    // explicit JSON boolean fixture marker remains relevant before LIMIT.
    let sql = "SELECT id, substr(path, 20) AS checkpoint_date
         FROM memories
         WHERE path GLOB '/agent/checkpoints/[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]'
           AND substr(path, 20) > ?1
           AND date(substr(path, 20), '+0 days') = substr(path, 20)
           AND date(substr(path, 20)) <= date(?2)
           AND archived = 0 AND superseded_by IS NULL AND revision > 0
           AND julianday(timestamp) <= julianday(?2)
           AND julianday(COALESCE(NULLIF(valid_from, ''), timestamp)) <= julianday(?2)
           AND (valid_until IS NULL OR julianday(valid_until) > julianday(?2))
           AND scope = ?3 AND domain IS ?4
           AND json_valid(metadata)
           AND COALESCE(NULLIF(json_extract(metadata, '$.provenance.agent_identity_id'), ''),
                        json_extract(metadata, '$.provenance.agent.agent_id')) = ?5
           AND json_extract(metadata, '$.provenance.session_project') IS ?6
           AND json_extract(metadata, '$.provenance.project_id') IS ?7
           AND json_extract(metadata, '$.provenance.workspace_id') IS ?8
           AND (?9 OR json_type(metadata, '$.synthetic_fixture') IS NOT 'true')
         ORDER BY path DESC, timestamp DESC, id ASC
         LIMIT 1";
    let newer: Option<(String, String)> = store
        .connection()
        .prepare_cached(sql)
        .and_then(|mut stmt| {
            stmt.query_row(
                params![
                    date,
                    cutoff,
                    entry.scope,
                    entry.domain,
                    agent,
                    provenance["session_project"].as_str(),
                    provenance["project_id"].as_str(),
                    provenance["workspace_id"].as_str(),
                    include_test,
                ],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
        })
        .map_err(|error| format!("checkpoint freshness lookup failed: {error}"))?;
    Ok(newer.map(|(id, newer_date)| {
        json!({
            "status": "older_checkpoint",
            "basis": "checkpoint_path_date",
            "checkpoint_date": date,
            "newer_checkpoint_id": id,
            "newer_checkpoint_date": newer_date,
            "read_time_only": true,
        })
    }))
}

pub(crate) fn annotate_search_results(
    store: &MemoryStore,
    request: &SearchMemoryParams,
    results: &mut [SearchResult],
) -> Result<(), String> {
    let opted_in = request
        .path_prefix
        .as_deref()
        .is_some_and(is_checkpoint_path);
    for result in results {
        // A persisted/caller-supplied key cannot impersonate this read hint.
        if let Some(metadata) = result.entry.metadata.as_object_mut() {
            metadata.remove(TRANSIENT_KEY);
        }
        // Role-specific sandbox rules are applied later. Do not disclose an
        // off-result checkpoint reference before that authorization boundary.
        if !opted_in
            || request
                .agent_role
                .as_deref()
                .is_some_and(|role| !role.trim().is_empty())
        {
            continue;
        }
        if let Some(hint) = annotation(
            store,
            &result.entry,
            request.include_test,
            request.as_of.as_deref(),
        )? {
            if let Some(metadata) = result.entry.metadata.as_object_mut() {
                metadata.insert(TRANSIENT_KEY.to_string(), hint);
            }
        }
    }
    Ok(())
}

pub(crate) fn markdown_suffix(row: &Value) -> String {
    let hint = &row["checkpoint_freshness"];
    if hint["status"].as_str() != Some("older_checkpoint") {
        return String::new();
    }
    hint["newer_checkpoint_date"]
        .as_str()
        .filter(|date| NaiveDate::parse_from_str(date, "%Y-%m-%d").is_ok())
        .map(|date| format!(" [older checkpoint; newer checkpoint: {date}]"))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn checkpoint(id: &str, date: &str, agent: &str, project: &str) -> MemoryEntry {
        let mut entry = crate::tests::make_entry(id);
        entry.path = format!("{PREFIX}{date}");
        entry.text = format!("Checkpoint decision evidence for {id}");
        entry.summary = entry.text.clone();
        entry.timestamp = format!("{date}T10:00:00Z");
        entry.scope = "project".to_string();
        entry.importance = 0.8;
        entry.retention_policy = Some("durable".to_string());
        entry.metadata = json!({"provenance": {
            "agent_identity_id": agent, "session_project": project,
            "project_id": "project-id", "workspace_id": "workspace-id",
        }});
        entry
    }

    #[test]
    fn checkpoint_freshness_respects_agent_scope_project_and_fixture_boundaries() {
        let server = crate::tests::make_server();
        server
            .with_global_store(|store| {
                let old = checkpoint("old", "2026-05-30", "alice", "Tachi");
                let newest = checkpoint("newest", "2026-07-09", "alice", "Tachi");
                let mut different_scope = checkpoint("user", "2026-08-01", "alice", "Tachi");
                different_scope.scope = "user".to_string();
                let mut fixture = checkpoint("fixture", "2026-10-01", "alice", "Tachi");
                fixture.metadata["synthetic_fixture"] = json!(true);
                for entry in [
                    old.clone(),
                    newest.clone(),
                    different_scope,
                    fixture,
                    checkpoint("bob", "2026-08-02", "bob", "Tachi"),
                    checkpoint("other-project", "2026-08-03", "alice", "Other"),
                ] {
                    store.upsert(&entry).map_err(|error| error.to_string())?;
                }
                let cutoff = Some("2026-11-01T00:00:00Z");
                let hint = annotation(store, &old, false, cutoff)?.expect("older checkpoint hint");
                assert_eq!(hint["newer_checkpoint_id"], "newest");
                assert!(annotation(store, &newest, false, cutoff)?.is_none());
                assert_eq!(
                    annotation(store, &old, true, cutoff)?.unwrap()["newer_checkpoint_id"],
                    "fixture",
                );
                assert!(annotation(store, &old, false, Some("2026-06-01T00:00:00Z"))?.is_none());
                let mut anonymous = old.clone();
                anonymous.metadata = json!({});
                assert!(annotation(store, &anonymous, false, cutoff)?.is_none());
                Ok(())
            })
            .unwrap();
    }

    #[test]
    fn checkpoint_recency_limit_and_get_keep_history_while_status_marks_late_import() {
        let (server, _project_db) =
            crate::tests::make_server_with_project_fixture("checkpoint-freshness");
        let mut old = checkpoint("late-import", "2026-05-30", "alice", "Tachi");
        old.timestamp = "2026-10-20T10:00:00Z".to_string();
        let newer = checkpoint("newer-date", "2026-07-09", "alice", "Tachi");
        server
            .with_project_store(|store| {
                for entry in [&old, &newer] {
                    store.upsert(entry).map_err(|error| error.to_string())?;
                }
                // The latest checkpoint is outside the recency LIMIT, and its
                // date is older than the late import's ingestion timestamp.
                let recent = store
                    .list_by_path_recent_for_recall(PREFIX, 1, false, false)
                    .map_err(|error| error.to_string())?;
                assert_eq!(recent[0].id, old.id);
                let hint = annotation(store, &recent[0], false, None)?.unwrap();
                assert_eq!(hint["newer_checkpoint_id"], newer.id);
                let fetched = store
                    .get(&old.id)
                    .map_err(|error| error.to_string())?
                    .unwrap();
                assert_eq!(fetched.metadata, old.metadata);
                assert_eq!(fetched.importance, 0.8);
                assert_eq!(fetched.retention_policy.as_deref(), Some("durable"));
                assert_eq!(fetched.access_count, 0);
                assert_eq!(fetched.revision, 1);
                let successor: Option<String> = store
                    .connection()
                    .query_row(
                        "SELECT superseded_by FROM memories WHERE id = ?1",
                        [&old.id],
                        |row| row.get(0),
                    )
                    .map_err(|error| error.to_string())?;
                assert!(
                    successor.is_none(),
                    "read hint must not persist supersession"
                );
                Ok(())
            })
            .unwrap();
        // Identical attribution in another physical store cannot supersede
        // either project row, even when its checkpoint date is newer.
        server
            .with_global_store(|store| {
                store
                    .upsert(&checkpoint("other-db", "2026-09-01", "alice", "Tachi"))
                    .map_err(|error| error.to_string())
            })
            .unwrap();
        let rows = crate::status_ops::list_recent_checkpoint_entries(&server, 1, false);
        assert_eq!(rows[0]["id"], old.id);
        assert_eq!(
            rows[0]["checkpoint_freshness"]["newer_checkpoint_id"],
            newer.id
        );
    }

    #[test]
    fn checkpoint_dates_ignore_invalid_expired_archived_and_same_day_successors() {
        let server = crate::tests::make_server();
        server
            .with_global_store(|store| {
                let old = checkpoint("old", "2026-02-01", "alice", "Tachi");
                let mut invalid = checkpoint("invalid-day", "2026-02-30", "alice", "Tachi");
                invalid.timestamp = "2026-03-01T00:00:00Z".to_string();
                let mut expired = checkpoint("expired", "2026-03-01", "alice", "Tachi");
                expired.valid_until = Some("2026-03-02T00:00:00Z".to_string());
                let mut archived = checkpoint("archived", "2026-03-03", "alice", "Tachi");
                archived.archived = true;
                let mut same_day = checkpoint("same-day", "2026-02-01", "alice", "Tachi");
                same_day.timestamp = "2026-02-01T23:00:00Z".to_string();
                for entry in [&old, &invalid, &expired, &archived, &same_day] {
                    store.upsert(entry).map_err(|error| error.to_string())?;
                }
                assert!(annotation(store, &old, false, Some("2026-04-01T00:00:00Z"))?.is_none());
                Ok(())
            })
            .unwrap();
    }
}
