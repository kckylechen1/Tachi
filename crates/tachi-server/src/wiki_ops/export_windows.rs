//! Windows boundary for Wiki export.
//!
//! The transactional exporter relies on Unix descriptor and inode semantics
//! for its write authority.  Windows must not compile a path-based substitute:
//! refusing before any output inspection or mutation keeps the unsupported
//! operation explicit while the read/search Wiki surfaces remain available.

use crate::server_state::MemoryServer;
use serde_json::Value;
use std::path::Path;

pub(crate) const UNSUPPORTED: &str = "Wiki export is unsupported on this platform: transactional Unix write authority is unavailable";

pub(crate) fn export_wiki_obsidian(
    _server: &MemoryServer,
    _project: &str,
    _output: &Path,
) -> Result<Value, String> {
    Err(UNSUPPORTED.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn platform_refusal_wiki_export_preserves_absent_output() {
        let (server, _home) = crate::tests::make_server_with_temp_home();
        let root = tempfile::tempdir().expect("temporary output root");
        let output = root.path().join("wiki");

        let error = export_wiki_obsidian(&server, "wiki", &output).expect_err("unsupported");

        assert!(error.contains("unsupported on this platform"));
        assert!(
            !output.exists(),
            "refusal must not create an output directory"
        );
    }

    #[test]
    fn platform_refusal_wiki_export_preserves_existing_output() {
        let (server, _home) = crate::tests::make_server_with_temp_home();
        let root = tempfile::tempdir().expect("temporary output root");
        let output = root.path().join("wiki");
        std::fs::create_dir(&output).expect("output directory");
        let sentinel = output.join("sentinel.md");
        std::fs::write(&sentinel, b"foreign bytes").expect("sentinel");
        let before = std::fs::read(&sentinel).expect("sentinel before");

        let error = export_wiki_obsidian(&server, "wiki", &output).expect_err("unsupported");

        assert!(error.contains("unsupported on this platform"));
        assert_eq!(std::fs::read(sentinel).expect("sentinel after"), before);
    }
}
