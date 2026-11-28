//! What a checkout's env files may do: nothing, but say so once.
//!
//! `.tachi/config.env`, `.sigil/config.env` and `.env` in a checkout are
//! content a repository holds. They used to be loaded into the daemon's
//! environment, which let a checkout set provider keys and endpoints, the
//! database path, and whatever else the daemon reads from there. They are not
//! applied any more. A file that holds a daemon setting gets one line on
//! stderr, so that a setting which stopped working can be found.
//!
//! The line is best effort. It carries names, never a value. It says what was
//! found in the first 64 KiB of a file, by the names `.env.example` and the
//! provider key list document and by the `TACHI_` prefix. A file is looked at
//! only to be named: a pipe or a device is not opened in a way that waits for
//! a writer. The filesystem calls themselves are ordinary synchronous ones.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// How much of one file is looked at.
const SCAN_BYTES: u64 = 64 * 1024;
/// How many setting names one line lists.
const NAMES_LISTED: usize = 8;
/// How much of a path one line shows.
const PATH_SHOWN: usize = 240;
/// How long a name can be and still be one. The longest setting the daemon
/// reads is 53 characters.
const NAME_LONGEST: usize = 96;

/// What `.env.example` documents, in its plain and in its commented form: the
/// settings users were told to put in a project's `.env`.
const DOCUMENTED_EXAMPLE: &str = include_str!("../../../../../.env.example");

/// The files the operator's configuration is read from.
pub(super) struct TrustedFiles<'a> {
    pub(super) user_home: Option<&'a Path>,
    pub(super) operator_config_home: Option<&'a Path>,
}

impl TrustedFiles<'_> {
    /// The files themselves, those that have a home.
    fn paths(&self) -> impl Iterator<Item = PathBuf> + '_ {
        self.operator_config_home
            .iter()
            .flat_map(|home| ["config.env", "secrets.env"].map(|name| home.join(name)))
            .chain(self.user_home.iter().flat_map(|home| {
                [".sigil/config.env", ".secrets/master.env"].map(|name| home.join(name))
            }))
    }

    /// Whether `path` is one of them. The working directory can be the user's
    /// own home, and a project's env file can be a link to one of them. Such
    /// a file was applied, not ignored, and is not named as ignored.
    pub(super) fn contains(&self, path: &Path) -> bool {
        let Ok(path) = std::fs::canonicalize(path) else {
            return false;
        };
        self.paths()
            .any(|trusted| std::fs::canonicalize(&trusted).is_ok_and(|trusted| trusted == path))
    }
}

/// The daemon settings one file holds, by name.
#[derive(Debug, PartialEq, Eq)]
struct Settings {
    names: Vec<String>,
    /// The file is longer than what was looked at, so there can be more.
    cut_short: bool,
}

/// The line that documents one checkout file when it holds a daemon setting.
/// `None` for a file that is one of the operator's own, is absent, is not a
/// regular file, cannot be read as text, or holds none of these settings: a
/// project's `.env` commonly holds settings for other tools, which are none
/// of the daemon's business.
pub(super) fn note_ignored(path: &Path, trusted: &TrustedFiles<'_>) -> Option<String> {
    if trusted.contains(path) {
        return None;
    }
    let settings = daemon_settings_in(path)?;
    if settings.names.is_empty() {
        return None;
    }
    Some(ignored_line(path, &settings, trusted.operator_config_home))
}

