#![cfg(unix)]

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

/// Packaged doctor process tests spawn real binaries that open SQLite and load
/// extensions. Running them concurrently in one file races extension init and
/// produces false `provider_health_persist_sqlite_deadline` failures on the
/// success path. Serialize only this process-test file — not the suite.
fn process_test_lock() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

struct MockProvider {
    endpoint: String,
    stop: Arc<AtomicBool>,
    /// Accepted HTTP connections — proves packaged doctor probe traffic started.
    requests: Arc<AtomicUsize>,
    /// One `"{target} {bearer} {model}"` entry per request whose headers were
    /// parsed.
    seen: Arc<(Mutex<Vec<String>>, Condvar)>,
    ordering_failed: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl MockProvider {
    fn unauthorized() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock provider");
        listener
            .set_nonblocking(true)
            .expect("make mock provider nonblocking");
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let stop = Arc::new(AtomicBool::new(false));
        let requests = Arc::new(AtomicUsize::new(0));
        let seen = Arc::new((Mutex::new(Vec::new()), Condvar::new()));
        let ordering_failed = Arc::new(AtomicBool::new(false));
        let thread_stop = stop.clone();
        let thread_requests = requests.clone();
        let thread_seen = seen.clone();
        let thread_ordering_failed = ordering_failed.clone();
        let thread = std::thread::spawn(move || {
            while !thread_stop.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        thread_requests.fetch_add(1, Ordering::Release);
                        let seen = thread_seen.clone();
                        let ordering_failed = thread_ordering_failed.clone();
                        std::thread::spawn(move || {
                            respond_unauthorized(stream, seen, ordering_failed)
                        });
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    Err(_) => break,
                }
            }
        });
        Self {
            endpoint,
            stop,
            requests,
            seen,
            ordering_failed,
            thread: Some(thread),
        }
    }

    fn request_count(&self) -> usize {
        self.requests.load(Ordering::Acquire)
    }

    /// Sorted `"{target} {bearer} {model}"` identity of every parsed request,
    /// so a probe that never reached the provider, a duplicated attempt, or two
    /// lanes that collided on one key are all distinguishable — the accepted
    /// connection count alone is not. Waits briefly for the per-connection
    /// readers to catch up with the accept counter so callers compare complete
    /// evidence rather than a mid-flight snapshot.
    fn request_identities(&self) -> Vec<String> {
        assert!(
            !self.ordering_failed.load(Ordering::Acquire),
            "extract fallback did not reach the mock before the distill gate deadline"
        );
        let started = Instant::now();
        let mut identities = self.recorded();
        while identities.len() < self.request_count() && started.elapsed() < Duration::from_secs(5)
        {
            std::thread::sleep(Duration::from_millis(25));
            identities = self.recorded();
        }
        identities.sort();
        identities
    }

    fn recorded(&self) -> Vec<String> {
        self.seen
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// Wait until the request counter is stable for `quiet` (probes settled).
    fn wait_until_requests_settle(&self, quiet: Duration, deadline: Duration) {
        let started = Instant::now();
        let mut last = self.request_count();
        let mut quiet_since = Instant::now();
        loop {
            let now = self.request_count();
            if now != last {
                last = now;
                quiet_since = Instant::now();
            } else if quiet_since.elapsed() >= quiet {
                return;
            }
            if started.elapsed() >= deadline {
                panic!(
                    "mock provider requests did not settle (count={last}) within {} ms",
                    deadline.as_millis()
                );
            }
            std::thread::sleep(Duration::from_millis(25));
        }
    }
}

