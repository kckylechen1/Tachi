use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

const MAX_STAFF_CONFIG_BYTES: usize = 64 * 1024;
const DEFAULT_OPENCODE_PROVIDER: &str = "openai";

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct OpencodeStaffConfig {
    #[serde(default)]
    pub providers: Option<Vec<String>>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SeatModelConfig {
    #[serde(default)]
    pub default_model: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct StaffConfig {
    #[serde(default)]
    pub opencode: OpencodeStaffConfig,
    #[serde(default)]
    pub seats: BTreeMap<String, SeatModelConfig>,
}

impl StaffConfig {
    pub fn admitted_opencode_providers(&self) -> Vec<String> {
        match &self.opencode.providers {
            Some(providers) => providers.clone(),
            None => vec![DEFAULT_OPENCODE_PROVIDER.to_string()],
        }
    }

    pub fn seat_default_model(&self, seat: &str) -> Option<&str> {
        self.seats
            .get(seat)
            .and_then(|seat_cfg| seat_cfg.default_model.as_deref())
            .map(str::trim)
            .filter(|model| !model.is_empty())
    }

    pub fn validate_opencode_model(&self, raw: &str) -> Result<String, String> {
        let model = raw.trim();
        if model.is_empty()
            || model
                .chars()
                .any(|ch| ch.is_whitespace() || ch.is_control())
        {
            return Err(
                "OpenCode model must be a non-empty `provider/model` string without whitespace"
                    .to_string(),
            );
        }
        let Some((provider, model_id)) = model.split_once('/') else {
            return Err(format!(
                "OpenCode model '{model}' must include a `provider/model` prefix"
            ));
        };
        if provider.is_empty() || model_id.is_empty() {
            return Err(format!(
                "OpenCode model '{model}' must include non-empty provider and model identifiers"
            ));
        }
        let allowed = self.admitted_opencode_providers();
        if !allowed.iter().any(|entry| entry == provider) {
            let allowlist = if allowed.is_empty() {
                "<empty>".to_string()
            } else {
                allowed.join(", ")
            };
            return Err(format!(
                "OpenCode provider '{provider}' is not permitted by staff.json opencode.providers allowlist ([{allowlist}])"
            ));
        }
        Ok(model.to_string())
    }
}

pub fn load_staff_config(tachi_home: &Path) -> Result<StaffConfig, String> {
    let raw = match read_staff_config_bytes(tachi_home)? {
        Some(raw) => raw,
        None => return Ok(StaffConfig::default()),
    };
    let path = tachi_home.join("staff.json");
    let config: StaffConfig = serde_json::from_slice(&raw)
        .map_err(|err| format!("parse staff config {}: {err}", path.display()))?;
    validate_loaded_staff_config(&config)?;
    Ok(config)
}

#[cfg(unix)]
fn read_staff_config_bytes(tachi_home: &Path) -> Result<Option<Vec<u8>>, String> {
    use std::io::Read as _;
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};

    let path = tachi_home.join("staff.json");
    let dir_file = match std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_CLOEXEC)
        .open(tachi_home)
    {
        Ok(dir) => dir,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => {
            return Err(format!(
                "open staff config dir {}: {err}",
                tachi_home.display()
            ))
        }
    };
    let fd = unsafe {
        libc::openat(
            dir_file.as_raw_fd(),
            c"staff.json".as_ptr(),
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        let err = std::io::Error::last_os_error();
        if err.raw_os_error() == Some(libc::ENOENT) {
            return Ok(None);
        }
        if err.raw_os_error() == Some(libc::ELOOP) {
            return Err(format!(
                "staff config {} must be a regular file, not a symlink",
                path.display()
            ));
        }
        return Err(format!("open staff config {}: {err}", path.display()));
    }
    let staff_file = unsafe { std::fs::File::from_raw_fd(fd) };

    let euid = unsafe { libc::geteuid() };
    let dir_meta = dir_file
        .metadata()
        .map_err(|err| format!("stat staff config dir {}: {err}", tachi_home.display()))?;
    if !dir_meta.is_dir() || dir_meta.uid() != euid || (dir_meta.mode() & 0o022 != 0) {
        return Err(format!(
            "staff config dir {} must be an owner-only directory (uid {euid}, not group- or world-writable)",
            tachi_home.display()
        ));
    }
    let staff_meta = staff_file
        .metadata()
        .map_err(|err| format!("stat staff config {}: {err}", path.display()))?;
    if !staff_meta.file_type().is_file() {
        return Err(format!(
            "staff config {} must be a regular file, not a symlink or directory",
            path.display()
        ));
    }
    if staff_meta.uid() != euid {
        return Err(format!(
            "staff config {} must be owned by current uid {euid}",
            path.display()
        ));
    }
    if staff_meta.mode() & 0o022 != 0 {
        return Err(format!(
            "staff config {} must not be group- or world-writable",
            path.display()
        ));
    }
    if staff_meta.len() > MAX_STAFF_CONFIG_BYTES as u64 {
        return Err(format!(
            "staff config {} exceeds {MAX_STAFF_CONFIG_BYTES} bytes",
            path.display()
        ));
    }
    let mut raw = Vec::new();
    staff_file
        .take((MAX_STAFF_CONFIG_BYTES + 1) as u64)
        .read_to_end(&mut raw)
        .map_err(|err| format!("read staff config {}: {err}", path.display()))?;
    if raw.len() > MAX_STAFF_CONFIG_BYTES {
        return Err(format!(
            "staff config {} exceeds {MAX_STAFF_CONFIG_BYTES} bytes",
            path.display()
        ));
    }
    Ok(Some(raw))
}

#[cfg(not(unix))]
fn read_staff_config_bytes(tachi_home: &Path) -> Result<Option<Vec<u8>>, String> {
    use std::io::Read as _;

    let path = tachi_home.join("staff.json");
    let link_meta = match std::fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => {
            return Err(format!(
                "read staff config metadata {}: {err}",
                path.display()
            ))
        }
    };
    if link_meta.file_type().is_symlink() || !link_meta.is_file() {
        return Err(format!(
            "staff config {} must be a regular file, not a symlink or directory",
            path.display()
        ));
    }
    let file = std::fs::File::open(&path)
        .map_err(|err| format!("open staff config {}: {err}", path.display()))?;
    let meta = file
        .metadata()
        .map_err(|err| format!("stat staff config {}: {err}", path.display()))?;
    if !meta.is_file() {
        return Err(format!(
            "staff config {} must be a regular file, not a symlink or directory",
            path.display()
        ));
    }
    if meta.len() > MAX_STAFF_CONFIG_BYTES as u64 {
        return Err(format!(
            "staff config {} exceeds {MAX_STAFF_CONFIG_BYTES} bytes",
            path.display()
        ));
    }
    let mut raw = Vec::new();
    file.take((MAX_STAFF_CONFIG_BYTES + 1) as u64)
        .read_to_end(&mut raw)
        .map_err(|err| format!("read staff config {}: {err}", path.display()))?;
    if raw.len() > MAX_STAFF_CONFIG_BYTES {
        return Err(format!(
            "staff config {} exceeds {MAX_STAFF_CONFIG_BYTES} bytes",
            path.display()
        ));
    }
    Ok(Some(raw))
}

fn validate_loaded_staff_config(config: &StaffConfig) -> Result<(), String> {
    if let Some(providers) = &config.opencode.providers {
        for provider in providers {
            if provider.is_empty()
                || provider.trim() != provider
                || provider.contains('/')
                || provider
                    .chars()
                    .any(|ch| ch.is_whitespace() || ch.is_control())
            {
                return Err(format!(
                    "invalid provider entry '{provider}' in staff.json opencode.providers"
                ));
            }
        }
    }
    for (seat, seat_cfg) in &config.seats {
        let Some(profile) = crate::resolve_dispatch_profile(seat) else {
            return Err(format!(
                "unknown seat '{seat}' in staff.json seats; only admitted OpenCode native profiles can be configured"
            ));
        };
        if profile.name != seat {
            return Err(format!(
                "seat key '{seat}' in staff.json must use canonical profile name '{}'",
                profile.name
            ));
        }
        if profile.backend != "opencode" || profile.native_session.is_none() {
            return Err(format!(
                "seat '{seat}' in staff.json is not an OpenCode native profile"
            ));
        }
        if let Some(default_model) = seat_cfg.default_model.as_deref() {
            config.validate_opencode_model(default_model)?;
        }
    }
    Ok(())
}
