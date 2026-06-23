#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::hub_ops::security_scan) enum SecurityScanBackend {
    ClaudeCli,
    RawApi,
    Disabled,
}

impl SecurityScanBackend {
    pub(in crate::hub_ops::security_scan) fn as_str(self) -> &'static str {
        match self {
            SecurityScanBackend::ClaudeCli => "claude_cli",
            SecurityScanBackend::RawApi => "raw_api",
            SecurityScanBackend::Disabled => "disabled",
        }
    }
}

/// Resolve `SKILL_SECURITY_SCAN_BACKEND` env var into a backend choice.
/// Unknown / missing values default to `claude_cli`.
pub(in crate::hub_ops::security_scan) fn resolve_security_scan_backend() -> SecurityScanBackend {
    match std::env::var("SKILL_SECURITY_SCAN_BACKEND")
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "raw_api" | "raw-api" | "rawapi" => SecurityScanBackend::RawApi,
        "disabled" | "off" | "false" | "0" => SecurityScanBackend::Disabled,
        _ => SecurityScanBackend::ClaudeCli,
    }
}

#[cfg(test)]
mod backend_tests {
    use super::{resolve_security_scan_backend, SecurityScanBackend};
    use std::sync::Mutex;

    // Serialize env mutation across tests in this module.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn with_backend_env<F: FnOnce()>(value: Option<&str>, f: F) {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let prev = std::env::var("SKILL_SECURITY_SCAN_BACKEND").ok();
        match value {
            Some(v) => std::env::set_var("SKILL_SECURITY_SCAN_BACKEND", v),
            None => std::env::remove_var("SKILL_SECURITY_SCAN_BACKEND"),
        }
        f();
        match prev {
            Some(v) => std::env::set_var("SKILL_SECURITY_SCAN_BACKEND", v),
            None => std::env::remove_var("SKILL_SECURITY_SCAN_BACKEND"),
        }
    }

    #[test]
    fn backend_defaults_to_claude_cli_when_unset() {
        with_backend_env(None, || {
            assert_eq!(
                resolve_security_scan_backend(),
                SecurityScanBackend::ClaudeCli
            );
        });
    }

    #[test]
    fn backend_defaults_to_claude_cli_when_unknown() {
        with_backend_env(Some("bogus"), || {
            assert_eq!(
                resolve_security_scan_backend(),
                SecurityScanBackend::ClaudeCli
            );
        });
    }

    #[test]
    fn backend_recognises_raw_api() {
        with_backend_env(Some("raw_api"), || {
            assert_eq!(resolve_security_scan_backend(), SecurityScanBackend::RawApi);
        });
        with_backend_env(Some("RAW-API"), || {
            assert_eq!(resolve_security_scan_backend(), SecurityScanBackend::RawApi);
        });
    }

    #[test]
    fn backend_recognises_disabled() {
        with_backend_env(Some("disabled"), || {
            assert_eq!(
                resolve_security_scan_backend(),
                SecurityScanBackend::Disabled
            );
        });
        with_backend_env(Some("0"), || {
            assert_eq!(
                resolve_security_scan_backend(),
                SecurityScanBackend::Disabled
            );
        });
    }

    #[test]
    fn backend_recognises_explicit_claude_cli() {
        with_backend_env(Some("claude_cli"), || {
            assert_eq!(
                resolve_security_scan_backend(),
                SecurityScanBackend::ClaudeCli
            );
        });
    }

    #[test]
    fn backend_as_str_matches_env_values() {
        assert_eq!(SecurityScanBackend::ClaudeCli.as_str(), "claude_cli");
        assert_eq!(SecurityScanBackend::RawApi.as_str(), "raw_api");
        assert_eq!(SecurityScanBackend::Disabled.as_str(), "disabled");
    }
}
