use crate::server_state::MemoryServer;
use chrono::Utc;
use regex::Regex;
use serde_json::{json, Value};
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::time::SystemTime;

#[derive(Debug, Clone)]
struct Frontmatter {
    title: Option<String>,
    summary: Option<String>,
    category: Option<String>,
    organize: Option<bool>,
    other_fields: Vec<(String, String)>,
}

/// 解析 Markdown 文件的 Frontmatter 和正文
fn parse_frontmatter(content: &str) -> (Option<Frontmatter>, &str) {
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
fn serialize_frontmatter(fm: &Frontmatter) -> String {
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

/// 判断任务卡片状态是否在 DB 中已经解决
fn is_task_resolved(
    server: &MemoryServer,
    card_id: Option<&str>,
    task_text: Option<&str>,
    raw_task_text: Option<&str>,
) -> bool {
    let check_entry = |entry: &memory_core::MemoryEntry| -> bool {
        if entry.archived {
            return true;
        }
        if entry.category == "kanban" {
            if let Some(status) = entry.metadata.get("status").and_then(|s| s.as_str()) {
                return status == "resolved" || status == "expired";
            }
        } else if entry.category == "handoff" {
            // 解析 Handoff 状态
            let has_status = entry
                .metadata
                .get("status")
                .and_then(|s| s.as_str())
                .is_some_and(|status| matches!(status, "acknowledged" | "promoted"));
            let is_ack = entry
                .metadata
                .get("acknowledged")
                .and_then(|a| a.as_bool())
                .unwrap_or(false);
            let has_handoff_ack = entry
                .metadata
                .get("handoff")
                .and_then(|h| h.get("acknowledged").and_then(|a| a.as_bool()))
                .unwrap_or(false);
            return has_status || is_ack || has_handoff_ack;
        }
        false
    };

    // 1. 如果有显式 card_id，优先精准检查
    if let Some(id) = card_id {
        // 先去 project DB 查，再去 global DB 查
        if server.has_project_db() {
            if let Ok(Some(entry)) =
                server.with_project_store_read(|store| store.get(id).map_err(|e| e.to_string()))
            {
                if check_entry(&entry) {
                    return true;
                }
            }
        }
        if let Ok(Some(entry)) =
            server.with_global_store_read(|store| store.get(id).map_err(|e| e.to_string()))
        {
            if check_entry(&entry) {
                return true;
            }
        }
    }

    // 2. 如果没有 card_id，根据文本内容模糊查匹配的 kanban/handoff
    if task_text.is_some() || raw_task_text.is_some() {
        let clean_text = task_text.unwrap_or("").trim();
        let raw_text = raw_task_text.unwrap_or("").trim();
        if !clean_text.is_empty() || !raw_text.is_empty() {
            let sql = "SELECT id, category, archived, metadata FROM memories WHERE category IN ('kanban', 'handoff') AND (summary = ?1 OR summary = ?2)";

            let mut matched = false;
            let query_store = |store: &mut memory_core::MemoryStore| -> Result<bool, String> {
                let mut stmt = store.connection().prepare(sql).map_err(|e| e.to_string())?;
                let mut rows = stmt
                    .query(rusqlite::params![clean_text, raw_text])
                    .map_err(|e| e.to_string())?;
                while let Some(row) = rows.next().map_err(|e| e.to_string())? {
                    let archived: bool = row.get(2).map_err(|e| e.to_string())?;
                    let category: String = row.get(1).map_err(|e| e.to_string())?;
                    let metadata_str: String = row.get(3).map_err(|e| e.to_string())?;
                    let metadata: Value = serde_json::from_str(&metadata_str).unwrap_or(json!({}));

                    let mut resolved = archived;
                    if category == "kanban" {
                        if let Some(status) = metadata.get("status").and_then(|s| s.as_str()) {
                            resolved = resolved || status == "resolved" || status == "expired";
                        }
                    } else if category == "handoff" {
                        let has_status = metadata
                            .get("status")
                            .and_then(|s| s.as_str())
                            .is_some_and(|status| matches!(status, "acknowledged" | "promoted"));
                        let is_ack = metadata
                            .get("acknowledged")
                            .and_then(|a| a.as_bool())
                            .unwrap_or(false);
                        let has_handoff_ack = metadata
                            .get("handoff")
                            .and_then(|h| h.get("acknowledged").and_then(|a| a.as_bool()))
                            .unwrap_or(false);
                        resolved = resolved || has_status || is_ack || has_handoff_ack;
                    }
                    if resolved {
                        return Ok(true);
                    }
                }
                Ok(false)
            };

            if server.has_project_db() {
                if let Ok(res) = server.with_project_store_read(|store| query_store(store)) {
                    if res {
                        matched = true;
                    }
                }
            }
            if !matched {
                if let Ok(res) = server.with_global_store_read(|store| query_store(store)) {
                    if res {
                        matched = true;
                    }
                }
            }
            if matched {
                return true;
            }
        }
    }

    false
}

/// 清洗并标准化 Markdown 行中的任务文本以作匹配
fn clean_task_text(text: &str) -> String {
    // 移除诸如 P0:, P1:, [P0], (P0) 之类的前缀
    static RE_PREFIX: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    let re_prefix = RE_PREFIX.get_or_init(|| {
        Regex::new(r"(?i)^\s*(p[0-9]\s*[:：\-]*\s*|\[p[0-9]\]\s*|\(p[0-9]\)\s*)").unwrap()
    });
    let text_no_prio = re_prefix.replace(text, "");

    // 移除开头的空格及符号
    text_no_prio
        .trim_start_matches(|c: char| {
            c.is_whitespace() || c == ':' || c == '：' || c == '-' || c == '•'
        })
        .trim()
        .to_string()
}

/// 扫描并就地勾选 Markdown 正文中的任务项
fn sync_tasks_in_content(server: &MemoryServer, content: &str) -> (String, bool) {
    static RE_TODO: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    static RE_CARD: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    let re_todo = RE_TODO.get_or_init(|| Regex::new(r"^(\s*[\-\*\+]\s+\[\s*\]\s+)(.+)$").unwrap());
    let re_card =
        RE_CARD.get_or_init(|| Regex::new(r"<!--\s*tachi:([a-zA-Z0-9_\-]+)\s*-->").unwrap());

    let mut modified = false;
    let mut new_lines = Vec::new();

    for line in content.lines() {
        if let Some(caps) = re_todo.captures(line) {
            let prefix = caps.get(1).unwrap().as_str();
            let body = caps.get(2).unwrap().as_str();

            // 提取显式 card_id
            let card_id = re_card.captures(body).map(|c| c.get(1).unwrap().as_str());

            // 提取并清洗任务文本
            // 需要去掉注释本身，以便进行文本匹配
            let body_no_comment = re_card.replace_all(body, "");
            let task_text = clean_task_text(&body_no_comment);
            let raw_task_text = body_no_comment.trim().to_string();

            if is_task_resolved(server, card_id, Some(&task_text), Some(&raw_task_text)) {
                // 还原去除多余字符后的格式
                let mut new_prefix_clean = String::new();
                let mut found_bracket = false;
                for c in prefix.chars() {
                    if c == '[' {
                        new_prefix_clean.push_str("[x]");
                        found_bracket = true;
                    } else if found_bracket {
                        if c == ']' {
                            found_bracket = false;
                        }
                    } else {
                        new_prefix_clean.push(c);
                    }
                }
                new_lines.push(format!("{}{}", new_prefix_clean, body));
                modified = true;
                continue;
            }
        }
        new_lines.push(line.to_string());
    }

    (new_lines.join("\n"), modified)
}

/// 校验路径是否逃逸出 docs 根目录，并安全规范化
fn secure_join(root: &Path, rel_part: &str) -> Result<PathBuf, String> {
    let canonical_root = root
        .canonicalize()
        .map_err(|e| format!("Failed to canonicalize root: {e}"))?;

    // 检查是否有符号链接或 .. 跨越
    let mut cursor = root.to_path_buf();
    for component in Path::new(rel_part).components() {
        match component {
            Component::Normal(part) => {
                cursor.push(part);
                if let Ok(meta) = fs::symlink_metadata(&cursor) {
                    if meta.file_type().is_symlink() {
                        return Err(format!(
                            "Symlink traversal detected at '{}'",
                            cursor.display()
                        ));
                    }
                }
            }
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(format!("Escape path validation failed for '{}'", rel_part));
            }
        }
    }

    // 如果目标目录已存在，检查 canonical 关系
    if cursor.exists() {
        let canonical_cursor = cursor
            .canonicalize()
            .map_err(|e| format!("Failed to canonicalize target path: {e}"))?;
        if !canonical_cursor.starts_with(&canonical_root) {
            return Err(format!(
                "Path '{}' escapes root '{}'",
                cursor.display(),
                root.display()
            ));
        }
    }

    // Final safeguard: verify the resolved path doesn't escape via any TOCTOU race
    // For new (non-existent) files, canonicalize the parent directory instead
    if let Some(parent) = cursor.parent() {
        if let Ok(canonical_parent) = parent.canonicalize() {
            if canonical_parent != canonical_root && !canonical_parent.starts_with(&canonical_root)
            {
                return Err(format!(
                    "Resolved path '{}' escapes root after canonicalize",
                    cursor.display()
                ));
            }
        }
    }

    Ok(cursor)
}

fn is_archive_dir(path: &Path) -> bool {
    path.file_name().is_some_and(|name| name == "archive")
}

fn is_hidden_or_config_dir(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| {
            name.starts_with('.')
                || matches!(
                    name,
                    "node_modules"
                        | "__pycache__"
                        | ".git"
                        | ".github"
                        | ".claude"
                        | ".cursor"
                        | ".gemini"
                        | ".codex"
                        | ".vscode"
                        | ".idea"
                )
        })
}

