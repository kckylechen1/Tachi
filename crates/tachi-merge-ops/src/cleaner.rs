use tokio::process::Command;

#[derive(Debug, serde::Deserialize, serde::Serialize)]
pub struct CleanerRemoveReport {
    pub removed: bool,
    #[serde(default)]
    pub warnings: Vec<String>,
    #[serde(default)]
    pub errors: Vec<String>,
}

pub fn resolve_tachi_clean_bin() -> std::path::PathBuf {
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

pub async fn remove_worktree_with_cleaner(worktree: &str) -> Result<CleanerRemoveReport, String> {
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

#[cfg(test)]
mod tests {
    use super::resolve_tachi_clean_bin;

    // This is the only test in this crate's test binary that touches
    // `TACHI_CLEAN_BIN`, so no cross-test env lock is needed here.
    #[test]
    fn cleaner_bridge_uses_explicit_cleaner_binary_override() {
        let old_value = std::env::var_os("TACHI_CLEAN_BIN");
        std::env::set_var("TACHI_CLEAN_BIN", "/tmp/custom-tachi-clean");

        let resolved = resolve_tachi_clean_bin();

        match old_value {
            Some(value) => std::env::set_var("TACHI_CLEAN_BIN", value),
            None => std::env::remove_var("TACHI_CLEAN_BIN"),
        }
        assert_eq!(
            resolved,
            std::path::PathBuf::from("/tmp/custom-tachi-clean")
        );
    }
}
