//! LLM呼び出しの失敗をユーザー向けのエラー発言に変換する。種別コードと`CoreError`からの
//! 分類をここ1箇所に閉じる。文言は言語ファイルの`turn_error.{種別コード}`にあり、画面は種別
//! コードから表示言語の文言を引く。

use crate::error::CoreError;
use crate::i18n::{self, Language};
use crate::llm::{ErrorDetail, LlmError, Readiness};

/// エラー発言としてDBに保存する1件分。`kind()`が`messages.error_kind`、
/// `user_message()`が`messages.content`、`detail()`が`messages.error_detail`に入る。
///
/// 文言は種別コードだけで決まる(行ごとの値を持たない)。画面が保存済みの行から
/// 同じ文言を引き直せるのは、このためである。
///
/// 詳細を持つかどうかはバリアントの形で決まる。持てるのは、アダプタがサニタイズした詳細と、
/// 秘密情報を含まない識別子だけで、どちらも`llm::ErrorDetail`のコンストラクタでしか作れない。
/// 鍵ストア・設定ファイル・MCPサーバー由来の失敗は、鍵名・パス・URLを含みうるため詳細を
/// 持たない。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TurnFailure {
    NoProvider,
    /// 起動時に設定ファイルを読めなかった。理由は設定画面に出す。
    SettingsUnreadable,
    NoModel,
    /// 応答タイムアウト(設定画面「一般」)までに応答を読み切れなかった。
    ResponseTimeout {
        detail: ErrorDetail,
    },
    /// 接続先に届かなかったか、応答の途中で接続が切れた。
    ConnectionFailed {
        detail: ErrorDetail,
    },
    /// 応答は届いたが、期待した形として読めなかった。
    InvalidResponse {
        detail: ErrorDetail,
    },
    EmptyResponse,
    /// 文言はコンテキスト長の設定を促す。設定すると履歴の間引き
    /// (`orchestration::history_trim`)がその長さに収めるため。
    ContextExceeded {
        detail: ErrorDetail,
    },
    /// 思考に対応しないモデルに思考の強さを送り、APIが拒んだ。
    ThinkingUnsupported {
        detail: ErrorDetail,
    },
    /// 選んだ思考の強さを、モデルが受け付けなかった。
    ThinkingEffortUnsupported {
        detail: ErrorDetail,
    },
    ToolRoundLimit,
    /// 1ターン内のツール実行に使える合計時間を使い切った。
    ToolTimeout,
    /// ユーザーが応答の生成を止めた(`orchestration::stop_response`)。
    Stopped,
    /// APIキーの未設定・不正。どちらも実際の呼び出しがHTTP 401/403を返してここに落ちる。
    Auth {
        detail: ErrorDetail,
    },
    RateLimit {
        detail: ErrorDetail,
    },
    /// モデルが応答を断った。言い方を変えるか、別のモデルで再試行してもらう。
    Refused {
        detail: ErrorDetail,
    },
    /// 鍵を登録したプロバイダーなのに、資格情報ストアから鍵を読めなかった。外部へは何も
    /// 送っていない。理由は設定画面に出す。
    KeyUnavailable,
    /// 設定不備(鍵ストア・プロバイダー設定・設定ファイル、ヘッダーに載せられない鍵)。
    /// 鍵名やパスを含みうるため詳細は出さない。
    ProviderConfig,
    /// 上記のいずれにも当たらない、プロバイダーがエラーとして返した応答。
    Provider {
        detail: ErrorDetail,
    },
    /// 内部エラー。detailにはバリアント相当の短い識別子だけを載せる。
    Unexpected {
        detail: ErrorDetail,
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
            TurnFailure::ThinkingEffortUnsupported { .. } => "thinking_effort_unsupported",
            TurnFailure::ToolRoundLimit => "tool_round_limit",
            TurnFailure::ToolTimeout => "tool_timeout",
            TurnFailure::Stopped => STOPPED_KIND,
            TurnFailure::Auth { .. } => "auth",
            TurnFailure::RateLimit { .. } => "rate_limit",
            TurnFailure::Refused { .. } => "refused",
            TurnFailure::KeyUnavailable => "key_unavailable",
            TurnFailure::ProviderConfig => "provider_config",
            TurnFailure::Provider { .. } => "provider",
            TurnFailure::Unexpected { .. } => "unexpected",
        }
    }

    /// `messages.content`に入れる英語の定型文言。エクスポートとアプリの外で読むためのもので、
    /// 画面は`kind()`から表示言語の文言を引く。
    pub fn user_message(&self) -> String {
        i18n::text(Language::En, &message_key(self.kind())).to_string()
    }

    /// 画面のエラー発言の「詳細」専用。`content`(定型文言)には混ぜない。
    pub fn detail(&self) -> Option<&str> {
        match self {
            TurnFailure::ResponseTimeout { detail }
            | TurnFailure::ConnectionFailed { detail }
            | TurnFailure::InvalidResponse { detail }
            | TurnFailure::ContextExceeded { detail }
            | TurnFailure::ThinkingUnsupported { detail }
            | TurnFailure::ThinkingEffortUnsupported { detail }
            | TurnFailure::Auth { detail }
            | TurnFailure::RateLimit { detail }
            | TurnFailure::Refused { detail }
            | TurnFailure::Provider { detail }
            | TurnFailure::Unexpected { detail } => Some(detail.as_str()),
            TurnFailure::NoProvider
            | TurnFailure::SettingsUnreadable
            | TurnFailure::NoModel
            | TurnFailure::EmptyResponse
            | TurnFailure::ToolRoundLimit
            | TurnFailure::ToolTimeout
            | TurnFailure::Stopped
            | TurnFailure::KeyUnavailable
            | TurnFailure::ProviderConfig => None,
        }
    }
}