fn is_markdown_file(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("md"))
}

/// 递归扫描指定目录下的所有 Markdown 文件，排除 archive 目录并按字典序排序
fn scan_md_files_recursive(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut queue = vec![dir.to_path_buf()];
    while let Some(current_dir) = queue.pop() {
        if let Ok(entries) = fs::read_dir(&current_dir) {
            for entry in entries.filter_map(|e| e.ok()) {
                let path = entry.path();
                let Ok(meta) = fs::symlink_metadata(&path) else {
                    continue;
                };
                if meta.file_type().is_symlink() {
                    continue;
                }
                if meta.is_dir() {
                    if is_archive_dir(&path) || is_hidden_or_config_dir(&path) {
                        continue;
                    }
                    queue.push(path);
                } else if meta.is_file() && is_markdown_file(&path) {
                    files.push(path);
                }
            }
        }
    }
    files.sort();
    files
}

fn unique_archive_target(archive_dir: &Path, stem: &str) -> (PathBuf, String) {
    let timestamp = Utc::now().timestamp_millis();
    for suffix in 0..1000 {
        let filename = if suffix == 0 {
            format!("{}.{}.md", stem, timestamp)
        } else {
            format!("{}.{}.{}.md", stem, timestamp, suffix)
        };
        let path = archive_dir.join(&filename);
        if !path.exists() {
            return (path, filename);
        }
    }
    let filename = format!(
        "{}.{}.{}.md",
        stem,
        timestamp,
        uuid::Uuid::new_v4().as_simple()
    );
    (archive_dir.join(&filename), filename)
}

