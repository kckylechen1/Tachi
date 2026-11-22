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
pub(super) struct Peer {
    pub agent_identity_id: String,
    pub host_id: String,
    pub public_key_sha256: String,
    pub endpoint: Option<String>,
    #[serde(default)]
    pub repositories: Vec<RepositoryGrant>,
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
    pub bind: SocketAddr,
    pub certificate: PathBuf,
    pub private_key: PathBuf,
    pub peer_ca: PathBuf,
    pub peers: Vec<Peer>,
    pub public_endpoint: Option<String>,
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
        let config: Self = serde_json::from_slice(&bytes)?;
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
        }
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

fn read_operator_file(path: &Path, private: bool) -> Result<Vec<u8>, Box<dyn Error>> {
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
