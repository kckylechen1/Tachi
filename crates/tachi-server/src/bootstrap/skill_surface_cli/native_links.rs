//! Explicit, owner-operated native Skill directory links. No discovery, copying,
//! execution, database writes, or host configuration edits occur here.

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

mod filesystem;
#[cfg(test)]
mod tests;

use filesystem::{identity, move_exclusive, present, LinkIdentity, Store};

const STORE_NAME: &str = ".tachi-skill-links";
const RECEIPT_NAME: &str = "receipt.json";
const STAGED_NAME: &str = "entry";

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Receipt {
    version: u32,
    name: String,
    source: PathBuf,
    // Windows read_link can normalize the spelling of a canonical path. Record
    // its actual representation while the source exists; unlink must also work
    // after the source moves, without canonicalizing a dangling link.
    link_target: PathBuf,
    target_dir: PathBuf,
    identity: LinkIdentity,
}

#[derive(Debug, Serialize)]
pub(super) struct Report {
    schema_version: &'static str,
    action: &'static str,
    target_dir: PathBuf,
    links: Vec<LinkReport>,
}

#[derive(Debug, Serialize)]
struct LinkReport {
    name: String,
    source: Option<PathBuf>,
    target: PathBuf,
    state: &'static str,
}

impl Report {
    fn one(
        action: &'static str,
        root: &Path,
        name: &str,
        source: Option<&Path>,
        state: &'static str,
    ) -> Self {
        Self {
            schema_version: "tachi.skill_surface.native_links.v1",
            action,
            target_dir: root.to_path_buf(),
            links: vec![LinkReport {
                name: name.into(),
                source: source.map(Path::to_path_buf),
                target: root.join(name),
                state,
            }],
        }
    }
}

pub(super) fn print_report(report: &Report, json: bool) -> Result<(), serde_json::Error> {
    if json {
        println!("{}", serde_json::to_string_pretty(report)?);
    } else {
        println!("Native skills: {}", report.target_dir.display());
        for entry in &report.links {
            println!(
                "{}: {} ({})",
                entry.name,
                entry.state,
                entry.target.display()
            );
            if let Some(source) = &entry.source {
                println!("  source: {}", source.display());
            }
        }
        if report.links.is_empty() {
            println!("No Tachi-managed links.");
        }
    }
    Ok(())
}

fn validate_name(name: &str) -> Result<(), String> {
    if name.is_empty()
        || name.len() > 128
        || !name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err(
            "Skill name must contain 1–128 ASCII letters, digits, hyphens, or underscores".into(),
        );
    }
    // These names are device aliases on Windows even when followed by a path.
    let upper = name.to_ascii_uppercase();
    if matches!(upper.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (upper.len() == 4
            && (upper.starts_with("COM") || upper.starts_with("LPT"))
            && matches!(upper.as_bytes()[3], b'1'..=b'9'))
    {
        return Err(format!("reserved Skill directory name: {name}"));
    }
    Ok(())
}

fn source_directory(source: &Path, root: &Path) -> Result<(String, PathBuf), String> {
    let resolved = fs::canonicalize(source)
        .map_err(|e| format!("resolve source {}: {e}", source.display()))?;
    if resolved.to_str().is_none() {
        return Err(
            "source path must be UTF-8 so it can be recorded in the ownership receipt".into(),
        );
    }
    let name = source
        .file_name()
        .or_else(|| resolved.file_name())
        .and_then(|s| s.to_str())
        .ok_or("source must end in a UTF-8 Skill directory name")?
        .to_owned();
    validate_name(&name)?;
    let source = resolved;
    if !source.is_dir() || !source.join("SKILL.md").is_file() {
        return Err(format!(
            "source {} must be a directory containing SKILL.md",
            source.display()
        ));
    }
    if root.starts_with(&source) || source.starts_with(root.join(STORE_NAME)) {
        return Err(
            "source must not contain the target directory or live in Tachi link metadata".into(),
        );
    }
    Ok((name, source))
}

