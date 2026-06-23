#[derive(Debug, Clone)]
pub(super) struct Frontmatter {
    pub(super) title: Option<String>,
    pub(super) summary: Option<String>,
    pub(super) category: Option<String>,
    pub(super) organize: Option<bool>,
    pub(super) other_fields: Vec<(String, String)>,
}

/// 解析 Markdown 文件的 Frontmatter 和正文
pub(super) fn parse_frontmatter(content: &str) -> (Option<Frontmatter>, &str) {
    if !content.starts_with("---") {
        return (None, content);
    }

    let lines: Vec<&str> = content.lines().collect();
    if lines.is_empty() || lines[0] != "---" {
        return (None, content);
    }

    let mut end_idx = None;
    for i in 1..lines.len() {
        if lines[i] == "---" {
            end_idx = Some(i);
            break;
        }
    }

    let end_idx = match end_idx {
        Some(idx) => idx,
        None => return (None, content),
    };

    let mut title = None;
    let mut summary = None;
    let mut category = None;
    let mut organize = None;
    let mut other_fields = Vec::new();

    for i in 1..end_idx {
        let line = lines[i];
        if line.trim().is_empty() {
            continue;
        }
        if let Some(pos) = line.find(':') {
            let key = line[..pos].trim().to_string();
            let val = line[pos + 1..].trim();
            // 去除两端引号
            let val_clean = if (val.starts_with('"') && val.ends_with('"'))
                || (val.starts_with('\'') && val.ends_with('\''))
            {
                if val.len() >= 2 {
                    val[1..val.len() - 1].trim().to_string()
                } else {
                    val.to_string()
                }
            } else {
                val.to_string()
            };

            match key.as_str() {
                "title" => title = Some(val_clean),
                "summary" => summary = Some(val_clean),
                "category" => category = Some(val_clean),
                "organize" => {
                    organize = Some(val_clean.parse::<bool>().unwrap_or(true));
                }
                _ => other_fields.push((key, val_clean)),
            }
        }
    }

    // 找到正文的偏移量，防止换行符丢失
    let mut char_idx = 3; // "---"
    let mut delim_count = 0;
    let mut bytes_offset = 0;
    for (idx, ch) in content.char_indices() {
        if ch == '\n' {
            let line = &content[bytes_offset..idx].trim_end();
            if line == &"---" {
                delim_count += 1;
                if delim_count == 2 {
                    char_idx = idx + 1;
                    break;
                }
            }
            bytes_offset = idx + 1;
        }
    }
    // Handle closing "---" at end of file with no trailing newline
    if delim_count == 1 {
        let last_line = content[bytes_offset..].trim_end();
        if last_line == "---" {
            char_idx = content.len();
        }
    }

    let rest = if char_idx < content.len() {
        &content[char_idx..]
    } else {
        ""
    };

    (
        Some(Frontmatter {
            title,
            summary,
            category,
            organize,
            other_fields,
        }),
        rest,
    )
}

/// 序列化 Frontmatter 结构为 Markdown 头部
pub(super) fn serialize_frontmatter(fm: &Frontmatter) -> String {
    let mut s = String::new();
    s.push_str("---\n");
    if let Some(ref t) = fm.title {
        s.push_str(&format!("title: \"{}\"\n", t.replace('"', "\\\"")));
    }
    if let Some(ref sum) = fm.summary {
        s.push_str(&format!("summary: \"{}\"\n", sum.replace('"', "\\\"")));
    }
    if let Some(ref cat) = fm.category {
        s.push_str(&format!("category: \"{}\"\n", cat));
    }
    if let Some(org) = fm.organize {
        s.push_str(&format!("organize: {}\n", org));
    }
    for (k, v) in &fm.other_fields {
        if v.contains(' ') || v.contains(':') || v.contains('"') || v.contains('\'') {
            s.push_str(&format!("{}: \"{}\"\n", k, v.replace('"', "\\\"")));
        } else {
            s.push_str(&format!("{}: {}\n", k, v));
        }
    }
    s.push_str("---\n");
    s
}
