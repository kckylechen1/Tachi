use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use rusqlite::Connection;

use super::*;

pub(super) fn read_cc_switch_skills(db_path: &Path) -> Result<Vec<CcSwitchSkillRow>, String> {
    if !db_path.exists() {
        return Ok(Vec::new());
    }

    let conn = Connection::open(db_path).map_err(|e| format!("open cc-switch db: {e}"))?;
    let table_exists: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='skills')",
            [],
            |row| row.get::<_, i64>(0),
        )
        .map(|value| value != 0)
        .map_err(|e| format!("inspect cc-switch skills table: {e}"))?;
    if !table_exists {
        return Ok(Vec::new());
    }

    let mut stmt = conn
        .prepare(
            "SELECT name, directory, enabled_claude, enabled_codex, enabled_gemini,
                    enabled_opencode, enabled_hermes, content_hash
             FROM skills
             ORDER BY name",
        )
        .map_err(|e| format!("prepare cc-switch skill query: {e}"))?;

    let rows = stmt
        .query_map([], |row| {
            let mut enabled_hosts = Vec::new();
            for (idx, host) in [
                (2, "claude"),
                (3, "codex"),
                (4, "gemini"),
                (5, "opencode"),
                (6, "hermes"),
            ] {
                let enabled: i64 = row.get(idx)?;
                if enabled != 0 {
                    enabled_hosts.push(host.to_string());
                }
            }
            Ok(CcSwitchSkillRow {
                name: row.get(0)?,
                directory: row.get(1)?,
                enabled_hosts,
                content_hash: row.get(7)?,
            })
        })
        .map_err(|e| format!("query cc-switch skills: {e}"))?;

    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|e| format!("read cc-switch skill row: {e}"))
}

pub(super) fn build_cc_switch_projection_status(
    home: &Path,
    hosts: &[&str],
    cc_switch_skills: &[CcSwitchSkillRow],
    entries: &[SkillEntryStatus],
) -> Vec<CcSwitchProjectionStatus> {
    let host_set: BTreeSet<&str> = hosts.iter().copied().collect();
    let check_hosts: Vec<&str> = ["claude", "codex", "gemini"]
        .into_iter()
        .filter(|host| host_set.contains(host))
        .collect();
    let cc_switch_root = home.join(".cc-switch").join("skills");

    cc_switch_skills
        .iter()
        .map(|skill| {
            let mut issues = Vec::new();
            let mut checks = Vec::new();
            let source_hash = find_entry(entries, "cc-switch", &skill.name)
                .and_then(|entry| entry.hash.as_deref());
            for host in &check_hosts {
                let expected = skill.enabled_hosts.iter().any(|h| h == host);
                let actual = find_entry(entries, host, &skill.name);
                if expected && actual.is_none() {
                    issues.push(format!("missing_projection:{host}"));
                } else if !expected && actual.is_some() {
                    issues.push(format!("unexpected_projection:{host}"));
                }

                let check = projection_check(
                    home,
                    host,
                    &skill.name,
                    actual,
                    &cc_switch_root,
                    source_hash,
                );
                if expected
                    && check.exists
                    && !check.points_to_cc_switch
                    && !check.content_matches_cc_switch
                {
                    issues.push(format!("projection_content_drift:{host}"));
                }
                checks.push(check);
            }

            let status = if issues.is_empty() {
                if skill
                    .enabled_hosts
                    .iter()
                    .any(|host| check_hosts.contains(&host.as_str()))
                {
                    "synced"
                } else {
                    "disabled"
                }
            } else {
                "drift"
            };

            CcSwitchProjectionStatus {
                name: skill.name.clone(),
                enabled_hosts: skill.enabled_hosts.clone(),
                checks,
                status: status.to_string(),
                issues,
            }
        })
        .collect()
}

fn find_entry<'a>(
    entries: &'a [SkillEntryStatus],
    store: &str,
    name: &str,
) -> Option<&'a SkillEntryStatus> {
    entries
        .iter()
        .find(|entry| entry.store == store && entry.name == name)
}

fn projection_check(
    home: &Path,
    host: &str,
    name: &str,
    entry: Option<&SkillEntryStatus>,
    cc_switch_root: &Path,
    cc_switch_hash: Option<&str>,
) -> HostProjectionCheck {
    let path = host_skill_path(home, host, name);
    let exists = entry.is_some();
    let is_symlink = entry.is_some_and(|entry| entry.is_symlink);
    let symlink_target = entry.and_then(|entry| entry.symlink_target.clone());
    let points_to_cc_switch = symlink_target
        .as_deref()
        .is_some_and(|target| Path::new(target).starts_with(cc_switch_root));
    let content_matches_cc_switch = entry
        .and_then(|entry| entry.hash.as_deref())
        .zip(cc_switch_hash)
        .is_some_and(|(actual, expected)| actual == expected);

    HostProjectionCheck {
        host: host.to_string(),
        path: path.display().to_string(),
        exists,
        is_symlink,
        symlink_target,
        points_to_cc_switch,
        content_matches_cc_switch,
    }
}

fn host_skill_path(home: &Path, host: &str, name: &str) -> PathBuf {
    match host {
        "claude" => home.join(".claude").join("skills").join(name),
        "codex" => home.join(".codex").join("skills").join(name),
        "gemini" => home.join(".gemini").join("skills").join(name),
        _ => home.join(format!(".{host}")).join("skills").join(name),
    }
}
