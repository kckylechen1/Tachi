//! SQL for the six model-broker catalog tables (tachi#1681 D1/D7).
//!
//! Row types live in [`crate::catalog`], mirroring the split
//! `vault::accounts` / `db::vault_accounts` already uses for provider
//! accounts.
//!
//! # Two shapes deliberately absent
//!
//! There is **no generic update and no delete** on catalog metadata, for the
//! same reason `db::vault_accounts` has neither: the audit value of
//! `model_deployment_events` evaporates the moment a caller can rewrite a
//! deployment row without saying why. Every write here is a named transition —
//! import a deployment, advance it because its content changed, retire it —
//! and each one appends its event in the same call.
//!
//! That absence is also what makes discrimination 5 structural rather than
//! aspirational: **a probe or auth failure can never erase catalog metadata**,
//! because the only writes this module offers that a failure path could reach
//! are health writes (PR-C, `model_deployment_health`, a different table) and
//! event appends. No accessor exists that could null out a `context_window`
//! or drop a deployment because a key stopped working.
//!
//! # Transactions
//!
//! These accessors never open a transaction of their own, so they compose
//! inside a caller's write transaction (an inner `BEGIN` would fail outright —
//! SQLite has no nested transactions). The multi-statement ones document what
//! a caller outside a transaction risks.

use rusqlite::{params, Connection, OptionalExtension};

use crate::catalog::fold::CatalogProjection;
use crate::catalog::{
    partition_authoritative_at, AttachmentBounds, AuthoritativePartition, CatalogSource,
    DeploymentCapabilities, DeploymentEventKind, ModelAlias, ModelAliasBinding, ModelDeployment,
    ModelDeploymentEvent, ModelDeploymentHealth, NewModelDeployment, NewModelDeploymentEvent,
    PricingSnapshot, ProtocolKind, DEPLOYMENT_STATUS_RETIRED,
};
use crate::error::MemoryError;
use crate::vault::health::EvidenceKind;

use super::common::now_utc_iso;

const DEPLOYMENT_COLUMNS: &str = "deployment_id, provider_account_id, endpoint_ref, \
     protocol_kind, provider_model_id, effective_version, capabilities, context_window, \
     max_output, attachment_bounds, region, data_policy, pricing_snapshot_ref, catalog_source, \
     fetched_at, effective_at, expires_at, status, revision, source_refs, created_at, updated_at";

const EVENT_COLUMNS: &str =
    "id, deployment_id, revision, event_kind, plan_digest, evidence, created_at";

/// What [`upsert_model_deployment`] did.
///
/// Shaped after `FingerprintUpdate` (#1680): the `Unchanged` arm is what makes
/// re-running the env import on every process start a genuine no-op instead of
/// a stream of identical events and a revision counter that climbs forever.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeploymentWrite {
    /// The deployment did not exist. Row created at revision 1 with a
    /// `deployment_imported` event.
    Created { revision: i64, event_id: i64 },
    /// The stored content digest already equalled the new one. Nothing moved:
    /// no revision bump, no event, and `fetched_at` is refreshed in place
    /// because "when did we last confirm this" is not a catalog change.
    Unchanged { revision: i64 },
    /// Content changed. Row advanced with a `deployment_updated` event.
    Advanced { revision: i64, event_id: i64 },
}

impl DeploymentWrite {
    pub fn revision(&self) -> i64 {
        match self {
            Self::Created { revision, .. }
            | Self::Unchanged { revision }
            | Self::Advanced { revision, .. } => *revision,
        }
    }

    /// Whether this write appended an event — i.e. whether the catalog
    /// actually moved.
    pub fn moved(&self) -> bool {
        !matches!(self, Self::Unchanged { .. })
    }
}

/// What [`upsert_pricing_snapshot`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PricingSnapshotWrite {
    /// First time these exact prices were seen for this provider.
    Created,
    /// A snapshot with this content-addressed id already existed, so the
    /// re-import deduped onto it. The stored row is left **untouched** —
    /// rewriting even its `fetched_at` would mutate a row historical outcome
    /// rows point at.
    Deduped,
}

