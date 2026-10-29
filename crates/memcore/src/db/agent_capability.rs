//! Operator-authorized ACP capability grant/revoke on admitted
//! AgentIdentity rows (#1960).
//!
//! Default closed: an identity carries no ACP grant until an operator
//! grants one. The grant shape is the #1733 closed envelope — validation
//! delegates to the same predicate the attachment check runs, so a grant
//! that would not authorize an attachment cannot be written. Unrelated
//! top-level `capability_json` keys are preserved and confer no ACP
//! authority. Every grant and revoke appends an immutable audit receipt to
//! the `hard_state` namespace `agent_capability_audit` (no new ledger:
//! reuse of the existing deterministic KV store). Revocation removes the
//! `acp` key and takes effect for NEW attachments; the attachment path
//! re-reads the row and already refuses a revoked grant, including on
//! replay of an earlier attachment.

use super::harness_session_attachments::authorization_from_capability_json;
use super::open::{authorize_reserved_reference_write, ReservedReferenceWriteFlag};
use crate::error::MemoryError;
use rusqlite::params;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

/// Re-export of the always-compiled namespace constant (see `db::state`).
pub use super::state::AGENT_CAPABILITY_AUDIT_NS;

/// The #1733 closed grant content. Names are validated against the
/// attachment predicate's canonical sets — aliases, unknowns, duplicates
/// and empty lists refuse.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcpCapabilityGrant {
    pub tool_profiles: Vec<String>,
    pub capability_classes: Vec<String>,
}

fn envelope_digest(capability_json: Option<&str>) -> String {
    let mut hasher = Sha256::new();
    hasher.update(capability_json.unwrap_or(""));
    let digest = hasher.finalize();
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

fn now_utc() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

fn read_capability_json(
    conn: &rusqlite::Connection,
    agent_identity_id: &str,
) -> Result<Option<String>, MemoryError> {
    conn.query_row(
        "SELECT capability_json FROM agent_identities WHERE agent_identity_id = ?1",
        params![agent_identity_id],
        |row| row.get::<_, Option<String>>(0),
    )
    .map_err(|error| match error {
        rusqlite::Error::QueryReturnedNoRows => MemoryError::WorkClaimIncompatibleState(format!(
            "AgentIdentity {agent_identity_id} does not exist; grant an admitted identity"
        )),
        other => MemoryError::Sqlite(other),
    })
}

fn parse_root_object(
    capability_json: Option<&str>,
    agent_identity_id: &str,
) -> Result<serde_json::Map<String, Value>, MemoryError> {
    match capability_json {
        None => Ok(serde_json::Map::new()),
        Some(raw) => {
            let root: Value = serde_json::from_str(raw).map_err(|error| {
                MemoryError::WorkClaimIncompatibleState(format!(
                    "AgentIdentity {agent_identity_id} has malformed capability_json: {error}"
                ))
            })?;
            root.as_object().cloned().ok_or_else(|| {
                MemoryError::WorkClaimIncompatibleState(format!(
                    "AgentIdentity {agent_identity_id} capability_json must be an object"
                ))
            })
        }
    }
}

fn validate_via_attachment_predicate(
    candidate_json: &str,
    agent_identity_id: &str,
    grant: &AcpCapabilityGrant,
) -> Result<(), MemoryError> {
    // A grant must satisfy the SAME predicate an attachment runs: probe it
    // with the grant's own first names so the full closed-shape validation
    // (canonical names, no duplicates, no unknown fields) executes against
    // the exact bytes that will be stored.
    let tool_profile = grant.tool_profiles.first().ok_or_else(|| {
        MemoryError::WorkClaimIncompatibleState(
            "ACP tool_profiles grant must not be empty".to_string(),
        )
    })?;
    let capability_class = grant.capability_classes.first().ok_or_else(|| {
        MemoryError::WorkClaimIncompatibleState(
            "ACP capability_classes grant must not be empty".to_string(),
        )
    })?;
    authorization_from_capability_json(
        Some(candidate_json),
        agent_identity_id,
        tool_profile,
        capability_class,
    )
    .map(|_| ())
}

fn append_audit_receipt(
    conn: &mut rusqlite::Connection,
    flag: &ReservedReferenceWriteFlag,
    action: &str,
    agent_identity_id: &str,
    actor: &str,
    before_digest: &str,
    after_digest: &str,
) -> Result<Value, MemoryError> {
    let at = now_utc();
    // Sequence is read INSIDE the caller's write transaction: two
    // byte-identical operations in the same second get distinct keys, and a
    // collision cannot occur because BEGIN IMMEDIATE serializes writers.
    let sequence: i64 = conn.query_row(
        "SELECT COUNT(*) FROM hard_state WHERE namespace = ?1",
        params![AGENT_CAPABILITY_AUDIT_NS],
        |row| row.get(0),
    )?;
    let receipt = json!({
        "action": action,
        "agent_identity_id": agent_identity_id,
        "actor": actor,
        "before_digest": before_digest,
        "after_digest": after_digest,
        "at": at,
        "sequence": sequence,
    });
    let digest8: String = envelope_digest(Some(
        format!("{action}{agent_identity_id}{actor}{before_digest}{after_digest}").as_str(),
    ))
    .chars()
    .take(8)
    .collect();
    let key = format!("{action}:{agent_identity_id}:{sequence:08}-{digest8}");
    // The audit namespace is row-guarded (#1960 review round 1): arm the
    // reserved-reference write authorization for the insert.
    let _authorization = authorize_reserved_reference_write(flag)?;
    conn.execute(
        "INSERT INTO hard_state (namespace, key, value_json, version, created_at, updated_at) \
         VALUES (?1, ?2, ?3, 1, ?4, ?4)",
        params![AGENT_CAPABILITY_AUDIT_NS, key, receipt.to_string(), at],
    )?;
    Ok(receipt)
}

/// Run one grant/revoke body as a single write transaction: the read, the
/// row update and the audit receipt commit together or not at all (#1960
/// review round 1 — a receipt collision must roll back the whole operation
/// instead of leaving an un-audited grant behind).
fn in_write_transaction<T>(
    conn: &mut rusqlite::Connection,
    body: impl FnOnce(&mut rusqlite::Connection) -> Result<T, MemoryError>,
) -> Result<T, MemoryError> {
    conn.execute_batch("BEGIN IMMEDIATE")?;
    match body(conn) {
        Ok(value) => {
            conn.execute_batch("COMMIT")?;
            Ok(value)
        }
        Err(error) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(error)
        }
    }
}

