use super::profile::{repository_id, JobKind};
use rustls::pki_types::{pem::PemObject, CertificateDer, PrivateKeyDer};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::collections::HashSet;
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
    pub verify_policy: Option<String>,
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
        if let Some(path) = &config.origin_database {
            if !path.is_absolute()
                || !path
                    .canonicalize()?
                    .starts_with(config.home.canonicalize()?)
            {
                return Err(
                    "origin database must be an existing absolute path under its protected home"
                        .into(),
                );
            }
            let metadata = std::fs::symlink_metadata(path)?;
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
        validate_destinations(&mut config.destinations)?;
        Ok(config)
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

fn validate_destinations(destinations: &mut [DestinationPolicy]) -> Result<(), Box<dyn Error>> {
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
        } else if let Some(verify_policy) = dest.verify_policy.as_deref() {
            if !identifier(verify_policy) {
                return Err("invalid remote verify_policy identifier".into());
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
            verify_policy: Some("dgx2_acceptance_v1".into()),
            resource_slot: "slot-1".into(),
            max_concurrent_runs: 2,
            lease_ttl_secs: 600,
        };
        validate_destinations(&mut [valid.clone()]).unwrap();

        let mut writable = valid.clone();
        writable.review_profile = Some("codex_terra_write".into());
        assert!(validate_destinations(&mut [writable]).is_err());

        let mut bad_limit = valid.clone();
        bad_limit.max_concurrent_runs = 0;
        assert!(validate_destinations(&mut [bad_limit]).is_err());

        let mut bad_ttl = valid.clone();
        bad_ttl.lease_ttl_secs = 15;
        assert!(validate_destinations(&mut [bad_ttl]).is_err());

        let mut not_git = valid.clone();
        let plain_dir = dir.path().join("not-git");
        std::fs::create_dir_all(&plain_dir).unwrap();
        not_git.repo_root = plain_dir;
        assert!(validate_destinations(&mut [not_git]).is_err());

        #[cfg(unix)]
        {
            let link_repo = dir.path().join("link-repo");
            std::os::unix::fs::symlink(&real_repo, &link_repo).unwrap();
            let mut symlinked = valid;
            symlinked.repo_root = link_repo;
            assert!(validate_destinations(&mut [symlinked]).is_err());
        }
    }
}
