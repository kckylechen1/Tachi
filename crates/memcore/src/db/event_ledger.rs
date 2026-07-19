use std::collections::BTreeMap;

use rusqlite::{params, Connection, OptionalExtension};

use crate::error::MemoryError;
use crate::types::{
    AuthorityLevel, ContinuityMetrics, EffectScope, MetricCount, OutcomeEvidenceBasis,
    ProjectionKind, SessionOutcomeKind, SessionOutcomeMetrics, TachiEventQuery, TachiEventRecord,
};

fn trim_filter(value: &Option<String>) -> Option<&str> {
    value.as_deref().map(str::trim).filter(|s| !s.is_empty())
}

fn event_limit(limit: usize) -> i64 {
    limit.clamp(1, 500) as i64
}

fn table_exists(conn: &Connection, name: &str) -> bool {
    conn.query_row(
        "SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1",
        [name],
        |row| row.get::<_, i64>(0),
    )
    .is_ok()
}

fn event_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<TachiEventRecord> {
    let authority_raw: String = row.get("authority")?;
    let effects_raw: String = row.get("effects")?;
    let hints_raw: String = row.get("projection_hints")?;
    let payload_raw: String = row.get("payload_json")?;
    let provenance_raw: String = row.get("provenance_json")?;

    let effect_names: Vec<String> = serde_json::from_str(&effects_raw).unwrap_or_default();
    let hint_names: Vec<String> = serde_json::from_str(&hints_raw).unwrap_or_default();
    let payload = serde_json::from_str(&payload_raw).unwrap_or_else(|_| serde_json::json!({}));
    let provenance =
        serde_json::from_str(&provenance_raw).unwrap_or_else(|_| serde_json::json!({}));

    Ok(TachiEventRecord {
        id: row.get("id")?,
        source_repo: row.get("source_repo")?,
        adapter: row.get("adapter")?,
        project: row.get("project")?,
        domain: row.get("domain")?,
        session_id: row.get("session_id")?,
        actor: row.get("actor")?,
        event_type: row.get("event_type")?,
        authority: AuthorityLevel::from_str_opt(Some(&authority_raw)),
        effects: effect_names
            .iter()
            .map(|value| EffectScope::from_str_opt(Some(value)))
            .collect(),
        projection_hints: hint_names
            .iter()
            .filter_map(|value| ProjectionKind::from_str_opt(Some(value)))
            .collect(),
        payload,
        provenance,
        created_at: row.get("created_at")?,
    })
}

pub fn insert_tachi_event(conn: &Connection, event: &TachiEventRecord) -> Result<(), MemoryError> {
    let effects = event
        .effects
        .iter()
        .map(|value| value.as_str())
        .collect::<Vec<_>>();
    let projection_hints = event
        .projection_hints
        .iter()
        .map(|value| value.as_str())
        .collect::<Vec<_>>();
    let effects_json = serde_json::to_string(&effects)?;
    let projection_hints_json = serde_json::to_string(&projection_hints)?;
    let payload_json = serde_json::to_string(&event.payload)?;
    let provenance_json = serde_json::to_string(&event.provenance)?;

    conn.execute(
        "INSERT INTO tachi_events (
            id, source_repo, adapter, project, domain, session_id, actor,
            event_type, authority, effects, projection_hints,
            payload_json, provenance_json, created_at
         )
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
        params![
            &event.id,
            &event.source_repo,
            &event.adapter,
            &event.project,
            &event.domain,
            &event.session_id,
            &event.actor,
            &event.event_type,
            event.authority.as_str(),
            effects_json,
            projection_hints_json,
            payload_json,
            provenance_json,
            &event.created_at,
        ],
    )?;
    Ok(())
}

pub fn insert_tachi_event_if_absent(
    conn: &Connection,
    event: &TachiEventRecord,
) -> Result<bool, MemoryError> {
    let effects = event.effects.iter().map(|v| v.as_str()).collect::<Vec<_>>();
    let hints = event
        .projection_hints
        .iter()
        .map(|v| v.as_str())
        .collect::<Vec<_>>();
    let changed = conn.execute(
        "INSERT INTO tachi_events (id, source_repo, adapter, project, domain, session_id, actor,
         event_type, authority, effects, projection_hints, payload_json, provenance_json, created_at)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)
         ON CONFLICT(id) DO NOTHING",
        params![&event.id, &event.source_repo, &event.adapter, &event.project, &event.domain,
            &event.session_id, &event.actor, &event.event_type, event.authority.as_str(),
            serde_json::to_string(&effects)?, serde_json::to_string(&hints)?,
            serde_json::to_string(&event.payload)?, serde_json::to_string(&event.provenance)?,
            &event.created_at],
    )?;
    if changed == 1 {
        return Ok(true);
    }
    let existing = conn
        .query_row(
            "SELECT id, source_repo, adapter, project, domain, session_id, actor,
                event_type, authority, effects, projection_hints, payload_json,
                provenance_json, created_at
         FROM tachi_events WHERE id = ?1",
            [&event.id],
            event_row,
        )
        .optional()?;
    if let Some(mut existing) = existing {
        // Wall-clock insertion time is not event content. Stable identity must
        // protect every semantic field without turning a retry into conflict.
        existing.created_at = event.created_at.clone();
        let same = serde_json::to_value(&existing)? == serde_json::to_value(event)?;
        if same {
            return Ok(false);
        }
        return Err(MemoryError::InvalidArg(format!(
            "tachi event id collision: {}",
            event.id
        )));
    }
    Err(MemoryError::Internal(format!(
        "tachi event '{}' disappeared after id conflict",
        event.id
    )))
}