/// Public attachment-predicate wrapper for consumers and CLI tests: does
/// this capability envelope admit an attachment with the given profile and
/// class? Same typed refusal strings the attachment path produces.
pub fn acp_grant_allows_attachment(
    capability_json: Option<&str>,
    agent_identity_id: &str,
    tool_profile: &str,
    capability_class: &str,
) -> Result<(), String> {
    authorization_from_capability_json(
        capability_json,
        agent_identity_id,
        tool_profile,
        capability_class,
    )
    .map(|_| ())
    .map_err(|error| error.to_string())
}

/// Grant the #1733 closed ACP envelope to an admitted identity. Refuses a
/// self-grant (actor == identity), unknown identities, malformed existing
/// envelopes and any grant the attachment predicate would not accept;
/// nothing is written on refusal. Unrelated top-level keys are preserved.
pub(crate) fn grant_agent_acp_capability(
    conn: &mut rusqlite::Connection,
    flag: &ReservedReferenceWriteFlag,
    agent_identity_id: &str,
    grant: &AcpCapabilityGrant,
    actor: &str,
) -> Result<Value, MemoryError> {
    if actor == agent_identity_id {
        return Err(MemoryError::WorkClaimIncompatibleState(format!(
            "AgentIdentity {agent_identity_id} cannot grant itself the ACP capability"
        )));
    }
    in_write_transaction(conn, |conn| {
        let before = read_capability_json(conn, agent_identity_id)?;
        let mut root = parse_root_object(before.as_deref(), agent_identity_id)?;
        let mut acp = serde_json::Map::new();
        acp.insert(
            "tool_profiles".to_string(),
            Value::Array(
                grant
                    .tool_profiles
                    .iter()
                    .map(|name| Value::String(name.clone()))
                    .collect(),
            ),
        );
        acp.insert(
            "capability_classes".to_string(),
            Value::Array(
                grant
                    .capability_classes
                    .iter()
                    .map(|name| Value::String(name.clone()))
                    .collect(),
            ),
        );
        root.insert("acp".to_string(), Value::Object(acp));
        let candidate = Value::Object(root).to_string();
        validate_via_attachment_predicate(&candidate, agent_identity_id, grant)?;
        conn.execute(
            "UPDATE agent_identities SET capability_json = ?1 WHERE agent_identity_id = ?2",
            params![candidate, agent_identity_id],
        )?;
        append_audit_receipt(
            conn,
            flag,
            "grant",
            agent_identity_id,
            actor,
            &envelope_digest(before.as_deref()),
            &envelope_digest(Some(&candidate)),
        )
    })
}

