//! Machine-local execution boundary for dispatches (#1010).
//!
//! A host profile says which class of side effect may run on this machine; it
//! does not discover hardware, infer data ownership, or replace action-level
//! confirmation gates.

use serde_json::json;
use tachi_params::ExecutionLevel;

const HOST_PROFILE_ENV: &str = "TACHI_HOST_PROFILE";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HostProfile {
    Development,
    HomeData,
    Release,
}

impl HostProfile {
    pub(crate) fn parse(raw: &str) -> Result<Self, String> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "development" | "dev" => Ok(Self::Development),
            "home_data" | "home-data" | "home" => Ok(Self::HomeData),
            "release" => Ok(Self::Release),
            value => Err(format!(
                "invalid {HOST_PROFILE_ENV}={value:?}; expected development, home_data, or release"
            )),
        }
    }

    pub(crate) fn current() -> Result<Self, String> {
        match std::env::var(HOST_PROFILE_ENV) {
            Ok(value) if !value.trim().is_empty() => Self::parse(&value),
            Ok(_) | Err(std::env::VarError::NotPresent) => Ok(Self::Development),
            Err(err) => Err(format!("read {HOST_PROFILE_ENV}: {err}")),
        }
    }

    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Development => "development",
            Self::HomeData => "home_data",
            Self::Release => "release",
        }
    }

    pub(crate) const fn max_execution_level(self) -> ExecutionLevel {
        match self {
            Self::Development | Self::Release => ExecutionLevel::L1,
            Self::HomeData => ExecutionLevel::L3,
        }
    }
}

pub(crate) fn resolved_execution_level(requested: Option<ExecutionLevel>) -> ExecutionLevel {
    requested.unwrap_or(ExecutionLevel::L1)
}

pub(crate) fn permits(profile: HostProfile, level: ExecutionLevel) -> bool {
    level.rank() <= profile.max_execution_level().rank()
}

pub(crate) fn authorize_dispatch(
    requested: Option<ExecutionLevel>,
) -> Result<(HostProfile, ExecutionLevel), String> {
    let profile = HostProfile::current()?;
    let level = resolved_execution_level(requested);
    let max = profile.max_execution_level();
    if !permits(profile, level) {
        return Err(format!(
            "host_profile_mismatch: host_profile={} permits through {}, but dispatch requires {}; set TACHI_HOST_PROFILE=home_data only on the approved data host",
            profile.name(),
            max.as_str(),
            level.as_str(),
        ));
    }
    Ok((profile, level))
}

pub(crate) fn runtime_json() -> serde_json::Value {
    match HostProfile::current() {
        Ok(profile) => json!({
            "profile": profile.name(),
            "max_execution_level": profile.max_execution_level().as_str(),
            "source": HOST_PROFILE_ENV,
        }),
        Err(error) => json!({
            "profile": "invalid",
            "source": HOST_PROFILE_ENV,
            "configuration_error": error,
        }),
    }
}

fn config_path(app_home: &std::path::Path) -> std::path::PathBuf {
    app_home.join("config.env")
}

fn persist_profile(profile: HostProfile, app_home: &std::path::Path) -> Result<(), String> {
    std::fs::create_dir_all(app_home)
        .map_err(|error| format!("create TACHI_HOME {}: {error}", app_home.display()))?;
    let path = config_path(app_home);
    let existing = match std::fs::read_to_string(&path) {
        Ok(body) => body,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(format!("read {}: {error}", path.display())),
    };
    let mut lines = existing
        .lines()
        .filter(|line| {
            !line
                .trim_start()
                .starts_with(&format!("{HOST_PROFILE_ENV}="))
        })
        .map(str::to_string)
        .collect::<Vec<_>>();
    lines.push(format!("{HOST_PROFILE_ENV}={}", profile.name()));
    let body = format!("{}\n", lines.join("\n"));
    crate::utils::write_owner_only_file_atomic(&path, body.as_bytes())
        .map_err(|error| format!("write {}: {error}", path.display()))
}

pub(crate) fn run_cli(
    action: tachi_bootstrap::cli::HostAction,
    app_home: &std::path::Path,
) -> Result<(), Box<dyn std::error::Error>> {
    match action {
        tachi_bootstrap::cli::HostAction::Show { json: json_out } => {
            let mut status = runtime_json();
            if let Some(object) = status.as_object_mut() {
                object.insert(
                    "config_path".to_string(),
                    json!(config_path(app_home).display().to_string()),
                );
            }
            if json_out {
                println!("{}", serde_json::to_string_pretty(&status)?);
            } else if let Some(error) = status
                .get("configuration_error")
                .and_then(serde_json::Value::as_str)
            {
                println!("host profile: invalid ({error})");
            } else {
                println!(
                    "host profile: {} (max execution level {})",
                    status["profile"].as_str().unwrap_or("unknown"),
                    status["max_execution_level"].as_str().unwrap_or("unknown"),
                );
                println!("config: {}", config_path(app_home).display());
            }
        }
        tachi_bootstrap::cli::HostAction::Set {
            profile: raw,
            json: json_out,
        } => {
            let profile = HostProfile::parse(&raw)
                .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidInput, error))?;
            persist_profile(profile, app_home)
                .map_err(|error| std::io::Error::new(std::io::ErrorKind::Other, error))?;
            let result = json!({
                "profile": profile.name(),
                "max_execution_level": profile.max_execution_level().as_str(),
                "config_path": config_path(app_home),
                "effective_after": "next Tachi process; restart a resident daemon during an approved maintenance window",
            });
            if json_out {
                println!("{}", serde_json::to_string_pretty(&result)?);
            } else {
                println!(
                    "host profile saved: {} (max {}). Restart a resident daemon during an approved maintenance window before dispatching with it.",
                    profile.name(),
                    profile.max_execution_level().as_str(),
                );
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parser_accepts_declared_profiles_only() {
        assert_eq!(
            HostProfile::parse("development"),
            Ok(HostProfile::Development)
        );
        assert_eq!(HostProfile::parse("home_data"), Ok(HostProfile::HomeData));
        assert_eq!(HostProfile::parse("release"), Ok(HostProfile::Release));
        assert!(HostProfile::parse("laptop").is_err());
    }

    #[test]
    fn omitted_dispatches_are_l1_not_l0() {
        assert_eq!(resolved_execution_level(None), ExecutionLevel::L1);
    }

    #[test]
    fn only_home_data_permits_product_and_resident_levels() {
        assert!(permits(HostProfile::Development, ExecutionLevel::L1));
        assert!(!permits(HostProfile::Development, ExecutionLevel::L2));
        assert!(permits(HostProfile::HomeData, ExecutionLevel::L3));
        assert!(!permits(HostProfile::Release, ExecutionLevel::L2));
    }

    #[test]
    fn persist_profile_replaces_only_its_own_config_key() {
        let home = tempfile::tempdir().expect("temp TACHI_HOME");
        let path = config_path(home.path());
        std::fs::write(&path, "OTHER=value\nTACHI_HOST_PROFILE=release\n").expect("seed config");

        persist_profile(HostProfile::HomeData, home.path()).expect("persist profile");
        assert_eq!(
            std::fs::read_to_string(&path).expect("read config"),
            "OTHER=value\nTACHI_HOST_PROFILE=home_data\n"
        );
    }
}
