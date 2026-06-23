use tokio::process::Command;

#[derive(Debug, serde::Deserialize, serde::Serialize)]
pub(in crate::dispatch_ops::merge) struct CleanerRemoveReport {
    pub removed: bool,
    #[serde(default)]
    pub warnings: Vec<String>,
    #[serde(default)]
    pub errors: Vec<String>,
}

pub(crate) fn resolve_tachi_clean_bin() -> std::path::PathBuf {
    if let Some(bin) = std::env::var_os("TACHI_CLEAN_BIN") {
        return std::path::PathBuf::from(bin);
    }

    let bin_name = format!("tachi-clean{}", std::env::consts::EXE_SUFFIX);

    if let Ok(current_exe) = std::env::current_exe() {
        if let Some(dir) = current_exe.parent() {
            let sibling = dir.join(&bin_name);
            if sibling.exists() {
                return sibling;
            }
        }
    }

    std::path::PathBuf::from(bin_name)
}

pub(in crate::dispatch_ops::merge) async fn remove_worktree_with_cleaner(
    worktree: &str,
) -> Result<CleanerRemoveReport, String> {
    let cleaner_bin = resolve_tachi_clean_bin();
    let out = Command::new(&cleaner_bin)
        .args(["wt-remove", worktree, "--force", "--json"])
        .output()
        .await
        .map_err(|err| format!("failed to run {}: {err}", cleaner_bin.display()))?;

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    match serde_json::from_str::<CleanerRemoveReport>(stdout.trim()) {
        Ok(report) => Ok(report),
        Err(err) => {
            if out.status.success() {
                return Err(format!(
                    "cleaner succeeded but returned invalid JSON: {err}. Output: {}",
                    stdout.trim()
                ));
            }

            let mut message = stderr.trim().to_string();
            if message.is_empty() {
                message = stdout.trim().to_string();
            }
            Err(if message.is_empty() {
                format!("{} exited with {}", cleaner_bin.display(), out.status)
            } else {
                format!("{} failed: {message}", cleaner_bin.display())
            })
        }
    }
}
