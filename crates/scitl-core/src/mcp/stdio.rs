//! stdio方式のMCPサーバー(子プロセス)への接続。
//!
//! stdio方式の登録は、アプリと同じ権限での任意コード実行の許可にあたる。信頼境界は
//! ユーザーが登録したこと自体に置くが、その上で次の防御を重ねる。
//! - 環境変数を継承させない(`env_clear`)。既定の環境変数継承は、ユーザーが登録した
//!   サーバーへシェルの他の秘密情報(他社APIキー等)を黙って渡してしまう
//! - シェルを経由しない(`command`/`args`を分離したまま渡す。1行のコマンド文字列を
//!   受け取って分割するような実装はしない)。Windowsでは`npx`のように実体がバッチファイルの
//!   コマンドがあり、バッチファイルはOSの仕組みとして`cmd.exe`が実行する。その場合も引数は
//!   分離したまま渡し、`cmd.exe`に解釈されない形へ整えるのは標準ライブラリに任せる
//!   (整えられない引数は起動の失敗になる)
//! - プロセスグループごとkillする(`npx`等が生む孫プロセスの取り残しを防ぐ。
//!   `rmcp`の子プロセスtransport自体は直接の子しかkillしないため`process-wrap`の
//!   `ProcessGroup`/`JobObject`を明示的に併用する)。子が標準入力の終了を受けて
//!   自分で終了した場合も、残ったグループをkillする(`KillGroupAfterExit`)
//! - stderrは継承させず、上限付きで捕捉してエラー診断にのみ使う(サーバーが書いた
//!   文字列をアプリの標準エラーへ素通りさせない)。Windowsではコンソールウィンドウも持たせない
//!   (GUIからの起動でコンソールウィンドウが開くのと、CLIの端末へ直接書かれるのを防ぐ)

#[cfg(windows)]
use std::path::PathBuf;
use std::process::Stdio;
use std::{future::Future, pin::Pin, process::ExitStatus};

use process_wrap::tokio::{ChildWrapper, CommandWrap};
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
    #[cfg(windows)]
    let mut cmd = tokio::process::Command::new(
        batch_file_on_path(
            command,
            std::env::var_os("PATH")
                .iter()
                .flat_map(std::env::split_paths),
        )
        .unwrap_or_else(|| command.into()),
    );
    #[cfg(not(windows))]
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
    #[cfg(unix)]
    wrapped.wrap(process_wrap::tokio::ProcessGroup::leader());
    // `JobObject`は`KillOnDrop`が併用されているときだけ、ジョブのハンドルが閉じられたら成員を
    // 終了させる設定にする。アプリ自体が終了・異常終了してkillを呼べなかった場合も、ハンドルは
    // OSが閉じるので孫まで止まる。
    #[cfg(windows)]
    wrapped
        .wrap(process_wrap::tokio::KillOnDrop)
        .wrap(process_wrap::tokio::JobObject)
        .wrap(process_wrap::tokio::CreationFlags(
            windows::Win32::System::Threading::CREATE_NO_WINDOW,
        ));
    // グループを作るラッパーより後に足し、その外側に被せる(`wrap_child`は足した順に適用される)。
    wrapped.wrap(KillGroupAfterExit);

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

/// 子プロセスの終了を待ち終えたあとに、プロセスグループ(Windowsではジョブ)の残りをkillさせる。
///
/// 子が自分で終了した場合は他にkillを呼ぶ経路が無いので、`wait`の後に結果に関わらず
/// グループへSIGKILLを送る(既に誰も居なければ失敗するだけなので結果は捨てる)。
/// 成員が残っている間はそのグループIDが別のプロセスに再利用されないので(POSIXの規定)、
/// 届く先は残った成員になる。Windowsではジョブをハンドルで指すので、取り違えは起きない。
#[derive(Debug)]
struct KillGroupAfterExit;

