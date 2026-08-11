//! Generic provider-neutral host-owned ACP attachment admission (#1733).
//!
//! This route records an admitted binding and projects a policy-derived ACP
//! stdio descriptor. It never owns or invokes an ACP lifecycle operation.

use crate::server_state::MemoryServer;
use crate::tool_params::TachiAgentEvalParams;
use memcore::{
    attach_harness_session, authorize_harness_session_attachment, get_harness_session_attachment,
    HarnessSessionAttachmentAdmission, HarnessSessionAttachmentCapabilities,
    HarnessSessionAttachmentSelector, HarnessSessionHostAdmission, NewHarnessSessionAttachment,
    TRUSTED_LOCAL_HOST_DECLARED_BASIS,
};
use serde_json::{json, Value};

fn required(value: Option<String>, field: &str) -> Result<String, String> {
    value
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| format!("{field} is required"))
}

fn canonical_name(value: Option<String>, field: &str) -> Result<String, String> {
    let value = value.ok_or_else(|| format!("{field} is required"))?;
    if value.is_empty() || value.trim() != value {
        return Err(format!("{field} must use its canonical spelling"));
    }
    Ok(value)
}

fn sha256_hex(value: &str) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(value.as_bytes()))
}

fn project_descriptors(
    tool_profile: &str,
    capability_class: &str,
    agent_identity_id: &str,
) -> Result<(Vec<Value>, String), String> {
    if !memcore::ACP_TOOL_PROFILES.contains(&tool_profile)
        || !memcore::ACP_CAPABILITY_CLASSES.contains(&capability_class)
    {
        return Err(format!(
            "requested ACP profile/class '{tool_profile}/{capability_class}' is not canonical"
        ));
    }
    if agent_identity_id.trim().is_empty() {
        return Err("agent_identity_id is required for ACP MCP projection".to_string());
    }
    let command = std::env::current_exe()
        .map_err(|error| format!("resolve absolute Tachi MCP command: {error}"))?;
    if !command.is_absolute() {
        return Err("resolved Tachi MCP command is not absolute".to_string());
    }
    let descriptors = vec![json!({
        "name": "tachi",
        "command": command,
        "args": ["serve"],
        "env": [
            {"name": "TACHI_PROFILE", "value": tool_profile},
            {"name": "TACHI_AGENT_SEAT", "value": agent_identity_id.trim()},
        ],
    })];
    let descriptor_digest = sha256_hex(&Value::Array(descriptors.clone()).to_string());
    Ok((descriptors, descriptor_digest))
}

fn capabilities(params: &TachiAgentEvalParams) -> Result<String, String> {
    HarnessSessionAttachmentCapabilities::from_names(&params.session_capabilities)
        .and_then(|capabilities| capabilities.canonical_json())
        .map_err(|error| error.to_string())
}

fn current_host_admission(
    server: &MemoryServer,
    requested_host_identity: Option<String>,
    requested_receipt_ref: Option<String>,
) -> Result<(HarnessSessionHostAdmission, String), String> {
    let (host_identity, connection_id, admission_state) = server
        .work_claim_connection()
        .ok_or_else(|| "current host admission is unavailable".to_string())?;
    let host_identity = host_identity
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| "current host admission has no host identity".to_string())?;
    if !matches!(admission_state.as_str(), "self_asserted" | "verified") {
        return Err("current host admission is not active".to_string());
    }
    let requested_host = required(requested_host_identity, "host_identity")?;
    if requested_host != host_identity {
        return Err("host_identity does not match the current host connection".to_string());
    }
    let receipt_ref = required(requested_receipt_ref, "admission_receipt_ref")?;
    Ok((
        HarnessSessionHostAdmission {
            host_identity,
            connection_id,
        },
        receipt_ref,
    ))
}

fn require_negotiated_v1(protocol_version: Option<i64>) -> Result<(), String> {
    if protocol_version == Some(1) {
        Ok(())
    } else {
        Err("ACP negotiated protocol_version must be exactly 1".to_string())
    }
}

fn attach_input(
    params: &TachiAgentEvalParams,
    host_identity: &str,
    admission_receipt_ref: &str,
    policy_digest: String,
    descriptor_digest: String,
) -> Result<NewHarnessSessionAttachment, String> {
    Ok(NewHarnessSessionAttachment {
        host_identity: host_identity.to_string(),
        identity_attribution_basis: TRUSTED_LOCAL_HOST_DECLARED_BASIS.to_string(),
        protocol_version: "1".to_string(),
        adapter_connection_identity: required(
            params.adapter_connection_identity.clone(),
            "adapter_connection_identity",
        )?,
        remote_session_id: required(params.remote_session_id.clone(), "remote_session_id")?,
        work_claim_id: required(params.work_claim_id.clone(), "work_claim_id")?,
        expected_transition_version: params
            .expected_transition_revision
            .ok_or_else(|| "expected_transition_revision is required".to_string())?,
        agent_identity_id: required(params.agent_identity_id.clone(), "agent_identity_id")?,
        contract_digest: required(params.contract_digest.clone(), "contract_digest")?,
        capabilities_json: capabilities(params)?,
        tool_profile: required(params.tool_profile.clone(), "tool_profile")?,
        capability_class: required(params.capability_class.clone(), "capability_class")?,
        policy_digest,
        descriptor_digest,
        idempotency_key: required(params.idempotency_key.clone(), "idempotency_key")?,
        admission_receipt_ref: admission_receipt_ref.to_string(),
    })
}

