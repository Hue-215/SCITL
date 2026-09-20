//! stdio方式のMCPサーバー(子プロセス)への接続とツール一覧取得。
//!
//! stdio方式の登録は、アプリと同じ権限での任意コード実行の許可であることに注意
//! (Opusレビュー指摘)。信頼境界は「ユーザーが明示的に登録したこと」自体に置く
//! (principles.md 4節「外部連携の境界を明確にする」)が、その上で以下の多層防御を行う:
//! - 環境変数を継承させない(`env_clear`)。既定の環境変数継承は、ユーザーが登録した
//!   サーバーへシェルの他の秘密情報(他社APIキー等)を黙って渡してしまう
//! - シェルを経由しない(`command`/`args`を分離したまま渡す。1行のコマンド文字列を
//!   受け取って分割するような実装はしない)
//! - プロセスグループごとkillする(`npx`等が生む孫プロセスの取り残しを防ぐ。
//!   `rmcp`の子プロセスtransport自体は直接の子しかkillしないため`process-wrap`の
//!   `ProcessGroup`/`JobObject`を明示的に併用する)
//! - stderrは継承させず、上限付きで捕捉してエラー診断にのみ使う(サーバーが書いた
//!   文字列をアプリの標準エラーへ素通りさせない)

use std::process::Stdio;

use process_wrap::tokio::{CommandWrap, ProcessGroup};
use rmcp::transport::TokioChildProcess;
use rmcp::ServiceExt;
use secrecy::ExposeSecret;
use tokio::io::AsyncReadExt;
use tokio::process::ChildStderr;

use crate::config::SecretRef;
use crate::db::error::CoreError;

use super::{resolve_secrets, sanitize_tool_text, McpToolInfo};

/// 子プロセスの標準エラーから読み取る上限バイト数。エラー表示に使う分だけあればよく、
/// サーバーが大量に出力してもメモリを食い潰さないようにする。
const MAX_CAPTURED_STDERR_BYTES: usize = 4096;

pub(super) async fn list_tools(
    command: &str,
    args: &[String],
    env_refs: &[SecretRef],
) -> Result<Vec<McpToolInfo>, CoreError> {
    let mut cmd = tokio::process::Command::new(command);
    cmd.args(args);

    // 親プロセスの環境変数を継承させない。子プロセスに必要なのは実行に最低限要る
    // ものと、ユーザーが明示的に登録した値だけ(Opusレビュー指摘)。
    cmd.env_clear();
    for name in inherited_env_allowlist() {
        if let Ok(value) = std::env::var(name) {
            cmd.env(name, value);
        }
    }
    let resolved = resolve_secrets(env_refs).await?;
    for (name, secret) in &resolved {
        cmd.env(name, secret.expose_secret());
    }

    let mut wrapped: CommandWrap = cmd.into();
    wrapped.wrap(ProcessGroup::leader());
    #[cfg(windows)]
    wrapped.wrap(process_wrap::tokio::JobObject);

    let (child, stderr) = TokioChildProcess::builder(wrapped)
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| CoreError::Mcp(format!("failed to spawn MCP server process: {e}")))?;

    // 子プロセス(`child`)の後始末は`rmcp`側に委ねる: `serve`に渡した後は
    // `service.cancel()`が`Transport::close`経由で`graceful_shutdown`を呼び、
    // `serve`自体が失敗した場合も`TokioChildProcess`のDropがkillする(安全網)。
    match run(child).await {
        Ok(tools) => Ok(tools),
        Err(reason) => {
            let captured = capture_stderr(stderr).await;
            Err(CoreError::Mcp(if captured.is_empty() {
                reason
            } else {
                format!("{reason} (stderr: {captured})")
            }))
        }
    }
}

async fn run(child: TokioChildProcess) -> Result<Vec<McpToolInfo>, String> {
    let service = ()
        .serve(child)
        .await
        .map_err(|e| format!("failed to connect: {e}"))?;
    let result = service.list_tools(None).await;
    let _ = service.cancel().await;
    let result = result.map_err(|e| format!("failed to list tools: {e}"))?;
    Ok(result
        .tools
        .into_iter()
        .map(|t| McpToolInfo {
            name: sanitize_tool_text(&t.name),
            description: t.description.as_deref().map(sanitize_tool_text),
        })
        .collect())
}

async fn capture_stderr(stderr: Option<ChildStderr>) -> String {
    let Some(mut stderr) = stderr else {
        return String::new();
    };
    let mut buf = Vec::with_capacity(MAX_CAPTURED_STDERR_BYTES);
    let _ = (&mut stderr)
        .take(MAX_CAPTURED_STDERR_BYTES as u64)
        .read_to_end(&mut buf)
        .await;
    sanitize_tool_text(&String::from_utf8_lossy(&buf))
}

/// stdio子プロセスに引き継ぐ環境変数の許可リスト。OS標準の実行に必要な最小限のみ
/// (PATH解決、ホームディレクトリ、Windowsのシステムディレクトリ)。
fn inherited_env_allowlist() -> &'static [&'static str] {
    if cfg!(windows) {
        &["PATH", "USERPROFILE", "SYSTEMROOT", "TEMP", "TMP"]
    } else {
        &["PATH", "HOME"]
    }
}
