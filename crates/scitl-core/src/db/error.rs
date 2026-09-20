#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    #[error("database error: {0}")]
    Db(#[from] rusqlite::Error),
    #[error("migration error: {0}")]
    Migration(#[from] rusqlite_migration::Error),
    #[error("task {0} not found")]
    TaskNotFound(i64),
    #[error("unknown argument: {0}")]
    UnknownArgument(String),
    #[error("invalid argument {name}: {reason}")]
    InvalidArgument { name: String, reason: String },
    #[error("llm provider error: {0}")]
    Llm(String),
}

pub type Result<T> = std::result::Result<T, CoreError>;
