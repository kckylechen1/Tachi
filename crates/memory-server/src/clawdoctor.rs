use super::*;

/// Default health check interval: 5 minutes
pub const DEFAULT_CLAWDOCTOR_INTERVAL_SECS: u64 = 300;
/// How many consecutive failures before we consider OpenClaw dead
pub const DEFAULT_CLAWDOCTOR_FAIL_THRESHOLD: u32 = 3;
/// HTTP timeout for the health ping
const HEALTH_PING_TIMEOUT_MS: u64 = 5_000;
/// Cooldown after a restart before resuming checks (give it time to boot)
const RESTART_COOLDOWN_SECS: u64 = 60;

/// Run the clawdoctor background loop.
///
/// Periodically pings the OpenClaw gateway at `base_url`. If it fails
/// `fail_threshold` consecutive times, kills the gateway process and
/// restarts it via `gateway-wrapper.sh`.
///
/// Events are published to Ghost Whispers topic "clawdoctor" and saved
/// as memories for audit trail.
pub async fn run_clawdoctor(
    server: MemoryServer,
    base_url: String,
    interval_secs: u64,
    fail_threshold: u32,
) {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_millis(HEALTH_PING_TIMEOUT_MS))
        .build()
        .unwrap_or_default();

    let mut consecutive_failures: u32 = 0;
    let mut interval = tokio::time::interval(Duration::from_secs(interval_secs));
    // First tick fires immediately — skip it so we don't check before OpenClaw
    // has had time to boot.
    interval.tick().await;

    eprintln!(
        "[clawdoctor] started (url={}, interval={}s, threshold={})",
        base_url, interval_secs, fail_threshold
    );

    loop {
        interval.tick().await;

        let healthy = ping_openclaw(&client, &base_url).await;

        if healthy {
            if consecutive_failures > 0 {
                eprintln!(
                    "[clawdoctor] OpenClaw recovered (was {} consecutive failures)",
                    consecutive_failures
                );
                publish_event(
                    &server,
                    "recovered",
                    &format!(
                        "OpenClaw recovered after {} consecutive failures",
                        consecutive_failures
                    ),
                )
                .await;
            }
            consecutive_failures = 0;
        } else {
            consecutive_failures += 1;
            eprintln!(
                "[clawdoctor] OpenClaw health check failed ({}/{})",
                consecutive_failures, fail_threshold
            );

            if consecutive_failures >= fail_threshold {
                eprintln!("[clawdoctor] OpenClaw unresponsive — attempting restart...");
                publish_event(
                    &server,
                    "restart",
                    &format!(
                        "OpenClaw unresponsive after {} consecutive failures at {}. Attempting restart.",
                        consecutive_failures, base_url
                    ),
                )
                .await;

                save_incident_memory(
                    &server,
                    &format!(
                        "clawdoctor triggered restart: OpenClaw gateway at {} failed {} consecutive health checks",
                        base_url, consecutive_failures
                    ),
                )
                .await;

                match restart_openclaw().await {
                    Ok(output) => {
                        eprintln!("[clawdoctor] restart completed: {}", output.trim());
                        publish_event(
                            &server,
                            "restart_ok",
                            &format!("OpenClaw restart succeeded: {}", output.trim()),
                        )
                        .await;
                    }
                    Err(e) => {
                        eprintln!("[clawdoctor] restart FAILED: {}", e);
                        publish_event(
                            &server,
                            "restart_failed",
                            &format!("OpenClaw restart failed: {}", e),
                        )
                        .await;
                    }
                }

                consecutive_failures = 0;

                // Cooldown — give OpenClaw time to boot before next check
                eprintln!(
                    "[clawdoctor] cooldown {}s before resuming checks",
                    RESTART_COOLDOWN_SECS
                );
                tokio::time::sleep(Duration::from_secs(RESTART_COOLDOWN_SECS)).await;
            }
        }
    }
}

