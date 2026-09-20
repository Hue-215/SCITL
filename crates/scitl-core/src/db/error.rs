#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    #[error("database error: {0}")]
    Db(#[from] rusqlite::Error),
    #[error("migration error: {0}")]
    Migration(#[from] rusqlite_migration::Error),
    #[error("task {0} not found")]
    TaskNotFound(i64),
    #[error("task step {0} not found")]
    TaskStepNotFound(i64),
    #[error("unknown argument: {0}")]
    UnknownArgument(String),
    #[error("invalid argument {name}: {reason}")]
    InvalidArgument { name: String, reason: String },
    #[error("llm provider error: {0}")]
    Llm(String),
    #[error("invalid provider configuration: {0}")]
    ProviderConfig(String),
    #[error("secret store error: {0}")]
    Secrets(String),
    #[error("config file error: {0}")]
    Config(String),
    #[error("MCP server error: {0}")]
    Mcp(String),
}

pub type Result<T> = std::result::Result<T, CoreError>;
