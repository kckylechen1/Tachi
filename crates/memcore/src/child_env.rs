//! What a program launched by this process may inherit from its environment.
//!
//! The daemon's process marker describes the daemon, never the programs it
//! starts. A build additionally runs the ticket's own source code, not a
//! runtime the operator chose for its credentials, so it inherits none of
//! this process's credentials.

use std::ffi::OsString;
use std::process::Command;

/// Marks the long-lived daemon so it executes writes itself; a child that
/// inherits it stops forwarding to the daemon.
const DAEMON_MARKER: &str = "TACHI_DAEMON";

/// Name endings of variables that carry a credential.
const CREDENTIAL_SUFFIXES: &[&str] = &[
    "_API_KEY",
    "_APIKEY",
    "_ACCESS_KEY",
    "_SECRET_KEY",
    "_PRIVATE_KEY",
    "_SESSION_KEY",
    "_TOKEN",
    "_SECRET",
    "_SECRETS",
    "_PASSWORD",
    "_PASSWD",
    "_CREDENTIAL",
    "_CREDENTIALS",
];

/// Credential variables whose names carry none of the suffixes above. Named
/// exactly: a `_PWD` suffix would also take the working directory.
const CREDENTIAL_NAMES: &[&str] = &[
    "PGPASSWORD",
    "MYSQL_PWD",
    "PGPASSFILE",
    "REDISCLI_AUTH",
    "DOCKER_AUTH_CONFIG",
    "AWS_SHARED_CREDENTIALS_FILE",
    "AWS_WEB_IDENTITY_TOKEN_FILE",
    "KRB5CCNAME",
];

/// Toolchain configuration is the build tool's own setting: a private registry
/// token under `CARGO_` stays, because withholding it breaks the build.
const TOOLCHAIN_PREFIXES: &[&str] = &["CARGO_", "RUSTUP_"];

pub fn is_credential_env_name(name: &str) -> bool {
    let name = name.to_ascii_uppercase();
    !TOOLCHAIN_PREFIXES
        .iter()
        .any(|prefix| name.starts_with(prefix))
        && (CREDENTIAL_NAMES.contains(&name.as_str())
            || CREDENTIAL_SUFFIXES
                .iter()
                .any(|suffix| name.ends_with(suffix) || name == suffix.trim_start_matches('_')))
}

/// A child that inherits this process's environment still never inherits the
/// daemon marker.
pub fn withhold_process_marker(command: &mut Command) {
    if !command.get_envs().any(|(name, _)| name == DAEMON_MARKER) {
        command.env_remove(DAEMON_MARKER);
    }
}

/// Withhold from a build the ambient credentials it would otherwise inherit.
/// Returns how many were removed.
pub fn withhold_inherited_credentials(command: &mut Command) -> usize {
    withhold_inherited_credentials_from(std::env::vars_os(), command)
}

