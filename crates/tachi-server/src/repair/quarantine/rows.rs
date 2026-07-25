use super::*;

#[derive(Debug)]
pub(in crate::repair::quarantine) struct QRow {
    pub(in crate::repair::quarantine) id: String,
    pub(in crate::repair::quarantine) path: String,
    pub(in crate::repair::quarantine) original_path: String,
    pub(in crate::repair::quarantine) expected_db: String,
    pub(in crate::repair::quarantine) actual_db: String,
    pub(in crate::repair::quarantine) detected_at: String,
    pub(in crate::repair::quarantine) db_label: String,
    pub(in crate::repair::quarantine) db_path: PathBuf,
}

pub(in crate::repair::quarantine) fn collect_quarantined(
    manifest: &Manifest,
) -> Result<Vec<QRow>, Box<dyn std::error::Error>> {
    let mut out = Vec::new();
    for entry in &manifest.dbs {
        crate::path_utils::manifest_db_leaf_exists(entry)?;
    }
    for entry in select_dbs(manifest, None) {
        let conn = match Connection::open(&entry.path) {
            Ok(c) => c,
            Err(_) => continue,
        };
        let mut stmt = match conn.prepare(
            "SELECT id, path,
                    COALESCE(json_extract(metadata, '$.quarantine.original_path'), ''),
                    COALESCE(json_extract(metadata, '$.quarantine.expected_db'), ''),
                    COALESCE(json_extract(metadata, '$.quarantine.actual_db'), ''),
                    COALESCE(json_extract(metadata, '$.quarantine.detected_at'), '')
             FROM memories
             WHERE path LIKE '/_quarantine/%'",
        ) {
            Ok(s) => s,
            Err(_) => continue,
        };
        let label = label_for(&entry);
        let dbp = PathBuf::from(&entry.path);
        let rows = stmt
            .query_map([], |row| {
                Ok(QRow {
                    id: row.get(0)?,
                    path: row.get(1)?,
                    original_path: row.get(2)?,
                    expected_db: row.get(3)?,
                    actual_db: row.get(4)?,
                    detected_at: row.get(5)?,
                    db_label: label.clone(),
                    db_path: dbp.clone(),
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        out.extend(rows);
    }
    Ok(out)
}
