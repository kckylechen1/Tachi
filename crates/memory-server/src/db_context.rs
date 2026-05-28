//! Active database targeting — makes MCP responses explicit about which DBs are queried.

use crate::MemoryServer;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub(crate) struct DbContext {
    pub requested_project: Option<String>,
    pub requested_domain: Option<String>,
    pub global_db: PathBuf,
    pub bound_project_db: Option<PathBuf>,
    pub effective_targets: Vec<String>,
    pub hint: Option<String>,
}

pub(crate) fn describe_db_context(
    server: &MemoryServer,
    project: Option<&str>,
    domain: Option<&str>,
) -> DbContext {
    let global_db = server.global_db_path_buf();
    let bound_project_db = server.project_db_path_buf();

    let (effective_targets, hint) = match project {
        Some(name) => {
            let named = MemoryServer::resolve_named_project_db_path(name)
                .map(|p| p.display().to_string())
                .unwrap_or_else(|_| format!("~/.tachi/projects/{name}/memory.db (missing)"));
            (
                vec![format!("named project `{name}` → {named}")],
                Some(format!(
                    "Searching only project `{name}`. Omit `project` to search global + the daemon-bound project DB."
                )),
            )
        }
        None => {
            let mut targets = vec![format!("global → {}", global_db.display())];
            if let Some(ref bound) = bound_project_db {
                targets.push(format!(
                    "daemon-bound project → {} ({})",
                    bound.display(),
                    project_label_from_path(bound)
                ));
            }
            let hint = bound_project_db.as_ref().map(|bound| {
                format!(
                    "Daemon is bound to `{}`. Code workspace may differ from this DB — pass `project=\"<name>\"` to query `~/.tachi/projects/<name>/memory.db` instead.",
                    bound.display()
                )
            });
            (targets, hint)
        }
    };

    DbContext {
        requested_project: project.map(str::to_string),
        requested_domain: domain.map(str::to_string),
        global_db,
        bound_project_db,
        effective_targets,
        hint,
    }
}

fn project_label_from_path(path: &Path) -> String {
    path.parent()
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str())
        .unwrap_or("project")
        .to_string()
}

pub(crate) fn format_db_context_markdown(ctx: &DbContext) -> String {
    let mut lines = vec!["### Database context".to_string()];
    if let Some(project) = &ctx.requested_project {
        lines.push(format!("- **Requested project:** `{project}`"));
    } else {
        lines.push("- **Requested project:** _(none — using daemon defaults)_".to_string());
    }
    if let Some(domain) = &ctx.requested_domain {
        lines.push(format!("- **Domain filter:** `{domain}`"));
    }
    for target in &ctx.effective_targets {
        lines.push(format!("- {target}"));
    }
    if let Some(hint) = &ctx.hint {
        lines.push(format!("\n> {hint}"));
    }
    lines.join("\n")
}

pub(crate) fn diagnostics_footer() -> &'static str {
    "\n---\n_Full system diagnostics: `tachi_status` or `tachi_doctor`. Memory/wiki tools return recall data only._"
}
