use std::path::PathBuf;

pub(crate) fn tachi_home() -> PathBuf {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    for key in ["TACHI_HOME", "SIGIL_HOME", "TACHI_APP_HOME"] {
        if let Ok(raw) = std::env::var(key) {
            if raw == "~" {
                return home;
            }
            if let Some(rest) = raw.strip_prefix("~/") {
                return home.join(rest);
            }
            if !raw.trim().is_empty() {
                return PathBuf::from(raw);
            }
        }
    }
    home.join(".tachi")
}