/// LLM 辅助分类与元数据提取，支持在测试模式或 LLM 异常时的启发式 fallback
async fn classify_and_extract_metadata(
    server: &MemoryServer,
    source_path: &str,
    content: &str,
) -> (String, String, String) {
    #[cfg(test)]
    {
        let _ = server;
        get_test_fallback_metadata(source_path, content)
    }

    #[cfg(not(test))]
    {
        let system_prompt = "You are an expert software engineer organizing a project wiki/documentation. \
Analyze the provided document source path and content. Determine the most appropriate category path, a concise title, and a brief summary (under 100 characters). \
\
Available standard categories:\
1. docs/engineering/architecture (for specs, system design, architectural decisions)\
2. docs/engineering/devops (for setups, deployments, SOPs, CI/CD)\
3. docs/engineering/code-review (for style guides, API contracts, ADRs)\
4. docs/engineering/debugging (for troubleshooting, incident reports, post-mortems)\
5. docs/product/<product_name> (PRDs, roadmaps, features. Replace <product_name> with actual name in lowercase, e.g. docs/product/hyperion)\
6. docs/agent/<agent_name> (Agent identity, profiles, handoff configs. Replace <agent_name> with actual name in lowercase, e.g. docs/agent/antigravity)\
\
Respond ONLY with a JSON object. No markdown wrapping except the raw JSON content:\
{\
  \"category_path\": \"docs/engineering/architecture\",\
  \"title\": \"Document Title\",\
  \"summary\": \"Short 1-sentence summary\"\
}";
        let user_prompt = format!(
            "Source Path: {}\n\nContent (preview):\n{}",
            source_path,
            content.chars().take(4000).collect::<String>()
        );

        match server
            .llm
            .call_extract_llm(system_prompt, &user_prompt, None, 0.2, 500)
            .await
        {
            Ok(resp) => {
                if let Ok(json_str) = crate::llm::LlmClient::extract_json_payload(&resp) {
                    if let Ok(val) = serde_json::from_str::<Value>(json_str) {
                        let category_path = val
                            .get("category_path")
                            .and_then(|v| v.as_str())
                            .unwrap_or("docs/engineering/architecture")
                            .to_string();
                        let title = val
                            .get("title")
                            .and_then(|v| v.as_str())
                            .unwrap_or("Untitled Document")
                            .to_string();
                        let summary = val
                            .get("summary")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        return (category_path, title, summary);
                    }
                }
                get_test_fallback_metadata(source_path, content)
            }
            Err(e) => {
                tracing::warn!(
                    "[wiki_organize] LLM classification error: {}; falling back",
                    e
                );
                get_test_fallback_metadata(source_path, content)
            }
        }
    }
}

