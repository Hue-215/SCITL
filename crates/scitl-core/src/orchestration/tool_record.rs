//! ツール実行記録(`kind='tool_execution'`の行)の`content`の形。書き込み(`turn`・
//! `operations`)と読み戻し(`history`)、実行を画面へ知らせるイベント(`turn_event`)で
//! 同じ形を使うため、ここに1つだけ置く。

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// フィールドをcoreの外に開かないのは、記録を作るのがcoreの中(ターンの処理と、応答生成以外の
/// 経路での操作)だけだから(外へは直列化した形で渡るだけ)。操作の記録は`call_id`を持たない。
///
/// 古い記録には今は使わないキー(`tool_kind`)が残っているので、知らないキーは拒まずに無視する。
#[derive(Debug, Serialize, Deserialize)]
pub struct ToolExecutionRecord {
    pub(crate) tool: String,
    /// 読めなかった引数は、モデルが実際に何を出したかが分かるよう生の文字列で残す。
    /// この形は「正しく読めた引数が文字列だった」場合と見分けられないが、読めなかった
    /// 呼び出しは実行せず、結果は必ず失敗になる([`is_error_result`])。
    pub(crate) arguments: Value,
    pub(crate) result: Value,
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
/// 閉じた状態で1行に出す引数の要約と失敗の文言も、同じ規則で作って渡す。
/// どの文字が見えないかの判定と、どこで切り詰めるかを画面に写さないため、ここで作る。
#[derive(Debug, Clone, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
pub struct ToolExecutionView {
    /// 呼び出したツールの名前。記録から読めなければ`None`。
    tool: Option<String>,
    arguments: String,
    result: String,
    is_error: bool,
    /// 引数の1行の要約(`キー: 値`を並べたもの。値はJSONの形)。長い値と全体は省略する。
    /// 引数が無ければ空。
    summary: String,
    /// 失敗の文言の1行(結果の`error`の値)。失敗でなければ`None`。
    error: Option<String>,
}

/// 要約の1つの値と、要約全体の長さの上限(文字数)。
const SUMMARY_VALUE_CHARS: usize = 40;
const SUMMARY_CHARS: usize = 120;

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
            summary: summary(arguments),
            error: result.get("error").map(|error| match error {
                Value::String(text) => one_line(text, SUMMARY_CHARS),
                other => one_line(&other.to_string(), SUMMARY_CHARS),
            }),
        }
    }
}

/// 引数の要約。オブジェクトはキーごとに`キー: 値`(値は詰めたJSON)、それ以外(読めなかった
/// 引数の生の文字列等)は値そのものを詰めたJSONで出す。
fn summary(arguments: &Value) -> String {
    let line = match arguments {
        Value::Object(fields) => fields
            .iter()
            .map(|(key, value)| {
                let value = crate::text::ellipsize(&value.to_string(), SUMMARY_VALUE_CHARS);
                format!("{key}: {value}")
            })
            .collect::<Vec<_>>()
            .join(", "),
        other => other.to_string(),
    };
    one_line(&line, SUMMARY_CHARS)
}

/// 見えない文字を見える形にしたうえで1行に畳み、`max`文字で省略する。
fn one_line(s: &str, max: usize) -> String {
    let revealed = crate::text::reveal_invisible(s);
    crate::text::ellipsize(&crate::text::collapse_whitespace(&revealed), max)
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
        assert_eq!(view.summary, "title: \"a\\u202Eb\"");
        assert_eq!(view.error.as_deref(), Some("x\\u200By"));
    }

    #[test]
    fn summary_lists_arguments_on_one_line_and_shortens_long_values() {
        let long = "あ".repeat(60);
        let content = json!({
            "tool": "add_steps",
            "arguments": { "descriptions": ["買い出し", "支払い"], "note": long },
            "result": { "ok": true },
        })
        .to_string();
        let view = ToolExecutionView::of_content(&content);
        assert!(view
            .summary
            .starts_with("descriptions: [\"買い出し\",\"支払い\"], note: \"あ"));
        assert!(view.summary.ends_with('…'));
        assert!(view.summary.chars().count() <= SUMMARY_CHARS + 1);
        assert_eq!(view.error, None);
    }

    #[test]
    fn a_multiline_error_is_folded_to_one_line() {
        let content = json!({
            "tool": "web__search",
            "arguments": "{\"q\": ",
            "result": { "error": "first\n\nsecond" },
        })
        .to_string();
        let view = ToolExecutionView::of_content(&content);
        assert_eq!(view.summary, "\"{\\\"q\\\": \"");
        assert_eq!(view.error.as_deref(), Some("first second"));
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
