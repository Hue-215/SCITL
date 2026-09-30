//! 1ターンの各ラウンドでモデルへ送る発言列とツールの組み立て。ターン([`super::turn`])と
//! 送信内容のプレビュー([`super::preview`])が同じ組み立てを通る。
//!
//! ラウンドをまたいで、前に送った部分は変えない。後ろにこのターンの往復を足すだけにする
//! (`docs/spec/rebuild/architecture.md`「前に送った部分を変えない」)。

use crate::blocking;
use crate::db::messages::Chat;
use crate::db::{with_conn, SharedConnection};
use crate::error::Result;
use crate::llm::{ChatMessage, PromptText, ToolOffer, ToolSchema};
use crate::orchestration::history::{self, HistoryOptions, StoredChat};
use crate::orchestration::history_trim::trim_history;
use crate::orchestration::state_prompt::{build_state, build_system_prompt};
use crate::orchestration::TurnContext;
use crate::tools::{self, external::ExternalToolset};

/// ツールの上限に達したあとの最後の呼び出しで、発言列の末尾に足す一節。
/// ツールを呼べない理由を伝えないと、モデルがツールを呼ぶつもりの文を返しがちになる。
const ROUND_LIMIT_NOTE: &str = "The tool call limit for this turn has been reached, so no more \
     tools can be called. Reply to the user based on the tool results so far.";

/// 1ターンのうち、ラウンドによらない送信の材料。
pub(super) struct TurnRequest {
    /// システムプロンプト、間引いた履歴、最新状態を添えた直近のユーザー発言。ターンの間は
    /// 変えない。
    opening: Vec<ChatMessage>,
    exposed_tools: Vec<ToolSchema>,
    tools_available: bool,
}

impl TurnRequest {
    /// `stored`は、送る対象のユーザー発言の挿入・カスケード削除を済ませたあとの行。
    ///
    /// 最新状態と履歴の間引きはここで1回だけ決める。このターンの往復の分は、間引きが応答の
    /// ために空けておく分から使う。往復が伸びて収まらなくなっても間引き直さない(前に送った
    /// 部分が変わる)ので、そのときはプロバイダーのコンテキスト超過のエラーでターンが終わる。
    pub(super) async fn prepare(
        db: SharedConnection,
        ctx: &TurnContext<'_>,
        chat: Chat,
        stored: StoredChat,
        external: &ExternalToolset,
    ) -> Result<Self> {
        // 内部ツールと外部ツールを1つの一覧にして公開する。名前空間化と衝突の排除は
        // `ExternalToolset`が済ませてある。ツールに対応しないモデルには何も渡さない
        // (対応しないモデルにツールを渡すと、リクエストごと拒否するサーバーがある)。
        let tools_available = ctx.capabilities.tools;
        let options = HistoryOptions {
            tools_available,
            image_input: ctx.capabilities.image,
            opening: ctx.opening_message.to_string(),
        };
        // 添付画像の読み出しはファイルI/Oなので、DBのロックの外でブロッキング処理として行う。
        let store = ctx.attachments.store();
        let history =
            blocking::run(move || Ok(history::build_history(stored, &options, &store))).await?;
        let mut exposed_tools = Vec::new();
        if tools_available {
            exposed_tools.extend(tools::schemas(chat));
            exposed_tools.extend(external.schemas());
        }
        let system = ChatMessage::System(build_system_prompt(chat, &ctx.prompts, tools_available));
        let state = with_conn(db, move |conn| build_state(conn, chat)).await?;

        // 最新状態は直近のユーザー発言に添えるが、見積もりでは別の発言として数える。
        let state_estimate = ChatMessage::user(state.clone());
        let kept = trim_history(
            &history,
            ctx.capabilities.context_length,
            [&system, &state_estimate],
            &exposed_tools,
        );
        let mut opening = Vec::with_capacity(1 + kept.len());
        opening.push(system);
        opening.extend(kept.iter().cloned());
        attach_state(&mut opening, &state);

        Ok(Self {
            opening,
            exposed_tools,
            tools_available,
        })
    }

