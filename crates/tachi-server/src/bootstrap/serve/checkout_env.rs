//! What a checkout's env files may do: nothing, but say so once.
//!
//! `.tachi/config.env`, `.sigil/config.env` and `.env` in a checkout are
//! content a repository holds. They used to be loaded into the daemon's
//! environment, which let a checkout set provider keys and endpoints, the
//! database path, and whatever else the daemon reads from there. They are not
//! applied any more. A file that holds a daemon setting gets one line on
//! stderr, so that a setting which stopped working can be found. A value is
//! never on that line.

use crate::bootstrap::env_cmd::dotenv_lines::{assignment_rhs, DotenvText};
use std::io::Read;
use std::path::Path;
use std::sync::OnceLock;

/// How much of one file is looked at.
const SCAN_BYTES: u64 = 64 * 1024;
/// How many setting names one line lists.
const NAMES_LISTED: usize = 8;
/// How much of a path one line shows.
const PATH_SHOWN: usize = 240;

/// What `.env.example` documents, in its plain and in its commented form: the
/// settings users were told to put in a project's `.env`.
const DOCUMENTED_EXAMPLE: &str = include_str!("../../../../../.env.example");

/// The daemon settings one file holds, by name.
#[derive(Debug, PartialEq, Eq)]
struct Settings {
    names: Vec<String>,
    /// The file is longer than what was looked at, so there can be more.
    cut_short: bool,
}

/// The line that documents one checkout file when it holds a daemon setting.
/// `None` for a file that is absent, is not a regular file, cannot be read as
/// text, or holds none of these settings: a project's `.env` commonly holds
/// settings for other tools, which are none of the daemon's business.
pub(super) fn note_ignored(path: &Path, operator_config_home: Option<&Path>) -> Option<String> {
    let settings = daemon_settings_in(path)?;
    if settings.names.is_empty() {
        return None;
    }
    Some(ignored_line(path, &settings, operator_config_home))
}

/// The one line that says a file is not applied, what it holds, and where
/// daemon settings go instead.
fn ignored_line(path: &Path, settings: &Settings, operator_config_home: Option<&Path>) -> String {
    let count = settings.names.len();
    let listed = settings.names[..count.min(NAMES_LISTED)].join(", ");
    let unlisted = count.saturating_sub(NAMES_LISTED);
    let mut line = format!(
        "[startup] {} is in a checkout and is not applied to the daemon. It holds, {}{} daemon setting{}: {}",
        shown(path),
        if settings.cut_short { "in its first 64 KiB, " } else { "" },
        count,
        if count == 1 { "" } else { "s" },
        listed,
    );
    if unlisted > 0 {
        line.push_str(&format!(", and {unlisted} more"));
    }
    line.push_str(". ");
    match operator_config_home {
        Some(home) => line.push_str(&format!(
            "Daemon settings go in {} and keys in {}.",
            shown(&home.join("config.env")),
            shown(&home.join("secrets.env")),
        )),
        None => line.push_str(
            "Daemon settings go in the operator's config.env and secrets.env, and no user \
             home directory was found for them: set TACHI_HOME to an absolute path.",
        ),
    }
    line
}

/// The daemon settings in one file, or `None` when there is nothing to read.
fn daemon_settings_in(path: &Path) -> Option<Settings> {
    // Asked what it is before it is opened: a directory opens too, and a pipe
    // would open and then wait for a writer. A link is opened without
    // waiting and asked on the descriptor, which is what gets read.
    let entry = std::fs::symlink_metadata(path).ok()?;
    let file_type = entry.file_type();
    if !(file_type.is_file() || file_type.is_symlink()) {
        return None;
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NONBLOCK);
    }
    let file = options.open(path).ok()?;
    if !file.metadata().ok()?.is_file() {
        return None;
    }
    let mut bytes = Vec::new();
    file.take(SCAN_BYTES + 1).read_to_end(&mut bytes).ok()?;
    let mut cut_short = false;
    if bytes.len() as u64 > SCAN_BYTES {
        // Whole lines only: the last one may have been cut in the middle, of
        // a character too.
        let scanned = &bytes[..SCAN_BYTES as usize];
        let end = scanned.iter().rposition(|byte| *byte == b'\n')?;
        bytes.truncate(end);
        cut_short = true;
    }
    let text = String::from_utf8(bytes).ok()?;
    let parsed = DotenvText::parse(&text).ok()?;
    let mut names = Vec::new();
    for entry in &parsed.entries {
        let Some(name) = setting_name(&entry.text) else {
            continue;
        };
        if (name.starts_with("TACHI_") || documented_settings().contains(&name))
            && !names.contains(&name)
        {
            names.push(name);
        }
    }
    Some(Settings { names, cut_short })
}

