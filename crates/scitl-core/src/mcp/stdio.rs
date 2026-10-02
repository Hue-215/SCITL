//! stdio方式のMCPサーバー(子プロセス)への接続。
//!
//! stdio方式の登録は、アプリと同じ権限での任意コード実行の許可にあたる。信頼境界は
//! ユーザーが登録したこと自体に置くが、その上で次の防御を重ねる。
//! - 環境変数を継承させない(`env_clear`)。既定の環境変数継承は、ユーザーが登録した
//!   サーバーへシェルの他の秘密情報(他社APIキー等)を黙って渡してしまう
//! - シェルを経由しない(`command`/`args`を分離したまま渡す。1行のコマンド文字列を
//!   受け取って分割するような実装はしない)
//! - プロセスグループごとkillする(`npx`等が生む孫プロセスの取り残しを防ぐ。
//!   `rmcp`の子プロセスtransport自体は直接の子しかkillしないため`process-wrap`の
//!   `ProcessGroup`/`JobObject`を明示的に併用する)。子が標準入力の終了を受けて
//!   自分で終了した場合も、残ったグループをkillする(`KillGroupAfterExit`)
//! - stderrは継承させず、上限付きで捕捉してエラー診断にのみ使う(サーバーが書いた
//!   文字列をアプリの標準エラーへ素通りさせない)

use std::process::Stdio;
#[cfg(unix)]
use std::{future::Future, pin::Pin, process::ExitStatus};

#[cfg(unix)]
use process_wrap::tokio::ChildWrapper;
use process_wrap::tokio::{CommandWrap, ProcessGroup};
use rmcp::transport::TokioChildProcess;
use rmcp::ServiceExt;
use secrecy::ExposeSecret;
use tokio::io::AsyncReadExt;
use tokio::process::ChildStderr;

use crate::config::SecretRef;
use crate::error::CoreError;

use super::{resolve_secrets, ClientService};

/// 子プロセスの標準エラーから読み取る上限バイト数。エラー表示に使う分だけあればよく、
/// サーバーが大量に出力してもメモリを食い潰さないようにする。
const MAX_CAPTURED_STDERR_BYTES: usize = 4096;
/// 捕捉した標準エラーを画面に出す診断文字列として整えるときの上限文字数。
const MAX_STDERR_CHARS: usize = 2000;

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
    // `ProcessGroup`より後に足し、その外側に被せる(`wrap_child`は足した順に適用される)。
    #[cfg(unix)]
    wrapped.wrap(KillGroupAfterExit);
    #[cfg(windows)]
    wrapped.wrap(process_wrap::tokio::JobObject);

    let (child, stderr) = TokioChildProcess::builder(wrapped)
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| CoreError::Mcp(format!("failed to spawn MCP server process: {e}")))?;

    // 子プロセス(`child`)の後始末は`rmcp`側に委ねる: `serve`に渡した後は
    // `service.cancel()`が`Transport::close`経由で`graceful_shutdown`を呼び、
    // `serve`自体が失敗した場合も`TokioChildProcess`のDropがkillする(安全網)。
    match super::client_config().serve(child).await {
        Ok(service) => {
            drain_stderr(stderr);
            Ok(service)
        }
        Err(e) => {
            let captured = capture_stderr(stderr).await;
            let reason = format!("failed to connect: {}", super::describe_server_error(&e));
            Err(CoreError::Mcp(if captured.is_empty() {
                reason
            } else {
                format!("{reason} (stderr: {captured})")
            }))
        }
    }
}

/// 子プロセスの終了を待ち終えたあとに、プロセスグループの残りをkillさせる。
///
/// 子が自分で終了した場合は他にkillを呼ぶ経路が無いので、`wait`の後に結果に関わらず
/// グループへSIGKILLを送る(既に誰も居なければ失敗するだけなので結果は捨てる)。
/// 成員が残っている間はそのグループIDが別のプロセスに再利用されないので(POSIXの規定)、
/// 届く先は残った成員になる。
#[cfg(unix)]
#[derive(Debug)]
struct KillGroupAfterExit;

