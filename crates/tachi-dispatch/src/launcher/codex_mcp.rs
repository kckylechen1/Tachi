//! Codex per-run MCP projection, preserving the carrier's configuration layers.

use serde_json::{json, Value};
use std::collections::{BTreeSet, HashMap};
#[cfg(unix)]
use std::io::Read;
use std::path::Path;
use std::process::Command;
#[cfg(unix)]
use std::process::Stdio;
use std::time::Duration;
#[cfg(unix)]
use std::time::Instant;

const INVENTORY_TIMEOUT: Duration = Duration::from_secs(15);
#[cfg(unix)]
const MAX_INVENTORY_BYTES: u64 = 1024 * 1024;

struct CodexMcpProjection {
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

    fn apply(self, command: &mut Command) {
        for value in self.rendered_overrides() {
            command.arg("-c");
            command.arg(value);
        }
        command.envs(self.env_vars);
    }
}

/// Apply a per-run MCP projection after the runtime has admitted its context.
///
/// The caller supplies a complete `mcp list --json` inventory command using the
/// worker's admitted executable and effective configuration directory. Both
/// commands must retain ambient environment inheritance, with matching explicit
/// values and removals; commands using `env_clear` are outside this contract.
/// Directory-descriptor anchoring belongs to that runtime owner, not this layer.
pub fn apply_codex_mcp_config(
    worker: &mut Command,
    inventory: &mut Command,
    config_path: &Path,
) -> Result<(), String> {
    let raw = std::fs::read(config_path).map_err(|error| format!("read MCP config: {error}"))?;
    let config: Value =
        serde_json::from_slice(&raw).map_err(|error| format!("parse MCP config: {error}"))?;
    let namespace = uuid::Uuid::new_v4().simple().to_string();
    // Validate the generated descriptor before inspecting the installed carrier.
    project(&config, &BTreeSet::new(), &namespace, |name| {
        worker_environment(worker, name)
    })?;
    let existing = inspect_inventory_command(inventory, INVENTORY_TIMEOUT, Duration::from_secs(1))?;
    let projection = project(&config, &existing, &namespace, |name| {
        worker_environment(worker, name)
    })?;
    // No worker argv or environment changes are published on any refusal.
    projection.apply(worker);
    Ok(())
}

fn worker_environment(worker: &Command, name: &str) -> Option<String> {
    match worker.get_envs().find(|(key, _)| *key == name) {
        Some((_, value)) => value.and_then(|value| value.to_str().map(str::to_owned)),
        None => std::env::var(name).ok(),
    }
}

#[cfg(not(unix))]
fn inspect_inventory_command(
    _command: &mut Command,
    _timeout: Duration,
    _stdout_timeout: Duration,
) -> Result<BTreeSet<String>, String> {
    Err("Codex MCP injection requires a Unix owned process group for configuration inspection. This platform has no supported probe ownership boundary; use an explicitly configured Staff profile with MCP injection disabled to keep the existing Codex configuration.".into())
}