/// The one line that says a file is not applied, what it holds, and where
/// daemon settings go instead. Its length is bounded: so many names, each so
/// long, and so much of each path.
fn ignored_line(path: &Path, settings: &Settings, operator_config_home: Option<&Path>) -> String {
    let count = settings.names.len();
    let listed = settings.names[..count.min(NAMES_LISTED)].join(", ");
    let unlisted = count.saturating_sub(NAMES_LISTED);
    let mut line = format!(
        "[startup] {} is in a checkout and is not applied to the daemon. It holds, {}{} daemon setting{}: {}",
        shown_path(path),
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
            shown_path(&home.join("config.env")),
            shown_path(&home.join("secrets.env")),
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
    let mut names = Vec::new();
    for name in assigned_names(&text) {
        if (name.starts_with("TACHI_")
            || documented_settings().contains(&name)
            || provider_key_names().contains(&name))
            && !names.contains(&name)
        {
            names.push(name);
        }
    }
    Some(Settings { names, cut_short })
}

/// Where a physical line ends for the parser that loads env files, which is
/// dotenvy. Its rules are followed here, so that a line counts as an
/// assignment exactly when the loader would have taken it as one.
#[derive(Clone, Copy, PartialEq, Eq)]
enum LineEnd {
    /// The assignment is over.
    Complete,
    /// The rest of the physical line is a comment.
    Comment,
    /// An assignment is open and the next physical line continues it: a
    /// quote or an escape has not closed, or the line ended in a space.
    Escape,
    Strong,
    StrongEscape,
    Weak,
    WeakEscape,
    Space,
}

impl LineEnd {
    /// The next physical line continues the same assignment.
    fn is_open(self) -> bool {
        !matches!(self, LineEnd::Complete | LineEnd::Comment)
    }
}

/// The state after one physical line (including its line ending), matching
/// pinned `dotenvy`'s `QuotedLines` / `eval_end_state` rules.
fn line_end(state: LineEnd, line: &str) -> LineEnd {
    line.chars().fold(state, |state, c| match state {
        LineEnd::Space => match c {
            '#' => LineEnd::Comment,
            '\\' => LineEnd::Escape,
            '"' => LineEnd::Weak,
            '\'' => LineEnd::Strong,
            _ => LineEnd::Complete,
        },
        LineEnd::Complete => match c {
            c if c.is_whitespace() && c != '\n' && c != '\r' => LineEnd::Space,
            '\\' => LineEnd::Escape,
            '"' => LineEnd::Weak,
            '\'' => LineEnd::Strong,
            _ => LineEnd::Complete,
        },
        LineEnd::Escape => LineEnd::Complete,
        LineEnd::Weak => match c {
            '\\' => LineEnd::WeakEscape,
            '"' => LineEnd::Complete,
            _ => LineEnd::Weak,
        },
        LineEnd::WeakEscape => LineEnd::Weak,
        LineEnd::Strong => match c {
            '\\' => LineEnd::StrongEscape,
            '\'' => LineEnd::Complete,
            _ => LineEnd::Strong,
        },
        LineEnd::StrongEscape => LineEnd::Strong,
        LineEnd::Comment => LineEnd::Comment,
    })
}

/// The names of the assignments in `text`, in order: one for each logical line
/// that closes inside it. The name is what stands before the first `=` of the
/// logical line, so nothing of a value can be taken for one. A logical line
/// still open where the text ends is left out, as the loader leaves it out.
fn assigned_names(text: &str) -> Vec<String> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut names = Vec::new();
    let mut state = LineEnd::Complete;
    let mut open: Option<String> = None;
    for line in text.split_inclusive('\n') {
        if state == LineEnd::Complete {
            // The loader skips a logical line that starts with a comment,
            // whatever follows on it.
            let trimmed = line.trim_start();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            open = assignment_name(line);
        }
        state = line_end(state, line);
        if !state.is_open() {
            names.extend(open.take());
            state = LineEnd::Complete;
        }
    }
    names
}

/// The name the logical line starting at `line` assigns to, when it assigns a
/// setting: `NAME=value`, with `NAME` in the spelling settings are
/// documented in.
fn assignment_name(line: &str) -> Option<String> {
    let trimmed = line.trim_start();
    let body = trimmed
        .strip_prefix("export ")
        .map(str::trim_start)
        .unwrap_or(trimmed);
    let name = body.split_once('=')?.0.trim_end();
    is_setting_name(name).then(|| name.to_string())
}

fn is_setting_name(name: &str) -> bool {
    if name.len() > NAME_LONGEST {
        return false;
    }
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

/// Every provider key the key list knows, under each of its names, read once.
fn provider_key_names() -> &'static [String] {
    static NAMES: OnceLock<Vec<String>> = OnceLock::new();
    NAMES.get_or_init(|| {
        let mut names = Vec::new();
        for def in crate::provider_config::key_registry::API_KEY_DEFS {
            for name in std::iter::once(def.key).chain(def.aliases.iter().copied()) {
                if !names.iter().any(|known: &String| known == name) {
                    names.push(name.to_string());
                }
            }
        }
        names
    })
}

