---
name: release-build
description: SCITLの配布用ビルドの手順(scripts/release-build.sh・release-build.ps1の使い方、ビルドした人の絶対パスをバイナリから外す仕組みと確かめ方、配布物(zip・AppImageを入れたtar.gz)と第三者ライセンスの一覧の組み立て、trim-pathsが安定版に入ったときの移し替え)。配布物を作るとき、依存のライセンスの許容(about.toml)を変えるとき、配布用ビルドの設定(RUSTFLAGS・[profile.release])を変えるとき、バイナリに焼き込まれる情報を調べるときに開く。
---

# 配布用のビルド

配布物は`scripts/`のスクリプトで作る。`cargo build --release`や`npx tauri build`を直接叩くと、
ビルドした人の絶対パスがバイナリに残る(下の「何をしているか」)。`release/*`への移行と
配布はメンテナが行う(CLAUDE.md「進行中の作業」)。

## 1. 実行する

| OS | コマンド |
|---|---|
| Linux | `scripts/release-build.sh` |
| Windows | `pwsh scripts/release-build.ps1 --no-bundle`(PowerShell 7.2以降。それより古いと最初の行で止まる) |

macOSは今は対象にしていない(資格情報の保存先が無い。`docs/spec/architecture/network-secrets.md`)。
shはAppImageを作るので、Linuxでしか動かない。

- 前提: Rust・Node.js(npm)と、Tauriのビルドに要るシステムの依存(CI`.github/workflows/ci.yml`の
  「システム依存を入れる」と同じ)。npmの依存(`frontend/`と`crates/scitl-tauri/`)はスクリプトが入れる。
  `cargo-about`(CIの「第三者ライセンス」と同じ版・同じコマンドで入れる)も前提にする。Windowsは
  MSVCのツールチェーン(Rustの既定)を使う。LinuxのAppImageは、Tauriが束ねる道具(linuxdeploy)を
  初回に取りに行くので、ネットワークが要る
- 引数はそのまま`tauri build`に渡る。Windowsは配布物にインストーラーを使わないので`--no-bundle`を
  付ける。Linuxは束ね方をAppImageに固定するので、`--bundles`・`--no-bundle`を渡すと止まる。
  `--target`と`CARGO_TARGET_DIR`には対応しない(検査するバイナリの場所が変わる。検査の前に止まる)
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

スクリプトはビルドの後、GUIとCLIの実行ファイルの中身に3つのパスが残っていないかを調べ、残っていれば
失敗で終わる。Linuxは、組み立てた配布物のフォルダと、AppImageを展開した中身(GUIの実行ファイル、
同梱のライブラリ、束ねる道具が作る設定ファイル)のすべてのファイルと、シンボリックリンクの向き先を
調べ、残っていれば配布物を消す。調べるのは実行ファイルのバイト列だけで、Tauriが圧縮して埋め込む画面の資産の中は
見ない(今はVite側でsourcemapを出していないので、パスは入らない)。手で確かめるなら次のとおり。

```sh
LC_ALL=C grep -c -a -F "$HOME/" target/release/scitl   # 0 なら残っていない
```

置き換えの対象を足したら(例えば別の場所に置いた依存)、スクリプトの検査にも足す。

## 4. 配布物にまとめる

スクリプトは検査の後、GUIとCLIを配布物のフォルダにまとめ、Windowsはzip、Linuxはtar.gz(実行の
許可を保つため)にする。出来るのは`target/dist/scitl-<版>-windows-x64.zip`と
`target/dist/scitl-<版>-linux-x64.tar.gz`。版は`Cargo.toml`の`[workspace.package]`から取る。
LinuxのGUIは、実行ファイルではなくAppImage(`scitl.AppImage`。Tauriが付ける名前は製品名の空白を
含むので付け替える)を入れる。それ以外の中身は同じ。tarには、持ち主をroot(`0/0`)、権限を
`755`・`644`の形に揃えて記録する(そのままだと、ビルドした人のユーザー名とumaskが入る)。