// ─── model_deployments ───────────────────────────────────────────────────────

/// Import or refresh one deployment row, appending the matching event.
///
/// Three statements at most, no internal transaction (see the module note).
pub fn upsert_model_deployment(
    conn: &Connection,
    new: &NewModelDeployment,
) -> Result<DeploymentWrite, MemoryError> {
    let now = now_utc_iso();
    let existing = get_model_deployment(conn, &new.deployment_id)?;

    let capabilities = serde_json::to_string(&new.capabilities)?;
    let attachment_bounds = serde_json::to_string(&new.attachment_bounds)?;
    let source_refs = serde_json::to_string(&new.source_refs)?;

    let Some(existing) = existing else {
        conn.execute(
            "INSERT INTO model_deployments (
                deployment_id, provider_account_id, endpoint_ref, protocol_kind,
                provider_model_id, effective_version, capabilities, context_window,
                max_output, attachment_bounds, region, data_policy, pricing_snapshot_ref,
                catalog_source, fetched_at, effective_at, expires_at, status, revision,
                source_refs, created_at, updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16,
                       ?17, ?18, 1, ?19, ?20, ?20)",
            params![
                new.deployment_id,
                new.provider_account_id,
                new.endpoint_ref,
                new.protocol_kind.as_str(),
                new.provider_model_id,
                new.effective_version,
                capabilities,
                new.context_window,
                new.max_output,
                attachment_bounds,
                new.region,
                new.data_policy,
                new.pricing_snapshot_ref,
                new.catalog_source.as_str(),
                new.fetched_at,
                new.effective_at,
                new.expires_at,
                new.status,
                source_refs,
                now,
            ],
        )?;
        let event_id = append_model_deployment_event(
            conn,
            &NewModelDeploymentEvent::new(
                new.deployment_id.clone(),
                1,
                DeploymentEventKind::DeploymentImported,
            )
            .with_evidence(
                serde_json::json!({ "content_digest": new.content_digest() }).to_string(),
            ),
        )?;
        return Ok(DeploymentWrite::Created {
            revision: 1,
            event_id,
        });
    };

    if existing.content_digest() == new.content_digest() {
        // Confirmation, not change: move only the observation timestamp.
        conn.execute(
            "UPDATE model_deployments SET fetched_at = ?2 WHERE deployment_id = ?1",
            params![new.deployment_id, new.fetched_at],
        )?;
        return Ok(DeploymentWrite::Unchanged {
            revision: existing.revision,
        });
    }

    let revision = existing.revision + 1;
    conn.execute(
        "UPDATE model_deployments SET
            provider_account_id = ?2, endpoint_ref = ?3, protocol_kind = ?4,
            provider_model_id = ?5, effective_version = ?6, capabilities = ?7,
            context_window = ?8, max_output = ?9, attachment_bounds = ?10, region = ?11,
            data_policy = ?12, pricing_snapshot_ref = ?13, catalog_source = ?14,
            fetched_at = ?15, effective_at = ?16, expires_at = ?17, status = ?18,
            revision = ?19, source_refs = ?20, updated_at = ?21
         WHERE deployment_id = ?1",
        params![
            new.deployment_id,
            new.provider_account_id,
            new.endpoint_ref,
            new.protocol_kind.as_str(),
            new.provider_model_id,
            new.effective_version,
            capabilities,
            new.context_window,
            new.max_output,
            attachment_bounds,
            new.region,
            new.data_policy,
            new.pricing_snapshot_ref,
            new.catalog_source.as_str(),
            new.fetched_at,
            new.effective_at,
            new.expires_at,
            new.status,
            revision,
            source_refs,
            now,
        ],
    )?;
    let event_id = append_model_deployment_event(
        conn,
        &NewModelDeploymentEvent::new(
            new.deployment_id.clone(),
            revision,
            DeploymentEventKind::DeploymentUpdated,
        )
        .with_evidence(
            serde_json::json!({
                "previous_content_digest": existing.content_digest(),
                "content_digest": new.content_digest(),
            })
            .to_string(),
        ),
    )?;
    Ok(DeploymentWrite::Advanced { revision, event_id })
}

