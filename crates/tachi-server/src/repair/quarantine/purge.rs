use super::rows::{collect_quarantined, QRow};
use super::*;

pub fn cmd_purge(
    manifest: &Manifest,
    older_than_days: u64,
    apply: bool,
    json_out: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let cutoff: DateTime<Utc> = Utc::now() - Duration::days(older_than_days as i64);
    let cutoff_iso = cutoff.to_rfc3339();
    let rows = collect_quarantined(manifest)?;

    let stale: Vec<&QRow> = rows
        .iter()
        .filter(|q| !q.detected_at.is_empty() && q.detected_at < cutoff_iso)
        .collect();

    if stale.is_empty() {
        let msg = format!(
            "[OK] no quarantined rows older than {} day(s) (cutoff={})",
            older_than_days, cutoff_iso
        );
        if json_out {
            println!(
                "{}",
                json!({"purged": 0, "cutoff": cutoff_iso, "apply": apply})
            );
        } else {
            println!("{msg}");
        }
        return Ok(());
    }

    if !apply {
        if json_out {
            let body = json!({
                "cutoff": cutoff_iso,
                "candidates": stale.len(),
                "apply": false,
            });
            println!("{}", serde_json::to_string_pretty(&body)?);
        } else {
            println!(
                "[!] DRY-RUN: would purge {} quarantined row(s) older than {} day(s)",
                stale.len(),
                older_than_days
            );
        }
        return Ok(());
    }

    // Group by src DB and delete.
    use std::collections::HashMap;
    let mut by_src: HashMap<PathBuf, Vec<&QRow>> = HashMap::new();
    for q in &stale {
        by_src.entry(q.db_path.clone()).or_default().push(*q);
    }
    let mut purged = 0usize;
    for (db_path, group) in by_src {
        let mut conn = Connection::open(&db_path)?;
        let tx = conn.transaction()?;
        for q in group {
            tx.execute("DELETE FROM memories WHERE id = ?1", params![&q.id])?;
            if let Err(e) = tx.execute("DELETE FROM memories_fts WHERE id = ?1", params![&q.id]) {
                eprintln!(
                    "warning: failed to delete FTS row for purged quarantine row {}: {e}",
                    q.id
                );
            }
            // Propagate symbolic delete failure so the memories DELETE cannot
            // commit without removing the trigram projection (#1335 oracle NOT-READY).
            memcore::db::delete_memories_symbolic_fts(&tx, &q.id)?;
            purged += 1;
        }
        tx.commit()?;
    }
    if json_out {
        println!(
            "{}",
            json!({"purged": purged, "cutoff": cutoff_iso, "apply": true})
        );
    } else {
        println!("[OK] purged {} quarantined row(s).", purged);
    }
    Ok(())
}
