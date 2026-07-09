use crate::MemoryServer;
use std::path::PathBuf;

fn tachi_home() -> PathBuf {
    if let Ok(home) = std::env::var("TACHI_HOME") {
        PathBuf::from(home)
    } else if let Ok(home) = std::env::var("HOME") {
        PathBuf::from(home).join(".tachi")
    } else {
        std::env::temp_dir().join("tachi")
    }
}

pub(super) fn runs_dir_for_server(server: &MemoryServer) -> PathBuf {
    let global_db = server.global_db_path_buf();
    if global_db.file_name().and_then(|name| name.to_str()) == Some("memory.db") {
        if let Some(global_dir) = global_db.parent() {
            if global_dir.file_name().and_then(|name| name.to_str()) == Some("global") {
                if let Some(app_home) = global_dir.parent() {
                    return app_home.join("runs");
                }
            }
        }
    }
    tachi_home().join("runs")
}
