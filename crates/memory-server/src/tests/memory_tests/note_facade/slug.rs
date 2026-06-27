use super::*;

#[test]
fn write_note_file_falls_back_when_slug_has_no_ascii_tokens() {
    let _temp_home = TempHomeGuard::new();

    let (abs_path, rel_path) = crate::notes_ops::write_note_file(
        "body for non-ascii title",
        None,
        Some("中文 标题"),
        Some("notes-test"),
        Some("note"),
        &[],
    )
    .expect("write note file");

    assert!(abs_path.exists(), "note path should exist: {abs_path:?}");
    assert!(
        rel_path.starts_with("inbox/"),
        "unexpected note path: {rel_path}"
    );
    assert!(
        rel_path.ends_with("-note.md"),
        "non-ascii title should use note slug fallback: {rel_path}"
    );
}