/// `mcp list` output can contain credentials: retain only names and never
/// include stdout/stderr in an error or launch receipt.
#[cfg(unix)]
fn inspect_inventory_command(
    command: &mut Command,
    timeout: Duration,
    stdout_timeout: Duration,
) -> Result<BTreeSet<String>, String> {
    use std::os::fd::AsRawFd;
    use std::os::unix::process::CommandExt;

    command
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let child = command.spawn().map_err(|error| {
        format!("Cannot inspect Codex MCP configuration before injection: {error}")
    })?;
    let mut process = InventoryProcess {
        child,
        owns_root: true,
    };
    let mut stdout = process
        .child
        .stdout
        .take()
        .expect("piped Codex inventory stdout");
    let fd = stdout.as_raw_fd();
    // Nonblocking reads keep pipe ownership on this stack. An inherited pipe
    // held by a wrapper descendant cannot strand a detached reader thread.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(
            "Cannot make Codex MCP inventory output nonblocking; injection was refused".into(),
        );
    }
    let deadline = Instant::now() + timeout;
    let mut stdout_deadline = None;
    let mut stdout_closed = false;
    let mut raw = Vec::new();
    let mut buffer = [0; 8192];
    loop {
        while !stdout_closed {
            match stdout.read(&mut buffer) {
                Ok(0) => stdout_closed = true,
                Ok(count) => {
                    raw.extend_from_slice(&buffer[..count]);
                    if raw.len() as u64 > MAX_INVENTORY_BYTES {
                        return Err(
                            "Codex MCP inventory exceeds the 1 MiB limit; injection was refused"
                                .into(),
                        );
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => {
                    return Err("Cannot read Codex MCP inventory; injection was refused".into())
                }
            }
        }
        let exited = process.observe_exit().map_err(|error| {
            format!("Cannot wait for Codex MCP configuration inspection: {error}")
        })?;
        if exited && stdout_closed {
            let status = process.terminate_and_reap().map_err(|error| {
                format!("Cannot finish owned Codex MCP configuration inspection: {error}")
            })?;
            if !status.success() {
                return Err(format!("Codex MCP configuration inspection failed ({status}); injection was refused. Run `codex mcp list --json` in the admitted workspace and repair its configuration before retrying."));
            }
            return parse_inventory(&raw);
        }
        let now = Instant::now();
        if exited && now >= *stdout_deadline.get_or_insert(now + stdout_timeout) {
            return Err("Codex MCP inventory did not close stdout; injection was refused".into());
        }
        if now >= deadline {
            return Err(format!("Codex MCP configuration inspection timed out after {timeout:?}; injection was refused. Run `codex mcp list --json` in the admitted workspace before retrying."));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// As in the managed subprocess runner, keep the group leader unreaped until
/// every group signal finishes. This covers ordinary wrapper descendants, not
/// processes that deliberately escape with setsid/setpgid.
#[cfg(unix)]
struct InventoryProcess {
    child: std::process::Child,
    owns_root: bool,
}

#[cfg(unix)]
impl InventoryProcess {
    fn observe_exit(&mut self) -> std::io::Result<bool> {
        if !self.owns_root {
            return Err(std::io::Error::other(
                "Codex inventory child ownership was lost",
            ));
        }
        // WNOWAIT reserves the child's PID/PGID even after its normal exit.
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        let result = unsafe {
            libc::waitid(
                libc::P_PID,
                self.child.id() as libc::id_t,
                &mut info,
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        };
        if result != 0 {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::ECHILD) {
                self.owns_root = false;
            }
            return Err(error);
        }
        Ok(unsafe { info.si_pid() } != 0)
    }

    fn terminate_and_reap(&mut self) -> std::io::Result<std::process::ExitStatus> {
        if !self.owns_root {
            return Err(std::io::Error::other(
                "Codex inventory child ownership was lost",
            ));
        }
        let _exit_observed = match self.observe_exit() {
            Ok(exited) => exited,
            Err(error) if !self.owns_root => return Err(error),
            Err(_) => false,
        };
        // SAFETY: process_group(0) created this group, and WNOWAIT has not
        // consumed its leader. No termination signal follows an attempt to reap it.
        let result = unsafe { libc::kill(-(self.child.id() as libc::pid_t), libc::SIGKILL) };
        let signal_error = (result != 0)
            .then(std::io::Error::last_os_error)
            .filter(|error| error.raw_os_error() != Some(libc::ESRCH));
        if signal_error.is_some() {
            let _ = self.child.kill();
        }
        self.owns_root = false;
        let status = self.child.wait();
        #[cfg(target_os = "macos")]
        let signal_error = signal_error.filter(|error| {
            // Darwin skips zombie group members and can return EPERM for a
            // sole unreaped leader. Only definite absence after a successful
            // reap resolves that error; live or unobservable groups still fail.
            !(_exit_observed
                && status.is_ok()
                && error.raw_os_error() == Some(libc::EPERM)
                && inventory_group_is_absent(self.child.id() as libc::pid_t))
        });
        match signal_error {
            Some(error) => Err(error),
            None => status,
        }
    }
}

#[cfg(all(unix, any(target_os = "macos", test)))]
fn inventory_group_is_absent(pgid: libc::pid_t) -> bool {
    // SAFETY: signal 0 is only an existence/permission query. A recycled PGID
    // can make this proof fail conservatively, but receives no termination signal.
    let result = unsafe { libc::kill(-pgid, 0) };
    result != 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
}

#[cfg(unix)]
impl Drop for InventoryProcess {
    fn drop(&mut self) {
        if self.owns_root {
            let _ = self.terminate_and_reap();
        }
    }
}

#[cfg(any(unix, test))]
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

fn mcp_toml_string(value: &str) -> String {
    serde_json::to_string(value)
        .expect("string")
        .replace('\u{7f}', "\\u007f")
}

/// JSON basic-string escaping also needs an explicit DEL escape for TOML.
fn mcp_toml_value(value: &Value) -> String {
    match value {
        Value::String(value) => mcp_toml_string(value),
        Value::Object(map) => {
            let mut entries: Vec<_> = map.iter().collect();
            entries.sort_by_key(|(key, _)| *key);
            format!(
                "{{ {} }}",
                entries
                    .into_iter()
                    .map(|(key, value)| {
                        format!("{} = {}", mcp_toml_string(key), mcp_toml_value(value))
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