fn respond_unauthorized(
    mut stream: TcpStream,
    seen: Arc<(Mutex<Vec<String>>, Condvar)>,
    ordering_failed: Arc<AtomicBool>,
) {
    stream.set_read_timeout(Some(Duration::from_secs(2))).ok();
    let mut request = Vec::new();
    let mut chunk = [0_u8; 4096];
    loop {
        let Ok(read) = stream.read(&mut chunk) else {
            return;
        };
        if read == 0 {
            return;
        }
        request.extend_from_slice(&chunk[..read]);
        let Some(header_end) = request.windows(4).position(|bytes| bytes == b"\r\n\r\n") else {
            continue;
        };
        let headers = String::from_utf8_lossy(&request[..header_end]);
        let content_length = headers
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse::<usize>().ok())
                    .flatten()
            })
            .unwrap_or(0);
        if request.len() >= header_end + 4 + content_length {
            break;
        }
    }
    let identity = record_identity(&request, &seen);
    if identity.split_whitespace().nth(1) == Some("local-mock-distill-key") {
        // EXTRACT_API_KEY is also a distill rotation candidate. Observing its
        // fallback request proves extract has already handled the primary 401,
        // so distill cannot race it for that key. This orders only this fixture.
        let extract_fallback = LOCAL_ROLES
            .iter()
            .find(|role| role.role == "chat_extract_fallback")
            .expect("declared extract fallback");
        let expected = format!(
            "{} {} {}",
            extract_fallback.target, extract_fallback.key, extract_fallback.model
        );
        let observed = seen.0.lock().unwrap_or_else(|error| error.into_inner());
        let (observed, _) = seen
            .1
            .wait_timeout_while(observed, Duration::from_secs(5), |requests| {
                !requests.contains(&expected)
            })
            .unwrap_or_else(|error| error.into_inner());
        if !observed.contains(&expected) {
            ordering_failed.store(true, Ordering::Release);
            return;
        }
    }
    let _ = stream.write_all(
        b"HTTP/1.1 401 Unauthorized\r\nContent-Type: application/json\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}",
    );
    let _ = stream.flush();
}

/// Records one request as `"{target} {bearer} {model}"` — the identity the
/// packaged-doctor test compares against its declared role matrix.
fn record_identity(request: &[u8], seen: &(Mutex<Vec<String>>, Condvar)) -> String {
    let text = String::from_utf8_lossy(request).to_string();
    let (headers, body) = text.split_once("\r\n\r\n").unwrap_or((text.as_str(), ""));
    let target = headers
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .unwrap_or("<no-target>");
    let bearer = headers
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("authorization").then(|| {
                let value = value.trim();
                value
                    .strip_prefix("Bearer")
                    .or_else(|| value.strip_prefix("bearer"))
                    .unwrap_or(value)
                    .trim()
                    .to_string()
            })
        })
        .unwrap_or_else(|| "<no-authorization>".to_string());
    let model = body
        .split_once("\"model\":\"")
        .and_then(|(_, rest)| rest.split_once('"'))
        .map(|(model, _)| model.to_string())
        .unwrap_or_else(|| "<no-model>".to_string());
    let identity = format!("{target} {bearer} {model}");
    seen.0
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .push(identity.clone());
    seen.1.notify_all();
    identity
}

impl Drop for MockProvider {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            thread.join().expect("join mock provider");
        }
    }
}

struct ProcessOutput {
    status: std::process::ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

fn wait_with_deadline(mut child: std::process::Child, deadline: Duration) -> ProcessOutput {
    let mut stdout = child.stdout.take().expect("piped stdout");
    let mut stderr = child.stderr.take().expect("piped stderr");
    let stdout_reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        stdout.read_to_end(&mut bytes).expect("read child stdout");
        bytes
    });
    let stderr_reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        stderr.read_to_end(&mut bytes).expect("read child stderr");
        bytes
    });

    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().expect("poll packaged doctor") {
            break status;
        }
        if started.elapsed() >= deadline {
            child.kill().expect("kill hung packaged doctor");
            let _ = child.wait();
            let stdout = stdout_reader.join().expect("join stdout reader");
            let stderr = stderr_reader.join().expect("join stderr reader");
            panic!(
                "packaged doctor did not terminate within {} ms; stdout={}; stderr={}",
                deadline.as_millis(),
                String::from_utf8_lossy(&stdout),
                String::from_utf8_lossy(&stderr)
            );
        }
        std::thread::sleep(Duration::from_millis(25));
    };

    ProcessOutput {
        status,
        stdout: stdout_reader.join().expect("join stdout reader"),
        stderr: stderr_reader.join().expect("join stderr reader"),
    }
}

/// One provider role the packaged doctor probes, with its own credential,
/// model and mock endpoint.
struct LocalRole {
    /// Role name for failure messages.
    role: &'static str,
    /// Credential env var whose 401 this role owns.
    key_env: &'static str,
    key: &'static str,
    /// Env var pinning the role's model to the local mock deployment, or
    /// `None` where the client compiles the model in (Voyage rerank).
    model_env: Option<&'static str>,
    model: &'static str,
    /// Env var carrying this role's endpoint.
    base_url_env: &'static str,
    /// Request target the mock must see for this role.
    target: &'static str,
}

