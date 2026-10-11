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
    /// 新しい通信先の登録を、利用者が確認のダイアログで取りやめた(`settings::DestinationDialog`)。
    #[error("the registration was cancelled")]
    Cancelled,
    #[error("MCP server error: {0}")]
    Mcp(String),
    /// リンクを開けない(許可されていない・解釈できないURL、OS側の起動失敗)。
    #[error("link error: {0}")]
    Link(String),
    /// 実行基盤側の失敗(ブロッキング処理のタスクがパニックした等)。
    #[error("internal error: {0}")]
    Internal(String),
}

/// コマンドの失敗の文言のキーの前置き(言語ファイルの`command_error.{種類}`)。画面は[`CoreError::kind`]を
/// 付けて引く(`docs/spec/architecture/i18n.md`「coreの文言とエラー発言」)。
pub const MESSAGE_KEY_PREFIX: &str = "command_error.";

impl CoreError {
    /// 失敗の種類の安定したコード。画面はこれから表示言語の文言を引き、表示文(英語の診断文)は
    /// 詳細として添える。表示文はツールのエラーとしてモデルにも渡り、ログとも共有するので、
    /// 表示言語には訳さない(`docs/spec/architecture/prompt-shape.md`「モデルに渡す固定文言は
    /// 英語で書く」)。画面が表示文を照合して訳すと、文言を変えたときに黙って漏れる。
    ///
    /// 種類を足したら、言語ファイルの`command_error.{種類}`も足す(テストが照らす)。
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Db(_) => "database",
            Self::Migration(_) => "migration",
            Self::TaskNotFound(_) => "task_not_found",
            Self::TaskStepNotFound(_) => "task_step_not_found",
            Self::MemoryNotFound(_) => "memory_not_found",
            Self::MessageNotFound(_) => "message_not_found",
            Self::AttachmentNotFound(_) => "attachment_not_found",
            Self::Attachment(_) => "attachment",
            Self::Export(_) => "export",
            Self::InvalidMessageOperation(_) => "invalid_message_operation",
            Self::ChatBusy(_) => "chat_busy",
            Self::UnknownArgument(_) => "unknown_argument",
            Self::UnknownTool(_) => "unknown_tool",
            Self::InvalidArgument { .. } => "invalid_argument",
            Self::Llm(_) => "llm",
            Self::ProviderConfig(_) => "provider_config",
            Self::Secrets(_) => "secrets",
            Self::Config(_) => "config",
            Self::InvalidSettings(_) => "invalid_settings",
            Self::Rejected(_) => "rejected",
            Self::Cancelled => "cancelled",
            Self::Mcp(_) => "mcp",
            Self::Link(_) => "link",
            Self::Internal(_) => "internal",
        }
    }
}

pub type Result<T> = std::result::Result<T, CoreError>;

/// コマンドの失敗を画面へ渡す形。画面は`kind`から表示言語の文言を引き、`detail`(英語の診断文)を
/// 詳細として添える。`detail`は外部から来た文字列(プロバイダーやMCPサーバーの応答)を含みうるので、
/// 画面はプレーンテキストとして出す。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
pub struct CommandFailure {
    /// [`CoreError::kind`]。
    pub kind: &'static str,
    /// [`CoreError`]の表示文。
    pub detail: String,
}

impl From<&CoreError> for CommandFailure {
    fn from(error: &CoreError) -> Self {
        Self {
            kind: error.kind(),
            detail: error.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::{self, Language};

    /// 全種類。網羅的な`match`を置き、種類を足したらここがコンパイルエラーになるようにする。
    fn every_error() -> Vec<CoreError> {
        let text = || "x".to_string();
        let all = vec![
            CoreError::Db(rusqlite::Error::InvalidQuery),
            CoreError::Migration(text()),
            CoreError::TaskNotFound(1),
            CoreError::TaskStepNotFound(1),
            CoreError::MemoryNotFound(1),
            CoreError::MessageNotFound(1),
            CoreError::AttachmentNotFound(1),
            CoreError::Attachment(text()),
            CoreError::Export(text()),
            CoreError::InvalidMessageOperation(text()),
            CoreError::ChatBusy(crate::db::messages::Chat::General),
            CoreError::UnknownArgument(text()),
            CoreError::UnknownTool(text()),
            CoreError::InvalidArgument {
                name: text(),
                reason: text(),
            },
            CoreError::Llm(crate::llm::LlmError::EmptyResponse),
            CoreError::ProviderConfig(text()),
            CoreError::Secrets(text()),
            CoreError::Config(text()),
            CoreError::InvalidSettings(text()),
            CoreError::Rejected(crate::settings::Rejections(Vec::new())),
            CoreError::Cancelled,
            CoreError::Mcp(text()),
            CoreError::Link(text()),
            CoreError::Internal(text()),
        ];
        for error in &all {
            match error {
                CoreError::Db(_)
                | CoreError::Migration(_)
                | CoreError::TaskNotFound(_)
                | CoreError::TaskStepNotFound(_)
                | CoreError::MemoryNotFound(_)
                | CoreError::MessageNotFound(_)
                | CoreError::AttachmentNotFound(_)
                | CoreError::Attachment(_)
                | CoreError::Export(_)
                | CoreError::InvalidMessageOperation(_)
                | CoreError::ChatBusy(_)
                | CoreError::UnknownArgument(_)
                | CoreError::UnknownTool(_)
                | CoreError::InvalidArgument { .. }
                | CoreError::Llm(_)
                | CoreError::ProviderConfig(_)
                | CoreError::Secrets(_)
                | CoreError::Config(_)
                | CoreError::InvalidSettings(_)
                | CoreError::Rejected(_)
                | CoreError::Cancelled
                | CoreError::Mcp(_)
                | CoreError::Link(_)
                | CoreError::Internal(_) => {}
            }
        }
        all
    }

    #[test]
    fn a_command_failure_carries_the_kind_and_the_diagnostic() {
        assert_eq!(
            CommandFailure::from(&CoreError::TaskNotFound(3)),
            CommandFailure {
                kind: "task_not_found",
                detail: "task 3 not found".to_string(),
            }
        );
    }

    /// どの種類にも、すべての表示言語の文言がある。値は差し込まない(詳細は画面が後ろに添える)。
    #[test]
    fn every_kind_has_a_message_in_every_language() {
        let mut kinds = std::collections::HashSet::new();
        for error in every_error() {
            let kind = error.kind();
            assert!(kinds.insert(kind), "{kind} is used by two errors");
            let key = format!("{MESSAGE_KEY_PREFIX}{kind}");
            for lang in Language::ALL {
                let text = i18n::text(lang, &key);
                assert_ne!(text, key, "{kind} has no message");
                assert!(!text.contains('{'), "{key} must not take placeholders");
            }
        }
    }
}
