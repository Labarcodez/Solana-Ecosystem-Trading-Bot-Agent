use thiserror::Error;

#[derive(Debug, Error)]
pub enum StorageError {
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),

    #[error("storage actor has shut down")]
    ActorGone,

    #[error("invalid stored value for {field}: {value}")]
    InvalidValue { field: &'static str, value: String },
}
