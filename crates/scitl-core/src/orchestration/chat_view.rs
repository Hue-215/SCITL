//! 画面に渡す会話の行。どの行をどの順で出すかは`db::messages::list_for_chat`が決め、
//! ここは行ごとの表示の形を足すだけ。

use rusqlite::Connection;
use serde::Serialize;

use crate::db::messages::{self, Chat, Kind, Message};
use crate::error::Result;
use crate::orchestration::tool_record::ToolExecutionView;
use crate::orchestration::turn::require_chat;

#[derive(Debug, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
pub struct MessageView {
    #[serde(flatten)]
    pub message: Message,
    /// ツール実行記録の行だけが持つ。
    pub tool_execution: Option<ToolExecutionView>,
}

/// 1つの会話の行を、画面に出す形で返す。タスクが存在しない・削除済みなら`TaskNotFound`
/// (行の無い会話と区別する)。
pub fn list_chat(conn: &Connection, chat: Chat) -> Result<Vec<MessageView>> {
    require_chat(conn, chat)?;
    Ok(messages::list_for_chat(conn, chat)?
        .into_iter()
        .map(|message| MessageView {
            tool_execution: (message.kind == Kind::ToolExecution)
                .then(|| ToolExecutionView::of_content(&message.content)),
            message,
        })
        .collect())
}
