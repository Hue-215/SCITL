//! crate共通のエラー型。DB・LLM・MCP・設定・添付のどの層も、呼び出し元へはこの型で返す。

#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    #[error("database error: {0}")]
    Db(#[from] rusqlite::Error),
    #[error("migration error: {0}")]
    Migration(String),
    #[error("task {0} not found")]
    TaskNotFound(i64),
    #[error("task step {0} not found")]
    TaskStepNotFound(i64),
    #[error("memory {0} not found")]
    MemoryNotFound(i64),
    #[error("message {0} not found")]
    MessageNotFound(i64),
    #[error("attachment {0} not found")]
    AttachmentNotFound(i64),
    /// 添付の実体の読み書き・送信前の添付の取り出しの失敗。
    #[error("attachment error: {0}")]
    Attachment(String),
    /// Markdownエクスポートの書き込みの失敗。
    #[error("export error: {0}")]
    Export(String),
    #[error("invalid message operation: {0}")]
    InvalidMessageOperation(String),
    /// その会話は既に応答を生成中(`orchestration::TurnContext::generating`)。
    #[error("{0} is already generating a response")]
    ChatBusy(crate::db::messages::Chat),
    #[error("unknown argument: {0}")]
    UnknownArgument(String),
    /// モデルが公開していないツール名を呼んだ。
    #[error("unknown tool: {0}")]
    UnknownTool(String),
    #[error("invalid argument {name}: {reason}")]
    InvalidArgument { name: String, reason: String },
    /// LLMプロバイダーの呼び出しの失敗。種類はアダプタが決める。
    #[error("llm provider error: {0}")]
    Llm(#[from] crate::llm::LlmError),
    #[error("invalid provider configuration: {0}")]
    ProviderConfig(String),
    #[error("secret store error: {0}")]
    Secrets(String),
    #[error("config file error: {0}")]
    Config(String),
    /// 設定・登録の操作が規則に反する(空の名前、未登録のID、重複、範囲外の値)。
    #[error("invalid settings: {0}")]
    InvalidSettings(String),
    /// 画面の入力を受け付けなかった。画面が欄の近くに表示言語で出す理由で、種類のまま返す
    /// (`settings::FormOutcome`)。
    #[error("input rejected: {0}")]
    Rejected(crate::settings::Rejections),
    #[error("MCP server error: {0}")]
    Mcp(String),
    /// リンクを開けない(許可されていない・解釈できないURL、OS側の起動失敗)。
    #[error("link error: {0}")]
    Link(String),
    /// 実行基盤側の失敗(ブロッキング処理のタスクがパニックした等)。
    #[error("internal error: {0}")]
    Internal(String),
}

pub type Result<T> = std::result::Result<T, CoreError>;
