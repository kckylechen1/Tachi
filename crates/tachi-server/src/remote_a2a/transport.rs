use super::admission::Connection;
use super::config::{public_key_digest, GatewayConfig, Peer};
use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use hyper_util::{rt::TokioIo, service::TowerToHyperService};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, SignatureScheme};
use serde_json::{json, Value};
use std::error::Error;
use std::fs::File;
use std::io::ErrorKind;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::net::TcpListener;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;
use tokio_rustls::TlsAcceptor;

/// Admitted connections served at once.
const MAX_CONNECTIONS: usize = 64;
/// Handshakes in flight at once. A connection that has not completed TLS may
/// only hold one of these, so peers that are not admitted yet can never
/// occupy the slots kept for admitted connections.
const MAX_HANDSHAKES: usize = 32;
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
const CONNECTION_LIFETIME: Duration = Duration::from_secs(15 * 60);

/// What the accept loop does after `TcpListener::accept` fails.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AfterAcceptError {
    /// The peer went away before its connection was returned. The listener is
    /// fine, so the loop simply accepts again.
    PeerClosed,
    /// The listener itself is short of a resource. Log it, pause and accept
    /// again; a transient failure must not end the gateway process.
    RetryAfter(Duration),
}

fn accept_error_policy(error: &std::io::Error, backoff: Duration) -> AfterAcceptError {
    if matches!(
        error.kind(),
        ErrorKind::ConnectionRefused | ErrorKind::ConnectionAborted | ErrorKind::ConnectionReset
    ) {
        AfterAcceptError::PeerClosed
    } else {
        AfterAcceptError::RetryAfter(backoff)
    }
}

/// Pause after each consecutive accept failure: doubled, at most one second.
fn next_backoff(backoff: Duration) -> Duration {
    if backoff.is_zero() {
        Duration::from_millis(50)
    } else {
        backoff.saturating_mul(2).min(Duration::from_secs(1))
    }
}

/// The two admission stages of the listener.
#[derive(Debug)]
struct AdmissionSlots {
    handshakes: Arc<Semaphore>,
    established: Arc<Semaphore>,
}

impl AdmissionSlots {
    fn new() -> Self {
        Self {
            handshakes: Arc::new(Semaphore::new(MAX_HANDSHAKES)),
            established: Arc::new(Semaphore::new(MAX_CONNECTIONS)),
        }
    }
}

/// How often events raised before a peer is admitted may be written. A peer
/// that was not admitted must not be able to fill the log.
#[derive(Debug)]
struct EventBudget {
    window_start: Instant,
    written: u32,
    suppressed: u32,
}

const EVENT_WINDOW: Duration = Duration::from_secs(1);
const EVENTS_PER_WINDOW: u32 = 20;

impl EventBudget {
    fn new(now: Instant) -> Self {
        Self {
            window_start: now,
            written: 0,
            suppressed: 0,
        }
    }

    /// Whether this event is written. The first event written after a window
    /// rolls over also reports how many were dropped before it.
    fn allow(&mut self, now: Instant) -> Option<u32> {
        if now.saturating_duration_since(self.window_start) >= EVENT_WINDOW {
            self.window_start = now;
            self.written = 0;
        }
        self.written += 1;
        (self.written <= EVENTS_PER_WINDOW).then(|| std::mem::take(&mut self.suppressed))
    }

    fn suppress(&mut self) {
        self.suppressed += 1;
    }
}

type UnauthenticatedEvents = Arc<std::sync::Mutex<EventBudget>>;

/// Writes one event raised before admission: a static reason, the peer
/// address and a public-key fingerprint. Never request bytes, headers or key
/// material.
fn log_unauthenticated(
    events: &UnauthenticatedEvents,
    reason: &str,
    peer: std::net::SocketAddr,
    detail: impl std::fmt::Display,
) {
    let mut budget = events
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    match budget.allow(Instant::now()) {
        Some(suppressed) => tracing::warn!(
            target: "tachi::remote_a2a",
            peer = %peer,
            reason,
            suppressed,
            detail = %detail,
            "remote gateway: connection not admitted"
        ),
        None => budget.suppress(),
    }
}

fn key_fingerprint(key: &str) -> &str {
    key.get(..16).unwrap_or(key)
}

/// The gateway logs to stderr; stdout carries its JSON control lines.
fn init_logging() {
    use tracing_subscriber::EnvFilter;
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,tachi_server=info"));
    let _ = tracing_subscriber::fmt()
        .with_ansi(false)
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .try_init();
}

pub(super) fn acquire_gateway_supervisor_lock(home: &Path) -> std::io::Result<File> {
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;
        use std::os::unix::fs::OpenOptionsExt;

        let lock = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(home.join("remote-gateway.lock"))?;
        // SAFETY: lock owns the open descriptor throughout this call.
        if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(lock)
    }
    #[cfg(not(unix))]
    {
        let _ = home;
        Err(std::io::Error::new(
            ErrorKind::Unsupported,
            "remote gateway locking is unavailable",
        ))
    }
}