/// Declared local role matrix for the blocked-writer doctor test. The probe
/// set is exactly `status_ops::status_health::probes::run_provider_probe_report`:
/// the embed and rerank lanes, plus the extract and distill chat lanes, each
/// walking its primary tier and then its configured fallback tier. Every role
/// owns one key, one model and the same mock endpoint, so the mock's request
/// identities are one row per role — a shared key, a re-selected key, a lane
/// that never reached the provider, or an off-mock endpoint all change the
/// set. `REASONING_API_KEY` is the distill lane's second rotation member: it
/// is served with the distill lane's own model. `ZAI_API_KEY` and the other
/// provider-named members are deliberately absent: with an operator-explicit
/// lane base URL they cannot serve this endpoint, so they are not roles.
const LOCAL_ROLES: &[LocalRole] = &[
    LocalRole {
        role: "voyage_embed",
        key_env: "VOYAGE_API_KEY",
        key: "local-mock-voyage-embed-key",
        model_env: Some("TACHI_EMBEDDING_MODEL"),
        model: "local-mock-voyage-embed-model",
        base_url_env: "VOYAGE_BASE_URL",
        target: "/v1/embeddings",
    },
    LocalRole {
        role: "voyage_rerank",
        key_env: "VOYAGE_RERANK_API_KEY",
        key: "local-mock-voyage-rerank-key",
        model_env: Some("VOYAGE_RERANK_MODEL"),
        model: "local-mock-voyage-rerank-model",
        base_url_env: "TACHI_RERANK_VOYAGE_ENDPOINT",
        target: "/v1/rerank",
    },
    LocalRole {
        role: "chat_extract_primary",
        key_env: "EXTRACT_API_KEY",
        key: "local-mock-extract-key",
        model_env: Some("EXTRACT_MODEL"),
        model: "local-mock-extract-model",
        base_url_env: "EXTRACT_BASE_URL",
        target: "/v1/chat/completions",
    },
    LocalRole {
        role: "chat_extract_fallback",
        key_env: "EXTRACT_FALLBACK_API_KEY",
        key: "local-mock-extract-fallback-key",
        model_env: Some("EXTRACT_FALLBACK_MODEL"),
        model: "local-mock-extract-fallback-model",
        base_url_env: "EXTRACT_FALLBACK_BASE_URL",
        target: "/v1/chat/completions",
    },
    LocalRole {
        role: "chat_distill_primary",
        key_env: "DISTILL_API_KEY",
        key: "local-mock-distill-key",
        model_env: Some("DISTILL_MODEL"),
        model: "local-mock-distill-model",
        base_url_env: "DISTILL_BASE_URL",
        target: "/v1/chat/completions",
    },
    LocalRole {
        role: "chat_distill_rotation",
        key_env: "REASONING_API_KEY",
        key: "local-mock-distill-rotation-key",
        model_env: Some("DISTILL_MODEL"),
        model: "local-mock-distill-model",
        // The rotation member is served by the distill lane's own endpoint and
        // model, which is what makes it a distinct key rather than a new lane.
        base_url_env: "REASONING_BASE_URL",
        target: "/v1/chat/completions",
    },
    LocalRole {
        role: "chat_distill_fallback",
        key_env: "DISTILL_FALLBACK_API_KEY",
        key: "local-mock-distill-fallback-key",
        model_env: Some("DISTILL_FALLBACK_MODEL"),
        model: "local-mock-distill-fallback-model",
        base_url_env: "DISTILL_FALLBACK_BASE_URL",
        target: "/v1/chat/completions",
    },
];

/// The matrix's own expected request identities, sorted like
/// [`MockProvider::request_identities`].
fn expected_local_role_requests() -> Vec<String> {
    let mut expected: Vec<String> = LOCAL_ROLES
        .iter()
        .map(|role| format!("{} {} {}", role.target, role.key, role.model))
        .collect();
    expected.sort();
    expected
}