pub(crate) fn handle_attach_session(
    server: &MemoryServer,
    params: TachiAgentEvalParams,
) -> Result<String, String> {
    require_negotiated_v1(params.protocol_version)?;
    let (host, admission_receipt_ref) = current_host_admission(
        server,
        params.host_identity.clone(),
        params.admission_receipt_ref.clone(),
    )?;
    let agent_identity_id = required(params.agent_identity_id.clone(), "agent_identity_id")?;
    let tool_profile = canonical_name(params.tool_profile.clone(), "tool_profile")?;
    let capability_class = canonical_name(params.capability_class.clone(), "capability_class")?;
    let authorization = server.with_global_store_read(|store| {
        authorize_harness_session_attachment(
            store.connection(),
            &agent_identity_id,
            &tool_profile,
            &capability_class,
        )
        .map_err(|error| error.to_string())
    })?;
    let (descriptors, descriptor_digest) =
        project_descriptors(&tool_profile, &capability_class, &agent_identity_id)?;
    let mut input = attach_input(
        &params,
        &host.host_identity,
        &admission_receipt_ref,
        authorization.policy_digest.clone(),
        descriptor_digest.clone(),
    )?;
    input.policy_digest = authorization.policy_digest.clone();
    input.descriptor_digest = descriptor_digest.clone();
    let receipt = server.with_global_store(|store| {
        attach_harness_session(
            store.connection_mut(),
            &input,
            &host,
            crate::claims_ops::CLAIM_TTL_SECONDS,
        )
        .map_err(|error| error.to_string())
    })?;
    let admission = match receipt.admission {
        HarnessSessionAttachmentAdmission::Created => "created",
        HarnessSessionAttachmentAdmission::Replayed => "replayed",
    };
    serde_json::to_string(&json!({
        "status": "completed",
        "action": "attach_session",
        "admission": admission,
        "attachment_id": receipt.attachment.attachment_id,
        "state": receipt.attachment.state.as_str(),
            "binding": {
                "host_identity": receipt.attachment.host_identity,
                "identity_attribution_basis": receipt.attachment.identity_attribution_basis,
            "agent_identity_id": receipt.attachment.agent_identity_id,
            "work_claim_id": receipt.attachment.work_claim_id,
            "expected_transition_revision": receipt.attachment.expected_transition_version,
            "protocol_version": receipt.attachment.protocol_version,
            "adapter_connection_identity": receipt.attachment.adapter_connection_identity,
            "remote_session_id": receipt.attachment.remote_session_id,
            "contract_digest": receipt.attachment.contract_digest,
            "session_capabilities": serde_json::from_str::<Value>(&receipt.attachment.capabilities_json)
                .map_err(|error| error.to_string())?,
            "admission_receipt_ref": receipt.attachment.admission_receipt_ref,
        },
        "policy": {
            "tool_profile": receipt.attachment.tool_profile,
            "capability_class": receipt.attachment.capability_class,
            "decision": "allow",
            "policy_digest": authorization.policy_digest,
        },
        "descriptor_digest": descriptor_digest,
        "mcpServers": descriptors,
    }))
    .map_err(|error| format!("serialize ACP attachment receipt: {error}"))
}

fn redacted_descriptors(descriptors: Vec<Value>) -> Vec<Value> {
    descriptors
        .into_iter()
        .map(|mut descriptor| {
            if let Some(env) = descriptor.get_mut("env").and_then(Value::as_array_mut) {
                for entry in env {
                    if let Some(object) = entry.as_object_mut() {
                        object.insert("value".to_string(), Value::String("<redacted>".into()));
                    }
                }
            }
            descriptor
        })
        .collect()
}

