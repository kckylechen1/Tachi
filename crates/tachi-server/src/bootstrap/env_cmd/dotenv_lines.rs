//! Logical dotenv lines. A quoted value may span physical lines, so callers
//! that rewrite or scan dotenv files must treat it as one entry; otherwise a
//! rewrite leaves the tail of a secret behind and a scan reads it as keys.

use crate::vault_crypto::ZeroizingString;

pub(crate) struct DotenvEntry {
    /// 1-based physical line where the entry starts.
    pub(crate) line_no: usize,
    /// Entry text; physical lines of a multi-line value are joined with `\n`.
    pub(crate) text: ZeroizingString,
}

pub(crate) struct DotenvText {
    pub(crate) entries: Vec<DotenvEntry>,
    eol: &'static str,
    trailing_newline: bool,
}

impl DotenvText {
    pub(crate) fn parse(content: &str) -> Result<Self, String> {
        let eol = if content.contains("\r\n") {
            "\r\n"
        } else {
            "\n"
        };
        let mut entries = Vec::new();
        let mut lines = content.lines().enumerate();
        while let Some((idx, line)) = lines.next() {
            let mut text = ZeroizingString::new(line.to_string());
            if assignment_rhs(line).is_some_and(|rhs| open_quote(rhs).is_some()) {
                loop {
                    let Some((_, next)) = lines.next() else {
                        return Err(format!("line {}: quoted value is never closed", idx + 1));
                    };
                    text.as_mut_string().push('\n');
                    text.as_mut_string().push_str(next);
                    if assignment_rhs(&text).is_some_and(|rhs| open_quote(rhs).is_none()) {
                        break;
                    }
                }
            }
            entries.push(DotenvEntry {
                line_no: idx + 1,
                text,
            });
        }
        Ok(Self {
            entries,
            eol,
            trailing_newline: content.ends_with('\n'),
        })
    }

    /// Render replacement lines with the source file's line ending and its
    /// trailing-newline choice. An empty source gets a trailing newline.
    pub(crate) fn render(&self, lines: &[String]) -> String {
        let mut body = lines
            .iter()
            .map(|line| line.replace('\n', self.eol))
            .collect::<Vec<_>>()
            .join(self.eol);
        if !body.is_empty() && (self.trailing_newline || self.entries.is_empty()) {
            body.push_str(self.eol);
        }
        body
    }
}

/// Right-hand side of an assignment line, or `None` for blanks, comments and
/// lines without `=`.
pub(crate) fn assignment_rhs(line: &str) -> Option<&str> {
    let trimmed = line.trim_start();
    if trimmed.is_empty() || trimmed.starts_with('#') {
        return None;
    }
    let body = trimmed
        .strip_prefix("export ")
        .map(str::trim_start)
        .unwrap_or(trimmed);
    body.split_once('=').map(|(_, rhs)| rhs)
}

/// The quote character a value opens without closing, if any.
fn open_quote(rhs: &str) -> Option<char> {
    match closing_quote_end(rhs.trim_start()) {
        Some(None) => Some(rhs.trim_start().chars().next().unwrap_or('"')),
        _ => None,
    }
}

/// For a value starting with a quote: `Some(Some(i))` is the byte index just
/// past the closing quote, `Some(None)` means it never closes. `None` means
/// the value is unquoted.
fn closing_quote_end(value: &str) -> Option<Option<usize>> {
    let mut chars = value.char_indices();
    match chars.next() {
        Some((_, '\'')) => Some(value[1..].find('\'').map(|i| i + 2)),
        Some((_, '"')) => {
            let mut escaped = false;
            for (i, c) in chars {
                if escaped {
                    escaped = false;
                } else if c == '\\' {
                    escaped = true;
                } else if c == '"' {
                    return Some(Some(i + 1));
                }
            }
            Some(None)
        }
        _ => None,
    }
}

/// Trailing inline comment after a value, including its leading whitespace,
/// so a rewritten pointer line keeps it. Empty when there is none.
pub(crate) fn trailing_comment(rhs: &str) -> &str {
    let value = rhs.trim_start();
    match closing_quote_end(value) {
        Some(Some(end)) => {
            let rest = &value[end..];
            if rest.trim_start().starts_with('#') {
                rest
            } else {
                ""
            }
        }
        Some(None) => "",
        None => value
            .find(" #")
            .or_else(|| value.find("\t#"))
            .map_or("", |i| &value[i..]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn multi_line_quoted_values_are_one_entry() {
        let text =
            DotenvText::parse("A=1\nPRIVATE_KEY=\"first-line\nsecond=line\"\nB='x\ny=z'\nC=3\n")
                .expect("parse");
        let entries: Vec<&str> = text.entries.iter().map(|e| &*e.text).collect();
        assert_eq!(
            entries,
            [
                "A=1",
                "PRIVATE_KEY=\"first-line\nsecond=line\"",
                "B='x\ny=z'",
                "C=3"
            ]
        );
        assert_eq!(text.entries[3].line_no, 6);
    }

    #[test]
    fn unterminated_quote_is_refused() {
        let err = DotenvText::parse("A=\"never closed\nB=1\n")
            .err()
            .expect("unterminated quote must fail");
        assert!(err.contains("line 1"), "{err}");
    }

    #[test]
    fn render_keeps_line_endings_and_trailing_newline_choice() {
        let crlf = DotenvText::parse("A=1\r\nB=2\r\n").expect("parse");
        assert_eq!(crlf.render(&["A=1".into(), "B=x".into()]), "A=1\r\nB=x\r\n");
        let bare = DotenvText::parse("A=1").expect("parse");
        assert_eq!(bare.render(&["A=x".into()]), "A=x");
    }

    #[test]
    fn trailing_comment_survives_quoted_and_unquoted_values() {
        assert_eq!(
            trailing_comment("abc # rotation ticket"),
            " # rotation ticket"
        );
        assert_eq!(trailing_comment(" 'a # b'  # note"), "  # note");
        assert_eq!(trailing_comment("\"a \\\" # b\""), "");
        assert_eq!(trailing_comment("plain"), "");
    }
}
