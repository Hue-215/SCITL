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

/// 具象プロバイダの境界。`orchestration::turn`はこのtraitのみを知り、
/// プロバイダ固有の癖は各実装内に閉じ込める(architecture.md 3節)。
#[async_trait::async_trait]
pub trait LlmAdapter: Send + Sync {
    async fn send(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolSchema],
    ) -> Result<Vec<ResponseEvent>, CoreError>;
}
