//! Boot configuration for the portable server.
//!
//! Flags win over env; env wins over defaults. Kept to a hand-rolled parser so
//! the crate pulls no CLI framework — the surface is intentionally tiny.
//!
//! | flag | env | default | meaning |
//! | :- | :- | :- | :- |
//! | `--global-db <path>` | `PORTABLE_MEMORY_DB` | in-memory | kernel DB file |
//! | `--decay-policy <name>` | `PORTABLE_DECAY_POLICY` | `default` | #791 scorer hook |
//! | `--daemon` | `PORTABLE_DAEMON=true` | off (stdio) | serve streamable HTTP on loopback instead of stdio (tachi #938) |
//! | `--port <n>` | `PORTABLE_PORT` | `7919` | loopback port for `--daemon` mode; ignored in stdio mode |
//! | `--allow-schema-migration` | `PORTABLE_ALLOW_SCHEMA_MIGRATION=true` | off (refuse) | #1119: authorize migrating an EXISTING older-schema persistent DB forward in place; translated into a typed `memcore::MigrationAuthority`, never a process env var read by the gate itself |

use std::sync::Arc;

use portable_kernel::DecayPolicy;

/// Sentinel db path meaning "open an ephemeral in-memory store".
pub const IN_MEMORY: &str = ":memory:";

/// Default `--daemon` loopback port. Deliberately not the full tachi-server
/// daemon's port (6919, see `tachi-server`'s bootstrap) — the two daemons are
/// meant to run side by side on one workstation without a port collision.
pub const DEFAULT_PORT: u16 = 7919;

pub struct Config {
    /// DB path, or [`IN_MEMORY`] for an ephemeral store.
    pub db_path: String,
    /// Project databases attached to the global kernel store. The runtime keeps
    /// a collection so future store classes do not force a two-slot redesign.
    pub project_db_paths: Vec<String>,
    /// Name of the selected decay policy (for status/reporting).
    pub decay_policy_name: String,
    /// Resolved policy; `None` = kernel default (current behavior).
    pub decay_policy: Option<Arc<dyn DecayPolicy>>,
    /// `true` selects `--daemon` (streamable HTTP on loopback) instead of the
    /// default stdio transport.
    pub daemon: bool,
    /// Loopback port for `--daemon` mode. Ignored in stdio mode.
    pub port: u16,
    /// #1119: explicit opt-in to migrate an EXISTING older-schema persistent
    /// DB forward in place (both `db_path` and every `project_db_paths`
    /// entry). Default `false` (fail-closed): `main.rs` translates this into
    /// a typed `memcore::MigrationAuthority::Allow` / `Deny` at the DB-open
    /// call site — never a process env var read by the gate itself. Has no
    /// effect on in-memory stores (always fresh, never gated).
    pub allow_schema_migration: bool,
}

impl Config {
    /// Build from process args + environment. Returns a human-readable error
    /// string on malformed flags or an unknown decay policy.
    pub fn from_args_and_env() -> Result<Self, String> {
        Self::parse(std::env::args().skip(1), |k| std::env::var(k).ok())
    }

    /// Testable core: parse an arbitrary arg iterator with an env lookup fn.
    pub fn parse<I, F>(args: I, env: F) -> Result<Self, String>
    where
        I: IntoIterator<Item = String>,
        F: Fn(&str) -> Option<String>,
    {
        let mut db_path: Option<String> = None;
        let mut project_db_paths = Vec::new();
        let mut decay_policy_name: Option<String> = None;
        let mut daemon = false;
        let mut port: Option<u16> = None;
        let mut allow_schema_migration = false;

        let mut it = args.into_iter();
        while let Some(arg) = it.next() {
            match arg.as_str() {
                "--global-db" => {
                    db_path = Some(
                        it.next()
                            .ok_or_else(|| "--global-db requires a path".to_string())?,
                    );
                }
                "--project-db" => {
                    project_db_paths.push(
                        it.next()
                            .ok_or_else(|| "--project-db requires a path".to_string())?,
                    );
                }
                "--decay-policy" => {
                    decay_policy_name = Some(
                        it.next()
                            .ok_or_else(|| "--decay-policy requires a name".to_string())?,
                    );
                }
                "--daemon" => {
                    daemon = true;
                }
                "--allow-schema-migration" => {
                    allow_schema_migration = true;
                }
                "--port" => {
                    let raw = it
                        .next()
                        .ok_or_else(|| "--port requires a number".to_string())?;
                    port = Some(parse_port(&raw)?);
                }
                other if other.starts_with("--global-db=") => {
                    db_path = Some(other["--global-db=".len()..].to_string());
                }
                other if other.starts_with("--project-db=") => {
                    project_db_paths.push(other["--project-db=".len()..].to_string());
                }
                other if other.starts_with("--decay-policy=") => {
                    decay_policy_name = Some(other["--decay-policy=".len()..].to_string());
                }
                other if other.starts_with("--port=") => {
                    port = Some(parse_port(&other["--port=".len()..])?);
                }
                other => {
                    return Err(format!(
                        "unknown argument '{other}' (supported: --global-db, --project-db, --decay-policy, --daemon, --port, --allow-schema-migration)"
                    ));
                }
            }
        }

        let db_path = db_path
            .or_else(|| env("PORTABLE_MEMORY_DB"))
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| IN_MEMORY.to_string());

