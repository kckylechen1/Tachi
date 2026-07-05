use super::*;

pub(super) fn cleanup_runs_dir(root: &Path, now: SystemTime) -> (usize, usize) {
    cleanup_runs_dir_recursive(root, now, 0)
}

fn cleanup_runs_dir_recursive(root: &Path, now: SystemTime, depth: usize) -> (usize, usize) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return (0, 0);
    };

    let success_max = Duration::from_secs(SUCCESS_RETENTION_DAYS * 86_400);
    let failed_max = Duration::from_secs(FAILED_RETENTION_DAYS * 86_400);

    let mut removed = 0usize;
    let mut scanned = 0usize;
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }

        let status_path = path.join("status.json");
        let manifest_path = path.join("source_manifest.json");
        if !status_path.exists() && !manifest_path.exists() {
            let child_has_dirs = std::fs::read_dir(&path)
                .ok()
                .into_iter()
                .flatten()
                .flatten()
                .any(|e| e.path().is_dir());
            if child_has_dirs && depth < 8 {
                let (r, s) = cleanup_runs_dir_recursive(&path, now, depth + 1);
                removed += r;
                scanned += s;
                continue;
            }
        }

        scanned += 1;

        let (failed, modified) = if manifest_path.exists() {
            let mtime = std::fs::metadata(&manifest_path)
                .and_then(|m| m.modified())
                .ok();
            (false, mtime)
        } else if let Ok(raw) = std::fs::read_to_string(&status_path) {
            let failed = serde_json::from_str::<Value>(&raw)
                .ok()
                .and_then(|v| v.get("status").and_then(|s| s.as_str()).map(str::to_string))
                .map(|s| s != "success")
                .unwrap_or(true);
            let mtime = std::fs::metadata(&status_path)
                .and_then(|m| m.modified())
                .ok();
            (failed, mtime)
        } else {
            let mtime = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
            (true, mtime)
        };

        let Some(modified) = modified else { continue };
        let Ok(age) = now.duration_since(modified) else {
            continue;
        };
        let max = if failed { failed_max } else { success_max };
        if age > max && std::fs::remove_dir_all(&path).is_ok() {
            removed += 1;
        }
    }
    (removed, scanned)
}