#[cfg(unix)]
impl process_wrap::tokio::CommandWrapper for KillGroupAfterExit {
    fn wrap_child(
        &mut self,
        inner: Box<dyn ChildWrapper>,
        _core: &CommandWrap,
    ) -> std::io::Result<Box<dyn ChildWrapper>> {
        Ok(Box::new(KillGroupAfterExitChild(inner)))
    }
}

#[cfg(unix)]
#[derive(Debug)]
struct KillGroupAfterExitChild(Box<dyn ChildWrapper>);

#[cfg(unix)]
impl ChildWrapper for KillGroupAfterExitChild {
    fn inner(&self) -> &dyn ChildWrapper {
        self.0.as_ref()
    }

    fn inner_mut(&mut self) -> &mut dyn ChildWrapper {
        self.0.as_mut()
    }

    fn into_inner(self: Box<Self>) -> Box<dyn ChildWrapper> {
        self.0
    }

    fn wait(&mut self) -> Pin<Box<dyn Future<Output = std::io::Result<ExitStatus>> + Send + '_>> {
        Box::pin(async {
            let status = self.0.wait().await;
            // 内側は`ProcessGroupChild`なので、`start_kill`はグループ全体へのSIGKILLになる。
            let _ = self.0.start_kill();
            status
        })
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
    crate::text::display_block(&String::from_utf8_lossy(&buf), MAX_STDERR_CHARS)
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

/// 多層防御(環境変数を継承させない・プロセスグループごと終了させる)が、依存の更新で
/// 黙って崩れていないことを、偽のMCPサーバーを実際に起動して確かめる。
#[cfg(all(test, unix))]
mod tests {
    use std::time::{Duration, Instant};

    use secrecy::SecretString;

    use super::*;

    /// `initialize`にだけ応答する偽のMCPサーバー。受け取った環境変数と、自分が起動した
    /// 孫プロセスのPIDを、第1引数のディレクトリに書き出す。第2引数が`linger`なら、
    /// 標準入力が閉じられても終了せずに居座る。
    const FAKE_SERVER: &str = r#"
out="$1"
env > "$out/env"
sleep 60 &
echo $! > "$out/grandchild"
read -r line
id=$(printf '%s\n' "$line" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
version=$(printf '%s\n' "$line" | sed -n 's/.*"protocolVersion":"\([^"]*\)".*/\1/p')
printf '{"jsonrpc":"2.0","id":%s,"result":{"protocolVersion":"%s","capabilities":{},"serverInfo":{"name":"fake","version":"0"}}}\n' "$id" "$version"
while read -r line; do :; done
if [ "$2" = linger ]; then sleep 60; fi
"#;

    /// 偽のサーバーのスクリプトと出力を置く、テストごとの一時ディレクトリ。
    struct Scratch(tempfile::TempDir);

    impl Scratch {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            std::fs::write(dir.path().join("server.sh"), FAKE_SERVER).unwrap();
            Self(dir)
        }

        fn args(&self, mode: &str) -> Vec<String> {
            vec![
                self.0.path().join("server.sh").display().to_string(),
                self.0.path().display().to_string(),
                mode.to_string(),
            ]
        }

        fn read(&self, file: &str) -> String {
            std::fs::read_to_string(self.0.path().join(file)).unwrap()
        }

        fn grandchild(&self) -> String {
            self.read("grandchild").trim().to_string()
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            // テストが途中で落ちても孫プロセスを残さない。
            if let Ok(pid) = std::fs::read_to_string(self.0.path().join("grandchild")) {
                let _ = std::process::Command::new("kill")
                    .args(["-KILL", pid.trim()])
                    .status();
            }
        }
    }

    /// プロセスが生きているか。終了して回収を待つだけのゾンビは終了扱いにする。
    fn is_alive(pid: &str) -> bool {
        let out = std::process::Command::new("ps")
            .args(["-o", "stat=", "-p", pid])
            .output()
            .unwrap();
        let stat = String::from_utf8_lossy(&out.stdout);
        let stat = stat.trim();
        !stat.is_empty() && !stat.starts_with('Z')
    }

