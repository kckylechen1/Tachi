use super::admission::Connection;
use super::config::{public_key_digest, GatewayConfig, Peer};
use axum::{
    extract::State,
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use hyper_util::{rt::TokioIo, service::TowerToHyperService};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, SignatureScheme};
use serde_json::{json, Value};
use std::error::Error;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;
use tokio_rustls::TlsAcceptor;

pub(super) async fn serve(
    config: GatewayConfig,
    server: crate::MemoryServer,
) -> Result<(), Box<dyn Error>> {
    let verifier = rustls::server::WebPkiClientVerifier::builder_with_provider(
        Arc::new(config.roots()?),
        Arc::new(rustls::crypto::ring::default_provider()),
    )
    .build()?;
    let mut tls = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_protocol_versions(&[&rustls::version::TLS13])?
    .with_client_cert_verifier(verifier.clone())
    .with_single_cert(config.certificates()?, config.key()?)?;
    // Every admission observes a fresh certificate/key-possession handshake.
    tls.session_storage = Arc::new(rustls::server::NoServerSessionStorage {});
    tls.send_tls13_tickets = 0;
    tls.alpn_protocols = vec![b"http/1.1".to_vec()];
    let acceptor = TlsAcceptor::from(Arc::new(tls));
    let listener = TcpListener::bind(config.bind).await?;
    // This is the gateway's own operator-selected local principal. The remote
    // caller is admitted separately from its TLS proof on each connection.
    crate::claims_ops::admit_agent_connection(
        &server,
        Some(config.agent_identity_id.clone()),
        true,
    )?;
    println!(
        "{}",
        json!({"remote_gateway_listening":listener.local_addr()?.to_string(),"execution":"unavailable"})
    );
    let config = Arc::new(config);
    let capacity = Arc::new(Semaphore::new(64));
    let mut connections = JoinSet::new();
    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => break,
            Some(_) = connections.join_next(), if !connections.is_empty() => {},
            accepted = listener.accept() => {
                let (stream, _) = accepted?;
                let Ok(permit) = capacity.clone().try_acquire_owned() else { continue; };
                let acceptor = acceptor.clone();
                let config = config.clone();
                let base = server.clone();
                let verifier = verifier.clone();
                connections.spawn(async move {
                    let _permit = permit;
                    let Ok(Ok(stream)) = tokio::time::timeout(Duration::from_secs(10), acceptor.accept(stream)).await else { return; };
                    let connection = match Connection::from_tls(&base, config, stream.get_ref().1, verifier) {
                        Ok(connection) => Arc::new(connection),
                        Err(_) => return,
                    };
                    let router = Router::new()
                        .route("/health", get(health))
                        .route("/rpc", post(super::rpc::handle))
                        .route("/.well-known/agent-card.json", get(super::rpc::agent_card))
                        .with_state(connection);
                    let service = TowerToHyperService::new(router);
                    let _ = tokio::time::timeout(
                        Duration::from_secs(15 * 60),
                        hyper::server::conn::http1::Builder::new()
                            .header_read_timeout(Duration::from_secs(10))
                            .timer(hyper_util::rt::TokioTimer::new())
                            .serve_connection(TokioIo::new(stream), service),
                    ).await;
                });
            }
        }
    }
    connections.abort_all();
    while connections.join_next().await.is_some() {}
    Ok(())
}

async fn health(State(connection): State<Arc<Connection>>) -> impl IntoResponse {
    if connection.current().is_err() {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({"error":"remote admission unavailable"})),
        );
    }
    let Some(admission) = connection.server.verified_work_claim_connection() else {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({"error":"remote admission unavailable"})),
        );
    };
    (
        StatusCode::OK,
        Json(json!({
            "schema_version":"tachi.remote_admission.v1",
            "gateway":{
                "agent_identity_id":connection.config.agent_identity_id,
                "host_id":connection.config.host_id,
                "trust_domain":connection.config.trust_domain,
            },
            "peer":{
                "agent_identity_id":connection.peer.agent_identity_id,
                "host_id":connection.peer.host_id,
                "admission_id":admission.admission_id,
                "connection_id":admission.connection_id,
                "verification_method":admission.verification_method,
                "verification_version":admission.verification_version,
            },
            "execution":"unavailable"
        })),
    )
}

#[derive(Debug)]
struct PinnedServerVerifier {
    webpki: Arc<dyn ServerCertVerifier>,
    public_key_sha256: String,
}

impl ServerCertVerifier for PinnedServerVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        server_name: &ServerName<'_>,
        ocsp_response: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        let verified = self.webpki.verify_server_cert(
            end_entity,
            intermediates,
            server_name,
            ocsp_response,
            now,
        )?;
        if public_key_digest(end_entity)? != self.public_key_sha256 {
            return Err(rustls::Error::General(
                "destination key is not enrolled".into(),
            ));
        }
        Ok(verified)
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.webpki.verify_tls12_signature(message, cert, dss)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.webpki.verify_tls13_signature(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.webpki.supported_verify_schemes()
    }
}

pub(super) async fn probe(config: &GatewayConfig, peer_id: &str) -> Result<Value, Box<dyn Error>> {
    let peer = config
        .peers
        .iter()
        .find(|p| p.agent_identity_id == peer_id)
        .ok_or("destination is not enrolled")?;
    let client = pinned_client(config, peer)?;
    probe_with_client(config, peer, &client).await
}

pub(super) fn pinned_client(
    config: &GatewayConfig,
    peer: &Peer,
) -> Result<reqwest::Client, Box<dyn Error>> {
    let verifier = rustls::client::WebPkiServerVerifier::builder_with_provider(
        Arc::new(config.roots()?),
        Arc::new(rustls::crypto::ring::default_provider()),
    )
    .build()?;
    let tls = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_protocol_versions(&[&rustls::version::TLS13])?
    .dangerous()
    .with_custom_certificate_verifier(Arc::new(PinnedServerVerifier {
        webpki: verifier,
        public_key_sha256: peer.public_key_sha256.clone(),
    }))
    .with_client_auth_cert(config.certificates()?, config.key()?)?;
    Ok(reqwest::Client::builder()
        .use_preconfigured_tls(tls)
        .https_only(true)
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(15))
        .build()?)
}

pub(super) async fn probe_with_client(
    config: &GatewayConfig,
    peer: &Peer,
    client: &reqwest::Client,
) -> Result<Value, Box<dyn Error>> {
    let endpoint = peer
        .endpoint
        .as_deref()
        .ok_or("destination has no configured endpoint")?;
    let mut response = client
        .get(url::Url::parse(endpoint)?.join("/health")?)
        .send()
        .await?
        .error_for_status()?;
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        if bytes.len() + chunk.len() > 8192 {
            return Err("remote admission response exceeds 8 KiB".into());
        }
        bytes.extend_from_slice(&chunk);
    }
    let result: Value = serde_json::from_slice(&bytes)?;
    if result["schema_version"] != "tachi.remote_admission.v1"
        || result["gateway"]["agent_identity_id"] != peer.agent_identity_id
        || result["gateway"]["host_id"] != peer.host_id
        || result["gateway"]["trust_domain"] != config.trust_domain
        || result["peer"]["agent_identity_id"] != config.agent_identity_id
        || result["peer"]["host_id"] != config.host_id
        || result["peer"]["verification_method"] != memcore::VERIFIED_ADMISSION_MTLS_METHOD
        || result["peer"]["verification_version"] != memcore::VERIFIED_ADMISSION_VERSION
    {
        return Err(
            "remote admission response does not match the enrolled identities/domain".into(),
        );
    }
    Ok(result)
}