/// A path for the one line: escaped, and no longer than a line can hold.
pub(super) fn shown_path(path: &Path) -> String {
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

    fn names_in(body: &str) -> Vec<String> {
        let (_dir, path) = file_with(body);
        daemon_settings_in(&path).expect("the file is read").names
    }

    fn no_trusted_files() -> TrustedFiles<'static> {
        TrustedFiles {
            user_home: None,
            operator_config_home: None,
        }
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
            "TACHI_HOME",
        ] {
            assert!(names.iter().any(|known| known == name), "{name}");
        }
        assert!(names.iter().all(|name| is_setting_name(name)), "{names:?}");
    }

    /// The daemon reads a key the example does not name. The key list does.
    #[test]
    fn provider_keys_are_daemon_settings_even_when_the_example_omits_them() {
        let keys = provider_key_names();
        for name in [
            "VOYAGE_RERANK_API_KEY",
            "VOYAGE_API_KEY",
            "SILICONFLOW_API_KEY",
        ] {
            assert!(keys.iter().any(|known| known == name), "{name}");
        }
        assert_eq!(
            names_in("VOYAGE_RERANK_API_KEY=sk-not-printed\n"),
            vec!["VOYAGE_RERANK_API_KEY".to_string()]
        );
    }

    #[test]
    fn only_daemon_settings_are_reported_by_name_and_only_once() {
        assert_eq!(
            names_in(
                "# a comment\n\
                 VOYAGE_API_KEY=sk-not-printed\n\
                 export SILICONFLOW_BASE_URL=https://example.invalid\n\
                 DATABASE_URL=postgres://other-tool\n\
                 TACHI_WAL_CHECKPOINT_SECS=30\n\
                 VOYAGE_API_KEY=sk-again\n\
                 lower_case=1\n\
                 this is prose with = sign\n",
            ),
            vec![
                "VOYAGE_API_KEY".to_string(),
                "SILICONFLOW_BASE_URL".to_string(),
                "TACHI_WAL_CHECKPOINT_SECS".to_string(),
            ]
        );
    }

    /// A value is split into logical lines the way the loader splits them. A
    /// quote opens a value wherever it appears, not only first, and so do a
    /// single quote and an escape at the end of a line. What follows on the
    /// next physical line is part of that value, and none of it is a name.
    #[test]
    fn a_line_inside_a_value_is_not_an_assignment() {
        for body in [
            "OTHER=prefix\"\nVOYAGE_API_KEY=tail\"\n",
            "OTHER=prefix'\nVOYAGE_API_KEY=tail'\n",
            "A=x\\\n# \"\nOTHER=\"\nTACHI_PRIVATE_FRAGMENT=inside\n\"\n",
            "OTHER=\"line one\nVOYAGE_API_KEY=inside\nstill inside\"\n",
            "OTHER='line one\nTACHI_PROJECT=inside\nstill inside'\n",
        ] {
            assert_eq!(names_in(body), Vec::<String>::new(), "{body:?}");
        }
        assert_eq!(
            names_in(
                "OTHER=\"line one\nVOYAGE_API_KEY=inside\nstill inside\"\nVOYAGE_API_KEY=sk-real\n"
            ),
            vec!["VOYAGE_API_KEY".to_string()],
            "the assignment after the value is its own, once"
        );
        assert_eq!(
            names_in("OTHER=foo   \nSILICONFLOW_MODEL=qwen\n"),
            vec!["SILICONFLOW_MODEL".to_string()],
            "trailing spaces do not continue onto the next line"
        );
    }

    /// A comment ends the line it is on, whatever quotes follow it. A logical
    /// line that starts with a comment is skipped whole.
    #[test]
    fn a_comment_ends_the_line() {
        assert_eq!(
            names_in(
                "VOYAGE_API_KEY=x # \"not open\nSILICONFLOW_BASE_URL=y\n# \" neither is this\n"
            ),
            vec![
                "VOYAGE_API_KEY".to_string(),
                "SILICONFLOW_BASE_URL".to_string(),
            ]
        );
    }

    /// An assignment still open where the file ends was never applied: the
    /// loader stops there. It is not named. What closed before it is.
    #[test]
    fn an_assignment_open_at_the_end_is_left_out_and_the_rest_is_kept() {
        assert_eq!(
            names_in("VOYAGE_API_KEY=\"never closed\n"),
            Vec::<String>::new()
        );
        assert_eq!(
            names_in("TACHI_PROJECT=x\nOTHER=\"unterminated\n"),
            vec!["TACHI_PROJECT".to_string()]
        );
    }

    #[test]
    fn what_is_past_the_scan_is_not_looked_at_and_the_line_says_so() {
        let filler = "X".repeat(70 * 1024);
        let (_dir, path) = file_with(&format!("DATABASE_URL=postgres://x\n{filler}\n"));
        let settings = daemon_settings_in(&path).expect("the file is read");
        assert!(settings.cut_short);
        assert!(settings.names.is_empty());
        // Cut inside a value: the assignment it belongs to is open, and is
        // left out; what closed before it is kept.
        let mut body = "VOYAGE_API_KEY=sk-real\n".to_string();
        body.push_str("OTHER=\"");
        body.push_str(&filler);
        body.push('\n');
        let (_dir, path) = file_with(&body);
        let settings = daemon_settings_in(&path).expect("the file is read");
        assert!(settings.cut_short);
        assert_eq!(settings.names, vec!["VOYAGE_API_KEY".to_string()]);
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

    /// A name is bounded. One long enough to carry a value is not a name.
    #[test]
    fn a_name_past_the_longest_setting_is_not_one() {
        let long = format!("TACHI_{}", "X".repeat(NAME_LONGEST));
        assert_eq!(names_in(&format!("{long}=1\n")), Vec::<String>::new());
        let longest = format!("TACHI_{}", "X".repeat(NAME_LONGEST - "TACHI_".len()));
        assert_eq!(
            names_in(&format!("{longest}=1\n")),
            vec![longest],
            "up to the bound it is still named"
        );
    }

    /// The operator's own files are not named as ignored: when the working
    /// directory is the user's home, or when a project's file is a link to
    /// one of them.
    #[test]
    fn the_operators_own_files_are_not_named() {
        let home = tempfile::tempdir().expect("home");
        let operator = home.path().join(".tachi");
        std::fs::create_dir_all(&operator).expect("operator home");
        let config = operator.join("config.env");
        std::fs::write(&config, "VOYAGE_API_KEY=sk-operator\n").expect("config.env");
        let trusted = TrustedFiles {
            user_home: Some(home.path()),
            operator_config_home: Some(operator.as_path()),
        };
        assert_eq!(
            note_ignored(&config, &trusted),
            None,
            "the operator's own file"
        );
        assert_eq!(
            names_in("\u{feff}VOYAGE_API_KEY=sk-bom\n"),
            vec!["VOYAGE_API_KEY".to_string()],
            "a UTF-8 BOM at the start of the file does not hide the first setting"
        );

        #[cfg(unix)]
        {
            let checkout = tempfile::tempdir().expect("checkout");
            let link = checkout.path().join(".tachi").join("config.env");
            std::fs::create_dir_all(link.parent().expect("parent")).expect("parent");
            std::os::unix::fs::symlink(&config, &link)
                .expect("a project file that is a link to the operator's");
            assert_eq!(
                note_ignored(&link, &trusted),
                None,
                "a link to the operator's file is the operator's file"
            );
            assert!(
                note_ignored(&link, &no_trusted_files()).is_some(),
                "without the operator's home it is a file that was ignored"
            );
        }
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
        assert!(
            line.contains(
                "10 daemon settings: TACHI_SETTING_0, TACHI_SETTING_1, TACHI_SETTING_2, \
                 TACHI_SETTING_3, TACHI_SETTING_4, TACHI_SETTING_5, TACHI_SETTING_6, \
                 TACHI_SETTING_7, and 2 more."
            ),
            "{line}"
        );
    }

    /// However much a file holds, the line stays short.
    #[test]
    fn the_line_stays_short() {
        let body: String = (0..40)
            .map(|index| format!("TACHI_{}_{index}=1\n", "L".repeat(NAME_LONGEST - 12)))
            .collect();
        let (_dir, path) = file_with(&body);
        let settings = daemon_settings_in(&path).expect("the file is read");
        let home = tempfile::tempdir().expect("tempdir");
        let line = ignored_line(&path, &settings, Some(home.path()));
        assert!(line.len() < 2000, "{} bytes: {line}", line.len());
    }
}
