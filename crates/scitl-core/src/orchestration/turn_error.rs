//! LLM呼び出しの失敗をユーザー向けのエラー発言に変換する(Issue #40)。
//! 種別コード・文言・`CoreError`からの分類をここ1箇所に閉じる(`principles.md` 5節)。

use crate::db::error::CoreError;
use crate::llm::{LlmError, Readiness};

/// エラー発言としてDBに保存する1件分。`kind()`が`messages.error_kind`、
/// `user_message()`が`messages.content`、`detail()`が`messages.error_detail`に入る。
///
/// 詳細を持つかどうかはバリアントの形で決まる(Issue #159)。持てるのは、アダプタが
/// サニタイズした詳細(`llm::ErrorDetail`)と、秘密情報を含まない識別子だけ。鍵ストア・
/// 設定ファイル・MCPサーバー由来の失敗は、鍵名・パス・URLを含みうるため詳細を持たない。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TurnFailure {
    NoProvider,
    /// 起動時に設定ファイルを読めなかった(Issue #155)。理由は設定画面に出す。
    SettingsUnreadable,
    NoModel,
    /// 応答タイムアウト(設定画面「一般」)までに応答を読み切れなかった。
    ResponseTimeout {
        detail: String,
    },
    /// 接続先に届かなかったか、応答の途中で接続が切れた。
    ConnectionFailed {
        detail: String,
    },
    /// 応答は届いたが、期待した形として読めなかった。
    InvalidResponse {
        detail: String,
    },
    EmptyResponse,
    /// 上限が未設定なら設定を促すヒントを文言に加える(`legacy/backend.md` 4節手順6)。
    /// `config.rs`に上限の項目自体が無いため、#40時点では常に`false`。
    ContextExceeded {
        limit_configured: bool,
        detail: String,
    },
    /// 思考に対応しないモデルに思考の強さを送り、APIが拒んだ。
    ThinkingUnsupported {
        detail: String,
    },
    ToolRoundLimit,
    /// 1ターン内のツール実行に使える合計時間を使い切った(Issue #71)。
    ToolTimeout,
    /// APIキー未設定・不正のどちらも実際の呼び出しがHTTP 401/403を返してここに落ちる
    /// (`Readiness`のドキュメント参照。事前チェックでは「未設定」と「認証不要」を
    /// 区別できないため、実際に呼んで判定する設計)。
    Auth {
        detail: String,
    },
    RateLimit {
        detail: String,
    },
    /// 設定不備(鍵ストア・プロバイダー設定・設定ファイル、ヘッダーに載せられない鍵)。
    /// 鍵名やパスを含みうるため詳細は出さない。
    ProviderConfig,
    /// 上記のいずれにも当たらない、プロバイダーがエラーとして返した応答。
    Provider {
        detail: String,
    },
    /// 内部エラー。detailにはバリアント相当の短い識別子だけを載せる。
    Unexpected {
        detail: String,
    },
}