pub(super) async fn serve(
    config: GatewayConfig,
    server: crate::MemoryServer,
    _supervisor_lock: File,
) -> Result<(), Box<dyn Error>> {
    init_logging();
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
    if let Some((Some(identity), connection_id, state)) = server.work_claim_connection() {
        if state == "self_asserted" {
            let gateway_actor = memcore::db::a2a::A2aTransitionActor {
                agent_identity_id: identity,
                connection_id,
            };
            super::executor::recover_orphaned_gateway_runs(&server, &config, &gateway_actor)?;
            server.with_global_store(|store| {
                memcore::db::a2a::reconcile_active_remote_runs(store.connection(), &gateway_actor)
                    .map_err(|error| error.to_string())
            })?;
        }
    }
    let execution_label = match config.execution_mode {
        super::config::DestinationExecutionMode::Execute => "enabled",
        super::config::DestinationExecutionMode::RefusalOnly
        | super::config::DestinationExecutionMode::AdmitOnly => "unavailable",
    };
    println!(
        "{}",
        json!({"remote_gateway_listening":listener.local_addr()?.to_string(),"execution":execution_label})
    );
    let config = Arc::new(config);
    let run_registry = Arc::new(super::executor::RemoteRunRegistry::new());
    let slots = Arc::new(AdmissionSlots::new());
    let unauthenticated: UnauthenticatedEvents =
        Arc::new(std::sync::Mutex::new(EventBudget::new(Instant::now())));
    let mut backoff = Duration::ZERO;
    let mut connections = JoinSet::new();
    #[cfg(unix)]
    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => break,
            _ = async {
                #[cfg(unix)]
                {
                    let _ = sigterm.recv().await;
                }
                #[cfg(not(unix))]
                {
                    std::future::pending::<()>().await;
                }
            } => break,
            Some(_) = connections.join_next(), if !connections.is_empty() => {},
            accepted = listener.accept() => {
                let (stream, peer) = match accepted {
                    Ok(accepted) => {
                        backoff = Duration::ZERO;
                        accepted
                    }
                    Err(error) => match accept_error_policy(&error, next_backoff(backoff)) {
                        AfterAcceptError::PeerClosed => {
                            tracing::debug!(
                                target: "tachi::remote_a2a",
                                error = %error,
                                "remote gateway: peer closed before accept"
                            );
                            continue;
                        }
                        AfterAcceptError::RetryAfter(pause) => {
                            tracing::warn!(
                                target: "tachi::remote_a2a",
                                error = %error,
                                pause_ms = pause.as_millis() as u64,
                                "remote gateway: accept failed, retrying"
                            );
                            backoff = pause;
                            tokio::time::sleep(pause).await;
                            continue;
                        }
                    },
                };
                let Ok(handshaking) = slots.handshakes.clone().try_acquire_owned() else {
                    log_unauthenticated(&unauthenticated, "handshake_slots_busy", peer, "");
                    continue;
                };
                let acceptor = acceptor.clone();
                let config = config.clone();
                let base = server.clone();
                let verifier = verifier.clone();
                let run_registry = run_registry.clone();
                let slots = slots.clone();
                let unauthenticated = unauthenticated.clone();
                connections.spawn(async move {
                    let admitted = {
                        // Held only until admission succeeds or fails.
                        let _handshaking = handshaking;
                        let stream = match tokio::time::timeout(HANDSHAKE_TIMEOUT, acceptor.accept(stream)).await {
                            Ok(Ok(stream)) => stream,
                            Ok(Err(error)) => {
                                log_unauthenticated(&unauthenticated, "tls_handshake_failed", peer, error);
                                return;
                            }
                            Err(_) => {
                                log_unauthenticated(&unauthenticated, "tls_handshake_timeout", peer, "");
                                return;
                            }
                        };
                        let Ok(established) = slots.established.clone().try_acquire_owned() else {
                            log_unauthenticated(&unauthenticated, "connection_slots_busy", peer, "");
                            return;
                        };
                        match Connection::from_tls(&base, config.clone(), stream.get_ref().1, verifier.clone(), run_registry).await {
                            Ok(connection) => (stream, established, Arc::new(connection)),
                            Err(rejection) => {
                                log_unauthenticated(
                                    &unauthenticated,
                                    "peer_rejected",
                                    peer,
                                    format!("cause={} key={}", rejection.reason, key_fingerprint(&rejection.key)),
                                );
                                return;
                            }
                        }
                    };
                    let (stream, _established, connection) = admitted;
                    tracing::info!(
                        target: "tachi::remote_a2a",
                        peer = %peer,
                        identity = %connection.peer.agent_identity_id,
                        "remote gateway: peer admitted"
                    );
                    let router = Router::new()
                        .route("/health", get(health))
                        .route("/rpc", post(super::rpc::handle))
                        .route("/.well-known/agent-card.json", get(super::rpc::agent_card))
                        .with_state(connection);
                    let service = TowerToHyperService::new(router);
                    let _ = tokio::time::timeout(
                        CONNECTION_LIFETIME,
                        hyper::server::conn::http1::Builder::new()
                            .header_read_timeout(HANDSHAKE_TIMEOUT)
                            .timer(hyper_util::rt::TokioTimer::new())
                            .serve_connection(TokioIo::new(stream), service),
                    )
                    .await;
                });
            }
        }
    }
    connections.abort_all();
    while connections.join_next().await.is_some() {}
    run_registry.shutdown().await;
    Ok(())
}