/// The name an assignment line sets, when it sets a setting: `NAME=value`,
/// with `NAME` in the spelling settings are documented in.
fn setting_name(line: &str) -> Option<String> {
    assignment_rhs(line)?;
    let trimmed = line.trim_start();
    let body = trimmed
        .strip_prefix("export ")
        .map(str::trim_start)
        .unwrap_or(trimmed);
    let name = body.split_once('=')?.0.trim_end();
    is_setting_name(name).then(|| name.to_string())
}

fn is_setting_name(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    matches!(first, 'A'..='Z' | '_')
        && chars.all(|character| matches!(character, 'A'..='Z' | '0'..='9' | '_'))
}

/// Every setting `.env.example` names, read once.
fn documented_settings() -> &'static [String] {
    static NAMES: OnceLock<Vec<String>> = OnceLock::new();
    NAMES.get_or_init(|| {
        let mut names = Vec::new();
        for line in DOCUMENTED_EXAMPLE.lines() {
            let mut text = line.trim();
            while let Some(rest) = text.strip_prefix('#') {
                text = rest.trim_start();
            }
            // Prose has no `=` after a name, and so is not a setting.
            if let Some((name, _)) = text.split_once('=') {
                let name = name.trim_end();
                if is_setting_name(name) && !names.iter().any(|known: &String| known == name) {
                    names.push(name.to_string());
                }
            }
        }
        names
    })
}