/// Revoke the ACP capability: removes the `acp` key, preserves every other
/// top-level key, and takes effect for NEW attachments (the attachment
/// path re-reads the row). Revoking an identity with no grant refuses and
/// writes nothing. Self-revocation is refused the same way as self-grant.
pub(crate) fn revoke_agent_acp_capability(
    conn: &mut rusqlite::Connection,
    flag: &ReservedReferenceWriteFlag,
    agent_identity_id: &str,
    actor: &str,
) -> Result<Value, MemoryError> {
    if actor == agent_identity_id {
        return Err(MemoryError::WorkClaimIncompatibleState(format!(
            "AgentIdentity {agent_identity_id} cannot revoke its own ACP capability"
        )));
    }
    in_write_transaction(conn, |conn| {
        let before = read_capability_json(conn, agent_identity_id)?;
        let mut root = parse_root_object(before.as_deref(), agent_identity_id)?;
        if root.remove("acp").is_none() {
            return Err(MemoryError::WorkClaimIncompatibleState(format!(
                "AgentIdentity {agent_identity_id} has no ACP capability grant to revoke"
            )));
        }
        // An identity with no remaining capabilities carries no envelope at all:
        // write NULL so a revoked identity is refused with the exact typed
        // reason a never-granted one gets ("has no ACP capability grant").
        let after: Option<String> = if root.is_empty() {
            None
        } else {
            Some(Value::Object(root).to_string())
        };
        conn.execute(
            "UPDATE agent_identities SET capability_json = ?1 WHERE agent_identity_id = ?2",
            params![after, agent_identity_id],
        )?;
        append_audit_receipt(
            conn,
            flag,
            "revoke",
            agent_identity_id,
            actor,
            &envelope_digest(before.as_deref()),
            &envelope_digest(after.as_deref()),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (tempfile::TempDir, crate::MemoryStore) {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("tachi-memory.db");
        let mut store = crate::MemoryStore::open(path.to_str().unwrap()).unwrap();
        store
            .connection_mut()
            .execute(
                "INSERT INTO agent_identities (agent_identity_id, created_at) \
                 VALUES ('zc-a2-e2e', '2026-10-29T00:00:00Z')",
                [],
            )
            .unwrap();
        (temp, store)
    }

    fn full_grant() -> AcpCapabilityGrant {
        AcpCapabilityGrant {
            tool_profiles: vec!["observe".to_string(), "delegate".to_string()],
            capability_classes: vec!["tachi".to_string()],
        }
    }

    fn stored(store: &crate::MemoryStore) -> Option<String> {
        store
            .connection()
            .query_row(
                "SELECT capability_json FROM agent_identities \
                 WHERE agent_identity_id = 'zc-a2-e2e'",
                [],
                |row| row.get::<_, Option<String>>(0),
            )
            .unwrap()
    }

    fn audit_count(store: &crate::MemoryStore) -> i64 {
        store
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM hard_state WHERE namespace = ?1",
                params![AGENT_CAPABILITY_AUDIT_NS],
                |row| row.get(0),
            )
            .unwrap()
    }

    #[test]
    fn grant_stores_the_closed_envelope_and_passes_the_attachment_predicate() {
        let (_temp, mut store) = store();
        let receipt = store
            .grant_agent_acp_capability("zc-a2-e2e", &full_grant(), "operator-kyle")
            .expect("grant");
        assert_eq!(receipt["action"], json!("grant"));
        acp_grant_allows_attachment(stored(&store).as_deref(), "zc-a2-e2e", "observe", "tachi")
            .expect("stored grant authorizes an attachment");
        acp_grant_allows_attachment(stored(&store).as_deref(), "zc-a2-e2e", "delegate", "tachi")
            .expect("delegate profile also admitted");
        assert_eq!(audit_count(&store), 1);
        assert_eq!(receipt["actor"], json!("operator-kyle"));
    }

    #[test]
    fn default_closed_without_grant_attachment_refuses() {
        let (_temp, store) = store();
        let error = acp_grant_allows_attachment(
            stored(&store).as_deref(),
            "zc-a2-e2e",
            "delegate",
            "tachi",
        )
        .expect_err("no grant refuses");
        assert!(error.contains("has no ACP capability grant"), "{error}");
    }

    #[test]
    fn revoke_removes_authority_for_new_attachments_and_audits() {
        let (_temp, mut store) = store();
        store
            .grant_agent_acp_capability("zc-a2-e2e", &full_grant(), "operator-kyle")
            .unwrap();
        let receipt = store
            .revoke_agent_acp_capability("zc-a2-e2e", "operator-kyle")
            .expect("revoke");
        assert_eq!(receipt["action"], json!("revoke"));
        let error = acp_grant_allows_attachment(
            stored(&store).as_deref(),
            "zc-a2-e2e",
            "delegate",
            "tachi",
        )
        .expect_err("revoked refuses new attachments");
        assert!(error.contains("has no ACP capability grant"), "{error}");
        assert_eq!(audit_count(&store), 2, "grant + revoke receipts");
    }

    #[test]
    fn same_second_grant_revoke_grant_is_atomic_and_fully_audited() {
        // Review round 1 P1 regression: the receipt insert must not fail
        // after the row update committed, and byte-identical repeats must
        // not collide on the audit key.
        let (_temp, mut store) = store();
        store
            .grant_agent_acp_capability("zc-a2-e2e", &full_grant(), "operator-kyle")
            .unwrap();
        store
            .revoke_agent_acp_capability("zc-a2-e2e", "operator-kyle")
            .unwrap();
        store
            .grant_agent_acp_capability("zc-a2-e2e", &full_grant(), "operator-kyle")
            .expect("same-second repeat grant succeeds with a distinct receipt key");
        assert_eq!(audit_count(&store), 3, "all three operations audited");
        acp_grant_allows_attachment(stored(&store).as_deref(), "zc-a2-e2e", "delegate", "tachi")
            .expect("final state is the re-grant");
    }

    #[test]
    fn self_grant_and_self_revoke_refuse_writing_nothing() {
        let (_temp, mut store) = store();
        let error = store
            .grant_agent_acp_capability("zc-a2-e2e", &full_grant(), "zc-a2-e2e")
            .expect_err("self-grant refuses");
        assert!(error.to_string().contains("cannot grant itself"), "{error}");
        assert!(stored(&store).is_none());
        assert_eq!(audit_count(&store), 0);

        store
            .grant_agent_acp_capability("zc-a2-e2e", &full_grant(), "operator-kyle")
            .unwrap();
        let error = store
            .revoke_agent_acp_capability("zc-a2-e2e", "zc-a2-e2e")
            .expect_err("self-revoke refuses");
        assert!(
            error.to_string().contains("cannot revoke its own"),
            "{error}"
        );
        assert!(stored(&store).is_some(), "grant untouched");
    }

    #[test]
    fn over_broad_or_malformed_grants_refuse_and_write_nothing() {
        let (_temp, mut store) = store();
        for grant in [
            AcpCapabilityGrant {
                tool_profiles: vec!["admin".to_string()],
                capability_classes: vec!["tachi".to_string()],
            },
            AcpCapabilityGrant {
                tool_profiles: vec!["read".to_string()],
                capability_classes: vec!["tachi".to_string()],
            },
            AcpCapabilityGrant {
                tool_profiles: vec!["tachi".to_string()],
                capability_classes: vec!["tachi".to_string()],
            },
            AcpCapabilityGrant {
                tool_profiles: vec!["observe".to_string(), "observe".to_string()],
                capability_classes: vec!["tachi".to_string()],
            },
            AcpCapabilityGrant {
                tool_profiles: vec![],
                capability_classes: vec!["tachi".to_string()],
            },
        ] {
            let error = store
                .grant_agent_acp_capability("zc-a2-e2e", &grant, "operator-kyle")
                .expect_err("invalid grant refuses");
            assert!(
                stored(&store).is_none(),
                "nothing written for invalid grant: {error}"
            );
        }
        assert_eq!(audit_count(&store), 0);
    }

    #[test]
    fn unknown_identity_refuses() {
        let (_temp, mut store) = store();
        let error = store
            .grant_agent_acp_capability("ghost", &full_grant(), "operator-kyle")
            .expect_err("unknown identity refuses");
        assert!(error.to_string().contains("does not exist"), "{error}");
    }

    #[test]
    fn unrelated_top_level_keys_are_preserved_and_revoke_keeps_them() {
        let (_temp, mut store) = store();
        store
            .connection_mut()
            .execute(
                "UPDATE agent_identities SET capability_json = ?1 \
                 WHERE agent_identity_id = 'zc-a2-e2e'",
                [r#"{"future_capability":{"level":3}}"#],
            )
            .unwrap();
        store
            .grant_agent_acp_capability("zc-a2-e2e", &full_grant(), "operator-kyle")
            .unwrap();
        let granted: Value = serde_json::from_str(&stored(&store).unwrap()).unwrap();
        assert_eq!(granted["future_capability"]["level"], json!(3));

        store
            .revoke_agent_acp_capability("zc-a2-e2e", "operator-kyle")
            .unwrap();
        let revoked: Value = serde_json::from_str(&stored(&store).unwrap()).unwrap();
        assert!(revoked.get("acp").is_none(), "acp removed");
        assert_eq!(revoked["future_capability"]["level"], json!(3));
    }

    #[test]
    fn revoking_without_a_grant_refuses() {
        let (_temp, mut store) = store();
        let error = store
            .revoke_agent_acp_capability("zc-a2-e2e", "operator-kyle")
            .expect_err("nothing to revoke");
        assert!(
            error
                .to_string()
                .contains("no ACP capability grant to revoke"),
            "{error}"
        );
        assert_eq!(audit_count(&store), 0);
    }

    #[test]
    fn malformed_existing_envelope_refuses_rather_than_discarding() {
        let (_temp, mut store) = store();
        store
            .connection_mut()
            .execute(
                "UPDATE agent_identities SET capability_json = 'not-json' \
                 WHERE agent_identity_id = 'zc-a2-e2e'",
                [],
            )
            .unwrap();
        let error = store
            .grant_agent_acp_capability("zc-a2-e2e", &full_grant(), "operator-kyle")
            .expect_err("malformed envelope refuses");
        assert!(
            error.to_string().contains("malformed capability_json"),
            "{error}"
        );
        assert_eq!(
            stored(&store).as_deref(),
            Some("not-json"),
            "bytes preserved on refusal"
        );
    }

    #[test]
    fn general_state_api_cannot_mutate_the_audit_namespace() {
        // Review round 1 P2 regression: the audit namespace must refuse the
        // general set_state/delete_state surface like the other write-once
        // authority namespaces.
        let (_temp, mut store) = store();
        store
            .grant_agent_acp_capability("zc-a2-e2e", &full_grant(), "operator-kyle")
            .unwrap();
        let error = store
            .set_state(AGENT_CAPABILITY_AUDIT_NS, "any-key", r#"{"forged":true}"#)
            .expect_err("general set_state refuses the audit namespace");
        assert!(
            error
                .to_string()
                .contains("append-only ACP capability audit"),
            "{error}"
        );
        let error = store
            .delete_state(AGENT_CAPABILITY_AUDIT_NS, "any-key")
            .expect_err("general delete_state refuses the audit namespace");
        assert!(
            error
                .to_string()
                .contains("append-only ACP capability audit"),
            "{error}"
        );
        assert_eq!(audit_count(&store), 1, "no receipt was added or removed");
    }
}
