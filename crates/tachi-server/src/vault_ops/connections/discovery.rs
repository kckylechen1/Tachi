use memcore::vault::{VaultEntry, VaultKeyRotation};
use rmcp::schemars::{self, JsonSchema};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::{
    load_catalog, select_connection, Connection, ConnectionContext, ConnectionKind,
    ConnectionTarget,
};
use crate::vault_ops::access::ensure_agent_allowed;
use crate::vault_ops::ctx::VaultCtx;

#[derive(Debug, Clone, Copy, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[schemars(inline)]
pub(crate) enum ConnectionAction {
    List,
    Describe,
    Prepare,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ConnectionQuery {
    pub action: ConnectionAction,
    /// Required for describe and prepare.
    pub name: Option<String>,
    /// List filter: api or ssh.
    pub kind: Option<ConnectionKind>,
    /// List filter: a substring of the connection name or purpose.
    pub query: Option<String>,
    /// List size, from 1 to 100. Defaults to 50.
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum CredentialStatus {
    NotApplicable,
    Configured,
    Missing,
    InvalidRotation,
}

struct CredentialMetadata {
    status: CredentialStatus,
    requires_bound_consumer: bool,
}

/// Metadata only: no decrypt, rotation advance, health mutation, or access touch.
/// A pool is visible only when every possible selected member is authorized.
fn credential_metadata(
    connection: &Connection,
    context: &ConnectionContext,
    entries: &[VaultEntry],
    rotations: &[VaultKeyRotation],
) -> Option<CredentialMetadata> {
    let ConnectionTarget::Api { env, .. } = &connection.target else {
        return Some(CredentialMetadata {
            status: CredentialStatus::NotApplicable,
            requires_bound_consumer: false,
        });
    };
    let mut metadata = CredentialMetadata {
        status: CredentialStatus::Configured,
        requires_bound_consumer: false,
    };
    for reference in env.values() {
        let name = reference.strip_prefix("vault:")?;
        let rotation = rotations.iter().find(|row| row.prefix == name);
        // Direct member reads validate a configured parent rotation before
        // selecting the named entry. Keep metadata diagnostics aligned.
        let parent_rotation = crate::provider_config::parse_rotation_member_name(name)
            .and_then(|(prefix, _)| rotations.iter().find(|row| row.prefix == prefix));
        let candidates: Vec<_> = match rotation {
            Some(_) => entries
                .iter()
                .filter(|entry| memcore::api_key_pool_member_index(&entry.name, name).is_some())
                .collect(),
            None => entries.iter().filter(|entry| entry.name == name).collect(),
        };
        if candidates
            .iter()
            .any(|entry| ensure_agent_allowed(entry, context.agent_id.as_deref()).is_err())
        {
            return None;
        }
        metadata.requires_bound_consumer |= candidates
            .iter()
            .any(|entry| entry.allowed_agents.is_some());
        if candidates.is_empty() {
            metadata.status = CredentialStatus::Missing;
        } else if [rotation, parent_rotation]
            .into_iter()
            .flatten()
            .any(|rotation| memcore::validate_api_key_rotation(entries, rotation).is_err())
            && metadata.status != CredentialStatus::Missing
        {
            metadata.status = CredentialStatus::InvalidRotation;
        }
    }
    Some(metadata)
}

fn describe(
    connection: &Connection,
    context: &ConnectionContext,
    metadata: &CredentialMetadata,
) -> Value {
    let mut blockers = Vec::new();
    if !connection.enabled {
        blockers.push("disabled");
    }
    if connection
        .executor
        .as_ref()
        .is_some_and(|node| node != &context.node || context.node == "unknown_host")
    {
        blockers.push("different_executor");
    }
    if metadata.status == CredentialStatus::Missing {
        blockers.push("credential_missing");
    }
    if metadata.status == CredentialStatus::InvalidRotation {
        blockers.push("invalid_rotation");
    }
    // Standalone CLI uses local host authority and cannot impersonate the MCP
    // server's bound consumer. Do not offer an unusable launch recipe.
    if !connection.allowed_agents.is_empty() || metadata.requires_bound_consumer {
        blockers.push("requires_bound_consumer");
    }
    json!({
        "name": connection.name,
        "purpose": connection.purpose,
        "kind": connection.kind(),
        "enabled": connection.enabled,
        "executor": connection.executor,
        "target": connection.target,
        "credential_status": metadata.status,
        "blockers": blockers,
        "connection_check": "not_performed",
        "credentials_resolved": false
    })
}

pub(crate) fn handle_connections(
    server: &impl VaultCtx,
    params: ConnectionQuery,
    context: ConnectionContext,
) -> Result<String, String> {
    let limit = params.limit.unwrap_or(50);
    if !(1..=100).contains(&limit) || params.query.as_ref().is_some_and(|query| query.len() > 256) {
        return Err("Use a limit from 1 to 100 and a query of at most 256 bytes".into());
    }
    if params.action != ConnectionAction::List
        && params.name.as_ref().is_none_or(|name| name.is_empty())
    {
        return Err("Connection name is required for describe and prepare".into());
    }
    let db = server
        .global_db_path_buf()
        .canonicalize()
        .map_err(|_| "Cannot establish the connection catalog location".to_string())?;
    let catalog = load_catalog(&db)?;
    let visible: Vec<_> = if params.action == ConnectionAction::List {
        let query = params.query.as_deref().unwrap_or("").to_lowercase();
        catalog
            .connections
            .iter()
            .filter(|connection| {
                connection.visible_to(&context)
                    && params.kind.is_none_or(|kind| connection.kind() == kind)
                    && (connection.name.to_lowercase().contains(&query)
                        || connection.purpose.to_lowercase().contains(&query))
            })
            .collect()
    } else {
        vec![select_connection(
            &catalog,
            params.name.as_deref().unwrap_or_default(),
            &context,
        )?]
    };
    // SSH-only discovery does not require a configured or unlocked Vault.
    let (entries, rotations) = if visible
        .iter()
        .any(|connection| connection.kind() == ConnectionKind::Api)
    {
        server.with_global_store_read(|store| {
            Ok((
                store
                    .vault_list_entries()
                    .map_err(|_| "Cannot inspect credential metadata".to_string())?,
                store
                    .vault_list_rotations()
                    .map_err(|_| "Cannot inspect credential metadata".to_string())?,
            ))
        })?
    } else {
        (Vec::new(), Vec::new())
    };
    let mut records: Vec<_> = visible
        .into_iter()
        .filter_map(|connection| {
            credential_metadata(connection, &context, &entries, &rotations)
                .map(|metadata| describe(connection, &context, &metadata))
        })
        .collect();
    if params.action != ConnectionAction::List && records.is_empty() {
        return Err("Connection is unavailable to this consumer or project".into());
    }
    let mut response = json!({
        "catalog_revision": catalog.revision,
        "current_executor": context.node,
        "current_project": context.project,
        "execution": "not_started"
    });
    if params.action == ConnectionAction::List {
        response["truncated"] = json!(records.len() > limit);
        records.truncate(limit);
        response["connections"] = json!(records);
    } else {
        let record = records.remove(0);
        if params.action == ConnectionAction::Prepare
            && record["blockers"].as_array().is_some_and(Vec::is_empty)
        {
            response["launch"] = json!({
                "argv_prefix": ["tachi", "--global-db", db.to_string_lossy().as_ref(), "vault", "connection", "run", params.name.as_deref().unwrap_or_default(), "--expect-revision", &catalog.revision.to_string()],
                "command_separator": "--",
                "authorization": "existing_host_execution",
                "credential_resolution": if record["kind"] == "api" { "at_run_with_cli_unlock_source" } else { "system_ssh_configuration" },
                "project_scope": "checked_against_run_cwd"
            });
        }
        response["connection"] = record;
    }
    serde_json::to_string_pretty(&response).map_err(|_| "Cannot encode connection response".into())
}
