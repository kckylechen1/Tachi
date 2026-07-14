use chrono::Utc;
use std::path::{Component, Path, PathBuf};

// ─── Notes filesystem helpers ──────────────────────────────────────────────────

const NOTE_SUBDIRS: &[&str] = &[
    "inbox",
    "brainstorm",
    "dispatch",
    "handoff",
    "reflections",
    "proposals",
];

/// Resolve the notes root directory under the active Tachi home.
pub(crate) fn notes_root(home: &Path) -> PathBuf {
    home.join("notes")
}

/// Ensure standard subdirectories exist under the notes root.
fn ensure_notes_dirs(root: &Path) -> Result<(), String> {
    for dir in NOTE_SUBDIRS {
        std::fs::create_dir_all(root.join(dir))
            .map_err(|e| format!("Failed to create notes dir '{}': {e}", dir))?;
    }
    Ok(())
}

/// Resolve a note file path within the notes root.
///
/// Rules:
/// - `None` or empty path → `inbox/<timestamp>-<slug>.md`
/// - Relative path ending in `.md` → `notes/<path>`
/// - Relative path not ending in `.md` → `notes/<path>/<timestamp>-<slug>.md`
/// - Absolute paths are rejected (security: no escape from notes root)
/// - `..` traversal is rejected
fn resolve_note_path(
    root: &Path,
    user_path: Option<&str>,
    slug: &str,
    ts: &str,
) -> Result<PathBuf, String> {
    let default_file = format!("{}-{}.md", ts, slug);
    let rel_path = match user_path {
        None | Some("") => PathBuf::from("inbox").join(&default_file),
        Some(p) => {
            let candidate = Path::new(p);
            let mut clean = PathBuf::new();
            for component in candidate.components() {
                match component {
                    Component::Normal(part) => clean.push(part),
                    Component::CurDir => {}
                    Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                        return Err(format!(
                            "Note paths must be relative and stay under notes root (got '{}').",
                            p
                        ));
                    }
                }
            }
            if p.ends_with(".md") {
                clean
            } else {
                clean.join(&default_file)
            }
        }
    };
    let resolved = root.join(&rel_path);

    let canonical_root = root
        .canonicalize()
        .map_err(|e| format!("Failed to canonicalize notes root: {e}"))?;
    let parent = resolved
        .parent()
        .ok_or_else(|| "Note path has no parent directory".to_string())?;
    let relative_parent = rel_path
        .parent()
        .ok_or_else(|| "Note path has no relative parent".to_string())?;
    let mut cursor = root.to_path_buf();
    for component in relative_parent.components() {
        if let Component::Normal(part) = component {
            cursor.push(part);
            if let Ok(meta) = std::fs::symlink_metadata(&cursor) {
                if meta.file_type().is_symlink() {
                    return Err("Note path escapes the notes root directory".to_string());
                }
            }
        }
    }
    std::fs::create_dir_all(parent).map_err(|e| format!("Failed to create note dir: {e}"))?;
    let canonical_parent = parent
        .canonicalize()
        .map_err(|e| format!("Failed to canonicalize note parent: {e}"))?;
    if !canonical_parent.starts_with(&canonical_root) {
        return Err("Note path escapes the notes root directory".to_string());
    }
    if let Ok(meta) = std::fs::symlink_metadata(&resolved) {
        if meta.file_type().is_symlink() {
            return Err("Note path escapes the notes root directory".to_string());
        }
    }

    Ok(resolved)
}

/// Build a human-readable markdown note with frontmatter.
fn build_note_markdown(
    text: &str,
    title: &str,
    topic: Option<&str>,
    category: Option<&str>,
    keywords: &[String],
    source: &str,
) -> String {
    let ts = Utc::now().to_rfc3339();
    let kw_str = if keywords.is_empty() {
        "[]".to_string()
    } else {
        format!(
            "[{}]",
            keywords
                .iter()
                .map(|k| format!("\"{}\"", k))
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    format!(
        "---\ntitle: \"{}\"\ncreated_at: \"{}\"\ntopic: \"{}\"\ncategory: \"{}\"\nkeywords: {}\nsource: \"{}\"\n---\n\n{}",
        title.replace('"', "\\\""),
        ts,
        topic.unwrap_or("").replace('"', "\\\""),
        category.unwrap_or("note").replace('"', "\\\""),
        kw_str,
        source.replace('"', "\\\""),
        text,
    )
}

/// Write a note to the filesystem and return the relative path + absolute path.
pub(crate) fn write_note_file(
    home: &Path,
    text: &str,
    user_path: Option<&str>,
    title: Option<&str>,
    topic: Option<&str>,
    category: Option<&str>,
    keywords: &[String],
) -> Result<(PathBuf, String), String> {
    let root = notes_root(home);
    ensure_notes_dirs(&root)?;

    let now = Utc::now();
    let ts = now.format("%Y%m%dT%H%M%SZ").to_string();

    let slug_source = title
        .map(str::to_string)
        .unwrap_or_else(|| text.chars().take(40).collect::<String>());
    let slug: String = slug_source
        .to_ascii_lowercase()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect();
    let slug = {
        let s: String = slug
            .split('-')
            .filter(|part| !part.is_empty())
            .take(6)
            .collect::<Vec<_>>()
            .join("-");
        if s.is_empty() {
            "note".to_string()
        } else {
            format!("{:.60}", s) // cap length
        }
    };

    let abs_path = resolve_note_path(&root, user_path, &slug, &ts)?;

    let note_title = title
        .map(String::from)
        .unwrap_or_else(|| slug.replace('-', " "));
    let md = build_note_markdown(text, &note_title, topic, category, keywords, "tachi_save");

    // Ensure parent dir exists
    if let Some(parent) = abs_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create note parent dir: {e}"))?;
    }

    std::fs::write(&abs_path, &md).map_err(|e| format!("Failed to write note file: {e}"))?;

    // Compute relative path from notes root
    let rel = abs_path
        .strip_prefix(&root)
        .unwrap_or(&abs_path)
        .to_string_lossy()
        .to_string();

    Ok((abs_path, rel))
}