impl process_wrap::tokio::CommandWrapper for KillGroupAfterExit {
    fn wrap_child(
        &mut self,
        inner: Box<dyn ChildWrapper>,
        _core: &CommandWrap,
    ) -> std::io::Result<Box<dyn ChildWrapper>> {
        Ok(Box::new(KillGroupAfterExitChild(inner)))
    }
}

#[derive(Debug)]
struct KillGroupAfterExitChild(Box<dyn ChildWrapper>);

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

    #[cfg(windows)]
    fn process_handle(&self) -> Option<std::os::windows::io::BorrowedHandle<'_>> {
        self.0.process_handle()
    }

    fn wait(&mut self) -> Pin<Box<dyn Future<Output = std::io::Result<ExitStatus>> + Send + '_>> {
        Box::pin(async {
            #[cfg(unix)]
            let status = self.0.wait().await;
            // `JobObjectChild`の`wait`は、ジョブの成員全員の終了を待つものとされている(10.0.0では
            // 孫が残っていても返るが、文書どおりになると孫が居る間は返らなくなる)。それに依らず、
            // その内側(起動した子そのもの)の終了だけを待つ。
            #[cfg(windows)]
            let status = self.0.inner_mut().wait().await;
            // 内側は`ProcessGroupChild`(Windowsでは`JobObjectChild`)なので、`start_kill`は
            // グループ全体へのSIGKILL(ジョブの終了)になる。
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

/// 拡張子もディレクトリも付いていないコマンド名が、PATH上のバッチファイル(`npx.cmd`等)を
/// 指していれば、そのパスを返す。
///
/// OSも標準ライブラリも、拡張子の無い名前には`.exe`しか補わないので、`npx`と登録された
/// サーバーはそのままでは起動できない。`.exe`が先に見つかる場合は`None`を返し、解決を
/// 標準ライブラリに任せる(どちらが選ばれるかはPATHの並び順で決まる)。
///
/// 絶対パスでないPATHの要素は探さない。空の要素(PATHの末尾の`;`等)や相対の要素を繋ぐと、
/// 作業ディレクトリを探すことになる。
#[cfg(windows)]
fn batch_file_on_path(command: &str, dirs: impl Iterator<Item = PathBuf>) -> Option<PathBuf> {
    use std::path::{Component, Path};

    let name = Path::new(command);
    let mut components = name.components();
    let bare =
        matches!(components.next(), Some(Component::Normal(_))) && components.next().is_none();
    if !bare || name.extension().is_some() {
        return None;
    }
    for dir in dirs.filter(|dir| dir.is_absolute()) {
        let with_extension = |extension: &str| dir.join(format!("{command}.{extension}"));
        if with_extension("exe").is_file() {
            return None;
        }
        if let Some(batch) = ["cmd", "bat"]
            .iter()
            .map(|e| with_extension(e))
            .find(|p| p.is_file())
        {
            return Some(batch);
        }
    }
    None
}

/// stdio子プロセスに引き継ぐ環境変数の許可リスト。プログラムがOSの上で動くのに要る場所の
/// 情報だけにする(PATH解決、ホームディレクトリ、Windowsのシステムとアプリのデータのディレクトリ)。
///
/// Windowsのものは、MCPの公式SDK(TypeScript)が既定で引き継ぐ一覧に`TMP`を足したもの。
/// Windowsのプログラムは、これらが指す場所を前提に動く。`APPDATA`・`LOCALAPPDATA`が無いと、
/// `npm`はキャッシュをホームディレクトリ直下に作り、インストール先を解決できない。
fn inherited_env_allowlist() -> &'static [&'static str] {
    if cfg!(windows) {
        &[
            "APPDATA",
            "COMSPEC",
            "HOMEDRIVE",
            "HOMEPATH",
            "LOCALAPPDATA",
            "PATH",
            "PATHEXT",
            "PROCESSOR_ARCHITECTURE",
            "PROGRAMDATA",
            "PROGRAMFILES",
            "PROGRAMFILES(X86)",
            "PROGRAMW6432",
            "SYSTEMDRIVE",
            "SYSTEMROOT",
            "TEMP",
            "TMP",
            "USERNAME",
            "USERPROFILE",
            "WINDIR",
        ]
    } else {
        &["PATH", "HOME"]
    }
}

/// 多層防御(環境変数を継承させない・プロセスグループごと終了させる)が、依存の更新で
/// 黙って崩れていないことを、偽のMCPサーバーを実際に起動して確かめる。
#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use secrecy::SecretString;

    use super::*;

    /// 偽のサーバーを動かすシェルと、スクリプトのファイル名。
    #[cfg(unix)]
    const SHELL: &str = "/bin/sh";
    #[cfg(unix)]
    const SCRIPT: &str = "server.sh";
    #[cfg(windows)]
    const SHELL: &str = "powershell.exe";
    #[cfg(windows)]
    const SCRIPT: &str = "server.ps1";

    /// シェルにスクリプトを実行させるための、スクリプトのパスより前に置く引数。
    #[cfg(unix)]
    const SHELL_ARGS: &[&str] = &[];
    #[cfg(windows)]
    const SHELL_ARGS: &[&str] = &[
        "-NoProfile",
        "-NonInteractive",
        "-ExecutionPolicy",
        "Bypass",
        "-File",
    ];

    /// `initialize`にだけ応答する偽のMCPサーバー。受け取った環境変数と、自分が起動した
    /// 孫プロセスのPIDを、第1引数のディレクトリに書き出す。第2引数が`linger`なら、
    /// 標準入力が閉じられても終了せずに居座る。
    ///
    /// Windows版は、孫に標準入出力を引き継がせない(孫の出力が応答に混ざるため)。スクリプトは
    /// ASCIIだけで書く。Windows PowerShellはBOMの無いスクリプトをOSの既定の文字コードで読むので、
    /// 日本語のコメントを置くと、環境によっては次の行まで巻き込んで読まれる。
    #[cfg(windows)]
    const FAKE_SERVER: &str = r#"
param($out, $mode)
$vars = Get-ChildItem env: | ForEach-Object { "$($_.Name)=$($_.Value)" }
[IO.File]::WriteAllLines("$out\env", [string[]]$vars)
$psi = New-Object System.Diagnostics.ProcessStartInfo
$psi.FileName = "$env:SYSTEMROOT\System32\ping.exe"
$psi.Arguments = '-n 60 127.0.0.1'
$psi.UseShellExecute = $false
$psi.CreateNoWindow = $true
$psi.RedirectStandardInput = $true
$psi.RedirectStandardOutput = $true
$psi.RedirectStandardError = $true
$grandchild = [System.Diagnostics.Process]::Start($psi)
[IO.File]::WriteAllText("$out\grandchild", "$($grandchild.Id)")
$line = [Console]::In.ReadLine()
$id = [regex]::Match($line, '"id":(\d+)').Groups[1].Value
$version = [regex]::Match($line, '"protocolVersion":"([^"]*)"').Groups[1].Value
[Console]::Out.WriteLine('{"jsonrpc":"2.0","id":' + $id + ',"result":{"protocolVersion":"' + $version + '","capabilities":{},"serverInfo":{"name":"fake","version":"0"}}}')
[Console]::Out.Flush()
while ($null -ne [Console]::In.ReadLine()) {}
if ($mode -eq 'linger') { Start-Sleep 60 }
"#;
    #[cfg(unix)]
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
            std::fs::write(dir.path().join(SCRIPT), FAKE_SERVER).unwrap();
            Self(dir)
        }

        fn args(&self, mode: &str) -> Vec<String> {
            let mut args: Vec<String> = SHELL_ARGS.iter().map(|a| a.to_string()).collect();
            args.extend([
                self.0.path().join(SCRIPT).display().to_string(),
                self.0.path().display().to_string(),
                mode.to_string(),
            ]);
            args
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
                #[cfg(unix)]
                let _ = std::process::Command::new("kill")
                    .args(["-KILL", pid.trim()])
                    .status();
                #[cfg(windows)]
                let _ = std::process::Command::new("taskkill")
                    .args(["/F", "/PID", pid.trim()])
                    .output();
            }
        }
    }

    /// プロセスが生きているか。
    #[cfg(windows)]
    fn is_alive(pid: &str) -> bool {
        let out = std::process::Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}"), "/FO", "CSV", "/NH"])
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).contains(&format!("\"{pid}\""))
    }

    /// プロセスが生きているか。終了して回収を待つだけのゾンビは終了扱いにする。
    #[cfg(unix)]
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
            connect(SHELL, &scratch.args(mode), env_refs),
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
        #[cfg(unix)]
        const SHELL_OWN: &[&str] = &["PWD", "OLDPWD", "SHLVL", "_"];
        // PowerShellが起動時に自分で足す(最後のものは`-ExecutionPolicy`の指定による)。名前の
        // 大文字小文字は版で揺れるので区別しない。
        #[cfg(windows)]
        const SHELL_OWN: &[&str] = &["PATHEXT", "PSMODULEPATH", "PSEXECUTIONPOLICYPREFERENCE"];
        for name in names(&env) {
            let shell_own = SHELL_OWN.iter().any(|own| {
                if cfg!(windows) {
                    own.eq_ignore_ascii_case(name)
                } else {
                    *own == name
                }
            });
            assert!(
                allowlist.contains(&name) || name == "FAKE_TOKEN" || shell_own,
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

    /// 子が標準入力の終了を受けて自分で終了すると`rmcp`は`kill()`を呼ばないので、Unixでは
    /// グループの残りを止めるのは`KillGroupAfterExit`だけになる(Windowsでは、子を落とした
    /// ときにジョブのハンドルが閉じることでも止まる)。
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

    #[cfg(windows)]
    mod batch_files {
        use std::path::{Path, PathBuf};

        use super::super::batch_file_on_path;

        fn dir_with(files: &[&str]) -> tempfile::TempDir {
            let dir = tempfile::tempdir().unwrap();
            for file in files {
                std::fs::write(dir.path().join(file), "").unwrap();
            }
            dir
        }

        fn find(command: &str, dirs: &[&Path]) -> Option<PathBuf> {
            batch_file_on_path(command, dirs.iter().map(|d| d.to_path_buf()))
        }

        #[test]
        fn a_bare_name_finds_the_batch_file_on_the_path() {
            let empty = dir_with(&[]);
            // 拡張子の無い同名のファイル(シェルスクリプト)は、npmが並べて置く。
            let node = dir_with(&["npx", "npx.cmd"]);

            assert_eq!(
                find("npx", &[empty.path(), node.path()]),
                Some(node.path().join("npx.cmd"))
            );
        }

        #[test]
        fn an_executable_found_first_is_left_to_the_standard_library() {
            let exe = dir_with(&["tool.exe"]);
            let batch = dir_with(&["tool.cmd"]);

            assert_eq!(find("tool", &[exe.path(), batch.path()]), None);
            assert_eq!(
                find("tool", &[batch.path(), exe.path()]),
                Some(batch.path().join("tool.cmd"))
            );
            // 同じディレクトリに両方あれば、実行ファイルが勝つ。
            let both = dir_with(&["tool.exe", "tool.cmd"]);
            assert_eq!(find("tool", &[both.path()]), None);
        }

        #[test]
        fn a_name_with_an_extension_or_a_directory_is_not_searched() {
            let dir = dir_with(&["npx.cmd", "npx.cmd.cmd"]);

            assert_eq!(find("npx.cmd", &[dir.path()]), None);
            assert_eq!(find(r"bin\npx", &[dir.path()]), None);
            assert_eq!(find("missing", &[dir.path()]), None);
        }
    }
}