#[test]
fn packaged_doctor_joins_blocked_provider_writer_and_emits_one_terminal_json_document() {
    let _guard = process_test_lock();
    let temp = tempfile::tempdir().expect("temporary packaged-doctor home");
    let home = temp.path().join("home");
    let app_home = temp.path().join("tachi-home");
    let global_db = app_home.join("global").join(memcore::MEMORY_DB_FILENAME);
    std::fs::create_dir_all(&home).expect("create isolated HOME");
    std::fs::create_dir_all(global_db.parent().unwrap()).expect("create global DB parent");

    let store = memcore::MemoryStore::open(global_db.to_str().unwrap()).expect("seed global DB");
    drop(store);
    let lock_owner = rusqlite::Connection::open(&global_db).expect("open external lock owner");
    lock_owner
        .execute_batch("BEGIN IMMEDIATE")
        .expect("hold provider-health writer lock");

    // The lock lives on an owned thread so the parent can release it only after
    // the actual public join deadline fires, rather than estimating its start
    // from HTTP traffic. This avoids waiting for every serialized provider writer
    // to pay its own two-second SQLite busy deadline after that fact is proven.
    let (release_lock, release_lock_rx) = std::sync::mpsc::sync_channel(0);
    let lock_owner_thread = std::thread::spawn(move || {
        release_lock_rx
            .recv_timeout(Duration::from_secs(60))
            .expect("parent releases external lock owner");
        lock_owner
            .execute_batch("ROLLBACK")
            .expect("release test lock");
    });

    let provider = MockProvider::unauthorized();
    let chat_endpoint = format!("{}/v1/chat/completions", provider.endpoint);
    let rerank_endpoint = format!("{}/v1/rerank", provider.endpoint);
    // Every 401 belongs to exactly one declared role (see `LOCAL_ROLES`), and
    // every primary/fallback URL plus every model stays on this local mock.
    let mut child = Command::new(env!("CARGO_BIN_EXE_tachi-server"))
        .args([
            "--global-db",
            global_db.to_str().unwrap(),
            "--no-project-db",
            "doctor",
            "--json",
            "--run-daily",
        ])
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", &home)
        .env("TACHI_HOME", &app_home)
        .env(
            "RUST_LOG",
            "off,tachi_server::status_ops::status_health::probes=warn",
        )
        .env("VOYAGE_BASE_URL", &provider.endpoint)
        .env("TACHI_RERANK_VOYAGE_ENDPOINT", &rerank_endpoint)
        // An embedding-model override must declare its vector width.
        .env("TACHI_EMBEDDING_DIM", "1024")
        .env("SUMMARY_API_KEY", "local-mock-summary-key")
        .env("SUMMARY_BASE_URL", &chat_endpoint)
        .env("SUMMARY_MODEL", "local-mock-summary-model")
        .envs(LOCAL_ROLES.iter().flat_map(|role| {
            let mut pairs: Vec<(String, String)> = vec![
                (role.key_env.to_string(), role.key.to_string()),
                (
                    role.base_url_env.to_string(),
                    // The embeddings client appends its own route to the
                    // provider base URL; every other role posts to `target`.
                    if role.target == "/v1/embeddings" {
                        provider.endpoint.clone()
                    } else {
                        format!("{}{}", provider.endpoint, role.target)
                    },
                ),
            ];
            if let Some(model_env) = role.model_env {
                pairs.push((model_env.to_string(), role.model.to_string()));
            }
            pairs
        }))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn packaged doctor");

    // HTTP quiet does not prove the persistence join started. Keep the lock
    // until the real timeout branch reports that it is draining the writer.
    // The diagnostic goes to the isolated app-home log, never JSON stdout.
    let log_path = app_home.join("logs/tachi.log");
    let join_deadline = Instant::now() + Duration::from_secs(45);
    let join_timeout_observed = loop {
        let log = std::fs::read_to_string(&log_path).unwrap_or_default();
        if log.contains("provider_health_persist_join_timeout")
            && log.contains("waiting for writer completion")
        {
            break true;
        }
        if child
            .try_wait()
            .expect("poll doctor before writer release")
            .is_some()
            || Instant::now() >= join_deadline
        {
            break false;
        }
        std::thread::sleep(Duration::from_millis(25));
    };
    release_lock
        .send(())
        .expect("signal external lock owner release");
    let output = wait_with_deadline(child, Duration::from_secs(35));

    assert!(
        join_timeout_observed,
        "doctor must report its actual join timeout before lock release; requests={}; log={}; stdout={}; stderr={}",
        provider.request_count(),
        std::fs::read_to_string(&log_path).unwrap_or_default(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        provider.request_identities(),
        expected_local_role_requests(),
        "every declared local role must reach the local provider exactly once; declared roles={:?}",
        LOCAL_ROLES.iter().map(|role| role.role).collect::<Vec<_>>(),
    );
    assert!(
        output.status.success(),
        "packaged doctor failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let document: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "stdout must be exactly one valid terminal JSON document: {error}; stdout={}; stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    });
    let remediation = document["daily_remediation"]
        .as_str()
        .expect("doctor JSON carries daily remediation receipt");
    assert!(
        remediation.contains("provider_health_persist status=timeout"),
        "remediation={remediation}"
    );
    assert!(
        remediation.contains("cause=provider_health_persist_join_timeout"),
        "remediation={remediation}"
    );
    assert!(
        remediation.contains("writer_joined=true"),
        "remediation={remediation}"
    );
    assert!(
        remediation.contains("second writer forbidden"),
        "remediation={remediation}"
    );

    lock_owner_thread.join().expect("join external lock owner");

    let release_probe = rusqlite::Connection::open(&global_db).expect("open release probe");
    release_probe
        .execute_batch("BEGIN IMMEDIATE; ROLLBACK;")
        .expect("packaged doctor left no provider writer or SQLite lock alive after exit");
}

