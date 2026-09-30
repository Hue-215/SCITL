//! 会話履歴の間引き。どこまで送るかの判断はここに閉じる。

use crate::llm::{estimate_message, estimate_tools, ChatMessage, ToolSchema};

/// コンテキスト長のうち、応答(思考を含む)のために空けておく割合の逆数。出力の上限を
/// リクエストで指定していないため、入力が長いほど応答に使える長さが削られる。
const RESPONSE_SHARE_DIVISOR: usize = 4;

/// `history`のうち、コンテキスト長に収まる末尾を返す。`others`(システムプロンプト)と
/// `tools`は間引かずに必ず送るので、先に予算から引く。
///
/// 間引く単位は、ユーザー発言から次のユーザー発言の手前まで。ツールの往復
/// (`tool_calls`を持つassistantと続くtool)を途中で切らず、間引いた履歴はユーザー発言から
/// 始まるので、アダプタが先頭にユーザー発言を補うことも無い。
///
/// 最後の単位(このターンのユーザー発言)は、収まらなくても残す。見積もりは多めなので
/// 実際には収まることがあり、収まらなければコンテキスト超過のエラー発言が上限の設定を促す。
/// ここで送らずに止めると、その両方の道を塞ぐ。
pub(super) fn trim_history<'a, 'b>(
    history: &'a [ChatMessage],
    context_length: u32,
    others: impl IntoIterator<Item = &'b ChatMessage>,
    tools: &[ToolSchema],
) -> &'a [ChatMessage] {
    let context_length = context_length as usize;
    let budget = (context_length - context_length / RESPONSE_SHARE_DIVISOR)
        .saturating_sub(others.into_iter().map(estimate_message).sum())
        .saturating_sub(estimate_tools(tools));

    let mut used = 0;
    let mut keep_from = history.len();
    for (i, message) in history.iter().enumerate().rev() {
        used += estimate_message(message);
        // 先頭は、ユーザー発言でなくても単位の始まりとして扱う(全体が収まるなら全部送る)。
        let starts_unit = i == 0 || matches!(message, ChatMessage::User { .. });
        if !starts_unit {
            continue;
        }
        if used > budget && keep_from < history.len() {
            break;
        }
        keep_from = i;
    }
    &history[keep_from..]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::{PromptText, ToolCallRequest};

    fn user(text: &str) -> ChatMessage {
        ChatMessage::user(PromptText::user_message(text, None))
    }

    fn assistant(text: &str) -> ChatMessage {
        ChatMessage::Assistant {
            content: Some(text.to_string()),
            tool_calls: Vec::new(),
            replay: Default::default(),
        }
    }

    fn tool_round_trip() -> [ChatMessage; 2] {
        [
            ChatMessage::Assistant {
                content: None,
                tool_calls: vec![ToolCallRequest {
                    id: Some("call_1".to_string()),
                    name: "search".to_string(),
                    arguments: serde_json::json!({}).into(),
                }],
                replay: Default::default(),
            },
            ChatMessage::Tool {
                tool_call_id: Some("call_1".to_string()),
                content: PromptText::untrusted("result"),
                images: Vec::new(),
            },
        ]
    }

    /// `history[from..]`がちょうど収まるコンテキスト長。
    fn context_length_fitting(history: &[ChatMessage], from: usize) -> u32 {
        let needed: usize = history[from..].iter().map(estimate_message).sum();
        // 応答の分を空けた残りが`needed`以上になる最小の長さ。
        let length = (needed * RESPONSE_SHARE_DIVISOR).div_ceil(RESPONSE_SHARE_DIVISOR - 1);
        u32::try_from(length).unwrap()
    }

    #[test]
    fn keeps_everything_when_it_fits() {
        let history = vec![assistant("a0"), user("u1"), assistant("a1"), user("u2")];
        let length = context_length_fitting(&history, 0);
        assert_eq!(trim_history(&history, length, &[], &[]), &history[..]);
    }

    #[test]
    fn drops_the_oldest_units_and_starts_from_a_user_message() {
        let [call, result] = tool_round_trip();
        let history = vec![
            user("u1"),
            call,
            result,
            assistant("a1"),
            user("u2"),
            assistant("a2"),
            user("u3"),
        ];
        // 途中のassistantから始めれば収まる長さでも、ユーザー発言の境目まで落とす。
        let length = context_length_fitting(&history, 3);
        assert_eq!(trim_history(&history, length, &[], &[]), &history[4..]);
    }

    #[test]
    fn subtracts_what_is_always_sent_from_the_budget() {
        let history = vec![user("u1"), assistant("a1"), user("u2")];
        let length = context_length_fitting(&history, 0);
        let system = [ChatMessage::System("system prompt".to_string())];
        assert_eq!(trim_history(&history, length, &system, &[]), &history[2..]);
    }

    #[test]
    fn keeps_the_latest_user_message_even_when_it_does_not_fit() {
        let history = vec![user("u1"), assistant("a1"), user("u2")];
        assert_eq!(trim_history(&history, 1, &[], &[]), &history[2..]);
    }
}
