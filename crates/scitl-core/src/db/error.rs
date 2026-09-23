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
    #[error("message {0} not found")]
    MessageNotFound(i64),
    #[error("invalid message operation: {0}")]
    InvalidMessageOperation(String),
    /// そのタスクは既に応答を生成中(`orchestration::TurnContext::generating`)。
    #[error("task {0} is already generating a response")]
    TaskBusy(i64),
    #[error("unknown argument: {0}")]
    UnknownArgument(String),
    /// モデルが公開していないツール名を呼んだ。
    #[error("unknown tool: {0}")]
    UnknownTool(String),
    #[error("invalid argument {name}: {reason}")]
    InvalidArgument { name: String, reason: String },
    /// HTTP応答を伴わないプロバイダー呼び出しの失敗(接続失敗・応答の解釈失敗等)。
    #[error("llm provider error: {0}")]
    Llm(String),
    /// プロバイダーが非成功の状態コードを返した。`body`はアダプタがサニタイズ済みのもの
    /// (`openai_compat::sanitize_error_body`)に限る。
    #[error("llm provider returned http {status}: {body}")]
    LlmHttp { status: u16, body: String },
    #[error("invalid provider configuration: {0}")]
    ProviderConfig(String),
    #[error("secret store error: {0}")]
    Secrets(String),
    #[error("config file error: {0}")]
    Config(String),
    /// 設定・登録の操作が規則に反する(空の名前、未登録のID、重複、範囲外の値)。
    #[error("invalid settings: {0}")]
    InvalidSettings(String),
    #[error("MCP server error: {0}")]
    Mcp(String),
    /// 実行基盤側の失敗(ブロッキング処理のタスクがパニックした等)。
    #[error("internal error: {0}")]
    Internal(String),
}

pub type Result<T> = std::result::Result<T, CoreError>;