fn admission_unavailable() -> Response {
    (
        StatusCode::FORBIDDEN,
        Json(json!({"error":"remote admission unavailable"})),
    )
        .into_response()
}

async fn health(State(connection): State<Arc<Connection>>) -> Response {
    // Certificate revalidation and the store read block; keep them off the
    // Tokio worker.
    match super::rpc::blocking(move || health_snapshot(&connection)).await {
        Ok(response) => response,
        Err(_) => admission_unavailable(),
    }
}

fn health_snapshot(connection: &Connection) -> Response {
    if connection.current().is_err() {
        return admission_unavailable();
    }
    let Some(admission) = connection.server.verified_work_claim_connection() else {
        return admission_unavailable();
    };
    let execution_label = match connection.config.execution_mode {
        super::config::DestinationExecutionMode::Execute => "enabled",
        super::config::DestinationExecutionMode::RefusalOnly
        | super::config::DestinationExecutionMode::AdmitOnly => "unavailable",
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
            "execution":execution_label
        })),
    )
        .into_response()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn gateway_home_has_at_most_one_supervisor() {
        let home = tempfile::tempdir().unwrap();
        let lock = acquire_gateway_supervisor_lock(home.path()).unwrap();
        assert_eq!(
            acquire_gateway_supervisor_lock(home.path())
                .unwrap_err()
                .kind(),
            ErrorKind::WouldBlock
        );
        drop(lock);
        assert!(acquire_gateway_supervisor_lock(home.path()).is_ok());
    }

    #[test]
    fn a_peer_that_closes_before_accept_does_not_pause_the_listener() {
        for kind in [
            ErrorKind::ConnectionRefused,
            ErrorKind::ConnectionAborted,
            ErrorKind::ConnectionReset,
        ] {
            assert_eq!(
                accept_error_policy(&std::io::Error::from(kind), Duration::from_millis(200)),
                AfterAcceptError::PeerClosed,
                "{kind:?}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_listener_resource_shortage_pauses_and_retries_with_a_bounded_backoff() {
        let shortage = std::io::Error::from_raw_os_error(libc::EMFILE);
        // Consecutive failures, driven the way the accept loop drives them.
        let mut backoff = Duration::ZERO;
        let mut pauses = Vec::new();
        for _ in 0..7 {
            match accept_error_policy(&shortage, next_backoff(backoff)) {
                AfterAcceptError::RetryAfter(pause) => {
                    pauses.push(pause);
                    backoff = pause;
                }
                AfterAcceptError::PeerClosed => panic!("the listener is short of descriptors"),
            }
        }
        let ms = |millis: u64| Duration::from_millis(millis);
        assert_eq!(
            pauses,
            [
                ms(50),
                ms(100),
                ms(200),
                ms(400),
                ms(800),
                ms(1000),
                ms(1000)
            ]
        );
    }

    #[test]
    fn events_before_admission_are_written_at_most_twenty_per_second() {
        let start = Instant::now();
        let mut budget = EventBudget::new(start);
        for _ in 0..EVENTS_PER_WINDOW {
            assert_eq!(budget.allow(start), Some(0));
        }
        // A peer that was never admitted keeps failing: nothing more is
        // written in this window, and the dropped count is reported once.
        assert_eq!(budget.allow(start), None);
        budget.suppress();
        budget.suppress();
        assert_eq!(budget.allow(start), None);
        budget.suppress();
        assert_eq!(budget.allow(start + EVENT_WINDOW), Some(3));
        assert_eq!(budget.allow(start + EVENT_WINDOW), Some(0));
    }

    #[tokio::test]
    async fn an_unfinished_handshake_never_holds_an_established_slot() {
        let slots = AdmissionSlots::new();
        let pending: Vec<_> = (0..MAX_HANDSHAKES)
            .map(|_| {
                slots
                    .handshakes
                    .clone()
                    .try_acquire_owned()
                    .expect("handshake slot")
            })
            .collect();
        assert!(
            slots.handshakes.clone().try_acquire_owned().is_err(),
            "the handshake stage is bounded"
        );
        assert_eq!(
            slots.established.available_permits(),
            MAX_CONNECTIONS,
            "peers that have not completed TLS occupy no established slot"
        );
        let admitted: Vec<_> = (0..MAX_CONNECTIONS)
            .map(|_| {
                slots
                    .established
                    .clone()
                    .try_acquire_owned()
                    .expect("established slot")
            })
            .collect();
        assert!(
            slots.established.clone().try_acquire_owned().is_err(),
            "the established stage is bounded"
        );
        drop(pending);
        drop(admitted);
    }

    #[test]
    fn a_logged_key_is_only_a_fingerprint() {
        assert_eq!(key_fingerprint(&"ab".repeat(32)), "abababababababab");
        assert_eq!(key_fingerprint("short"), "short");
        assert_eq!(key_fingerprint(""), "");
    }
}