pub(crate) fn handle_get_attachment(
    server: &MemoryServer,
    params: TachiAgentEvalParams,
) -> Result<String, String> {
    require_negotiated_v1(params.protocol_version)?;
    let (host, admission_receipt_ref) = current_host_admission(
        server,
        params.host_identity.clone(),
        params.admission_receipt_ref.clone(),
    )?;
    let selector = if let Some(attachment_id) = params.attachment_id.clone() {
        HarnessSessionAttachmentSelector::AttachmentId(required(
            Some(attachment_id),
            "attachment_id",
        )?)
    } else {
        HarnessSessionAttachmentSelector::NaturalKey {
            host_identity: host.host_identity.clone(),
            protocol_version: "1".to_string(),
            adapter_connection_identity: required(
                params.adapter_connection_identity.clone(),
                "adapter_connection_identity",
            )?,
            remote_session_id: required(params.remote_session_id.clone(), "remote_session_id")?,
        }
    };
    let attachment = server.with_global_store_read(|store| {
        get_harness_session_attachment(
            store.connection(),
            &selector,
            &host,
            &admission_receipt_ref,
            crate::claims_ops::CLAIM_TTL_SECONDS,
        )
        .map_err(|error| error.to_string())
    })?;
    let Some(attachment) = attachment else {
        return Err("ACP attachment was not found".to_string());
    };
    let authorization = server.with_global_store_read(|store| {
        authorize_harness_session_attachment(
            store.connection(),
            &attachment.agent_identity_id,
            &attachment.tool_profile,
            &attachment.capability_class,
        )
        .map_err(|error| error.to_string())
    })?;
    if authorization.policy_digest != attachment.policy_digest {
        return Err("ACP attachment descriptor policy drifted; refusing projection".to_string());
    }
    let (descriptors, descriptor_digest) = project_descriptors(
        &attachment.tool_profile,
        &attachment.capability_class,
        &attachment.agent_identity_id,
    )?;
    if descriptor_digest != attachment.descriptor_digest {
        return Err("ACP attachment descriptor policy drifted; refusing projection".to_string());
    }
    serde_json::to_string(&json!({
        "status": "completed",
        "action": "get_attachment",
        "attachment_id": attachment.attachment_id,
        "state": attachment.state.as_str(),
        "binding": {
            "host_identity": attachment.host_identity,
            "identity_attribution_basis": attachment.identity_attribution_basis,
            "agent_identity_id": attachment.agent_identity_id,
            "work_claim_id": attachment.work_claim_id,
            "expected_transition_revision": attachment.expected_transition_version,
            "protocol_version": attachment.protocol_version,
            "adapter_connection_identity": attachment.adapter_connection_identity,
            "remote_session_id": attachment.remote_session_id,
            "contract_digest": attachment.contract_digest,
            "session_capabilities": serde_json::from_str::<Value>(&attachment.capabilities_json)
                .map_err(|error| error.to_string())?,
            "admission_receipt_ref": attachment.admission_receipt_ref,
        },
        "policy": {
            "tool_profile": attachment.tool_profile,
            "capability_class": attachment.capability_class,
            "decision": "allow",
            "policy_digest": attachment.policy_digest,
        },
        "descriptor_digest": attachment.descriptor_digest,
        "mcpServers": redacted_descriptors(descriptors),
    }))
    .map_err(|error| format!("serialize ACP attachment projection: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool_params::TachiAgentEvalParams;
    use memcore::{
        insert_agent_identity, insert_work_claim, record_unverified_admission, AgentIdentity,
        NewWorkClaim, UnverifiedAdmissionState, WorkClaimMode,
    };

    fn test_server() -> MemoryServer {
        let db_path = crate::utils::test_fixture_path(format!(
            "acp-attachment-{}.sqlite",
            uuid::Uuid::new_v4()
        ));
        let server = MemoryServer::new(db_path, None).expect("test memory server");
        server.set_tool_profile(Some(tachi_hub::ToolProfile::coordinate()));
        server
    }

    fn seed_valid_admission(server: &MemoryServer) {
        server
            .with_global_store(|store| {
                insert_agent_identity(
                    store.connection(),
                    &AgentIdentity {
                        agent_identity_id: "host-1".to_string(),
                        display_name: None,
                        seat: None,
                        capability_json: None,
                        created_at: String::new(),
                    },
                )
                .map_err(|error| error.to_string())?;
                insert_agent_identity(
                    store.connection(),
                    &AgentIdentity {
                        agent_identity_id: "agent-1".to_string(),
                        display_name: None,
                        seat: None,
                        capability_json: Some(
                            r#"{"acp":{"tool_profiles":["delegate"],"capability_classes":["tachi"]}}"#
                                .to_string(),
                        ),
                        created_at: String::new(),
                    },
                )
                .map_err(|error| error.to_string())?;
                record_unverified_admission(
                    store.connection(),
                    "admission-1",
                    "host-1",
                    "connection-1",
                    UnverifiedAdmissionState::SelfAsserted,
                )
                .map_err(|error| error.to_string())?;
                insert_work_claim(
                    store.connection_mut(),
                    &NewWorkClaim {
                        claim_id: "claim-1".to_string(),
                        agent_identity_id: "agent-1".to_string(),
                        session_client: Some("connection-1".to_string()),
                        issue_ref: None,
                        flow_id: None,
                        dispatch_id: None,
                        branch: "branch".to_string(),
                        worktree_path: "/tmp/acp-attachment-claim".to_string(),
                        declared_file_scope: "[\"src/lib.rs\"]".to_string(),
                        role: "executor".to_string(),
                        mode: WorkClaimMode::Writable,
                        expected_head: "head".to_string(),
                        lease_expires_at: "2030-01-01T00:00:00Z".to_string(),
                        created_at: String::new(),
                    },
                )
                .map_err(|error| error.to_string())?;
                Ok(())
            })
            .expect("seed valid ACP attachment admission");
        server.set_work_claim_connection(
            Some("host-1".to_string()),
            "connection-1".to_string(),
            "self_asserted".to_string(),
        );
    }

    fn attachment_params(action: &str, idempotency_key: &str) -> TachiAgentEvalParams {
        TachiAgentEvalParams {
            action: action.to_string(),
            host_identity: Some("host-1".to_string()),
            agent_identity_id: Some("agent-1".to_string()),
            work_claim_id: Some("claim-1".to_string()),
            expected_transition_revision: Some(0),
            protocol_version: Some(1),
            adapter_connection_identity: Some("adapter-1".to_string()),
            remote_session_id: Some("remote-1".to_string()),
            contract_digest: Some("contract-digest".to_string()),
            session_capabilities: vec!["observe".to_string(), "load".to_string()],
            tool_profile: Some("delegate".to_string()),
            capability_class: Some("tachi".to_string()),
            idempotency_key: Some(idempotency_key.to_string()),
            admission_receipt_ref: Some("admission-1".to_string()),
            ..Default::default()
        }
    }

    fn row_count(server: &MemoryServer) -> i64 {
        server
            .with_global_store_read(|store| {
                store
                    .connection()
                    .query_row(
                        "SELECT COUNT(*) FROM harness_session_attachments",
                        [],
                        |row| row.get(0),
                    )
                    .map_err(|error| error.to_string())
            })
            .expect("attachment row count")
    }

    fn stored_rows(server: &MemoryServer) -> String {
        server
            .with_global_store_read(|store| {
                store
                    .connection()
                    .query_row(
                        "SELECT COALESCE(json_group_array(json_object(
                            'attachment_id', attachment_id,
                            'host_identity', host_identity,
                            'identity_attribution_basis', identity_attribution_basis,
                            'protocol_version', protocol_version,
                            'adapter_connection_identity', adapter_connection_identity,
                            'remote_session_id', remote_session_id,
                            'work_claim_id', work_claim_id,
                            'expected_transition_version', expected_transition_version,
                            'agent_identity_id', agent_identity_id,
                            'contract_digest', contract_digest,
                            'capabilities_json', capabilities_json,
                            'tool_profile', tool_profile,
                            'capability_class', capability_class,
                            'policy_digest', policy_digest,
                            'descriptor_digest', descriptor_digest,
                            'idempotency_key', idempotency_key,
                            'admission_receipt_ref', admission_receipt_ref,
                            'state', state,
                            'created_at', created_at,
                            'updated_at', updated_at
                        )), '[]') FROM harness_session_attachments",
                        [],
                        |row| row.get(0),
                    )
                    .map_err(|error| error.to_string())
            })
            .expect("stored attachment bytes")
    }

    fn set_capability_json(server: &MemoryServer, capability_json: Option<&str>) {
        server
            .with_global_store(|store| {
                store
                    .connection()
                    .execute(
                        "UPDATE agent_identities SET capability_json = ?1 WHERE agent_identity_id = 'agent-1'",
                        rusqlite::params![capability_json],
                    )
                    .map(|_| ())
                    .map_err(|error| error.to_string())
            })
            .expect("update stored ACP grant");
    }

    #[test]
    fn descriptor_projection_is_an_acp_array_with_absolute_stdio_command() {
        let (servers, descriptor_digest) =
            project_descriptors("delegate", "tachi", "agent-1").unwrap();
        assert_eq!(servers.len(), 1);
        assert!(servers[0]["command"]
            .as_str()
            .is_some_and(|path| { std::path::Path::new(path).is_absolute() }));
        assert!(servers[0]["args"].is_array());
        assert!(servers[0]["env"][0]["name"].is_string());
        assert!(servers[0]["env"][0]["value"].is_string());
        assert!(!descriptor_digest.is_empty());
    }

    #[test]
    fn non_canonical_policy_names_are_refused_before_projection() {
        assert!(project_descriptors("admin", "tachi", "agent-1").is_err());
        for (tool_profile, capability_class) in [
            ("coordinate", "tachi"),
            ("standard", "tachi"),
            ("delegate", "memory"),
            ("observe", "standard"),
            ("delegate", "admin"),
            ("STANDARD", "tachi"),
        ] {
            assert!(
                project_descriptors(tool_profile, capability_class, "agent-1").is_err(),
                "{tool_profile}/{capability_class} must not materialize an ACP descriptor"
            );
        }
    }

    #[tokio::test]
    async fn attach_session_facade_replays_the_same_receipt_and_descriptor() {
        let server = test_server();
        seed_valid_admission(&server);
        let params = attachment_params("attach_session", "idem-1");
        let first_raw = crate::agent_eval::handle_agent_eval(&server, params.clone())
            .await
            .expect("valid ACP attachment");
        assert!(!first_raw.contains("\"method\""));
        assert!(!first_raw.contains("\"params\""));
        assert!(!first_raw.contains("session/new"));
        assert!(!first_raw.contains("session/load"));
        assert!(!first_raw.contains("session/resume"));
        let first: Value = serde_json::from_str(&first_raw).expect("first attachment JSON");
        let replay: Value = serde_json::from_str(
            &crate::agent_eval::handle_agent_eval(&server, params)
                .await
                .expect("exact replay"),
        )
        .expect("replay attachment JSON");
        assert_eq!(first["admission"], "created");
        assert_eq!(replay["admission"], "replayed");
        assert_eq!(first["attachment_id"], replay["attachment_id"]);
        assert_eq!(first["descriptor_digest"], replay["descriptor_digest"]);
        assert_eq!(first["mcpServers"], replay["mcpServers"]);
        assert!(first["mcpServers"].is_array());
        assert!(first["mcpServers"][0]["command"]
            .as_str()
            .is_some_and(|command| std::path::Path::new(command).is_absolute()));
        assert!(first["mcpServers"][0]["env"]
            .as_array()
            .unwrap()
            .iter()
            .all(|entry| entry["name"].is_string() && entry["value"].is_string()));
        assert_eq!(row_count(&server), 1);
        let stored = stored_rows(&server);
        assert!(!stored.contains("TACHI_PROFILE"));
        assert!(!stored.contains("api_key"));
        assert_eq!(first["state"], "attached");
        assert_eq!(
            first["binding"]["identity_attribution_basis"],
            TRUSTED_LOCAL_HOST_DECLARED_BASIS
        );
    }

    #[tokio::test]
    async fn changed_binding_conflicts_without_mutating_the_receipt() {
        let server = test_server();
        seed_valid_admission(&server);
        let params = attachment_params("attach_session", "idem-2");
        crate::agent_eval::handle_agent_eval(&server, params.clone())
            .await
            .expect("valid ACP attachment");
        let before = stored_rows(&server);
        let mut changed = params;
        changed.remote_session_id = Some("remote-2".to_string());
        let error = crate::agent_eval::handle_agent_eval(&server, changed)
            .await
            .expect_err("changed binding must conflict");
        assert!(error.contains("conflicts"), "{error}");
        assert_eq!(row_count(&server), 1);
        assert_eq!(stored_rows(&server), before);
    }

    #[tokio::test]
    async fn invalid_admission_claim_and_capability_fail_before_insert() {
        let server = test_server();
        seed_valid_admission(&server);

        let mut stale = attachment_params("attach_session", "idem-stale");
        stale.expected_transition_revision = Some(1);
        let error = crate::agent_eval::handle_agent_eval(&server, stale)
            .await
            .expect_err("stale claim must be refused");
        assert!(
            error.contains("expected") || error.contains("revision"),
            "{error}"
        );
        assert_eq!(row_count(&server), 0);

        let mut unsupported = attachment_params("attach_session", "idem-capability");
        unsupported.session_capabilities = vec!["invented".to_string()];
        let error = crate::agent_eval::handle_agent_eval(&server, unsupported)
            .await
            .expect_err("unsupported capability must be refused");
        assert!(
            error.contains("unsupported ACP session capability"),
            "{error}"
        );
        assert_eq!(row_count(&server), 0);

        let mut rejected = attachment_params("attach_session", "idem-admission");
        rejected.admission_receipt_ref = Some("missing-admission".to_string());
        let error = crate::agent_eval::handle_agent_eval(&server, rejected)
            .await
            .expect_err("missing admission must be refused");
        assert!(error.contains("admission receipt"), "{error}");
        assert_eq!(row_count(&server), 0);
    }

    #[tokio::test]
    async fn unauthorized_policy_and_new_session_action_are_refused() {
        let server = test_server();
        seed_valid_admission(&server);
        let mut unauthorized = attachment_params("attach_session", "idem-policy");
        unauthorized.tool_profile = Some("admin".to_string());
        let error = crate::agent_eval::handle_agent_eval(&server, unauthorized)
            .await
            .expect_err("unauthorized policy must be refused");
        assert!(
            error.contains("canonical admitted") || error.contains("not admitted"),
            "{error}"
        );
        assert_eq!(row_count(&server), 0);

        for (grant, idempotency_key, expected, requested_profile, requested_class) in [
            (
                r#"{"other":{"tool_profiles":["delegate"]}}"#,
                "idem-missing-acp",
                "missing object acp",
                "delegate",
                "tachi",
            ),
            (
                r#"{"acp":{"tool_profiles":["delegate","delegate"],"capability_classes":["tachi"]}}"#,
                "idem-duplicate-acp",
                "duplicate name",
                "delegate",
                "tachi",
            ),
            (
                r#"{"acp":{"tool_profiles":["admin"],"capability_classes":["tachi"]}}"#,
                "idem-unknown-acp",
                "unknown canonical name",
                "delegate",
                "tachi",
            ),
            (
                r#"{"acp":{"tool_profiles":[1],"capability_classes":["tachi"]}}"#,
                "idem-non-string-acp",
                "entries must be strings",
                "delegate",
                "tachi",
            ),
            (
                r#"{"acp":{"tool_profiles":["observe"],"capability_classes":["tachi"]}}"#,
                "idem-absent-profile",
                "not admitted",
                "delegate",
                "tachi",
            ),
            (
                r#"{"acp":{"tool_profiles":["coordinate"],"capability_classes":["tachi"]}}"#,
                "idem-coordinate-profile",
                "not a canonical admitted profile",
                "coordinate",
                "tachi",
            ),
            (
                r#"{"acp":{"tool_profiles":["standard"],"capability_classes":["tachi"]}}"#,
                "idem-standard-profile",
                "not a canonical admitted profile",
                "standard",
                "tachi",
            ),
            (
                r#"{"acp":{"tool_profiles":["delegate"],"capability_classes":["memory"]}}"#,
                "idem-memory-class",
                "not a canonical admitted class",
                "delegate",
                "memory",
            ),
            (
                r#"{"acp":{"tool_profiles":["observe"],"capability_classes":["standard"]}}"#,
                "idem-standard-class",
                "not a canonical admitted class",
                "observe",
                "standard",
            ),
        ] {
            set_capability_json(&server, Some(grant));
            let mut params = attachment_params("attach_session", idempotency_key);
            params.tool_profile = Some(requested_profile.to_string());
            params.capability_class = Some(requested_class.to_string());
            let error = crate::agent_eval::handle_agent_eval(&server, params)
                .await
                .expect_err("malformed or insufficient ACP grant must refuse");
            assert!(error.contains(expected), "expected {expected} in {error}");
            assert_eq!(row_count(&server), 0);
        }

        for action in ["session/new", "session/load", "session/resume"] {
            let error = crate::agent_eval::handle_agent_eval(
                &server,
                TachiAgentEvalParams {
                    action: action.to_string(),
                    ..Default::default()
                },
            )
            .await
            .expect_err("ACP lifecycle actions remain host-owned");
            assert!(error.contains("Invalid eval action"), "{error}");
        }
        assert_eq!(row_count(&server), 0);
    }

    #[tokio::test]
    async fn opaque_binding_whitespace_conflicts_without_mutating_the_full_row() {
        let server = test_server();
        seed_valid_admission(&server);
        let params = attachment_params("attach_session", "idem-opaque-whitespace");
        crate::agent_eval::handle_agent_eval(&server, params.clone())
            .await
            .expect("valid ACP attachment");
        let before = stored_rows(&server);

        for field in [
            "adapter_connection_identity",
            "remote_session_id",
            "work_claim_id",
            "contract_digest",
            "admission_receipt_ref",
        ] {
            let mut changed = params.clone();
            match field {
                "adapter_connection_identity" => {
                    changed.adapter_connection_identity = Some(" adapter-1 ".to_string())
                }
                "remote_session_id" => changed.remote_session_id = Some(" remote-1 ".to_string()),
                "work_claim_id" => changed.work_claim_id = Some(" claim-1 ".to_string()),
                "contract_digest" => {
                    changed.contract_digest = Some(" contract-digest ".to_string())
                }
                "admission_receipt_ref" => {
                    changed.admission_receipt_ref = Some(" admission-1 ".to_string())
                }
                _ => unreachable!("test field must be covered"),
            }
            let error = crate::agent_eval::handle_agent_eval(&server, changed)
                .await
                .expect_err("opaque whitespace changes must conflict with the original binding");
            assert!(error.contains("conflicts"), "{field}: {error}");
            assert_eq!(row_count(&server), 1, "{field} must not add a row");
            assert_eq!(
                stored_rows(&server),
                before,
                "{field} must preserve every row byte"
            );
        }
    }

    #[tokio::test]
    async fn missing_null_and_malformed_identity_grants_fail_with_typed_refusals() {
        let server = test_server();
        seed_valid_admission(&server);

        let mut missing_identity = attachment_params("attach_session", "idem-missing-identity");
        missing_identity.agent_identity_id = Some("agent-missing".to_string());
        let error = crate::agent_eval::handle_agent_eval(&server, missing_identity)
            .await
            .expect_err("missing identity must refuse before insert");
        assert!(error.contains("unknown agent identity"), "{error}");
        assert!(!error.contains("Invalid column type"), "{error}");
        assert_eq!(row_count(&server), 0);

        for (grant, idempotency_key, expected) in [
            (None, "idem-null-grant", "no ACP capability grant"),
            (
                Some("not-json"),
                "idem-malformed-grant",
                "malformed capability_json",
            ),
        ] {
            set_capability_json(&server, grant);
            let error = crate::agent_eval::handle_agent_eval(
                &server,
                attachment_params("attach_session", idempotency_key),
            )
            .await
            .expect_err("missing or malformed grant must refuse");
            assert!(error.contains(expected), "expected {expected} in {error}");
            assert!(!error.contains("Invalid column type"), "{error}");
            assert_eq!(row_count(&server), 0);
        }
    }

    #[tokio::test]
    async fn replay_after_claim_release_refuses_without_mutating_the_full_row() {
        let server = test_server();
        seed_valid_admission(&server);
        let params = attachment_params("attach_session", "idem-release-replay");
        crate::agent_eval::handle_agent_eval(&server, params.clone())
            .await
            .expect("valid ACP attachment");
        let before = stored_rows(&server);
        server
            .with_global_store(|store| {
                store
                    .connection()
                    .execute(
                        "UPDATE session_claims
                         SET state = 'released', transition_version = transition_version + 1
                         WHERE claim_id = 'claim-1'",
                        [],
                    )
                    .map(|_| ())
                    .map_err(|error| error.to_string())
            })
            .expect("release the authoritative WorkClaim");

        let error = crate::agent_eval::handle_agent_eval(&server, params)
            .await
            .expect_err("replay must revalidate the released claim");
        assert!(error.contains("released"), "{error}");
        assert_eq!(row_count(&server), 1);
        assert_eq!(stored_rows(&server), before);
    }

    #[tokio::test]
    async fn get_attachment_is_natural_key_read_only_and_redacts_env_values() {
        let server = test_server();
        seed_valid_admission(&server);
        let params = attachment_params("attach_session", "idem-get");
        let created: Value = serde_json::from_str(
            &crate::agent_eval::handle_agent_eval(&server, params.clone())
                .await
                .expect("valid ACP attachment"),
        )
        .expect("attachment JSON");
        let attachment_id = created["attachment_id"].as_str().unwrap().to_string();
        let before = stored_rows(&server);

        let mut get = attachment_params("get_attachment", "unused");
        get.attachment_id = Some(attachment_id);
        let projection: Value = serde_json::from_str(
            &crate::agent_eval::handle_agent_eval(&server, get)
                .await
                .expect("get attachment"),
        )
        .expect("projection JSON");
        assert_eq!(projection["mcpServers"].as_array().unwrap().len(), 1);
        for entry in projection["mcpServers"][0]["env"].as_array().unwrap() {
            assert_eq!(entry["value"], "<redacted>");
        }
        assert_eq!(stored_rows(&server), before);
    }

    #[tokio::test]
    async fn host_runtime_binding_version_and_foreign_host_are_fail_closed() {
        let server = test_server();
        seed_valid_admission(&server);

        for version in [0, 2] {
            let mut params = attachment_params("attach_session", &format!("idem-v{version}"));
            params.protocol_version = Some(version);
            let error = crate::agent_eval::handle_agent_eval(&server, params)
                .await
                .expect_err("unstable ACP version must refuse before insert");
            assert!(error.contains("exactly 1"), "{error}");
            assert_eq!(row_count(&server), 0);
        }

        let mut mismatched_host = attachment_params("attach_session", "idem-host-mismatch");
        mismatched_host.host_identity = Some("request-only-host".to_string());
        let error = crate::agent_eval::handle_agent_eval(&server, mismatched_host)
            .await
            .expect_err("request-only host identity must not authorize attachment");
        assert!(error.contains("current host connection"), "{error}");
        assert_eq!(row_count(&server), 0);

        let created: Value = serde_json::from_str(
            &crate::agent_eval::handle_agent_eval(
                &server,
                attachment_params("attach_session", "idem-foreign-host"),
            )
            .await
            .expect("valid attachment"),
        )
        .unwrap();
        for version in [0, 2] {
            let mut get = attachment_params("get_attachment", &format!("unused-v{version}"));
            get.protocol_version = Some(version);
            get.attachment_id = None;
            let error = crate::agent_eval::handle_agent_eval(&server, get)
                .await
                .expect_err("unstable ACP natural-key version must refuse");
            assert!(error.contains("exactly 1"), "{error}");
        }
        let before = stored_rows(&server);
        server
            .with_global_store(|store| {
                insert_agent_identity(
                    store.connection(),
                    &AgentIdentity {
                        agent_identity_id: "host-2".to_string(),
                        display_name: None,
                        seat: None,
                        capability_json: None,
                        created_at: String::new(),
                    },
                )
                .map_err(|error| error.to_string())?;
                record_unverified_admission(
                    store.connection(),
                    "admission-2",
                    "host-2",
                    "connection-2",
                    UnverifiedAdmissionState::SelfAsserted,
                )
                .map_err(|error| error.to_string())
            })
            .expect("seed second host admission");
        server.set_work_claim_connection(
            Some("host-2".to_string()),
            "connection-2".to_string(),
            "self_asserted".to_string(),
        );

        let mut foreign_id = attachment_params("get_attachment", "unused");
        foreign_id.attachment_id = created["attachment_id"].as_str().map(str::to_string);
        foreign_id.host_identity = Some("host-2".to_string());
        foreign_id.admission_receipt_ref = Some("admission-2".to_string());
        let error = crate::agent_eval::handle_agent_eval(&server, foreign_id)
            .await
            .expect_err("foreign host must not learn an attachment by id");
        assert_eq!(error, "ACP attachment was not found");
        assert_eq!(stored_rows(&server), before);

        let mut foreign_natural = attachment_params("get_attachment", "unused-natural");
        foreign_natural.host_identity = Some("host-2".to_string());
        foreign_natural.admission_receipt_ref = Some("admission-2".to_string());
        let error = crate::agent_eval::handle_agent_eval(&server, foreign_natural)
            .await
            .expect_err("foreign host must not learn an attachment by natural key");
        assert_eq!(error, "ACP attachment was not found");
        assert_eq!(stored_rows(&server), before);
    }

    #[tokio::test]
    async fn heartbeat_and_lease_equivalence_classes_refuse_attach_replay_and_get() {
        for (field, value, expected) in [
            ("heartbeat_at", "2000-01-01T00:00:00Z", "heartbeat is stale"),
            ("heartbeat_at", "not-a-timestamp", "heartbeat is stale"),
            (
                "lease_expires_at",
                "2000-01-01T00:00:00Z",
                "lease is expired",
            ),
            (
                "lease_expires_at",
                "not-a-timestamp",
                "lease is missing or malformed",
            ),
        ] {
            let server = test_server();
            seed_valid_admission(&server);
            server
                .with_global_store(|store| {
                    store
                        .connection()
                        .execute(
                            &format!(
                                "UPDATE session_claims SET {field} = ?1 WHERE claim_id = 'claim-1'"
                            ),
                            [value],
                        )
                        .map(|_| ())
                        .map_err(|error| error.to_string())
                })
                .expect("mutate claim freshness fixture");
            let error = crate::agent_eval::handle_agent_eval(
                &server,
                attachment_params("attach_session", &format!("idem-{field}-{value}")),
            )
            .await
            .expect_err("stale or malformed claim must refuse attach");
            assert!(error.contains(expected), "expected {expected} in {error}");
            assert_eq!(row_count(&server), 0);

            let server = test_server();
            seed_valid_admission(&server);
            let created: Value = serde_json::from_str(
                &crate::agent_eval::handle_agent_eval(
                    &server,
                    attachment_params("attach_session", &format!("idem-replay-{field}")),
                )
                .await
                .expect("fresh claim attaches"),
            )
            .unwrap();
            let before = stored_rows(&server);
            server
                .with_global_store(|store| {
                    store
                        .connection()
                        .execute(
                            &format!(
                                "UPDATE session_claims SET {field} = ?1 WHERE claim_id = 'claim-1'"
                            ),
                            [value],
                        )
                        .map(|_| ())
                        .map_err(|error| error.to_string())
                })
                .expect("mutate claim freshness fixture");
            let error = crate::agent_eval::handle_agent_eval(
                &server,
                attachment_params("attach_session", &format!("idem-replay-{field}")),
            )
            .await
            .expect_err("stale or malformed claim must refuse exact replay");
            assert!(error.contains(expected), "expected {expected} in {error}");
            assert_eq!(stored_rows(&server), before);

            let mut get = attachment_params("get_attachment", "unused");
            get.attachment_id = created["attachment_id"].as_str().map(str::to_string);
            let error = crate::agent_eval::handle_agent_eval(&server, get)
                .await
                .expect_err("stale or malformed claim must refuse get");
            assert!(error.contains(expected), "expected {expected} in {error}");
            assert_eq!(stored_rows(&server), before);
        }
    }
}