/// エラー発言の文言のキーの前置き(後ろに種別コードを付ける)。画面も同じ前置きで引き直すので、
/// 画面へ値を書き出す(`i18n`のテストが`frontend/src/bindings/SharedConstants.ts`に書く)。
pub const MESSAGE_KEY_PREFIX: &str = "turn_error.";

/// 利用者が止めたターンの種別コード。
pub(crate) const STOPPED_KIND: &str = "stopped";

/// エラー発言の種別コードに対応する、表示言語の文言(画面がエラー発言の行に出すものと同じ)。
pub fn localized_message(lang: crate::i18n::Language, kind: &str) -> String {
    crate::i18n::text(lang, &message_key(kind)).to_string()
}

/// 種別コードに対応する言語ファイルのキー。
fn message_key(kind: &str) -> String {
    format!("{MESSAGE_KEY_PREFIX}{kind}")
}

/// アダプタが構成不足で呼び出しに進めない場合の分類。準備が整っていれば`None`。
pub fn from_readiness(readiness: Readiness) -> Option<TurnFailure> {
    match readiness {
        Readiness::Ready => None,
        Readiness::NoModel => Some(TurnFailure::NoModel),
    }
}

/// `CoreError`の全バリアントを網羅する(`_ =>`を書かない)。バリアントが増えたときに
/// このmatchがコンパイルエラーになることで、分類漏れが黙って`unexpected`に落ちるのを防ぐ。
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
        CoreError::MemoryNotFound(_) => unexpected("memory_not_found"),
        CoreError::MessageNotFound(_) => unexpected("message_not_found"),
        CoreError::AttachmentNotFound(_) => unexpected("attachment_not_found"),
        // 実体の置き場所のパスを含みうるので、文言は載せない。
        CoreError::Attachment(_) => unexpected("attachment"),
        CoreError::InvalidMessageOperation(_) => unexpected("invalid_message_operation"),
        // ターンの行を書く前に返すので、ターンの経路には来ない。
        CoreError::ChatBusy(_) => unexpected("chat_busy"),
        CoreError::UnknownArgument(_) => unexpected("unknown_argument"),
        CoreError::UnknownTool(_) => unexpected("unknown_tool"),
        CoreError::Internal(_) => unexpected("internal"),
        // 設定操作でだけ起きる。ターンの経路には来ない。
        CoreError::InvalidSettings(_) => unexpected("invalid_settings"),
        // 画面の入力を受け取る設定操作でだけ起きる。ターンの経路には来ない。
        CoreError::Rejected(_) => unexpected("rejected"),
        // 通信先の登録でだけ起きる。ターンの経路には来ない。
        CoreError::Cancelled => unexpected("cancelled"),
        // リンクを開く操作でだけ起きる。ターンの経路には来ない。
        CoreError::Link(_) => unexpected("link"),
        // エクスポートでだけ起きる。ターンの経路には来ない。
        CoreError::Export(_) => unexpected("export"),
        CoreError::InvalidArgument { .. } => unexpected("invalid_argument"),
    }
}

