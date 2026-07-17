use crate::MemoryServer;
use std::path::PathBuf;

pub(super) fn runs_dir_for_server(server: &MemoryServer) -> PathBuf {
    let global_db = server.global_db_path_buf();
    if global_db
        .file_name()
        .and_then(|name| name.to_str())
        .map(memcore::is_memory_db_filename)
        .unwrap_or(false)
    {
        if let Some(global_dir) = global_db.parent() {
            if global_dir.file_name().and_then(|name| name.to_str()) == Some("global") {
                if let Some(app_home) = global_dir.parent() {
                    return app_home.join("runs");
                }
            }
        }
    }
    // #1096 leaf-2a: fall back to the server-bound home identity (resolved
    // once at construction via the full TACHI_HOME → SIGIL_HOME →
    // TACHI_APP_HOME → workspace → ~/.tachi chain) instead of this file's
    // former private two-key (`TACHI_HOME`/`HOME`) duplicate, which drifted
    // from the canonical `path_utils::tachi_home()` precedence.
    server.tachi_home_dir().join("runs")
}