pub fn list_tachi_events(
    conn: &Connection,
    query: &TachiEventQuery,
) -> Result<Vec<TachiEventRecord>, MemoryError> {
    let mut stmt = conn.prepare(
        "SELECT
            id, source_repo, adapter, project, domain, session_id, actor,
            event_type, authority, effects, projection_hints,
            payload_json, provenance_json, created_at
         FROM tachi_events
         WHERE (?1 IS NULL OR project = ?1)
           AND (?2 IS NULL OR domain = ?2)
           AND (?3 IS NULL OR event_type = ?3)
           AND (?4 IS NULL OR session_id = ?4)
           AND (?5 IS NULL OR source_repo = ?5)
           AND (?6 IS NULL OR adapter = ?6)
         ORDER BY created_at DESC, id DESC
         LIMIT ?7",
    )?;
    let rows = stmt.query_map(
        params![
            trim_filter(&query.project),
            trim_filter(&query.domain),
            trim_filter(&query.event_type),
            trim_filter(&query.session_id),
            trim_filter(&query.source_repo),
            trim_filter(&query.adapter),
            event_limit(query.limit),
        ],
        event_row,
    )?;

    let mut events = Vec::new();
    for row in rows {
        events.push(row?);
    }
    Ok(events)
}

fn payload_str<'a>(payload: &'a serde_json::Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter()
        .find_map(|key| payload.get(*key).and_then(|value| value.as_str()))
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn metric_counts(map: BTreeMap<String, usize>) -> Vec<MetricCount> {
    map.into_iter()
        .map(|(label, count)| MetricCount { label, count })
        .collect()
}

pub fn continuity_metrics(
    conn: &Connection,
    window_event_limit: usize,
) -> Result<ContinuityMetrics, MemoryError> {
    let limit = window_event_limit.clamp(1, 500);
    let mut session_outcomes = SessionOutcomeMetrics {
        window_event_limit: limit,
        note: "read-only signal; not a verdict or routing gate".to_string(),
        ..SessionOutcomeMetrics::default()
    };

    if !table_exists(conn, "tachi_events") {
        return Ok(ContinuityMetrics { session_outcomes });
    }

    let query = TachiEventQuery {
        event_type: Some("session.outcome".to_string()),
        limit,
        ..TachiEventQuery::default()
    };
    let events = list_tachi_events(conn, &query)?;
    let mut labels = BTreeMap::<String, usize>::new();
    let mut basis_counts = BTreeMap::<String, usize>::new();

    for event in events {
        let outcome = SessionOutcomeKind::from_str_opt(payload_str(
            &event.payload,
            &["outcome", "outcome_label", "label"],
        ));
        let basis = OutcomeEvidenceBasis::from_str_opt(payload_str(
            &event.payload,
            &[
                "evidence_basis",
                "basis",
                "label_basis",
                "adversarial_basis",
            ],
        ));

        session_outcomes.outcome_events += 1;
        *labels.entry(outcome.as_str().to_string()).or_default() += 1;
        *basis_counts.entry(basis.as_str().to_string()).or_default() += 1;

        if outcome.is_challenge_rate_eligible() {
            session_outcomes.eligible_outcomes += 1;
        }
        if outcome == SessionOutcomeKind::AiCorrected {
            session_outcomes.ai_corrected += 1;
        }
    }

    session_outcomes.challenge_rate = (session_outcomes.eligible_outcomes > 0)
        .then(|| session_outcomes.ai_corrected as f64 / session_outcomes.eligible_outcomes as f64);
    session_outcomes.labels = metric_counts(labels);
    session_outcomes.evidence_basis = metric_counts(basis_counts);

    Ok(ContinuityMetrics { session_outcomes })
}
