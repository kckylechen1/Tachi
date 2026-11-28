use super::types::{CredentialProfile, CredentialProfileDocument};
use memcore::anchored_fs::AnchoredDirectory;
use std::ffi::OsStr;
use std::io;
use std::path::{Path, PathBuf};

const MAX_PROFILE_CONFIG_BYTES: usize = 1024 * 1024;

pub fn default_credentials_dir() -> PathBuf {
    PathBuf::from(".tachi").join("credentials")
}

fn open_profile_directory(path: &Path) -> io::Result<AnchoredDirectory> {
    let path = std::path::absolute(path)?;
    // These macOS system aliases are outside the project tree. Walk their
    // fixed physical paths without following any project-supplied symlink.
    #[cfg(target_os = "macos")]
    for alias in ["/var", "/tmp", "/etc"] {
        if let Ok(suffix) = path.strip_prefix(alias) {
            let target = Path::new("private").join(&alias[1..]);
            if std::fs::read_link(alias).is_ok_and(|link| link == target) {
                return AnchoredDirectory::open_absolute(&Path::new("/").join(target).join(suffix));
            }
        }
    }
    AnchoredDirectory::open_absolute(&path)
}

fn read_profile_document(
    dir: &AnchoredDirectory,
    name: &OsStr,
    path: &Path,
) -> Result<CredentialProfileDocument, String> {
    let (_, raw) = dir
        .read_regular_file(name, MAX_PROFILE_CONFIG_BYTES)
        .and_then(|result| {
            result.ok_or_else(|| io::Error::other("expected an existing regular file"))
        })
        .map_err(|e| format!("read credential profile config '{}': {e}", path.display()))?;
    serde_json::from_slice(&raw)
        .map_err(|e| format!("parse credential profile config '{}': {e}", path.display()))
}

pub fn load_credential_profile_from_path(
    path: &Path,
    profile_name: &str,
) -> Result<CredentialProfile, String> {
    let name = path.file_name().ok_or_else(|| {
        format!(
            "credential profile config '{}' must name a file",
            path.display()
        )
    })?;
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let dir = open_profile_directory(parent)
        .map_err(|e| format!("read credential profile config '{}': {e}", path.display()))?;
    read_profile_document(&dir, name, path)?
        .credential_profiles
        .remove(profile_name)
        .ok_or_else(|| {
            format!(
                "Credential profile '{profile_name}' not found in {}",
                path.display()
            )
        })
}

pub fn find_credential_profile(
    credentials_dir: &Path,
    profile_name: &str,
) -> Result<(PathBuf, CredentialProfile), String> {
    let directory_error = |e| {
        format!(
            "read credential profile directory '{}': {e}",
            credentials_dir.display()
        )
    };
    let dir = open_profile_directory(credentials_dir).map_err(directory_error)?;
    // Enumeration only supplies candidate names. Every document is opened
    // relative to the directory handle captured above.
    let entries = std::fs::read_dir(credentials_dir).map_err(directory_error)?;
    let mut skipped_invalid_configs = Vec::new();
    for entry in entries {
        let name = entry
            .map_err(|e| format!("read credential profile directory entry: {e}"))?
            .file_name();
        if Path::new(&name).extension() != Some(OsStr::new("json")) {
            continue;
        }
        let path = credentials_dir.join(&name);
        let mut doc = match read_profile_document(&dir, &name, &path) {
            Ok(doc) => doc,
            Err(err) => {
                skipped_invalid_configs.push(err);
                continue;
            }
        };
        if let Some(profile) = doc.credential_profiles.remove(profile_name) {
            return Ok((path, profile));
        }
    }
    let mut err = format!(
        "Credential profile '{profile_name}' not found under {}",
        credentials_dir.display()
    );
    if !skipped_invalid_configs.is_empty() {
        err.push_str(&format!(
            "; skipped invalid configs: {}",
            skipped_invalid_configs.join(", ")
        ));
    }
    Err(err)
}
