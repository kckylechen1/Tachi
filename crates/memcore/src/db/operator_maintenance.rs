use super::memory_crud::{delete_memory_within_tx, refuse_reserved_rem_operation_mutation};
use crate::db::{self, StoreProfile};
use crate::error::MemoryError;
use crate::types::GcConfig;
use rusqlite::types::{Value, ValueRef};
use rusqlite::{params, params_from_iter, Connection, TransactionBehavior};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

const A2A_BODY_SCRUB_BATCH: usize = 100;
const KANBAN_CATEGORY: &str = "kanban";
const KANBAN_PATH_PREFIX: &str = "/kanban/";
const KANBAN_DISPATCH_PATH_PREFIX: &str = "/kanban/tasks/";
const KANBAN_DISPATCH_NON_TERMINAL_STATES: &[&str] = &[
    "TASK_STATE_WORKING",
    "TASK_STATE_PENDING",
    "TASK_STATE_INPUT_REQUIRED",
];

/// Closed accounting registry for the only GC operation this module serves.
pub const OPERATOR_GC_CLASSES: &[&str] = &[
    "access_history_quota",
    "processed_events_age",
    "audit_log_age_or_cap",
    "agent_known_state_age",
    "access_history_orphan",
    "query_diversity_reconcile",
    "query_diversity_excluded_protected",
    "agent_known_state_orphan",
    "recall_impression_groups_age_or_quota",
    "a2a_terminal_body_scrub",
    "kanban_terminal_or_stale",
    "kanban_excluded_protected",
];

