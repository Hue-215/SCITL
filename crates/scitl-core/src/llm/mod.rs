mod capabilities;
mod error;
mod prompt;
pub mod providers;
mod token_estimate;

use std::sync::Arc;

use serde::{Deserialize, Serialize};

pub use capabilities::{
    fallback_capabilities, resolve_capabilities, CapabilityLayer, DetectedCapabilities,
    DetectedCatalog, ModelCapabilities, DEFAULT_CAPABILITIES, FALLBACK_CONTEXT_LENGTH,
};
pub use error::{ErrorDetail, LlmError};
pub use prompt::{user_message_format_note, AttachmentNote, PromptText};
pub use token_estimate::{estimate_message, estimate_tools};

use crate::config::ReasoningEffort;
use crate::error::CoreError;

/// アダプタ層が上位に渡す形は完成した応答1つではなくイベントの並び
/// (docs/spec/principles.md 3節「応答はイベントの並びとして受け取る」、Issue #8)。
/// ストリーミングしないプロバイダーも各イベントを1回ずつ渡せば同じ経路に乗る
/// ([`LlmAdapter::send`])。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ResponseEvent {
    TextDelta {
        text: String,
    },
    /// モデルの思考(reasoning)の断片(Issue #42)。表示・`messages.reasoning`への保存
    /// 専用のイベントであり、`ChatMessage`には対応する構成要素が無い
    /// (`docs/spec/principles.md` 3節「思考は履歴に送り返さない」)。次のAPI呼び出しの
    /// 入力に混ざり込む経路が型として存在しないようにするための意図的な非対称設計。
    ReasoningDelta {
        text: String,
    },
    ToolCall {
        /// プロバイダが払い出した呼び出しID。1応答に複数のツール呼び出しが載る場合に
        /// 結果と対応付けるために保持する。払い出さないプロバイダもあるためOption。
        id: Option<String>,
        name: String,
        arguments: ToolArguments,
    },
    Done {
        finish_reason: FinishReason,
    },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
#[serde(rename_all = "snake_case")]
pub enum FinishReason {
    Stop,
    ToolCall,
    /// 出力が長さ制限で打ち切られた。Stopに潰すと打ち切りを上位層が検知できない。
    Length,
}

/// アダプタに渡す発言列の1要素。`{role, content}`の平坦な構造ではなく列挙型にするのは、
/// 「userなのに`tool_call_id`を持つ」といった不正な組み合わせを型で防ぐため
/// (docs/spec/rebuild/architecture.md 3節。`config.rs`の`McpEndpoint`と同じ理由)。
///
/// ツール呼び出しと結果は、同一ターン内のループでは分類(状態系/事実系)によらず
/// モデルに返す(docs/spec/rebuild/tools.md 4節)。この往復を表現するために
/// `Assistant`の`tool_calls`と`Tool`を持つ。DBの`messages`テーブルには保存しない
/// (次ターン以降の入力履歴に残さないのはdocs/spec/principles.md 3節、テーブルへの
/// 不保存はdocs/spec/rebuild/data-model.md 2節)。
#[derive(Debug, Clone, PartialEq)]
pub enum ChatMessage {
    System(String),
    /// ユーザー発言。送信日時と添付の情報を本文の外に置いた囲みとして、組み立て済みの形で運ぶ
    /// ([`PromptText::user_message`]。Issue #68。`docs/spec/legacy/backend.md` 4節手順2
    /// 「本文とは別の構造化情報として付与する。地の文に混ぜない」)。`images`は一緒に送る
    /// 添付画像(Issue #21)で、どの画像を送るかは`attachments::delivery`が決める。
    User {
        text: PromptText,
        images: Vec<InlineImage>,
    },
    Assistant {
        content: Option<String>,
        tool_calls: Vec<ToolCallRequest>,
    },
    Tool {
        /// プロバイダが払い出した呼び出しIDをそのまま返す。捏造しない
        /// (払い出さないプロバイダにはこのフィールド自体を送らない。architecture.md 3節)。
        tool_call_id: Option<String>,
        content: PromptText,
        /// 結果として返す画像(添付の読み込み。Issue #213)。ツール結果に画像を載せられない
        /// APIがあるため、リクエストでどう表すかはアダプタが決める。
        images: Vec<InlineImage>,
    },
}

impl ChatMessage {
    /// 画像を伴わないユーザー発言。
    pub fn user(text: PromptText) -> Self {
        Self::User {
            text,
            images: Vec::new(),
        }
    }
}