fn unexpected(detail: &'static str) -> TurnFailure {
    TurnFailure::Unexpected {
        detail: ErrorDetail::internal(detail),
    }
}

/// 種類の判定はアダプタが済ませている(`llm::LlmError`)。ここは種類を文言の側へ
/// 対応させるだけ。
fn from_llm_error(e: &LlmError) -> TurnFailure {
    match e {
        LlmError::InvalidRequest(_) => TurnFailure::ProviderConfig,
        LlmError::Timeout(detail) => TurnFailure::ResponseTimeout {
            detail: detail.clone(),
        },
        LlmError::Connection(detail) => TurnFailure::ConnectionFailed {
            detail: detail.clone(),
        },
        LlmError::InvalidResponse(detail) => TurnFailure::InvalidResponse {
            detail: detail.clone(),
        },
        LlmError::EmptyResponse => TurnFailure::EmptyResponse,
        LlmError::ContextExceeded(detail) => TurnFailure::ContextExceeded {
            detail: detail.clone(),
        },
        LlmError::ReasoningEffortRejected(detail) => TurnFailure::ThinkingUnsupported {
            detail: detail.clone(),
        },
        LlmError::ReasoningEffortValueRejected(detail) => TurnFailure::ThinkingEffortUnsupported {
            detail: detail.clone(),
        },
        LlmError::Auth(detail) => TurnFailure::Auth {
            detail: detail.clone(),
        },
        LlmError::RateLimit(detail) => TurnFailure::RateLimit {
            detail: detail.clone(),
        },
        LlmError::Refused(detail) => TurnFailure::Refused {
            detail: detail.clone(),
        },
        LlmError::Http(detail) => TurnFailure::Provider {
            detail: detail.clone(),
        },
    }
}

#[cfg(test)]
mod tests {
    use reqwest::StatusCode;

    use super::*;
    use crate::llm::SentSecrets;

