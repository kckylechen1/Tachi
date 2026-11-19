//! Persistent TODOs and handoff packets used by task intake.
//!
//! The existing `orchestrator` hard-state namespace and stored shapes are retained.

use crate::server_state::MemoryServer;
use serde::{Deserialize, Serialize};

// No TTL/backfill: a TODO list can reopen after all items finish, and handoff
// current_state is free text, not a terminal-state enum. Neither row has a safe
// terminal-write boundary; cleanup must not infer one from prose or a snapshot.
const ORCHESTRATOR_NS: &str = "orchestrator";
/// A todo update reads, modifies, and writes one shared list. Retrying a small,
/// fixed number of CAS conflicts preserves independent concurrent updates without
/// turning the orchestrator into an unbounded write loop.
const TODO_UPDATE_MAX_ATTEMPTS: usize = 3;

#[cfg(test)]
type TodoUpdateSnapshotCallback = Box<dyn FnOnce(&MemoryServer, &str) + Send>;

#[cfg(test)]
struct TodoUpdateSnapshotHook {
    task_key: String,
    callback: TodoUpdateSnapshotCallback,
}

#[cfg(test)]
fn todo_update_snapshot_hook() -> &'static std::sync::Mutex<Option<TodoUpdateSnapshotHook>> {
    static HOOK: std::sync::OnceLock<std::sync::Mutex<Option<TodoUpdateSnapshotHook>>> =
        std::sync::OnceLock::new();
    HOOK.get_or_init(|| std::sync::Mutex::new(None))
}

#[cfg(test)]
pub(crate) fn set_todo_update_snapshot_hook(
    task_key: impl Into<String>,
    callback: impl FnOnce(&MemoryServer, &str) + Send + 'static,
) {
    let mut hook = todo_update_snapshot_hook()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    assert!(
        hook.is_none(),
        "todo-update snapshot hook already installed"
    );
    *hook = Some(TodoUpdateSnapshotHook {
        task_key: task_key.into(),
        callback: Box::new(callback),
    });
}

#[cfg(test)]
fn run_todo_update_snapshot_hook(server: &MemoryServer, key: &str) {
    let hook = {
        let mut slot = todo_update_snapshot_hook()
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if slot.as_ref().is_some_and(|hook| hook.task_key == key) {
            slot.take()
        } else {
            None
        }
    };
    if let Some(hook) = hook {
        (hook.callback)(server, key);
    }
}

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

/// Fields consumed by the existing TODO update; no tool action or handoff payload.
#[derive(Default)]
pub(crate) struct TodoUpdate {
    pub todo_id: Option<String>,
    pub todo_content: Option<String>,
    pub todo_status: Option<String>,
    pub parent_todo_id: Option<String>,
    pub agent: Option<String>,
    pub issue_ref: Option<String>,
    pub blocked_reason: Option<String>,
    pub verification: Option<String>,
    pub references: Vec<String>,
}

fn todos_key(task_id: &str) -> String {
    format!("todos:{task_id}")
}

fn handoff_key(task_id: &str) -> String {
    format!("handoff:{task_id}")
}

fn load_json_with_version<T: for<'de> Deserialize<'de>>(
    server: &MemoryServer,
    key: &str,
) -> Result<Option<(T, u32)>, String> {
    server.with_global_store(|store| -> Result<Option<(T, u32)>, String> {
        let raw = store
            .get_state_kv(ORCHESTRATOR_NS, key)
            .map_err(|e| format!("orchestrator get_state: {e}"))?;
        raw.map(|(json, version)| {
            serde_json::from_str(&json)
                .map(|parsed| (parsed, version))
                .map_err(|e| format!("orchestrator parse {key}: {e}"))
        })
        .transpose()
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

fn save_json_if_version<T: Serialize>(
    server: &MemoryServer,
    key: &str,
    value: &T,
    version: Option<u32>,
) -> Result<bool, String> {
    let json = serde_json::to_string(value).map_err(|e| format!("orchestrator serialize: {e}"))?;
    server.with_global_store(|store| {
        match version {
            Some(version) => store.set_state_if_version(ORCHESTRATOR_NS, key, &json, version),
            None => store.insert_state_if_absent(ORCHESTRATOR_NS, key, &json),
        }
        .map_err(|e| format!("orchestrator compare-and-set: {e}"))
    })
}

fn required_task_id(task_id: &str) -> Result<String, String> {
    let task_id = task_id.trim();
    if task_id.is_empty() {
        return Err("task_id is required".to_string());
    }
    Ok(task_id.to_string())
}

/// Update one TODO without constructing a tool request or JSON response.
pub(crate) fn update_todo(
    server: &MemoryServer,
    task_id: &str,
    params: TodoUpdate,
) -> Result<usize, String> {
    let task_id = required_task_id(task_id)?;
    let todo_id = params
        .todo_id
        .clone()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let key = todos_key(&task_id);
    for attempt in 0..TODO_UPDATE_MAX_ATTEMPTS {
        let (mut list, state_version) =
            match load_json_with_version::<OrchestratorTodoList>(server, &key)? {
                Some((list, version)) => (list, Some(version)),
                None => (
                    OrchestratorTodoList {
                        task_id: task_id.clone(),
                        todos: Vec::new(),
                        updated_at: memcore::time_compat::rfc3339_compat(jiff::Timestamp::now()),
                    },
                    None,
                ),
            };
        // Test-only controlled interleaving: a competing write after this
        // snapshot must make the insert/CAS fail and force a fresh merge.
        #[cfg(test)]
        run_todo_update_snapshot_hook(server, &key);
        let now = memcore::time_compat::rfc3339_compat(jiff::Timestamp::now());
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
            if let Some(content) = params
                .todo_content
                .as_ref()
                .filter(|c| !c.trim().is_empty())
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
        if save_json_if_version(server, &key, &list, state_version)? {
            return Ok(list.todos.len());
        }
        if attempt + 1 == TODO_UPDATE_MAX_ATTEMPTS {
            return Err(format!(
                "todo_update could not preserve concurrent updates for task '{task_id}' after {TODO_UPDATE_MAX_ATTEMPTS} attempts"
            ));
        }
    }
    unreachable!("todo update either persisted or returned a conflict error")
}

/// Persist the existing handoff shape, with its timestamp owned by the writer.
pub(crate) fn write_handoff(
    server: &MemoryServer,
    mut packet: HandoffPacket,
) -> Result<HandoffPacket, String> {
    packet.task_id = required_task_id(&packet.task_id)?;
    if packet.objective.trim().is_empty() {
        return Err("objective is required for handoff_write".to_string());
    }
    packet.updated_at = memcore::time_compat::rfc3339_compat(jiff::Timestamp::now());
    save_json(server, &handoff_key(&packet.task_id), &packet)?;
    Ok(packet)
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
