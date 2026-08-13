use chrono::{Duration, Utc};
use memcore::{A2aInsertOutcome, NewA2aEnvelope};
use tachi_params::{TachiA2aAction, TachiA2aParams};

use crate::MemoryServer;

const DEFAULT_TURN_RESPONSE_TTL_DAYS: u32 = 7;
const MAX_TURN_RESPONSE_TTL_DAYS: u32 = 30;
const DEFAULT_STATUS_LIMIT: usize = 20;

fn required(value: Option<String>, field: &str) -> Result<String, String> {
    value
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("{field} is required"))
}

fn current_local_issuer(server: &MemoryServer) -> Result<(String, String), String> {
    let Some((identity, connection_id, admission)) = server.work_claim_connection() else {
        return Err("a2a issuer admission unavailable; initialize the connection first".into());
    };
    match admission.as_str() {
        "rejected" => return Err("a2a issuer admission rejected".into()),
        "unavailable" => return Err("a2a issuer admission unavailable".into()),
        "self_asserted" => {}
        other => return Err(format!("a2a issuer admission '{other}' is not local")),
    }
    let identity = identity.ok_or_else(|| "a2a issuer identity unavailable".to_string())?;
    Ok((identity, connection_id))
}

fn respond(server: &MemoryServer, params: TachiA2aParams) -> Result<String, String> {
    if params.limit.is_some() {
        return Err("limit is valid only for action=status".into());
    }
    let (issuer_agent_identity_id, issuer_connection_id) = current_local_issuer(server)?;
    let recipient_agent_identity_id = required(
        params.recipient_agent_identity_id,
        "recipient_agent_identity_id",
    )?;
    let subject_ref = required(params.subject_ref, "subject_ref")?;
    let raw_text = required(params.text, "text")?;
    if raw_text.len() > memcore::db::a2a::MAX_A2A_TURN_RESPONSE_BYTES {
        return Err(format!(
            "turn_response/v1 text exceeds {} bytes",
            memcore::db::a2a::MAX_A2A_TURN_RESPONSE_BYTES
        ));
    }
    let without_think_tags = crate::memory_search_ops::scrub_think_tags(&raw_text);
    let (safe_text, secret_redactions) =
        crate::memory_search_ops::scrub_secrets(&without_think_tags);
    if secret_redactions > 0 {
        return Err("secret-bearing turn_response/v1 text is refused".into());
    }
    if safe_text.trim().is_empty() {
        return Err("text is empty after hidden-reasoning scrubbing".into());
    }
    if safe_text.len() > memcore::db::a2a::MAX_A2A_TURN_RESPONSE_BYTES {
        return Err(format!(
            "turn_response/v1 text exceeds {} bytes",
            memcore::db::a2a::MAX_A2A_TURN_RESPONSE_BYTES
        ));
    }
    let idempotency_key = required(params.idempotency_key, "idempotency_key")?;
    let ttl_days = params.ttl_days.unwrap_or(DEFAULT_TURN_RESPONSE_TTL_DAYS);
    if !(1..=MAX_TURN_RESPONSE_TTL_DAYS).contains(&ttl_days) {
        return Err(format!("ttl_days must be 1..={MAX_TURN_RESPONSE_TTL_DAYS}"));
    }
    let created_at = Utc::now();
    let request = NewA2aEnvelope {
        envelope_id: format!("a2a-{}", uuid::Uuid::new_v4()),
        issuer_agent_identity_id,
        issuer_connection_id,
        recipient_agent_identity_id,
        subject_ref,
        body: safe_text,
        idempotency_key,
        created_at: created_at.to_rfc3339(),
        expires_at: (created_at + Duration::days(i64::from(ttl_days))).to_rfc3339(),
    };
    let outcome = server.with_global_store(|store| {
        memcore::insert_a2a_envelope(store.connection_mut(), &request)
            .map_err(|error| error.to_string())
    })?;
    let (delivery, envelope, receipt) = match outcome {
        A2aInsertOutcome::Created { envelope, receipt } => ("created", envelope, receipt),
        A2aInsertOutcome::Replay { envelope, receipt } => ("replayed", envelope, receipt),
    };
    serde_json::to_string(&serde_json::json!({
        "contract": "tachi.a2a.v1",
        "action": "respond",
        "status": "completed",
        "delivery": delivery,
        "envelope_id": envelope.envelope_id,
        "kind": envelope.kind,
        "recipient_agent_identity_id": envelope.recipient_agent_identity_id,
        "subject_ref": envelope.subject_ref,
        "body_digest": envelope.body_digest,
        "current_state": envelope.current_state,
        "receipt": receipt,
        "identity_assurance": {
            "issuer": envelope.issuer_identity_assurance,
            "recipient": envelope.recipient_identity_assurance,
        },
        "limits": {
            "max_local_turn_response_bytes": memcore::db::a2a::MAX_A2A_TURN_RESPONSE_BYTES,
            "ttl_days": ttl_days,
        },
    }))
    .map_err(|error| error.to_string())
}