fn seed_named_project_distill_candidates(app_home: &std::path::Path, project: &str) {
    let project_db = app_home
        .join("projects")
        .join(project)
        .join(memcore::MEMORY_DB_FILENAME);
    std::fs::create_dir_all(project_db.parent().unwrap()).expect("create named project dir");
    let mut store =
        memcore::MemoryStore::open(project_db.to_str().unwrap()).expect("open named project DB");
    for idx in 0..3 {
        let entry = memcore::MemoryEntry {
            id: format!("candidate-{idx}"),
            path: format!("/project/bounded/{idx}"),
            summary: format!("bounded scan candidate {idx}"),
            text: format!("bounded scan candidate memory {idx}"),
            importance: 0.7,
            timestamp: format!("2026-01-01T00:00:{idx:02}Z"),
            valid_from: String::new(),
            valid_until: None,
            category: "fact".to_string(),
            topic: "bounded-scan".to_string(),
            keywords: vec!["bounded".to_string()],
            persons: Vec::new(),
            entities: vec!["bounded-scan".to_string()],
            location: String::new(),
            source: "manual".to_string(),
            scope: "project".to_string(),
            archived: false,
            access_count: 0,
            scored_count: 0,
            last_access: None,
            last_use_at: None,
            revision: 1,
            metadata: serde_json::json!({}),
            vector: None,
            retention_policy: None,
            domain: None,
            recall_count: 0,
            query_diversity: 0,
            tier: "raw".to_string(),
        };
        store.upsert(&entry).expect("seed distill candidate");
    }
}

fn spawn_packaged_doctor(
    global_db: &std::path::Path,
    home: &std::path::Path,
    app_home: &std::path::Path,
    provider: &MockProvider,
) -> std::process::Child {
    let chat_endpoint = format!("{}/v1/chat/completions", provider.endpoint);
    let rerank_endpoint = format!("{}/v1/rerank", provider.endpoint);
    Command::new(env!("CARGO_BIN_EXE_tachi-server"))
        .args([
            "--global-db",
            global_db.to_str().unwrap(),
            "--no-project-db",
            "doctor",
            "--json",
            "--run-daily",
        ])
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", home)
        .env("TACHI_HOME", app_home)
        .env("RUST_LOG", "off")
        .env("VOYAGE_API_KEY", "test-only-voyage-key")
        .env("VOYAGE_BASE_URL", &provider.endpoint)
        .env("TACHI_RERANK_VOYAGE_ENDPOINT", &rerank_endpoint)
        .env("EXTRACT_API_KEY", "test-only-extract-key")
        .env("SUMMARY_API_KEY", "test-only-summary-key")
        .env("DISTILL_API_KEY", "test-only-distill-key")
        .env("REASONING_API_KEY", "test-only-reasoning-key")
        .env("EXTRACT_BASE_URL", &chat_endpoint)
        .env("SUMMARY_BASE_URL", &chat_endpoint)
        .env("DISTILL_BASE_URL", &chat_endpoint)
        .env("REASONING_BASE_URL", &chat_endpoint)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn packaged doctor")
}