fn read_receipt(store: &Store, name: &str) -> Result<Option<Receipt>, String> {
    validate_name(name)?;
    let slot = store.path.join(name);
    if !present(&slot)? {
        return Ok(None);
    }
    filesystem::check_directory(&slot)?;
    let path = slot.join(RECEIPT_NAME);
    if !present(&path)? {
        return Err(format!(
            "incomplete link metadata at {}; preserve it for manual inspection",
            slot.display()
        ));
    }
    let receipt: Receipt = serde_json::from_slice(&filesystem::read_receipt(&path)?)
        .map_err(|e| format!("invalid ownership receipt {}: {e}", path.display()))?;
    if receipt.version != 1
        || receipt.name != name
        || receipt.target_dir != store.root
        || !receipt.source.is_absolute()
        || !receipt.link_target.is_absolute()
        || receipt.source.starts_with(&store.path)
        || store.root.starts_with(&receipt.source)
    {
        return Err(format!(
            "ownership receipt does not match this Skill and target directory: {}",
            path.display()
        ));
    }
    Ok(Some(receipt))
}

fn matching_link(path: &Path, receipt: &Receipt) -> Result<bool, String> {
    let metadata =
        fs::symlink_metadata(path).map_err(|e| format!("inspect {}: {e}", path.display()))?;
    if !metadata.file_type().is_symlink() {
        return Ok(false);
    }
    let target = fs::read_link(path).map_err(|e| format!("read link {}: {e}", path.display()))?;
    Ok(target == receipt.link_target && identity(path)? == receipt.identity)
}

fn require_matching(path: &Path, receipt: &Receipt) -> Result<(), String> {
    if matching_link(path, receipt)? {
        Ok(())
    } else {
        Err(format!(
            "refusing to change {}: it no longer matches the Tachi-owned link",
            path.display()
        ))
    }
}

/// Keep the receipt until the owned entry has disappeared. An empty slot after a
/// crash is harmless; an unknown entry is never removed recursively.
fn clear_receipt(store: &Store, name: &str) -> Result<(), String> {
    let slot = store.path.join(name);
    for entry in fs::read_dir(&slot).map_err(|e| format!("read {}: {e}", slot.display()))? {
        let entry = entry.map_err(|e| e.to_string())?;
        if entry.file_name() != RECEIPT_NAME {
            return Err(format!(
                "unexpected metadata at {}; preserve it for manual inspection",
                entry.path().display()
            ));
        }
    }
    fs::remove_file(slot.join(RECEIPT_NAME))
        .map_err(|e| format!("remove ownership receipt: {e}"))?;
    fs::remove_dir(&slot).map_err(|e| format!("remove empty metadata directory: {e}"))?;
    crate::utils::sync_parent_dir(&slot)
}

pub(super) fn link(source: &Path, target_dir: &Path) -> Result<Report, String> {
    let root = filesystem::target_directory(target_dir)?;
    let (name, source) = source_directory(source, &root)?;
    let store = Store::open(root, true)?.expect("writable store exists");
    let target = store.root.join(&name);
    let slot = store.path.join(&name);
    let staged = slot.join(STAGED_NAME);
    let receipt = if let Some(receipt) = read_receipt(&store, &name)? {
        if receipt.source != source {
            return Err(format!(
                "{name} is managed from {}; unlink it before selecting another source",
                receipt.source.display()
            ));
        }
        if present(&target)? {
            require_matching(&target, &receipt)?;
            if present(&staged)? {
                return Err(format!("both target and staged link exist for {name}; refusing to guess which to change"));
            }
            return Ok(Report::one(
                "link",
                &store.root,
                &name,
                Some(&source),
                "unchanged",
            ));
        }
        if !present(&staged)? {
            return Err(format!("managed link {name} is missing; run unlink to clear its receipt before linking again"));
        }
        require_matching(&staged, &receipt)?;
        receipt
    } else {
        if present(&target)? {
            return Err(format!(
                "refusing to replace unmanaged entry {}",
                target.display()
            ));
        }
        filesystem::create_private_directory(&slot)?;
        crate::utils::sync_parent_dir(&slot)?;
        // Write and sync ownership before publishing the final native entry.
        if let Err(error) = filesystem::create_directory_link(&source, &staged) {
            let _ = fs::remove_dir(&slot);
            return Err(error);
        }
        let receipt = Receipt {
            version: 1,
            name: name.clone(),
            source: source.clone(),
            link_target: fs::read_link(&staged)
                .map_err(|e| format!("read staged link {}: {e}", staged.display()))?,
            target_dir: store.root.clone(),
            identity: identity(&staged)?,
        };
        crate::utils::write_owner_only_file_atomic(
            &slot.join(RECEIPT_NAME),
            &serde_json::to_vec_pretty(&receipt).map_err(|e| e.to_string())?,
        )?;
        receipt
    };
    move_exclusive(&staged, &target)?;
    require_matching(&target, &receipt)?;
    crate::utils::sync_parent_dir(&target)?;
    crate::utils::sync_parent_dir(&staged)?;
    Ok(Report::one(
        "link",
        &store.root,
        &name,
        Some(&source),
        "linked",
    ))
}