/// Closed accounting registry for canonical exact-id deletion.
pub const OPERATOR_DELETE_CLASSES: &[&str] = &[
    "memory_row",
    "memory_fts",
    "memory_symbolic_fts",
    "memory_vector",
    "memory_edges",
    "access_history",
    "agent_known_state",
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MaintenanceClassFact {
    pub class: String,
    pub count: usize,
    pub digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Mutation {
    DeleteRowids {
        table: &'static str,
        rowids: Vec<i64>,
    },
    Diversity(Vec<(String, i64)>),
    ScrubA2a(Vec<String>),
    DeleteKanban(Vec<String>),
    None,
}

#[derive(Debug, Clone)]
struct CandidateClass {
    fact: MaintenanceClassFact,
    mutation: Mutation,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GcMaintenanceOutcome {
    pub source: Vec<MaintenanceClassFact>,
    pub post: Vec<MaintenanceClassFact>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeleteMaintenanceOutcome {
    pub source: Vec<MaintenanceClassFact>,
    pub post: Vec<MaintenanceClassFact>,
    pub deleted: bool,
}

fn encode_value(value: ValueRef<'_>, out: &mut Vec<u8>) {
    match value {
        ValueRef::Null => out.push(0),
        ValueRef::Integer(value) => {
            out.push(1);
            out.extend_from_slice(&value.to_le_bytes());
        }
        ValueRef::Real(value) => {
            out.push(2);
            out.extend_from_slice(&value.to_bits().to_le_bytes());
        }
        ValueRef::Text(value) => {
            out.push(3);
            out.extend_from_slice(&(value.len() as u64).to_le_bytes());
            out.extend_from_slice(value);
        }
        ValueRef::Blob(value) => {
            out.push(4);
            out.extend_from_slice(&(value.len() as u64).to_le_bytes());
            out.extend_from_slice(value);
        }
    }
}

fn digest_fingerprints(mut fingerprints: Vec<Vec<u8>>) -> String {
    fingerprints.sort();
    let mut hasher = Sha256::new();
    for fingerprint in fingerprints {
        hasher.update((fingerprint.len() as u64).to_le_bytes());
        hasher.update(fingerprint);
    }
    format!("{:x}", hasher.finalize())
}

fn candidate_from_query(
    conn: &Connection,
    class: &str,
    sql: &str,
    values: Vec<Value>,
    mutation: impl FnOnce(Vec<String>) -> Result<Mutation, MemoryError>,
) -> Result<CandidateClass, MemoryError> {
    let mut stmt = conn.prepare(sql)?;
    let column_count = stmt.column_count();
    let mut rows = stmt.query(params_from_iter(values.iter()))?;
    let mut keys = Vec::new();
    let mut fingerprints = Vec::new();
    while let Some(row) = rows.next()? {
        let first = row.get_ref(0)?;
        keys.push(match first {
            ValueRef::Integer(value) => value.to_string(),
            ValueRef::Text(value) => String::from_utf8_lossy(value).into_owned(),
            other => {
                return Err(MemoryError::Internal(format!(
                    "maintenance candidate {class} has non-key first column {other:?}"
                )))
            }
        });
        let mut fingerprint = Vec::new();
        for index in 0..column_count {
            encode_value(row.get_ref(index)?, &mut fingerprint);
        }
        fingerprints.push(fingerprint);
    }
    let fact = MaintenanceClassFact {
        class: class.to_string(),
        count: fingerprints.len(),
        digest: digest_fingerprints(fingerprints),
    };
    Ok(CandidateClass {
        fact,
        mutation: mutation(keys)?,
    })
}

fn rowid_candidate(
    conn: &Connection,
    class: &str,
    table: &'static str,
    sql: &str,
    values: Vec<Value>,
) -> Result<CandidateClass, MemoryError> {
    candidate_from_query(conn, class, sql, values, |keys| {
        let rowids = keys
            .into_iter()
            .map(|key| {
                key.parse::<i64>().map_err(|error| {
                    MemoryError::Internal(format!(
                        "maintenance candidate {class} rowid {key:?} is invalid: {error}"
                    ))
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Mutation::DeleteRowids { table, rowids })
    })
}

fn empty_or_none_class(class: &str, fingerprints: Vec<Vec<u8>>) -> CandidateClass {
    CandidateClass {
        fact: MaintenanceClassFact {
            class: class.to_string(),
            count: fingerprints.len(),
            digest: digest_fingerprints(fingerprints),
        },
        mutation: Mutation::None,
    }
}

fn validate_registry(
    facts: &[MaintenanceClassFact],
    expected: &[&str],
    operation: &str,
) -> Result<(), MemoryError> {
    if facts.len() == expected.len()
        && facts
            .iter()
            .map(|fact| fact.class.as_str())
            .eq(expected.iter().copied())
    {
        Ok(())
    } else {
        Err(MemoryError::Internal(format!(
            "operator {operation} class registry is incomplete or reordered"
        )))
    }
}

fn cutoff_modifier(days: u64) -> String {
    format!("-{days} days")
}

pub fn is_kanban_gc_candidate(
    category: &str,
    path: &str,
    metadata_json: &str,
    timestamp: &str,
    as_of: &str,
    max_age_days: u64,
) -> Result<bool, MemoryError> {
    let metadata: serde_json::Value = serde_json::from_str(metadata_json).map_err(|error| {
        MemoryError::InvalidArg(format!("parse kanban metadata failed: {error}"))
    })?;
    let reapable = if category == KANBAN_CATEGORY {
        metadata
            .get("status")
            .and_then(serde_json::Value::as_str)
            .map(|status| status.trim().to_ascii_lowercase())
            .is_some_and(|status| matches!(status.as_str(), "resolved" | "expired"))
    } else if path.starts_with(KANBAN_DISPATCH_PATH_PREFIX) {
        metadata
            .get("a2a_state")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|state| {
                KANBAN_DISPATCH_NON_TERMINAL_STATES.contains(&state)
                    && !(state == "TASK_STATE_INPUT_REQUIRED"
                        && metadata
                            .get("closure_kind")
                            .and_then(serde_json::Value::as_str)
                            == Some("partial"))
            })
    } else {
        false
    };
    if !reapable {
        return Ok(false);
    }
    let as_of = chrono::DateTime::parse_from_rfc3339(as_of)
        .map_err(|error| MemoryError::InvalidArg(format!("invalid GC as_of: {error}")))?
        .with_timezone(&chrono::Utc);
    let timestamp = chrono::DateTime::parse_from_rfc3339(timestamp)
        .map_err(|error| {
            MemoryError::InvalidArg(format!("parse kanban timestamp failed: {error}"))
        })?
        .with_timezone(&chrono::Utc);
    let days = i64::try_from(max_age_days).unwrap_or(i64::MAX);
    Ok(timestamp < as_of - chrono::Duration::days(days))
}

fn collect_gc_candidates(
    conn: &Connection,
    cfg: &GcConfig,
    profile: StoreProfile,
    as_of: &str,
    kanban_max_age_days: u64,
    include_kanban: bool,
) -> Result<Vec<CandidateClass>, MemoryError> {
    chrono::DateTime::parse_from_rfc3339(as_of)
        .map_err(|error| MemoryError::InvalidArg(format!("invalid GC as_of: {error}")))?;
    let product = profile.includes_product();
    let mut candidates = Vec::new();

    let access_quota = rowid_candidate(
        conn,
        "access_history_quota",
        "access_history",
        "SELECT CAST(rowid AS TEXT),memory_id,event_kind,accessed_at,query_hash
           FROM (SELECT rowid,memory_id,event_kind,accessed_at,query_hash,
                        ROW_NUMBER() OVER (
                          PARTITION BY memory_id,event_kind
                          ORDER BY accessed_at DESC
                        ) AS rn
                   FROM access_history)
          WHERE rn > ?1 ORDER BY rowid",
        vec![Value::Integer(
            i64::try_from(cfg.access_history_keep_per_memory).unwrap_or(i64::MAX),
        )],
    )?;
    let quota_rowids = match &access_quota.mutation {
        Mutation::DeleteRowids { rowids, .. } => rowids.iter().copied().collect::<BTreeSet<_>>(),
        _ => BTreeSet::new(),
    };
    candidates.push(access_quota);

    candidates.push(rowid_candidate(
        conn,
        "processed_events_age",
        "processed_events",
        "SELECT CAST(rowid AS TEXT),event_hash,event_id,worker,created_at
           FROM processed_events
          WHERE created_at < STRFTIME('%Y-%m-%dT%H:%M:%fZ',?1,?2)
          ORDER BY rowid",
        vec![
            Value::Text(as_of.to_string()),
            Value::Text(cutoff_modifier(u64::from(cfg.processed_events_max_days))),
        ],
    )?);

    if product {
        candidates.push(rowid_candidate(
            conn,
            "audit_log_age_or_cap",
            "audit_log",
            "SELECT CAST(rowid AS TEXT),id,created_at
               FROM audit_log
              WHERE created_at < STRFTIME('%Y-%m-%dT%H:%M:%fZ',?1,?2)
                 OR id NOT IN (
                    SELECT id FROM audit_log
                     WHERE created_at >= STRFTIME('%Y-%m-%dT%H:%M:%fZ',?1,?2)
                     ORDER BY id DESC LIMIT ?3)
              ORDER BY id",
            vec![
                Value::Text(as_of.to_string()),
                Value::Text(cutoff_modifier(u64::from(cfg.audit_log_max_days))),
                Value::Integer(i64::try_from(cfg.audit_log_max_rows).unwrap_or(i64::MAX)),
            ],
        )?);
        candidates.push(rowid_candidate(
            conn,
            "agent_known_state_age",
            "agent_known_state",
            "SELECT CAST(rowid AS TEXT),agent_id,memory_id,revision,synced_at
               FROM agent_known_state
              WHERE synced_at < STRFTIME('%Y-%m-%dT%H:%M:%fZ',?1,?2)
              ORDER BY rowid",
            vec![
                Value::Text(as_of.to_string()),
                Value::Text(cutoff_modifier(u64::from(cfg.agent_known_state_max_days))),
            ],
        )?);
    } else {
        candidates.push(empty_or_none_class("audit_log_age_or_cap", Vec::new()));
        candidates.push(empty_or_none_class("agent_known_state_age", Vec::new()));
    }

    let (orphan_access_sql, orphan_access_values) = if quota_rowids.is_empty() {
        (
            "SELECT CAST(rowid AS TEXT),memory_id,event_kind,accessed_at,query_hash
               FROM access_history
              WHERE memory_id NOT IN (SELECT id FROM memories)
              ORDER BY rowid"
                .to_string(),
            Vec::new(),
        )
    } else {
        let placeholders = std::iter::repeat_n("?", quota_rowids.len())
            .collect::<Vec<_>>()
            .join(",");
        (
            format!(
                "SELECT CAST(rowid AS TEXT),memory_id,event_kind,accessed_at,query_hash
                   FROM access_history
                  WHERE memory_id NOT IN (SELECT id FROM memories)
                    AND rowid NOT IN ({placeholders})
                  ORDER BY rowid"
            ),
            quota_rowids.iter().copied().map(Value::Integer).collect(),
        )
    };
    let orphan_access = rowid_candidate(
        conn,
        "access_history_orphan",
        "access_history",
        &orphan_access_sql,
        orphan_access_values,
    )?;
    let orphan_rowids = match &orphan_access.mutation {
        Mutation::DeleteRowids { rowids, .. } => rowids.iter().copied().collect::<BTreeSet<_>>(),
        _ => BTreeSet::new(),
    };
    candidates.push(orphan_access);

    let mut diversity_by_memory: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    {
        let mut stmt = conn.prepare(
            "SELECT rowid,memory_id,query_hash FROM access_history WHERE query_hash != ''",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;
        for row in rows {
            let (rowid, memory_id, query_hash) = row?;
            if !quota_rowids.contains(&rowid) && !orphan_rowids.contains(&rowid) {
                diversity_by_memory
                    .entry(memory_id)
                    .or_default()
                    .insert(query_hash);
            }
        }
    }
    let mut diversity = Vec::new();
    let mut diversity_fingerprints = Vec::new();
    let mut excluded_diversity = Vec::new();
    {
        let mut stmt = conn.prepare("SELECT id,query_diversity FROM memories ORDER BY id")?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?;
        for row in rows {
            let (id, current) = row?;
            let desired = diversity_by_memory
                .get(&id)
                .map_or(0, |hashes| hashes.len() as i64);
            let mut fingerprint = Vec::new();
            encode_value(ValueRef::Text(id.as_bytes()), &mut fingerprint);
            encode_value(ValueRef::Integer(current), &mut fingerprint);
            encode_value(ValueRef::Integer(desired), &mut fingerprint);
            let guard = refuse_reserved_rem_operation_mutation(&id, "query-diversity reconciled")
                .and_then(|()| {
                    db::refuse_retired_sticky_row_within_tx(conn, &id, "query-diversity reconciled")
                });
            match guard {
                Ok(()) => {
                    diversity.push((id, desired));
                    diversity_fingerprints.push(fingerprint);
                }
                Err(MemoryError::InvalidArg(_)) => excluded_diversity.push(fingerprint),
                Err(error) => return Err(error),
            }
        }
    }
    candidates.push(CandidateClass {
        fact: MaintenanceClassFact {
            class: "query_diversity_reconcile".to_string(),
            count: diversity.len(),
            digest: digest_fingerprints(diversity_fingerprints),
        },
        mutation: Mutation::Diversity(diversity),
    });
    candidates.push(empty_or_none_class(
        "query_diversity_excluded_protected",
        excluded_diversity,
    ));

    if product {
        candidates.push(rowid_candidate(
            conn,
            "agent_known_state_orphan",
            "agent_known_state",
            "SELECT CAST(rowid AS TEXT),agent_id,memory_id,revision,synced_at
               FROM agent_known_state
              WHERE memory_id NOT IN (SELECT id FROM memories)
                AND synced_at >= STRFTIME('%Y-%m-%dT%H:%M:%fZ',?1,?2)
              ORDER BY rowid",
            vec![
                Value::Text(as_of.to_string()),
                Value::Text(cutoff_modifier(u64::from(cfg.agent_known_state_max_days))),
            ],
        )?);
    } else {
        candidates.push(empty_or_none_class("agent_known_state_orphan", Vec::new()));
    }

    candidates.push(rowid_candidate(
        conn,
        "recall_impression_groups_age_or_quota",
        "recall_impression_groups",
        "SELECT CAST(rowid AS TEXT),*
           FROM recall_impression_groups
          WHERE created_at < STRFTIME('%Y-%m-%dT%H:%M:%fZ',?1,?2)
             OR group_id NOT IN (
                SELECT group_id FROM recall_impression_groups
                 WHERE created_at >= STRFTIME('%Y-%m-%dT%H:%M:%fZ',?1,?2)
                 ORDER BY created_at DESC,group_id DESC LIMIT ?3)
          ORDER BY group_id",
        vec![
            Value::Text(as_of.to_string()),
            Value::Text(cutoff_modifier(u64::from(cfg.recall_impression_max_days))),
            Value::Integer(i64::try_from(cfg.recall_impression_max_groups).unwrap_or(i64::MAX)),
        ],
    )?);

    if product {
        candidates.push(candidate_from_query(
            conn,
            "a2a_terminal_body_scrub",
            "SELECT e.envelope_id,e.current_state,e.state_version,r.occurred_at,e.body
               FROM a2a_envelopes e
               JOIN a2a_delivery_receipts r
                 ON r.envelope_id=e.envelope_id
                AND r.state=e.current_state
                AND r.envelope_version=e.state_version
              WHERE e.body IS NOT NULL
                AND e.current_state IN ('consumed','expired')
                AND unixepoch(r.occurred_at,'+90 days') <= unixepoch(?1)
              ORDER BY r.occurred_at,e.envelope_id LIMIT ?2",
            vec![
                Value::Text(as_of.to_string()),
                Value::Integer(A2A_BODY_SCRUB_BATCH as i64),
            ],
            |ids| Ok(Mutation::ScrubA2a(ids)),
        )?);
    } else {
        candidates.push(empty_or_none_class("a2a_terminal_body_scrub", Vec::new()));
    }

    let mut kanban_ids = Vec::new();
    let mut kanban_fingerprints = Vec::new();
    let mut excluded_kanban = Vec::new();
    if include_kanban {
        let mut stmt = conn.prepare(
            "SELECT id,path,category,timestamp,metadata FROM memories
              WHERE path LIKE ?1 ORDER BY id",
        )?;
        let rows = stmt.query_map([format!("{KANBAN_PATH_PREFIX}%")], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
            ))
        })?;
        for row in rows {
            let (id, path, category, timestamp, metadata) = row?;
            if !is_kanban_gc_candidate(
                &category,
                &path,
                &metadata,
                &timestamp,
                as_of,
                kanban_max_age_days,
            )? {
                continue;
            }
            let mut fingerprint = Vec::new();
            for value in [&id, &path, &category, &timestamp, &metadata] {
                encode_value(ValueRef::Text(value.as_bytes()), &mut fingerprint);
            }
            let guard = refuse_reserved_rem_operation_mutation(&id, "deleted by kanban GC")
                .and_then(|()| {
                    db::refuse_retired_sticky_row_within_tx(conn, &id, "deleted by kanban GC")
                });
            match guard {
                Ok(()) => {
                    kanban_ids.push(id);
                    kanban_fingerprints.push(fingerprint);
                }
                Err(MemoryError::InvalidArg(_)) => excluded_kanban.push(fingerprint),
                Err(error) => return Err(error),
            }
        }
    }
    candidates.push(CandidateClass {
        fact: MaintenanceClassFact {
            class: "kanban_terminal_or_stale".to_string(),
            count: kanban_ids.len(),
            digest: digest_fingerprints(kanban_fingerprints),
        },
        mutation: Mutation::DeleteKanban(kanban_ids),
    });
    candidates.push(empty_or_none_class(
        "kanban_excluded_protected",
        excluded_kanban,
    ));

    Ok(candidates)
}

pub(crate) fn gc_candidate_facts(
    conn: &Connection,
    cfg: &GcConfig,
    profile: StoreProfile,
    as_of: &str,
    kanban_max_age_days: u64,
    include_kanban: bool,
) -> Result<Vec<MaintenanceClassFact>, MemoryError> {
    let facts = collect_gc_candidates(
        conn,
        cfg,
        profile,
        as_of,
        kanban_max_age_days,
        include_kanban,
    )?
    .into_iter()
    .map(|candidate| candidate.fact)
    .collect::<Vec<_>>();
    validate_registry(&facts, OPERATOR_GC_CLASSES, "GC")?;
    Ok(facts)
}

fn apply_candidates(
    conn: &Connection,
    candidates: &[CandidateClass],
    vec_available: bool,
    profile: StoreProfile,
) -> Result<(), MemoryError> {
    for candidate in candidates {
        match &candidate.mutation {
            Mutation::DeleteRowids { table, rowids } => {
                let sql = format!("DELETE FROM {table} WHERE rowid=?1");
                for rowid in rowids {
                    conn.execute(&sql, [rowid])?;
                }
            }
            Mutation::Diversity(rows) => {
                for (id, desired) in rows {
                    conn.execute(
                        "UPDATE memories SET query_diversity=?1 WHERE id=?2",
                        params![desired, id],
                    )?;
                }
            }
            Mutation::ScrubA2a(ids) => {
                for id in ids {
                    conn.execute(
                        "UPDATE a2a_envelopes SET body=NULL WHERE envelope_id=?1",
                        [id],
                    )?;
                }
            }
            Mutation::DeleteKanban(ids) => {
                for id in ids {
                    delete_memory_within_tx(conn, id, vec_available, profile)?;
                }
            }
            Mutation::None => {}
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn apply_gc_candidate_facts<F>(
    conn: &mut Connection,
    cfg: &GcConfig,
    profile: StoreProfile,
    vec_available: bool,
    as_of: &str,
    kanban_max_age_days: u64,
    include_kanban: bool,
    expected: &[MaintenanceClassFact],
    before_commit: F,
) -> Result<GcMaintenanceOutcome, MemoryError>
where
    F: FnOnce(
        &Connection,
        &[MaintenanceClassFact],
        &[MaintenanceClassFact],
    ) -> Result<(), MemoryError>,
{
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let source_candidates = collect_gc_candidates(
        &tx,
        cfg,
        profile,
        as_of,
        kanban_max_age_days,
        include_kanban,
    )?;
    let source = source_candidates
        .iter()
        .map(|candidate| candidate.fact.clone())
        .collect::<Vec<_>>();
    if source != expected {
        return Err(MemoryError::InvalidArg(
            "maintenance GC source facts changed after planning".to_string(),
        ));
    }
    apply_candidates(&tx, &source_candidates, vec_available, profile)?;
    let post = gc_candidate_facts(
        &tx,
        cfg,
        profile,
        as_of,
        kanban_max_age_days,
        include_kanban,
    )?;
    before_commit(&tx, &source, &post)?;
    tx.commit()?;
    Ok(GcMaintenanceOutcome { source, post })
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn apply_operator_gc_candidate_facts<F>(
    conn: &mut Connection,
    cfg: &GcConfig,
    profile: StoreProfile,
    vec_available: bool,
    as_of: &str,
    kanban_max_age_days: u64,
    include_kanban: bool,
    expected: &[MaintenanceClassFact],
    authority_key: &str,
    before_commit: F,
) -> Result<GcMaintenanceOutcome, MemoryError>
where
    F: FnOnce(
        &Connection,
        &[MaintenanceClassFact],
        &[MaintenanceClassFact],
    ) -> Result<String, MemoryError>,
{
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let source_candidates = collect_gc_candidates(
        &tx,
        cfg,
        profile,
        as_of,
        kanban_max_age_days,
        include_kanban,
    )?;
    let source = source_candidates
        .iter()
        .map(|candidate| candidate.fact.clone())
        .collect::<Vec<_>>();
    if source != expected {
        return Err(MemoryError::InvalidArg(
            "maintenance GC source facts changed after planning".to_string(),
        ));
    }
    apply_candidates(&tx, &source_candidates, vec_available, profile)?;
    let post = gc_candidate_facts(
        &tx,
        cfg,
        profile,
        as_of,
        kanban_max_age_days,
        include_kanban,
    )?;
    let authority_json = before_commit(&tx, &source, &post)?;
    insert_operator_maintenance_authority(&tx, authority_key, &authority_json)?;
    tx.commit()?;
    Ok(GcMaintenanceOutcome { source, post })
}

fn fact_from_query(
    conn: &Connection,
    class: &str,
    sql: &str,
    id: &str,
) -> Result<MaintenanceClassFact, MemoryError> {
    Ok(
        candidate_from_query(conn, class, sql, vec![Value::Text(id.to_string())], |_| {
            Ok(Mutation::None)
        })?
        .fact,
    )
}

pub(crate) fn delete_candidate_facts(
    conn: &Connection,
    id: &str,
    vec_available: bool,
    profile: StoreProfile,
) -> Result<Vec<MaintenanceClassFact>, MemoryError> {
    let id = id.trim();
    if id.is_empty() {
        return Err(MemoryError::InvalidArg("empty ID".to_string()));
    }
    refuse_reserved_rem_operation_mutation(id, "deleted")?;
    db::refuse_retired_sticky_row_within_tx(conn, id, "deleted")?;
    let mut facts = vec![
        fact_from_query(
            conn,
            "memory_row",
            "SELECT id,* FROM memories WHERE id=?1",
            id,
        )?,
        fact_from_query(
            conn,
            "memory_fts",
            "SELECT rowid,* FROM memories_fts WHERE id=?1",
            id,
        )?,
        fact_from_query(
            conn,
            "memory_symbolic_fts",
            "SELECT rowid,* FROM memories_symbolic_fts WHERE id=?1",
            id,
        )?,
    ];
    facts.push(if vec_available {
        fact_from_query(
            conn,
            "memory_vector",
            "SELECT id,embedding FROM memories_vec WHERE id=?1",
            id,
        )?
    } else {
        empty_or_none_class("memory_vector", Vec::new()).fact
    });
    facts.push(fact_from_query(
        conn,
        "memory_edges",
        "SELECT rowid,* FROM memory_edges WHERE source_id=?1 OR target_id=?1",
        id,
    )?);
    facts.push(fact_from_query(
        conn,
        "access_history",
        "SELECT rowid,* FROM access_history WHERE memory_id=?1",
        id,
    )?);
    facts.push(if profile.includes_product() {
        fact_from_query(
            conn,
            "agent_known_state",
            "SELECT rowid,* FROM agent_known_state WHERE memory_id=?1",
            id,
        )?
    } else {
        empty_or_none_class("agent_known_state", Vec::new()).fact
    });
    validate_registry(&facts, OPERATOR_DELETE_CLASSES, "delete")?;
    Ok(facts)
}

#[cfg(test)]
pub(crate) fn apply_delete_candidate_facts<F>(
    conn: &mut Connection,
    id: &str,
    vec_available: bool,
    profile: StoreProfile,
    expected: &[MaintenanceClassFact],
    before_commit: F,
) -> Result<DeleteMaintenanceOutcome, MemoryError>
where
    F: FnOnce(
        &Connection,
        &[MaintenanceClassFact],
        &[MaintenanceClassFact],
    ) -> Result<(), MemoryError>,
{
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let source = delete_candidate_facts(&tx, id, vec_available, profile)?;
    if source != expected {
        return Err(MemoryError::InvalidArg(
            "maintenance delete source facts changed after planning".to_string(),
        ));
    }
    let deleted = delete_memory_within_tx(&tx, id, vec_available, profile)?;
    let post = delete_candidate_facts(&tx, id, vec_available, profile)?;
    before_commit(&tx, &source, &post)?;
    tx.commit()?;
    Ok(DeleteMaintenanceOutcome {
        source,
        post,
        deleted,
    })
}

pub(crate) fn apply_operator_delete_candidate_facts<F>(
    conn: &mut Connection,
    id: &str,
    vec_available: bool,
    profile: StoreProfile,
    expected: &[MaintenanceClassFact],
    authority_key: &str,
    before_commit: F,
) -> Result<DeleteMaintenanceOutcome, MemoryError>
where
    F: FnOnce(
        &Connection,
        &[MaintenanceClassFact],
        &[MaintenanceClassFact],
    ) -> Result<String, MemoryError>,
{
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let source = delete_candidate_facts(&tx, id, vec_available, profile)?;
    if source != expected {
        return Err(MemoryError::InvalidArg(
            "maintenance delete source facts changed after planning".to_string(),
        ));
    }
    let deleted = delete_memory_within_tx(&tx, id, vec_available, profile)?;
    let post = delete_candidate_facts(&tx, id, vec_available, profile)?;
    let authority_json = before_commit(&tx, &source, &post)?;
    insert_operator_maintenance_authority(&tx, authority_key, &authority_json)?;
    tx.commit()?;
    Ok(DeleteMaintenanceOutcome {
        source,
        post,
        deleted,
    })
}

fn insert_operator_maintenance_authority(
    conn: &Connection,
    key: &str,
    value_json: &str,
) -> Result<(), MemoryError> {
    if key.len() != 64 || !key.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(MemoryError::InvalidArg(
            "operator maintenance authority key must be a SHA-256 plan digest".to_string(),
        ));
    }
    serde_json::from_str::<serde_json::Value>(value_json).map_err(|error| {
        MemoryError::InvalidArg(format!(
            "operator maintenance committed authority is not valid JSON: {error}"
        ))
    })?;
    let now = db::now_utc_iso();
    conn.execute(
        "INSERT INTO hard_state (namespace, key, value_json, version, created_at, updated_at)
         VALUES (?1, ?2, ?3, 1, ?4, ?4)",
        params![
            super::state::OPERATOR_MAINTENANCE_RECEIPT_NAMESPACE,
            key,
            value_json,
            now
        ],
    )?;
    Ok(())
}

pub(crate) fn operator_maintenance_authority(
    conn: &Connection,
    key: &str,
) -> Result<Option<String>, MemoryError> {
    let result = conn.query_row(
        "SELECT value_json, version FROM hard_state WHERE namespace=?1 AND key=?2",
        params![super::state::OPERATOR_MAINTENANCE_RECEIPT_NAMESPACE, key],
        |row| Ok((row.get::<_, String>(0)?, row.get::<_, u32>(1)?)),
    );
    match result {
        Ok((value, 1)) => Ok(Some(value)),
        Ok((_value, version)) => Err(MemoryError::InvalidArg(format!(
            "operator maintenance authority has invalid version {version}"
        ))),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(error) => Err(error.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{schema, StoreProfile};
    use crate::types::GcConfig;
    use rusqlite::Connection;

    fn product_connection() -> Connection {
        let _ = libsimple::enable_auto_extension();
        crate::db::register_sqlite_vec();
        let conn = Connection::open_in_memory().expect("open maintenance fixture");
        schema::init_schema(&conn).expect("initialize current product schema");
        conn
    }

    #[test]
    fn gc_preview_and_apply_share_frozen_source_facts_and_precommit_boundary() {
        let mut conn = product_connection();
        conn.execute(
            "INSERT INTO processed_events(event_hash,event_id,worker,created_at)
             VALUES ('old-hash','old-event','worker','2020-01-01T00:00:00Z')",
            [],
        )
        .expect("seed old processed event");
        let cfg = GcConfig::default();
        let as_of = "2026-08-13T00:00:00Z";

        let plan = gc_candidate_facts(&conn, &cfg, StoreProfile::TachiFull, as_of, 30, true)
            .expect("select-only GC preview");
        assert_eq!(
            plan.iter()
                .find(|fact| fact.class == "processed_events_age")
                .map(|fact| fact.count),
            Some(1)
        );

        let mut precommit_observed = false;
        let outcome = apply_gc_candidate_facts(
            &mut conn,
            &cfg,
            StoreProfile::TachiFull,
            true,
            as_of,
            30,
            true,
            &plan,
            |tx, source, post| {
                precommit_observed = true;
                assert_eq!(source, plan.as_slice());
                assert_eq!(
                    tx.query_row::<i64, _, _>(
                        "SELECT COUNT(*) FROM processed_events WHERE event_hash='old-hash'",
                        [],
                        |row| row.get(0),
                    )
                    .expect("read inside transaction"),
                    0,
                    "mutation must already be staged before durable prepared receipt callback"
                );
                assert_eq!(
                    post.iter()
                        .find(|fact| fact.class == "processed_events_age")
                        .map(|fact| fact.count),
                    Some(0)
                );
                Ok(())
            },
        )
        .expect("apply exact frozen candidates");
        assert!(precommit_observed);
        assert_eq!(outcome.source, plan);
        assert_eq!(
            conn.query_row::<i64, _, _>("SELECT COUNT(*) FROM processed_events", [], |row| {
                row.get(0)
            })
            .unwrap(),
            0
        );
    }

    #[test]
    fn exact_delete_missing_is_noop_and_existing_removes_associated_rows() {
        let mut conn = product_connection();
        conn.execute(
            "INSERT INTO memories(id,path,summary,text,timestamp,created_at,updated_at)
             VALUES ('delete-me','/test','private summary','private body',
                     '2026-01-01T00:00:00Z','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z')",
            [],
        )
        .expect("seed memory");
        conn.execute(
            "INSERT INTO access_history(memory_id,accessed_at,event_kind,query_hash)
             VALUES ('delete-me','2026-01-02T00:00:00Z','display','q')",
            [],
        )
        .expect("seed associated state");

        let missing = delete_candidate_facts(&conn, "missing", false, StoreProfile::TachiFull)
            .expect("missing plan");
        assert!(missing.iter().all(|fact| fact.count == 0));

        let plan = delete_candidate_facts(&conn, "delete-me", false, StoreProfile::TachiFull)
            .expect("existing delete plan");
        assert_eq!(plan[0].class, "memory_row");
        assert_eq!(plan[0].count, 1);
        let outcome = apply_delete_candidate_facts(
            &mut conn,
            "delete-me",
            false,
            StoreProfile::TachiFull,
            &plan,
            |_tx, _source, post| {
                assert!(post.iter().all(|fact| fact.count == 0));
                Ok(())
            },
        )
        .expect("delete exact id");
        assert!(outcome.deleted);
        assert_eq!(
            conn.query_row::<i64, _, _>(
                "SELECT COUNT(*) FROM access_history WHERE memory_id='delete-me'",
                [],
                |row| row.get(0),
            )
            .unwrap(),
            0
        );
    }

    #[test]
    fn gc_registry_is_complete_for_empty_zero_and_product_off_inputs() {
        let conn = product_connection();
        let zero = GcConfig {
            access_history_keep_per_memory: 0,
            processed_events_max_days: 0,
            audit_log_max_days: 0,
            audit_log_max_rows: 0,
            agent_known_state_max_days: 0,
            recall_impression_max_groups: 0,
            recall_impression_max_days: 0,
        };
        let facts = gc_candidate_facts(
            &conn,
            &zero,
            StoreProfile::PortableKernel,
            "2026-08-13T00:00:00Z",
            0,
            true,
        )
        .unwrap();
        assert_eq!(
            facts
                .iter()
                .map(|fact| fact.class.as_str())
                .collect::<Vec<_>>(),
            OPERATOR_GC_CLASSES
        );
        assert!(facts.iter().all(|fact| fact.count == 0));
    }

    #[test]
    fn gc_accounts_for_protected_diversity_while_an_allowed_peer_progresses() {
        let mut conn = product_connection();
        for (id, path) in [
            ("ordinary-diversity", "/notes/ordinary"),
            (
                "wiki-rem:operation:protected",
                "/wiki-rem/operation/protected",
            ),
        ] {
            conn.execute(
                "INSERT INTO memories(
                    id,path,summary,text,timestamp,created_at,updated_at,query_diversity
                 ) VALUES (?1,?2,'summary','body','2026-08-01T00:00:00Z',
                           '2026-08-01T00:00:00Z','2026-08-01T00:00:00Z',99)",
                params![id, path],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO access_history(memory_id,accessed_at,event_kind,query_hash)
                 VALUES (?1,'2026-08-02T00:00:00Z','display','query')",
                [id],
            )
            .unwrap();
        }
        let cfg = GcConfig::default();
        let as_of = "2026-08-13T00:00:00Z";
        let plan =
            gc_candidate_facts(&conn, &cfg, StoreProfile::TachiFull, as_of, 30, true).unwrap();
        assert_eq!(
            plan.iter()
                .find(|fact| fact.class == "query_diversity_excluded_protected")
                .map(|fact| fact.count),
            Some(1),
            "every unsafe candidate must be typed rather than silently omitted"
        );
        apply_gc_candidate_facts(
            &mut conn,
            &cfg,
            StoreProfile::TachiFull,
            true,
            as_of,
            30,
            true,
            &plan,
            |_tx, _source, _post| Ok(()),
        )
        .unwrap();
        let diversity = |id: &str| {
            conn.query_row(
                "SELECT query_diversity FROM memories WHERE id=?1",
                [id],
                |row| row.get::<_, i64>(0),
            )
            .unwrap()
        };
        assert_eq!(diversity("ordinary-diversity"), 1);
        assert_eq!(diversity("wiki-rem:operation:protected"), 99);
    }

    #[test]
    fn kanban_predicate_is_strict_at_cutoff_and_malformed_metadata_is_loud() {
        let as_of = "2026-08-13T00:00:00Z";
        assert!(!is_kanban_gc_candidate(
            "kanban",
            "/kanban/cards/equal",
            r#"{"status":"resolved"}"#,
            "2026-07-14T00:00:00Z",
            as_of,
            30,
        )
        .unwrap());
        assert!(is_kanban_gc_candidate(
            "kanban",
            "/kanban/cards/old",
            r#"{"status":"resolved"}"#,
            "2026-07-13T23:59:59Z",
            as_of,
            30,
        )
        .unwrap());
        assert!(is_kanban_gc_candidate(
            "kanban",
            "/kanban/cards/bad",
            "not-json",
            "2026-07-01T00:00:00Z",
            as_of,
            30,
        )
        .is_err());
    }
}
