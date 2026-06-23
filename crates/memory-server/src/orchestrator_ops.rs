//! Persistent orchestrator state: TODOs and handoff packets (#157).
//!
//! Stored in global `hard_state` under namespace `orchestrator` (survives compaction).

use crate::server_state::MemoryServer;
use crate::tool_params::TachiOrchestratorParams;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::json;

const ORCHESTRATOR_NS: &str = "orchestrator";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum TodoStatus {
    Pending,
    InProgress,
    Blocked,
    Done,
    Cancelled,
    Superseded,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct OrchestratorTodo {
    pub id: String,
    #[serde(default)]
    pub issue_ref: Option<String>,
    #[serde(default)]
    pub parent_id: Option<String>,
    pub agent: String,
    pub status: TodoStatus,
    pub content: String,
    #[serde(default)]
    pub blocked_reason: Option<String>,
    #[serde(default)]
    pub verification: Option<String>,
    #[serde(default)]
    pub references: Vec<String>,
    pub created_at: String,
    pub updated_at: String,
    #[serde(default)]
    pub completed_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct OrchestratorTodoList {
    pub task_id: String,
    pub todos: Vec<OrchestratorTodo>,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct HandoffPacket {
    pub task_id: String,
    pub objective: String,
    pub current_state: String,
    #[serde(default)]
    pub completed_steps: Vec<String>,
    #[serde(default)]
    pub remaining_steps: Vec<String>,
    #[serde(default)]
    pub files_touched: Vec<String>,
    #[serde(default)]
    pub commands_run: Vec<String>,
    #[serde(default)]
    pub tests_run: Vec<String>,
    #[serde(default)]
    pub known_blockers: Vec<String>,
    pub next_action: String,
    #[serde(default)]
    pub references: Vec<String>,
    #[serde(default)]
    pub newest_user_instruction: Option<String>,
    pub updated_at: String,
}

fn todos_key(task_id: &str) -> String {
    format!("todos:{task_id}")
}

fn handoff_key(task_id: &str) -> String {
    format!("handoff:{task_id}")
}

fn load_json<T: for<'de> Deserialize<'de>>(
    server: &MemoryServer,
    key: &str,
) -> Result<Option<T>, String> {
    server.with_global_store(|store| -> Result<Option<T>, String> {
        let raw = store
            .get_state_kv(ORCHESTRATOR_NS, key)
            .map_err(|e| format!("orchestrator get_state: {e}"))?;
        Ok(match raw {
            Some((json, _version)) => {
                let parsed: T = serde_json::from_str(&json)
                    .map_err(|e| format!("orchestrator parse {key}: {e}"))?;
                Some(parsed)
            }
            None => None,
        })
    })
}

fn save_json<T: Serialize>(server: &MemoryServer, key: &str, value: &T) -> Result<(), String> {
    let json = serde_json::to_string(value).map_err(|e| format!("orchestrator serialize: {e}"))?;
    server.with_global_store(|store| -> Result<(), String> {
        store
            .set_state(ORCHESTRATOR_NS, key, &json)
            .map_err(|e| format!("orchestrator set_state: {e}"))?;
        Ok(())
    })
}

fn list_state_rows(server: &MemoryServer) -> Result<Vec<memory_core::db::StateRow>, String> {
    server.with_global_store_read(|store| {
        store
            .list_state(ORCHESTRATOR_NS)
            .map_err(|e| format!("orchestrator list_state: {e}"))
    })
}

fn has_incomplete_todos(list: &OrchestratorTodoList) -> bool {
    list.todos.iter().any(|todo| {
        !matches!(
            todo.status,
            TodoStatus::Done | TodoStatus::Cancelled | TodoStatus::Superseded
        )
    })
}

fn infer_active_task_id(server: &MemoryServer) -> Result<Option<String>, String> {
    let rows = list_state_rows(server)?;
    for row in rows.iter().filter(|row| row.key.starts_with("todos:")) {
        let Ok(list) = serde_json::from_str::<OrchestratorTodoList>(&row.value_json) else {
            continue;
        };
        if has_incomplete_todos(&list) {
            return Ok(Some(list.task_id));
        }
    }
    Ok(rows
        .iter()
        .filter_map(|row| row.key.strip_prefix("handoff:").map(str::to_string))
        .find(|task_id| !task_id.trim().is_empty()))
}

pub(crate) async fn handle_orchestrator(
    server: &MemoryServer,
    params: TachiOrchestratorParams,
) -> Result<String, String> {
    let action = params.action.trim().to_ascii_lowercase();
    let task_id = params
        .task_id
        .as_deref()
        .map(str::trim)
        .filter(|task_id| !task_id.is_empty())
        .map(str::to_string)
        .or_else(|| {
            if action == "recovery_briefing" {
                infer_active_task_id(server).ok().flatten()
            } else {
                None
            }
        });
    let Some(task_id) = task_id else {
        if action == "recovery_briefing" {
            return serde_json::to_string(&serde_json::json!({
                "task_id": null,
                "handoff": null,
                "todos": null,
                "incomplete_todos": [],
                "hint": "No active task found. Write a handoff or TODO to make recovery_briefing resumable.",
            }))
            .map_err(|e| format!("serialize recovery_briefing: {e}"));
        }
        return Err("task_id is required".to_string());
    };

    match action.as_str() {
        "todo_list" => {
            let list = load_json::<OrchestratorTodoList>(server, &todos_key(&task_id))?
                .unwrap_or_else(|| OrchestratorTodoList {
                    task_id: task_id.clone(),
                    todos: Vec::new(),
                    updated_at: Utc::now().to_rfc3339(),
                });
            serde_json::to_string(&list).map_err(|e| format!("serialize todo_list: {e}"))
        }
        "todo_update" => {
            let mut list = load_json::<OrchestratorTodoList>(server, &todos_key(&task_id))?
                .unwrap_or_else(|| OrchestratorTodoList {
                    task_id: task_id.clone(),
                    todos: Vec::new(),
                    updated_at: Utc::now().to_rfc3339(),
                });
            let now = Utc::now().to_rfc3339();
            let todo_id = params
                .todo_id
                .clone()
                .filter(|s| !s.trim().is_empty())
                .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
            let status = match params.todo_status.as_deref() {
                Some(raw) => parse_todo_status(raw)?,
                None => list
                    .todos
                    .iter()
                    .find(|todo| todo.id == todo_id)
                    .map(|todo| todo.status.clone())
                    .unwrap_or(TodoStatus::Pending),
            };
            if let Some(existing) = list.todos.iter_mut().find(|t| t.id == todo_id) {
                if let Some(content) = params.todo_content.as_ref().filter(|c| !c.trim().is_empty())
                {
                    existing.content = content.clone();
                }
                if let Some(agent) = params.agent.as_ref().filter(|a| !a.trim().is_empty()) {
                    existing.agent = agent.clone();
                }
                if status == TodoStatus::Done {
                    if existing.status != TodoStatus::Done {
                        existing.completed_at = Some(now.clone());
                    }
                } else {
                    existing.completed_at = None;
                }
                if status == TodoStatus::Blocked {
                    if let Some(reason) = params.blocked_reason.clone() {
                        existing.blocked_reason = Some(reason);
                    }
                } else {
                    existing.blocked_reason = None;
                }
                existing.status = status.clone();
                existing.updated_at = now.clone();
                if let Some(verification) = params.verification.clone() {
                    existing.verification = Some(verification);
                }
            } else {
                let content = params
                    .todo_content
                    .clone()
                    .filter(|c| !c.trim().is_empty())
                    .ok_or_else(|| "todo_content is required when creating a new todo".to_string())?;
                list.todos.push(OrchestratorTodo {
                    id: todo_id.clone(),
                    issue_ref: params.issue_ref.clone(),
                    parent_id: params.parent_todo_id.clone(),
                    agent: params
                        .agent
                        .clone()
                        .unwrap_or_else(|| "unknown".to_string()),
                    status: status.clone(),
                    content,
                    blocked_reason: params.blocked_reason.clone(),
                    verification: params.verification.clone(),
                    references: params.references.clone(),
                    created_at: now.clone(),
                    updated_at: now.clone(),
                    completed_at: if status == TodoStatus::Done {
                        Some(now.clone())
                    } else {
                        None
                    },
                });
            }
            list.updated_at = now;
            save_json(server, &todos_key(&task_id), &list)?;
            serde_json::to_string(&json!({
                "ok": true,
                "task_id": task_id,
                "todo_count": list.todos.len(),
            }))
            .map_err(|e| format!("serialize todo_update: {e}"))
        }
        "handoff_write" => {
            let objective = params
                .objective
                .clone()
                .filter(|s| !s.trim().is_empty())
                .ok_or_else(|| "objective is required for handoff_write".to_string())?;
            let packet = HandoffPacket {
                task_id: task_id.clone(),
                objective,
                current_state: params.current_state.clone().unwrap_or_default(),
                completed_steps: params.completed_steps.clone(),
                remaining_steps: params.remaining_steps.clone(),
                files_touched: params.files_touched.clone(),
                commands_run: params.commands_run.clone(),
                tests_run: params.tests_run.clone(),
                known_blockers: params.known_blockers.clone(),
                next_action: params
                    .next_action
                    .clone()
                    .unwrap_or_else(|| "Resume from handoff packet.".to_string()),
                references: params.references.clone(),
                newest_user_instruction: params.newest_user_instruction.clone(),
                updated_at: Utc::now().to_rfc3339(),
            };
            save_json(server, &handoff_key(&task_id), &packet)?;
            serde_json::to_string(&json!({ "ok": true, "handoff": packet }))
                .map_err(|e| format!("serialize handoff_write: {e}"))
        }
        "handoff_read" => {
            let packet = load_json::<HandoffPacket>(server, &handoff_key(&task_id))?;
            serde_json::to_string(&json!({ "task_id": task_id, "handoff": packet }))
                .map_err(|e| format!("serialize handoff_read: {e}"))
        }
        "recovery_briefing" => {
            let todos = load_json::<OrchestratorTodoList>(server, &todos_key(&task_id))?;
            let handoff = load_json::<HandoffPacket>(server, &handoff_key(&task_id))?;
            let incomplete: Vec<_> = todos
                .as_ref()
                .map(|list| {
                    list.todos
                        .iter()
                        .filter(|t| {
                            !matches!(
                                t.status,
                                TodoStatus::Done | TodoStatus::Cancelled | TodoStatus::Superseded
                            )
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            serde_json::to_string(&json!({
                "task_id": task_id,
                "handoff": handoff,
                "todos": todos,
                "incomplete_todos": incomplete,
                "hint": "Inject handoff + active TODOs into worker prompt before resume.",
            }))
            .map_err(|e| format!("serialize recovery_briefing: {e}"))
        }
        other => Err(format!(
            "Invalid orchestrator action '{other}'. Use todo_list, todo_update, handoff_write, handoff_read, recovery_briefing."
        )),
    }
}

fn parse_todo_status(raw: &str) -> Result<TodoStatus, String> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "pending" => Ok(TodoStatus::Pending),
        "in_progress" | "in-progress" | "working" => Ok(TodoStatus::InProgress),
        "blocked" => Ok(TodoStatus::Blocked),
        "done" | "completed" => Ok(TodoStatus::Done),
        "cancelled" | "canceled" => Ok(TodoStatus::Cancelled),
        "superseded" => Ok(TodoStatus::Superseded),
        other => Err(format!("Unknown todo status '{other}'")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_status_aliases() {
        assert_eq!(
            parse_todo_status("in_progress").unwrap(),
            TodoStatus::InProgress
        );
        assert_eq!(parse_todo_status("done").unwrap(), TodoStatus::Done);
    }
}