```
scitl-<版>-windows-x64/
├── scitl.exe
├── scitl-cli.exe
├── LICENSE
└── THIRD-PARTY-LICENSES/
    ├── rust.txt              # Rustのクレート
    ├── frontend.md           # 画面のバンドルに入ったnpmのパッケージ(Viteのbuild.license)
    └── NotoJP-LICENSE.txt    # 同梱フォント
```

- `tauri build`はGUIしか作らないので、CLIはスクリプトが同じ置き換えを付けて別にビルドし、検査にも掛ける
- フォルダの組み立ては`scripts/assemble-dist.mjs`にある(圧縮だけがOSごと)
- `rust.txt`に載せるクレートは、cargo-aboutに洗い出させる(`about.toml`の`targets`向けのもの。ビルド
  スクリプトとテストにしか使わないものは除く)。配布しない`scitl-debug-cli`の依存や手続きマクロも入るので、
  実行ファイルに入るものより広い。漏れが無ければよい
- ライセンス文は、各クレートに入っているファイル(`LICENSE*`・`NOTICE*`等)をそのまま載せる。cargo-about
  (0.9.2)が照合して選ぶ文面は、照合に外れると著作権者の名前が入っていないひな形に置き換わり、その
  ことを失敗にもしない(約50クレートがそうなった)。ファイルを持たないクレート(13個)だけ、標準の文面を
  その旨を添えて載せる。Rustの標準ライブラリはクレートの一覧に出ないので、冒頭に書いてある
- `about.toml`の`accepted`に無いライセンスの依存があると、失敗する。スクリプトはビルドを始める前に、CIは
  PRごとに、同じ確認を走らせる(`assemble-dist.mjs --check-licenses`)。足してよいライセンスかは
  `docs/spec/architecture/tech-stack.md`「ライセンス」で判断する
- cargo-aboutは、対象のOS向けにしか使わないクレートを取りに行くので、ネットワークが要る
- AppImageは、WebKitGTK・GTKほか、ビルドした環境のライブラリを約150個同梱する(展開すると約270MB)。
  それらのライセンスは、束ねる道具がDebianのパッケージの`copyright`ファイル(`usr/share/doc/<パッケージ>/`)
  をAppImageの中に入れる。ただし多くはLGPL・GPLの全文を`/usr/share/common-licenses/`に任せていて、
  その全文は入っていない。同梱するライブラリはビルドする環境(Ubuntuの版)で変わる
- AppImageはglibcを同梱しないので、**ビルドした環境のglibcより古い環境では起動しない**。Ubuntu 26.04
  (glibc 2.43)でビルドしたものは、Ubuntu 24.04・Debian 13・Fedora 43では同梱のWebKit・GLibの読み込みで
  止まり、Fedora 44・Arch Linuxでは画面まで出た(2026-10、コンテナで確認)。GL・X11・fontconfig・
  freetype・harfbuzz・fribidi等は同梱せず、利用者の環境のものを使う
- AppImageの中身の展開・起動の確かめ方: `scitl.AppImage --appimage-extract`で`squashfs-root/`に
  展開される。起動を試すときは、`XDG_DATA_HOME`・`XDG_CONFIG_HOME`・`XDG_CACHE_HOME`を作業用の
  場所に向けて、利用者のデータ(`~/.local/share/net.niigo.scitl`)に触れないようにする

## 5. trim-pathsが安定版に入ったら

Cargoの`trim-paths`が安定版に入ったら、`[profile.release]`に`trim-paths = true`を置き、スクリプトの
`--remap-path-prefix`を外す(Rust 1.97では、まだ`-Z`の不安定な機能)。検査はそのまま残す。
`trim-paths`が変えるのはrustcの出力なので、ps1の`CL`(Cのソースの場所)は、外しても検査が通ると
確かめてから外す。