fn get_test_fallback_metadata(source_path: &str, content: &str) -> (String, String, String) {
    let path = Path::new(source_path);
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("untitled");
    let title = stem.replace('-', " ").replace('_', " ");

    // 粗略 summary 提取
    let first_line = content
        .lines()
        .find(|line| !line.trim().is_empty() && !line.starts_with("---"))
        .unwrap_or("")
        .trim_start_matches(|c| c == '#' || c == ' ')
        .trim();
    let summary = if first_line.is_empty() {
        content.chars().take(80).collect::<String>()
    } else {
        first_line.chars().take(80).collect::<String>()
    };

    let stem_lower = stem.to_lowercase();
    let category_path = if stem_lower.contains("agent") {
        "docs/agent/test_agent".to_string()
    } else if stem_lower.contains("prd") || stem_lower.contains("product") {
        "docs/product/test_product".to_string()
    } else if stem_lower.contains("deploy")
        || stem_lower.contains("devops")
        || stem_lower.contains("setup")
    {
        "docs/engineering/devops".to_string()
    } else if stem_lower.contains("review")
        || stem_lower.contains("contract")
        || stem_lower.contains("adr")
    {
        "docs/engineering/code-review".to_string()
    } else if stem_lower.contains("debug")
        || stem_lower.contains("troubleshoot")
        || stem_lower.contains("fix")
    {
        "docs/engineering/debugging".to_string()
    } else {
        "docs/engineering/architecture".to_string()
    };

    (category_path, title, summary)
}