    fn detail(text: &str) -> ErrorDetail {
        ErrorDetail::http(
            StatusCode::INTERNAL_SERVER_ERROR,
            text,
            &SentSecrets::default(),
        )
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
            (
                LlmError::ReasoningEffortValueRejected(detail("x")),
                "thinking_effort_unsupported",
            ),
            (LlmError::Auth(detail("x")), "auth"),
            (LlmError::RateLimit(detail("x")), "rate_limit"),
            (LlmError::Refused(detail("x")), "refused"),
            (LlmError::Http(detail("x")), "provider"),
        ];
        for (e, kind) in cases {
            assert_eq!(llm(e).kind(), kind);
        }
    }

    /// アダプタが作った詳細は詳細の列にだけ載せ、定型文言には混ぜない。
    #[test]
    fn llm_failures_keep_their_detail_out_of_the_user_message() {
        let with_detail = [
            LlmError::Timeout(detail("upstream overloaded")),
            LlmError::Connection(detail("upstream overloaded")),
            LlmError::InvalidResponse(detail("upstream overloaded")),
            LlmError::ContextExceeded(detail("upstream overloaded")),
            LlmError::ReasoningEffortRejected(detail("upstream overloaded")),
            LlmError::ReasoningEffortValueRejected(detail("upstream overloaded")),
            LlmError::Auth(detail("upstream overloaded")),
            LlmError::RateLimit(detail("upstream overloaded")),
            LlmError::Refused(detail("upstream overloaded")),
            LlmError::Http(detail("upstream overloaded")),
        ];
        for e in with_detail {
            let failure = llm(e);
            assert_eq!(failure.detail(), Some("HTTP 500: upstream overloaded"));
            assert!(!failure.user_message().contains("upstream overloaded"));
        }
    }

    /// 全種別。網羅的な`match`を置き、種別を足したらここがコンパイルエラーになるようにする。
    fn every_failure() -> Vec<TurnFailure> {
        let detail = || ErrorDetail::internal("x");
        let all = vec![
            TurnFailure::NoProvider,
            TurnFailure::SettingsUnreadable,
            TurnFailure::NoModel,
            TurnFailure::ResponseTimeout { detail: detail() },
            TurnFailure::ConnectionFailed { detail: detail() },
            TurnFailure::InvalidResponse { detail: detail() },
            TurnFailure::EmptyResponse,
            TurnFailure::ContextExceeded { detail: detail() },
            TurnFailure::ThinkingUnsupported { detail: detail() },
            TurnFailure::ThinkingEffortUnsupported { detail: detail() },
            TurnFailure::ToolRoundLimit,
            TurnFailure::ToolTimeout,
            TurnFailure::Stopped,
            TurnFailure::Auth { detail: detail() },
            TurnFailure::RateLimit { detail: detail() },
            TurnFailure::Refused { detail: detail() },
            TurnFailure::KeyUnavailable,
            TurnFailure::ProviderConfig,
            TurnFailure::Provider { detail: detail() },
            TurnFailure::Unexpected { detail: detail() },
        ];
        for failure in &all {
            match failure {
                TurnFailure::NoProvider
                | TurnFailure::SettingsUnreadable
                | TurnFailure::NoModel
                | TurnFailure::ResponseTimeout { .. }
                | TurnFailure::ConnectionFailed { .. }
                | TurnFailure::InvalidResponse { .. }
                | TurnFailure::EmptyResponse
                | TurnFailure::ContextExceeded { .. }
                | TurnFailure::ThinkingUnsupported { .. }
                | TurnFailure::ThinkingEffortUnsupported { .. }
                | TurnFailure::ToolRoundLimit
                | TurnFailure::ToolTimeout
                | TurnFailure::Stopped
                | TurnFailure::Auth { .. }
                | TurnFailure::RateLimit { .. }
                | TurnFailure::Refused { .. }
                | TurnFailure::KeyUnavailable
                | TurnFailure::ProviderConfig
                | TurnFailure::Provider { .. }
                | TurnFailure::Unexpected { .. } => {}
            }
        }
        all
    }

    /// 画面は保存済みの行の種別コードから文言を引き直すため、文言は行ごとの値を持てない。
    #[test]
    fn every_failure_has_a_message_without_placeholders() {
        for failure in every_failure() {
            let key = message_key(failure.kind());
            for lang in Language::ALL {
                let text = i18n::text(lang, &key);
                assert_ne!(text, key, "{} has no message", failure.kind());
                assert!(!text.contains('{'), "{key} must not take placeholders");
            }
        }
    }

    /// 保存する文言は英語(エクスポートの固定文言と揃える)。
    #[test]
    fn stored_message_is_english() {
        assert_eq!(
            TurnFailure::EmptyResponse.user_message(),
            i18n::text(Language::En, "turn_error.empty_response")
        );
        assert!(TurnFailure::EmptyResponse.user_message().is_ascii());
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
                detail: ErrorDetail::internal("mcp")
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
                detail: ErrorDetail::internal("task_not_found")
            }
        );
        assert!(!failure.user_message().contains("42"));
    }

    /// 上限に達したときの2種類は別の種別にする(文言がそれぞれの上限を案内するため)。
    #[test]
    fn tool_limit_failures_are_distinct_kinds() {
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
        // APIキー未設定・不正のどちらも、実際の呼び出しが401/403を返して初めて分かる。
        let failure = llm(LlmError::from_status(
            StatusCode::UNAUTHORIZED,
            "missing bearer token",
            &SentSecrets::default(),
        ));
        assert_eq!(failure.kind(), "auth");
        assert!(failure.user_message().contains("not set"));
    }
}
