use super::*;
use std::fs::{self, File};
use std::io::{BufRead, BufReader};

pub(crate) const MAX_EVAL_FIXTURE_BYTES: u64 = 5 * 1024 * 1024;
const MAX_EVAL_ROWS: usize = 10_000;
pub(crate) const MAX_LIVE_EVAL_LIMIT: usize = 5_000;
pub(crate) const AGENT_EVAL_FIXTURE_ENV: &str = "TACHI_AGENT_EVAL_ALLOW_FIXTURE";

pub(crate) fn load_eval_jsonl(path: &Path) -> Result<Vec<EvalRow>, String> {
    let metadata =
        fs::metadata(path).map_err(|e| format!("stat eval file {}: {e}", path.display()))?;
    if !metadata.is_file() {
        return Err(format!(
            "eval fixture {} must be a regular file",
            path.display()
        ));
    }
    if metadata.len() > MAX_EVAL_FIXTURE_BYTES {
        return Err(format!(
            "eval fixture {} is too large ({} bytes > {} byte cap)",
            path.display(),
            metadata.len(),
            MAX_EVAL_FIXTURE_BYTES
        ));
    }

    let file = File::open(path).map_err(|e| format!("open eval file {}: {e}", path.display()))?;
    let reader = BufReader::new(file);
    let mut rows = Vec::new();
    for (line_no, line) in reader.lines().enumerate() {
        let line = line.map_err(|e| format!("read line {}: {e}", line_no + 1))?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if rows.len() >= MAX_EVAL_ROWS {
            return Err(format!(
                "eval fixture {} has more than {} rows",
                path.display(),
                MAX_EVAL_ROWS
            ));
        }
        let row: EvalRow = serde_json::from_str(trimmed)
            .map_err(|e| format!("parse eval line {}: {e}", line_no + 1))?;
        rows.push(row);
    }
    Ok(rows)
}

pub(crate) fn capped_eval_limit(limit: Option<usize>) -> usize {
    limit.unwrap_or(500).clamp(1, MAX_LIVE_EVAL_LIMIT)
}

pub(crate) fn eval_fixture_replay_allowed() -> bool {
    std::env::var(AGENT_EVAL_FIXTURE_ENV)
        .ok()
        .as_deref()
        .is_some_and(|value| matches!(value, "1" | "true" | "TRUE" | "yes" | "YES"))
}
