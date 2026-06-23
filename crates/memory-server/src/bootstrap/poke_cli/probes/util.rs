use serde_json::Value;
use std::path::Path;
use std::time::{Duration, Instant};

pub(in crate::bootstrap::poke_cli) async fn wait_for_file(
    path: &Path,
    timeout: Duration,
) -> Result<(), String> {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if tokio::fs::metadata(path).await.is_ok() {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    Err(format!(
        "timed out after {}s waiting for {}",
        timeout.as_secs(),
        path.display()
    ))
}

pub(in crate::bootstrap::poke_cli) fn collect_strings(value: &Value) -> Vec<String> {
    let mut out = Vec::new();
    collect_strings_inner(value, &mut out);
    out
}

fn collect_strings_inner(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::String(text) => out.push(text.clone()),
        Value::Array(items) => {
            for item in items {
                collect_strings_inner(item, out);
            }
        }
        Value::Object(map) => {
            for item in map.values() {
                collect_strings_inner(item, out);
            }
        }
        _ => {}
    }
}
