//! ツール実行記録(`kind='tool_execution'`の行)の`content`の形。書き込み(`turn`・
//! `operations`)と読み戻し(`history`)、実行を画面へ知らせるイベント(`turn_event`)で
//! 同じ形を使うため、ここに1つだけ置く。

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::tools::ToolKind;

/// フィールドをcoreの外に開かないのは、記録を作るのがcoreの中(ターンの処理と、応答生成以外の
/// 経路での操作)だけだから(外へは直列化した形で渡るだけ)。操作の記録は`tool_kind`・
/// `call_id`を持たない。
#[derive(Debug, Serialize, Deserialize)]
pub struct ToolExecutionRecord {
    pub(crate) tool: String,
    /// 読めなかった引数は、モデルが実際に何を出したかが分かるよう生の文字列で残す。
    /// この形は「正しく読めた引数が文字列だった」場合と見分けられないが、読めなかった
    /// 呼び出しは実行しないので`tool_kind`を持たない。
    pub(crate) arguments: Value,
    pub(crate) result: Value,
    /// 実行したときにツール定義が決めた分類。実行しなかった呼び出し
    /// (引数が読めない・公開していない名前・接続先が無い)と、Issue #11より前の記録には無い。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) tool_kind: Option<ToolKind>,
    /// プロバイダーが払い出した呼び出しID。記録のためだけに持ち、次ターン以降の履歴には
    /// 使わない(`history::history_call_id`)。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) call_id: Option<String>,
}

/// ツール結果が失敗を表すか。最上位の`error`キーを作るのはSCITL自身だけ
/// (実行の失敗と`mcp::to_result_value`。サーバーの構造化出力は`structured_content`の下に入る)。
/// 画面の失敗の印([`ToolExecutionView::is_error`])も同じ基準。
pub(super) fn is_error_result(result: &Value) -> bool {
    result.get("error").is_some()
}

/// 画面に出すツール実行記録。記録はモデルや外部ツールが何を出したかをそのまま確かめるための
/// 表示なので、引数と結果は整形したJSONのまま、見えない文字だけを見える形にして渡す。
/// どの文字が見えないかの判定を画面に写さないため、ここで作る。
#[derive(Debug, Clone, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
pub struct ToolExecutionView {
    /// 呼び出したツールの名前。記録から読めなければ`None`。
    tool: Option<String>,
    arguments: String,
    result: String,
    is_error: bool,
}

impl ToolExecutionView {
    pub(super) fn of_record(record: &ToolExecutionRecord) -> Self {
        Self::new(Some(&record.tool), &record.arguments, &record.result)
    }

    /// 保存済みの行の`content`から作る。行は読めた形を保証しないので、読めない項目は
    /// 欠けたものとして扱う(引数は空のオブジェクト、結果は`null`)。
    pub(super) fn of_content(content: &str) -> Self {
        let value: Value = serde_json::from_str(content).unwrap_or(Value::Null);
        let arguments = value
            .get("arguments")
            .cloned()
            .unwrap_or_else(|| Value::Object(Default::default()));
        let result = value.get("result").cloned().unwrap_or(Value::Null);
        Self::new(
            value.get("tool").and_then(Value::as_str),
            &arguments,
            &result,
        )
    }

    fn new(tool: Option<&str>, arguments: &Value, result: &Value) -> Self {
        Self {
            tool: tool.map(str::to_string),
            arguments: pretty(arguments),
            result: pretty(result),
            is_error: is_error_result(result),
        }
    }
}

fn pretty(value: &Value) -> String {
    let json = serde_json::to_string_pretty(value).expect("a JSON value serializes");
    crate::text::reveal_invisible(&json)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn view_reveals_invisible_characters_in_arguments_and_results() {
        let content = json!({
            "tool": "add_step",
            "arguments": { "title": "a\u{202E}b" },
            "result": { "error": "x\u{200B}y" },
        })
        .to_string();
        let view = ToolExecutionView::of_content(&content);
        assert_eq!(view.tool.as_deref(), Some("add_step"));
        assert_eq!(view.arguments, "{\n  \"title\": \"a\\u202Eb\"\n}");
        assert_eq!(view.result, "{\n  \"error\": \"x\\u200By\"\n}");
        assert!(view.is_error);
    }

    #[test]
    fn unreadable_content_is_shown_as_missing_fields() {
        let view = ToolExecutionView::of_content("not json");
        assert_eq!(view.tool, None);
        assert_eq!(view.arguments, "{}");
        assert_eq!(view.result, "null");
        assert!(!view.is_error);
    }
}
