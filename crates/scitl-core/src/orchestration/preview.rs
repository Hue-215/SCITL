//! 送信内容のプレビュー(Issue #23)。次のターンの最初のリクエストを、モデルを呼ばずに
//! 組み立てる。発言列とツールの組み立てはターンと同じ[`TurnRequest`]を通し、本文はアダプタが
//! `send`と同じ組み立てで返す(`LlmAdapter::request_preview`)。写しを作らないので、ここで
//! 見えるものがそのままターンで送られる。

use serde::Serialize;

use crate::db::messages::Chat;
use crate::db::{in_rolled_back_transaction, with_conn, SharedConnection};
use crate::error::{CoreError, Result};
use crate::llm::RequestPreview;
use crate::mcp::McpSessions;
use crate::orchestration::history;
use crate::orchestration::turn::{
    insert_user_message, prepare_external_tools, ready_adapter, require_chat, require_content,
};
use crate::orchestration::turn_request::TurnRequest;
use crate::orchestration::{TurnContext, TurnFailure};
use crate::tools::external::ExternalToolset;

#[derive(Debug, Default)]
pub struct PreviewOptions {
    /// 次に送るユーザー発言。あれば送ったときと同じく会話に書いてから組み立て、書いた行は
    /// 巻き戻す。無ければ保存済みの会話のまま組み立てる。
    pub message: Option<String>,
    /// 有効にした外部ツールサーバーへ接続して、外部ツールの定義を含めるか。接続は外部の
    /// プロセスの起動や通信を伴うので、求められたときだけ行う。
    pub external_tools: bool,
}

#[derive(Debug, Serialize)]
pub struct Preview {
    /// 外部ツールの定義を含めたか。含めなければ、ターンでは外部ツールの分だけ増える。
    pub external_tools: bool,
    #[serde(flatten)]
    pub request: RequestPreview,
}

/// 次のターンの最初のリクエスト。チャットを使えない(プロバイダー・モデルの未選択等)なら、
/// ターンがエラー発言にするのと同じ分類を返す。何も保存しない。
pub async fn preview_request(
    db: SharedConnection,
    ctx: &TurnContext<'_>,
    chat: Chat,
    options: PreviewOptions,
) -> Result<std::result::Result<Preview, TurnFailure>> {
    let adapter = match ready_adapter(ctx) {
        Ok(adapter) => adapter,
        Err(failure) => return Ok(Err(failure)),
    };
    if let Some(text) = &options.message {
        require_content(text, 0)?;
    }

    let message = options.message;
    let stored = with_conn(db.clone(), move |conn| {
        in_rolled_back_transaction(conn, |conn| {
            require_chat(conn, chat)?;
            if let Some(text) = &message {
                insert_user_message(conn, chat, text)?;
            }
            history::load(conn, chat)
        })
    })
    .await?;

    // ターンと同じく、ツールに対応しないモデルには外部ツールも渡さないので繋がない。
    let external_tools = options.external_tools && ctx.capabilities.tools;
    let external = if external_tools {
        let mut sessions = McpSessions::new();
        let external = prepare_external_tools(&ctx.mcp, chat, &mut sessions).await;
        sessions.close().await;
        external
    } else {
        ExternalToolset::default()
    };

    let request = TurnRequest::prepare(ctx, chat, stored, &external).await?;
    let final_call = request.tool_rounds(ctx) == 0;
    let (messages, offered) = request.round(db, ctx, &[], final_call).await?;
    let request = adapter
        .request_preview(&messages, offered, ctx.reasoning_effort)
        .ok_or_else(|| {
            CoreError::Internal("this provider cannot preview its requests".to_string())
        })??;
    Ok(Ok(Preview {
        external_tools,
        request,
    }))
}
