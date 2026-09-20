pub mod providers;

use serde::{Deserialize, Serialize};

use crate::db::error::CoreError;

/// アダプタ層が上位に返す形は完成した応答1つではなくイベントの並び
/// (docs/spec/principles.md 3節「応答はイベントの並びとして受け取る」、Issue #8)。
/// ストリーミングしないプロバイダーも各イベントを1回ずつ返せば同じ経路に乗る。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
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
        arguments: serde_json::Value,
    },
    Done {
        finish_reason: FinishReason,
    },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FinishReason {
    Stop,
    ToolCall,
    /// 出力が長さ制限で打ち切られた。Stopに潰すと打ち切りを上位層が検知できない。
    Length,
    Error,
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
#[derive(Debug, Clone, Serialize, PartialEq)]
pub enum ChatMessage {
    System(String),
    User(String),
    Assistant {
        content: Option<String>,
        tool_calls: Vec<ToolCallRequest>,
    },
    Tool {
        /// プロバイダが払い出した呼び出しIDをそのまま返す。捏造しない
        /// (払い出さないプロバイダにはこのフィールド自体を送らない。architecture.md 3節)。
        tool_call_id: Option<String>,
        content: String,
    },
}

/// モデルが出したツール呼び出し1件。往復のために`Assistant`側で保持する。
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ToolCallRequest {
    pub id: Option<String>,
    pub name: String,
    pub arguments: serde_json::Value,
}

#[derive(Debug, Clone)]
pub struct ToolSchema {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

/// アダプタが構成不足で呼び出しに進めない状態(Issue #40)。プロバイダの選択有無は
/// `Option<&dyn LlmAdapter>`の`None`で表すためここには含めない
/// (`orchestration::turn_error::from_readiness`参照)。
///
/// APIキーの空・未設定はここに含めない。ローカルプロバイダーは認証不要で意図的に
/// 空のままにする場合があり、空文字列だけでは「未設定で使えない」のか「設定不要」なのかを
/// 区別できない(`main.rs::build_active_adapter`のドキュメント参照: 資格情報ストアが
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
    /// モデル未選択・APIキー未設定を、実際にAPIを呼ぶ前に判定する。プロバイダごとに
    /// 判定材料(保持しているモデル名・鍵)が異なるため各実装に委ねる。
    fn readiness(&self) -> Readiness;

    async fn send(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolSchema],
    ) -> Result<Vec<ResponseEvent>, CoreError>;
}