/// Retire a deployment: a status transition plus its event, never a delete.
/// A deployment that once existed is exactly the history an operator needs,
/// and every alias binding that referenced it stays resolvable.
pub fn retire_model_deployment(
    conn: &Connection,
    deployment_id: &str,
) -> Result<Option<DeploymentWrite>, MemoryError> {
    let Some(existing) = get_model_deployment(conn, deployment_id)? else {
        return Ok(None);
    };
    if existing.status == DEPLOYMENT_STATUS_RETIRED {
        return Ok(Some(DeploymentWrite::Unchanged {
            revision: existing.revision,
        }));
    }
    let revision = existing.revision + 1;
    conn.execute(
        "UPDATE model_deployments SET status = ?2, revision = ?3, updated_at = ?4
         WHERE deployment_id = ?1",
        params![
            deployment_id,
            DEPLOYMENT_STATUS_RETIRED,
            revision,
            now_utc_iso(),
        ],
    )?;
    let event_id = append_model_deployment_event(
        conn,
        &NewModelDeploymentEvent::new(
            deployment_id,
            revision,
            DeploymentEventKind::DeploymentRetired,
        ),
    )?;
    Ok(Some(DeploymentWrite::Advanced { revision, event_id }))
}

pub fn get_model_deployment(
    conn: &Connection,
    deployment_id: &str,
) -> Result<Option<ModelDeployment>, MemoryError> {
    conn.query_row(
        &format!("SELECT {DEPLOYMENT_COLUMNS} FROM model_deployments WHERE deployment_id = ?1"),
        params![deployment_id],
        row_to_deployment,
    )
    .optional()?
    .transpose()
}

/// Every deployment row, ordered by id so a caller's snapshot is
/// insertion-order independent.
pub fn list_model_deployments(conn: &Connection) -> Result<Vec<ModelDeployment>, MemoryError> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {DEPLOYMENT_COLUMNS} FROM model_deployments ORDER BY deployment_id"
    ))?;
    let rows = stmt.query_map([], row_to_deployment)?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row??);
    }
    Ok(out)
}

/// Deployments that came from one catalog source — the query the #1685
/// cutover needs to ask exactly ("what did the env chains produce").
pub fn list_model_deployments_by_source(
    conn: &Connection,
    source: CatalogSource,
) -> Result<Vec<ModelDeployment>, MemoryError> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {DEPLOYMENT_COLUMNS} FROM model_deployments \
         WHERE catalog_source = ?1 ORDER BY deployment_id"
    ))?;
    let rows = stmt.query_map(params![source.as_str()], row_to_deployment)?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row??);
    }
    Ok(out)
}

