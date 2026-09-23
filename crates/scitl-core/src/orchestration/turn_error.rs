//! LLM呼び出しの失敗をユーザー向けのエラー発言に変換する(Issue #40)。
//! 種別コード・文言・`CoreError`からの分類をここ1箇所に閉じる(`principles.md` 5節)。

use crate::db::error::CoreError;
use crate::llm::Readiness;

/// エラー発言としてDBに保存する1件分。`kind()`が`messages.error_kind`、
/// `user_message()`が`messages.content`に入る。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TurnFailure {
    NoProvider,
    NoModel,
    EmptyResponse,
    /// 上限が未設定なら設定を促すヒントを文言に加える(`legacy/backend.md` 4節手順6)。
    /// `config.rs`に上限の項目自体が無いため、#40時点では常に`false`。
    ContextExceeded {
        limit_configured: bool,
    },
    ToolRoundLimit,
    /// 1ターン内のツール実行に使える合計時間を使い切った(Issue #71)。
    ToolTimeout,
    /// APIキー未設定・不正のどちらも実際の呼び出しがHTTP 401/403を返してここに落ちる
    /// (`Readiness`のドキュメント参照。事前チェックでは「未設定」と「認証不要」を
    /// 区別できないため、実際に呼んで判定する設計)。
    Auth,
    RateLimit,
    /// 設定不備(鍵ストア・プロバイダー設定・設定ファイル)。鍵名やパスを含みうるため
    /// 詳細は出さない。
    ProviderConfig,
    /// 上記のいずれにも分類できないプロバイダー呼び出しの失敗。
    Provider,
    /// `CoreError::Llm`の中身が既知パターンに当たらなかった場合と、内部エラー。
    /// detailに載るのは、内部エラーならバリアント相当の短い識別子、`Llm`なら
    /// HTTP応答を伴わない失敗の文言(接続失敗・応答の解釈失敗等。`openai_compat.rs`が
    /// URLを剥がしてから作る)。HTTPエラーは状態コードで分類されるためここへは来ず、
    /// プロバイダ応答の本文は載らない。
    Unexpected {
        detail: String,
    },
}