fn assert_db_reopenable(global_db: &std::path::Path) {
    let release_probe = rusqlite::Connection::open(global_db).expect("open release probe");
    release_probe
        .execute_batch("BEGIN IMMEDIATE; ROLLBACK;")
        .expect("packaged doctor left no provider writer or SQLite lock alive after exit");
}

fn parse_one_terminal_json(stdout: &[u8], stderr: &[u8]) -> serde_json::Value {
    serde_json::from_slice(stdout).unwrap_or_else(|error| {
        panic!(
            "stdout must be exactly one valid terminal JSON document: {error}; stdout={}; stderr={}",
            String::from_utf8_lossy(stdout),
            String::from_utf8_lossy(stderr)
        )
    })
}

#[test]
fn packaged_doctor_run_daily_success_completes_persist_and_distill_phases() {
    let _guard = process_test_lock();
    let provider = MockProvider::unauthorized();
    let temp = tempfile::tempdir().expect("temporary packaged-doctor home");
    let home = temp.path().join("home");
    let app_home = temp.path().join("tachi-home");
    let global_db = app_home.join("global").join(memcore::MEMORY_DB_FILENAME);
    std::fs::create_dir_all(&home).expect("create isolated HOME");
    std::fs::create_dir_all(global_db.parent().unwrap()).expect("create global DB parent");
    let store = memcore::MemoryStore::open(global_db.to_str().unwrap()).expect("seed global DB");
    drop(store);
    let output = wait_with_deadline(
        spawn_packaged_doctor(&global_db, &home, &app_home, &provider),
        Duration::from_secs(35),
    );

    assert!(
        output.status.success(),
        "packaged doctor success path must exit 0: stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    let document = parse_one_terminal_json(&output.stdout, &output.stderr);
    let remediation = document["daily_remediation"]
        .as_str()
        .expect("doctor JSON carries daily remediation receipt");
    assert!(
        remediation.contains("provider_health_persist") && remediation.contains("status=ok"),
        "success path must complete provider-persistence phase: {remediation}"
    );
    assert!(
        remediation.contains("distill:") && remediation.contains("errors=0"),
        "success path must complete distill phase cleanly: {remediation}"
    );

    let marker_path = app_home.join("foundry-runs").join(".last_distill_run");
    let marker: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(&marker_path).expect("success distill marker must exist"),
    )
    .expect("success distill marker must be JSON");
    assert!(
        marker.get("error").is_none(),
        "success marker must not carry error field: {marker}"
    );
    assert_eq!(
        marker["errors"], 0,
        "success marker errors must be 0: {marker}"
    );
    assert!(
        marker.get("ts").and_then(|v| v.as_str()).is_some(),
        "success marker must carry ts: {marker}"
    );

    assert_db_reopenable(&global_db);
}

