//! 1ターンの各ラウンドでモデルへ送る発言列とツールの組み立て。ターン([`super::turn`])と
//! 送信内容のプレビュー([`super::preview`])が同じ組み立てを通る(principles.md 5節)。

use crate::blocking;
use crate::db::messages::Chat;
use crate::db::{with_conn, SharedConnection};
use crate::error::Result;
use crate::llm::{ChatMessage, PromptText, ToolSchema};
use crate::orchestration::history::{self, HistoryOptions, StoredChat};
use crate::orchestration::history_trim::trim_history;
use crate::orchestration::state_prompt::{build_state, build_system_prompt};
use crate::orchestration::TurnContext;
use crate::tools::{self, external::ExternalToolset};

/// ツールの上限に達したあとの最後の呼び出しで、最新状態の囲みに足す一節。
/// ツールを渡さない理由を伝えないと、モデルがツールを呼ぶつもりの文を返しがちになる。
const ROUND_LIMIT_NOTE: &str = "The tool call limit for this turn has been reached, so no tools \
     are available now. Reply to the user based on the tool results so far.";

/// 1ターンのうち、ラウンドによらない送信の材料。
pub(super) struct TurnRequest {
    chat: Chat,
    history: Vec<ChatMessage>,
    exposed_tools: Vec<ToolSchema>,
    tools_available: bool,
    /// ラウンドによらず同じ。毎回変わるものは[`Self::round`]が最新状態として添える。
    system: ChatMessage,
}

impl TurnRequest {
    /// `stored`は、送る対象のユーザー発言の挿入・カスケード削除を済ませたあとの行。
    pub(super) async fn prepare(
        ctx: &TurnContext<'_>,
        chat: Chat,
        stored: StoredChat,
        external: &ExternalToolset,
    ) -> Result<Self> {
        // 内部ツールと外部ツールを1つの一覧にして公開する(Issue #44)。名前空間化と
        // 衝突の排除は`ExternalToolset`が済ませてある。ツールに対応しないモデルには何も渡さない
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
        Ok(Self {
            chat,
            history,
            exposed_tools,
            tools_available,
            system: ChatMessage::System(build_system_prompt(chat, &ctx.prompts, tools_available)),
        })
    }

    pub(super) fn tools_available(&self) -> bool {
        self.tools_available
    }

    /// ツールを渡すラウンドの数。上限のラウンドまでツールを実行したら、ツールを渡さずに
    /// もう一度だけ呼ぶ(docs/spec/rebuild/tools.md 4節)。`u64`で数えるのは、上限が
    /// `u32::MAX`でも最後の1回を数えられるようにするため。ツールに対応しないモデルは、
    /// 最初の呼び出しがその最後の1回になる。
    pub(super) fn tool_rounds(&self, ctx: &TurnContext<'_>) -> u64 {
        if self.tools_available {
            u64::from(ctx.limits.max_rounds_per_turn)
        } else {
            0
        }
    }

    /// 1ラウンドで送る発言列と、渡すツール。`round_trip`はこのターンでここまでに行った
    /// ツール呼び出しの往復、`final_call`はツールを渡さない最後の呼び出しか。
    pub(super) async fn round(
        &self,
        db: SharedConnection,
        ctx: &TurnContext<'_>,
        round_trip: &[ChatMessage],
        final_call: bool,
    ) -> Result<(Vec<ChatMessage>, &[ToolSchema])> {
        let chat = self.chat;
        let notes: &'static [&'static str] = if final_call && self.tools_available {
            &[ROUND_LIMIT_NOTE]
        } else {
            &[]
        };
        let state = with_conn(db, move |conn| build_state(conn, chat, notes)).await?;

        let offered: &[ToolSchema] = if final_call { &[] } else { &self.exposed_tools };
        // このラウンドまでの往復と最新状態はラウンドごとに伸び・変わるので、間引きも
        // ラウンドごとにやり直す。最新状態は直近のユーザー発言に添えるが、見積もりでは
        // 別の発言として数える。
        let state_estimate = ChatMessage::user(state.clone());
        let kept = trim_history(
            &self.history,
            ctx.capabilities.context_length,
            [&self.system, &state_estimate]
                .into_iter()
                .chain(round_trip),
            offered,
        );

        let mut messages = Vec::with_capacity(1 + kept.len() + round_trip.len());
        messages.push(self.system.clone());
        messages.extend(kept.iter().cloned());
        attach_state(&mut messages, &state);
        messages.extend(round_trip.iter().cloned());
        Ok((messages, offered))
    }
}

/// 最新状態を直近のユーザー発言の後ろに添える。システムプロンプトに置かないのは、毎回変わる
/// ものを発言列の先頭に置くと、先頭一致のプロンプトキャッシュがそこで切れるため。会話の途中に
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
