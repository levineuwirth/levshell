//! Error type for the data crate.

use thiserror::Error;

pub type Result<T> = std::result::Result<T, DataError>;

#[derive(Debug, Error)]
pub enum DataError {
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),

    #[error("migration error: {0}")]
    Migration(#[from] rusqlite_migration::Error),

    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("blocking task join error: {0}")]
    Join(#[from] tokio::task::JoinError),

    #[error("io error opening database: {0}")]
    Io(#[from] std::io::Error),

    #[error("entity not found")]
    NotFound,

    #[error("invalid {field} value: {value}")]
    InvalidEnum { field: &'static str, value: String },
}
