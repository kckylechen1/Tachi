use super::profile::{repository_id, JobKind};
use rustls::pki_types::{pem::PemObject, CertificateDer, PrivateKeyDer};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashSet};
use std::error::Error;
use std::io::Read;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RepositoryGrant {
    pub repository: String,
    pub jobs: Vec<JobKind>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct OriginGrant {
    pub repository: String,
    pub canonical_project: String,
    pub jobs: Vec<JobKind>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Peer {
    pub agent_identity_id: String,
    pub host_id: String,
    pub public_key_sha256: String,
    pub endpoint: Option<String>,
    #[serde(default)]
    pub repositories: Vec<RepositoryGrant>,
    /// Independent outbound permission; inbound grants never authorize egress.
    #[serde(default)]
    pub outbound: Vec<OriginGrant>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum DestinationExecutionMode {
    #[default]
    RefusalOnly,
    AdmitOnly,
    Execute,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct DestinationPolicy {
    pub repository: String,
    pub canonical_project: String,
    pub repo_root: PathBuf,
    #[serde(skip)]
    pub canonical_repo_root: PathBuf,
    #[serde(skip)]
    pub canonical_git_dir: PathBuf,
    pub allowed_jobs: Vec<JobKind>,
    #[serde(default)]
    pub review_profile: Option<String>,
    #[serde(default)]
    pub review_timeout_secs: Option<u32>,
    #[serde(default)]
    pub verify_policy: Option<String>,
    #[serde(default)]
    pub verify_command: Option<Vec<String>>,
    #[serde(skip)]
    pub verify_command_identity: Option<(u64, u64)>,
    #[serde(default)]
    pub verify_timeout_secs: Option<u32>,
    pub resource_slot: String,
    pub max_concurrent_runs: u32,
    pub lease_ttl_secs: u32,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct GatewayConfig {
    pub schema_version: String,
    pub agent_identity_id: String,
    pub host_id: String,
    pub issuer_id: String,
    pub trust_domain: String,
    pub home: PathBuf,
    /// Dedicated worker run/worktree root outside `home`, required in `execute`
    /// mode so the worker sandbox can deny all reads/writes to `home`.
    #[serde(default)]
    pub worker_runs_dir: Option<PathBuf>,
    /// Existing origin global store, selected only by the protected operator
    /// configuration. Serve/revoke continue to use the gateway's own global.db.
    pub origin_database: Option<PathBuf>,
    pub bind: SocketAddr,
    pub certificate: PathBuf,
    pub private_key: PathBuf,
    pub peer_ca: PathBuf,
    pub peers: Vec<Peer>,
    pub public_endpoint: Option<String>,
    #[serde(default)]
    pub execution_mode: DestinationExecutionMode,
    #[serde(default)]
    pub destinations: Vec<DestinationPolicy>,
    #[serde(skip)]
    pub canonical_config_path: PathBuf,
    #[serde(skip)]
    pub canonical_home: PathBuf,
    #[serde(skip)]
    pub canonical_private_key: PathBuf,
    #[serde(skip)]
    pub canonical_origin_db: Option<PathBuf>,
    #[serde(skip)]
    pub canonical_worker_runs_dir: Option<PathBuf>,
}

pub(super) fn identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && value.trim() == value
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._:/-".contains(&b))
        && !tachi_runtime_core::secret_scan::contains_secret_like(value)
}

impl GatewayConfig {
    pub fn load(path: &Path) -> Result<Self, Box<dyn Error>> {
        let bytes = read_operator_file(path, true)?;
        let mut config: Self = serde_json::from_slice(&bytes)?;
        if config.schema_version != "tachi.remote_gateway.v1"
            || ![
                &config.agent_identity_id,
                &config.host_id,
                &config.issuer_id,
                &config.trust_domain,
            ]
            .into_iter()
            .all(|s| identifier(s))
            || !crate::session_identity::valid_agent_identity_assertion(&config.agent_identity_id)
            || config.peers.is_empty()
            || config.peers.len() > 64
        {
            return Err("invalid remote gateway identity, schema or enrollment count".into());
        }
        protected_path(&config.home, true)?;
        config.canonical_config_path = std::fs::canonicalize(path)?;
        config.canonical_home = std::fs::canonicalize(&config.home)?;
        if config.private_key.is_absolute() && config.private_key.exists() {
            config.canonical_private_key = std::fs::canonicalize(&config.private_key)?;
        }
        if let Some(origin_db) = &config.origin_database {
            let canon_origin = origin_db.canonicalize()?;
            if !origin_db.is_absolute() || !canon_origin.starts_with(&config.canonical_home) {
                return Err(
                    "origin database must be an existing absolute path under its protected home"
                        .into(),
                );
            }
            let metadata = std::fs::symlink_metadata(origin_db)?;
            if !metadata.is_file() || metadata.file_type().is_symlink() {
                return Err("origin database must be an existing regular file".into());
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                if metadata.uid() != unsafe { libc::geteuid() } || metadata.mode() & 0o022 != 0 {
                    return Err("origin database has unsafe ownership or write permissions".into());
                }
            }
            config.canonical_origin_db = Some(canon_origin);
        }
        if let Some(endpoint) = &config.public_endpoint {
            validate_endpoint(endpoint)?;
        }
        let mut keys = HashSet::new();
        let mut identities = HashSet::new();
        for peer in &config.peers {
            if !identifier(&peer.agent_identity_id)
                || !crate::session_identity::valid_agent_identity_assertion(&peer.agent_identity_id)
                || !identifier(&peer.host_id)
                || peer.agent_identity_id == config.agent_identity_id
                || peer.public_key_sha256.len() != 64
                || !peer
                    .public_key_sha256
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                || !keys.insert(&peer.public_key_sha256)
                || !identities.insert(&peer.agent_identity_id)
            {
                return Err("invalid or ambiguous remote peer enrollment".into());
            }
            if let Some(endpoint) = &peer.endpoint {
                validate_endpoint(endpoint)?;
            }
            let mut repositories = HashSet::new();
            if peer.repositories.len() > 64
                || (!peer.repositories.is_empty() && config.public_endpoint.is_none())
            {
                return Err(
                    "remote repository grants need a bounded set and explicit public endpoint"
                        .into(),
                );
            }
            for grant in &peer.repositories {
                let jobs: HashSet<_> = grant.jobs.iter().collect();
                if !repository_id(&grant.repository)
                    || !repositories.insert(&grant.repository)
                    || jobs.is_empty()
                    || jobs.len() != grant.jobs.len()
                {
                    return Err("invalid or ambiguous remote repository grant".into());
                }
            }
            let mut outbound = HashSet::new();
            if peer.outbound.len() > 64 {
                return Err("remote outbound grants exceed the bounded profile".into());
            }
            for grant in &peer.outbound {
                let jobs: HashSet<_> = grant.jobs.iter().collect();
                if !repository_id(&grant.repository)
                    || !identifier(&grant.canonical_project)
                    || !outbound.insert(&grant.repository)
                    || jobs.is_empty()
                    || jobs.len() != grant.jobs.len()
                    || peer.endpoint.is_none()
                {
                    return Err("invalid or ambiguous remote outbound grant".into());
                }
            }
        }
        validate_destinations(&mut config.destinations, config.execution_mode)?;
        if let Some(runs_dir) = &config.worker_runs_dir {
            protected_path(runs_dir, true)?;
            let canon_runs = std::fs::canonicalize(runs_dir)?;
            let overlaps = |other: &Path| {
                !other.as_os_str().is_empty()
                    && (canon_runs.starts_with(other) || other.starts_with(&canon_runs))
            };
            if overlaps(&config.canonical_home)
                || overlaps(&config.canonical_private_key)
                || overlaps(&config.canonical_config_path)
                || config.canonical_origin_db.as_deref().is_some_and(overlaps)
                || config
                    .destinations
                    .iter()
                    .any(|dest| overlaps(&dest.canonical_repo_root))
            {
                return Err(
                    "worker_runs_dir must be disjoint from gateway home, private key, config, origin_database, and destination repos"
                        .into(),
                );
            }
            config.canonical_worker_runs_dir = Some(canon_runs);
            if config.execution_mode == DestinationExecutionMode::Execute {
                config.worker_credential_paths()?;
            }
        } else if config.execution_mode == DestinationExecutionMode::Execute {
            return Err(
                "execute mode requires an isolated worker_runs_dir outside gateway home".into(),
            );
        }
        Ok(config)
    }

    /// Returns host credential paths and whether each denotes a directory.
    /// Include lexical paths as well as resolved targets of existing symlinks.
    pub(super) fn worker_credential_paths(&self) -> std::io::Result<BTreeMap<PathBuf, bool>> {
        const CODEX_AUTH: &[&str] = &["auth.json"];
        const OPENCODE_AUTH: &[&str] = &[
            "auth.json",
            "opencode.db",
            "opencode.db-wal",
            "opencode.db-shm",
        ];
        let mut paths = Vec::new();
        let mut provider_dirs = Vec::new();
        if let Some(home) = std::env::var_os("HOME") {
            let home = PathBuf::from(home);
            paths.extend(
                [
                    ".tachi",
                    ".sigil",
                    ".secrets",
                    ".config/opencode",
                    ".ssh",
                    ".aws",
                    ".gnupg",
                    ".config/gh",
                    ".kube",
                ]
                .map(|name| (home.join(name), true)),
            );
            paths.extend([".netrc", ".git-credentials"].map(|name| (home.join(name), false)));
            provider_dirs.push((home.join(".codex"), CODEX_AUTH));
            provider_dirs.push((home.join(".local/share/opencode"), OPENCODE_AUTH));
        }
        for (name, append_opencode, auth_files) in [
            ("CODEX_HOME", false, CODEX_AUTH),
            ("OPENCODE_DATA_DIR", false, OPENCODE_AUTH),
            ("XDG_DATA_HOME", true, OPENCODE_AUTH),
            ("XDG_CONFIG_HOME", true, &[][..]),
        ] {
            if let Some(value) = std::env::var_os(name) {
                let mut path = PathBuf::from(value);
                if append_opencode {
                    path.push("opencode");
                }
                provider_dirs.push((path, auth_files));
            }
        }
        for (dir, auth_files) in provider_dirs {
            // A provider may follow a credential symlink outside its data directory.
            paths.extend(auth_files.iter().map(|file| (dir.join(file), false)));
            paths.push((dir, true));
        }
        let mut protected = BTreeMap::new();
        for (path, directory) in paths {
            let path = std::path::absolute(path)?;
            match path.canonicalize() {
                Ok(canonical) => {
                    let is_dir = std::fs::metadata(&canonical)?.is_dir();
                    protected.insert(canonical, is_dir);
                    protected.insert(path, is_dir);
                }
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                    protected.insert(path, directory);
                }
                Err(err) => return Err(err),
            }
        }
        if self.canonical_worker_runs_dir.as_ref().is_some_and(|runs| {
            protected
                .keys()
                .any(|path| runs.starts_with(path) || path.starts_with(runs))
        }) {
            return Err(std::io::Error::other(
                "worker_runs_dir must be disjoint from host operator credential paths",
            ));
        }
        Ok(protected)
    }

    pub fn certificates(&self) -> Result<Vec<CertificateDer<'static>>, Box<dyn Error>> {
        let bytes = read_operator_file(&self.certificate, false)?;
        let certs = CertificateDer::pem_slice_iter(&bytes).collect::<Result<Vec<_>, _>>()?;
        if certs.is_empty() {
            return Err("remote gateway certificate chain is empty".into());
        }
        Ok(certs)
    }

    pub fn key(&self) -> Result<PrivateKeyDer<'static>, Box<dyn Error>> {
        Ok(PrivateKeyDer::from_pem_slice(&read_operator_file(
            &self.private_key,
            true,
        )?)?)
    }

    pub fn roots(&self) -> Result<rustls::RootCertStore, Box<dyn Error>> {
        let mut roots = rustls::RootCertStore::empty();
        let bytes = read_operator_file(&self.peer_ca, false)?;
        for cert in CertificateDer::pem_slice_iter(&bytes) {
            roots.add(cert?)?;
        }
        if roots.is_empty() {
            return Err("remote gateway peer CA set is empty".into());
        }
        Ok(roots)
    }
}

fn validate_endpoint(endpoint: &str) -> Result<(), Box<dyn Error>> {
    let url = url::Url::parse(endpoint)?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.path() != "/"
    {
        return Err("remote endpoint must be an HTTPS origin without credentials".into());
    }
    Ok(())
}

pub(super) fn read_operator_file(path: &Path, private: bool) -> Result<Vec<u8>, Box<dyn Error>> {
    if !path.is_absolute() {
        return Err("remote gateway paths must be explicit absolute paths".into());
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let file = options.open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err("remote gateway operator input is not a regular file".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let forbidden = if private { 0o077 } else { 0o022 };
        if metadata.uid() != unsafe { libc::geteuid() } || metadata.mode() & forbidden != 0 {
            return Err("remote gateway operator file has unsafe ownership or permissions".into());
        }
    }
    let mut bytes = Vec::new();
    file.take(64 * 1024 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > 64 * 1024 {
        return Err("remote gateway operator input exceeds 64 KiB".into());
    }
    Ok(bytes)
}

pub(super) fn public_key_digest(cert: &CertificateDer<'_>) -> Result<String, rustls::Error> {
    let parsed = rustls::server::ParsedCertificate::try_from(cert)?;
    Ok(format!(
        "{:x}",
        Sha256::digest(parsed.subject_public_key_info().as_ref())
    ))
}

// This protects operator files against other OS users. It is deliberately not
// presented as isolation from a worker running under the gateway's own UID.
fn protected_path(path: &Path, directory: bool) -> Result<(), Box<dyn Error>> {
    if !path.is_absolute() {
        return Err("remote gateway paths must be explicit absolute paths".into());
    }
    let metadata = std::fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink()
        || (directory && !metadata.is_dir())
        || (!directory && !metadata.is_file())
    {
        return Err("remote gateway protected path has the wrong file type".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.uid() != unsafe { libc::geteuid() } || metadata.mode() & 0o077 != 0 {
            return Err(
                "remote gateway home/config/key must be owned by its OS account and private".into(),
            );
        }
    }
    #[cfg(not(unix))]
    return Err(
        "remote gateway needs a verified OS file-protection adapter on this platform".into(),
    );
    Ok(())
}

pub(super) fn resolve_pinned_git_dir(
    repo_root: &Path,
) -> Result<(PathBuf, PathBuf), Box<dyn Error>> {
    if !repo_root.is_absolute() {
        return Err("remote destination repo_root must be an absolute path".into());
    }
    let root_meta = std::fs::symlink_metadata(repo_root)?;
    if root_meta.file_type().is_symlink() || !root_meta.is_dir() {
        return Err("remote destination repo_root must be a real non-symlink directory".into());
    }
    let canonical_root = std::fs::canonicalize(repo_root)?;
    let dot_git = repo_root.join(".git");
    let git_meta = std::fs::symlink_metadata(&dot_git)?;
    if git_meta.file_type().is_symlink() || (!git_meta.is_dir() && !git_meta.is_file()) {
        return Err("remote destination repo_root must contain a real .git entry".into());
    }
    let canonical_git_dir = if git_meta.is_dir() {
        std::fs::canonicalize(&dot_git)?
    } else {
        let raw = std::fs::read_to_string(&dot_git)?;
        let Some(spec) = raw
            .lines()
            .next()
            .and_then(|line| line.trim().strip_prefix("gitdir:"))
            .map(str::trim)
            .filter(|s| !s.is_empty())
        else {
            return Err("remote destination .git file is missing a valid gitdir directive".into());
        };
        let git_path = Path::new(spec);
        let resolved = if git_path.is_absolute() {
            git_path.to_path_buf()
        } else {
            canonical_root.join(git_path)
        };
        let resolved_meta = std::fs::symlink_metadata(&resolved)?;
        if resolved_meta.file_type().is_symlink() || !resolved_meta.is_dir() {
            return Err("remote destination gitdir must resolve to a real directory".into());
        }
        std::fs::canonicalize(&resolved)?
    };
    Ok((canonical_root, canonical_git_dir))
}

pub(super) type VerifiedCommandFile = (std::fs::File, Option<(u64, u64)>);

pub(super) fn open_verified_command_file(
    command: &[String],
) -> Result<VerifiedCommandFile, Box<dyn Error>> {
    if command.is_empty() || command.len() > 16 {
        return Err("remote verify_command must have 1..=16 arguments".into());
    }
    for arg in command {
        if arg.is_empty()
            || arg.len() > 1024
            || arg.contains('\0')
            || tachi_runtime_core::secret_scan::contains_secret_like(arg)
        {
            return Err("remote verify_command contains invalid or secret-like argument".into());
        }
    }
    let exe = Path::new(&command[0]);
    if !exe.is_absolute() {
        return Err("remote verify_command executable must be an absolute path".into());
    }
    let parent = exe
        .parent()
        .ok_or("remote verify_command executable has no parent directory")?;
    let parent_meta = std::fs::symlink_metadata(parent)?;
    if parent_meta.file_type().is_symlink() || !parent_meta.is_dir() {
        return Err("remote verify_command parent must be a real non-symlink directory".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let euid = unsafe { libc::geteuid() };
        if (parent_meta.uid() != euid && parent_meta.uid() != 0) || parent_meta.mode() & 0o022 != 0
        {
            return Err(
                "remote verify_command parent directory must be owner/root-owned and non-group/world-writable"
                    .into(),
            );
        }
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let file = options
        .open(exe)
        .map_err(|_| "remote verify_command executable is unavailable or a symlink")?;
    let meta = file.metadata()?;
    if !meta.is_file() {
        return Err("remote verify_command executable must be a regular file".into());
    }
    #[cfg(unix)]
    let identity = {
        use std::os::unix::fs::MetadataExt;
        if meta.uid() != unsafe { libc::geteuid() }
            || meta.mode() & 0o022 != 0
            || meta.mode() & 0o111 == 0
        {
            return Err(
                "remote verify_command executable must be owner-owned, non-group/world-writable, and executable"
                    .into(),
            );
        }
        Some((meta.dev(), meta.ino()))
    };
    #[cfg(not(unix))]
    let identity = None;
    Ok((file, identity))
}

fn validate_verify_command(command: &[String]) -> Result<Option<(u64, u64)>, Box<dyn Error>> {
    open_verified_command_file(command).map(|(_, id)| id)
}

fn validate_destinations(
    destinations: &mut [DestinationPolicy],
    mode: DestinationExecutionMode,
) -> Result<(), Box<dyn Error>> {
    if destinations.len() > 64 {
        return Err("remote destination policies exceed bounded profile".into());
    }
    let mut repos = HashSet::new();
    for dest in destinations {
        if !repository_id(&dest.repository)
            || !repos.insert(dest.repository.clone())
            || !identifier(&dest.canonical_project)
            || !identifier(&dest.resource_slot)
            || !(1..=16).contains(&dest.max_concurrent_runs)
            || !(60..=3600).contains(&dest.lease_ttl_secs)
        {
            return Err("invalid remote destination policy identity, slot, or limits".into());
        }
        if let Some(timeout) = dest.review_timeout_secs {
            if !(5..=dest.lease_ttl_secs).contains(&timeout) {
                return Err("remote review_timeout_secs must be within 5..=lease_ttl_secs".into());
            }
        }
        if let Some(timeout) = dest.verify_timeout_secs {
            if !(5..=dest.lease_ttl_secs).contains(&timeout) {
                return Err("remote verify_timeout_secs must be within 5..=lease_ttl_secs".into());
            }
        }
        let (canonical_root, canonical_git_dir) = resolve_pinned_git_dir(&dest.repo_root)?;
        dest.canonical_repo_root = canonical_root;
        dest.canonical_git_dir = canonical_git_dir;
        let jobs: HashSet<_> = dest.allowed_jobs.iter().collect();
        if jobs.is_empty() || jobs.len() != dest.allowed_jobs.len() {
            return Err("remote destination allowed_jobs must be non-empty and unique".into());
        }
        if jobs.contains(&JobKind::ReviewRevision) {
            let Some(profile_name) = dest.review_profile.as_deref() else {
                return Err("review_revision destination requires review_profile".into());
            };
            let Some(profile) = tachi_dispatch::native_session_profile(profile_name) else {
                return Err("unknown remote review_profile".into());
            };
            if profile.writable {
                return Err("remote review_profile must be read-only".into());
            }
        } else if let Some(profile_name) = dest.review_profile.as_deref() {
            let Some(profile) = tachi_dispatch::native_session_profile(profile_name) else {
                return Err("unknown remote review_profile".into());
            };
            if profile.writable {
                return Err("remote review_profile must be read-only".into());
            }
        }
        if jobs.contains(&JobKind::VerifyPr) {
            let Some(verify_policy) = dest.verify_policy.as_deref() else {
                return Err("verify_pr destination requires verify_policy".into());
            };
            if !identifier(verify_policy) {
                return Err("invalid remote verify_policy identifier".into());
            }
            if mode == DestinationExecutionMode::Execute {
                let Some(cmd) = dest.verify_command.as_deref() else {
                    return Err("execute mode verify_pr destination requires verify_command".into());
                };
                dest.verify_command_identity = validate_verify_command(cmd)?;
            } else if let Some(cmd) = dest.verify_command.as_deref() {
                dest.verify_command_identity = validate_verify_command(cmd)?;
            }
        } else {
            if let Some(verify_policy) = dest.verify_policy.as_deref() {
                if !identifier(verify_policy) {
                    return Err("invalid remote verify_policy identifier".into());
                }
            }
            if let Some(cmd) = dest.verify_command.as_deref() {
                dest.verify_command_identity = validate_verify_command(cmd)?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn admit_only_destination_validation_enforces_readonly_profile_real_repo_and_bounds() {
        let dir = tempfile::tempdir().unwrap();
        let real_repo = dir.path().join("repo");
        std::fs::create_dir_all(real_repo.join(".git")).unwrap();
        let valid = DestinationPolicy {
            repository: "owner/example".into(),
            canonical_project: "a".repeat(64),
            repo_root: real_repo.clone(),
            canonical_repo_root: PathBuf::new(),
            canonical_git_dir: PathBuf::new(),
            allowed_jobs: vec![JobKind::ReviewRevision, JobKind::VerifyPr],
            review_profile: Some("codex_sol_review".into()),
            review_timeout_secs: Some(60),
            verify_policy: Some("dgx2_acceptance_v1".into()),
            verify_command: None,
            verify_command_identity: None,
            verify_timeout_secs: Some(60),
            resource_slot: "slot-1".into(),
            max_concurrent_runs: 2,
            lease_ttl_secs: 600,
        };
        validate_destinations(&mut [valid.clone()], DestinationExecutionMode::AdmitOnly).unwrap();
        assert!(
            validate_destinations(&mut [valid.clone()], DestinationExecutionMode::Execute).is_err(),
            "execute mode verify_pr must require verify_command"
        );

        let mut writable = valid.clone();
        writable.review_profile = Some("codex_terra_write".into());
        assert!(
            validate_destinations(&mut [writable], DestinationExecutionMode::AdmitOnly).is_err()
        );

        let mut bad_limit = valid.clone();
        bad_limit.max_concurrent_runs = 0;
        assert!(
            validate_destinations(&mut [bad_limit], DestinationExecutionMode::AdmitOnly).is_err()
        );

        let mut bad_ttl = valid.clone();
        bad_ttl.lease_ttl_secs = 15;
        assert!(
            validate_destinations(&mut [bad_ttl], DestinationExecutionMode::AdmitOnly).is_err()
        );

        let mut bad_timeout = valid.clone();
        bad_timeout.review_timeout_secs = Some(601);
        assert!(
            validate_destinations(&mut [bad_timeout], DestinationExecutionMode::AdmitOnly).is_err()
        );

        let mut not_git = valid.clone();
        let plain_dir = dir.path().join("not-git");
        std::fs::create_dir_all(&plain_dir).unwrap();
        not_git.repo_root = plain_dir;
        assert!(
            validate_destinations(&mut [not_git], DestinationExecutionMode::AdmitOnly).is_err()
        );

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let script = dir.path().join("accept-pr.sh");
            std::fs::write(&script, "#!/bin/sh\necho ok\n").unwrap();
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
            let mut exec_valid = valid.clone();
            exec_valid.verify_command = Some(vec![script.to_string_lossy().into_owned()]);
            validate_destinations(&mut [exec_valid], DestinationExecutionMode::Execute).unwrap();

            let link_script = dir.path().join("accept-link.sh");
            std::os::unix::fs::symlink(&script, &link_script).unwrap();
            let mut symlink_cmd = valid.clone();
            symlink_cmd.verify_command = Some(vec![link_script.to_string_lossy().into_owned()]);
            assert!(
                validate_destinations(&mut [symlink_cmd], DestinationExecutionMode::Execute)
                    .is_err()
            );

            let link_repo = dir.path().join("link-repo");
            std::os::unix::fs::symlink(&real_repo, &link_repo).unwrap();
            let mut symlinked = valid;
            symlinked.repo_root = link_repo;
            assert!(
                validate_destinations(&mut [symlinked], DestinationExecutionMode::AdmitOnly)
                    .is_err()
            );
        }
    }
}
