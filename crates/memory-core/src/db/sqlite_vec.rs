use rusqlite::{Connection, Result as SqlResult};
use std::sync::Once;

static SQLITE_VEC_AUTO_EXT_ONCE: Once = Once::new();

pub fn register_sqlite_vec() {
    SQLITE_VEC_AUTO_EXT_ONCE.call_once(|| unsafe {
        let init_fn = std::mem::transmute::<
            *const (),
            unsafe extern "C" fn(
                *mut rusqlite::ffi::sqlite3,
                *mut *mut i8,
                *const rusqlite::ffi::sqlite3_api_routines,
            ) -> i32,
        >(sqlite_vec::sqlite3_vec_init as *const ());
        let rc = rusqlite::ffi::sqlite3_auto_extension(Some(init_fn));
        if rc != rusqlite::ffi::SQLITE_OK {
            eprintln!("warning: sqlite-vec auto_extension registration failed (rc={rc})");
        }
    });
}

/// Attempt to create the sqlite-vec virtual table so KNN search is available.
/// Assumes `register_sqlite_vec()` was called before the connection was opened.
/// Returns true if the vec0 table was created successfully, false otherwise.
pub fn try_load_sqlite_vec(conn: &Connection) -> bool {
    let r: SqlResult<()> = conn.execute_batch(
        r#"
        CREATE VIRTUAL TABLE IF NOT EXISTS memories_vec USING vec0(
            id      TEXT PRIMARY KEY,
            embedding float[1024]
        );
    "#,
    );
    r.is_ok()
}

/// Serialise a float32 vector into the binary blob format sqlite-vec expects.
pub fn serialize_f32(vec: &[f32]) -> Vec<u8> {
    let mut buf = Vec::with_capacity(vec.len() * 4);
    for &v in vec {
        buf.extend_from_slice(&v.to_le_bytes());
    }
    buf
}