impl TurnFailure {
    pub fn kind(&self) -> &'static str {
        match self {
            TurnFailure::NoProvider => "no_provider",
            TurnFailure::NoModel => "no_model",
            TurnFailure::EmptyResponse => "empty_response",
            TurnFailure::ContextExceeded { .. } => "context_exceeded",
            TurnFailure::ToolRoundLimit => "tool_round_limit",
            TurnFailure::ToolTimeout => "tool_timeout",
            TurnFailure::Auth => "auth",
            TurnFailure::RateLimit => "rate_limit",
            TurnFailure::ProviderConfig => "provider_config",
            TurnFailure::Provider => "provider",
            TurnFailure::Unexpected { .. } => "unexpected",
        }
    }

    pub fn user_message(&self) -> String {
        match self {
            TurnFailure::NoProvider => {
                "LLMプロバイダーが設定されていません。設定画面で追加してください。".to_string()
            }
            TurnFailure::NoModel => {
                "モデルが選択されていません。設定画面でモデルを選択してください。".to_string()
            }
            TurnFailure::EmptyResponse => {
                "モデルからの応答が空でした。もう一度お試しください。".to_string()
            }
            TurnFailure::ContextExceeded { limit_configured } => {
                if *limit_configured {
                    "会話がコンテキストの上限を超えました。".to_string()
                } else {
                    "会話がコンテキストの上限を超えた可能性があります。設定画面でコンテキスト\
                     上限を設定すると、次回から早めに警告できます。"
                        .to_string()
                }
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
            TurnFailure::Auth => {
                "APIキーが未設定か正しくないか、権限がありません。設定画面でAPIキーを\
                 確認してください。"
                    .to_string()
            }
            TurnFailure::RateLimit => {
                "APIの利用制限に達しました。しばらく待ってから再度お試しください。".to_string()
            }
            TurnFailure::ProviderConfig => {
                "プロバイダーの設定に問題があります。設定画面を確認してください。".to_string()
            }
            TurnFailure::Provider => "LLMプロバイダーとの通信に失敗しました。".to_string(),
            TurnFailure::Unexpected { detail } => {
                format!("予期しないエラーが発生しました: {detail}")
            }
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
        CoreError::Llm(detail) => classify_llm_error(detail),
        CoreError::Secrets(_) | CoreError::ProviderConfig(_) | CoreError::Config(_) => {
            TurnFailure::ProviderConfig
        }
        // MCPのツール呼び出しの失敗は`turn::execute_call`が結果JSONに落とすため、通常は
        // ここへ来ない。来た場合もプロバイダー起因の失敗として扱う(詳細はMCPサーバーの
        // URL等を含みうるため出さない)。
        CoreError::Mcp(_) => TurnFailure::Provider,
        // 内部エラー。ユーザーに見せて意味のある文言が作れないため`unexpected`に寄せるが、
        // detailにはバリアント名相当の短い識別子のみを載せ、生の`to_string()`は使わない。
        CoreError::Db(_) => unexpected("db"),
        CoreError::Migration(_) => unexpected("migration"),
        CoreError::TaskNotFound(_) => unexpected("task_not_found"),
        CoreError::TaskStepNotFound(_) => unexpected("task_step_not_found"),
        CoreError::MessageNotFound(_) => unexpected("message_not_found"),
        CoreError::InvalidMessageOperation(_) => unexpected("invalid_message_operation"),
        CoreError::UnknownArgument(_) => unexpected("unknown_argument"),
        CoreError::UnknownTool(_) => unexpected("unknown_tool"),
        CoreError::Internal(_) => unexpected("internal"),
        // 設定操作でだけ起きる。ターンの経路には来ない。
        CoreError::InvalidSettings(_) => unexpected("invalid_settings"),
        CoreError::InvalidArgument { .. } => unexpected("invalid_argument"),
    }
}

fn unexpected(detail: &str) -> TurnFailure {
    TurnFailure::Unexpected {
        detail: detail.to_string(),
    }
}

/// `CoreError::Llm`の中身を判定する。判定材料は`openai_compat.rs`が組み立てる
/// `"http {status}: {body}"`とそれ以外の固定文字列(`"empty choices"`等)のみで、
/// プロバイダ実装依存のため必ず外れるケースが残る。外れたものは`Unexpected`に落ちる。
fn classify_llm_error(detail: &str) -> TurnFailure {
    if detail == "empty choices" {
        return TurnFailure::EmptyResponse;
    }

    if let Some((status, body)) = parse_http_status(detail) {
        return match status {
            401 | 403 => TurnFailure::Auth,
            429 => TurnFailure::RateLimit,
            _ if looks_like_context_exceeded(body) => TurnFailure::ContextExceeded {
                limit_configured: false,
            },
            _ => TurnFailure::Provider,
        };
    }

    unexpected(detail)
}

fn parse_http_status(detail: &str) -> Option<(u16, &str)> {
    let rest = detail.strip_prefix("http ")?;
    let (status_str, body) = rest.split_once(':')?;
    let status = status_str.trim().parse().ok()?;
    Some((status, body))
}

fn looks_like_context_exceeded(body: &str) -> bool {
    let lower = body.to_lowercase();
    lower.contains("context_length_exceeded")
        || lower.contains("maximum context length")
        || lower.contains("context length")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_maps_known_llm_errors() {
        assert_eq!(
            classify(&CoreError::Llm("empty choices".to_string())),
            TurnFailure::EmptyResponse
        );
        assert_eq!(
            classify(&CoreError::Llm("http 401: unauthorized".to_string())),
            TurnFailure::Auth
        );
        assert_eq!(
            classify(&CoreError::Llm("http 429: rate limited".to_string())),
            TurnFailure::RateLimit
        );
        assert_eq!(
            classify(&CoreError::Llm(
                "http 400: This model's maximum context length is 8192 tokens".to_string()
            )),
            TurnFailure::ContextExceeded {
                limit_configured: false
            }
        );
        assert_eq!(
            classify(&CoreError::Llm("http 500: internal error".to_string())),
            TurnFailure::Provider
        );
    }

    #[test]
    fn classify_falls_back_to_unexpected_with_detail_for_unknown_llm_errors() {
        let failure = classify(&CoreError::Llm("connection reset by peer".to_string()));
        assert_eq!(
            failure,
            TurnFailure::Unexpected {
                detail: "connection reset by peer".to_string()
            }
        );
        assert!(failure.user_message().contains("connection reset by peer"));
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
        let failure = classify(&CoreError::Llm(
            "http 401: missing bearer token".to_string(),
        ));
        assert_eq!(failure, TurnFailure::Auth);
        assert!(failure.user_message().contains("未設定"));
    }
}
