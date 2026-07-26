use std::path::{Path, PathBuf};

use memcore::{get_foundry_config, set_foundry_config, MemoryStore, PerDbConfig};
use serde_json::json;

use super::capture_archive;
use crate::manifest::Manifest;
use tachi_bootstrap::cli::FoundryAction;

pub(crate) async fn run_foundry(
    action: FoundryAction,
    app_home: &Path,
    global_db_path: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    match action {
        FoundryAction::CaptureArchivePlan { db, as_of, output } => {
            let target = db.unwrap_or_else(|| global_db_path.to_path_buf());
            let as_of = match as_of {
                Some(value) => value
                    .parse()
                    .map_err(|e| format!("invalid --as-of RFC3339: {e}"))?,
                None => chrono::Utc::now(),
            };
            let plan = capture_archive::build_plan(&target, as_of)?;
            let json = serde_json::to_string_pretty(&plan)?;
            println!("{json}");
            if let Some(path) = output {
                capture_archive::write_owner_atomic(&path, json.as_bytes())?;
            }
            Ok(())
        }
        FoundryAction::CaptureArchiveApply { db, plan, confirm } => {
            let target = db.unwrap_or_else(|| global_db_path.to_path_buf());
            let receipt =
                capture_archive::apply(&target, capture_archive::read_plan(&plan)?, confirm)?;
            println!("{}", serde_json::to_string_pretty(&receipt)?);
            Ok(())
        }
        FoundryAction::CaptureArchiveRestore {
            db,
            receipt,
            confirm,
        } => {
            let target = db.unwrap_or_else(|| global_db_path.to_path_buf());
            let receipt = capture_archive::restore(
                &target,
                capture_archive::read_receipt(&receipt)?,
                confirm,
            )?;
            println!("{}", serde_json::to_string_pretty(&receipt)?);
            Ok(())
        }
        FoundryAction::ConfigGet { db } => {
            let target = db.unwrap_or_else(|| global_db_path.to_path_buf());
            let cfg = read_per_db_config(&target)?;
            println!(
                "{}",
                serde_json::to_string_pretty(&json!({
                    "db": target.display().to_string(),
                    "config": cfg,
                }))?
            );
            Ok(())
        }
        FoundryAction::ConfigSet {
            db,
            enabled,
            max_jobs_per_minute,
            distill_concurrency,
            enrichment_concurrency,
            llm_provider_override,
        } => {
            let target = db.unwrap_or_else(|| global_db_path.to_path_buf());
            let path_str = target
                .to_str()
                .ok_or_else(|| format!("non-utf8 path: {}", target.display()))?;
            let store = MemoryStore::open_with_label(path_str, "tachi-foundry-config")
                .map_err(|e| format!("open {}: {e}", target.display()))?;
            let mut cfg = get_foundry_config(store.connection())
                .map_err(|e| format!("get_foundry_config: {e}"))?;
            if let Some(v) = enabled {
                cfg.enabled = v;
            }
            if let Some(v) = max_jobs_per_minute {
                cfg.max_jobs_per_minute = v;
            }
            if let Some(v) = distill_concurrency {
                cfg.distill_concurrency = v;
            }
            if let Some(v) = enrichment_concurrency {
                cfg.enrichment_concurrency = v;
            }
            if let Some(v) = llm_provider_override {
                cfg.llm_provider_override = if v.is_empty() { None } else { Some(v) };
            }
            set_foundry_config(store.connection(), &cfg, "tachi-cli")
                .map_err(|e| format!("set_foundry_config: {e}"))?;
            println!(
                "{}",
                serde_json::to_string_pretty(&json!({
                    "db": target.display().to_string(),
                    "config": cfg,
                    "updated": true,
                }))?
            );
            Ok(())
        }
        FoundryAction::ConfigList { json: json_out } => {
            let manifest_path = app_home.join("manifest.json");
            let manifest = Manifest::load(&manifest_path).unwrap_or_else(|_| Manifest::empty());
            let mut entries: Vec<serde_json::Value> = Vec::new();
            for entry in &manifest.dbs {
                if is_checkpoint_fixture_path(&entry.path) {
                    continue;
                }
                let p = PathBuf::from(&entry.path);
                let leaf_exists = match crate::path_utils::manifest_db_leaf_exists(entry) {
                    Ok(exists) => exists,
                    Err(error) => {
                        entries.push(json!({
                            "db": entry.path,
                            "label": entry.scope_hint,
                            "config": null,
                            "error": error,
                        }));
                        continue;
                    }
                };
                if !leaf_exists {
                    entries.push(json!({
                        "db": entry.path,
                        "label": entry.scope_hint,
                        "config": null,
                        "error": "missing on disk",
                    }));
                    continue;
                }
                match read_per_db_config(&p) {
                    Ok(cfg) => entries.push(json!({
                        "db": entry.path,
                        "label": entry.scope_hint,
                        "config": cfg,
                    })),
                    Err(e) => entries.push(json!({
                        "db": entry.path,
                        "label": entry.scope_hint,
                        "config": null,
                        "error": e.to_string(),
                    })),
                }
            }
            if json_out {
                println!("{}", serde_json::to_string_pretty(&entries)?);
            } else {
                for e in &entries {
                    let db = e.get("db").and_then(|v| v.as_str()).unwrap_or("?");
                    let label = e.get("label").and_then(|v| v.as_str()).unwrap_or("?");
                    if let Some(err) = e.get("error").and_then(|v| v.as_str()) {
                        eprintln!("[X] {label:<20} {db}  ({err})");
                    } else {
                        let cfg = e.get("config").cloned().unwrap_or(serde_json::Value::Null);
                        let enabled = cfg.get("enabled").and_then(|v| v.as_bool()).unwrap_or(true);
                        let max_jpm = cfg
                            .get("max_jobs_per_minute")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(0);
                        let dc = cfg
                            .get("distill_concurrency")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(0);
                        let ec = cfg
                            .get("enrichment_concurrency")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(0);
                        println!(
                            "[OK] {label:<20} enabled={enabled} max_jpm={max_jpm:<3} distill={dc} enrich={ec}  {db}"
                        );
                    }
                }
            }
            Ok(())
        }
    }
}

pub(crate) fn is_checkpoint_fixture_path(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    let file = lower
        .rsplit(std::path::MAIN_SEPARATOR)
        .next()
        .unwrap_or(&lower);
    file.contains(".checkpointed.") && file.ends_with(".db")
}

fn read_per_db_config(path: &Path) -> Result<PerDbConfig, Box<dyn std::error::Error>> {
    let path_str = path
        .to_str()
        .ok_or_else(|| format!("non-utf8 path: {}", path.display()))?;
    let store = MemoryStore::open_with_label(path_str, "tachi-foundry-config")
        .map_err(|e| format!("open {}: {e}", path.display()))?;
    let cfg =
        get_foundry_config(store.connection()).map_err(|e| format!("get_foundry_config: {e}"))?;
    Ok(cfg)
}
