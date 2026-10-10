---
name: release-build
description: SCITLのリリースの流れ(release/*を切る→版を上げる→確かめる→配布物を作って起動を確かめる→mainへ取り込む→タグ→GitHub Releases→developへ戻す)と、配布用ビルドの手順(scripts/release-build.sh・release-build.ps1・release-build-android.shの使い方、Androidの署名の鍵の扱い、ビルドした人の絶対パスをバイナリから外す仕組みと確かめ方、配布物(zip・tar.gz・APK)と第三者ライセンスの一覧の組み立て、trim-pathsが安定版に入ったときの移し替え)。リリースするとき、版を上げるとき、配布物を作るとき、依存のライセンスの許容(about.toml)を変えるとき、配布用ビルドの設定(RUSTFLAGS・[profile.release])を変えるとき、バイナリに焼き込まれる情報を調べるときに開く。
---

# 配布用のビルド

リリース全体の流れは6節、Androidの配布物(署名したAPK)は7節。配布物は`scripts/`のスクリプトで作る。`cargo build --release`や`npx tauri build`を直接叩くと、
ビルドした人の絶対パスがバイナリに残る(下の「何をしているか」)。`release/*`への移行と
配布はメンテナが行う(CLAUDE.md「進行中の作業」)。

## 1. 実行する

| OS | コマンド |
|---|---|
| Linux | `scripts/release-build.sh` |
| Windows | `pwsh scripts/release-build.ps1`(PowerShell 7.2以降。それより古いと最初の行で止まる) |

macOSは今は対象にしていない(資格情報の保存先が無い。`docs/spec/architecture/network-secrets.md`)。
shはGNU tarのオプションを使うので、Linux向け。

- 前提: Rust・Node.js(npm)と、Tauriのビルドに要るシステムの依存(CI`.github/workflows/ci.yml`の
  「システム依存を入れる」と同じ)。npmの依存(`frontend/`と`crates/scitl-tauri/`)はスクリプトが入れる。
  `cargo-about`(CIの「第三者ライセンス」と同じ版・同じコマンドで入れる)も前提にする。Windowsは
  MSVCのツールチェーン(Rustの既定)を使う
- 引数はそのまま`tauri build`に渡る。配布物はインストーラーを使わず実行ファイルだけなので、
  スクリプトが`--no-bundle`を付け、`--bundles`・`--no-bundle`を渡すと止まる。
  `--target`と`CARGO_TARGET_DIR`には対応しない(検査するバイナリの場所が変わる。スクリプトは前のビルドの
  実行ファイルを先に消すので、検査の前に止まる)
- `RUSTFLAGS`・`CARGO_ENCODED_RUSTFLAGS`を設定したままだと止まる。外してから実行する。スクリプトが渡す
  `CARGO_ENCODED_RUSTFLAGS`は、ほかの場所のrustflags(`~/.cargo/config.toml`の`build.rustflags`・
  `[target.*].rustflags`、`CARGO_BUILD_RUSTFLAGS`等)より優先され、それらは**黙って効かなくなる**
  (リンカーの指定や`target-cpu`等)。リポジトリの`.cargo/config.toml`にrustflagsは無い
- フラグが開発時のビルドと違うので、`target/release`は全部作り直しになる
- `tauri build`は`crates/scitl-tauri/Cargo.toml`の依存の書き方を自分の形(`{ version = "2",
  features = [] }`)に揃え直すので、リポジトリにはその形で置いてある。整理のつもりで短い形に戻すと、
  ビルドのたびに作業ツリーが汚れる。書き直しはLFなので、`.gitattributes`でこのファイルをLFに
  固定してある(CRLFでチェックアウトされると、内容が同じでも変更ありと出る)

## 2. 何をしているか

`--remap-path-prefix`で、次の3つのパスをバイナリに焼き込まれる形から置き換える(Issue #380)。

| パス | 置き換え先 | 主に入るもの |
|---|---|---|
| ホームディレクトリ | `~` | 下の2つに当たらない分 |
| `CARGO_HOME`(既定は`~/.cargo`) | `cargo-home` | 依存クレートのパニックの位置(`registry/src/…`)。Linuxで約580箇所あった |
| このリポジトリ | `.` | 今は0箇所(ワークスペースのクレートは相対パスで渡る)。`env!("CARGO_MANIFEST_DIR")`等が焼き込まれたときの保険 |

- `strip = true`(`[profile.release]`)が消すのはシンボルとデバッグ情報だけで、パニックの位置などの
  文字列は残る
- `.cargo/config.toml`の`rustflags`は環境変数を展開できないので、スクリプトで渡す
- パスは、書かれたままの形(cargoは`CARGO_HOME`のシンボリックリンクを解決せずに使う)と、リンクを
  解決した形の両方を置き換え、両方を検査する
- パスに空白があっても割れないよう、区切りが空白でない`CARGO_ENCODED_RUSTFLAGS`で渡す
- rustcは後に書いた置き換えから当てはまるかを見るので、広いもの(ホーム)を先に書く
- ホームがルート(`HOME=/`)・ドライブの直下なら止める(置き換えがすべてのパスに当たるため)

### Cのソースの場所

rustcの置き換えは、依存クレートがCのコンパイラに作らせる部分には届かない。今は`aws-lc-sys`(TLSの
暗号ライブラリ)のCのソースが、自分の場所を`__FILE__`として焼き込む。

- Linuxでは`aws-lc-sys`が自分で`-ffile-prefix-map`を付けるので、スクリプトは何もしない
- Windows(MSVC)では付けないので、ps1が環境変数`CL`(コンパイラが引数の前に足して読む)で
  `/d1trimfile:`を渡し、同じ3つの場所を取り除く。置き換えではないので、`registry\src\…`から
  始まる形で残る
- `aws-lc-sys`は、自分のソースの場所を、リンクを辿り、パスの長さの上限を避けるために8.3形式
  (`…\CARGO~1\registry\…`)にしてからコンパイラへ渡す。MSVCは`#include`されたファイルだけを長い形に
  直すので、両方の形が焼き込まれる。ps1は3つの場所それぞれの8.3形式も、置き換え・取り除き・検査の
  対象にする
- `/d1trimfile:`は文書に載っていないフラグ(MSVC 14.51で確認、2026-10)。効かなくなれば検査が失敗する
- cargoは`CL`の変化を見ない。`CL`に渡すフラグを変えたら、`cargo clean --release -p aws-lc-sys`で
  Cのソースを作り直させる

## 3. 確かめる

スクリプトはビルドの後、組み立てた配布物のフォルダのすべてのファイル(実行ファイルとライセンス類)に
3つのパスが残っていないかを調べ、残っていれば配布物を消して失敗で終わる。ファイルのバイト列をそのまま
探すので、Tauriが圧縮して埋め込む画面の資産の中は見ない(今はVite側でsourcemapを出していないので、
パスは入らない)。手で確かめるなら次のとおり。

```sh
LC_ALL=C grep -c -a -F "$HOME/" target/release/scitl   # 0 なら残っていない
```

置き換えの対象を足したら(例えば別の場所に置いた依存)、スクリプトの検査にも足す。

## 4. 配布物にまとめる

スクリプトはGUIとCLIを配布物のフォルダにまとめ、3節の検査を通したら、Windowsはzip、Linuxは
tar.gz(実行の許可を保つため)にする。出来るのは`target/dist/scitl-<版>-windows-x64.zip`と
`target/dist/scitl-<版>-linux-x64.tar.gz`。版は`Cargo.toml`の`[workspace.package]`から取る。
Linuxの中身は、実行ファイルに拡張子が無いほかは同じ。tarには、持ち主をroot(`0/0`)、権限を
`755`・`644`の形に揃えて記録する(そのままだと、ビルドした人のユーザー名とumaskが入る)。

```
scitl-<版>-windows-x64/
├── scitl.exe
├── scitl-cli.exe
├── README.md                 # リポジトリのREADME.mdをそのまま入れる
├── LICENSE
└── THIRD-PARTY-LICENSES/
    ├── rust.txt              # Rustのクレート
    ├── frontend.txt          # 画面のバンドルに入ったnpmのパッケージ(Viteのbuild.licenseが出す一覧から作る)
    └── NotoJP-LICENSE.txt    # 同梱フォント(先頭に、フォントファイルに記録された著作権表示を書いてある)
```

- `tauri build`はGUIしか作らないので、CLIはスクリプトが同じ置き換えを付けて別にビルドし、検査にも掛ける
- Windowsの実行ファイルは、VCランタイムを静的にリンクする(GUIは`tauri build`が、CLIは
  `crates/scitl-cli/build.rs`が`static_vcruntime`で行う)。既定のままだと`VCRUNTIME140.dll`
  (Visual C++の再頒布可能パッケージ)を求め、入っていないPCで起動できない。ps1は、配布物の
  実行ファイルが`VCRUNTIME140`を求めていれば失敗にする。手で確かめるなら、`dumpbin /dependents`
  (Linuxからは`objdump -p <exe> | grep 'DLL Name'`)に`VCRUNTIME140.dll`が出ないことを見る
- フォルダの組み立ては`scripts/assemble-dist.mjs`にある(圧縮だけがOSごと)
- `rust.txt`に載せるクレートは、cargo-aboutに洗い出させる(`about.toml`の`targets`向けのもの。ビルド
  スクリプトとテストにしか使わないものは除く)。配布しない`scitl-debug-cli`の依存や手続きマクロも入るので、
  実行ファイルに入るものより広い。漏れが無ければよい
- ライセンス文は、各クレートに入っているファイル(`LICENSE*`・`NOTICE*`等)をそのまま載せる。cargo-about
  (0.9.2)が照合して選ぶ文面は、照合に外れると著作権者の名前が入っていないひな形に置き換わり、その
  ことを失敗にもしない(約50クレートがそうなった)。Rustの標準ライブラリと、それと一緒に実行ファイルに入って
  依存の一覧に現れないクレート(addr2line・compiler_builtins・gimli・object・rustc-demangle)は、
  cargo-aboutが見ないので、`licenses/`に写したファイルを載せる(`assemble-dist.mjs`の`STD`。Rustを
  更新したら`licenses/README.md`の手順で見直す)
- パッケージにファイルを持たないクレートは、上流のリポジトリから`licenses/`に写したファイルを載せる
  (入手先は`licenses/README.md`)。写しの無いクレートが増えると失敗するので、写して`assemble-dist.mjs`の
  `SUPPLIED`に足す。上流にもファイルが無く、標準の文面で足りるもの(MPL-2.0の`selectors`)だけは
  `STANDARD_TEXT`に置き、標準の文面をその旨を添えて載せる
- クレートのライセンスとは別に実行ファイルに入るもの(`webview2-com-sys`が静的にリンクする、Microsoftの
  WebView2 SDKのローダー)は、`BUNDLED`でそのクレートの項に足す。SDKの`LICENSE.txt`はバイナリでの
  配布にも著作権表示とライセンス文の同梱を求める
- `about.toml`の`accepted`に無いライセンスの依存があると、失敗する。スクリプトはビルドを始める前に、CIは
  PRごとに、同じ確認を走らせる(`assemble-dist.mjs --check-licenses`)。足してよいライセンスかは
  `docs/spec/architecture/tech-stack.md`「ライセンス」で判断する
- npmのパッケージも同じ`accepted`で確かめる。Viteがビルドのときに出す一覧(`frontend/dist-meta/license.json`。`vite.config.ts`が`dist`の外へ
  移す。`dist`に残すと`tauri build`が画面の資産として埋め込むため)の
  SPDXの式を読み、許容外か、ライセンスの無いパッケージがあれば失敗する。ライセンスファイルを持たない
  パッケージも、著作権者の名前を載せられないので失敗にする。CIはフロントエンドのビルドの後に
  走らせる(`assemble-dist.mjs --check-frontend-licenses`)
- cargo-aboutは、対象のOS向けにしか使わないクレートを取りに行くので、ネットワークが要る
- Linuxの実行ファイルは、WebKitGTK 4.1・GTK3・libsoup3・GLib等を利用者の環境から読み込む。利用者は
  `libwebkit2gtk-4.1-0`(Fedoraは`webkit2gtk4.1`)を入れておく必要がある(GTK等は依存として入る)。
  無いと、起動時にライブラリが見つからないというエラーで止まる
- 求めるglibcの版は、ビルドした環境で決まる。Ubuntu 26.04でビルドすると、GUIはglibc 2.39以上、CLIは
  2.34以上(`objdump -T <実行ファイル> | grep -o 'GLIBC_[0-9.]*' | sort -uV | tail -1`で確かめる)
- AppImageにしないのは、大きさ(tar.gzが約98MB。実行ファイルだけなら約22MB)、同梱したWebKit・GLibが
  ビルドした環境のglibcを求めて古いディストリビューションで動かないこと、LGPLのライブラリを再配布する
  ことになるため(Issue #407)
- 起動を試すときは、展開したフォルダの`data`にデータが書かれる(`docs/spec/data-model/tables.md`
  「データディレクトリの場所」)。一時ディレクトリの中では起動を断るので、`target/`の下などに展開する。
  WebViewのプロファイルはOSごとのアプリの場所に書かれるので、それも分けるなら`XDG_DATA_HOME`・
  `XDG_CACHE_HOME`を作業用の場所に向ける

## 5. trim-pathsが安定版に入ったら

Cargoの`trim-paths`が安定版に入ったら、`[profile.release]`に`trim-paths = true`を置き、スクリプトの
`--remap-path-prefix`を外す(Rust 1.97では、まだ`-Z`の不安定な機能)。検査はそのまま残す。
`trim-paths`が変えるのはrustcの出力なので、ps1の`CL`(Cのソースの場所)は、外しても検査が通ると
確かめてから外す。

## 6. リリースの流れ

`release/*`を切るところから、`main`への取り込み・タグ・配布までは、メンテナが行う(CLAUDE.md)。
Claudeは頼まれた段だけを手伝う。版はセマンティックバージョニングで、最初の版は`0.1.0`。

1. **`release/<版>`を切る**: `develop`から切る(例: `release/0.1.0`)
2. **版を上げる**: `Cargo.toml`の`[workspace.package]`の`version`が`release/<版>`の版と違えば書き換え、
   `cargo check --workspace`で`Cargo.lock`を更新して、両方をコミットする。版はここ1箇所で、GUI・CLI・
   配布物の名前はここから取る(`tauri.conf.json`には`version`を置かない。`package.json`の`0.0.0`は
   配布物に出ない)
3. **確かめる**: `release/*`へのpushでCIが回る(Windowsのジョブを含む)。すべて通ることと、Dependabotの
   開いているアラートが無いことを確かめる(`gh api 'repos/{owner}/{repo}/dependabot/alerts?state=open' --jq length`。
   扱いは`docs/spec/architecture/tech-stack.md`「依存の脆弱性」)。直すものがあれば`release/*`の上で直す
4. **配布物を作る**: `release/<版>`の先頭で、LinuxとWindowsのそれぞれで1節のスクリプトを走らせる。
   出来るのは`target/dist/scitl-<版>-linux-x64.tar.gz`と`target/dist/scitl-<版>-windows-x64.zip`。
   版はCargo.tomlから取り、コミットの情報は焼き込まないので、`main`へ取り込んだ後のものと中身は同じ。
   AndroidのAPKは、Linuxで`release-build.sh`の**後に**7節のスクリプトを走らせて作る(`release-build.sh`は
   `target/dist`を空にしてから始める)。出来るのは`target/dist/scitl-<版>-android-arm64.apk`
5. **起動を確かめる**: 一時ディレクトリの外(`target/`の下など)に展開して起動し、画面が出ることと、
   展開したフォルダに`data`ができることを見る。APKは7節の「確かめる」のとおりに実機へ入れて見る。
   直すものがあれば`release/*`の上で直し、手順4からやり直す(タグを打つ前に済ませ、打ち直しを避ける)
6. **`main`へ取り込む**: `release/<版>` → `main`のPRを作り、メンテナがマージする
7. **タグを打つ**: `main`のマージコミットに注釈付きタグ`v<版>`を打ってpushする
   (`git tag -a v0.1.0 -m "SCITL 0.1.0"`)
8. **GitHub Releasesに置く**: `gh release create v<版> <tar.gz> <zip> <apk> --title "SCITL <版>" --notes-file <ノート>`。
   リポジトリが非公開の間は、コラボレーターしかダウンロードできない。ノートには変わったことを書き、
   次も書く(`README.md`は内容を意図して少なくしてあり、配布物に入るのもそれなので、利用者が
   これらを読めるのはノートだけになる。ビルドする環境を変えたら、求めるglibcの版を直す)
   - Windowsの実行ファイルにコード署名が無く、初回の起動でSmartScreenの警告が出ること
   - Linuxは`libwebkit2gtk-4.1-0`(Fedoraは`webkit2gtk4.1`)が要ること、求めるglibcの版(4節)
   - AndroidはAndroid 10以上・arm64の端末向けであること。入れるときに、ダウンロードに使ったアプリ
     (ブラウザ等)へ「提供元不明のアプリ」の許可が要ること。データはアプリの中に置かれ、
     アンインストールすると消えること
   - データは展開したフォルダの`data`に置かれること。版を上げるときは、古いフォルダの`data`を
     新しいフォルダへ移すこと
   - ユーザーのフォルダの下に置くこと。同時に開かないこと。同期するフォルダ・ネットワークドライブに
     置かないこと(`docs/spec/data-model/tables.md`「データディレクトリの場所」)
   - 外部ツール(MCP)はstreamable_httpだけに対応すること。stdioで動くサーバーは中継するツール
     (`supergateway`・`mcp-proxy`等)で使い、中継は`127.0.0.1`で待ち受けさせ、認証用のトークンを
     必須にして、その値をヘッダーとして登録すること(`docs/spec/tools.md` 4.5節)
9. **`develop`へ戻す**: `release/*`の上で版を上げた・直したものがあれば、`release/<版>` → `develop`の
   PRを作り、メンテナがマージする。何も無ければ省く

## 7. Androidの配布物

署名したAPKを1つ作る。置き場所はデスクトップの配布物と同じGitHub Releases。Google Playには出さない。

### 署名の鍵

Androidは、同じ鍵で署名されたAPKだけを、入っているアプリへの上書きとして受け付ける。**鍵を失うか
変えると、同じアプリとして更新できなくなる**(上書きのインストールを断られ、利用者はアンインストール
してから入れ直すことになり、アプリの中のデータが消える)。鍵が漏れると、SCITLを名乗る更新を
他人が作れる。

- 鍵(キーストアのファイル)とパスワードはメンテナが持つ。**リポジトリにも、Claudeのセッションにも
  置かない**。Claudeは本番の鍵を作らず、パスワードを受け取らない。スクリプトを本番の鍵で走らせるのは
  メンテナ
- 控えは、キーストアのファイルとパスワードを、ビルドする機械とは別の場所に置く(置き場所はメンテナが
  決める)
- 本番の鍵の証明書の指紋(SHA-256)は、`release-build-android.sh`の`release_certificate`に書いてある
  (指紋は、配ったAPKから誰でも読める公開の情報)。スクリプトは、配布するAPK(arm64)がこの鍵で
  署名されていなければ失敗にする。控えから戻したキーストアが正しいかも、この指紋で確かめる
- 鍵はPKCS12のキーストアに1つ作る。有効期間は、切れると更新を出せなくなるので長くする

```sh
keytool -genkeypair -keystore <置き場所>/scitl-release.p12 -storetype PKCS12 \
  -alias scitl -keyalg RSA -keysize 4096 -validity 10000 -dname "CN=SCITL Task Companion"
keytool -list -v -keystore <置き場所>/scitl-release.p12 -alias scitl | grep SHA256   # 指紋
```

デバッグビルド(`tauri android build --debug`)は、機械ごとに作られるデバッグ用の鍵で署名される。
本番の鍵とは別なので、デバッグビルドを入れてある端末にリリースのAPKは上書きできない(逆も同じ。
`INSTALL_FAILED_UPDATE_INCOMPATIBLE`)。先に`adb uninstall net.niigo.scitl`する(データは消える)。

### 実行する

```sh
export SCITL_ANDROID_KEYSTORE=<キーストアのファイルの絶対パス>
export SCITL_ANDROID_KEY_ALIAS=scitl
read -rsp 'キーストアのパスワード: ' SCITL_ANDROID_KEYSTORE_PASSWORD; echo   # 履歴に残さない
export SCITL_ANDROID_KEYSTORE_PASSWORD
scripts/release-build-android.sh
```

bashで実行する(zshの`read -p`は別の意味になる)。`read`は1行ずつ打つ(貼り付ける)。まとめて貼ると、パスワードを待っている間に次の行が入力として
読まれ、空のまま(または次の行をパスワードとして)進む。

- 前提: 1節のものに加えて、`docs/spec/architecture/tech-stack.md`「Androidのビルド」の道具
  (SDK・NDK・JDK・Rustのターゲット`aarch64-linux-android`)と環境変数。APKはビルドする側のOSに
  よらないので、スクリプトはLinux用だけを置く(Windowsでは作らない)
- 配布するのは実機向けの`arm64-v8a`だけ(4つのABIをまとめると、APKが数倍の大きさになる)。
  エミュレーターでリリースのAPKを確かめるときは`--target x86_64`を付ける
  (`scitl-<版>-android-x86_64.apk`が出来る。配布しない。本番の鍵でなくても通るので、Claudeが
  確かめるときは、その場で作った使い捨ての鍵を使う)
- 署名はスクリプトが`apksigner`で行う。Gradleには署名の設定を置いていないので、
  `npx tauri android build`を直接叩くと、署名の無い(端末に入れられない)APKが出来る
- パスワードは`apksigner`にだけ渡す(スクリプトが最初に環境変数から外す)。Gradleに渡すと、ビルドの
  後も数時間残るGradle・Kotlinのデーモンの環境変数に載り、同じ機械のほかのプロセスから読める。
  npmの依存のスクリプトや、依存クレートの`build.rs`にも届く
- `RUSTFLAGS`・`CARGO_ENCODED_RUSTFLAGS`の扱いは1節と同じ。フラグが開発時と違うので、
  `target/<ターゲット>/release`は全部作り直しになる

### 何をしているか

- **版**: `versionName`は`Cargo.toml`の`[workspace.package]`の版、`versionCode`は
  `major×1000000 + minor×1000 + patch`(0.2.0なら2000)。Gradleがcargoの解決結果から引くので、
  6節の手順2で版を上げれば両方上がる。`versionCode`が入っているものより小さいAPKは、上書きの
  インストールを断られる。`tauri.conf.json`に`version`を置かないのは6節のとおりで、tauri-cliは
  その場合`tauri.properties`を書かないので、これには頼らない
- **絶対パス**: 2節と同じ置き換えを、ネイティブのライブラリ(`lib/arm64-v8a/libscitl_tauri_lib.so`)に
  掛ける。環境変数は、tauri-cli → Gradle → tauri-cli → cargoの経路をそのまま届く(置き換えを
  付けないと、`.so`に依存クレートの場所が残る)。NDKのclangでビルドされるCのソース
  (`aws-lc-sys`・SQLite)の場所は、`.so`に入っていない(NDK 30.0で確認、2026-10。入れば検査が失敗する)
- **第三者ライセンスの一覧**: APKの中の`assets/licenses/`に、`LICENSE`と`THIRD-PARTY-LICENSES/`を入れる
  (APKは1つのファイルで渡るので、隣に置いても一緒に届かない)。Gradleのタスク`scitlLicenses`が、
  リリースのAPKを作るたびに`assemble-dist.mjs --android-licenses`を呼んで組み立てる
  - `rust.txt`は、cargo-aboutにAndroid向け(`aarch64-linux-android`)で洗い出させる。Androidにしか
    入らないクレートの写しは`SUPPLIED_ANDROID`に置く
  - `android.txt`は、Gradleが解決したMavenの依存(androidx・Material・Kotlinの標準ライブラリ等)。
    Gradleのタスク`scitlReleaseDependencies`が、リリースのAPKに入るものと各POMの書くライセンスを
    書き出し、`assemble-dist.mjs`がライセンスを見分けて`about.toml`の`accepted`と照らす。
    見分けられないもの・許容外のものがあれば失敗する。Mavenのパッケージの多くはライセンス文を
    持たないので、標準の文面を載せる(`licenses/apache-2.0`)。jarの中に表示のファイル(`META-INF`の
    NOTICE・LICENSE)を持つもの(Jackson等)は、それも載せる。AndroidのビルドはこれらをAPKから
    除くことがある(`META-INF/NOTICE`・`META-INF/LICENSE`は既定で除かれる)。Tauri本体とプラグインの
    Kotlin側は、Rustのクレートの中身なので`rust.txt`の側で足りる。`rustls-platform-verifier`のKotlinの
    部品(`org.rustls:rustls-platform-verifier`)はMavenの依存として`android.txt`に出るが、POMがライセンスを
    書いていないので、同じリポジトリのクレート`rustls-platform-verifier-android`の項(`rust.txt`)を指す
  - `assemble-dist.mjs --check-licenses`(CIと、ビルドの前)は、Android向けのクレートの一覧も確かめる。
    Mavenの依存はGradleを動かさないと分からないので、APKを作るときにだけ確かめる
- **縮小(R8)**: リリースビルドはR8が使われていないクラスを消し、名前を変える。RustからJNIで呼ぶ
  クラスは見えないので、`gen/android/app/proguard-rules.pro`に残す指定を書く。JNIで呼ぶものを足したら、
  ここにも足し、リリースのAPKで動かして確かめる(デバッグビルドは縮小しないので、気付けない)

### 確かめる

スクリプトは、出来たAPKについて次を確かめ、通れば署名して`target/dist`へ置く。

- 入っているネイティブのライブラリが、頼んだABIのものだけであること
- `assets/licenses/`に一覧が入っていること
- APKを展開したすべてのファイルに、3節の3つのパスが残っていないこと
- 署名したあと、署名が検証できることと、署名した鍵が本番のものであること(`apksigner verify`が出す
  証明書の指紋を`release_certificate`と比べる。x86_64は、違っていても知らせるだけ)

リリースのAPKはWebViewのデバッグ(`android-check`スキル4節の実寸の測定)が使えない。撮る・触るは使える。
手で確かめるのは次のとおり。

```sh
aapt2 dump badging <apk> | head -3        # versionCode・versionName・minSdk・targetSdk
adb install <apk>                         # 上書きなら -r。デバッグビルドが入っていれば先にアンインストールする
adb logcat -s SCITL RustStdoutStderr AndroidRuntime
```

R8で消えると困るもの(JNIで呼ぶもの)を通る操作を、リリースのAPKで一通り行う: HTTPSのプロバイダーへの
接続(証明書の検証。OCSPの宛先を持たない証明書の通信先で、CRLの取得を通す。Issue #535)、鍵付きのプロバイダーの登録(秘密情報の保存先)、応答の生成を裏へ回して通知を
受け取り、通知から会話を開く、エクスポート、添付の選択。最後に実機へ入れて起動する。

