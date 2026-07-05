use super::*;

pub(super) fn sanitize_label(label: &str) -> String {
    let cleaned: String = label
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let trimmed = cleaned.trim_matches('_');
    if trimmed.is_empty() {
        "run".to_string()
    } else {
        trimmed.chars().take(48).collect()
    }
}

pub(super) async fn write_owner_only_file_blocking(
    path: PathBuf,
    bytes: Vec<u8>,
) -> Result<(), String> {
    let display = path.display().to_string();
    tokio::task::spawn_blocking(move || write_owner_only_file(&path, &bytes))
        .await
        .map_err(|error| format!("owner-only write task failed for {display}: {error}"))?
}

pub(super) async fn write_run_file_blocking(path: PathBuf, body: String) -> Result<(), String> {
    let display = path.display().to_string();
    tokio::task::spawn_blocking(move || write_owner_only_file_atomic(&path, body.as_bytes()))
        .await
        .map_err(|error| format!("run-file write task failed for {display}: {error}"))?
}

pub(super) async fn write_run_status_file_blocking(
    run_dir: PathBuf,
    status: Value,
) -> Result<(), String> {
    let display = run_dir.display().to_string();
    tokio::task::spawn_blocking(move || {
        crate::runtime_files::write_run_status_file(&run_dir, &status)
    })
    .await
    .map_err(|error| format!("status write task failed for {display}: {error}"))?
}
