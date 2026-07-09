//! Boot configuration for the portable server.
//!
//! Flags win over env; env wins over defaults. Kept to a hand-rolled parser so
//! the crate pulls no CLI framework — the surface is intentionally tiny.
//!
//! | flag | env | default | meaning |
//! | :- | :- | :- | :- |
//! | `--global-db <path>` | `PORTABLE_MEMORY_DB` | in-memory | kernel DB file |
//! | `--decay-policy <name>` | `PORTABLE_DECAY_POLICY` | `default` | #791 scorer hook |

use std::sync::Arc;

use portable_kernel::DecayPolicy;

/// Sentinel db path meaning "open an ephemeral in-memory store".
pub const IN_MEMORY: &str = ":memory:";

pub struct Config {
    /// DB path, or [`IN_MEMORY`] for an ephemeral store.
    pub db_path: String,
    /// Name of the selected decay policy (for status/reporting).
    pub decay_policy_name: String,
    /// Resolved policy; `None` = kernel default (current behavior).
    pub decay_policy: Option<Arc<dyn DecayPolicy>>,
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
        let mut decay_policy_name: Option<String> = None;

        let mut it = args.into_iter();
        while let Some(arg) = it.next() {
            match arg.as_str() {
                "--global-db" => {
                    db_path = Some(
                        it.next()
                            .ok_or_else(|| "--global-db requires a path".to_string())?,
                    );
                }
                "--decay-policy" => {
                    decay_policy_name = Some(
                        it.next()
                            .ok_or_else(|| "--decay-policy requires a name".to_string())?,
                    );
                }
                other if other.starts_with("--global-db=") => {
                    db_path = Some(other["--global-db=".len()..].to_string());
                }
                other if other.starts_with("--decay-policy=") => {
                    decay_policy_name = Some(other["--decay-policy=".len()..].to_string());
                }
                other => {
                    return Err(format!(
                        "unknown argument '{other}' (supported: --global-db, --decay-policy)"
                    ));
                }
            }
        }

        let db_path = db_path
            .or_else(|| env("PORTABLE_MEMORY_DB"))
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| IN_MEMORY.to_string());

        let decay_policy_name = decay_policy_name
            .or_else(|| env("PORTABLE_DECAY_POLICY"))
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| "default".to_string());

        let decay_policy = crate::decay::resolve(&decay_policy_name)?;

        Ok(Self {
            db_path,
            decay_policy_name,
            decay_policy,
        })
    }
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
        assert_eq!(c.decay_policy_name, "default");
        assert!(c.decay_policy.is_none());
    }

    #[test]
    fn flags_win_and_flat_policy_resolves() {
        let args = vec![
            "--global-db".to_string(),
            "/tmp/mem.db".to_string(),
            "--decay-policy".to_string(),
            "flat".to_string(),
        ];
        let c = Config::parse(args, |_| Some("ignored".to_string())).expect("parse");
        assert_eq!(c.db_path, "/tmp/mem.db");
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
        let args = vec!["--daemon".to_string()];
        assert!(Config::parse(args, no_env).is_err());
    }
}