fn row_to_deployment(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<Result<ModelDeployment, MemoryError>> {
    let deployment_id: String = row.get("deployment_id")?;
    let protocol_raw: String = row.get("protocol_kind")?;
    let source_raw: String = row.get("catalog_source")?;
    let capabilities_raw: String = row.get("capabilities")?;
    let bounds_raw: String = row.get("attachment_bounds")?;
    let source_refs_raw: String = row.get("source_refs")?;

    let Some(protocol_kind) = ProtocolKind::parse(&protocol_raw) else {
        return Ok(Err(MemoryError::InvalidArg(format!(
            "deployment '{deployment_id}' has unknown protocol_kind '{protocol_raw}'"
        ))));
    };
    let Some(catalog_source) = CatalogSource::parse(&source_raw) else {
        return Ok(Err(MemoryError::InvalidArg(format!(
            "deployment '{deployment_id}' has unknown catalog_source '{source_raw}'"
        ))));
    };
    let capabilities: DeploymentCapabilities = match serde_json::from_str(&capabilities_raw) {
        Ok(value) => value,
        Err(err) => {
            return Ok(Err(MemoryError::InvalidArg(format!(
                "deployment '{deployment_id}' has unreadable capabilities JSON: {err}"
            ))))
        }
    };
    let attachment_bounds: AttachmentBounds = match serde_json::from_str(&bounds_raw) {
        Ok(value) => value,
        Err(err) => {
            return Ok(Err(MemoryError::InvalidArg(format!(
                "deployment '{deployment_id}' has unreadable attachment_bounds JSON: {err}"
            ))))
        }
    };
    let source_refs: Vec<String> = match serde_json::from_str(&source_refs_raw) {
        Ok(value) => value,
        Err(err) => {
            return Ok(Err(MemoryError::InvalidArg(format!(
                "deployment '{deployment_id}' has unreadable source_refs JSON: {err}"
            ))))
        }
    };

    Ok(Ok(ModelDeployment {
        deployment_id,
        provider_account_id: row.get("provider_account_id")?,
        endpoint_ref: row.get("endpoint_ref")?,
        protocol_kind,
        provider_model_id: row.get("provider_model_id")?,
        effective_version: row.get("effective_version")?,
        capabilities,
        context_window: row.get("context_window")?,
        max_output: row.get("max_output")?,
        attachment_bounds,
        region: row.get("region")?,
        data_policy: row.get("data_policy")?,
        pricing_snapshot_ref: row.get("pricing_snapshot_ref")?,
        catalog_source,
        fetched_at: row.get("fetched_at")?,
        effective_at: row.get("effective_at")?,
        expires_at: row.get("expires_at")?,
        status: row.get("status")?,
        revision: row.get("revision")?,
        source_refs,
        created_at: row.get("created_at")?,
        updated_at: row.get("updated_at")?,
    }))
}

// ─── model_deployment_events (append-only) ───────────────────────────────────

/// Append one audit row. The only write this table has: there is no update
/// and no delete accessor for `model_deployment_events` anywhere in the
/// codebase, which is what "append-only" means here.
pub fn append_model_deployment_event(
    conn: &Connection,
    event: &NewModelDeploymentEvent,
) -> Result<i64, MemoryError> {
    conn.execute(
        "INSERT INTO model_deployment_events
            (deployment_id, revision, event_kind, plan_digest, evidence, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            event.deployment_id,
            event.revision,
            event.event_kind,
            event.plan_digest,
            event.evidence,
            now_utc_iso(),
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

/// One deployment's event log, oldest first.
pub fn list_model_deployment_events(
    conn: &Connection,
    deployment_id: &str,
) -> Result<Vec<ModelDeploymentEvent>, MemoryError> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {EVENT_COLUMNS} FROM model_deployment_events WHERE deployment_id = ?1 ORDER BY id"
    ))?;
    let rows = stmt.query_map(params![deployment_id], row_to_event)?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

/// The whole catalog event log, oldest first — the input the event fold
/// replays.
pub fn list_all_model_deployment_events(
    conn: &Connection,
) -> Result<Vec<ModelDeploymentEvent>, MemoryError> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {EVENT_COLUMNS} FROM model_deployment_events ORDER BY id"
    ))?;
    let rows = stmt.query_map([], row_to_event)?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

/// Events appended after `after_id`, oldest first — the incremental half of
/// the replay-equivalence property.
pub fn list_model_deployment_events_after(
    conn: &Connection,
    after_id: i64,
) -> Result<Vec<ModelDeploymentEvent>, MemoryError> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {EVENT_COLUMNS} FROM model_deployment_events WHERE id > ?1 ORDER BY id"
    ))?;
    let rows = stmt.query_map(params![after_id], row_to_event)?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

fn row_to_event(row: &rusqlite::Row<'_>) -> rusqlite::Result<ModelDeploymentEvent> {
    Ok(ModelDeploymentEvent {
        id: row.get("id")?,
        deployment_id: row.get("deployment_id")?,
        revision: row.get("revision")?,
        event_kind: row.get("event_kind")?,
        plan_digest: row.get("plan_digest")?,
        evidence: row.get("evidence")?,
        created_at: row.get("created_at")?,
    })
}

