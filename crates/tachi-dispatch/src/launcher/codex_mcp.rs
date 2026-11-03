//! Codex per-run MCP projection, preserving the carrier's configuration layers.

use super::LaunchCommand;
use serde_json::{json, Value};
use std::collections::{BTreeSet, HashMap};
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const INVENTORY_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_INVENTORY_BYTES: u64 = 1024 * 1024;

pub(super) struct CodexMcpProjection {
    overrides: Vec<(String, Value)>,
    env_vars: HashMap<String, String>,
}

impl CodexMcpProjection {
    fn rendered_overrides(&self) -> Vec<String> {
        self.overrides
            .iter()
            .map(|(path, value)| format!("{path}={}", mcp_toml_value(value)))
            .collect()
    }

    pub(super) fn apply(self, command: &mut LaunchCommand) {
        for value in self.rendered_overrides() {
            command.arg("-c");
            command.arg(value);
        }
        command.env_vars.extend(self.env_vars);
    }
}

pub(super) fn projection_for_cli(
    path: &Path,
    cwd: Option<&str>,
) -> Result<CodexMcpProjection, String> {
    let raw = std::fs::read(path).map_err(|error| format!("read MCP config: {error}"))?;
    let config: Value =
        serde_json::from_slice(&raw).map_err(|error| format!("parse MCP config: {error}"))?;
    let namespace = uuid::Uuid::new_v4().simple().to_string();
    // Validate the generated descriptor before inspecting the installed carrier.
    project(&config, &BTreeSet::new(), &namespace, |name| {
        std::env::var(name).ok()
    })?;
    let existing = inspect_codex_mcp_names(cwd)?;
    project(&config, &existing, &namespace, |name| {
        std::env::var(name).ok()
    })
}

/// `mcp list` loads Codex's own effective configuration without starting an
/// agent or an MCP server. Its output can contain credentials: retain only names
/// and never include stdout/stderr in an error or launch receipt.
fn inspect_codex_mcp_names(cwd: Option<&str>) -> Result<BTreeSet<String>, String> {
    let mut command = Command::new("codex");
    command
        .args(["mcp", "list", "--json"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    if let Some(cwd) = cwd {
        command.current_dir(cwd);
    }
    let mut child = command.spawn().map_err(|error| {
        format!("Cannot inspect Codex MCP configuration before injection: {error}")
    })?;
    let stdout = child.stdout.take().expect("piped Codex inventory stdout");
    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let mut raw = Vec::new();
        let result = stdout
            .take(MAX_INVENTORY_BYTES + 1)
            .read_to_end(&mut raw)
            .map(|_| raw);
        let _ = sender.send(result);
    });
    let deadline = Instant::now() + INVENTORY_TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(20));
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("Codex MCP configuration inspection timed out after 15s; injection was refused. Run `codex mcp list --json` in the admitted workspace before retrying.".into());
            }
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!(
                    "Cannot wait for Codex MCP configuration inspection: {error}"
                ));
            }
        }
    };
    if !status.success() {
        return Err(format!("Codex MCP configuration inspection failed ({status}); injection was refused. Run `codex mcp list --json` in the admitted workspace and repair its configuration before retrying."));
    }
    let raw = receiver
        .recv_timeout(Duration::from_secs(1))
        .map_err(|_| "Codex MCP inventory did not close stdout; injection was refused")?
        .map_err(|_| "Cannot read Codex MCP inventory; injection was refused")?;
    if raw.len() as u64 > MAX_INVENTORY_BYTES {
        return Err("Codex MCP inventory exceeds the 1 MiB limit; injection was refused".into());
    }
    parse_inventory(&raw)
}

fn parse_inventory(raw: &[u8]) -> Result<BTreeSet<String>, String> {
    let value: Value = serde_json::from_slice(raw)
        .map_err(|_| "Codex MCP inventory is not JSON; injection was refused")?;
    let entries = value
        .as_array()
        .ok_or("Codex MCP inventory must be an array; update the CLI before retrying injection")?;
    entries
        .iter()
        .map(|entry| {
            entry
                .get("name")
                .and_then(Value::as_str)
                .filter(|name| !name.is_empty())
                .map(str::to_string)
                .ok_or_else(|| {
                    "Codex MCP inventory has no canonical server name; injection was refused"
                        .to_string()
                })
        })
        .collect()
}

