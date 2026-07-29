#![cfg(unix)]

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

struct MockProvider {
    endpoint: String,
    stop: Arc<AtomicBool>,
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
        let thread_stop = stop.clone();
        let thread = std::thread::spawn(move || {
            while !thread_stop.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((stream, _)) => {
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
            thread: Some(thread),
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