// ─── pricing_snapshots ───────────────────────────────────────────────────────

/// Store a minted snapshot, or dedupe onto the identical one already there.
///
/// `INSERT ... ON CONFLICT DO NOTHING` rather than an upsert: the primary key
/// is the digest of the prices, so a conflict *proves* the stored prices are
/// already the ones being written. Overwriting would only touch the
/// timestamps, and touching them would mutate a row that historical outcome
/// rows already point at.
pub fn upsert_pricing_snapshot(
    conn: &Connection,
    snapshot: &PricingSnapshot,
) -> Result<PricingSnapshotWrite, MemoryError> {
    let pricing_data = serde_json::to_string(snapshot.pricing_data())?;
    let changed = conn.execute(
        "INSERT INTO pricing_snapshots
            (snapshot_id, provider_kind, pricing_data, catalog_source, fetched_at, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(snapshot_id) DO NOTHING",
        params![
            snapshot.snapshot_id(),
            snapshot.provider_kind(),
            pricing_data,
            snapshot.catalog_source.map(|source| source.as_str()),
            snapshot.fetched_at,
            snapshot.created_at,
        ],
    )?;
    Ok(if changed == 0 {
        PricingSnapshotWrite::Deduped
    } else {
        PricingSnapshotWrite::Created
    })
}

/// Read a snapshot back, re-verifying that its id still equals the digest of
/// its own prices (see [`PricingSnapshot::from_stored`]).
pub fn get_pricing_snapshot(
    conn: &Connection,
    snapshot_id: &str,
) -> Result<Option<PricingSnapshot>, MemoryError> {
    let row: Option<(String, String, String, Option<String>, String, String)> = conn
        .query_row(
            "SELECT snapshot_id, provider_kind, pricing_data, catalog_source, fetched_at, created_at
             FROM pricing_snapshots WHERE snapshot_id = ?1",
            params![snapshot_id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            },
        )
        .optional()?;
    let Some((snapshot_id, provider_kind, pricing_data, catalog_source, fetched_at, created_at)) =
        row
    else {
        return Ok(None);
    };
    let pricing_data = serde_json::from_str(&pricing_data)?;
    let catalog_source = match catalog_source.as_deref() {
        None => None,
        Some(raw) => Some(CatalogSource::parse(raw).ok_or_else(|| {
            MemoryError::InvalidArg(format!(
                "pricing snapshot '{snapshot_id}' has unknown catalog_source '{raw}'"
            ))
        })?),
    };
    PricingSnapshot::from_stored(
        snapshot_id,
        provider_kind,
        pricing_data,
        catalog_source,
        fetched_at,
        created_at,
    )
    .map(Some)
}

// ─── model_aliases + model_alias_bindings (read side) ────────────────────────

/// Every alias, ordered by name. The reviewed plan/apply **write** path is
/// #1681 D2 / PR-D; PR-B only reads.
pub fn list_model_aliases(conn: &Connection) -> Result<Vec<ModelAlias>, MemoryError> {
    let mut stmt = conn.prepare(
        "SELECT alias_name, required_capabilities, constraints, status, revision, \
         policy_digest, source_refs, created_at, updated_at \
         FROM model_aliases ORDER BY alias_name",
    )?;
    let rows = stmt.query_map([], |row| {
        let source_refs_raw: String = row.get("source_refs")?;
        Ok(ModelAlias {
            alias_name: row.get("alias_name")?,
            required_capabilities: row.get("required_capabilities")?,
            constraints: row.get("constraints")?,
            status: row.get("status")?,
            revision: row.get("revision")?,
            policy_digest: row.get("policy_digest")?,
            source_refs: serde_json::from_str(&source_refs_raw).unwrap_or_default(),
            created_at: row.get("created_at")?,
            updated_at: row.get("updated_at")?,
        })
    })?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

/// Every binding, ordered (alias, priority, deployment) so a snapshot is
/// insertion-order independent.
pub fn list_model_alias_bindings(conn: &Connection) -> Result<Vec<ModelAliasBinding>, MemoryError> {
    let mut stmt = conn.prepare(
        "SELECT alias_name, deployment_id, priority, retired, created_at, updated_at \
         FROM model_alias_bindings ORDER BY alias_name, priority, deployment_id",
    )?;
    let rows = stmt.query_map([], |row| {
        let retired: i64 = row.get("retired")?;
        Ok(ModelAliasBinding {
            alias_name: row.get("alias_name")?,
            deployment_id: row.get("deployment_id")?,
            priority: row.get("priority")?,
            retired: retired != 0,
            created_at: row.get("created_at")?,
            updated_at: row.get("updated_at")?,
        })
    })?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

// ─── model_deployment_health (read side) ─────────────────────────────────────

/// Read one deployment's health row. The single **writer**
/// (`record_deployment_outcome`, #1681 D4) is PR-C's; PR-B ships the read so
/// the "a failure never erases catalog metadata" discriminator can assert on
/// both tables at once.
pub fn get_model_deployment_health(
    conn: &Connection,
    deployment_id: &str,
) -> Result<Option<ModelDeploymentHealth>, MemoryError> {
    Ok(conn
        .query_row(
            "SELECT deployment_id, state, cooldown_until, last_success_at, last_attempt_at, \
             last_error, error_count, evidence_kind, observed_at, metadata, updated_at \
             FROM model_deployment_health WHERE deployment_id = ?1",
            params![deployment_id],
            |row| {
                let evidence_raw: Option<String> = row.get("evidence_kind")?;
                Ok(ModelDeploymentHealth {
                    deployment_id: row.get("deployment_id")?,
                    state: row.get("state")?,
                    cooldown_until: row.get("cooldown_until")?,
                    last_success_at: row.get("last_success_at")?,
                    last_attempt_at: row.get("last_attempt_at")?,
                    last_error: row.get("last_error")?,
                    error_count: row.get("error_count")?,
                    evidence_kind: evidence_raw.as_deref().and_then(EvidenceKind::parse),
                    observed_at: row.get("observed_at")?,
                    metadata: row.get("metadata")?,
                    updated_at: row.get("updated_at")?,
                })
            },
        )
        .optional()?)
}

// ─── staleness at the store boundary ─────────────────────────────────────────

/// Deployments that are usable as present-tense truth at `now`, plus typed
/// reasons for every row that is not (tachi#1681 D7 PR-B).
///
/// The excluded half is returned rather than dropped because a resolver that
/// silently gets a shorter list cannot explain an abstain (#1681 D5), and
/// because "the catalog went quiet" and "everything in it expired" are very
/// different operational states that a bare `Vec` renders identical.
///
/// Note what this is *not*: a SQL `WHERE expires_at > ?`. The comparison is
/// done on parsed instants in [`ModelDeployment::freshness_at`], because
/// `expires_at` can arrive from an import that rendered RFC3339 with a numeric
/// offset — and SQLite's lexical string comparison would then call an expired
/// row fresh.
pub fn list_authoritative_deployments(
    conn: &Connection,
    now: &str,
) -> Result<AuthoritativePartition, MemoryError> {
    Ok(partition_authoritative_at(
        list_model_deployments(conn)?,
        now,
    ))
}

/// Fold the whole catalog event log.
pub fn replay_catalog_projection(conn: &Connection) -> Result<CatalogProjection, MemoryError> {
    Ok(CatalogProjection::replay(
        &list_all_model_deployment_events(conn)?,
    ))
}

/// Advance an existing projection with whatever was appended since its
/// watermark. The incremental half of the replay-equivalence property.
pub fn advance_catalog_projection(
    conn: &Connection,
    projection: &mut CatalogProjection,
) -> Result<(), MemoryError> {
    let events = list_model_deployment_events_after(conn, projection.last_event_id())?;
    projection.extend(&events);
    Ok(())
}
