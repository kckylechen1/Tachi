use rusqlite::{functions::FunctionFlags, Connection};

/// Match CRUD's `Vec<String>::join(" ")` when projecting stored JSON arrays.
/// This query-only function adds no persistent schema dependency or authority.
pub(super) fn register(conn: &Connection) -> rusqlite::Result<()> {
    conn.create_scalar_function(
        "memcore_fts_terms",
        1,
        FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC,
        |context| {
            let raw = context.get::<Option<String>>(0)?;
            Ok(raw.map(|raw| {
                serde_json::from_str::<Vec<String>>(&raw)
                    .map(|terms| terms.join(" "))
                    // Legacy malformed/non-string arrays remain readable.
                    // Preserve the old SQL replace/trim fallback, including
                    // SQL trim's ASCII-space-only behavior and NULL above.
                    .unwrap_or_else(|_| {
                        raw.replace(['[', ']', '"'], " ")
                            .trim_matches(' ')
                            .to_owned()
                    })
            }))
        },
    )
}
