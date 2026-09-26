//! ツール実行記録(`kind='tool_execution'`の行)の`content`の形。書き込み(`turn`)と
//! 読み戻し(`history`)で同じ形を使うため、ここに1つだけ置く(principles.md 5節)。

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::tools::ToolKind;

#[derive(Debug, Serialize, Deserialize)]
pub(super) struct ToolExecutionRecord {
    pub tool: String,
    /// 読めなかった引数は、モデルが実際に何を出したかが分かるよう生の文字列で残す。
    /// この形は「正しく読めた引数が文字列だった」場合と見分けられないが、読めなかった
    /// 呼び出しは実行しないので`tool_kind`を持たない。
    pub arguments: Value,
    pub result: Value,
    /// 実行したときにツール定義が決めた分類(tools.md 4節)。実行しなかった呼び出し
    /// (引数が読めない・公開していない名前・接続先が無い)と、Issue #11より前の記録には無い。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_kind: Option<ToolKind>,
    /// プロバイダーが払い出した呼び出しID。記録のためだけに持ち、次ターン以降の履歴には
    /// 使わない(`history::history_call_id`)。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub call_id: Option<String>,
}

/// ツール結果が失敗を表すか。最上位の`error`キーを作るのはSCITL自身だけ
/// (実行の失敗と`mcp::to_result_value`。サーバーの構造化出力は`structured_content`の下に入る)。
/// 画面の`isErrorResult`(frontend/src/thinking.ts)と同じ基準。
pub(super) fn is_error_result(result: &Value) -> bool {
    result.get("error").is_some()
}
