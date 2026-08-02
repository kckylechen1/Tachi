use std::path::Path;

pub(super) fn is_archive_dir(path: &Path) -> bool {
    path.file_name().is_some_and(|name| name == "archive")
}

pub(super) fn is_markdown_file(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("md"))
}