impl TurnFailure {
    pub fn kind(&self) -> &'static str {
        match self {
            TurnFailure::NoProvider => "no_provider",
            TurnFailure::SettingsUnreadable => "settings_unreadable",
            TurnFailure::NoModel => "no_model",
            TurnFailure::ResponseTimeout { .. } => "response_timeout",
            TurnFailure::ConnectionFailed { .. } => "connection_failed",
            TurnFailure::InvalidResponse { .. } => "invalid_response",
            TurnFailure::EmptyResponse => "empty_response",
            TurnFailure::ContextExceeded { .. } => "context_exceeded",
            TurnFailure::ThinkingUnsupported { .. } => "thinking_unsupported",
            TurnFailure::ToolRoundLimit => "tool_round_limit",
            TurnFailure::ToolTimeout => "tool_timeout",
            TurnFailure::Auth { .. } => "auth",
            TurnFailure::RateLimit { .. } => "rate_limit",
            TurnFailure::ProviderConfig => "provider_config",
            TurnFailure::Provider { .. } => "provider",
            TurnFailure::Unexpected { .. } => "unexpected",
        }
    }

    pub fn user_message(&self) -> String {
        match self {
            TurnFailure::NoProvider => {
                "LLMプロバイダーが設定されていません。設定画面で追加してください。".to_string()
            }
            TurnFailure::SettingsUnreadable => {
                "設定ファイルを読み込めませんでした。設定画面で詳細を確認してください。".to_string()
            }
            TurnFailure::NoModel => {
                "モデルが選択されていません。設定画面でモデルを選択してください。".to_string()
            }
            TurnFailure::ResponseTimeout { .. } => {
                "LLMプロバイダーの応答が時間内に届きませんでした。設定画面「一般」の\
                 応答タイムアウトで待ち時間を変更できます。"
                    .to_string()
            }
            TurnFailure::ConnectionFailed { .. } => {
                "LLMプロバイダーとの通信に失敗しました。接続先のサーバーが動いているか、\
                 ネットワークの状態を確認してください。"
                    .to_string()
            }
            TurnFailure::InvalidResponse { .. } => {
                "LLMプロバイダーの応答を解釈できませんでした。設定画面で接続先のURLを\
                 確認してください。"
                    .to_string()
            }
            TurnFailure::EmptyResponse => {
                "モデルからの応答が空でした。もう一度お試しください。".to_string()
            }
            TurnFailure::ContextExceeded {
                limit_configured, ..
            } => {
                if *limit_configured {
                    "会話がコンテキストの上限を超えました。".to_string()
                } else {
                    "会話がコンテキストの上限を超えた可能性があります。設定画面でコンテキスト\
                     上限を設定すると、次回から早めに警告できます。"
                        .to_string()
                }
            }
            TurnFailure::ThinkingUnsupported { .. } => {
                "このモデルは思考の強さの指定を受け付けませんでした。設定画面「APIプロバイダー」の\
                 モデル一覧で、このモデルの「思考」のチェックを外してください。"
                    .to_string()
            }
            TurnFailure::ToolRoundLimit => {
                "ツールの呼び出しが上限回数に達したため、応答の生成を打ち切りました。\
                 設定画面「ツール/MCP」で上限を変更できます。"
                    .to_string()
            }
            TurnFailure::ToolTimeout => {
                "ツールの実行時間が上限に達したため、応答の生成を打ち切りました。\
                 設定画面「ツール/MCP」で上限を変更できます。"
                    .to_string()
            }
            TurnFailure::Auth { .. } => {
                "APIキーが未設定か正しくないか、権限がありません。設定画面でAPIキーを\
                 確認してください。"
                    .to_string()
            }
            TurnFailure::RateLimit { .. } => {
                "APIの利用制限に達しました。しばらく待ってから再度お試しください。".to_string()
            }
            TurnFailure::ProviderConfig => {
                "プロバイダーの設定に問題があります。設定画面を確認してください。".to_string()
            }
            TurnFailure::Provider { .. } => "LLMプロバイダーがエラーを返しました。".to_string(),
            TurnFailure::Unexpected { .. } => "予期しないエラーが発生しました。".to_string(),
        }
    }

    /// 画面の「詳細を表示」専用。`content`(定型文言)には混ぜない。
    pub fn detail(&self) -> Option<&str> {
        match self {
            TurnFailure::ResponseTimeout { detail }
            | TurnFailure::ConnectionFailed { detail }
            | TurnFailure::InvalidResponse { detail }
            | TurnFailure::ContextExceeded { detail, .. }
            | TurnFailure::ThinkingUnsupported { detail }
            | TurnFailure::Auth { detail }
            | TurnFailure::RateLimit { detail }
            | TurnFailure::Provider { detail }
            | TurnFailure::Unexpected { detail } => Some(detail),
            TurnFailure::NoProvider
            | TurnFailure::SettingsUnreadable
            | TurnFailure::NoModel
            | TurnFailure::EmptyResponse
            | TurnFailure::ToolRoundLimit
            | TurnFailure::ToolTimeout
            | TurnFailure::ProviderConfig => None,
        }
    }
}

/// アダプタが構成不足で呼び出しに進めない場合の分類。準備が整っていれば`None`。
pub fn from_readiness(readiness: Readiness) -> Option<TurnFailure> {
    match readiness {
        Readiness::Ready => None,
        Readiness::NoModel => Some(TurnFailure::NoModel),
    }
}

