//! Named, non-secret connection descriptions. Preparing a connection never
//! decrypts credentials or executes host configuration.

use std::collections::{BTreeMap, HashSet};
use std::path::Path;

use rmcp::schemars::{self, JsonSchema};
use serde::{Deserialize, Serialize};

mod discovery;
mod storage;
#[cfg(test)]
mod tests;

pub(crate) use discovery::{handle_connections, ConnectionQuery};
pub(crate) use storage::{catalog_path, load_catalog, put_connection, remove_connection};

pub(crate) const CATALOG_VERSION: u32 = 1;
pub(crate) const MAX_CATALOG_BYTES: u64 = 512 * 1024;
const MAX_CONNECTIONS: usize = 256;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ConnectionCatalog {
    pub version: u32,
    pub revision: u64,
    pub connections: Vec<Connection>,
}

impl Default for ConnectionCatalog {
    fn default() -> Self {
        Self {
            version: CATALOG_VERSION,
            revision: 0,
            connections: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Connection {
    pub name: String,
    pub purpose: String,
    #[serde(default = "enabled_by_default")]
    pub enabled: bool,
    #[serde(default)]
    pub allowed_agents: Vec<String>,
    #[serde(default)]
    pub allowed_projects: Vec<String>,
    /// Local host-name constraint, not authenticated remote identity.
    #[serde(default)]
    pub executor: Option<String>,
    pub target: ConnectionTarget,
}

fn enabled_by_default() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum ConnectionTarget {
    Api {
        /// Environment variable -> vault:NAME. Literal secrets are rejected.
        env: BTreeMap<String, String>,
        #[serde(default)]
        endpoint: Option<String>,
        #[serde(default)]
        model: Option<String>,
    },
    Ssh {
        /// An already trusted OpenSSH Host alias on the execution host.
        alias: String,
        #[serde(default)]
        remote_cwd: Option<String>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ConnectionKind {
    Api,
    Ssh,
}

#[derive(Debug, Clone)]
pub(crate) struct ConnectionContext {
    pub agent_id: Option<String>,
    pub project: Option<String>,
    pub node: String,
}

impl Connection {
    pub(crate) fn kind(&self) -> ConnectionKind {
        match self.target {
            ConnectionTarget::Api { .. } => ConnectionKind::Api,
            ConnectionTarget::Ssh { .. } => ConnectionKind::Ssh,
        }
    }

    pub(crate) fn visible_to(&self, context: &ConnectionContext) -> bool {
        (self.allowed_agents.is_empty()
            || context
                .agent_id
                .as_ref()
                .is_some_and(|id| self.allowed_agents.contains(id)))
            && (self.allowed_projects.is_empty()
                || context
                    .project
                    .as_ref()
                    .is_some_and(|project| self.allowed_projects.contains(project)))
    }

    pub(crate) fn check_execution(&self, context: &ConnectionContext) -> Result<(), String> {
        if !self.visible_to(context) {
            return Err("Connection is unavailable to this consumer or project".into());
        }
        if !self.enabled {
            return Err("Connection is disabled".into());
        }
        if self
            .executor
            .as_ref()
            .is_some_and(|node| node != &context.node || context.node == "unknown_host")
        {
            return Err("Connection requires a different execution node".into());
        }
        Ok(())
    }
}

pub(crate) fn current_node() -> String {
    crate::managed_run_epoch::host_ref()
}

/// CLI authority remains the local host's existing execution authority. It
/// cannot claim a bound Agent identity by passing an argument or environment.
pub(crate) fn context_for_cli(cwd: &Path) -> ConnectionContext {
    let project = crate::utils::find_git_root_from(cwd)
        .and_then(|root| crate::path_utils::plan_c_dir_name_from_root(&root));
    ConnectionContext {
        agent_id: None,
        project,
        node: current_node(),
    }
}

pub(crate) fn select_connection<'a>(
    catalog: &'a ConnectionCatalog,
    name: &str,
    context: &ConnectionContext,
) -> Result<&'a Connection, String> {
    catalog
        .connections
        .iter()
        .find(|connection| connection.name == name && connection.visible_to(context))
        .ok_or_else(|| "Connection is unavailable to this consumer or project".to_string())
}

pub(crate) fn parse_connection(bytes: &[u8]) -> Result<Connection, String> {
    if bytes.len() > MAX_CATALOG_BYTES as usize {
        return Err("Connection definition is too large".into());
    }
    let connection: Connection = serde_json::from_slice(bytes).map_err(|error| {
        format!("Invalid connection definition at line {}, column {}; only documented non-secret fields are accepted", error.line(), error.column())
    })?;
    validate_connection(&connection)?;
    Ok(connection)
}

fn valid_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value.as_bytes()[0].is_ascii_alphanumeric()
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
}

fn safe_text(value: &str, max: usize) -> bool {
    !value.is_empty()
        && value.len() <= max
        && !value.chars().any(char::is_control)
        && !value.contains("-----BEGIN")
}

pub(crate) fn validate_connection(connection: &Connection) -> Result<(), String> {
    if !valid_name(&connection.name) {
        return Err("Invalid connection name".into());
    }
    if !safe_text(&connection.purpose, 512) {
        return Err("Invalid connection purpose".into());
    }
    if connection.allowed_agents.len() > 64
        || connection.allowed_projects.len() > 64
        || connection
            .allowed_agents
            .iter()
            .any(|id| !safe_text(id, 128))
        || connection
            .allowed_projects
            .iter()
            .any(|id| !crate::path_utils::is_canonical_project_identity(id))
    {
        return Err("Invalid connection scope".into());
    }
    if connection
        .executor
        .as_ref()
        .is_some_and(|node| !valid_name(node))
    {
        return Err("Invalid execution node".into());
    }
    match &connection.target {
        ConnectionTarget::Api {
            env,
            endpoint,
            model,
        } => {
            if env.is_empty() || env.len() > 64 {
                return Err(
                    "API connection must declare 1 to 64 Vault environment bindings".into(),
                );
            }
            for (name, reference) in env {
                if name.len() > 128
                    || !crate::utils::is_shell_env_name(name)
                    || reference
                        .strip_prefix("vault:")
                        .is_none_or(|secret| !valid_name(secret))
                {
                    return Err(
                        "API bindings must map environment names to vault:NAME references".into(),
                    );
                }
                if reference
                    .strip_prefix("vault:")
                    .is_some_and(crate::vault_ops::is_lane_slot_secret_name)
                {
                    return Err("Connection bindings require direct Vault secrets or pools; lane slots are not supported".into());
                }
            }
            if let Some(endpoint) = endpoint {
                let parsed =
                    url::Url::parse(endpoint).map_err(|_| "Invalid API endpoint".to_string())?;
                if endpoint.len() > 2048
                    || !matches!(parsed.scheme(), "http" | "https")
                    || parsed.host_str().is_none()
                    || !parsed.username().is_empty()
                    || parsed.password().is_some()
                    || parsed.query().is_some()
                    || parsed.fragment().is_some()
                    || memcore::catalog::endpoint::endpoint_credential_leak(endpoint).is_some()
                {
                    return Err("API endpoint must be an HTTP(S) URL without credential, query or fragment fields".into());
                }
            }
            if model.as_ref().is_some_and(|model| !safe_text(model, 256)) {
                return Err("Invalid model name".into());
            }
        }
        ConnectionTarget::Ssh { alias, remote_cwd } => {
            if !valid_name(alias) {
                return Err(
                    "SSH alias must be an existing host alias without options or whitespace".into(),
                );
            }
            if remote_cwd
                .as_ref()
                .is_some_and(|cwd| !safe_text(cwd, 4096) || !cwd.starts_with('/'))
            {
                return Err("SSH working directory must be an absolute POSIX path".into());
            }
        }
    }
    Ok(())
}

fn validate_catalog(catalog: &ConnectionCatalog) -> Result<(), String> {
    if catalog.version != CATALOG_VERSION {
        return Err("Unsupported connection catalog version".into());
    }
    if catalog.connections.len() > MAX_CONNECTIONS {
        return Err("Connection catalog has too many entries".into());
    }
    let mut names = HashSet::new();
    for connection in &catalog.connections {
        validate_connection(connection)?;
        if !names.insert(&connection.name) {
            return Err("Connection catalog contains duplicate names".into());
        }
    }
    Ok(())
}

/// OpenSSH sends a command string to the remote shell. Quote each argument
/// separately instead of relying on local argv boundaries surviving SSH.
pub(crate) fn ssh_remote_command(
    command: &[String],
    remote_cwd: Option<&str>,
) -> Result<String, String> {
    if command.is_empty()
        || command[0].is_empty()
        || command[0].starts_with('-')
        || command.iter().any(|arg| arg.contains('\0'))
    {
        return Err(
            "SSH command must contain a program without a leading option and no NUL bytes".into(),
        );
    }
    let quote = |value: &str| format!("'{}'", value.replace('\'', "'\\''"));
    let command = command
        .iter()
        .map(|arg| quote(arg))
        .collect::<Vec<_>>()
        .join(" ");
    Ok(match remote_cwd {
        Some(cwd) => format!("cd {} && exec {command}", quote(cwd)),
        None => format!("exec {command}"),
    })
}
