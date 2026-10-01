//! 会話履歴の間引き。どこまで送るかの判断はここに閉じる。

use crate::llm::{estimate_message, estimate_tools, ChatMessage, ToolSchema};

/// コンテキスト長のうち、応答(思考を含む)のために空けておく割合の逆数。出力の上限を
/// リクエストで指定していないため、入力が長いほど応答に使える長さが削られる。
const RESPONSE_SHARE_DIVISOR: usize = 4;

/// 間引いた結果。
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Trim {
    /// 並べ始める発言の位置。
    pub(super) keep_from: usize,
    /// `from`より後ろまで落としたか(前が変わったか)。
    pub(super) trimmed: bool,
}

/// `history`のうち、どこから並べるかを決める(`docs/spec/rebuild/architecture.md`
/// 「間引きの位置」)。`from`(保っている間引きの位置)から後ろが予算に収まれば、そこから
/// 並べる。超えたときだけ、予算の半分に収まるまで古い単位から落とす。間引くと前が変わり
/// 思考も外れるので、毎ターン少しずつではなく、まれにまとめて行う。
///
/// `others`(システムプロンプト)と`tools`は間引かずに必ず送るので、先に予算から引く。
/// `starts`は間引く単位の始まりの位置(昇順、`History::unit_starts`)で、`from`もその1つ。
/// 単位はユーザー発言から始まるので、ツールの往復を途中で切らず、アダプタが先頭にユーザー
/// 発言を補うことも無い。
///
/// 最後の単位(このターンのユーザー発言)は、収まらなくても残す。見積もりは多めなので
/// 実際には収まることがあり、収まらなければコンテキスト超過のエラー発言が上限の設定を促す。
/// ここで送らずに止めると、その両方の道を塞ぐ。
pub(super) fn trim_history<'b>(
    history: &[ChatMessage],
    starts: &[usize],
    from: usize,
    context_length: u32,
    others: impl IntoIterator<Item = &'b ChatMessage>,
    tools: &[ToolSchema],
) -> Trim {
    let context_length = context_length as usize;
    let budget = (context_length - context_length / RESPONSE_SHARE_DIVISOR)
        .saturating_sub(others.into_iter().map(estimate_message).sum())
        .saturating_sub(estimate_tools(tools));

    // `sizes[i]`は`history[i..]`の見積もり。
    let mut sizes = vec![0; history.len() + 1];
    for (i, message) in history.iter().enumerate().rev() {
        sizes[i] = sizes[i + 1] + estimate_message(message);
    }
    if sizes[from] <= budget {
        return Trim {
            keep_from: from,
            trimmed: false,
        };
    }
    let target = budget / 2;
    let mut keep_from = starts.last().copied().unwrap_or(from).max(from);
    for &start in starts.iter().rev().filter(|&&s| s >= from) {
        if sizes[start] > target {
            break;
        }
        keep_from = start;
    }
    Trim {
        keep_from,
        trimmed: keep_from > from,
    }
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

    /// ユーザー発言と先頭を単位の始まりとする(`History::unit_starts`の簡易版)。
    fn starts(history: &[ChatMessage]) -> Vec<usize> {
        history
            .iter()
            .enumerate()
            .filter(|(i, m)| *i == 0 || matches!(m, ChatMessage::User { .. }))
            .map(|(i, _)| i)
            .collect()
    }

    /// `history[from..]`がちょうど収まるコンテキスト長。
    fn context_length_fitting(history: &[ChatMessage], from: usize) -> u32 {
        let needed: usize = history[from..].iter().map(estimate_message).sum();
        // 応答の分を空けた残りが`needed`以上になる最小の長さ。
        let length = (needed * RESPONSE_SHARE_DIVISOR).div_ceil(RESPONSE_SHARE_DIVISOR - 1);
        u32::try_from(length).unwrap()
    }

    fn trim(history: &[ChatMessage], from: usize, length: u32) -> Trim {
        trim_history(history, &starts(history), from, length, &[], &[])
    }

    #[test]
    fn keeps_the_position_while_it_fits() {
        let history = vec![assistant("a0"), user("u1"), assistant("a1"), user("u2")];
        let length = context_length_fitting(&history, 0);
        assert_eq!(
            trim(&history, 0, length),
            Trim {
                keep_from: 0,
                trimmed: false
            }
        );
        // 保っている位置より前には戻らない。
        assert_eq!(trim(&history, 1, length).keep_from, 1);
    }

    /// `history[from..]`の見積もり。
    fn size(history: &[ChatMessage], from: usize) -> usize {
        history[from..].iter().map(estimate_message).sum()
    }

    /// 予算が`budget`以上で、3つ違わない値になるコンテキスト長。
    fn length_with_budget(budget: usize) -> u32 {
        u32::try_from(budget.div_ceil(3) * 4).unwrap()
    }

    /// 超えたら、ぎりぎりまでではなく予算の半分まで、ユーザー発言の境目で落とす。
    #[test]
    fn drops_the_oldest_units_down_to_half_the_budget() {
        let [call, result] = tool_round_trip();
        let history = vec![
            user("u1"),
            call,
            result,
            assistant("a1"),
            user("u2"),
            assistant("a2"),
            user("u3"),
            assistant("a3"),
            user("u4"),
        ];
        let starts = starts(&history);
        // 全体をわずかに超える予算。
        let budget = size(&history, 0) - 4;
        let trim = trim_history(&history, &starts, 0, length_with_budget(budget), &[], &[]);
        assert!(trim.trimmed);
        let budget = budget.div_ceil(3) * 3;
        assert!(starts.contains(&trim.keep_from));
        assert!(size(&history, trim.keep_from) <= budget / 2);
        let before = starts[starts.iter().position(|&s| s == trim.keep_from).unwrap() - 1];
        assert!(size(&history, before) > budget / 2);
    }

    #[test]
    fn keeps_an_empty_history_as_it_is() {
        assert_eq!(
            trim_history(&[], &[], 0, 1, &[], &[]),
            Trim {
                keep_from: 0,
                trimmed: false
            }
        );
    }

    #[test]
    fn subtracts_what_is_always_sent_from_the_budget() {
        let history = vec![user("u1"), assistant("a1"), user("u2")];
        let length = context_length_fitting(&history, 0);
        let system = [ChatMessage::System("system prompt".to_string())];
        let trim = trim_history(&history, &starts(&history), 0, length, &system, &[]);
        assert_eq!(trim.keep_from, 2);
    }

    #[test]
    fn keeps_the_latest_user_message_even_when_it_does_not_fit() {
        let history = vec![user("u1"), assistant("a1"), user("u2")];
        assert_eq!(
            trim(&history, 0, 1),
            Trim {
                keep_from: 2,
                trimmed: true
            }
        );
        // 最後の単位だけで超えていても、落とすものが無ければ前は変わらない。
        assert_eq!(
            trim(&history, 2, 1),
            Trim {
                keep_from: 2,
                trimmed: false
            }
        );
    }

    /// 単位の始まりに無い位置(保存から並べた区間の中)では切らない。
    #[test]
    fn cuts_only_at_the_given_unit_starts() {
        let history = vec![
            user(&"u1 ".repeat(200)),
            assistant("a1"),
            user("u2"),
            user("u3"),
            assistant("a3"),
            user("u4"),
        ];
        // 区間2..5の中の3は単位の始まりにしない。3からなら半分に収まるが、2からは収まらない。
        let starts = [0, 2, 5];
        let budget = size(&history, 3) * 2;
        assert!(budget / 2 < size(&history, 2) && budget < size(&history, 0));
        let trim = trim_history(&history, &starts, 0, length_with_budget(budget), &[], &[]);
        assert_eq!(trim.keep_from, 5);
    }
}
