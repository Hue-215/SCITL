//! 1ターンの各ラウンドでモデルへ送る発言列とツールの組み立て。ターン([`super::turn`])と
//! 送信内容のプレビュー([`super::preview`])が同じ組み立てを通る(principles.md 5節)。

use crate::blocking;
use crate::db::messages::Chat;
use crate::db::{with_conn, SharedConnection};
use crate::error::Result;
use crate::llm::{ChatMessage, ToolSchema};
use crate::orchestration::history::{self, HistoryOptions, StoredChat};
use crate::orchestration::history_trim::trim_history;
use crate::orchestration::state_prompt::build_system_prompt;
use crate::orchestration::{SystemPrompts, TurnContext};
use crate::tools::{self, external::ExternalToolset};

/// ツールの上限に達したあとの最後の呼び出しで、システムプロンプトの末尾に足す一節。
/// ツールを渡さない理由を伝えないと、モデルがツールを呼ぶつもりの文を返しがちになる。
const ROUND_LIMIT_NOTE: &str = "The tool call limit for this turn has been reached, so no tools \
     are available now. Reply to the user based on the tool results so far.";

/// 1ターンのうち、ラウンドによらない送信の材料。
pub(super) struct TurnRequest {
    chat: Chat,
    history: Vec<ChatMessage>,
    exposed_tools: Vec<ToolSchema>,
    tools_available: bool,
    // ラウンドごとにDBスレッドへ渡すので、所有した文字列で持つ。
    base_prompt: Option<String>,
    task_chat_prompt: Option<String>,
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
            base_prompt: ctx.prompts.base.map(str::to_string),
            task_chat_prompt: ctx.prompts.task_chat.map(str::to_string),
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
        let tools_available = self.tools_available;
        let base = self.base_prompt.clone();
        let task_chat = self.task_chat_prompt.clone();
        let mut system_prompt_text = with_conn(db, move |conn| {
            let prompts = SystemPrompts {
                base: base.as_deref(),
                task_chat: task_chat.as_deref(),
            };
            build_system_prompt(conn, chat, &prompts, tools_available)
        })
        .await?;
        if final_call && tools_available {
            system_prompt_text.push_str("\n\n");
            system_prompt_text.push_str(ROUND_LIMIT_NOTE);
        }

        let offered: &[ToolSchema] = if final_call { &[] } else { &self.exposed_tools };
        let system = ChatMessage::System(system_prompt_text);
        // システムプロンプトとこのラウンドまでの往復はラウンドごとに伸びるので、間引きも
        // ラウンドごとにやり直す。
        let kept = trim_history(
            &self.history,
            ctx.capabilities.context_length,
            std::iter::once(&system).chain(round_trip),
            offered,
        );

        let mut messages = Vec::with_capacity(1 + kept.len() + round_trip.len());
        messages.push(system);
        messages.extend(kept.iter().cloned());
        messages.extend(round_trip.iter().cloned());
        Ok((messages, offered))
    }
}
