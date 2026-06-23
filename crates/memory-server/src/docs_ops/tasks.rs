use crate::server_state::MemoryServer;
use regex::Regex;
use serde_json::{json, Value};

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
pub(super) fn sync_tasks_in_content(server: &MemoryServer, content: &str) -> (String, bool) {
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