/// ユーザー発言・ツール結果と一緒に送る画像。MIMEは実体の先頭バイトから決めたもの
/// (`attachments::image_mime_type`)に限る。
///
/// data URLは作るときに1度だけ組み立てて共有する。履歴はツールの往復のラウンドごとに
/// 複製されるので、画像の本体まで写さないため。
#[derive(Debug, Clone, PartialEq)]
pub struct InlineImage {
    data_url: Arc<str>,
}

impl InlineImage {
    /// 画像として扱う形式でなければ`None`。
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        use base64::Engine;
        let mut data_url = format!(
            "data:{};base64,",
            crate::attachments::image_mime_type(bytes)?
        );
        base64::engine::general_purpose::STANDARD.encode_string(bytes, &mut data_url);
        Some(Self {
            data_url: data_url.into(),
        })
    }

    pub fn data_url(&self) -> &str {
        &self.data_url
    }
}

/// モデルが出したツール呼び出し1件。往復のために`Assistant`側で保持する。
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ToolCallRequest {
    pub id: Option<String>,
    pub name: String,
    pub arguments: ToolArguments,
}

/// ツール呼び出しの引数。JSONとして読めなかった場合もアダプタで`Err`にせず、生の文字列の
/// まま上位へ運ぶ。読めない引数は性能の低いモデルでは日常的に起こり、ターンごと止めると
/// principles.md 3節「失敗しても会話を止めない」を割るため。かといって空オブジェクト等へ
/// 置き換えると同節「暗黙の型変換をしない」を割る(引数を伴うツールを引数無しで発火させる)。
/// 実行せずに失敗をモデルへ返す判断は`orchestration::turn`が持つ。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ToolArguments {
    Valid {
        #[cfg_attr(test, ts(type = "unknown"))]
        value: serde_json::Value,
    },
    Malformed {
        raw: String,
        error: String,
    },
}

impl ToolArguments {
    /// プロバイダーが文字列で返した引数を読む。JSONとして読める値であれば型は問わない
    /// (オブジェクトかどうかの検証は各ツールの引数検証に任せる)。
    pub fn parse(raw: String) -> Self {
        match serde_json::from_str(&raw) {
            Ok(value) => Self::Valid { value },
            Err(e) => Self::Malformed {
                raw,
                error: e.to_string(),
            },
        }
    }

    /// モデルへ送り返す文字列。往復であり、こちらで内容を作り変えないため、読めなかった
    /// 引数は受け取った文字列をそのまま返す。
    pub fn to_wire_string(&self) -> String {
        match self {
            Self::Valid { value } => value.to_string(),
            Self::Malformed { raw, .. } => raw.clone(),
        }
    }
}

impl From<serde_json::Value> for ToolArguments {
    fn from(value: serde_json::Value) -> Self {
        Self::Valid { value }
    }
}

/// モデルへ公開するツールの定義。どちらのコンストラクタを通ったかで、説明と引数スキーマに
/// 無害化が掛かっているかが決まる(docs/spec/rebuild/architecture.md 10節)。
#[derive(Debug, Clone)]
pub struct ToolSchema {
    name: String,
    description: String,
    parameters: serde_json::Value,
}

impl ToolSchema {
    /// アプリ自身が書いた定義(内部ツール)。名前と説明を`&'static str`に限るのは、実行時の
    /// 文字列(外部から来たもの)がこの経路で無害化を通らずに入るのを防ぐため。引数スキーマは
    /// 実行時に組み立てうる(DBの値を`enum`に並べる等)ので、外部と同じく無害化を掛ける。
    ///
    /// # Panics
    ///
    /// 無害化した引数スキーマを読み直せない場合。置き換えはJSONの形を崩さないため起きず、
    /// 起きればコード側の誤りなので、内部ツールの定義を組み立てるテストで止める。
    pub fn internal(
        name: &'static str,
        description: &'static str,
        parameters: serde_json::Value,
    ) -> Self {
        Self {
            name: name.to_string(),
            description: description.to_string(),
            parameters: prompt::neutralize_json_value(&parameters)
                .expect("neutralizing reserved tags keeps the schema valid JSON"),
        }
    }