/// A path for the one line: escaped, and no longer than a line can hold.
fn shown(path: &Path) -> String {
    let text = format!("{path:?}");
    if text.len() <= PATH_SHOWN {
        return text;
    }
    let mut cut = String::new();
    for character in text.chars() {
        if cut.len() + character.len_utf8() > PATH_SHOWN {
            break;
        }
        cut.push(character);
    }
    cut.push('…');
    cut
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file_with(body: &str) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(".env");
        std::fs::write(&path, body).expect("write the file");
        (dir, path)
    }

    #[test]
    fn documented_settings_are_the_ones_the_example_names() {
        let names = documented_settings();
        for name in [
            "VOYAGE_API_KEY",
            "SILICONFLOW_BASE_URL",
            "SILICONFLOW_MODEL",
            "EXTRACT_API_KEY",
            "MEMORY_DB_PATH",
            // Only in a commented line in the example.
            "EXTRACT_PROVIDER",
            "DEEPSEEK_API_KEY",
        ] {
            assert!(names.iter().any(|known| known == name), "{name}");
        }
        assert!(names.iter().all(|name| is_setting_name(name)), "{names:?}");
        assert!(
            names.iter().any(|known| known == "TACHI_HOME"),
            "a commented setting is documented too: {names:?}"
        );
    }

    #[test]
    fn only_daemon_settings_are_reported_by_name_and_only_once() {
        let (_dir, path) = file_with(
            "# a comment\n\
             VOYAGE_API_KEY=sk-not-printed\n\
             export SILICONFLOW_BASE_URL=https://example.invalid\n\
             DATABASE_URL=postgres://other-tool\n\
             TACHI_WAL_CHECKPOINT_SECS=30\n\
             VOYAGE_API_KEY=sk-again\n\
             lower_case=1\n\
             this is prose with = sign\n",
        );
        assert_eq!(
            daemon_settings_in(&path),
            Some(Settings {
                names: vec![
                    "VOYAGE_API_KEY".to_string(),
                    "SILICONFLOW_BASE_URL".to_string(),
                    "TACHI_WAL_CHECKPOINT_SECS".to_string(),
                ],
                cut_short: false,
            })
        );
    }

    /// A quoted value that spans lines is one entry: a line inside a secret
    /// is not an assignment of its own.
    #[test]
    fn a_setting_name_inside_a_multiline_value_is_not_a_setting() {
        let (_dir, path) = file_with(
            "OTHER_TOOL_PRIVATE_KEY=\"line one\n\
             SILICONFLOW_API_KEY=inside-the-value\n\
             still inside\"\n\
             VOYAGE_API_KEY=sk-real\n",
        );
        assert_eq!(
            daemon_settings_in(&path),
            Some(Settings {
                names: vec!["VOYAGE_API_KEY".to_string()],
                cut_short: false,
            })
        );
    }

    #[test]
    fn what_is_past_the_scan_is_not_looked_at_and_the_line_says_so() {
        let filler = "X".repeat(70 * 1024);
        let (_dir, path) = file_with(&format!("DATABASE_URL=postgres://x\n{filler}\n"));
        let settings = daemon_settings_in(&path).expect("the file is read");
        assert!(settings.cut_short);
        assert!(settings.names.is_empty());
    }

    #[test]
    fn an_absent_file_and_a_directory_say_nothing() {
        assert_eq!(daemon_settings_in(Path::new("/no/such/.env")), None);
        let dir = tempfile::tempdir().expect("tempdir");
        assert_eq!(daemon_settings_in(dir.path()), None);
    }

    /// A pipe where a file is expected would wait for a writer if it were
    /// opened the ordinary way. It is not opened that way, and says nothing.
    #[cfg(unix)]
    #[test]
    fn a_pipe_is_not_waited_for() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(".env");
        let name = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).expect("no NUL");
        // SAFETY: creates a pipe at a path this test owns, and nothing else.
        let created = unsafe { libc::mkfifo(name.as_ptr(), 0o600) };
        assert_eq!(created, 0, "mkfifo: {}", std::io::Error::last_os_error());
        let started = std::time::Instant::now();
        assert_eq!(daemon_settings_in(&path), None);
        assert!(
            started.elapsed() < std::time::Duration::from_secs(5),
            "the pipe was waited for"
        );
        // A link to it neither.
        let link = dir.path().join(".env-link");
        std::os::unix::fs::symlink(&path, &link).expect("link to the pipe");
        let started = std::time::Instant::now();
        assert_eq!(daemon_settings_in(&link), None);
        assert!(
            started.elapsed() < std::time::Duration::from_secs(5),
            "the pipe behind the link was waited for"
        );
    }

    /// The line names the file, its settings and where they go. A value is
    /// not on it, and a path with control characters in it is escaped.
    #[test]
    fn the_line_names_settings_and_never_a_value() {
        let home = tempfile::tempdir().expect("tempdir");
        let (_dir, path) = file_with("VOYAGE_API_KEY=sk-not-printed\nTACHI_X=1\n");
        let settings = daemon_settings_in(&path).expect("the file is read");
        let line = ignored_line(&path, &settings, Some(home.path()));
        assert!(
            line.starts_with(&format!("[startup] {:?} is in a checkout", path)),
            "{line}"
        );
        assert!(
            line.contains("2 daemon settings: VOYAGE_API_KEY, TACHI_X"),
            "{line}"
        );
        assert!(
            line.contains(&format!("{:?}", home.path().join("config.env"))),
            "{line}"
        );
        assert!(!line.contains("sk-not-printed"), "{line}");

        let odd = Path::new("/repo/\u{1b}[31m/.env");
        let line = ignored_line(odd, &settings, None);
        assert!(
            line.contains("\\u{1b}"),
            "a control character is escaped: {line}"
        );
        assert!(
            line.contains("set TACHI_HOME to an absolute path"),
            "{line}"
        );
    }

    #[test]
    fn more_names_than_fit_are_counted() {
        let body: String = (0..10)
            .map(|index| format!("TACHI_SETTING_{index}=1\n"))
            .collect();
        let (_dir, path) = file_with(&body);
        let settings = daemon_settings_in(&path).expect("the file is read");
        let line = ignored_line(&path, &settings, None);
        assert!(line.contains("10 daemon settings: TACHI_SETTING_0, TACHI_SETTING_1, TACHI_SETTING_2, TACHI_SETTING_3, TACHI_SETTING_4, TACHI_SETTING_5, TACHI_SETTING_6, TACHI_SETTING_7, and 2 more."), "{line}");
    }
}