/// `CoreError`の全バリアントを網羅する(`_ =>`を書かない)。バリアントが増えたときに
/// このmatchがコンパイルエラーになることで、分類漏れが黙って`unexpected`に落ちるのを防ぐ
/// (`principles.md` 1節「症状ではなく原因を直す」)。
pub fn classify(err: &CoreError) -> TurnFailure {
    match err {
        CoreError::Llm(e) => from_llm_error(e),
        CoreError::Secrets(_) | CoreError::ProviderConfig(_) | CoreError::Config(_) => {
            TurnFailure::ProviderConfig
        }
        // ツール呼び出しの失敗は`turn::execute_call`が結果JSONに落とし、ツール一覧を
        // 取れなかったサーバーは除いて続けるため、ターンの経路には来ない。
        CoreError::Mcp(_) => unexpected("mcp"),
        // 内部エラー。ユーザーに見せて意味のある文言が作れないため`unexpected`に寄せるが、
        // detailにはバリアント名相当の短い識別子のみを載せ、生の`to_string()`は使わない。
        CoreError::Db(_) => unexpected("db"),
        CoreError::Migration(_) => unexpected("migration"),
        CoreError::TaskNotFound(_) => unexpected("task_not_found"),
        CoreError::TaskStepNotFound(_) => unexpected("task_step_not_found"),
        CoreError::MessageNotFound(_) => unexpected("message_not_found"),
        CoreError::InvalidMessageOperation(_) => unexpected("invalid_message_operation"),
        // ターンの行を書く前に返すので、ターンの経路には来ない。
        CoreError::TaskBusy(_) => unexpected("task_busy"),
        CoreError::UnknownArgument(_) => unexpected("unknown_argument"),
        CoreError::UnknownTool(_) => unexpected("unknown_tool"),
        CoreError::Internal(_) => unexpected("internal"),
        // 設定操作でだけ起きる。ターンの経路には来ない。
        CoreError::InvalidSettings(_) => unexpected("invalid_settings"),
        // リンクを開く操作でだけ起きる。ターンの経路には来ない。
        CoreError::Link(_) => unexpected("link"),
        CoreError::InvalidArgument { .. } => unexpected("invalid_argument"),
    }
}

fn unexpected(detail: &str) -> TurnFailure {
    TurnFailure::Unexpected {
        detail: detail.to_string(),
    }
}

