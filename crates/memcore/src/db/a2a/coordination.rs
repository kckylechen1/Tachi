//! Project-bound coordination uses the existing identity, envelope and receipt owners.
use super::*;

fn unavailable() -> MemoryError {
    MemoryError::InvalidArg("a2a project admission is unavailable".into())
}

fn validate_project(project: &str) -> Result<(), MemoryError> {
    if project.len() != 64
        || !project
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    {
        return Err(unavailable());
    }
    Ok(())
}

pub(super) fn validate_coordination_subject(subject: &str) -> Result<(), MemoryError> {
    if !subject.strip_prefix("coordination:").is_some_and(|id| {
        !id.is_empty()
            && id.len() <= 128
            && id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._/-".contains(&b))
    }) {
        return Err(MemoryError::InvalidArg("send subject_ref must be coordination:<opaque-id>, at most 128 ASCII letters, digits, '.', '_', '/', '-'".into()));
    }
    Ok(())
}

/// The server calls this only with a resolved transport project; historical
/// unbound admissions stay unbound and are never inferred from caller text.
#[cfg(feature = "admin")]
pub fn record_a2a_project_admission(
    conn: &Connection,
    admission_id: &str,
    identity: &str,
    connection: &str,
    project: &str,
) -> Result<(), MemoryError> {
    validate_project(project)?;
    let tx = conn.unchecked_transaction()?;
    super::super::session_claims::record_unverified_admission(
        &tx,
        admission_id,
        identity,
        connection,
        super::super::session_claims::UnverifiedAdmissionState::SelfAsserted,
    )?;
    tx.execute(
        "UPDATE identity_admissions SET canonical_project=?1 WHERE admission_id=?2",
        params![project, admission_id],
    )?;
    tx.commit()?;
    Ok(())
}

pub(super) fn require_project_actor(
    conn: &Connection,
    identity: &str,
    connection: &str,
    project: &str,
) -> Result<(), MemoryError> {
    validate_project(project)?;
    let current: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM identity_admissions WHERE agent_identity_id=?1 AND connection_id=?2 AND canonical_project=?3 AND state='self_asserted')",
        params![identity, connection, project], |row| row.get(0),
    )?;
    if current {
        Ok(())
    } else {
        Err(unavailable())
    }
}

pub(super) fn project_recipient(
    conn: &Connection,
    identity: &str,
    project: &str,
) -> Result<Option<A2aRecipientEligibility>, MemoryError> {
    conn.query_row(
        "SELECT a.agent_identity_id,ia.admission_id,ia.connection_id,ia.state
         FROM agent_identities a JOIN identity_admissions ia ON ia.agent_identity_id=a.agent_identity_id
         WHERE a.agent_identity_id=?1 AND ia.canonical_project=?2 AND ia.state='self_asserted'
         ORDER BY ia.created_at DESC,ia.admission_id DESC LIMIT 1",
        params![identity, project], |row| Ok(A2aRecipientEligibility {
            agent_identity_id: row.get(0)?, admission_id: row.get(1)?, connection_id: row.get(2)?,
            identity_assurance: row.get(3)?, trust_domain: A2A_SAME_HOST_TRUST_DOMAIN.into(),
            trust_basis: "historical_local_admission".into(),
        }),
    ).optional().map_err(Into::into)
}

pub fn insert_a2a_coordination(
    conn: &mut Connection,
    request: &NewA2aEnvelope,
    project: &str,
) -> Result<A2aInsertOutcome, MemoryError> {
    validate_project(project)?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let outcome = insert_scoped_envelope_in_tx(&tx, request, Some(project))?;
    tx.commit()?;
    Ok(outcome)
}

pub fn list_a2a_project_status(
    conn: &Connection,
    actor: &A2aTransitionActor,
    project: &str,
    limit: usize,
) -> Result<Vec<A2aStatusRow>, MemoryError> {
    let tx = conn.unchecked_transaction()?;
    require_project_actor(&tx, &actor.agent_identity_id, &actor.connection_id, project)?;
    let rows = list_scoped_status(&tx, &actor.agent_identity_id, Some(project), limit)?;
    tx.commit()?;
    Ok(rows)
}

/// A deliberate delivery read. The transaction serializes concurrent readers;
/// a consumed receipt records adapter intake, not semantic acceptance by a model.
pub fn consume_a2a_for_project(
    conn: &mut Connection,
    actor: &A2aTransitionActor,
    project: &str,
    limit: usize,
    now: &str,
) -> Result<Vec<A2aEnvelope>, MemoryError> {
    consume_scoped(conn, actor, Some(project), limit, now)
}

#[cfg(all(test, feature = "admin"))]
mod tests;
