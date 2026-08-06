#![cfg(unix)]

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
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
        let thread_stop = stop.clone();
        let thread_requests = requests.clone();
        let thread = std::thread::spawn(move || {
            while !thread_stop.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        thread_requests.fetch_add(1, Ordering::Release);
                        std::thread::spawn(move || respond_unauthorized(stream));
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
            thread: Some(thread),
        }
    }

    fn request_count(&self) -> usize {
        self.requests.load(Ordering::Acquire)
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

fn respond_unauthorized(mut stream: TcpStream) {
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
    let _ = stream.write_all(
        b"HTTP/1.1 401 Unauthorized\r\nContent-Type: application/json\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}",
    );
    let _ = stream.flush();
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

    let provider = MockProvider::unauthorized();
    let chat_endpoint = format!("{}/v1/chat/completions", provider.endpoint);
    let rerank_endpoint = format!("{}/v1/rerank", provider.endpoint);
    let started = Instant::now();
    let child = Command::new(env!("CARGO_BIN_EXE_tachi-server"))
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
        .env("RUST_LOG", "off")
        .env("VOYAGE_API_KEY", "test-only-voyage-key")
        .env("VOYAGE_BASE_URL", &provider.endpoint)
        .env("TACHI_RERANK_VOYAGE_ENDPOINT", &rerank_endpoint)
        .env("EXTRACT_API_KEY", "test-only-extract-key")
        .env("DEEPSEEK_API_KEY", "test-only-deepseek-key")
        .env("EXTRACT_BASE_URL", &chat_endpoint)
        .env("SUMMARY_BASE_URL", &chat_endpoint)
        .env("DEEPSEEK_BASE_URL", &chat_endpoint)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn packaged doctor");
    let output = wait_with_deadline(child, Duration::from_secs(35));

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
    assert!(remediation.contains("provider_health_persist status=timeout"));
    assert!(remediation.contains("second writer forbidden"));

    // Keep the independent lock owner alive beyond the public 10-second join
    // deadline even when the writer's own two-second SQLite deadline lets the
    // packaged process exit earlier.
    let minimum_lock_hold = Duration::from_secs(11);
    if started.elapsed() < minimum_lock_hold {
        std::thread::sleep(minimum_lock_hold - started.elapsed());
    }
    lock_owner
        .execute_batch("ROLLBACK")
        .expect("release test lock");

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
        .env("DEEPSEEK_API_KEY", "test-only-deepseek-key")
        .env("EXTRACT_BASE_URL", &chat_endpoint)
        .env("SUMMARY_BASE_URL", &chat_endpoint)
        .env("DEEPSEEK_BASE_URL", &chat_endpoint)
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

fn is_sqlite_extension_load_flake(output: &ProcessOutput) -> bool {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    stdout.contains("automatic extension loading failed")
        || stderr.contains("automatic extension loading failed")
}

/// Retry a packaged-doctor attempt only on the known libsimple auto-extension
/// registration race. The closure should rebuild a fresh fixture each call.
fn with_extension_load_retries(mut run: impl FnMut() -> ProcessOutput) -> ProcessOutput {
    let mut last = run();
    for _ in 0..4 {
        if !is_sqlite_extension_load_flake(&last) {
            return last;
        }
        std::thread::sleep(Duration::from_millis(750));
        last = run();
    }
    last
}

#[test]
fn packaged_doctor_run_daily_success_completes_persist_and_distill_phases() {
    let _guard = process_test_lock();
    let provider = MockProvider::unauthorized();
    let mut kept: Option<(tempfile::TempDir, std::path::PathBuf, std::path::PathBuf)> = None;
    let output = with_extension_load_retries(|| {
        let temp = tempfile::tempdir().expect("temporary packaged-doctor home");
        let home = temp.path().join("home");
        let app_home = temp.path().join("tachi-home");
        let global_db = app_home.join("global").join(memcore::MEMORY_DB_FILENAME);
        std::fs::create_dir_all(&home).expect("create isolated HOME");
        std::fs::create_dir_all(global_db.parent().unwrap()).expect("create global DB parent");
        let store =
            memcore::MemoryStore::open(global_db.to_str().unwrap()).expect("seed global DB");
        drop(store);
        let output = wait_with_deadline(
            spawn_packaged_doctor(&global_db, &home, &app_home, &provider),
            Duration::from_secs(35),
        );
        if !is_sqlite_extension_load_flake(&output) {
            kept = Some((temp, app_home, global_db));
        }
        output
    });

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

    let (_temp, app_home, global_db) =
        kept.expect("accepted success attempt must retain fixture paths");
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
    let mut kept: Option<(tempfile::TempDir, std::path::PathBuf, std::path::PathBuf)> = None;
    let output = with_extension_load_retries(|| {
        let temp = tempfile::tempdir().expect("temporary packaged-doctor home");
        let home = temp.path().join("home");
        let app_home = temp.path().join("tachi-home");
        let global_db = app_home.join("global").join(memcore::MEMORY_DB_FILENAME);
        std::fs::create_dir_all(&home).expect("create isolated HOME");
        std::fs::create_dir_all(global_db.parent().unwrap()).expect("create global DB parent");
        let store =
            memcore::MemoryStore::open(global_db.to_str().unwrap()).expect("seed global DB");
        drop(store);
        seed_named_project_distill_candidates(&app_home, "fixture-distill");
        let output = wait_with_deadline(
            spawn_packaged_doctor(&global_db, &home, &app_home, &provider),
            Duration::from_secs(45),
        );
        if !is_sqlite_extension_load_flake(&output) {
            kept = Some((temp, app_home, global_db));
        }
        output
    });

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

    let (_temp, app_home, global_db) =
        kept.expect("accepted distill-failure attempt must retain fixture paths");
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
    if !output.stdout.is_empty() {
        let document = parse_one_terminal_json(&output.stdout, &output.stderr);
        let remediation = document
            .get("daily_remediation")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        assert!(
            !remediation.contains("errors=0")
                || remediation.contains("timeout")
                || remediation.contains("distill failed")
                || remediation.contains("distill skipped"),
            "emitted JSON must not claim clean distill success after cancel: {remediation}"
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