/// 種類の判定はアダプタが済ませている(`llm::LlmError`)。ここは種類を文言の側へ
/// 対応させるだけ。
fn from_llm_error(e: &LlmError) -> TurnFailure {
    match e {
        LlmError::InvalidRequest(_) => TurnFailure::ProviderConfig,
        LlmError::Timeout(detail) => TurnFailure::ResponseTimeout {
            detail: detail.to_string(),
        },
        LlmError::Connection(detail) => TurnFailure::ConnectionFailed {
            detail: detail.to_string(),
        },
        LlmError::InvalidResponse(detail) => TurnFailure::InvalidResponse {
            detail: detail.to_string(),
        },
        LlmError::EmptyResponse => TurnFailure::EmptyResponse,
        LlmError::ContextExceeded(detail) => TurnFailure::ContextExceeded {
            limit_configured: false,
            detail: detail.to_string(),
        },
        LlmError::ReasoningEffortRejected(detail) => TurnFailure::ThinkingUnsupported {
            detail: detail.to_string(),
        },
        LlmError::Auth(detail) => TurnFailure::Auth {
            detail: detail.to_string(),
        },
        LlmError::RateLimit(detail) => TurnFailure::RateLimit {
            detail: detail.to_string(),
        },
        LlmError::Http(detail) => TurnFailure::Provider {
            detail: detail.to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use reqwest::StatusCode;

    use super::*;
    use crate::llm::ErrorDetail;

    fn detail(text: &str) -> ErrorDetail {
        ErrorDetail::http(StatusCode::INTERNAL_SERVER_ERROR, text, "")
    }

    fn llm(e: LlmError) -> TurnFailure {
        classify(&CoreError::Llm(e))
    }

    #[test]
    fn classify_maps_each_llm_error_to_its_kind() {
        let cases = [
            (LlmError::InvalidRequest(detail("x")), "provider_config"),
            (LlmError::Timeout(detail("x")), "response_timeout"),
            (LlmError::Connection(detail("x")), "connection_failed"),
            (LlmError::InvalidResponse(detail("x")), "invalid_response"),
            (LlmError::EmptyResponse, "empty_response"),
            (LlmError::ContextExceeded(detail("x")), "context_exceeded"),
            (
                LlmError::ReasoningEffortRejected(detail("x")),
                "thinking_unsupported",
            ),
            (LlmError::Auth(detail("x")), "auth"),
            (LlmError::RateLimit(detail("x")), "rate_limit"),
            (LlmError::Http(detail("x")), "provider"),
        ];
        for (e, kind) in cases {
            assert_eq!(llm(e).kind(), kind);
        }
    }

    /// アダプタが作った詳細は詳細の列にだけ載せ、定型文言には混ぜない(Issue #159)。
    #[test]
    fn llm_failures_keep_their_detail_out_of_the_user_message() {
        let with_detail = [
            LlmError::Timeout(detail("upstream overloaded")),
            LlmError::Connection(detail("upstream overloaded")),
            LlmError::InvalidResponse(detail("upstream overloaded")),
            LlmError::ContextExceeded(detail("upstream overloaded")),
            LlmError::ReasoningEffortRejected(detail("upstream overloaded")),
            LlmError::Auth(detail("upstream overloaded")),
            LlmError::RateLimit(detail("upstream overloaded")),
            LlmError::Http(detail("upstream overloaded")),
        ];
        for e in with_detail {
            let failure = llm(e);
            assert_eq!(failure.detail(), Some("HTTP 500: upstream overloaded"));
            assert!(!failure.user_message().contains("upstream overloaded"));
        }
    }

    /// 応答タイムアウトは設定で延ばせると伝える。接続の失敗の文言では、タイムアウトの
    /// 設定を案内しない。
    #[test]
    fn response_timeout_points_at_the_setting() {
        let failure = llm(LlmError::Timeout(detail("x")));
        assert!(failure.user_message().contains("「一般」"));
        assert!(failure.user_message().contains("応答タイムアウト"));
        assert!(!llm(LlmError::Connection(detail("x")))
            .user_message()
            .contains("タイムアウト"));
    }

    /// ヘッダーに載せられない鍵は設定の不備として伝え、詳細は持たせない。
    #[test]
    fn invalid_request_is_a_provider_config_failure() {
        assert_eq!(
            llm(LlmError::InvalidRequest(detail("x"))),
            TurnFailure::ProviderConfig
        );
    }

    /// MCP由来の失敗はURL等を含みうるため、詳細を識別子だけにする。
    #[test]
    fn classify_drops_mcp_details() {
        let failure = classify(&CoreError::Mcp(
            "http://192.168.1.2:9000/mcp refused".to_string(),
        ));
        assert_eq!(
            failure,
            TurnFailure::Unexpected {
                detail: "mcp".to_string()
            }
        );
    }

    #[test]
    fn classify_never_leaks_secret_store_or_config_details() {
        let cases = [
            CoreError::Secrets("keyring locked at /home/user/.keyring".to_string()),
            CoreError::ProviderConfig("base_url is not a valid URL: /etc/secret".to_string()),
            CoreError::Config("failed to read config.toml: /home/user/secret".to_string()),
        ];
        for err in cases {
            let failure = classify(&err);
            assert_eq!(failure, TurnFailure::ProviderConfig);
            assert_eq!(failure.detail(), None);
            assert!(!failure.user_message().contains("keyring"));
            assert!(!failure.user_message().contains("secret"));
        }
    }

    #[test]
    fn classify_reduces_internal_errors_to_unexpected_without_raw_detail() {
        let failure = classify(&CoreError::TaskNotFound(42));
        assert_eq!(
            failure,
            TurnFailure::Unexpected {
                detail: "task_not_found".to_string()
            }
        );
        assert!(!failure.user_message().contains("42"));
    }

    /// 上限に達したときの2種類は、どちらも「設定で変えられる」と伝える(Issue #71。
    /// 変える手段が無いという元の不満がここに出るため)。
    #[test]
    fn tool_limit_failures_point_at_the_setting() {
        for failure in [TurnFailure::ToolRoundLimit, TurnFailure::ToolTimeout] {
            assert!(failure.user_message().contains("ツール/MCP"));
        }
        assert_ne!(
            TurnFailure::ToolRoundLimit.kind(),
            TurnFailure::ToolTimeout.kind()
        );
    }

    #[test]
    fn from_readiness_maps_unready_states() {
        assert_eq!(from_readiness(Readiness::Ready), None);
        assert_eq!(
            from_readiness(Readiness::NoModel),
            Some(TurnFailure::NoModel)
        );
    }

    #[test]
    fn classify_maps_missing_or_invalid_api_key_to_auth() {
        // APIキー未設定・不正のどちらも、実際の呼び出しが401/403を返すことで初めて
        // 判明する(事前チェックでは「未設定」と「ローカルプロバイダーの認証不要」を
        // 区別できないため)。
        let failure = llm(LlmError::from_status(
            StatusCode::UNAUTHORIZED,
            "missing bearer token",
            "",
        ));
        assert_eq!(failure.kind(), "auth");
        assert!(failure.user_message().contains("未設定"));
    }
}