#[test]
fn packaged_doctor_run_daily_distill_failure_surfaces_typed_cause_without_success_artifact() {
    let _guard = process_test_lock();
    let provider = MockProvider::unauthorized();
    let temp = tempfile::tempdir().expect("temporary packaged-doctor home");
    let home = temp.path().join("home");
    let app_home = temp.path().join("tachi-home");
    let global_db = app_home.join("global").join(memcore::MEMORY_DB_FILENAME);
    std::fs::create_dir_all(&home).expect("create isolated HOME");
    std::fs::create_dir_all(global_db.parent().unwrap()).expect("create global DB parent");
    let store = memcore::MemoryStore::open(global_db.to_str().unwrap()).expect("seed global DB");
    drop(store);
    seed_named_project_distill_candidates(&app_home, "fixture-distill");
    let output = wait_with_deadline(
        spawn_packaged_doctor(&global_db, &home, &app_home, &provider),
        Duration::from_secs(45),
    );

    assert!(
        !output.status.success(),
        "distill failure must exit non-zero; stdout={}; stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let document = parse_one_terminal_json(&output.stdout, &output.stderr);
    let remediation = document["daily_remediation"]
        .as_str()
        .expect("doctor JSON carries daily remediation receipt");
    assert!(
        remediation.contains("provider_health_persist") && remediation.contains("status=ok"),
        "distill failure path still requires persist join success first: {remediation}"
    );
    assert!(
        remediation.contains("distill failed:") || remediation.contains("distill batch failed:"),
        "terminal receipt must preserve typed distill failure cause: {remediation}"
    );
    assert!(
        remediation.contains("cause=")
            || remediation.contains("401")
            || remediation.contains("api"),
        "typed cause must remain visible in remediation: {remediation}"
    );

    let marker_path = app_home.join("foundry-runs").join(".last_distill_run");
    match std::fs::read_to_string(&marker_path) {
        Ok(raw) => {
            let marker: serde_json::Value =
                serde_json::from_str(&raw).expect("failure marker must be JSON when present");
            assert!(
                marker
                    .get("error")
                    .and_then(|v| v.as_str())
                    .is_some_and(|e| !e.is_empty()),
                "failure marker must be error-shaped, not clean success: {marker}"
            );
            // When absorbed errors>0, durable marker counters must match the
            // terminal summary (cold-review prescription #1).
            if remediation.contains("distill failed:") {
                let summary_errors = parse_remediation_counter(remediation, "errors=");
                let summary_distilled = parse_remediation_counter(remediation, "distilled=");
                let summary_skipped = parse_remediation_counter(remediation, "skipped=");
                let summary_fallback = parse_remediation_counter(remediation, "fallback=");
                assert!(
                    summary_errors > 0,
                    "absorbed-error path must report errors>0: {remediation}"
                );
                assert_eq!(
                    marker["errors"].as_u64().expect("marker errors u64"),
                    summary_errors,
                    "marker errors must match terminal summary: marker={marker}; remediation={remediation}"
                );
                assert_eq!(
                    marker["groups_distilled"].as_u64().expect("marker distilled"),
                    summary_distilled,
                    "marker groups_distilled must match summary: marker={marker}; remediation={remediation}"
                );
                assert_eq!(
                    marker["groups_skipped"].as_u64().expect("marker skipped"),
                    summary_skipped,
                    "marker groups_skipped must match summary: marker={marker}; remediation={remediation}"
                );
                assert_eq!(
                    marker["fallback_used"].as_u64().expect("marker fallback"),
                    summary_fallback,
                    "marker fallback_used must match summary: marker={marker}; remediation={remediation}"
                );
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            // Absent marker is also acceptable (no clean success artifact).
        }
        Err(error) => panic!("unexpected marker read failure: {error}"),
    }

    assert_db_reopenable(&global_db);
}

fn parse_remediation_counter(remediation: &str, key: &str) -> u64 {
    remediation
        .split_whitespace()
        .find_map(|token| {
            let rest = token.strip_prefix(key)?;
            // Counters may be glued to the next field via cause=…; take digits only.
            let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
            digits.parse().ok()
        })
        .unwrap_or_else(|| panic!("missing {key} counter in remediation: {remediation}"))
}

#[test]
fn packaged_doctor_run_daily_cancellation_releases_db_without_success_claim() {
    let _guard = process_test_lock();
    let temp = tempfile::tempdir().expect("temporary packaged-doctor home");
    let home = temp.path().join("home");
    let app_home = temp.path().join("tachi-home");
    let global_db = app_home.join("global").join(memcore::MEMORY_DB_FILENAME);
    std::fs::create_dir_all(&home).expect("create isolated HOME");
    std::fs::create_dir_all(global_db.parent().unwrap()).expect("create global DB parent");

    let store = memcore::MemoryStore::open(global_db.to_str().unwrap()).expect("seed global DB");
    drop(store);
    // Hold BEGIN IMMEDIATE so provider-health persist blocks after probes.
    let lock_owner = rusqlite::Connection::open(&global_db).expect("open external lock owner");
    lock_owner
        .execute_batch("BEGIN IMMEDIATE")
        .expect("hold provider-health writer lock");

    let provider = MockProvider::unauthorized();
    let mut child = spawn_packaged_doctor(&global_db, &home, &app_home, &provider);

    // Prove the child entered the bounded blocked provider-persistence path
    // before SIGTERM:
    // 1) MockProvider request latch — probes have started (HTTP accepted).
    // 2) Request counter quiet — probes settled.
    // 3) Brief settle — persist has had time to contend on BEGIN IMMEDIATE.
    // Startup under a held writer lock can take >15s (same budget as the
    // blocked-writer process test), so the latch deadline is 30s.
    let latch_deadline = Duration::from_secs(30);
    let latch_started = Instant::now();
    while provider.request_count() < 1 {
        if let Some(status) = child.try_wait().expect("poll child during latch") {
            let mut stdout = Vec::new();
            let mut stderr = Vec::new();
            if let Some(mut out) = child.stdout.take() {
                let _ = out.read_to_end(&mut stdout);
            }
            if let Some(mut err) = child.stderr.take() {
                let _ = err.read_to_end(&mut stderr);
            }
            panic!(
                "packaged doctor exited before any mock provider probe; status={status}; requests={}; stdout={}; stderr={}",
                provider.request_count(),
                String::from_utf8_lossy(&stdout),
                String::from_utf8_lossy(&stderr)
            );
        }
        if latch_started.elapsed() >= latch_deadline {
            let _ = child.kill();
            let output = wait_with_deadline(child, Duration::from_secs(5));
            panic!(
                "mock provider saw {} requests within {} ms; child still running until kill; stdout={}; stderr={}",
                provider.request_count(),
                latch_deadline.as_millis(),
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    provider.wait_until_requests_settle(Duration::from_millis(250), Duration::from_secs(20));
    // After probes settle, persist opens the DB (loads SQLite extensions) then
    // contends on BEGIN IMMEDIATE. Wait past persist's 2s sqlite busy budget
    // start so SIGTERM lands in the busy-wait, not mid-extension-init.
    std::thread::sleep(Duration::from_millis(2500));
    assert!(
        provider.request_count() >= 1,
        "cancellation must fire only after probe traffic proved blocked-phase entry"
    );

    let pid = child.id();
    let kill_status = Command::new("kill")
        .args(["-TERM", &pid.to_string()])
        .status()
        .expect("send SIGTERM to packaged doctor");
    assert!(kill_status.success(), "kill -TERM must succeed");

    let output = wait_with_deadline(child, Duration::from_secs(20));
    assert!(
        !output.status.success(),
        "cancelled packaged doctor must not report success; stdout={}; stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    // Hold the file lock after reaping so a SIGTERM'd child's extension
    // teardown does not overlap the next packaged binary's libsimple
    // registration (host-global SQLite auto-extension race).
    std::thread::sleep(Duration::from_secs(2));

    // Non-empty stdout must be exactly one valid JSON document — never skip
    // on parse failure. Empty stdout is OK only for abrupt kills that emit
    // nothing; success claim is still forbidden via exit status + marker.
    // When stdout is non-empty, `daily_remediation` must actually be present
    // (no unwrap_or("") fallback — a syntactically valid but empty/irrelevant
    // `{}` must fail) and must carry an explicit non-success disposition for
    // the provider/daily phase, not just parse as JSON.
    if !output.stdout.is_empty() {
        let document = parse_one_terminal_json(&output.stdout, &output.stderr);
        let remediation = document
            .get("daily_remediation")
            .and_then(|v| v.as_str())
            .unwrap_or_else(|| {
                panic!(
                    "non-empty stdout after cancellation must carry a daily_remediation string: document={document}"
                )
            });
        assert!(
            remediation.contains("status=timeout")
                || remediation.contains("distill failed")
                || remediation.contains("distill batch failed")
                || remediation.contains("distill skipped"),
            "cancelled packaged doctor terminal JSON must carry a non-success provider/daily disposition (timeout/distill failed/distill skipped), not merely be well-formed JSON: {remediation}"
        );
    }

    let marker_path = app_home.join("foundry-runs").join(".last_distill_run");
    if let Ok(raw) = std::fs::read_to_string(&marker_path) {
        let marker: serde_json::Value =
            serde_json::from_str(&raw).expect("marker JSON when present");
        assert!(
            marker.get("error").is_some()
                || marker
                    .get("errors")
                    .and_then(|v| v.as_u64())
                    .is_some_and(|n| n > 0),
            "cancellation must not leave a clean success distill marker: {marker}"
        );
    }

    lock_owner
        .execute_batch("ROLLBACK")
        .expect("release test lock");
    assert_db_reopenable(&global_db);
}
