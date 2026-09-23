//! stdio方式のMCPサーバー(子プロセス)への接続。
//!
//! stdio方式の登録は、アプリと同じ権限での任意コード実行の許可であることに注意。
//! 信頼境界は「ユーザーが明示的に登録したこと」自体に置く
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

use super::{resolve_secrets, ClientService};

/// 子プロセスの標準エラーから読み取る上限バイト数。エラー表示に使う分だけあればよく、
/// サーバーが大量に出力してもメモリを食い潰さないようにする。
const MAX_CAPTURED_STDERR_BYTES: usize = 4096;

/// 子プロセスを起動し、MCPセッションを確立する。確立に失敗した場合は、捕捉した
/// 標準エラー出力を添えたエラーを返す(接続できない原因はたいていサーバー側の
/// 起動失敗で、その手掛かりはstderrにしか出ないため)。
pub(super) async fn connect(
    command: &str,
    args: &[String],
    env_refs: &[SecretRef],
) -> Result<ClientService, CoreError> {
    let mut cmd = tokio::process::Command::new(command);
    cmd.args(args);

    // 親プロセスの環境変数を継承させない。子プロセスに必要なのは実行に最低限要る
    // ものと、ユーザーが明示的に登録した値だけ。
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
    match ().serve(child).await {
        Ok(service) => {
            drain_stderr(stderr);
            Ok(service)
        }
        Err(e) => {
            let captured = capture_stderr(stderr).await;
            let reason = format!("failed to connect: {e}");
            Err(CoreError::Mcp(if captured.is_empty() {
                reason
            } else {
                format!("{reason} (stderr: {captured})")
            }))
        }
    }
}

/// 接続後の標準エラーは読み捨てる。パイプを閉じる(handleをdropする)と、以降サーバーが
/// stderrへ書いた時点で壊れたパイプになり、読まずに保持し続けるとパイプのバッファが
/// 埋まった時点でサーバーが止まる。セッションはターンの間だけ生きるため、その間の
/// 出力を捨て続ける常駐タスクを1本置く(内容は使わない。診断に使うのは接続失敗時のみ)。
fn drain_stderr(stderr: Option<ChildStderr>) {
    let Some(mut stderr) = stderr else { return };
    tokio::spawn(async move {
        let mut buf = [0u8; 1024];
        while matches!(stderr.read(&mut buf).await, Ok(n) if n > 0) {}
    });
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
    super::sanitize_tool_text(&String::from_utf8_lossy(&buf))
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