    pub(super) fn tools_available(&self) -> bool {
        self.tools_available
    }

    /// ツールを渡すラウンドの数。上限のラウンドまでツールを実行したら、ツールを呼べないように
    /// してもう一度だけ呼ぶ(ツールに対応しないモデルは最初の呼び出しがそれにあたる)。`u64`で
    /// 数えるのは、上限が`u32::MAX`でも最後の1回を数えられるようにするため。
    pub(super) fn tool_rounds(&self, ctx: &TurnContext<'_>) -> u64 {
        if self.tools_available {
            u64::from(ctx.limits.max_rounds_per_turn)
        } else {
            0
        }
    }

    /// 1ラウンドで送る発言列と、渡すツール。`round_trip`はこのターンでここまでに行った
    /// ツール呼び出しの往復、`final_call`はツールを呼べない最後の呼び出しか。
    ///
    /// 最後の呼び出しでもツールの定義は同じものを渡し、呼べないことだけを伝える(どう禁じるかは
    /// アダプタが決める)。
    pub(super) fn round(
        &self,
        round_trip: &[ChatMessage],
        final_call: bool,
    ) -> (Vec<ChatMessage>, ToolOffer<'_>) {
        let mut messages = Vec::with_capacity(self.opening.len() + round_trip.len() + 1);
        messages.extend(self.opening.iter().cloned());
        messages.extend(round_trip.iter().cloned());
        if final_call && self.tools_available {
            messages.push(ChatMessage::user(PromptText::note(ROUND_LIMIT_NOTE)));
        }
        let offer = ToolOffer {
            schemas: &self.exposed_tools,
            callable: !final_call,
        };
        (messages, offer)
    }
}

/// 最新状態を直近のユーザー発言の後ろに添える。システムプロンプトに置かないのは、ターンごとに
/// 変わるものを発言列の先頭に置くと、先頭一致のプロンプトキャッシュがそこで切れるため。会話の途中に
/// システム発言として差し込まないのは、チャットテンプレートでそれを拒むローカルの推論サーバーが
/// あるため。ユーザー発言が無い発言列(応答すべき発言が無い)は送らない
/// (`history::awaits_reply`)が、あれば最新状態だけのユーザー発言として足す。
fn attach_state(messages: &mut Vec<ChatMessage>, state: &PromptText) {
    match messages.iter_mut().rev().find_map(|m| match m {
        ChatMessage::User { text, .. } => Some(text),
        _ => None,
    }) {
        Some(text) => *text = text.followed_by(state),
        None => messages.push(ChatMessage::user(state.clone())),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn state() -> PromptText {
        PromptText::state("2026-01-01T00:00:00Z", "current task state", &json!({}))
    }

    fn user_text(message: &ChatMessage) -> &str {
        match message {
            ChatMessage::User { text, .. } => text.as_str(),
            other => panic!("expected User, got {other:?}"),
        }
    }

    #[test]
    fn attaches_the_state_to_the_latest_user_message_only() {
        let first = PromptText::user_message("first", None);
        let latest = PromptText::user_message("latest", None);
        let mut messages = vec![
            ChatMessage::System("system".to_string()),
            ChatMessage::user(first.clone()),
            ChatMessage::Assistant {
                content: Some("reply".to_string()),
                tool_calls: Vec::new(),
                replay: Default::default(),
            },
            ChatMessage::user(latest.clone()),
        ];

        attach_state(&mut messages, &state());

        assert_eq!(messages.len(), 4);
        assert_eq!(user_text(&messages[1]), first.as_str());
        assert_eq!(
            user_text(&messages[3]),
            latest.followed_by(&state()).as_str()
        );
    }

    #[test]
    fn sends_the_state_alone_when_there_is_no_user_message() {
        let mut messages = vec![ChatMessage::System("system".to_string())];

        attach_state(&mut messages, &state());

        assert_eq!(messages.len(), 2);
        assert_eq!(user_text(&messages[1]), state().as_str());
    }
}