/// [`withhold_inherited_credentials`] over a supplied environment. A name
/// `command` already sets or removes explicitly is left to that decision.
pub fn withhold_inherited_credentials_from(
    ambient: impl IntoIterator<Item = (OsString, OsString)>,
    command: &mut Command,
) -> usize {
    withhold_process_marker(command);
    let explicit: Vec<OsString> = command
        .get_envs()
        .map(|(name, _)| name.to_os_string())
        .collect();
    let mut withheld = 0;
    for (name, _) in ambient {
        let Some(text) = name.to_str() else {
            continue;
        };
        if explicit.contains(&name) || !is_credential_env_name(text) {
            continue;
        }
        command.env_remove(&name);
        withheld += 1;
    }
    withheld
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;

    fn withheld(names: &[&str], command: &mut Command) -> Vec<String> {
        withhold_inherited_credentials_from(
            names
                .iter()
                .map(|name| (OsString::from(name), OsString::from("value"))),
            command,
        );
        let mut removed: Vec<String> = command
            .get_envs()
            .filter(|(_, value)| value.is_none())
            .map(|(name, _)| name.to_string_lossy().to_string())
            .collect();
        removed.sort();
        removed
    }

    // Everything a build needs to find its tools and reach a private registry
    // stays inherited.
    #[test]
    fn builds_keep_toolchain_and_session_variables() {
        let mut command = Command::new("cargo");
        assert_eq!(
            withheld(
                &[
                    "PATH",
                    "HOME",
                    "LANG",
                    "TMPDIR",
                    "HTTPS_PROXY",
                    "SSH_AUTH_SOCK",
                    "CARGO_HOME",
                    "CARGO_BUILD_JOBS",
                    "CARGO_REGISTRIES_PRIVATE_TOKEN",
                    "RUSTUP_TOOLCHAIN",
                    "AWS_ACCESS_KEY_ID",
                    "OPENAI_BASE_URL",
                    "TACHI_HOME",
                ],
                &mut command
            ),
            vec!["TACHI_DAEMON".to_string()]
        );
    }

    // A credential whose name carries none of the suffixes.
    #[test]
    fn builds_do_not_inherit_credentials_named_without_a_suffix() {
        let mut command = Command::new("cargo");
        let mut expected: Vec<String> = [
            "PGPASSWORD",
            "MYSQL_PWD",
            "PGPASSFILE",
            "REDISCLI_AUTH",
            "DOCKER_AUTH_CONFIG",
            "AWS_SHARED_CREDENTIALS_FILE",
            "AWS_WEB_IDENTITY_TOKEN_FILE",
            "KRB5CCNAME",
            "TACHI_DAEMON",
        ]
        .iter()
        .map(|name| name.to_string())
        .collect();
        expected.sort();
        assert_eq!(
            withheld(
                &[
                    "PGPASSWORD",
                    "MYSQL_PWD",
                    "PGPASSFILE",
                    "REDISCLI_AUTH",
                    "DOCKER_AUTH_CONFIG",
                    "AWS_SHARED_CREDENTIALS_FILE",
                    "AWS_WEB_IDENTITY_TOKEN_FILE",
                    "KRB5CCNAME",
                    // The working directory is not a credential.
                    "PWD",
                ],
                &mut command
            ),
            expected
        );
    }

    #[test]
    fn builds_do_not_inherit_ambient_credentials() {
        let mut command = Command::new("cargo");
        assert_eq!(
            withheld(
                &[
                    "GITHUB_TOKEN",
                    "OPENAI_API_KEY",
                    "ANTHROPIC_AUTH_TOKEN",
                    "AWS_SECRET_ACCESS_KEY",
                    "AZURE_CLIENT_SECRET",
                    "GOOGLE_APPLICATION_CREDENTIALS",
                    "HF_TOKEN",
                    "VAULT_PASSWORD",
                    "worker_api_key",
                ],
                &mut command
            ),
            [
                "ANTHROPIC_AUTH_TOKEN",
                "AWS_SECRET_ACCESS_KEY",
                "AZURE_CLIENT_SECRET",
                "GITHUB_TOKEN",
                "GOOGLE_APPLICATION_CREDENTIALS",
                "HF_TOKEN",
                "OPENAI_API_KEY",
                "TACHI_DAEMON",
                "VAULT_PASSWORD",
                "worker_api_key",
            ]
            .map(String::from)
        );
    }

    #[test]
    fn a_value_set_for_the_child_is_never_withheld() {
        let mut command = Command::new("cargo");
        command.env("OPENAI_API_KEY", "registry-copy");
        assert_eq!(
            withhold_inherited_credentials_from(
                [("OPENAI_API_KEY".into(), "ambient-copy".into())],
                &mut command
            ),
            0
        );
        let explicit: Vec<_> = command.get_envs().collect();
        assert_eq!(
            explicit,
            [
                (
                    OsStr::new("OPENAI_API_KEY"),
                    Some(OsStr::new("registry-copy"))
                ),
                (OsStr::new("TACHI_DAEMON"), None),
            ]
        );
    }

    // A dispatch worker inherits the daemon's provider keys by contract; only
    // the marker is withheld from it.
    #[test]
    fn a_worker_keeps_the_daemons_credentials_but_never_its_marker() {
        let mut command = Command::new("codex");
        command.env("OPENAI_API_KEY", "inherited-by-the-launcher");
        withhold_process_marker(&mut command);
        let explicit: Vec<_> = command.get_envs().collect();
        assert_eq!(
            explicit,
            [
                (
                    OsStr::new("OPENAI_API_KEY"),
                    Some(OsStr::new("inherited-by-the-launcher"))
                ),
                (OsStr::new("TACHI_DAEMON"), None),
            ]
        );
    }
}