fn status(server: &MemoryServer, params: TachiA2aParams) -> Result<String, String> {
    if params.recipient_agent_identity_id.is_some()
        || params.subject_ref.is_some()
        || params.text.is_some()
        || params.idempotency_key.is_some()
        || params.ttl_days.is_some()
    {
        return Err("respond fields are not accepted for action=status".into());
    }
    let (actor, _) = current_local_issuer(server)?;
    let limit = params.limit.unwrap_or(DEFAULT_STATUS_LIMIT);
    let envelopes = server.with_global_store_read(|store| {
        memcore::list_a2a_status(store.connection(), &actor, limit)
            .map_err(|error| error.to_string())
    })?;
    serde_json::to_string(&serde_json::json!({
        "contract": "tachi.a2a.v1",
        "action": "status",
        "status": "completed",
        "actor_agent_identity_id": actor,
        "envelopes": envelopes,
    }))
    .map_err(|error| error.to_string())
}

pub(crate) fn handle_tachi_a2a(
    server: &MemoryServer,
    params: TachiA2aParams,
) -> Result<String, String> {
    match params.action {
        TachiA2aAction::Respond => respond(server, params),
        TachiA2aAction::Status => status(server, params),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::make_server;

    fn respond(text: &str, key: &str) -> TachiA2aParams {
        TachiA2aParams {
            action: TachiA2aAction::Respond,
            recipient_agent_identity_id: Some("agent.recipient".to_string()),
            subject_ref: Some("peer_publication:publication-1".to_string()),
            text: Some(text.to_string()),
            idempotency_key: Some(key.to_string()),
            ttl_days: None,
            limit: None,
        }
    }

    fn status() -> TachiA2aParams {
        TachiA2aParams {
            action: TachiA2aAction::Status,
            recipient_agent_identity_id: None,
            subject_ref: None,
            text: None,
            idempotency_key: None,
            ttl_days: None,
            limit: None,
        }
    }

    fn seed_offline_recipient_and_sender(server: &crate::MemoryServer) {
        crate::claims_ops::admit_agent_connection(
            server,
            Some("agent.recipient".to_string()),
            true,
        )
        .expect("historical local recipient admission");
        crate::claims_ops::admit_agent_connection(server, Some("agent.sender".to_string()), true)
            .expect("current local sender admission");
    }

    /// Break caught: accepting an identityless, rejected, or remote current
    /// connection as the sender of a durable response.
    #[test]
    fn respond_requires_exact_current_local_admitted_issuer() {
        let server = make_server();
        let missing = handle_tachi_a2a(&server, respond("hello", "missing"))
            .expect_err("missing admission must fail");
        assert!(missing.contains("issuer"), "{missing}");

        crate::claims_ops::admit_agent_connection(&server, Some("agent.remote".to_string()), false)
            .expect("record unavailable remote admission");
        let unavailable = handle_tachi_a2a(&server, respond("hello", "remote"))
            .expect_err("remote admission must fail");
        assert!(unavailable.contains("unavailable"), "{unavailable}");

        crate::claims_ops::admit_agent_connection(&server, None, true)
            .expect("record rejected identityless admission");
        let rejected = handle_tachi_a2a(&server, respond("hello", "rejected"))
            .expect_err("rejected admission must fail");
        assert!(rejected.contains("rejected"), "{rejected}");
    }

    /// Break caught: replay creating a second row, status exposing body bytes,
    /// or a status read consuming the pending response.
    #[test]
    fn respond_replays_and_status_is_header_only_non_consuming() {
        let server = make_server();
        seed_offline_recipient_and_sender(&server);

        let first: serde_json::Value = serde_json::from_str(
            &handle_tachi_a2a(&server, respond("review complete", "reply-1"))
                .expect("create response"),
        )
        .unwrap();
        let replay: serde_json::Value = serde_json::from_str(
            &handle_tachi_a2a(&server, respond("review complete", "reply-1"))
                .expect("replay response"),
        )
        .unwrap();
        assert_eq!(first["envelope_id"], replay["envelope_id"]);
        assert_eq!(first["delivery"], "created");
        assert_eq!(replay["delivery"], "replayed");

        let status_body = handle_tachi_a2a(&server, status()).expect("status");
        assert!(!status_body.contains("review complete"), "{status_body}");
        let status: serde_json::Value = serde_json::from_str(&status_body).unwrap();
        assert_eq!(status["envelopes"][0]["current_state"], "received");
        assert_eq!(
            status["envelopes"][0]["receipts"].as_array().unwrap().len(),
            1
        );
    }

    /// Break caught: storing secret-bearing or over-limit advisory content,
    /// including a rejected write leaving any envelope/receipt residue.
    #[test]
    fn respond_rejects_secret_and_oversize_before_persistence() {
        let server = make_server();
        seed_offline_recipient_and_sender(&server);

        let secret = handle_tachi_a2a(
            &server,
            respond(
                "Authorization: Bearer sk-abc123def456ghi789jkl012mno345",
                "secret",
            ),
        )
        .expect_err("secret-bearing response must fail");
        assert!(secret.contains("secret-bearing"), "{secret}");
        let oversized = handle_tachi_a2a(
            &server,
            respond(
                &"x".repeat(memcore::db::a2a::MAX_A2A_TURN_RESPONSE_BYTES + 1),
                "large",
            ),
        )
        .expect_err("oversized response must fail");
        assert!(oversized.contains("4096"), "{oversized}");

        let counts: (i64, i64) = server
            .with_global_store_read(|store| {
                store
                    .connection()
                    .query_row(
                        "SELECT (SELECT COUNT(*) FROM a2a_envelopes), \
                                (SELECT COUNT(*) FROM a2a_delivery_receipts)",
                        [],
                        |row| Ok((row.get(0)?, row.get(1)?)),
                    )
                    .map_err(|error| error.to_string())
            })
            .expect("read rejected-write counts");
        assert_eq!(counts, (0, 0));
    }

    /// Break caught: action-polymorphic fields crossing the closed action
    /// boundary and either mutating on status or silently changing respond.
    #[test]
    fn respond_and_status_refuse_each_others_fields() {
        let server = make_server();
        seed_offline_recipient_and_sender(&server);

        let mut respond_with_limit = respond("review complete", "reply-with-limit");
        respond_with_limit.limit = Some(1);
        let denied = handle_tachi_a2a(&server, respond_with_limit)
            .expect_err("respond must deny status-only limit");
        assert!(denied.contains("only for action=status"), "{denied}");

        let mut status_with_text = status();
        status_with_text.text = Some("must not be stored".to_string());
        let denied = handle_tachi_a2a(&server, status_with_text)
            .expect_err("status must deny respond-only text");
        assert!(
            denied.contains("not accepted for action=status"),
            "{denied}"
        );

        let count: i64 = server
            .with_global_store_read(|store| {
                store
                    .connection()
                    .query_row("SELECT COUNT(*) FROM a2a_envelopes", [], |row| row.get(0))
                    .map_err(|error| error.to_string())
            })
            .expect("read denied action-field count");
        assert_eq!(count, 0);
    }
}