/// 执行 docs 整理与索引生成的核心主流程
pub(crate) async fn handle_wiki_organize(
    server: &MemoryServer,
    dir_path: &str,
    dry_run: bool,
) -> Result<String, String> {
    let root = Path::new(dir_path);
    if !root.is_dir() {
        return Err(format!("Provided path '{}' is not a directory", dir_path));
    }

    let canonical_root = root
        .canonicalize()
        .map_err(|e| format!("Failed to canonicalize docs path '{}': {e}", dir_path))?;

    // Confine to known workspace roots: CWD, home directory, temp, or global_db_path parent
    let canonical_bases: Vec<PathBuf> = [
        std::env::current_dir().ok(),
        dirs::home_dir(),
        Some(std::env::temp_dir()),
        server.global_db_path.parent().map(|p| p.to_path_buf()),
    ]
    .iter()
    .filter_map(|opt| opt.as_ref().and_then(|p| p.canonicalize().ok()))
    .collect();
    let confined = canonical_bases
        .iter()
        .any(|base| canonical_root.starts_with(base));
    if !confined {
        return Err(format!(
            "dir_path '{}' is outside workspace roots (cwd, home, temp, or DB parent)",
            dir_path
        ));
    }

    // 白名单保护文件列表
    let whitelist = ["README.md", "INSTALL.md", "_index.md"];

    let mut moved_count = 0;
    let mut synced_count = 0;
    let mut log_messages = Vec::new();

    // 建立标准分类结构目录
    let standard_dirs = [
        "engineering/architecture",
        "engineering/devops",
        "engineering/code-review",
        "engineering/debugging",
        "product",
        "agent",
        "archive",
    ];
    for sub in &standard_dirs {
        if dry_run {
            if !canonical_root.join(sub).is_dir() {
                log_messages.push(format!(
                    "[dry-run] Would create standard directory '{}'",
                    sub
                ));
            }
            continue;
        }
        fs::create_dir_all(canonical_root.join(sub))
            .map_err(|e| format!("Failed to create standard directory '{}': {e}", sub))?;
    }

    // 1. 递归扫描 Markdown 文件并收集需要处理的列表
    // 我们用广度优先/手动队列进行递归扫描以避免递归溢出且保持对符号链接等安全的处理
    let mut queue = vec![canonical_root.clone()];
    let mut md_paths = Vec::new();
    while let Some(current_dir) = queue.pop() {
        let entries = fs::read_dir(&current_dir)
            .map_err(|e| format!("Failed to read dir {}: {e}", current_dir.display()))?;
        for entry in entries {
            let entry = entry.map_err(|e| e.to_string())?;
            let path = entry.path();
            let meta = fs::symlink_metadata(&path)
                .map_err(|e| format!("Failed to inspect path {}: {e}", path.display()))?;
            if meta.file_type().is_symlink() {
                continue;
            }
            if meta.is_dir() {
                // 排除 archive
                if is_archive_dir(&path) {
                    continue;
                }
                queue.push(path);
            } else if meta.is_file() && is_markdown_file(&path) {
                // 排除根目录白名单
                let filename = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                if path.parent() == Some(&canonical_root) && whitelist.contains(&filename) {
                    continue;
                }
                md_paths.push(path);
            }
        }
    }
    md_paths.sort();

    // 记录标准分类目录前缀
    let standard_subdirs = ["engineering/", "product/", "agent/"];

    // 2. 对扫描出的每个 Markdown 执行 Frontmatter 解析、分类和物理移动
    for path in md_paths {
        let relative_path = path
            .strip_prefix(&canonical_root)
            .map_err(|e| format!("Strip prefix failed: {e}"))?;
        let relative_str = relative_path.to_string_lossy().replace('\\', "/");

        let content = fs::read_to_string(&path)
            .map_err(|e| format!("Failed to read file {}: {e}", path.display()))?;

        let (fm_opt, body) = parse_frontmatter(&content);

        // 检查 organize 逃生舱
        if let Some(ref fm) = fm_opt {
            if fm.organize == Some(false) {
                log_messages.push(format!(
                    "Skipped '{}': organize is explicitly set to false",
                    relative_str
                ));

                // Task sync on organize:false files is best-effort — errors are
                // logged but must not abort the remaining file scan.
                let (new_body, task_modified) = sync_tasks_in_content(server, body);
                if task_modified {
                    if dry_run {
                        log_messages.push(format!(
                            "[dry-run] Would sync task checkmarks in-place: '{}'",
                            relative_str
                        ));
                        synced_count += 1;
                        continue;
                    }
                    let new_content = if let Some(ref fm) = fm_opt {
                        format!("{}{}", serialize_frontmatter(fm), new_body)
                    } else {
                        new_body
                    };
                    let tmp = path.with_extension("md.tmp");
                    if let Err(e) =
                        fs::write(&tmp, &new_content).and_then(|_| fs::rename(&tmp, &path))
                    {
                        log_messages.push(format!(
                            "WARN: task sync write failed for '{}': {e}",
                            relative_str
                        ));
                    } else {
                        synced_count += 1;
                    }
                }
                continue;
            }
        }

        // 确定该文件当前是否已经在标准分类目录中
        let is_already_categorized = standard_subdirs
            .iter()
            .any(|sub| relative_str.starts_with(sub));

        // 提取或预测分类
        let (target_category_path, title, summary) = if let Some(ref fm) = fm_opt {
            if let Some(ref cat) = fm.category {
                let cat_rel = cat.strip_prefix("docs/").unwrap_or(cat);
                if standard_subdirs
                    .iter()
                    .any(|sub| cat_rel.starts_with(sub) || format!("{}/", cat_rel).starts_with(sub))
                {
                    // 使用已有的合法 category
                    let standard_cat = format!("docs/{}", cat_rel);
                    (
                        standard_cat,
                        fm.title.clone().unwrap_or_else(|| {
                            relative_path
                                .file_stem()
                                .unwrap()
                                .to_string_lossy()
                                .to_string()
                        }),
                        fm.summary.clone().unwrap_or_default(),
                    )
                } else {
                    // 原 category 不合法，调用 LLM
                    classify_and_extract_metadata(server, &relative_str, &content).await
                }
            } else {
                classify_and_extract_metadata(server, &relative_str, &content).await
            }
        } else {
            classify_and_extract_metadata(server, &relative_str, &content).await
        };

        // 解析标准分类路径为相对于 docs/ 的路径
        // target_category_path 类似 "docs/engineering/architecture"
        let dest_rel_dir = target_category_path
            .strip_prefix("docs/")
            .unwrap_or(&target_category_path);

        // 构造目标物理路径
        let filename = path.file_name().ok_or("Invalid filename")?;
        let dest_dir = secure_join(&canonical_root, dest_rel_dir)?;
        let dest_path = dest_dir.join(filename);

        // 确保目的地目录的父目录存在
        if !dry_run {
            if let Some(parent) = dest_path.parent() {
                fs::create_dir_all(parent).map_err(|e| {
                    format!("Failed to create directory '{}': {e}", parent.display())
                })?;
            }
        }

        let dest_rel_path = format!("{}/{}", dest_rel_dir, filename.to_string_lossy());

        // 更新/写入 Frontmatter
        let mut fm = fm_opt.unwrap_or(Frontmatter {
            title: Some(title),
            summary: Some(summary),
            category: Some(dest_rel_dir.to_string()),
            organize: Some(true),
            other_fields: Vec::new(),
        });

        // 保证 category 正确且同步
        fm.category = Some(dest_rel_dir.to_string());

        // 就地任务状态检测与勾选
        let (new_body, task_modified) = sync_tasks_in_content(server, body);
        let final_content = format!("{}{}", serialize_frontmatter(&fm), new_body);

        if task_modified {
            synced_count += 1;
        }

        // 如果物理路径不需要移动 (即已经在标准目录，且目的地一致)
        if is_already_categorized && path == dest_path {
            if dry_run {
                if final_content != content {
                    log_messages.push(format!(
                        "[dry-run] Would sync tasks/frontmatter in-place: '{}'",
                        relative_str
                    ));
                }
                continue;
            }
            // 只写入可能更新后的内容（就地勾选/Frontmatter 补齐）
            fs::write(&path, &final_content)
                .map_err(|e| format!("Failed to update file {}: {e}", path.display()))?;
            log_messages.push(format!(
                "Synced tasks/frontmatter in-place: '{}'",
                relative_str
            ));
        } else {
            if dry_run {
                if dest_path.exists() {
                    let mtime_src = fs::metadata(&path)
                        .and_then(|m| m.modified())
                        .unwrap_or(SystemTime::UNIX_EPOCH);
                    let mtime_dest = fs::metadata(&dest_path)
                        .and_then(|m| m.modified())
                        .unwrap_or(SystemTime::UNIX_EPOCH);
                    if mtime_src >= mtime_dest {
                        log_messages.push(format!(
                            "[dry-run] Would move (newer) '{}' to '{}' and archive older destination",
                            relative_str, dest_rel_path
                        ));
                    } else {
                        log_messages.push(format!(
                            "[dry-run] Would archive '{}' to 'archive/' (destination '{}' is newer)",
                            relative_str, dest_rel_path
                        ));
                    }
                } else {
                    log_messages.push(format!(
                        "[dry-run] Would move '{}' to '{}'",
                        relative_str, dest_rel_path
                    ));
                }
                moved_count += 1;
                continue;
            }
            // 处理物理移动与同名冲突
            if dest_path.exists() {
                // 读修改时间 (mtime)
                let meta_src = fs::metadata(&path).map_err(|e| e.to_string())?;
                let meta_dest = fs::metadata(&dest_path).map_err(|e| e.to_string())?;

                let mtime_src = meta_src.modified().unwrap_or(SystemTime::UNIX_EPOCH);
                let mtime_dest = meta_dest.modified().unwrap_or(SystemTime::UNIX_EPOCH);

                let archive_dir = canonical_root.join("archive");
                fs::create_dir_all(&archive_dir).map_err(|e| {
                    format!(
                        "Failed to create archive directory '{}': {e}",
                        archive_dir.display()
                    )
                })?;
                let stem = path.file_stem().unwrap().to_string_lossy().to_string();

                if mtime_src >= mtime_dest {
                    // 源文件（当前文件）更新，覆盖 dest，并将 dest 上的旧文件移至 archive
                    let (archive_path, archive_filename) =
                        unique_archive_target(&archive_dir, &stem);

                    fs::rename(&dest_path, &archive_path).map_err(|e| {
                        format!(
                            "Failed to archive older file '{}' -> '{}': {e}",
                            dest_path.display(),
                            archive_path.display()
                        )
                    })?;

                    // Atomic write: write to temp file then rename to avoid corruption on crash
                    let tmp_dest = dest_path.with_extension("md.tmp");
                    fs::write(&tmp_dest, &final_content).map_err(|e| {
                        format!("Failed to write temp file {}: {e}", tmp_dest.display())
                    })?;
                    fs::rename(&tmp_dest, &dest_path).map_err(|e| {
                        format!(
                            "Failed to rename {} -> {}: {e}",
                            tmp_dest.display(),
                            dest_path.display()
                        )
                    })?;
                    fs::remove_file(&path).map_err(|e| {
                        format!("Failed to remove source file {}: {e}", path.display())
                    })?;

                    log_messages.push(format!(
                        "Moved (newer) '{}' to '{}' (archived older to 'archive/{}')",
                        relative_str, dest_rel_path, archive_filename
                    ));
                } else {
                    // 目的地文件更新，放弃移动源文件，而是直接把源文件归档
                    let (archive_path, archive_filename) =
                        unique_archive_target(&archive_dir, &stem);

                    // Atomic write to archive
                    let tmp_archive = archive_path.with_extension("md.tmp");
                    fs::write(&tmp_archive, &final_content)
                        .map_err(|e| format!("Failed to write archive temp: {e}"))?;
                    fs::rename(&tmp_archive, &archive_path)
                        .map_err(|e| format!("Failed to rename archive temp: {e}"))?;
                    fs::remove_file(&path)
                        .map_err(|e| format!("Failed to remove source file after archive: {e}"))?;

                    log_messages.push(format!(
                        "Archived older '{}' directly to 'archive/{}' (destination '{}' was newer)",
                        relative_str, archive_filename, dest_rel_path
                    ));
                }
            } else {
                // 无同名冲突，直接写新路径，删旧路径
                fs::write(&dest_path, &final_content).map_err(|e| {
                    format!(
                        "Failed to write to destination {}: {e}",
                        dest_path.display()
                    )
                })?;
                fs::remove_file(&path)
                    .map_err(|e| format!("Failed to remove source file {}: {e}", path.display()))?;
                log_messages.push(format!("Moved '{}' to '{}'", relative_str, dest_rel_path));
            }
            moved_count += 1;
        }
    }

    // 3. 重新扫描标准分类目录，生成 _index.md
    let mut index_lines = Vec::new();
    index_lines.push("# Tachi Workspace Documents Index".to_string());
    index_lines.push(format!("*Last Updated: {}*\n", Utc::now().to_rfc3339()));
    index_lines.push(
        "This document tree is automatically maintained by Tachi. Do not edit manually.\n"
            .to_string(),
    );

    // 收集所有标准分类目录下的文件以建目录树
    // 标准子目录包括：engineering/architecture, engineering/devops, engineering/code-review, engineering/debugging, product, agent
    let categories = [
        (
            "Engineering: Architecture Specifications",
            "engineering/architecture",
        ),
        ("Engineering: DevOps & SOP Playbooks", "engineering/devops"),
        (
            "Engineering: Code Reviews & Decisions",
            "engineering/code-review",
        ),
        (
            "Engineering: Debugging & Post-mortems",
            "engineering/debugging",
        ),
        ("Product Roadmaps & PRDs", "product"),
        ("Agent Identities & Handoffs", "agent"),
    ];

    for (title, rel_dir) in &categories {
        let dir = canonical_root.join(rel_dir);
        if dir.is_dir() {
            let paths = scan_md_files_recursive(&dir);

            if !paths.is_empty() {
                index_lines.push(format!("## {}", title));
                for path in paths {
                    let filename = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                    let relative_url = path
                        .strip_prefix(&canonical_root)
                        .unwrap()
                        .to_string_lossy()
                        .replace('\\', "/");

                    let file_content = match fs::read_to_string(&path) {
                        Ok(c) => c,
                        Err(e) => {
                            tracing::warn!("docs_ops: failed to read {}: {e}", path.display());
                            continue;
                        }
                    };
                    let (fm_opt, _) = parse_frontmatter(&file_content);

                    let doc_title = fm_opt
                        .as_ref()
                        .and_then(|f| f.title.clone())
                        .unwrap_or_else(|| filename.replace(".md", ""));
                    let doc_summary = fm_opt
                        .as_ref()
                        .and_then(|f| f.summary.clone())
                        .unwrap_or_default();

                    if doc_summary.is_empty() {
                        index_lines.push(format!("- [{}](<{}>)", doc_title, relative_url));
                    } else {
                        index_lines.push(format!(
                            "- [{}](<{}>) - {}",
                            doc_title, relative_url, doc_summary
                        ));
                    }
                }
                index_lines.push("".to_string());
            }
        }
    }

    let index_content = index_lines.join("\n");
    let index_path = canonical_root.join("_index.md");
    if dry_run {
        let existing = fs::read_to_string(&index_path).unwrap_or_default();
        if existing != index_content {
            log_messages.push("[dry-run] Would rebuild docs/_index.md".to_string());
        }
    } else if let Err(e) = fs::write(&index_path, &index_content) {
        log_messages.push(format!("WARN: failed to write _index.md: {e}"));
    }

    let result = json!({
        "status": "success",
        "dry_run": dry_run,
        "moved_files": moved_count,
        "synced_tasks": synced_count,
        "log": log_messages,
    });

    Ok(serde_json::to_string_pretty(&result).unwrap())
}
