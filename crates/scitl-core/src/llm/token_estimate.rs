//! トークン数の見積もり(architecture.md 2節)。プロバイダーのトークナイザは使わず、
//! 文字数から多めに見積もる。多めに倒す理由は[`super::FALLBACK_CONTEXT_LENGTH`]と同じ。

use super::{ChatMessage, ToolSchema};

/// ASCII文字を何文字で1トークンと数えるか。英文は4文字前後で1トークンになるトークナイザが
/// 多いが、JSON・数字・記号はそれより細かく割れる。履歴には最新状態のJSONやツールの
/// 引数・結果が載るため、細かく割れる側に合わせる。
const ASCII_CHARS_PER_TOKEN: usize = 3;

/// 1発言ごとに、本文の外で増える分(役割の区切り・テンプレートの記号等)。
const MESSAGE_OVERHEAD: usize = 8;

/// ASCII以外(日本語等)は1文字1トークンと数える。現行のトークナイザの多くは、かな・
/// 漢字を1文字1トークン以下にまとめる。
pub fn estimate_text(text: &str) -> usize {
    let ascii = text.chars().filter(char::is_ascii).count();
    let other = text.chars().count() - ascii;
    ascii.div_ceil(ASCII_CHARS_PER_TOKEN) + other
}

pub fn estimate_message(message: &ChatMessage) -> usize {
    let body = match message {
        ChatMessage::System(text) => estimate_text(text),
        ChatMessage::User(text) => estimate_text(text.as_str()),
        ChatMessage::Assistant {
            content,
            tool_calls,
        } => {
            content.as_deref().map_or(0, estimate_text)
                + tool_calls
                    .iter()
                    .map(|call| {
                        call.id.as_deref().map_or(0, estimate_text)
                            + estimate_text(&call.name)
                            + estimate_text(&call.arguments.to_wire_string())
                    })
                    .sum::<usize>()
        }
        ChatMessage::Tool {
            tool_call_id,
            content,
        } => tool_call_id.as_deref().map_or(0, estimate_text) + estimate_text(content.as_str()),
    };
    MESSAGE_OVERHEAD + body
}

/// ツールの定義もリクエストの一部としてコンテキストを消費する。
pub fn estimate_tools(tools: &[ToolSchema]) -> usize {
    tools
        .iter()
        .map(|tool| {
            MESSAGE_OVERHEAD
                + estimate_text(tool.name())
                + estimate_text(tool.description())
                + estimate_text(&tool.parameters().to_string())
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_ascii_by_groups_and_other_characters_one_by_one() {
        assert_eq!(estimate_text(""), 0);
        assert_eq!(estimate_text("abc"), 1);
        assert_eq!(estimate_text("abcd"), 2);
        assert_eq!(estimate_text("締切"), 2);
        assert_eq!(estimate_text("締切 is"), 2 + 1);
    }

    #[test]
    fn counts_tool_calls_carried_by_an_assistant_message() {
        let plain = ChatMessage::Assistant {
            content: None,
            tool_calls: Vec::new(),
        };
        let with_call = ChatMessage::Assistant {
            content: None,
            tool_calls: vec![super::super::ToolCallRequest {
                id: Some("call_1".to_string()),
                name: "add_steps".to_string(),
                arguments: serde_json::json!({ "descriptions": ["買い出し"] }).into(),
            }],
        };
        assert!(estimate_message(&with_call) > estimate_message(&plain));
    }
}
