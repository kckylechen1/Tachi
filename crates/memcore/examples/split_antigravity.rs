use memcore::{MemoryEntry, MemoryStore};
use std::collections::{HashMap, HashSet};
use std::env;
use std::error::Error;
use std::fs;
use std::path::Path;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
enum Target {
    Global,
    Project(&'static str),
}

const PROJECT_NAMES: [&str; 5] = ["antigravity", "sigil", "hapi", "openclaw", "tachi"];

fn main() -> Result<(), Box<dyn Error>> {
    let home = env::var("HOME")?;
    let source_db = env_or_default(
        "ANTIGRAVITY_SOURCE_DB",
        format!("{home}/.gemini/antigravity/memory.db"),
    );
    let global_db = env_or_default("GLOBAL_DB", format!("{home}/.tachi/global/memory.db"));
    let apply = parse_bool_env("APPLY");
    let dry_run = !apply;

    let mut project_db_paths = HashMap::new();
    for project in PROJECT_NAMES {
        let env_key = format!("{}_PROJECT_DB", project.to_ascii_uppercase());
        let default_path = format!("{home}/.tachi/projects/{project}/memory.db");
        project_db_paths.insert(project, env_or_default(&env_key, default_path));
    }

    ensure_parent_dirs(&global_db)?;
    for path in project_db_paths.values() {
        ensure_parent_dirs(path)?;
    }

    let source = MemoryStore::open(&source_db)?;
    let total = source.stats(true)?.total as usize;
    let shallow_entries = source.get_all_with_options(total + 32, true)?;
    let mut entries = Vec::with_capacity(shallow_entries.len());
    for entry in shallow_entries {
        if let Some(full) = source.get_with_options(&entry.id, true)? {
            entries.push(full);
        }
    }

    let mut global_store = MemoryStore::open(&global_db)?;
    let mut project_stores: HashMap<&'static str, MemoryStore> = HashMap::new();
    for project in PROJECT_NAMES {
        let db_path = project_db_paths
            .get(project)
            .expect("project path must exist");
        project_stores.insert(project, MemoryStore::open(db_path)?);
    }

    let mut target_ids: HashMap<Target, HashSet<String>> = HashMap::new();
    let mut target_counts: HashMap<Target, usize> = HashMap::new();

    for entry in entries {
        let target = classify(&entry);
        let mut migrated = entry.clone();
        if matches!(target, Target::Project(_)) && migrated.scope == "general" {
            migrated.scope = "project".to_string();
        }

        if !dry_run {
            match target {
                Target::Global => global_store.upsert(&migrated)?,
                Target::Project(project) => project_stores
                    .get_mut(project)
                    .expect("project store must exist")
                    .upsert(&migrated)?,
            }
        }

        target_ids
            .entry(target)
            .or_default()
            .insert(migrated.id.clone());
        *target_counts.entry(target).or_default() += 1;
    }

    for (target, ids) in &target_ids {
        if dry_run || ids.is_empty() {
            continue;
        }

        let store = match target {
            Target::Global => &global_store,
            Target::Project(project) => project_stores
                .get(project)
                .expect("project store must exist"),
        };

        let mut seen = HashSet::new();
        for id in ids {
            for edge in source.get_edges(id, "both", None)? {
                if !ids.contains(&edge.source_id) || !ids.contains(&edge.target_id) {
                    continue;
                }
                let key = format!("{}|{}|{}", edge.source_id, edge.target_id, edge.relation);
                if !seen.insert(key) {
                    continue;
                }
                store.add_edge(&edge)?;
            }
        }
    }

    println!("Source DB: {source_db}");
    println!("Global DB: {global_db}");
    println!("Mode:      {}", if dry_run { "dry-run" } else { "apply" });
    println!();
    println!("Classified memories:");
    print_target_count(
        "global",
        target_counts.get(&Target::Global).copied().unwrap_or(0),
    );
    for project in PROJECT_NAMES {
        print_target_count(
            project,
            target_counts
                .get(&Target::Project(project))
                .copied()
                .unwrap_or(0),
        );
    }
    println!();

    if !dry_run {
        println!("Target DB stats:");
        print_db_stats("global", &global_store)?;
        for project in PROJECT_NAMES {
            let store = project_stores
                .get(project)
                .expect("project store must exist");
            print_db_stats(project, store)?;
        }
    }

    Ok(())
}

fn classify(entry: &MemoryEntry) -> Target {
    let path = entry.path.to_lowercase();
    let keywords = entry.keywords.join(" ").to_lowercase();
    let entities = entry.entities.join(" ").to_lowercase();
    let blob = format!(
        "{}\n{}\n{}\n{}\n{}\n{}",
        path,
        entry.summary.to_lowercase(),
        entry.text.to_lowercase(),
        entry.topic.to_lowercase(),
        keywords,
        entities,
    );

    if entry.scope == "global"
        || entry.scope == "user"
        || path.starts_with("/user")
        || path.starts_with("/kanban")
    {
        return Target::Global;
    }

    if starts_with_any(&path, &["/project/openclaw", "/openclaw"])
        || contains_any(
            &blob,
            &["openclaw", "memory-hybrid-bridge", "牙牙", "yaya", "acpx"],
        )
    {
        return Target::Project("openclaw");
    }

    if starts_with_any(&path, &["/project/antigravity", "/antigravity"])
        || contains_any(
            &blob,
            &[
                "antigravity",
                "mcp_config.json",
                "gemini cli",
                "codex cli mcp",
                "mcp_codex",
                "claude code mcp",
            ],
        )
    {
        return Target::Project("antigravity");
    }

    if starts_with_any(&path, &["/project/sigil", "/project/memory-mcp", "/sigil"])
        || contains_any(
            &blob,
            &[
                "sigil",
                "tachi-server",
                "memory mcp",
                "memory_events",
                "causalworker",
                "consolidator",
                "distiller",
                "project db",
            ],
        )
    {
        return Target::Project("sigil");
    }

    if starts_with_any(&path, &["/tachi", "/project/tachi"])
        || contains_any(
            &blob,
            &[
                "tachi daemon",
                "tachi hub",
                "ghost whispers",
                "记忆生命周期",
                "技能槽位",
                "virtual capability",
                "sandbox 动态拦截",
                "phase 1 静态扫描 hook",
            ],
        )
    {
        return Target::Project("tachi");
    }

    if starts_with_any(
        &path,
        &[
            "/hapi",
            "/hyperion",
            "/project/quant",
            "/project/quant-analyzer",
            "/project/quant_analyzer",
            "/project/quant_analyzer_2026",
            "/project/quant_analyzer_2026",
            "/project/quant_analyzer_2026",
            "/project/股票交易",
            "/project/交易策略",
            "/project/量化交易策略",
            "/project/投资策略",
            "/project/风控",
            "/project/v8",
            "/quant_analyzer",
            "/quant_analyzer_2026",
            "/quant_analyzer_2026",
        ],
    ) || contains_any(
        &blob,
        &[
            "hapi",
            "hyperion",
            "ls1",
            "v8",
            "autoresearch",
            "quant_core",
            "watchlist",
            "portfolio",
            "trade_history",
            "持仓",
            "交易",
            "风控",
            "股票",
            "仓位",
            "量化",
            "买入",
            "卖出",
            "回测",
        ],
    ) {
        return Target::Project("hapi");
    }

    Target::Global
}

fn env_or_default(key: &str, default: String) -> String {
    env::var(key).unwrap_or(default)
}

fn parse_bool_env(key: &str) -> bool {
    env::var(key)
        .map(|value| matches!(value.as_str(), "1" | "true" | "TRUE" | "yes" | "YES"))
        .unwrap_or(false)
}

fn ensure_parent_dirs(path: &str) -> Result<(), Box<dyn Error>> {
    let p = Path::new(path);
    if let Some(parent) = p.parent() {
        fs::create_dir_all(parent)?;
    }
    Ok(())
}

fn starts_with_any(haystack: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| haystack.starts_with(needle))
}

fn contains_any(haystack: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| haystack.contains(needle))
}

fn print_target_count(name: &str, count: usize) {
    println!("  {:<12} {}", name, count);
}

fn print_db_stats(name: &str, store: &MemoryStore) -> Result<(), Box<dyn Error>> {
    let (total, with_vec) = store.vector_stats()?;
    println!("  {:<12} total={} vectors={}", name, total, with_vec);
    Ok(())
}