/// Ping the OpenClaw gateway. Returns true if it responds within timeout.
async fn ping_openclaw(client: &reqwest::Client, base_url: &str) -> bool {
    // Try the WebSocket endpoint — a simple GET that should return *something*
    // (even a 400/426 means the server is alive).
    // Also check if the process is listening on the port at all.
    match client.get(base_url).send().await {
        Ok(resp) => {
            // Any HTTP response means the process is alive (even 404/500).
            let status = resp.status();
            if !status.is_success() && status.as_u16() != 426 {
                // 426 = Upgrade Required (expected for WS endpoint hit via HTTP)
                eprintln!(
                    "[clawdoctor] ping got unexpected status {} (still alive)",
                    status
                );
            }
            true
        }
        Err(e) => {
            eprintln!("[clawdoctor] ping failed: {}", e);
            false
        }
    }
}

/// Restart OpenClaw by running `openclaw gateway restart`.
async fn restart_openclaw() -> Result<String, String> {
    // Try `openclaw gateway restart` first (the official CLI command)
    let output = tokio::process::Command::new("openclaw")
        .args(["gateway", "restart"])
        .output()
        .await
        .map_err(|e| format!("failed to spawn openclaw CLI: {}", e))?;

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();

    if output.status.success() {
        Ok(format!("{}{}", stdout, stderr))
    } else {
        // Fallback: try running the gateway-wrapper.sh directly
        eprintln!(
            "[clawdoctor] `openclaw gateway restart` failed ({}), trying wrapper script...",
            output.status
        );

        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
        let wrapper_path = format!("{}/.openclaw/gateway-wrapper.sh", home);

        let wrapper_output = tokio::process::Command::new("bash")
            .args([&wrapper_path])
            .output()
            .await
            .map_err(|e| format!("wrapper script failed: {}", e))?;

        let w_stdout = String::from_utf8_lossy(&wrapper_output.stdout).to_string();
        let w_stderr = String::from_utf8_lossy(&wrapper_output.stderr).to_string();

        if wrapper_output.status.success() {
            Ok(format!("(via wrapper) {}{}", w_stdout, w_stderr))
        } else {
            Err(format!(
                "both restart methods failed. CLI: {}{}. Wrapper: {}{}",
                stdout, stderr, w_stdout, w_stderr
            ))
        }
    }
}

/// Log a clawdoctor event to stderr.
async fn publish_event(_server: &MemoryServer, event_type: &str, message: &str) {
    eprintln!("[clawdoctor] event: {} — {}", event_type, message);
}

/// Save a clawdoctor incident as a memory entry for audit trail.
async fn save_incident_memory(server: &MemoryServer, text: &str) {
    let id = uuid::Uuid::new_v4().to_string();
    let timestamp = Utc::now().to_rfc3339();

    let entry = MemoryEntry {
        id,
        path: "/clawdoctor/incidents".to_string(),
        summary: text.chars().take(100).collect(),
        text: text.to_string(),
        importance: 0.9,
        timestamp,
        valid_from: String::new(),
        valid_until: None,
        category: "experience".to_string(),
        topic: "clawdoctor".to_string(),
        keywords: vec![
            "clawdoctor".to_string(),
            "openclaw".to_string(),
            "restart".to_string(),
            "health-check".to_string(),
        ],
        persons: vec![],
        entities: vec!["OpenClaw".to_string(), "clawdoctor".to_string()],
        location: String::new(),
        source: "clawdoctor".to_string(),
        scope: "project".to_string(),
        archived: false,
        access_count: 0,
        last_access: None,
        revision: 1,
        metadata: json!({}),
        vector: None,
        retention_policy: None,
        domain: None,
    };

    let result =
        server.with_global_store(|store| store.upsert(&entry).map_err(|e| format!("upsert: {e}")));

    if let Err(e) = result {
        eprintln!("[clawdoctor] failed to save incident memory: {}", e);
    }
}