        if project_db_paths.is_empty() {
            if let Some(path) = env("PORTABLE_PROJECT_DB").filter(|path| !path.trim().is_empty()) {
                project_db_paths.push(path);
            }
        }

        let decay_policy_name = decay_policy_name
            .or_else(|| env("PORTABLE_DECAY_POLICY"))
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| "default".to_string());

        let decay_policy = crate::decay::resolve(&decay_policy_name)?;

        let daemon = daemon
            || env("PORTABLE_DAEMON")
                .map(|v| is_truthy(&v))
                .unwrap_or(false);

        let port = match port {
            Some(p) => p,
            None => match env("PORTABLE_PORT") {
                Some(v) if !v.trim().is_empty() => parse_port(&v)?,
                _ => DEFAULT_PORT,
            },
        };

        let allow_schema_migration = allow_schema_migration
            || env("PORTABLE_ALLOW_SCHEMA_MIGRATION")
                .map(|v| is_truthy(&v))
                .unwrap_or(false);

        Ok(Self {
            db_path,
            project_db_paths,
            decay_policy_name,
            decay_policy,
            daemon,
            port,
            allow_schema_migration,
        })
    }
}

/// Parse a `--port` / `PORTABLE_PORT` value into a `u16`, erroring on
/// anything that isn't a plain non-negative integer in `0..=65535`.
fn parse_port(raw: &str) -> Result<u16, String> {
    raw.trim()
        .parse::<u16>()
        .map_err(|_| format!("invalid port '{raw}' (expected a number in 0-65535)"))
}

