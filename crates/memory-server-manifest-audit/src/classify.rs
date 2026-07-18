use super::types::{PlannedAction, ProjectDbClass, ProjectDbInput, RelocationItem};

/// Returns `true` if a project directory name looks like a throwaway
/// UUID-shaped or smoke-test workspace rather than a real named project.
///
/// Matches:
///   * canonical 8-4-4-4-12 hex UUID (with or without surrounding noise)
///   * a 32-char (or longer) run of hex with no separators
///   * names containing common smoke/scratch markers (`smoke`, `tmp-`, `scratch-`)
///     combined with a long hex tail
pub fn is_uuid_smoke_test_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();

    // Canonical UUID anywhere in the name.
    if contains_canonical_uuid(&lower) {
        return true;
    }

    // Explicit smoke/scratch markers paired with a hex tail.
    let has_marker = lower.contains("smoke")
        || lower.contains("scratch")
        || lower.starts_with("tmp-")
        || lower.starts_with("tmp_")
        || lower.contains("-recall-smoke");
    if has_marker && longest_hex_run(&lower) >= 8 {
        return true;
    }

    // A bare long hex blob (>=32 contiguous hex chars) — e.g. a md5/uuid-no-dash.
    if longest_hex_run(&lower) >= 32 {
        return true;
    }

    false
}

fn contains_canonical_uuid(s: &str) -> bool {
    // Look for the 8-4-4-4-12 hex pattern. Hand-rolled to avoid a regex dep.
    let bytes = s.as_bytes();
    let n = bytes.len();
    let is_hex = |b: u8| b.is_ascii_digit() || (b'a'..=b'f').contains(&b);
    // Need 36 chars for the dashed form.
    if n < 36 {
        return false;
    }
    let groups = [8usize, 4, 4, 4, 12];
    for start in 0..=(n - 36) {
        let mut idx = start;
        let mut ok = true;
        for (gi, &glen) in groups.iter().enumerate() {
            for _ in 0..glen {
                if idx >= n || !is_hex(bytes[idx]) {
                    ok = false;
                    break;
                }
                idx += 1;
            }
            if !ok {
                break;
            }
            if gi < groups.len() - 1 {
                if idx >= n || bytes[idx] != b'-' {
                    ok = false;
                    break;
                }
                idx += 1;
            }
        }
        if ok {
            return true;
        }
    }
    false
}

fn longest_hex_run(s: &str) -> usize {
    let mut best = 0usize;
    let mut cur = 0usize;
    for b in s.bytes() {
        let is_hex = b.is_ascii_digit() || (b'a'..=b'f').contains(&b);
        if is_hex {
            cur += 1;
            best = best.max(cur);
        } else {
            cur = 0;
        }
    }
    best
}

/// PURE classification core. No I/O. Given pre-gathered facts, decide the class
/// and the planned action.
///
/// Precedence:
///   1. UUID/smoke-test name → garbage (even if it is a real file or symlink;
///      a throwaway workspace's data is not worth relocating).
///   2. Symlink → alias (target exists) or broken (target missing).
///   3. Real file + owning repo found → relocatable.
///   4. Real file, no owning repo → home-resident (keep, never move).
pub fn classify_project_db(input: &ProjectDbInput) -> RelocationItem {
    let db_path = input.db_path.to_string_lossy().to_string();

    // (1) UUID / smoke-test garbage takes precedence.
    if is_uuid_smoke_test_name(&input.project_name) {
        return RelocationItem {
            project_name: input.project_name.clone(),
            db_path,
            class: ProjectDbClass::UuidSmokeTestGarbage,
            action: PlannedAction::GarbageCollect,
            relocate_to: None,
            symlink_target: input
                .symlink_target
                .as_ref()
                .map(|p| p.to_string_lossy().to_string()),
            note: "project name looks like a UUID/smoke-test workspace — GC candidate".to_string(),
        };
    }

    // (2) Symlinks.
    if input.is_symlink {
        let target = input
            .symlink_target
            .as_ref()
            .map(|p| p.to_string_lossy().to_string());
        if input.symlink_target_exists {
            return RelocationItem {
                project_name: input.project_name.clone(),
                db_path,
                class: ProjectDbClass::SymlinkAlias,
                action: PlannedAction::KeepAlias,
                relocate_to: None,
                symlink_target: target,
                note: "healthy symlink alias into repo-local DB — keep".to_string(),
            };
        }
        return RelocationItem {
            project_name: input.project_name.clone(),
            db_path,
            class: ProjectDbClass::SymlinkBroken,
            action: PlannedAction::GarbageCollect,
            relocate_to: None,
            symlink_target: target,
            note: "dangling symlink (target missing) — GC the link only".to_string(),
        };
    }

    // (3) Real file with a discoverable owning repo → relocatable.
    if let Some(repo) = &input.owning_repo {
        // #1132: new/relocated DBs are created under the canonical filename, so
        // the relocation destination is `<repo>/.tachi/tachi-memory.db`, never
        // the legacy `memory.db` name. Mirrors
        // `memcore::db::filename::MEMORY_DB_FILENAME`.
        let dest = repo.join(".tachi").join("tachi-memory.db");
        return RelocationItem {
            project_name: input.project_name.clone(),
            db_path,
            class: ProjectDbClass::RealFileWithOwningRepo,
            action: PlannedAction::RelocateToRepo,
            relocate_to: Some(dest.to_string_lossy().to_string()),
            symlink_target: None,
            note: format!(
                "real per-project DB; owning repo {} — could relocate into <repo>/.tachi/",
                repo.display()
            ),
        };
    }

    // (4) Real file, no owning repo → keep as home-resident named project.
    RelocationItem {
        project_name: input.project_name.clone(),
        db_path,
        class: ProjectDbClass::RealFileHomeResident,
        action: PlannedAction::KeepHomeResident,
        relocate_to: None,
        symlink_target: None,
        note: "real DB with no owning repo — keep as home-resident named project".to_string(),
    }
}