    /// 終了は非同期に進むので、上限までポーリングして待つ。
    async fn wait_until_gone(pid: &str) -> bool {
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if !is_alive(pid) {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        false
    }

    fn names(env: &str) -> Vec<&str> {
        env.lines()
            .filter_map(|l| l.split_once('='))
            .map(|(n, _)| n)
            .collect()
    }

    /// 偽のサーバーの応答がrmcpの期待とずれると、接続は失敗せずに待ち続ける。テストが
    /// 止まらずに失敗するよう、上限を設ける。
    async fn connect_fake(scratch: &Scratch, mode: &str, env_refs: &[SecretRef]) -> ClientService {
        tokio::time::timeout(
            Duration::from_secs(10),
            connect("/bin/sh", &scratch.args(mode), env_refs),
        )
        .await
        .expect("the fake server did not complete initialize")
        .unwrap()
    }

    #[tokio::test]
    async fn only_the_allowlist_and_registered_values_reach_the_server() {
        // 既定の保存先はプロセス全体で1つで、元に戻せない。このテストバイナリで秘密情報を
        // 読み書きするテストは、以降すべてこのモックを使う。
        keyring_core::set_default_store(keyring_core::mock::Store::new().unwrap());
        crate::secrets::store("stdio-test-token", &SecretString::from("s3cret")).unwrap();
        // 親プロセスには許可リスト外の変数がある(無ければ、このテストは何も確かめない)。
        let allowlist = inherited_env_allowlist();
        assert!(std::env::vars().any(|(n, _)| !allowlist.contains(&n.as_str())));

        let scratch = Scratch::new();
        let refs = [SecretRef {
            name: "FAKE_TOKEN".to_string(),
            key_ref: "stdio-test-token".to_string(),
        }];
        let mut service = connect_fake(&scratch, "exit", &refs).await;
        let _ = service.close_with_timeout(Duration::from_secs(5)).await;

        let env = scratch.read("env");
        assert!(env.lines().any(|l| l == "FAKE_TOKEN=s3cret"), "{env}");
        if let Ok(path) = std::env::var("PATH") {
            assert!(env.lines().any(|l| l == format!("PATH={path}")), "{env}");
        }
        // シェル自身が設定する変数は除く。
        const SHELL_OWN: &[&str] = &["PWD", "OLDPWD", "SHLVL", "_"];
        for name in names(&env) {
            assert!(
                allowlist.contains(&name) || name == "FAKE_TOKEN" || SHELL_OWN.contains(&name),
                "{name} leaked into the server's environment"
            );
        }
    }

    #[tokio::test]
    async fn closing_kills_the_grandchildren_of_a_server_that_does_not_exit() {
        let scratch = Scratch::new();
        let mut service = connect_fake(&scratch, "linger", &[]).await;
        let grandchild = scratch.grandchild();
        assert!(is_alive(&grandchild));

        let _ = service.close_with_timeout(Duration::from_secs(5)).await;
        assert!(
            wait_until_gone(&grandchild).await,
            "grandchild {grandchild} survived"
        );
    }

    /// 子が標準入力の終了を受けて自分で終了すると`rmcp`は`kill()`を呼ばないので、
    /// グループの残りを止めるのは`KillGroupAfterExit`だけになる。
    #[tokio::test]
    async fn closing_kills_the_grandchildren_of_a_server_that_exits_on_its_own() {
        let scratch = Scratch::new();
        let mut service = connect_fake(&scratch, "exit", &[]).await;
        let grandchild = scratch.grandchild();
        assert!(is_alive(&grandchild));

        let _ = service.close_with_timeout(Duration::from_secs(5)).await;
        assert!(
            wait_until_gone(&grandchild).await,
            "grandchild {grandchild} survived"
        );
    }
}
