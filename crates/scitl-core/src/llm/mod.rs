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

/// アダプタ層が上位に渡す形は完成した応答1つではなくイベントの並び。ストリーミングしない
/// プロバイダーも各イベントを1回ずつ渡せば同じ経路に乗る([`LlmAdapter::send`])。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ResponseEvent {
    TextDelta {
        text: String,
    },
    /// モデルの思考(reasoning)の断片。表示・`messages.reasoning`への保存専用の
    /// イベントであり、`ChatMessage`には対応する構成要素が無い。次のAPI呼び出しの入力に
    /// 混ざり込む経路が型として存在しないようにするための意図的な非対称設計。
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

/// アダプタに渡す発言列の1要素。「userなのに`tool_call_id`を持つ」といった不正な組み合わせを
/// 型で防ぐため、平坦な構造ではなく列挙型にする。
///
/// `Assistant`の`tool_calls`と`Tool`は、同一ターン内のツールの往復だけに使う。DBには保存せず、
/// 次ターン以降の履歴にも載せない(過去のツール実行は`orchestration::history`が実行記録から
/// 組み立てる)。
#[derive(Debug, Clone, PartialEq)]
pub enum ChatMessage {
    System(String),
    /// ユーザー発言。送信日時と添付の情報を本文の外に置いた囲みとして、組み立て済みの形で運ぶ
    /// ([`PromptText::user_message`])。`images`は一緒に送る添付画像で、どの画像を送るかは
    /// `attachments::delivery`が決める。
    User {
        text: PromptText,
        images: Vec<InlineImage>,
    },
    Assistant {
        content: Option<String>,
        tool_calls: Vec<ToolCallRequest>,
        /// 同一ターン内の往復でだけ持つ([`Replay`])。履歴から組み立てた発言では空。
        replay: Replay,
    },
    Tool {
        /// プロバイダが払い出した呼び出しIDをそのまま返す。捏造せず、払い出さないプロバイダには
        /// このフィールド自体を送らない。
        tool_call_id: Option<String>,
        content: PromptText,
        /// 結果として返す画像(添付の読み込み)。ツール結果に画像を載せられないAPIがあるため、
        /// リクエストでどう表すかはアダプタが決める。
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

/// プロバイダーが、同じターンの次の呼び出しで受け取ったまま送り返すよう求める応答の一部
/// (Anthropic形式の署名付きの思考ブロック等)。中身を読み書きするのは、それを返したアダプタ
/// だけで、中核は同じターンの往復のアシスタント発言に載せて運ぶだけにする。表示も保存もしない
/// (`docs/spec/principles.md`「思考は履歴に送り返さない」)。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Replay(Option<serde_json::Value>);

impl Replay {
    pub(in crate::llm) fn new(value: serde_json::Value) -> Self {
        Self(Some(value))
    }

    pub(in crate::llm) fn get(&self) -> Option<&serde_json::Value> {
        self.0.as_ref()
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

    /// MIMEと、base64にした本体(data URLを使わずに、別々の欄で渡す方言向け)。
    pub fn media_type_and_data(&self) -> (&str, &str) {
        self.data_url
            .strip_prefix("data:")
            .and_then(|rest| rest.split_once(";base64,"))
            .expect("InlineImage::from_bytes builds a base64 data URL")
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
/// まま上位へ運ぶ(`orchestration::turn`が実行せずに失敗をモデルへ返す)。ターンごと止めると
/// 性能の低いモデルでは会話が続かず、空オブジェクト等に置き換えると引数を伴うツールを
/// 引数無しで発火させるため。
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
/// 無害化が掛かっているかが決まる。
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

/// 1回の呼び出しでモデルに渡すツールと、この呼び出しでツールを呼べるか。
///
/// `callable`が`false`のとき、呼び出しをどう禁じるか(定義を外すか、定義を渡したまま禁止の
/// 指定を送るか)は方言ごとに各アダプタが決める。
#[derive(Debug, Clone, Copy)]
pub struct ToolOffer<'a> {
    pub schemas: &'a [ToolSchema],
    pub callable: bool,
}

impl ToolOffer<'_> {
    /// ツールを渡さない呼び出し。
    pub const NONE: ToolOffer<'static> = ToolOffer {
        schemas: &[],
        callable: false,
    };
}

/// アダプタが構成不足で呼び出しに進めない状態。アダプタ自体が無い場合(プロバイダー未選択等)は
/// `orchestration::TurnContext::adapter`の`Err`で表す。
///
/// APIキーの空・未設定はここに含めない。認証不要のローカルプロバイダーと区別できないため、
/// 鍵が要るなら呼び出しが401/403を返し、`turn_error::classify`が`Auth`として分類する。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Readiness {
    Ready,
    NoModel,
}

/// 具象プロバイダの境界。`orchestration::turn`はこのtraitのみを知り、プロバイダ固有の癖は
/// 各実装内に閉じ込める。
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
    /// アダプタの都合で、呼び出し側は区別しない。渡したイベントはそのまま画面へ流れるため、
    /// 次を守る。
    ///
    /// - 失敗は`Err`で返し、イベントにしない(ストリームの途中で届くエラーも同じ)。
    ///   エラー本文を本文のイベントに載せると、`orchestration::turn_error`の伏せ字と長さの
    ///   上限を通らずに画面へ届く
    /// - `Err`を返したら、それまでに渡したイベントは無効とする(呼び出し側は保存しない)
    /// - 一度イベントを渡したら、この呼び出しの中でリクエストをやり直さない。画面に二重に
    ///   出るため。やり直すなら最初のイベントを渡す前に限る
    /// - `Done`は成功したときに最後に1回だけ渡す(画面はこれをラウンドの区切りに使う)
    ///
    /// 応答のうち、同じターンの次の呼び出しで送り返しが要るものは[`Replay`]で返す。
    async fn send(
        &self,
        messages: &[ChatMessage],
        tools: ToolOffer<'_>,
        reasoning_effort: Option<ReasoningEffort>,
        on_event: &mut (dyn FnMut(ResponseEvent) + Send),
    ) -> Result<Replay, CoreError>;

    /// [`Self::send`]が同じ引数で送るリクエストの本文を、送らずに返す(送信内容のプレビュー)。
    /// 実際のプロバイダーは必ず実装し、`send`と同じ組み立てを通す。既定の`None`はテスト用の
    /// アダプタのためのもの。
    ///
    /// 変えてよいのは画像の実体だけで、形式と長さに縮める(1枚で数MBになり、端末では読めない)。
    /// 縮めるのは方言の上で画像を置く位置に限る。他の文字列まで縮めると、モデルに渡る文を
    /// プレビューから隠せてしまう。要求URLは返さない(エラーの詳細と同じく、パスに鍵を置く
    /// ゲートウェイがあるため)。
    fn request_preview(
        &self,
        _messages: &[ChatMessage],
        _tools: ToolOffer<'_>,
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