fn project(
    config: &Value,
    existing: &BTreeSet<String>,
    namespace: &str,
    environment: impl Fn(&str) -> Option<String>,
) -> Result<CodexMcpProjection, String> {
    let servers = config
        .get("mcpServers")
        .and_then(Value::as_object)
        .ok_or("MCP config requires an mcpServers object")?;
    let mut overrides = Vec::new();
    let mut env_vars = HashMap::new();
    for (index, (name, entry)) in servers.iter().enumerate() {
        if name.is_empty()
            || !name
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-'))
        {
            return Err(format!(
                "MCP server name '{name}' cannot be represented as a Codex override"
            ));
        }
        if existing.contains(name) {
            return Err(format!("Codex already configures MCP server '{name}'. Its complete per-server tool policy is unavailable from `mcp list --json`, so injection was refused; Tachi will not disable that server or bypass its enabled_tools, disabled_tools or tools policy under a new name. Use an explicitly configured Staff profile with MCP injection disabled to keep the existing Codex configuration. Replacing it requires an explicit configuration and policy migration."));
        }
        let entry = entry
            .as_object()
            .ok_or("MCP server entry must be an object")?;
        let mut mapped = serde_json::Map::new();
        mapped.insert("enabled".into(), json!(true));
        if let Some(url) = entry.get("url") {
            if entry.get("type").and_then(Value::as_str) != Some("http")
                || !url.as_str().is_some_and(|url| !url.is_empty())
                || ["command", "args", "env"]
                    .iter()
                    .any(|key| entry.contains_key(*key))
            {
                return Err(format!(
                    "MCP server '{name}' requires an unambiguous HTTP descriptor"
                ));
            }
            mapped.insert("url".into(), url.clone());
            if let Some(headers) = entry.get("headers") {
                let headers = string_map(headers, "HTTP headers")?;
                let mut references = serde_json::Map::new();
                for (header_index, (header, value)) in headers.iter().enumerate() {
                    let key = format!(
                        "TACHI_CODEX_MCP_{}_{}_{}",
                        namespace.to_ascii_uppercase(),
                        index,
                        header_index
                    );
                    references.insert(header.clone(), json!(key));
                    env_vars.insert(key, value.as_str().expect("checked header string").into());
                }
                mapped.insert("env_http_headers".into(), Value::Object(references));
            }
        } else {
            if entry.get("type").is_some_and(|kind| kind != "stdio") {
                return Err(format!("MCP server '{name}' has no supported transport"));
            }
            let command = entry
                .get("command")
                .filter(|value| value.as_str().is_some_and(|value| !value.is_empty()))
                .ok_or_else(|| format!("MCP server '{name}' has no supported transport"))?;
            mapped.insert("command".into(), command.clone());
            if let Some(args) = entry.get("args") {
                if !args
                    .as_array()
                    .is_some_and(|args| args.iter().all(Value::is_string))
                {
                    return Err("MCP command args must be strings".into());
                }
                mapped.insert("args".into(), args.clone());
            }
            if let Some(env) = entry.get("env") {
                let env = string_map(env, "MCP environment")?;
                let mut names = Vec::new();
                for (key, value) in env {
                    if key.is_empty() || key.bytes().any(|byte| matches!(byte, b'=' | 0)) {
                        return Err("MCP environment names must be valid environment keys".into());
                    }
                    if environment(key).as_deref() != value.as_str() {
                        return Err(format!("Codex cannot safely project a literal stdio environment value for '{key}' without changing carrier authentication or exposing it in argv. Configure that MCP server in Codex, or provide the identical value in the existing host environment before requesting injection."));
                    }
                    names.push(key.clone());
                }
                mapped.insert("env_vars".into(), json!(names));
            }
        }
        // Codex recursively merges tables. A collision was refused above;
        // the admitted new transport gets its own per-run table.
        let injected_name = format!("tachi_dispatch_{namespace}_{index}");
        if existing.contains(&injected_name) {
            return Err(
                "Codex per-run MCP name already exists; retry injection with a fresh run".into(),
            );
        }
        overrides.push((
            format!("mcp_servers.{injected_name}"),
            Value::Object(mapped),
        ));
    }
    Ok(CodexMcpProjection {
        overrides,
        env_vars,
    })
}

fn string_map<'a>(
    value: &'a Value,
    label: &str,
) -> Result<&'a serde_json::Map<String, Value>, String> {
    value
        .as_object()
        .filter(|map| map.values().all(Value::is_string))
        .ok_or_else(|| format!("{label} must be a string map"))
}

/// The supported subset has TOML-compatible JSON basic-string escaping.
fn mcp_toml_value(value: &Value) -> String {
    match value {
        Value::Object(map) => {
            let mut entries: Vec<_> = map.iter().collect();
            entries.sort_by(|(left, _), (right, _)| left.cmp(right));
            format!(
                "{{ {} }}",
                entries
                    .into_iter()
                    .map(|(key, value)| {
                        format!(
                            "{} = {}",
                            serde_json::to_string(key).expect("string"),
                            mcp_toml_value(value)
                        )
                    })
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        }
        Value::Array(array) => format!(
            "[{}]",
            array
                .iter()
                .map(mcp_toml_value)
                .collect::<Vec<_>>()
                .join(", ")
        ),
        _ => value.to_string(),
    }
}

#[cfg(test)]
mod tests;