/// Truthy env-var parsing for `PORTABLE_DAEMON`, matching the boolean-flag
/// convention already used by `TACHI_TRADING_HOURS_GUARD` elsewhere in this
/// workspace: `1`/`true`/`yes`/`on` (case-insensitive) enable, anything else
/// (including unset) does not.
fn is_truthy(v: &str) -> bool {
    matches!(
        v.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_env(_: &str) -> Option<String> {
        None
    }

    #[test]
    fn defaults_to_in_memory_and_default_policy() {
        let c = Config::parse(Vec::<String>::new(), no_env).expect("parse");
        assert_eq!(c.db_path, IN_MEMORY);
        assert!(c.project_db_paths.is_empty());
        assert_eq!(c.decay_policy_name, "default");
        assert!(c.decay_policy.is_none());
        assert!(!c.daemon, "daemon mode must default off (stdio)");
        assert_eq!(c.port, DEFAULT_PORT);
        assert_eq!(
            DEFAULT_PORT, 7919,
            "must not default to the full daemon's 6919"
        );
        assert!(
            !c.allow_schema_migration,
            "schema-migration authority must default off (fail-closed, #1119)"
        );
    }

    /// #1119 discriminating test 1: the flag parses into `Config`.
    #[test]
    fn allow_schema_migration_flag_sets_config_field() {
        let args = vec!["--allow-schema-migration".to_string()];
        let c = Config::parse(args, no_env).expect("parse");
        assert!(c.allow_schema_migration);
    }

    #[test]
    fn allow_schema_migration_env_fallback_is_truthy_parsed() {
        let env = |k: &str| match k {
            "PORTABLE_ALLOW_SCHEMA_MIGRATION" => Some("true".to_string()),
            _ => None,
        };
        let c = Config::parse(Vec::<String>::new(), env).expect("parse");
        assert!(c.allow_schema_migration);

        let env_off = |k: &str| match k {
            "PORTABLE_ALLOW_SCHEMA_MIGRATION" => Some("nah".to_string()),
            _ => None,
        };
        let c_off = Config::parse(Vec::<String>::new(), env_off).expect("parse");
        assert!(
            !c_off.allow_schema_migration,
            "non-truthy PORTABLE_ALLOW_SCHEMA_MIGRATION must not enable migration authority"
        );
    }

    #[test]
    fn flags_win_and_flat_policy_resolves() {
        let args = vec![
            "--global-db".to_string(),
            "/tmp/mem.db".to_string(),
            "--decay-policy".to_string(),
            "flat".to_string(),
        ];
        let env = |k: &str| match k {
            "PORTABLE_MEMORY_DB" | "PORTABLE_DECAY_POLICY" => Some("ignored".to_string()),
            _ => None,
        };
        let c = Config::parse(args, env).expect("parse");
        assert_eq!(c.db_path, "/tmp/mem.db");
        assert!(c.project_db_paths.is_empty());
        assert_eq!(c.decay_policy_name, "flat");
        assert!(c.decay_policy.is_some());
    }

    #[test]
    fn env_fallback_applies_when_no_flags() {
        let env = |k: &str| match k {
            "PORTABLE_MEMORY_DB" => Some("/var/mem.db".to_string()),
            "PORTABLE_DECAY_POLICY" => Some("default".to_string()),
            _ => None,
        };
        let c = Config::parse(Vec::<String>::new(), env).expect("parse");
        assert_eq!(c.db_path, "/var/mem.db");
        assert_eq!(c.decay_policy_name, "default");
    }

    #[test]
    fn unknown_policy_errors() {
        let args = vec!["--decay-policy".to_string(), "bogus".to_string()];
        assert!(Config::parse(args, no_env).is_err());
    }

    #[test]
    fn unknown_flag_errors() {
        let args = vec!["--bogus-flag".to_string()];
        assert!(Config::parse(args, no_env).is_err());
    }

    #[test]
    fn project_db_flag_is_accepted_for_downstream_dual_store_clients() {
        let args = vec![
            "--global-db".to_string(),
            "/tmp/global.db".to_string(),
            "--project-db".to_string(),
            "/tmp/trading.db".to_string(),
        ];

        let config = Config::parse(args, no_env).expect("project db must parse");
        assert_eq!(config.project_db_paths, vec!["/tmp/trading.db"]);
    }

    #[test]
    fn daemon_flag_switches_mode_and_accepts_port_flag() {
        let args = vec![
            "--daemon".to_string(),
            "--port".to_string(),
            "9001".to_string(),
        ];
        let c = Config::parse(args, no_env).expect("parse");
        assert!(c.daemon);
        assert_eq!(c.port, 9001);
    }

    #[test]
    fn port_equals_form_parses() {
        let args = vec!["--port=4242".to_string()];
        let c = Config::parse(args, no_env).expect("parse");
        assert_eq!(c.port, 4242);
        assert!(!c.daemon, "--port alone must not imply --daemon");
    }

    #[test]
    fn daemon_env_fallback_is_truthy_parsed() {
        let env = |k: &str| match k {
            "PORTABLE_DAEMON" => Some("true".to_string()),
            _ => None,
        };
        let c = Config::parse(Vec::<String>::new(), env).expect("parse");
        assert!(c.daemon);

        let env_off = |k: &str| match k {
            "PORTABLE_DAEMON" => Some("nah".to_string()),
            _ => None,
        };
        let c_off = Config::parse(Vec::<String>::new(), env_off).expect("parse");
        assert!(
            !c_off.daemon,
            "non-truthy PORTABLE_DAEMON must not enable daemon mode"
        );
    }

    #[test]
    fn port_env_fallback_applies_when_no_flag() {
        let env = |k: &str| match k {
            "PORTABLE_PORT" => Some("5555".to_string()),
            _ => None,
        };
        let c = Config::parse(Vec::<String>::new(), env).expect("parse");
        assert_eq!(c.port, 5555);
    }

    #[test]
    fn port_flag_wins_over_env() {
        let args = vec!["--port".to_string(), "1234".to_string()];
        let env = |k: &str| match k {
            "PORTABLE_PORT" => Some("9999".to_string()),
            _ => None,
        };
        let c = Config::parse(args, env).expect("parse");
        assert_eq!(c.port, 1234);
    }

    #[test]
    fn bad_port_flag_errors() {
        let args = vec!["--port".to_string(), "not-a-number".to_string()];
        assert!(Config::parse(args, no_env).is_err());

        let args_overflow = vec!["--port".to_string(), "70000".to_string()];
        assert!(
            Config::parse(args_overflow, no_env).is_err(),
            "port above u16::MAX must error"
        );
    }

    #[test]
    fn bad_port_env_errors() {
        let env = |k: &str| match k {
            "PORTABLE_PORT" => Some("nope".to_string()),
            _ => None,
        };
        assert!(Config::parse(Vec::<String>::new(), env).is_err());
    }

    #[test]
    fn port_missing_value_errors() {
        let args = vec!["--port".to_string()];
        assert!(Config::parse(args, no_env).is_err());
    }
}
