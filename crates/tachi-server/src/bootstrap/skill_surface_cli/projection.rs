use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OpenFlags};

use super::*;

pub(super) fn reconcile_codex_projections(entries: &mut [SkillEntryStatus]) {
    let canonical: BTreeMap<_, _> = entries
        .iter()
        .filter(|entry| entry.store == "agents")
        .map(|entry| (entry.name.clone(), entry.clone()))
        .collect();
    for entry in entries.iter_mut().filter(|entry| entry.store == "codex") {
        let marker = Path::new(&entry.path).join(".agents-codex-projection.json");
        if std::fs::symlink_metadata(&marker)
            .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound)
        {
            continue;
        }
        let source = read_codex_projection_source(&marker);
        let mut projection = SkillProjectionStatus {
            source: source.as_ref().ok().cloned(),
            status: "unverified".into(),
        };
        let expected = canonical.get(&entry.name);
        match source.and_then(|source| verify_codex_projection(expected, entry, &source)) {
            Ok(true) => {
                projection.status = "projected".into();
                entry.comparison_hash = expected.and_then(|source| source.hash.clone());
            }
            Ok(false) => {
                projection.status = "drift".into();
                entry.issues.push("codex_projection_content_drift".into());
            }
            Err(issue) => entry.issues.push(issue.into()),
        }
        entry.projection = Some(projection);
    }
}

fn read_codex_projection_source(marker: &Path) -> Result<String, &'static str> {
    // agents-sync writes a regular marker containing the canonical skill's
    // directory. A marker alone never grants equivalence to arbitrary content.
    if !std::fs::symlink_metadata(marker).is_ok_and(|metadata| metadata.is_file()) {
        return Err("invalid_codex_projection_marker");
    }
    let text = std::fs::read_to_string(marker).map_err(|_| "unreadable_codex_projection_marker")?;
    let marker: serde_json::Value =
        serde_json::from_str(&text).map_err(|_| "invalid_codex_projection_marker")?;
    marker
        .get("source")
        .and_then(|source| source.as_str())
        .map(str::to_string)
        .ok_or("invalid_codex_projection_marker")
}

fn verify_codex_projection(
    canonical: Option<&SkillEntryStatus>,
    projected: &SkillEntryStatus,
    source: &str,
) -> Result<bool, &'static str> {
    let canonical = canonical.ok_or("missing_codex_canonical_source")?;
    let source = Path::new(source);
    let expected =
        std::fs::canonicalize(&canonical.path).map_err(|_| "unreadable_codex_canonical_source")?;
    if !source.is_absolute() || std::fs::canonicalize(source).ok().as_ref() != Some(&expected) {
        return Err("codex_projection_source_mismatch");
    }
    let canonical_content = std::fs::read_to_string(Path::new(&canonical.path).join("SKILL.md"))
        .map_err(|_| "unreadable_codex_canonical_source")?;
    let projected_content = std::fs::read_to_string(Path::new(&projected.path).join("SKILL.md"))
        .map_err(|_| "unreadable_codex_projected_skill")?;
    if canonical.hash.as_deref() != Some(crate::utils::stable_hash(&canonical_content).as_str())
        || projected.hash.as_deref() != Some(crate::utils::stable_hash(&projected_content).as_str())
    {
        return Err("codex_projection_changed_during_scan");
    }
    let normalized = codex_frontmatter_projection(&canonical_content)
        .ok_or("invalid_codex_canonical_frontmatter")?;
    Ok(normalized == universal_newlines(&projected_content))
}

fn universal_newlines(content: &str) -> String {
    // Match Python Path.read_text used by the native agents-sync producer.
    content.replace("\r\n", "\n").replace('\r', "\n")
}

fn codex_frontmatter_projection(content: &str) -> Option<String> {
    // Exact agents-sync@af0b943 projection: preserve name/description and their
    // continuation lines, remove other frontmatter, preserve the entire body.
    // This is deliberately not a generic YAML or semantic-content comparison.
    let content = universal_newlines(content);
    let lines: Vec<_> = content.split_inclusive('\n').collect();
    if lines.first()?.trim() != "---" {
        return None;
    }
    let end = (1..lines.len()).find(|&index| lines[index].trim() == "---")?;
    let mut kept = String::new();
    let mut found = BTreeSet::new();
    let mut index = 1;
    while index < end {
        let line = lines[index];
        let key = if line.starts_with(char::is_whitespace) {
            None
        } else {
            line.split_once(':').map(|(key, _)| key)
        };
        if let Some(key @ ("name" | "description")) = key {
            found.insert(key);
            kept.push_str(line);
            index += 1;
            while index < end
                && (lines[index].trim().is_empty() || lines[index].starts_with(char::is_whitespace))
            {
                kept.push_str(lines[index]);
                index += 1;
            }
        } else {
            index += 1;
        }
    }
    if found.len() != 2 {
        return None;
    }
    Some(format!("---\n{kept}---\n{}", lines[end + 1..].concat()))
}

pub(super) fn read_cc_switch_skills(db_path: &Path) -> Result<Vec<CcSwitchSkillRow>, String> {
    if !db_path.exists() {
        return Ok(Vec::new());
    }

    let conn = Connection::open_with_flags(db_path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|e| format!("open cc-switch db: {e}"))?;
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
    let content_matches_cc_switch = entry.zip(cc_switch_hash).is_some_and(|(entry, expected)| {
        entry.hash.as_deref() == Some(expected)
            || entry.comparison_hash.as_deref() == Some(expected)
    });

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
