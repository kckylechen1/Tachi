use super::rows::collect_quarantined;
use super::*;

pub fn cmd_list(manifest: &Manifest, json_out: bool) -> Result<(), Box<dyn std::error::Error>> {
    let rows = collect_quarantined(manifest)?;
    if json_out {
        let body = json!({
            "total": rows.len(),
            "rows": rows.iter().map(|q| json!({
                "id": q.id,
                "db": q.db_label,
                "path": q.path,
                "original_path": q.original_path,
                "expected_db": q.expected_db,
                "actual_db": q.actual_db,
                "detected_at": q.detected_at,
            })).collect::<Vec<_>>()
        });
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }
    if rows.is_empty() {
        println!(
            "[OK] no quarantined rows found across {} manifest DB(s).",
            manifest.dbs.len()
        );
        return Ok(());
    }
    println!("Quarantined rows: {}", rows.len());
    for q in &rows {
        println!(
            "  {id}  in_db={db} expected={exp}  {path} <- {orig}",
            id = q.id,
            db = q.db_label,
            exp = q.expected_db,
            path = q.path,
            orig = q.original_path,
        );
    }
    Ok(())
}
