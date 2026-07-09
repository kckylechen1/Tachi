use thiserror::Error;

#[derive(Debug, Error)]
pub enum MemoryError {
    #[error("SQLite error: {0}")]
    Sqlite(#[from] rusqlite::Error),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Invalid argument: {0}")]
    InvalidArg(String),

    #[error("Not found: {0}")]
    NotFound(String),

    #[error("Vault error: {0}")]
    Vault(String),

    #[error("Vault locked")]
    VaultLocked,

    #[error("Vault not initialized")]
    VaultNotInitialized,

    #[error("Duplicate entry: {0}")]
    Duplicate(String),

    #[error("Internal error: {0}")]
    Internal(String),
}