pub(super) fn unlink(name: &str, target_dir: &Path) -> Result<Report, String> {
    unlink_inner(name, target_dir, || {})
}

// The callback is a no-op in production; tests exercise the replacement window
// between checking a native link and moving it to the private staging directory.
fn unlink_inner(
    name: &str,
    target_dir: &Path,
    before_move: impl FnOnce(),
) -> Result<Report, String> {
    validate_name(name)?;
    let root = filesystem::target_directory(target_dir)?;
    let Some(store) = Store::open(root.clone(), false)? else {
        return absent_unlink(name, &root);
    };
    let Some(receipt) = read_receipt(&store, name)? else {
        return absent_unlink(name, &root);
    };
    let target = store.root.join(name);
    let staged = store.path.join(name).join(STAGED_NAME);
    if present(&target)? {
        require_matching(&target, &receipt)?;
        if present(&staged)? {
            return Err(format!(
                "both target and staged link exist for {name}; refusing to change either"
            ));
        }
        before_move();
        move_exclusive(&target, &staged)?;
        if let Err(error) = require_matching(&staged, &receipt) {
            // A third party replaced the native entry after preflight. Preserve
            // its object, restoring it only if the native name is still empty.
            return match move_exclusive(&staged, &target) {
                Ok(()) => Err(format!("{error}; replacement restored without deleting it")),
                Err(restore) => Err(format!(
                    "{error}; replacement preserved at {} (restore failed: {restore})",
                    staged.display()
                )),
            };
        }
        crate::utils::sync_parent_dir(&target)?;
        crate::utils::sync_parent_dir(&staged)?;
    }
    if present(&staged)? {
        require_matching(&staged, &receipt)?;
        filesystem::remove_directory_link(&staged)?;
        crate::utils::sync_parent_dir(&staged)?;
    }
    clear_receipt(&store, name)?;
    Ok(Report::one(
        "unlink",
        &root,
        name,
        Some(&receipt.source),
        "removed",
    ))
}

fn absent_unlink(name: &str, root: &Path) -> Result<Report, String> {
    if present(&root.join(name))? {
        return Err(format!(
            "refusing to remove unmanaged entry {}",
            root.join(name).display()
        ));
    }
    Ok(Report::one("unlink", root, name, None, "already_removed"))
}

pub(super) fn list(target_dir: &Path) -> Result<Report, String> {
    let root = filesystem::target_directory(target_dir)?;
    let mut report = Report {
        schema_version: "tachi.skill_surface.native_links.v1",
        action: "links",
        target_dir: root.clone(),
        links: Vec::new(),
    };
    let Some(store) = Store::open_read_only(root)? else {
        return Ok(report);
    };
    for entry in fs::read_dir(&store.path).map_err(|e| format!("read link metadata: {e}"))? {
        let entry = entry.map_err(|e| e.to_string())?;
        if entry.file_name() == ".lock" {
            continue;
        }
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| "non-UTF-8 link metadata name")?;
        let receipt =
            read_receipt(&store, &name)?.ok_or("link metadata changed during inventory")?;
        let target = store.root.join(&name);
        let staged = store.path.join(&name).join(STAGED_NAME);
        let state = if present(&target)? {
            if present(&staged)? || !matching_link(&target, &receipt)? {
                "changed"
            } else if !receipt.source.is_dir() {
                "broken"
            } else if !receipt.source.join("SKILL.md").is_file() {
                "invalid_source"
            } else {
                "linked"
            }
        } else if present(&staged)? {
            if matching_link(&staged, &receipt)? {
                "staged"
            } else {
                "changed"
            }
        } else {
            "missing"
        };
        report.links.push(LinkReport {
            name,
            source: Some(receipt.source),
            target,
            state,
        });
    }
    report.links.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(report)
}