    /// 外部サーバーが書いた定義。説明と引数スキーマの予約タグを無害化する。スキーマを
    /// 無害化した形で読み直せなければ`None`を返し、そのツールは公開しない。
    pub fn external(
        name: String,
        description: &str,
        parameters: &serde_json::Value,
    ) -> Option<Self> {
        Some(Self {
            name,
            description: PromptText::untrusted(description).as_str().to_string(),
            parameters: prompt::neutralize_json_value(parameters)?,
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn description(&self) -> &str {
        &self.description
    }

    pub fn parameters(&self) -> &serde_json::Value {
        &self.parameters
    }
}

/// アダプタが構成不足で呼び出しに進めない状態(Issue #40)。プロバイダの選択有無など、
/// アダプタ自体が無い場合は`orchestration::TurnContext::adapter`の`Err`で表すため
/// ここには含めない(`orchestration::turn_error::from_readiness`参照)。
///
/// APIキーの空・未設定はここに含めない。ローカルプロバイダーは認証不要で意図的に
/// 空のままにする場合があり、空文字列だけでは「未設定で使えない」のか「設定不要」なのかを
/// 区別できない(`providers::build_active_adapter`のドキュメント参照: 資格情報ストアが
/// 使えない場合も鍵無し扱いで起動を続け、実際のAPI呼び出し時にプロバイダー側の認証エラー
/// として表面化させる設計)。実際に鍵が必要なら呼び出しが401/403を返し、
/// `turn_error::classify`が`Auth`として分類する。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Readiness {
    Ready,
    NoModel,
}

/// 具象プロバイダの境界。`orchestration::turn`はこのtraitのみを知り、
/// プロバイダ固有の癖は各実装内に閉じ込める(architecture.md 3節)。
#[async_trait::async_trait]
pub trait LlmAdapter: Send + Sync {
    /// 実際にAPIを呼ぶ前に分かる構成不足(モデル未選択)を判定する。APIキーは判定しない
    /// (理由は[`Readiness`]のドキュメント)。判定材料はプロバイダごとに異なるため各実装に委ねる。
    fn readiness(&self) -> Readiness;

    /// `reasoning_effort`は、思考に対応するモデルでは常に`Some`、対応しないモデルでは
    /// `None`(指定そのものを拒むAPIがあるため、強さを指定しない)。どちらにするかは
    /// 呼び出し元が能力から決める。
    ///
    /// 応答のイベントは、生成した順に1件ずつ`on_event`へ渡す。ストリーミングするかどうかは
    /// アダプタの都合で、呼び出し側は区別しない(architecture.md 3節)。渡したイベントは
    /// そのまま画面へ流れるため、次を守る。
    ///
    /// - 失敗は`Err`で返し、イベントにしない(ストリームの途中で届くエラーも同じ)。
    ///   エラー本文を本文のイベントに載せると、`orchestration::turn_error`の伏せ字と長さの
    ///   上限を通らずに画面へ届く
    /// - `Err`を返したら、それまでに渡したイベントは無効とする(呼び出し側は保存しない)
    /// - 一度イベントを渡したら、この呼び出しの中でリクエストをやり直さない。画面に二重に
    ///   出るため。やり直すなら最初のイベントを渡す前に限る
    /// - `Done`は成功したときに最後に1回だけ渡す(画面はこれをラウンドの区切りに使う)
    async fn send(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolSchema],
        reasoning_effort: Option<ReasoningEffort>,
        on_event: &mut (dyn FnMut(ResponseEvent) + Send),
    ) -> Result<(), CoreError>;

    /// [`Self::send`]が同じ引数で送るリクエストの本文を、送らずに返す(送信内容のプレビュー。
    /// Issue #23)。実際のプロバイダーは必ず実装し、`send`と同じ組み立てを通す。既定の`None`は
    /// テスト用のアダプタのためのもの。
    ///
    /// 変えてよいのは画像の実体だけで、形式と長さに縮める(1枚で数MBになり、端末では読めない)。
    /// 縮めるのは方言の上で画像を置く位置に限る。他の文字列まで縮めると、モデルに渡る文を
    /// プレビューから隠せてしまう。要求URLは返さない(エラーの詳細と同じく、パスに鍵を置く
    /// ゲートウェイがあるため)。
    fn request_preview(
        &self,
        _messages: &[ChatMessage],
        _tools: &[ToolSchema],
        _reasoning_effort: Option<ReasoningEffort>,
    ) -> Option<RequestPreview> {
        None
    }
}

/// 送らずに組み立てたリクエストの本文。認証情報(ヘッダー)は持たない。本文はアダプタの
/// リクエストの型から直列化したもので、秘密情報(`secrecy::SecretString`)は直列化できないため
/// 型の上で本文に乗らない。
#[derive(Debug, Serialize)]
pub struct RequestPreview {
    pub body: serde_json::Value,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_malformed_arguments_as_the_raw_string() {
        let args = ToolArguments::parse("{\"title\": ".to_string());
        assert!(matches!(&args, ToolArguments::Malformed { raw, .. } if raw == "{\"title\": "));
        assert_eq!(args.to_wire_string(), "{\"title\": ");
    }

    #[test]
    fn parses_valid_arguments() {
        let args = ToolArguments::parse("{\"title\":\"a\"}".to_string());
        assert_eq!(
            args,
            ToolArguments::from(serde_json::json!({ "title": "a" }))
        );
        assert_eq!(args.to_wire_string(), "{\"title\":\"a\"}");
    }
}
